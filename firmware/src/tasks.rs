//! The small tasks: the PMU and RTC on I²C bus 1, the BOOT button, the GNSS
//! PPS line, the USB console, and a periodic status line.

extern crate alloc;

use core::fmt::Write as _;

use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_hal::Blocking;
use esp_hal::gpio::Input;
use esp_hal::i2c::master::I2c;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx;
use heapless::String;
use trail_core::battery;
use trail_core::pcf8563::{self, DateTime};

use crate::pmu::Pmu;
use crate::shared::{self, STORAGE_CMDS, StorageCmd, Stream, emit, event, with_status};

pub type I2c1 = I2c<'static, Blocking>;

fn rtc_read(i2c: &mut I2c1) -> Option<(DateTime, bool)> {
    let mut r = [0u8; 7];
    i2c.write_read(pcf8563::ADDR, &[pcf8563::SECONDS], &mut r)
        .ok()?;
    Some(pcf8563::decode(&r))
}

fn rtc_write(i2c: &mut I2c1, t: &DateTime) -> bool {
    let enc = pcf8563::encode(t);
    let mut b = [0u8; 8];
    b[0] = pcf8563::SECONDS;
    b[1..].copy_from_slice(&enc);
    i2c.write(pcf8563::ADDR, &b).is_ok()
}

#[embassy_executor::task]
pub async fn pmu_task(mut pmu: Pmu<I2c1>) {
    // The RTC as found.
    let rtc_at_boot = rtc_read(pmu.i2c());
    match rtc_at_boot {
        Some((t, vl)) => event(
            "rtc",
            format_args!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02} voltage_low={} plausible={}",
                t.year,
                t.month,
                t.day,
                t.hour,
                t.min,
                t.sec,
                vl,
                pcf8563::plausible(&t, vl)
            ),
        ),
        None => event("rtc", format_args!("PCF8563 did not answer")),
    }
    with_status(|s| {
        s.rtc_base = rtc_at_boot
            .filter(|(t, vl)| pcf8563::plausible(t, *vl))
            .map(|(t, _)| (pcf8563::unix_seconds(&t), shared::now_ms()));
        s.rtc_read_done = true;
    });
    let mut rtc_set = false;

    let mut ticker = Ticker::every(Duration::from_millis(100));
    let mut n: u32 = 0;
    loop {
        ticker.next().await;
        n = n.wrapping_add(1);

        if let Ok(b) = pmu.buttons() {
            if b.short {
                let on = !shared::status().display_on;
                with_status(|s| s.display_on = on);
                event(
                    "button",
                    format_args!("PWR short: display {}", if on { "on" } else { "off" }),
                );
            }
            if b.long {
                event("button", format_args!("PWR long"));
            }
        }

        if n % 50 == 1 {
            match pmu.telemetry() {
                Ok(t) => {
                    let curve = if t.battery {
                        battery::percent(t.vbat_mv)
                    } else {
                        0
                    };
                    emit(
                        Stream::Power,
                        format_args!(
                            "{},{},{},{},{},{},{},{},{:.1},0x{:02x},0x{:02x}",
                            t.vbat_mv,
                            t.vbus_mv,
                            t.vsys_mv,
                            t.battery as u8,
                            t.charging as u8,
                            t.vbus_good as u8,
                            t.soc_gauge_pct,
                            curve,
                            t.die_temp_c,
                            t.dcdc,
                            t.ldo
                        ),
                    );
                    if t.ldo & crate::pmu::ldo::ALDO3_LORA != 0 {
                        event("lora", format_args!("ALDO3 found ON; turning it off"));
                        let _ = pmu.configure(false, &mut esp_hal::delay::Delay::new());
                    }
                    with_status(|s| {
                        s.vbat_mv = t.vbat_mv;
                        s.vbus_mv = t.vbus_mv;
                        s.battery = t.battery;
                        s.charging = t.charging;
                        s.soc_pct = if t.soc_gauge_pct > 0 {
                            t.soc_gauge_pct
                        } else {
                            curve
                        };
                    });
                }
                Err(_) => log::warn!("PMU telemetry read failed"),
            }
        }

        // Set the RTC once per boot from a resolved GNSS time, if the RTC is
        // implausible or more than 2 s away from it. Only with a fix: a
        // receiver primed from the RTC reports that same time as resolved
        // before it has heard a satellite, and checking the RTC against its
        // own echo would make it look trustworthy.
        if !rtc_set
            && n.is_multiple_of(10)
            && shared::status().fix_ok
            && let Some(utc) = shared::status().utc
            && pcf8563::plausible(&utc, false)
        {
            let current = rtc_read(pmu.i2c());
            let off = current
                .filter(|(t, vl)| pcf8563::plausible(t, *vl))
                .map(|(t, _)| (pcf8563::unix_seconds(&t) - pcf8563::unix_seconds(&utc)).abs());
            if off.is_none_or(|o| o > 2) {
                let ok = rtc_write(pmu.i2c(), &utc);
                event(
                    "rtc_set",
                    format_args!(
                        "from GNSS {:04}-{:02}-{:02}T{:02}:{:02}:{:02} ok={} previous_offset_s={:?}",
                        utc.year, utc.month, utc.day, utc.hour, utc.min, utc.sec, ok, off
                    ),
                );
            } else {
                event(
                    "rtc_check",
                    format_args!("RTC agrees with GNSS within {:?} s", off),
                );
            }
            let unix = pcf8563::unix_seconds(&utc);
            with_status(|s| {
                s.rtc_base = Some((unix, shared::now_ms()));
                s.rtc_checked_unix = unix;
            });
            rtc_set = true;
        }

        // The computer's clock, sent over the console (`prime time`).
        if let Some(unix) = shared::RTC_SET.try_take() {
            let t = pcf8563::from_unix(unix);
            let ok = pcf8563::plausible(&t, false) && rtc_write(pmu.i2c(), &t);
            event(
                "rtc_set",
                format_args!(
                    "from the computer {:04}-{:02}-{:02}T{:02}:{:02}:{:02} ok={}",
                    t.year, t.month, t.day, t.hour, t.min, t.sec, ok
                ),
            );
            if ok {
                with_status(|s| {
                    s.rtc_base = Some((unix, shared::now_ms()));
                    s.rtc_checked_unix = unix;
                });
            }
        }
    }
}

