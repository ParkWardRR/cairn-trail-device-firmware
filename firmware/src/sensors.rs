//! I²C bus 0: the BME280 barometer, the magnetometer and the display. One task
//! owns the bus; ALDO1 powers all three and the bus's pull-ups.

use core::fmt::Write as _;

use embassy_time::{Duration, Instant, Ticker};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use esp_hal::Blocking;
use heapless::String;
use trail_core::bme280;
use trail_core::elevation::Elevation;

use crate::display::Sh1106;
use crate::shared::{self, Stream, emit, event, with_status};

type Bus = esp_hal::i2c::master::I2c<'static, Blocking>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magnetometer {
    Qmc6309,
    Qmc6310U,
    Qmc6310N,
}

impl Magnetometer {
    fn addr(self) -> u8 {
        match self {
            Magnetometer::Qmc6309 => 0x7C,
            Magnetometer::Qmc6310U => 0x1C,
            Magnetometer::Qmc6310N => 0x3C,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Magnetometer::Qmc6309 => "QMC6309 (carrier V3.1)",
            Magnetometer::Qmc6310U => "QMC6310U (carrier V3.0)",
            Magnetometer::Qmc6310N => "QMC6310N",
        }
    }
}

/// Read registers; if the device refuses a repeated-start read (the QMC6309
/// does after power-up), fall back to a write, a stop, then a read.
fn read_regs(bus: &mut Bus, addr: u8, reg: u8, buf: &mut [u8]) -> bool {
    if bus.write_read(addr, &[reg], buf).is_ok() {
        return true;
    }
    bus.write(addr, &[reg]).is_ok() && bus.read(addr, buf).is_ok()
}

fn read_reg(bus: &mut Bus, addr: u8, reg: u8) -> Option<u8> {
    let mut b = [0u8];
    read_regs(bus, addr, reg, &mut b).then_some(b[0])
}

/// Every address on the bus that acknowledges.
pub fn scan(bus: &mut Bus) -> heapless::Vec<u8, 16> {
    let mut found = heapless::Vec::new();
    // 0x78..=0x7F are reserved (10-bit addressing); probing them wedges the
    // QMC6309 at 0x7C, which is identified by chip ID instead.
    for addr in 0x08..0x78u8 {
        let mut b = [0u8];
        if bus.read(addr, &mut b).is_ok() {
            let _ = found.push(addr);
        }
    }
    found
}

/// Identify the magnetometer by chip ID, and the OLED by elimination: a
/// QMC6310N shares the OLED's default address.
pub fn identify(bus: &mut Bus) -> (Option<Magnetometer>, Option<u8>, Option<u8>) {
    let mag = if read_reg(bus, 0x7C, 0x00) == Some(0x90) {
        Some(Magnetometer::Qmc6309)
    } else if read_reg(bus, 0x1C, 0x00) == Some(0x80) {
        Some(Magnetometer::Qmc6310U)
    } else if read_reg(bus, 0x3C, 0x00) == Some(0x80) {
        Some(Magnetometer::Qmc6310N)
    } else {
        None
    };
    let oled = [0x3C, 0x3D].into_iter().find(|&a| {
        !(mag == Some(Magnetometer::Qmc6310N) && a == 0x3C) && {
            let mut b = [0u8];
            bus.read(a, &mut b).is_ok()
        }
    });
    let bme = [0x77, 0x76]
        .into_iter()
        .find(|&a| read_reg(bus, a, bme280::CHIP_ID_REG) == Some(bme280::CHIP_ID));
    (mag, oled, bme)
}

fn bme_init(bus: &mut Bus, addr: u8) -> Option<bme280::Calib> {
    bus.write(addr, &[bme280::RESET_REG, bme280::RESET_CMD])
        .ok()?;
    esp_hal::delay::Delay::new().delay_millis(5);
    let mut a = [0u8; 26];
    let mut b = [0u8; 7];
    bus.write_read(addr, &[bme280::CALIB_A_REG], &mut a).ok()?;
    bus.write_read(addr, &[bme280::CALIB_B_REG], &mut b).ok()?;
    // ctrl_hum only takes effect after a write to ctrl_meas.
    bus.write(addr, &[bme280::CTRL_HUM, bme280::CTRL_HUM_VALUE])
        .ok()?;
    bus.write(addr, &[bme280::CONFIG, bme280::CONFIG_VALUE])
        .ok()?;
    bus.write(addr, &[bme280::CTRL_MEAS, bme280::CTRL_MEAS_VALUE])
        .ok()?;
    Some(bme280::Calib::from_regs(&a, &b))
}

