//! The shared SPI bus (MOSI 35, MISO 37, SCK 36): the SD card (CS 47) and the
//! QMI8658 IMU (CS 34). This task owns the bus, samples the IMU at 50 Hz, and
//! writes every stream to its own CSV file in a new session directory.
//!
//! Rows are buffered per file and written in blocks; every file is flushed
//! (its directory entry updated) every 10 s, so a power cut loses at most the
//! last few seconds. This is the test logger, not Cairn's sealed format/v3
//! record — that arrives with T2.

extern crate alloc;

use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write as _;
use core::ops::ControlFlow;

use embassy_time::{Duration, Instant, Ticker};
use embedded_hal::digital::{ErrorType, OutputPin};
use embedded_hal_bus::spi::RefCellDevice;
use embedded_sdmmc::{
    Mode, RawDirectory, RawFile, RawVolume, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager,
};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use heapless::String;

use crate::imu::Qmi8658;
use crate::shared::{self, RECORDS, STORAGE_CMDS, STREAMS, StorageCmd, Stream, event, with_status};

type Bus = Spi<'static, Blocking>;

/// A chip select that borrows its pin, so devices can be rebuilt after an SD
/// failure without giving the pin away.
pub struct Cs<'a>(&'a RefCell<Output<'static>>);
impl ErrorType for Cs<'_> {
    type Error = core::convert::Infallible;
}
impl OutputPin for Cs<'_> {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().set_low();
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().set_high();
        Ok(())
    }
}

/// FAT timestamps from GNSS time once it is known.
#[derive(Clone, Copy)]
pub struct Clock;
impl TimeSource for Clock {
    fn get_timestamp(&self) -> Timestamp {
        let t = shared::status()
            .utc
            .unwrap_or(trail_core::pcf8563::DateTime {
                year: 2026,
                month: 1,
                day: 1,
                hour: 0,
                min: 0,
                sec: 0,
            });
        Timestamp::from_calendar(t.year, t.month, t.day, t.hour, t.min, t.sec).unwrap_or(
            Timestamp {
                year_since_1970: 56,
                zero_indexed_month: 0,
                zero_indexed_day: 0,
                hours: 0,
                minutes: 0,
                seconds: 0,
            },
        )
    }
}

/// Clock one byte with every chip select high. Many SD cards keep driving
/// MISO after their CS goes high until they see such a clock, and on this
/// board MISO is shared with the IMU (the V3.1 carrier has no level shifters
/// isolating the card). Called before every IMU access.
fn release_miso(bus: &RefCell<Bus>) {
    use embedded_hal::spi::SpiBus;
    let mut b = bus.borrow_mut();
    let _ = SpiBus::write(&mut *b, &[0xFF]);
    let _ = SpiBus::flush(&mut *b);
}

/// A block device that, for a GPT-formatted card, presents a synthesised MBR
/// in place of block 0 pointing at the first basic-data partition (see
/// trail_core::gpt). Block 0 is never written through this.
pub struct PartView<D> {
    inner: D,
    block0: Option<[u8; 512]>,
}