/// BOOT (GPIO 0): a short press marks the spot; a 1.5 s hold toggles the
/// display. Any press wakes the display.
#[embassy_executor::task]
pub async fn button_task(mut boot: Input<'static>) {
    loop {
        boot.wait_for_falling_edge().await;
        let down = Instant::now();
        Timer::after(Duration::from_millis(30)).await;
        if boot.is_high() {
            continue; // bounce
        }
        boot.wait_for_rising_edge().await;
        let held = down.elapsed();
        if held >= Duration::from_millis(1500) {
            let on = !shared::status().display_on;
            with_status(|s| s.display_on = on);
            event(
                "button",
                format_args!("BOOT long: display {}", if on { "on" } else { "off" }),
            );
        } else {
            let m = with_status(|s| {
                s.marks += 1;
                s.display_on = true;
                s.marks
            });
            let st = shared::status();
            event(
                "mark",
                format_args!(
                    "#{} fix_ok={} lat_e7={} lon_e7={} elevation_m={:.1}",
                    m, st.fix_ok as u8, st.lat_e7, st.lon_e7, st.elevation_m
                ),
            );
        }
    }
}

/// The receiver's 1 Hz time pulse on GPIO 6, timestamped against the
/// monotonic clock: the record of how the two clocks drift.
#[embassy_executor::task]
pub async fn pps_task(mut pps: Input<'static>) {
    let mut count: u32 = 0;
    loop {
        pps.wait_for_rising_edge().await;
        let t_us = Instant::now().as_micros();
        count = count.wrapping_add(1);
        emit(Stream::Pps, format_args!("{}", t_us));
        if count == 1 {
            event("pps", format_args!("first pulse"));
        }
    }
}