fn mag_init(bus: &mut Bus, mag: Magnetometer) -> bool {
    let a = mag.addr();
    // Soft reset, then range ±8 G with set/reset on. Each sample is a single
    // measurement triggered by the task, so the output data rate does not
    // matter and the two families share one path.
    let ok = bus.write(a, &[0x0B, 0x80]).is_ok();
    esp_hal::delay::Delay::new().delay_millis(5);
    let range = match mag {
        Magnetometer::Qmc6309 => 0b10 << 2,
        _ => 0b10 << 2,
    };
    ok && bus.write(a, &[0x0B, range]).is_ok()
        && (mag == Magnetometer::Qmc6309 || bus.write(a, &[0x29, 0x06]).is_ok())
}

fn mag_trigger(bus: &mut Bus, mag: Magnetometer) -> bool {
    // CMD1: mode = single measurement (0b10); oversampling defaults.
    bus.write(mag.addr(), &[0x0A, 0b10]).is_ok()
}

fn mag_read(bus: &mut Bus, mag: Magnetometer) -> Option<[i16; 3]> {
    let status = read_reg(bus, mag.addr(), 0x09)?;
    if status & 0x01 == 0 {
        return None;
    }
    let mut d = [0u8; 6];
    if !read_regs(bus, mag.addr(), 0x01, &mut d) {
        return None;
    }
    Some([
        i16::from_le_bytes([d[0], d[1]]),
        i16::from_le_bytes([d[2], d[3]]),
        i16::from_le_bytes([d[4], d[5]]),
    ])
}

fn draw(display: &mut Sh1106) {
    let s = shared::status();
    display.clear();
    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let mut lines: [String<24>; 6] = Default::default();
    let _ = write!(
        lines[0],
        "S{:04} {} {}",
        s.session,
        if s.sd_ok && s.recording {
            "REC"
        } else if s.sd_ok {
            "SD"
        } else {
            "NO SD"
        },
        if s.dropped_records > 0 { "!" } else { "" }
    );
    if s.fix_ok {
        let _ = write!(
            lines[1],
            "GPS {}D {}sv {:.1}m",
            s.fix_type,
            s.num_sv,
            s.h_acc_mm as f32 / 1000.0
        );
        let _ = write!(
            lines[2],
            "{:.5} {:.5}",
            s.lat_e7 as f32 / 1e7,
            s.lon_e7 as f32 / 1e7
        );
    } else if s.gnss_seen {
        let _ = write!(lines[1], "GPS search {}/{}sv", s.num_sv, s.sats_visible);
        let _ = write!(lines[2], "no fix yet");
    } else {
        let _ = write!(lines[1], "GPS not heard");
    }
    let _ = write!(
        lines[3],
        "Alt {:.0}m Elev {:.0}m",
        s.hmsl_mm as f32 / 1000.0,
        s.elevation_m
    );
    let _ = write!(
        lines[4],
        "+{:.0}m -{:.0}m {:.0}hPa",
        s.gain_m,
        s.loss_m,
        s.pressure_pa / 100.0
    );
    if s.battery {
        let _ = write!(
            lines[5],
            "Bat {}% {}mV{}",
            s.soc_pct,
            s.vbat_mv,
            if s.charging { "+" } else { "" }
        );
    } else {
        let _ = write!(lines[5], "USB {}mV no cell", s.vbus_mv);
    }
    for (i, l) in lines.iter().enumerate() {
        let _ = Text::with_baseline(l, Point::new(0, i as i32 * 10 + 2), style, Baseline::Top)
            .draw(display);
    }
}

/// How long the display stays on after a button press.
#[cfg(feature = "verbose")]
const DISPLAY_TIMEOUT: Option<Duration> = None;
#[cfg(not(feature = "verbose"))]
const DISPLAY_TIMEOUT: Option<Duration> = Some(Duration::from_secs(60));

