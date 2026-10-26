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

/// A whole file from the card's root, if it exists and fits.
fn read_root(vm: &Vm, s: &Session, name: &str, out: &mut [u8]) -> Option<usize> {
    let f = vm.open_file_in_dir(s.root, name, Mode::ReadOnly).ok()?;
    let mut n = 0;
    while n < out.len() {
        match vm.read(f, &mut out[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let _ = vm.close_file(f);
    Some(n)
}

/// Replace a file in the card's root.
fn write_root(vm: &Vm, s: &Session, name: &str, data: &[u8]) -> Result<(), ()> {
    let f = vm
        .open_file_in_dir(s.root, name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| ())?;
    let r = vm.write(f, data).map_err(|_| ());
    let c = vm.close_file(f).map_err(|_| ());
    r.and(c)
}

const PRIME_FILE: &str = "PRIME.TXT";
/// A position is saved for priming only when it is this good: 50 m.
const PRIME_SAVE_ACC_MM: u32 = 50_000;
/// While the fix stays good, PRIME.TXT is refreshed this often.
const PRIME_SAVE_EVERY: Duration = Duration::from_secs(300);

fn prime_read(vm: &Vm, s: &Session) -> Option<trail_core::prime::Prime> {
    let mut buf = [0u8; 512];
    let n = read_root(vm, s, PRIME_FILE, &mut buf)?;
    trail_core::prime::Prime::parse(core::str::from_utf8(&buf[..n]).ok()?)
}

fn prime_write(vm: &Vm, s: &Session, p: &trail_core::prime::Prime) -> Result<(), ()> {
    let mut text: String<512> = String::new();
    write!(text, "{}", p).map_err(|_| ())?;
    write_root(vm, s, PRIME_FILE, text.as_bytes())
}

/// A position worse than this is worth replacing with a Wi-Fi one: 1 km.
const WIFI_LOCATE_WORSE_THAN_CM: u32 = 100_000;
/// How long to wait for a Wi-Fi scan before giving up on it.
const WIFI_SCAN_WAIT: Duration = Duration::from_secs(8);

/// At boot: read PRIME.TXT and decide with the RTC what the receiver should
/// be told. Returns the plan and where its position came from.
fn prime_plan(vm: &Vm, s: &Session) -> (trail_core::prime::Plan, &'static str) {
    let rec = prime_read(vm, s);
    let p = trail_core::prime::plan(rec.as_ref(), shared::rtc_now());
    (
        p,
        match rec {
            Some(r) => r.source.as_str(),
            None => "no PRIME.TXT",
        },
    )
}

/// Whether a plan's position is missing or poor enough that a Wi-Fi
/// position would be better.
fn wants_wifi(p: &trail_core::prime::Plan) -> bool {
    p.pos.is_none_or(|q| q.acc_cm > WIFI_LOCATE_WORSE_THAN_CM)
}

/// Leave the plan for the GNSS task: the position frame now, the time as an
/// accuracy (the GNSS task reads the RTC as it sends), and only if the
/// receiver woke cold.
fn prime_commit(p: trail_core::prime::Plan, source: &str) {
    let mut frames: heapless::Vec<shared::AssistFrame, 2> = heapless::Vec::new();
    if let Some(pos) = p.pos {
        let mut raw = [0u8; 40];
        if let Some(n) = trail_core::ubx::mga_ini_pos_llh(
            pos.lat_e7, pos.lon_e7, pos.alt_cm, pos.acc_cm, &mut raw,
        ) {
            let mut f = shared::AssistFrame::new();
            let _ = f.extend_from_slice(&raw[..n]);
            let _ = frames.push(f);
        }
    }
    let anything = p.time.is_some() || p.pos.is_some();
    shared::PRIME_FRAMES.lock(|f| *f.borrow_mut() = frames);
    with_status(|st| {
        if st.prime == shared::PrimeState::Pending {
            st.prime_time_acc_ms = p.time.map_or(0, |t| t.acc_ms);
            st.prime_pos_acc_m = p.pos.map_or(0, |q| q.acc_cm / 100);
            st.prime = if anything {
                shared::PrimeState::Planned
            } else {
                shared::PrimeState::Nothing
            };
        }
    });
    event(
        "prime_plan",
        format_args!(
            "{} rtc={:?} time_acc_ms={:?} pos_acc_m={:?}",
            source,
            shared::rtc_now(),
            p.time.map(|t| t.acc_ms),
            p.pos.map(|q| q.acc_cm / 100)
        ),
    );
}

const WIFIFP_FILE: &str = "WIFIFP.CSV";
/// Fingerprint positions held in memory to keep new ones 100 m apart. A
/// fingerprint is ~200 bytes on the card; 2048 is ~400 KB.
const WIFIFP_MAX: usize = 2048;
/// Learn only from fixes this good: 30 m.
const WIFIFP_LEARN_ACC_MM: u32 = 30_000;
/// At most one learning scan this often.
const WIFIFP_LEARN_EVERY: Duration = Duration::from_secs(60);

/// Call `f` with each line of a root file, read in small pieces.
fn for_each_line(vm: &Vm, s: &Session, name: &str, mut f: impl FnMut(&str)) -> bool {
    let Ok(file) = vm.open_file_in_dir(s.root, name, Mode::ReadOnly) else {
        return false;
    };
    let mut line: String<400> = String::new();
    let mut buf = [0u8; 256];
    loop {
        let n = match vm.read(file, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' {
                f(line.as_str());
                line.clear();
            } else if line.push(b as char).is_err() {
                line.clear(); // an over-long line: not ours, skip it
            }
        }
    }
    if !line.is_empty() {
        f(line.as_str());
    }
    let _ = vm.close_file(file);
    true
}

/// The positions of every fingerprint on the card, for spacing new ones.
fn wififp_index(vm: &Vm, s: &Session) -> Vec<(i32, i32)> {
    let mut idx = Vec::new();
    for_each_line(vm, s, WIFIFP_FILE, |l| {
        if let Some(fp) = trail_core::wififp::Fingerprint::parse(l)
            && idx.len() < WIFIFP_MAX
        {
            idx.push((fp.lat_e7, fp.lon_e7));
        }
    });
    idx
}

/// Match a scan against every fingerprint on the card.
fn wififp_locate(
    vm: &Vm,
    s: &Session,
    scan: &shared::WifiScan,
) -> Option<trail_core::wififp::Estimate> {
    let mut knn = trail_core::wififp::Knn::<3>::default();
    let aps = &scan.aps[..scan.n];
    for_each_line(vm, s, WIFIFP_FILE, |l| {
        if let Some(fp) = trail_core::wififp::Fingerprint::parse(l) {
            knn.push(aps, &fp);
        }
    });
    knn.estimate()
}

/// Whether a good fix here would make a new fingerprint: nothing within
/// 100 m yet.
fn wififp_is_new_place(idx: &[(i32, i32)], lat_e7: i32, lon_e7: i32) -> bool {
    idx.len() < WIFIFP_MAX
        && idx.iter().all(|&(la, lo)| {
            trail_core::wififp::distance_m(la, lo, lat_e7, lon_e7)
                >= trail_core::wififp::LEARN_SPACING_M
        })
}

fn wififp_append(vm: &Vm, s: &Session, fp: &trail_core::wififp::Fingerprint) -> Result<(), ()> {
    let mut line: String<400> = String::new();
    writeln!(line, "{}", fp).map_err(|_| ())?;
    let f = vm
        .open_file_in_dir(s.root, WIFIFP_FILE, Mode::ReadWriteCreateOrAppend)
        .map_err(|_| ())?;
    let r = vm.write(f, line.as_bytes()).map_err(|_| ());
    let c = vm.close_file(f).map_err(|_| ());
    r.and(c)
}

/// The latest scan, if it finished after `since_ms`.
fn scan_since(since_ms: u64) -> Option<shared::WifiScan> {
    shared::WIFI_SCAN.lock(|s| s.borrow().filter(|x| x.at_ms >= since_ms))
}

/// While the fix is good: record it in PRIME.TXT for the next power-on.
fn prime_save_fix(vm: &Vm, s: &Session) -> bool {
    let st = shared::status();
    let Some(utc) = st
        .utc
        .filter(|_| st.fix_ok && st.h_acc_mm <= PRIME_SAVE_ACC_MM)
    else {
        return false;
    };
    let old = prime_read(vm, s);
    let rec = trail_core::prime::Prime {
        lat_e7: st.lat_e7,
        lon_e7: st.lon_e7,
        alt_cm: st.height_mm / 10,
        acc_cm: st.h_acc_mm / 10,
        fix_unix: trail_core::pcf8563::unix_seconds(&utc),
        rtc_unix: if st.rtc_checked_unix > 0 {
            st.rtc_checked_unix
        } else {
            old.map_or(0, |o| o.rtc_unix)
        },
        source: trail_core::prime::Source::Gnss,
    };
    prime_write(vm, s, &rec).is_ok()
}

/// The computer's time and/or a hand-given position, from `prime time|pos`.
fn prime_host(vm: &Vm, s: &Session, unix: Option<i64>, pos: Option<(i32, i32, i32, u32)>) {
    use trail_core::prime::{Prime, Source};
    let mut rec = prime_read(vm, s).unwrap_or(Prime {
        lat_e7: 0,
        lon_e7: 0,
        alt_cm: 0,
        acc_cm: 0,
        fix_unix: 0,
        rtc_unix: 0,
        source: Source::Manual,
    });
    if let Some(u) = unix {
        rec.rtc_unix = u;
    }
    if let Some((lat, lon, alt, acc)) = pos {
        rec.lat_e7 = lat;
        rec.lon_e7 = lon;
        rec.alt_cm = alt;
        rec.acc_cm = acc;
        // True now, if there is a clock worth believing to say when "now" is.
        rec.fix_unix = if shared::status().rtc_checked_unix > 0 {
            shared::rtc_now().unwrap_or(0)
        } else {
            0
        };
        rec.source = Source::Manual;
    }
    match prime_write(vm, s, &rec) {
        Ok(()) => esp_println::println!("prime: recorded in {}", PRIME_FILE),
        Err(()) => esp_println::println!("prime: FAILED to write {}", PRIME_FILE),
    }
}

fn prime_show(vm: &Vm, s: Option<&Session>) {
    let Some(s) = s else {
        esp_println::println!("prime: card not mounted");
        return;
    };
    let rec = prime_read(vm, s);
    match &rec {
        Some(r) => esp_println::print!("{}", r),
        None => esp_println::println!("(no {})", PRIME_FILE),
    }
    let p = trail_core::prime::plan(rec.as_ref(), shared::rtc_now());
    let st = shared::status();
    esp_println::println!(
        "prime: state={:?} would send now: time_acc_ms={:?} pos_acc_m={:?}",
        st.prime,
        p.time.map(|t| t.acc_ms),
        p.pos.map(|q| q.acc_cm / 100)
    );
}

/// ASSIST.UBX in the card's root: u-blox AssistNow data (UBX-MGA frames)
/// fetched by `trail assistnow`. Read a little per tick and handed to the
/// GNSS task frame by frame, so neither the IMU loop nor the receiver is
/// flooded. The file is left in place for the next boot; the receiver ignores
/// data that has expired. MGA-INI frames in it (the time an AssistNow Online
/// reply was made) are dropped: priming sends the time as it is now.
struct AssistFeed {
    file: RawFile,
    parser: trail_core::ubx::Parser,
    /// Frames parsed but not yet accepted by the GNSS task. A 128-byte read
    /// completes at most a handful of frames, so this never overflows: more
    /// is read only once it is empty.
    pending: heapless::Deque<shared::AssistFrame, 8>,
    frames: u32,
    eof: bool,
}

fn assist_open(vm: &Vm, s: &Session) -> Option<AssistFeed> {
    let file = vm
        .open_file_in_dir(s.root, "ASSIST.UBX", Mode::ReadOnly)
        .ok()?;
    let len = vm.file_length(file).unwrap_or(0);
    event(
        "assist_file",
        format_args!("ASSIST.UBX found, {} bytes", len),
    );
    Some(AssistFeed {
        file,
        parser: trail_core::ubx::Parser::new(),
        pending: heapless::Deque::new(),
        frames: 0,
        eof: false,
    })
}

/// Advance the feed a little; returns false once it has finished and closed
/// the file.
fn assist_step(vm: &Vm, a: &mut AssistFeed) -> bool {
    for _ in 0..4 {
        // Hand over what is parsed; stop at the first refusal (queue full).
        while let Some(f) = a.pending.pop_front() {
            if let Err(embassy_sync::channel::TrySendError::Full(f)) = shared::ASSIST.try_send(f) {
                let _ = a.pending.push_front(f);
                return true;
            }
        }
        if a.eof {
            // The empty frame tells the GNSS task the file is done.
            if shared::ASSIST.try_send(shared::AssistFrame::new()).is_err() {
                return true;
            }
            event(
                "assist_file",
                format_args!("{} UBX frames queued for the receiver", a.frames),
            );
            let _ = vm.close_file(a.file);
            return false;
        }
        let mut buf = [0u8; 128];
        let n = match vm.read(a.file, &mut buf) {
            Ok(0) | Err(_) => {
                a.eof = true;
                continue;
            }
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if let Some(info) = a.parser.push(b) {
                let mut frame = [0u8; 520];
                if let Some(len) =
                    trail_core::ubx::frame(info.class, info.id, a.parser.payload(&info), &mut frame)
                    && !trail_core::ubx::is_mga_ini(&frame[..len])
                {
                    let mut v = shared::AssistFrame::new();
                    if v.extend_from_slice(&frame[..len]).is_ok() && a.pending.push_back(v).is_ok()
                    {
                        a.frames += 1;
                        with_status(|s| s.assist_queued += 1);
                    }
                }
            }
        }
    }
    true
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
    // The first samples after enabling are garbage while the low-pass filters
    // settle (about 1.7 Hz at this rate; seen up to 3.7 g at rest for ~0.5 s):
    // record from one second after enabling.
    let imu_valid_from = Instant::now() + Duration::from_millis(1000);

    let mut bufs: [Vec<u8>; STREAMS] =
        core::array::from_fn(|_| Vec::with_capacity(WRITE_THRESHOLD + 512));
    let mut ticker = Ticker::every(Duration::from_millis(20));
    let mut last_flush = Instant::now();
    let mut last_retry: Option<Instant> = None;
    let mut want_recording = true;
    let mut imu_n: u32 = 0;
    let mut bytes: u64 = 0;
    let mut prime_planned = false;
    let mut prime_saved: Option<Instant> = None;
    // Wi-Fi fingerprints: their positions (read once), a boot-time locate
    // waiting for its scan, and a learning scan waiting for its result.
    let mut fp_index: Option<Vec<(i32, i32)>> = None;
    let mut locating: Option<(trail_core::prime::Plan, &'static str, u64)> = None;
    let mut learning: Option<(i32, i32, u16, i64, u64)> = None;
    let mut last_learn: Option<Instant> = None;

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

        let mut assist = session.as_ref().and_then(|s| assist_open(&vm, s));

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

            if let Some(a) = assist.as_mut()
                && !assist_step(&vm, a)
            {
                assist = None;
            }

            // Priming: plan once per boot when the card and the RTC are
            // read; record a good fix for the next power-on.
            if let Some(s) = session.as_ref() {
                let idx = fp_index.get_or_insert_with(|| {
                    let i = wififp_index(&vm, s);
                    event(
                        "wififp",
                        format_args!("{} fingerprints in {}", i.len(), WIFIFP_FILE),
                    );
                    with_status(|st| st.wififp = i.len() as u32);
                    i
                });
                if !prime_planned && shared::status().rtc_read_done {
                    prime_planned = true;
                    let (p, src) = prime_plan(&vm, s);
                    // No position worth sending: ask the Wi-Fi around us.
                    if wants_wifi(&p) && !idx.is_empty() {
                        let t = shared::now_ms();
                        shared::WIFI_SCAN_REQ.signal(());
                        locating = Some((p, src, t));
                    } else {
                        prime_commit(p, src);
                    }
                }
                if let Some((mut p, mut src, t)) = locating {
                    if let Some(scan) = scan_since(t) {
                        locating = None;
                        match wififp_locate(&vm, s, &scan) {
                            Some(e) => {
                                // Height: the old record's, else the
                                // barometer's (geoid unknown, so a wider
                                // accuracy).
                                let (alt_cm, floor_m) = match p.pos {
                                    Some(q) => (q.alt_cm, 0.0),
                                    None => ((shared::status().elevation_m * 100.0) as i32, 300.0),
                                };
                                let acc_m = e.acc_m.max(floor_m);
                                event(
                                    "wifi_locate",
                                    format_args!(
                                        "{} neighbours, best {:.0} dB, {} of {} heard: acc {:.0} m",
                                        e.k, e.best_db, scan.n, scan.heard, acc_m
                                    ),
                                );
                                p.pos = Some(trail_core::prime::PosAid {
                                    lat_e7: e.lat_e7,
                                    lon_e7: e.lon_e7,
                                    alt_cm,
                                    acc_cm: (acc_m * 100.0) as u32,
                                });
                                src = "wifi";
                            }
                            None => event(
                                "wifi_locate",
                                format_args!("no match ({} fixed of {} heard)", scan.n, scan.heard),
                            ),
                        }
                        prime_commit(p, src);
                    } else if shared::now_ms() - t > WIFI_SCAN_WAIT.as_millis() {
                        locating = None;
                        event(
                            "wifi_locate",
                            format_args!("no scan within {} s", WIFI_SCAN_WAIT.as_secs()),
                        );
                        prime_commit(p, src);
                    }
                }

                // Learning: a good fix at a new place, and the Wi-Fi heard there.
                let st = shared::status();
                if learning.is_none()
                    && st.fix_ok
                    && st.h_acc_mm <= WIFIFP_LEARN_ACC_MM
                    && last_learn.is_none_or(|t: Instant| t.elapsed() >= WIFIFP_LEARN_EVERY)
                    && let Some(utc) = st.utc
                    && wififp_is_new_place(idx, st.lat_e7, st.lon_e7)
                {
                    last_learn = Some(Instant::now());
                    let t = shared::now_ms();
                    shared::WIFI_SCAN_REQ.signal(());
                    learning = Some((
                        st.lat_e7,
                        st.lon_e7,
                        (st.h_acc_mm / 1000) as u16,
                        trail_core::pcf8563::unix_seconds(&utc),
                        t,
                    ));
                }
                if let Some((lat_e7, lon_e7, acc_m, unix, t)) = learning {
                    if let Some(scan) = scan_since(t) {
                        learning = None;
                        if scan.n > 0 {
                            let fp = trail_core::wififp::Fingerprint {
                                unix,
                                lat_e7,
                                lon_e7,
                                acc_m,
                                aps: scan.aps,
                                n: scan.n as u8,
                            };
                            if wififp_append(&vm, s, &fp).is_ok() {
                                idx.push((lat_e7, lon_e7));
                                with_status(|st| st.wififp = idx.len() as u32);
                                event(
                                    "wififp_learned",
                                    format_args!("#{}: {} access points", idx.len(), scan.n),
                                );
                            }
                        }
                    } else if shared::now_ms() - t > WIFI_SCAN_WAIT.as_millis() {
                        learning = None;
                    }
                }

                if prime_saved.is_none_or(|t| t.elapsed() >= PRIME_SAVE_EVERY)
                    && prime_save_fix(&vm, s)
                {
                    if prime_saved.is_none() {
                        event(
                            "prime_saved",
                            format_args!("first good fix recorded in {}", PRIME_FILE),
                        );
                    }
                    prime_saved = Some(Instant::now());
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
                        if let Some(a) = assist.take() {
                            let _ = vm.close_file(a.file);
                        }
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
                    StorageCmd::WifiLocate(scan) => match session.as_ref() {
                        Some(s) => match wififp_locate(&vm, s, &scan) {
                            Some(e) => esp_println::println!(
                                "wifi: at {:.7},{:.7} ±{:.0} m ({} neighbours, best {:.0} dB; {} of {} heard)",
                                e.lat_e7 as f64 / 1e7,
                                e.lon_e7 as f64 / 1e7,
                                e.acc_m,
                                e.k,
                                e.best_db,
                                scan.n,
                                scan.heard
                            ),
                            None => esp_println::println!(
                                "wifi: no match among {} fingerprints ({} of {} heard)",
                                fp_index.as_ref().map_or(0, |i| i.len()),
                                scan.n,
                                scan.heard
                            ),
                        },
                        None => esp_println::println!("wifi: card not mounted"),
                    },
                    StorageCmd::WifiForget => match session.as_ref() {
                        Some(s) => match write_root(&vm, s, WIFIFP_FILE, &[]) {
                            Ok(()) => {
                                if let Some(i) = fp_index.as_mut() {
                                    i.clear();
                                }
                                with_status(|st| st.wififp = 0);
                                event("wififp", format_args!("forgotten from the console"));
                                esp_println::println!("wifi: fingerprints forgotten");
                            }
                            Err(()) => {
                                esp_println::println!("wifi: FAILED to empty {}", WIFIFP_FILE)
                            }
                        },
                        None => esp_println::println!("wifi: card not mounted"),
                    },
                    StorageCmd::WifiLearn(fp) => match session.as_ref() {
                        Some(s) => match wififp_append(&vm, s, &fp) {
                            Ok(()) => {
                                let n = fp_index.as_mut().map_or(0, |i| {
                                    i.push((fp.lat_e7, fp.lon_e7));
                                    i.len()
                                });
                                with_status(|st| st.wififp = n as u32);
                                event(
                                    "wififp_learned",
                                    format_args!("#{} by hand: {} access points", n, fp.n),
                                );
                                esp_println::println!(
                                    "wifi: learned #{} with {} access points",
                                    n,
                                    fp.n
                                );
                            }
                            Err(()) => {
                                esp_println::println!("wifi: FAILED to write {}", WIFIFP_FILE)
                            }
                        },
                        None => esp_println::println!("wifi: card not mounted"),
                    },
                    StorageCmd::PrimeShow => prime_show(&vm, session.as_ref()),
                    StorageCmd::PrimeHost { unix, pos } => match session.as_ref() {
                        Some(s) => prime_host(&vm, s, unix, pos),
                        None => esp_println::println!(
                            "prime: card not mounted; sent to the receiver only"
                        ),
                    },
                    StorageCmd::PutAssist(data) => match session.as_ref() {
                        Some(s) => {
                            // The feed may hold the old file open.
                            if let Some(a) = assist.take() {
                                let _ = vm.close_file(a.file);
                            }
                            match write_root(&vm, s, "ASSIST.UBX", &data) {
                                Ok(()) => {
                                    event(
                                        "assist_file",
                                        format_args!(
                                            "ASSIST.UBX written over USB, {} bytes",
                                            data.len()
                                        ),
                                    );
                                    esp_println::println!(
                                        "assist: written {} bytes; feeding the receiver",
                                        data.len()
                                    );
                                    assist = assist_open(&vm, s);
                                }
                                Err(()) => {
                                    esp_println::println!("assist: FAILED to write ASSIST.UBX")
                                }
                            }
                        }
                        None => esp_println::println!("assist: FAILED card not mounted"),
                    },
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
        if let Some(a) = assist.take() {
            let _ = vm.close_file(a.file);
        }
        if let Some(s) = session.take() {
            close_session(&vm, s);
        }
        drop(vm);
    }
}
