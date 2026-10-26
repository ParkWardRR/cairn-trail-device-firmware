//! The small tasks: the PMU and RTC on I²C bus 1, the BOOT button, the GNSS
//! PPS line, the USB console, and a periodic status line.

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
        // implausible or more than 2 s away from it.
        if !rtc_set
            && n.is_multiple_of(10)
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
            rtc_set = true;
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
         power battery={} vbat={}mV vbus={}mV charging={} soc={}%  marks={} display={}",
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
    );
}

const HELP: &str = "commands: help | stat | ls | cat <S0001/GNSS.CSV or GNSS.CSV> | flush | stop | start | mark | reboot";

/// Line commands over the USB serial port.
#[embassy_executor::task]
pub async fn console_task(mut rx: UsbSerialJtagRx<'static, Blocking>) {
    let mut line: String<64> = String::new();
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