fn print_stat() {
    let s = shared::status();
    esp_println::println!(
        "cairn-trail {} ({}) {}{}\n\
         session S{:04} sd_ok={} recording={} bytes={} dropped={} write_errors={}\n\
         gnss seen={} model={} fix_ok={} type={} sv={}/{} lat={:.6} lon={:.6} hmsl={:.1}m h_acc={:.1}m v_acc={:.1}m pvt={} ubx_crc_err={}\n\
         baro ok={} p={:.1}Pa t={:.2}C rh={:.1}% elev={:.1}m gain={:.1}m loss={:.1}m anchored={}\n\
         imu ok={} accel_mg={:?}  mag ok={} heading={:.0}\n\
         power battery={} vbat={}mV vbus={}mV charging={} soc={}%  marks={} display={}\n\
         assistnow queued={} sent={} accepted={} not_used={}\n\
         prime {:?} time_acc={}ms pos_acc={}m rtc_now={:?} rtc_checked={} wifi_fingerprints={}",
        shared::VERSION,
        shared::FLAVOUR,
        shared::COMMIT,
        shared::DIRTY,
        s.session,
        s.sd_ok,
        s.recording,
        s.sd_bytes,
        s.dropped_records,
        s.write_errors,
        s.gnss_seen,
        core::str::from_utf8(&s.gnss_model)
            .unwrap_or("?")
            .trim_end_matches('\0'),
        s.fix_ok,
        s.fix_type,
        s.num_sv,
        s.sats_visible,
        s.lat_e7 as f64 / 1e7,
        s.lon_e7 as f64 / 1e7,
        s.hmsl_mm as f32 / 1000.0,
        s.h_acc_mm as f32 / 1000.0,
        s.v_acc_mm as f32 / 1000.0,
        s.pvt_count,
        s.ubx_checksum_errors,
        s.baro_ok,
        s.pressure_pa,
        s.temp_c,
        s.humidity_pct,
        s.elevation_m,
        s.gain_m,
        s.loss_m,
        s.qnh_anchored,
        s.imu_ok,
        s.accel_mg,
        s.mag_ok,
        s.heading_deg,
        s.battery,
        s.vbat_mv,
        s.vbus_mv,
        s.charging,
        s.soc_pct,
        s.marks,
        s.display_on,
        s.assist_queued,
        s.assist_sent,
        s.assist_accepted,
        s.assist_rejected,
        s.prime,
        s.prime_time_acc_ms,
        s.prime_pos_acc_m,
        shared::rtc_now(),
        s.rtc_checked_unix,
        s.wififp,
    );
}

const HELP: &str = "commands: help | stat | ls | cat <S0001/GNSS.CSV or GNSS.CSV> | flush | stop | start | mark | gnssid | vectors | gnss cold|warm|hot | wifi scan|locate|learn <lat> <lon>|forget | prime show|time <unix_ms>|pos <lat> <lon> [alt_m] [acc_m] | assist begin|data|commit | net | prov show|begin|data <hex>|commit <crc32>|erase | reboot";

/// A provisioning payload arriving over the console in hex lines.
static PROV_BUF: embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<alloc::vec::Vec<u8>>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(alloc::vec::Vec::new()));

fn hex_decode(s: &str, out: &mut alloc::vec::Vec<u8>) -> bool {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return false;
    }
    for pair in b.chunks(2) {
        let v = |c: u8| (c as char).to_digit(16);
        match (v(pair[0]), v(pair[1])) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            _ => return false,
        }
    }
    true
}

type ByteBuf = embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<alloc::vec::Vec<u8>>,
>;