#[embassy_executor::task]
pub async fn sensors_task(mut bus: Bus) {
    let found = scan(&mut bus);
    event("i2c0_scan", format_args!("{:02x?}", found.as_slice()));
    #[cfg(feature = "verbose")]
    for a in [0x7Cu8, 0x1C, 0x3C] {
        let mut r = [0u8; 12];
        let ok = read_regs(&mut bus, a, 0x00, &mut r);
        event(
            "diag_i2c",
            format_args!("addr 0x{:02x} ok={} regs00..0b={:02x?}", a, ok, r),
        );
    }
    let (mag, oled, bme) = identify(&mut bus);
    event(
        "i2c0_devices",
        format_args!(
            "mag={} oled={:?} bme280={:?}",
            mag.map(|m| m.name()).unwrap_or("none"),
            oled,
            bme
        ),
    );

    let calib = bme.and_then(|a| bme_init(&mut bus, a).map(|c| (a, c)));
    with_status(|s| s.baro_ok = calib.is_some());
    if calib.is_none() {
        event("baro", format_args!("BME280 not initialised"));
    }
    let mag_ok = mag.map(|m| mag_init(&mut bus, m)).unwrap_or(false);
    with_status(|s| s.mag_ok = mag_ok);

    let mut display = oled.map(Sh1106::new);
    if let Some(d) = display.as_mut()
        && d.init(&mut bus).is_err()
    {
        event("display", format_args!("SH1106 init failed"));
        display = None;
    }

    let mut elevation = Elevation::default();
    let mut last_gnss_count = 0u32;
    let mut ticker = Ticker::every(Duration::from_millis(100));
    let mut tick: u32 = 0;
    let mut display_on = true;
    let mut display_wake = Instant::now();
    let mut mag_pending = false;

    loop {
        ticker.next().await;
        tick = tick.wrapping_add(1);

        // Barometer at 4 Hz (every 250 ms ≈ every second or third tick).
        if (tick.is_multiple_of(5) || tick % 5 == 2)
            && let Some((addr, c)) = calib.as_ref()
        {
            let mut d = [0u8; 8];
            if bus.write_read(*addr, &[bme280::DATA_REG], &mut d).is_ok()
                && let Some(r) = c.compensate(&d)
            {
                let p = r.pressure_pa();
                // GNSS anchors the barometer when a new fix has come in.
                let st = shared::status();
                if st.pvt_count != last_gnss_count {
                    last_gnss_count = st.pvt_count;
                    if st.fix_ok && st.fix_type >= 3 {
                        elevation.on_gnss(st.hmsl_mm as f32 / 1000.0, st.v_acc_mm as f32 / 1000.0);
                    }
                }
                elevation.on_pressure(p);
                let e = elevation.state();
                emit(
                    Stream::Baro,
                    format_args!(
                        "{:.2},{:.2},{:.2},{:.1},{},{:.2},{:.1},{:.1}",
                        p,
                        r.temp_c(),
                        r.humidity_pct(),
                        e.qnh_pa,
                        e.anchored as u8,
                        e.elevation_m,
                        e.gain_m,
                        e.loss_m
                    ),
                );
                with_status(|s| {
                    s.pressure_pa = p;
                    s.temp_c = r.temp_c();
                    s.humidity_pct = r.humidity_pct();
                    s.elevation_m = e.elevation_m;
                    s.gain_m = e.gain_m;
                    s.loss_m = e.loss_m;
                    s.qnh_anchored = e.anchored;
                });
            }
        }

        // Magnetometer at 10 Hz: trigger, read on the next tick.
        if let Some(m) = mag.filter(|_| mag_ok) {
            if mag_pending && let Some(v) = mag_read(&mut bus, m) {
                let heading = libm::atan2f(v[1] as f32, v[0] as f32).to_degrees();
                let heading = if heading < 0.0 {
                    heading + 360.0
                } else {
                    heading
                };
                emit(
                    Stream::Mag,
                    format_args!("{},{},{},8,{:.1}", v[0], v[1], v[2], heading),
                );
                with_status(|s| s.heading_deg = heading);
            }
            mag_pending = mag_trigger(&mut bus, m);
        }

        // Display at 1 Hz.
        if tick.is_multiple_of(10) {
            let want = shared::status().display_on;
            if want && !display_on {
                display_wake = Instant::now();
            }
            if let Some(timeout) = DISPLAY_TIMEOUT
                && want
                && display_wake.elapsed() > timeout
            {
                with_status(|s| s.display_on = false);
            }
            let want = shared::status().display_on;
            if let Some(d) = display.as_mut() {
                if want != display_on {
                    let _ = d.set_on(&mut bus, want);
                }
                if want {
                    draw(d);
                    let _ = d.flush(&mut bus);
                }
            }
            display_on = want;
        }
    }
}