impl<D: embedded_sdmmc::BlockDevice<Error = embedded_sdmmc::SdCardError>> PartView<D> {
    /// Inspect the card; returns the view and a description of what it found.
    fn new(inner: D) -> (Self, &'static str) {
        use embedded_sdmmc::{Block, BlockIdx};
        use trail_core::gpt;
        let mut b = [Block::new()];
        if inner.read(&mut b, BlockIdx(0)).is_err() {
            return (
                Self {
                    inner,
                    block0: None,
                },
                "block 0 unreadable",
            );
        }
        if !gpt::is_protective_mbr(&b[0].contents) {
            return (
                Self {
                    inner,
                    block0: None,
                },
                "MBR",
            );
        }
        if inner.read(&mut b, BlockIdx(1)).is_err() {
            return (
                Self {
                    inner,
                    block0: None,
                },
                "GPT header unreadable",
            );
        }
        let Some(h) = gpt::header(&b[0].contents) else {
            return (
                Self {
                    inner,
                    block0: None,
                },
                "protective MBR without a GPT header",
            );
        };
        let per_block = 512 / h.entry_size as usize;
        let blocks = (h.entry_count as usize).div_ceil(per_block).min(32);
        for i in 0..blocks {
            if inner
                .read(&mut b, BlockIdx((h.entries_lba + i as u64) as u32))
                .is_err()
            {
                break;
            }
            for k in 0..per_block {
                let o = k * h.entry_size as usize;
                if let Some((first, last)) =
                    gpt::basic_data_extent(&b[0].contents[o..o + h.entry_size as usize])
                    && let Some(mbr) = gpt::mbr_for_extent(first, last)
                {
                    return (
                        Self {
                            inner,
                            block0: Some(mbr),
                        },
                        "GPT, using its first basic-data partition",
                    );
                }
            }
        }
        (
            Self {
                inner,
                block0: None,
            },
            "GPT without a usable basic-data partition",
        )
    }
}

impl<D: embedded_sdmmc::BlockDevice<Error = embedded_sdmmc::SdCardError>>
    embedded_sdmmc::BlockDevice for PartView<D>
{
    type Error = embedded_sdmmc::SdCardError;

    fn read(
        &self,
        blocks: &mut [embedded_sdmmc::Block],
        start: embedded_sdmmc::BlockIdx,
    ) -> Result<(), Self::Error> {
        self.inner.read(blocks, start)?;
        if let (Some(mbr), 0) = (self.block0.as_ref(), start.0) {
            blocks[0].contents.copy_from_slice(mbr);
        }
        Ok(())
    }

    fn write(
        &self,
        blocks: &[embedded_sdmmc::Block],
        start: embedded_sdmmc::BlockIdx,
    ) -> Result<(), Self::Error> {
        if self.block0.is_some() && start.0 == 0 {
            return Err(embedded_sdmmc::SdCardError::WriteError);
        }
        self.inner.write(blocks, start)
    }

    fn num_blocks(&self) -> Result<embedded_sdmmc::BlockCount, Self::Error> {
        self.inner.num_blocks()
    }
}

type Card<'a> = SdCard<RefCellDevice<'a, Bus, Cs<'a>, Delay>, Delay>;
type Vm<'a> = VolumeManager<PartView<Card<'a>>, Clock, 4, 12, 1>;

struct Session {
    number: u16,
    volume: RawVolume,
    root: RawDirectory,
    dir: RawDirectory,
    files: [RawFile; STREAMS],
}

const WRITE_THRESHOLD: usize = 2048;
const FLUSH_EVERY: Duration = Duration::from_secs(10);
const SD_FAST: Rate = Rate::from_mhz(8);
const SD_SLOW: Rate = Rate::from_khz(400);

fn spi_config(rate: Rate) -> SpiConfig {
    SpiConfig::default()
        .with_frequency(rate)
        .with_mode(esp_hal::spi::Mode::_0)
}

fn session_name(n: u16) -> String<8> {
    let mut s = String::new();
    let _ = write!(s, "S{:04}", n);
    s
}

/// The highest `Snnnn` directory in the root.
fn last_session(vm: &Vm, root: RawDirectory) -> u16 {
    let mut max = 0u16;
    let _ = vm.iterate_dir(root, |e| {
        let b = e.name.base_name();
        if e.attributes.is_directory()
            && b.len() == 5
            && b[0] == b'S'
            && let Ok(s) = core::str::from_utf8(&b[1..])
            && let Ok(n) = s.parse::<u16>()
        {
            max = max.max(n);
        }
        ControlFlow::Continue(())
    });
    max
}