/// An AssistNow file arriving over the console, for ASSIST.UBX.
static ASSIST_BUF: ByteBuf =
    embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(alloc::vec::Vec::new()));
/// Largest ASSIST.UBX accepted over the console. A day of AssistNow Online
/// data for every constellation is ~10 KB; Offline for a few weeks is larger.
const ASSIST_MAX: usize = 96 * 1024;

/// The `begin` / `data <hex>` / `commit <crc32>` steps shared by `prov` and
/// `assist`. Each reply starts with `<name>:`; `data` answers with the running
/// byte count so the sender can tell a lost byte at once. Returns the payload
/// once `commit` names its CRC-32; other subcommands return None and are
/// left to the caller.
fn chunked(
    name: &str,
    buf: &ByteBuf,
    max: usize,
    sub: &str,
    rest: &str,
) -> Option<alloc::vec::Vec<u8>> {
    match sub {
        "begin" => {
            buf.lock(|b| b.borrow_mut().clear());
            esp_println::println!("{}: ready", name);
        }
        "data" => {
            let (ok, n) = buf.lock(|b| {
                let mut b = b.borrow_mut();
                if b.len() + rest.len() / 2 > max {
                    return (false, b.len());
                }
                (hex_decode(rest, &mut b), b.len())
            });
            let verdict = if ok {
                "ok"
            } else if n + rest.len() / 2 > max {
                "FULL"
            } else {
                "BAD HEX"
            };
            esp_println::println!("{}: {} {}", name, verdict, n);
        }
        "commit" => {
            let want = u32::from_str_radix(rest, 16).ok();
            let data = buf.lock(|b| core::mem::take(&mut *b.borrow_mut()));
            let got = crate::keys::crc32(&data);
            if want != Some(got) {
                esp_println::println!(
                    "{}: CRC MISMATCH got {:08x} want {:?}; nothing written",
                    name,
                    got,
                    rest
                );
                return None;
            }
            return Some(data);
        }
        _ => {}
    }
    None
}

async fn assist(arg: &str) {
    let (sub, rest) = arg
        .split_once(' ')
        .map(|(a, b)| (a, b.trim()))
        .unwrap_or((arg, ""));
    if !matches!(sub, "begin" | "data" | "commit") {
        esp_println::println!("assist: begin | data <hex> | commit <crc32>  (writes ASSIST.UBX)");
        return;
    }
    if let Some(data) = chunked("assist", &ASSIST_BUF, ASSIST_MAX, sub, rest) {
        STORAGE_CMDS.send(StorageCmd::PutAssist(data)).await;
    }
}