fn open_session(vm: &Vm) -> Result<Session, &'static str> {
    let volume = vm
        .open_raw_volume(VolumeIdx(0))
        .map_err(|_| "no FAT volume")?;
    let root = vm.open_root_dir(volume).map_err(|_| "root dir")?;
    let number = last_session(vm, root).saturating_add(1);
    let name = session_name(number);
    vm.make_dir_in_dir(root, name.as_str())
        .map_err(|_| "mkdir")?;
    let dir = vm
        .open_dir(root, name.as_str())
        .map_err(|_| "open session dir")?;

    // INFO.TXT: what produced this session.
    if let Ok(f) = vm.open_file_in_dir(dir, "INFO.TXT", Mode::ReadWriteCreateOrTruncate) {
        let mut info: String<512> = String::new();
        let _ = write!(
            info,
            "device=cairn-trail\nboard=LILYGO T-Beam Supreme\nfirmware={} ({}){}\ncommit={}{}\nsession={}\n\
             mac={}\nlora=off (ALDO3 never enabled)\nt_ms is milliseconds since boot; UTC is a column, never the index.\n",
            shared::VERSION,
            shared::FLAVOUR,
            if shared::DIRTY.is_empty() {
                ""
            } else {
                " DIRTY TREE"
            },
            shared::COMMIT,
            shared::DIRTY,
            name,
            crate::mac_string(),
        );
        let _ = vm.write(f, info.as_bytes());
        let _ = vm.close_file(f);
    }

    let mut opened: [Option<RawFile>; STREAMS] = [None; STREAMS];
    for s in Stream::ALL {
        let f = vm
            .open_file_in_dir(dir, s.file_name(), Mode::ReadWriteCreateOrAppend)
            .map_err(|_| "open data file")?;
        let mut h: String<320> = String::new();
        let _ = writeln!(h, "{}", s.header());
        vm.write(f, h.as_bytes()).map_err(|_| "write header")?;
        opened[s as usize] = Some(f);
    }
    let files: [RawFile; STREAMS] =
        core::array::from_fn(|i| opened[i].expect("every stream opened"));
    Ok(Session {
        number,
        volume,
        root,
        dir,
        files,
    })
}

fn close_session(vm: &Vm, s: Session) {
    for f in s.files {
        let _ = vm.close_file(f);
    }
    let _ = vm.close_dir(s.dir);
    let _ = vm.close_dir(s.root);
    let _ = vm.close_volume(s.volume);
}

/// CRC-32 (IEEE), so a host reading `cat` output can check it arrived whole.
fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

fn print_file(vm: &Vm, f: RawFile, label: &str) {
    // Nothing else may print while a file streams, or the CRC cannot match.
    let level = log::max_level();
    log::set_max_level(log::LevelFilter::Off);
    print_file_inner(vm, f, label);
    log::set_max_level(level);
}

fn print_file_inner(vm: &Vm, f: RawFile, label: &str) {
    let len = vm.file_length(f).unwrap_or(0);
    esp_println::println!("----- BEGIN {} {} bytes -----", label, len);
    let mut buf = [0u8; 512];
    let mut crc = 0xFFFF_FFFFu32;
    let mut total = 0u32;
    loop {
        match vm.read(f, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                crc = crc32_update(crc, &buf[..n]);
                total += n as u32;
                if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                    esp_println::print!("{}", s);
                } else {
                    for &b in &buf[..n] {
                        esp_println::print!("{}", b as char);
                    }
                }
            }
            Err(_) => {
                esp_println::println!("\n[read error]");
                break;
            }
        }
    }
    esp_println::println!(
        "\n----- END {} {} bytes crc32={:08x} -----",
        label,
        total,
        !crc
    );
}

fn list(vm: &Vm, session: Option<&Session>) {
    let Ok(volume) = vm
        .open_raw_volume(VolumeIdx(0))
        .or_else(|_| session.map(|s| s.volume).ok_or(()))
    else {
        esp_println::println!("no volume");
        return;
    };
    let root = match session {
        Some(s) => s.root,
        None => match vm.open_root_dir(volume) {
            Ok(r) => r,
            Err(_) => return,
        },
    };
    let mut dirs: heapless::Vec<String<12>, 64> = heapless::Vec::new();
    let _ = vm.iterate_dir(root, |e| {
        let mut n: String<12> = String::new();
        let _ = write!(n, "{}", e.name);
        if e.attributes.is_directory() {
            if !n.starts_with('.') {
                let _ = dirs.push(n);
            }
        } else {
            esp_println::println!("{:>10}  {}", e.size, n);
        }
        ControlFlow::Continue(())
    });
    for d in &dirs {
        esp_println::println!("{}/", d);
        if let Ok(dir) = vm.open_dir(root, d.as_str()) {
            let _ = vm.iterate_dir(dir, |e| {
                if !e.attributes.is_directory() {
                    esp_println::println!("{:>10}  {}/{}", e.size, d, e.name);
                }
                ControlFlow::Continue(())
            });
            let _ = vm.close_dir(dir);
        }
    }
    if session.is_none() {
        let _ = vm.close_dir(root);
        let _ = vm.close_volume(volume);
    }
}

fn cat(vm: &Vm, session: Option<&Session>, path: &str) {
    let (dir_name, file_name) = match path.split_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, path),
    };
    // A file of the open session: read through its own handle.
    if let Some(s) = session {
        let current = session_name(s.number);
        if (dir_name.is_none() || dir_name == Some(current.as_str()))
            && let Some(st) = Stream::ALL
                .iter()
                .find(|st| st.file_name().eq_ignore_ascii_case(file_name))
        {
            let f = s.files[*st as usize];
            if vm.file_seek_from_start(f, 0).is_ok() {
                print_file(vm, f, path);
            }
            let _ = vm.file_seek_from_end(f, 0);
            return;
        }
    }
    let Some(d) = dir_name else {
        esp_println::println!("give a path like S0001/GNSS.CSV");
        return;
    };
    let Some(s) = session else {
        esp_println::println!("card not mounted");
        return;
    };
    match vm.open_dir(s.root, d) {
        Ok(dir) => {
            match vm.open_file_in_dir(dir, file_name, Mode::ReadOnly) {
                Ok(f) => {
                    print_file(vm, f, path);
                    let _ = vm.close_file(f);
                }
                Err(_) => esp_println::println!("no such file: {}", path),
            }
            let _ = vm.close_dir(dir);
        }
        Err(_) => esp_println::println!("no such directory: {}", d),
    }
}

/// Write every buffer that has data; on error, report and leave it.
fn write_out(
    vm: &Vm,
    s: &Session,
    bufs: &mut [Vec<u8>; STREAMS],
    only_over: usize,
) -> Result<usize, ()> {
    let mut written = 0;
    for st in Stream::ALL {
        let b = &mut bufs[st as usize];
        if !b.is_empty() && b.len() >= only_over {
            vm.write(s.files[st as usize], b).map_err(|_| ())?;
            written += b.len();
            b.clear();
        }
    }
    Ok(written)
}

fn flush_all(vm: &Vm, s: &Session) -> Result<(), ()> {
    for f in s.files {
        vm.flush_file(f).map_err(|_| ())?;
    }
    Ok(())
}