/// `prime`: show what the receiver will be primed with, or give it the
/// computer's time (`prime time <unix_ms>`) or a position (`prime pos <lat>
/// <lon> [alt_m] [acc_m]`, height above the WGS84 ellipsoid). Both are sent
/// to the receiver at once and recorded in PRIME.TXT for the next power-on.
async fn prime(arg: &str) {
    let mut words = arg.split_whitespace();
    match words.next().unwrap_or("show") {
        "show" => STORAGE_CMDS.send(StorageCmd::PrimeShow).await,
        "time" => {
            let Some(ms) = words.next().and_then(|w| w.parse::<i64>().ok()) else {
                esp_println::println!("prime: time <unix milliseconds>");
                return;
            };
            let unix = ms.div_euclid(1000);
            let t = pcf8563::from_unix(unix);
            if !pcf8563::plausible(&t, false) {
                esp_println::println!("prime: implausible time {}", unix);
                return;
            }
            let mut f = shared::AssistFrame::new();
            let mut raw = [0u8; 40];
            // A second covers the USB round trip and the console's polling.
            if let Some(n) = trail_core::ubx::mga_ini_time_utc(
                &t,
                (ms.rem_euclid(1000) * 1_000_000) as u32,
                1_000,
                &mut raw,
            ) {
                let _ = f.extend_from_slice(&raw[..n]);
                let _ = shared::ASSIST.try_send(f);
            }
            shared::RTC_SET.signal(unix);
            STORAGE_CMDS
                .send(StorageCmd::PrimeHost {
                    unix: Some(unix),
                    pos: None,
                })
                .await;
            esp_println::println!(
                "prime: time {:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z sent to the receiver and the RTC",
                t.year,
                t.month,
                t.day,
                t.hour,
                t.min,
                t.sec
            );
        }
        "pos" => {
            let fixed = |w: Option<&str>, d| w.and_then(|w| trail_core::prime::parse_fixed(w, d));
            let lat = fixed(words.next(), 7).and_then(|v| i32::try_from(v).ok());
            let lon = fixed(words.next(), 7).and_then(|v| i32::try_from(v).ok());
            let alt_cm = fixed(words.next(), 2)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0);
            let acc_cm = fixed(words.next(), 2)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(trail_core::prime::MANUAL_ACC_CM);
            let (Some(lat), Some(lon)) = (lat, lon) else {
                esp_println::println!("prime: pos <lat> <lon> [alt_m] [acc_m]");
                return;
            };
            if !(-900_000_000..=900_000_000).contains(&lat)
                || !(-1_800_000_000..=1_800_000_000).contains(&lon)
            {
                esp_println::println!("prime: position out of range");
                return;
            }
            let mut f = shared::AssistFrame::new();
            let mut raw = [0u8; 40];
            if let Some(n) = trail_core::ubx::mga_ini_pos_llh(lat, lon, alt_cm, acc_cm, &mut raw) {
                let _ = f.extend_from_slice(&raw[..n]);
                let _ = shared::ASSIST.try_send(f);
            }
            STORAGE_CMDS
                .send(StorageCmd::PrimeHost {
                    unix: None,
                    pos: Some((lat, lon, alt_cm, acc_cm)),
                })
                .await;
            esp_println::println!(
                "prime: position sent to the receiver, accuracy {} m",
                acc_cm / 100
            );
        }
        _ => {
            esp_println::println!("prime: show | time <unix_ms> | pos <lat> <lon> [alt_m] [acc_m]")
        }
    }
}

/// Ask for a Wi-Fi scan and wait for it (up to 8 s).
async fn console_scan() -> Option<shared::WifiScan> {
    let t = shared::now_ms();
    shared::WIFI_SCAN_REQ.signal(());
    for _ in 0..80 {
        Timer::after(Duration::from_millis(100)).await;
        if let Some(s) = shared::WIFI_SCAN.lock(|s| s.borrow().filter(|x| x.at_ms >= t)) {
            return Some(s);
        }
    }
    esp_println::println!("wifi: no scan result within 8 s");
    None
}