#[embassy_executor::task]
pub async fn storage_task(mut bus: Bus, sd_cs: Output<'static>, imu_cs: Output<'static>) {
    let delay = Delay::new();
    // Both chip selects high before anything talks on the bus, then the SD
    // card's 74+ idle clocks with CS high (SD spec 6.4.1).
    let sd_cs = RefCell::new(sd_cs);
    let imu_cs = RefCell::new(imu_cs);
    let _ = bus.apply_config(&spi_config(SD_SLOW));
    {
        use embedded_hal::spi::SpiBus;
        let _ = bus.write(&[0xFF; 10]);
        let _ = bus.flush();
    }
    let bus = RefCell::new(bus);

    // The SD card first. Until a card has seen CMD0 with its CS low it is in
    // SD-bus mode, where it ignores CS and listens to every clock on the bus;
    // talking to the IMU before that confuses the card, and the card then
    // drives MISO under the IMU. LilyGO's factory test has the same order.
    {
        let probe: Card = SdCard::new(RefCellDevice::new(&bus, Cs(&sd_cs), delay).unwrap(), delay);
        match probe.num_bytes() {
            Ok(n) => event(
                "sd",
                format_args!(
                    "card in SPI mode, {} MB, type {:?}",
                    n / 1_000_000,
                    probe.get_card_type()
                ),
            ),
            Err(e) => event("sd", format_args!("no card answered CMD0: {:?}", e)),
        }
    }

    let mut imu = Qmi8658::new(RefCellDevice::new(&bus, Cs(&imu_cs), delay).unwrap());
    let mut d = delay;
    release_miso(&bus);
    #[cfg(feature = "verbose")]
    event(
        "diag_imu",
        format_args!("regs00..0f={:02x?}", imu.dump().ok()),
    );
    let imu_ok = match imu.init(&mut d, |step, value, whoami| {
        event(
            "imu_step",
            format_args!("{} value=0x{:02x} whoami=0x{:02x}", step, value, whoami),
        )
    }) {
        Ok((0x05, rev)) => {
            event(
                "imu",
                format_args!("QMI8658 revision 0x{:02x}, +-8 g, +-512 dps, 62.5 Hz", rev),
            );
            true
        }
        Ok((id, _)) => {
            event("imu", format_args!("unexpected WHO_AM_I 0x{:02x}", id));
            false
        }
        Err(_) => {
            event("imu", format_args!("SPI error"));
            false
        }
    };
    with_status(|s| s.imu_ok = imu_ok);
    // The first samples after enabling are garbage while the filters settle
    // (seen up to 3.7 g at rest): record from 200 ms after enabling.
    let imu_valid_from = Instant::now() + Duration::from_millis(200);

    let mut bufs: [Vec<u8>; STREAMS] =
        core::array::from_fn(|_| Vec::with_capacity(WRITE_THRESHOLD + 512));
    let mut ticker = Ticker::every(Duration::from_millis(20));
    let mut last_flush = Instant::now();
    let mut last_retry: Option<Instant> = None;
    let mut want_recording = true;
    let mut imu_n: u32 = 0;
    let mut bytes: u64 = 0;

    loop {
        // (Re)mount the card and open a session.
        let card: Card = SdCard::new(RefCellDevice::new(&bus, Cs(&sd_cs), delay).unwrap(), delay);
        let _ = bus.borrow_mut().apply_config(&spi_config(SD_SLOW));
        let size = card.num_bytes();
        let _ = bus.borrow_mut().apply_config(&spi_config(SD_FAST));
        let (view, layout) = PartView::new(card);
        if size.is_ok() {
            event("sd_layout", format_args!("{}", layout));
        }
        let vm: Vm = VolumeManager::new_with_limits(view, Clock, 1000);
        let mut session = match size {
            Ok(n) if want_recording => match open_session(&vm) {
                Ok(s) => {
                    // Persist the headers and INFO.TXT at once, so even a
                    // session cut off in its first seconds is readable.
                    let _ = flush_all(&vm, &s);
                    event(
                        "session",
                        format_args!("S{:04} opened; card {} MB", s.number, n / 1_000_000),
                    );
                    with_status(|st| {
                        st.session = s.number;
                        st.sd_ok = true;
                        st.recording = true;
                    });
                    Some(s)
                }
                Err(e) => {
                    event("sd", format_args!("card {} MB but {}", n / 1_000_000, e));
                    None
                }
            },
            Ok(_) => None,
            Err(e) => {
                if last_retry.is_none() {
                    event("sd", format_args!("mount failed: {:?}", e));
                }
                None
            }
        };
        if session.is_none() {
            with_status(|st| {
                st.sd_ok = false;
                st.recording = false;
            });
        }
        last_retry = Some(Instant::now());

        // Run until the card fails, or a stop/start asks for a new session.
        loop {
            ticker.next().await;

            if imu_ok {
                release_miso(&bus);
                if let Ok(s) = imu.sample().map_err(|_| ()).and_then(|s| {
                    if Instant::now() >= imu_valid_from {
                        Ok(s)
                    } else {
                        Err(())
                    }
                }) {
                    imu_n = imu_n.wrapping_add(1);
                    let b = &mut bufs[Stream::Imu as usize];
                    let mut line: String<96> = String::new();
                    let _ = writeln!(
                        line,
                        "{},{},{},{},{:.1},{:.1},{:.1},{:.2}",
                        shared::now_ms(),
                        s.accel_mg[0],
                        s.accel_mg[1],
                        s.accel_mg[2],
                        s.gyro_ddps[0] as f32 / 10.0,
                        s.gyro_ddps[1] as f32 / 10.0,
                        s.gyro_ddps[2] as f32 / 10.0,
                        s.temp_c
                    );
                    if session.is_some() {
                        b.extend_from_slice(line.as_bytes());
                    }
                    if imu_n.is_multiple_of(50) {
                        log::debug!("IMU {}", line.trim_end());
                        with_status(|st| st.accel_mg = s.accel_mg);
                    }
                }
            }

            while let Ok(r) = RECORDS.try_receive() {
                #[cfg(feature = "verbose")]
                if !matches!(r.stream, Stream::Mag | Stream::Sat) || imu_n.is_multiple_of(50) {
                    log::debug!("{} {}", r.stream.file_name(), r.line);
                }
                if session.is_some() {
                    let b = &mut bufs[r.stream as usize];
                    b.extend_from_slice(r.line.as_bytes());
                    b.push(b'\n');
                }
            }

            let mut failed = false;
            if let Some(s) = session.as_ref() {
                match write_out(&vm, s, &mut bufs, WRITE_THRESHOLD) {
                    Ok(n) => bytes += n as u64,
                    Err(()) => failed = true,
                }
                if !failed && last_flush.elapsed() >= FLUSH_EVERY {
                    last_flush = Instant::now();
                    match write_out(&vm, s, &mut bufs, 0).and_then(|n| flush_all(&vm, s).map(|_| n))
                    {
                        Ok(n) => bytes += n as u64,
                        Err(()) => failed = true,
                    }
                }
                with_status(|st| st.sd_bytes = bytes);
            }

            if let Ok(cmd) = STORAGE_CMDS.try_receive() {
                match cmd {
                    StorageCmd::List => list(&vm, session.as_ref()),
                    StorageCmd::Cat(p) => {
                        if let Some(s) = session.as_ref() {
                            let _ = write_out(&vm, s, &mut bufs, 0);
                            let _ = flush_all(&vm, s);
                        }
                        cat(&vm, session.as_ref(), p.as_str());
                    }
                    StorageCmd::Flush => {
                        if let Some(s) = session.as_ref() {
                            let r = write_out(&vm, s, &mut bufs, 0).and_then(|_| flush_all(&vm, s));
                            esp_println::println!(
                                "flush: {}",
                                if r.is_ok() { "ok" } else { "FAILED" }
                            );
                        }
                    }
                    StorageCmd::Stop => {
                        if let Some(s) = session.take() {
                            let _ = write_out(&vm, &s, &mut bufs, 0);
                            let n = s.number;
                            close_session(&vm, s);
                            event(
                                "session",
                                format_args!("S{:04} closed; card safe to remove", n),
                            );
                        }
                        want_recording = false;
                        with_status(|st| st.recording = false);
                    }
                    StorageCmd::Start => {
                        want_recording = true;
                        if session.is_none() {
                            break; // reopen through the mount path
                        }
                    }
                }
            }

            if failed {
                with_status(|st| {
                    st.write_errors += 1;
                    st.sd_ok = false;
                    st.recording = false;
                });
                event("sd", format_args!("write failed; remounting"));
                for b in bufs.iter_mut() {
                    b.clear();
                }
                break;
            }
            if session.is_none()
                && want_recording
                && last_retry
                    .map(|t| t.elapsed() > Duration::from_secs(10))
                    .unwrap_or(true)
            {
                break; // retry the mount
            }
        }
        if let Some(s) = session.take() {
            close_session(&vm, s);
        }
        drop(vm);
    }
}