/// `wifi scan` lists what is heard; `wifi locate` matches it against the
/// fingerprints on the card; `wifi learn <lat> <lon> [acc_m]` records what is
/// heard as a fingerprint at a position given by hand (a place the GNSS
/// cannot see, such as indoors at home).
async fn wifi(arg: &str) {
    let mut words = arg.split_whitespace();
    match words.next().unwrap_or("") {
        "scan" => {
            let Some(s) = console_scan().await else {
                return;
            };
            esp_println::println!(
                "wifi: {} heard, {} fixed kept (strongest first):",
                s.heard,
                s.n
            );
            for ap in &s.aps[..s.n] {
                esp_println::println!("  {:02x?} {} dBm", ap.bssid, ap.rssi);
            }
        }
        "forget" => STORAGE_CMDS.send(StorageCmd::WifiForget).await,
        "locate" => {
            if let Some(s) = console_scan().await {
                STORAGE_CMDS.send(StorageCmd::WifiLocate(s)).await;
            }
        }
        "learn" => {
            let fixed = |w: Option<&str>, d| w.and_then(|w| trail_core::prime::parse_fixed(w, d));
            let lat = fixed(words.next(), 7).and_then(|v| i32::try_from(v).ok());
            let lon = fixed(words.next(), 7).and_then(|v| i32::try_from(v).ok());
            let acc_m = words
                .next()
                .and_then(|w| w.parse::<u16>().ok())
                .unwrap_or(20);
            let (Some(lat_e7), Some(lon_e7)) = (lat, lon) else {
                esp_println::println!("wifi: learn <lat> <lon> [acc_m]");
                return;
            };
            let Some(s) = console_scan().await else {
                return;
            };
            if s.n == 0 {
                esp_println::println!("wifi: nothing fixed heard; nothing learned");
                return;
            }
            let unix = if shared::status().rtc_checked_unix > 0 {
                shared::rtc_now().unwrap_or(0)
            } else {
                0
            };
            let fp = trail_core::wififp::Fingerprint {
                unix,
                lat_e7,
                lon_e7,
                acc_m,
                aps: s.aps,
                n: s.n as u8,
            };
            STORAGE_CMDS.send(StorageCmd::WifiLearn(fp)).await;
        }
        _ => esp_println::println!("wifi: scan | locate | learn <lat> <lon> [acc_m] | forget"),
    }
}

fn prov(arg: &str) {
    let (sub, rest) = arg
        .split_once(' ')
        .map(|(a, b)| (a, b.trim()))
        .unwrap_or((arg, ""));
    match sub {
        "show" | "" => esp_println::println!(
            "prov: mac={} {}",
            crate::mac_string(),
            crate::keys::summary()
        ),
        "begin" | "data" | "commit" => {
            let Some(data) = chunked("prov", &PROV_BUF, crate::keys::SIZE, sub, rest) else {
                return;
            };
            match crate::keys::write(&data) {
                Ok(()) => {
                    event(
                        "keys",
                        format_args!("provisioned over USB: {}", crate::keys::summary()),
                    );
                    esp_println::println!(
                        "prov: written {} bytes; reboot to start Wi-Fi",
                        data.len()
                    );
                }
                Err(e) => esp_println::println!("prov: FAILED {}", e),
            }
        }
        "erase" => match crate::keys::erase() {
            Ok(()) => esp_println::println!("prov: erased"),
            Err(e) => esp_println::println!("prov: FAILED {}", e),
        },
        _ => esp_println::println!("prov: show | begin | data <hex> | commit <crc32> | erase"),
    }
}

/// Line commands over the USB serial port.
#[embassy_executor::task]
pub async fn console_task(mut rx: UsbSerialJtagRx<'static, Blocking>) {
    let mut line: String<1100> = String::new();
    loop {
        Timer::after(Duration::from_millis(10)).await;
        while let Ok(b) = rx.read_byte() {
            match b {
                b'\r' | b'\n' => {
                    let cmd = line.trim();
                    if !cmd.is_empty() {
                        run(cmd).await;
                    }
                    line.clear();
                }
                0x08 | 0x7F => {
                    line.pop();
                }
                b if b.is_ascii() && !b.is_ascii_control() => {
                    let _ = line.push(b as char);
                }
                _ => {}
            }
        }
    }
}

async fn run(cmd: &str) {
    let (word, arg) = cmd
        .split_once(' ')
        .map(|(w, a)| (w, a.trim()))
        .unwrap_or((cmd, ""));
    match word {
        "help" | "?" => esp_println::println!("{}", HELP),
        "stat" => print_stat(),
        "ls" => STORAGE_CMDS.send(StorageCmd::List).await,
        "cat" => {
            let mut p: String<40> = String::new();
            let _ = p.push_str(arg);
            STORAGE_CMDS.send(StorageCmd::Cat(p)).await
        }
        "flush" => STORAGE_CMDS.send(StorageCmd::Flush).await,
        "stop" => STORAGE_CMDS.send(StorageCmd::Stop).await,
        "start" => STORAGE_CMDS.send(StorageCmd::Start).await,
        "prov" => prov(arg),
        "prime" => prime(arg).await,
        "wifi" => wifi(arg).await,
        "vectors" => {
            crate::vectors::run();
        }
        "gnss" => {
            // Restart the receiver forgetting some of what it knows, to
            // measure time to first fix and test priming. Follow `cold` with
            // `reboot` to see the logger prime a receiver that woke empty.
            let mask = match arg {
                "cold" => Some(0xFFFF),
                "warm" => Some(0x0001),
                "hot" => Some(0x0000),
                _ => None,
            };
            let mut raw = [0u8; 16];
            match mask.and_then(|m| trail_core::ubx::cfg_rst(m, &mut raw)) {
                Some(n) => {
                    let mut f = shared::AssistFrame::new();
                    let _ = f.extend_from_slice(&raw[..n]);
                    let _ = shared::GNSS_CMD.try_send(f);
                    event(
                        "gnss_restart",
                        format_args!("{} start requested from the console", arg),
                    );
                    esp_println::println!("gnss: {} start sent", arg);
                }
                None => esp_println::println!("gnss: cold | warm | hot"),
            }
        }
        "assist" => assist(arg).await,
        "net" => {
            let s = shared::status();
            esp_println::println!(
                "net: wifi_up={} checkins={} errors={} | {}",
                s.wifi_up,
                s.checkins,
                s.checkin_errors,
                crate::keys::summary()
            );
        }
        "gnssid" => {
            // The receiver's identity frames in hex, whole, for AssistNow
            // registration (scripts/assistnow.py reads these lines).
            crate::shared::GNSS_IDENTITY.lock(|g| {
                let g = g.borrow();
                esp_println::print!("UBX-MON-VER ");
                for b in g.mon_ver.iter() {
                    esp_println::print!("{:02x}", b);
                }
                esp_println::print!("\nUBX-SEC-UNIQID ");
                for b in g.uniqid.iter() {
                    esp_println::print!("{:02x}", b);
                }
                esp_println::println!();
            });
        }
        "mark" => {
            let m = with_status(|s| {
                s.marks += 1;
                s.marks
            });
            event("mark", format_args!("#{} from console", m));
        }
        "reboot" => {
            STORAGE_CMDS.send(StorageCmd::Stop).await;
            Timer::after(Duration::from_millis(500)).await;
            esp_hal::system::software_reset();
        }
        _ => esp_println::println!("unknown: {}\n{}", cmd, HELP),
    }
}

#[cfg(feature = "verbose")]
const STATUS_EVERY: Duration = Duration::from_secs(10);
#[cfg(not(feature = "verbose"))]
const STATUS_EVERY: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub async fn status_task() {
    let mut ticker = Ticker::every(STATUS_EVERY);
    loop {
        ticker.next().await;
        let s = shared::status();
        let mut fix: String<48> = String::new();
        if s.fix_ok {
            let _ = write!(
                fix,
                "{}D {}sv h{:.1}m",
                s.fix_type,
                s.num_sv,
                s.h_acc_mm as f32 / 1000.0
            );
        } else {
            let _ = write!(fix, "nofix {}/{}sv", s.num_sv, s.sats_visible);
        }
        log::info!(
            "S{:04} sd={} {}B drop={} | gnss {} pvt={} | {:.1}hPa elev={:.1}m +{:.0}/-{:.0} | imu={} mag={} | vbus={}mV bat={}mV",
            s.session,
            s.sd_ok as u8,
            s.sd_bytes,
            s.dropped_records,
            fix,
            s.pvt_count,
            s.pressure_pa / 100.0,
            s.elevation_m,
            s.gain_m,
            s.loss_m,
            s.imu_ok as u8,
            s.mag_ok as u8,
            s.vbus_mv,
            s.vbat_mv
        );
    }
}
