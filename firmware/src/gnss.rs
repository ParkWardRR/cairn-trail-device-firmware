//! The u-blox MAX-M10S on UART1 (ESP TX 8, RX 9), powered by ALDO4.
//!
//! This task runs on an interrupt-priority executor, so a long SD card write
//! on the thread executor cannot starve the UART's 128-byte FIFO.

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::Async;
use esp_hal::uart::{Config, RxConfig, Uart};
use heapless::String;
use trail_core::pcf8563::DateTime;
use trail_core::ubx::{self, NavPvt, Parser, key};

use crate::shared::{Stream, emit, event, with_status};

pub type GnssUart = Uart<'static, Async>;

const BAUDS: [u32; 4] = [9600, 38400, 115_200, 230_400];
const RUN_BAUD: u32 = 115_200;

async fn send(uart: &mut GnssUart, frame: &[u8]) {
    let _ = uart.write_async(frame).await;
    let _ = uart.flush_async().await;
}

fn set_baud(uart: &mut GnssUart, baud: u32) {
    let cfg = Config::default().with_baudrate(baud).with_rx(
        RxConfig::default()
            .with_fifo_full_threshold(64)
            .with_timeout(4),
    );
    let _ = uart.apply_config(&cfg);
}

/// Wait up to `timeout` for a frame of the given class/id; returns its payload
/// length (the payload stays in `parser`).
async fn wait_for(
    uart: &mut GnssUart,
    parser: &mut Parser,
    class: u8,
    id: u8,
    timeout: Duration,
) -> Option<(usize, [u8; 64])> {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 128];
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match select(uart.read_async(&mut buf), Timer::after(remaining)).await {
            Either::First(Ok(n)) => {
                for &b in &buf[..n] {
                    if let Some(f) = parser.push(b)
                        && f.class == class
                        && f.id == id
                    {
                        let mut keep = [0u8; 64];
                        let p = parser.payload(&f);
                        let k = p.len().min(64);
                        keep[..k].copy_from_slice(&p[..k]);
                        return Some((p.len(), keep));
                    }
                }
            }
            Either::First(Err(_)) => {}
            Either::Second(()) => break,
        }
    }
    None
}

/// Find the receiver's baud rate by polling MON-VER at each candidate.
async fn probe(uart: &mut GnssUart, parser: &mut Parser) -> Option<u32> {
    let mut poll = [0u8; 8];
    let n = ubx::poll(ubx::CLASS_MON, ubx::MON_VER, &mut poll).unwrap();
    for &baud in &BAUDS {
        set_baud(uart, baud);
        Timer::after(Duration::from_millis(50)).await;
        for _ in 0..2 {
            send(uart, &poll[..n]).await;
            if wait_for(
                uart,
                parser,
                ubx::CLASS_MON,
                ubx::MON_VER,
                Duration::from_millis(1200),
            )
            .await
            .is_some()
            {
                return Some(baud);
            }
        }
    }
    None
}

/// Configure the receiver from scratch: clear anything another firmware left
/// in battery-backed RAM, then write Cairn's configuration to the RAM layer.
/// Returns true if every step was acknowledged.
async fn configure(uart: &mut GnssUart, parser: &mut Parser) -> bool {
    let mut out = [0u8; 300];

    // 1. Reset configuration to defaults (navigation data is kept).
    let n = ubx::cfg_reset_to_defaults(&mut out).unwrap();
    send(uart, &out[..n]).await;
    let reset_ack = wait_for(
        uart,
        parser,
        ubx::CLASS_ACK,
        ubx::ACK_ACK,
        Duration::from_millis(1500),
    )
    .await;
    // The ACK is often lost: loading defaults changes the baud rate at once.
    event(
        "gnss_cfg_reset",
        format_args!(
            "ack={} (may be lost to the baud change)",
            reset_ack.is_some()
        ),
    );
    Timer::after(Duration::from_millis(300)).await;

    // Defaults may have changed the baud rate back to the module's own.
    let Some(baud) = probe(uart, parser).await else {
        event("gnss", format_args!("lost after config reset"));
        return false;
    };

    // 2. Cairn's configuration, RAM layer only.
    let items = [
        (key::UART1INPROT_UBX, 1),
        (key::UART1OUTPROT_UBX, 1),
        (key::UART1OUTPROT_NMEA, 0),
        (key::MSGOUT_NAV_PVT_UART1, 1),
        (key::MSGOUT_NAV_SAT_UART1, 5),
        (key::NAVSPG_DYNMODEL, ubx::dynmodel::PEDESTRIAN),
        (key::RATE_MEAS, 1000),
        (key::RATE_NAV, 1),
        (key::PM_OPERATEMODE, 0), // full power: measure before saving power
        (key::NAVSPG_ACKAIDING, 1), // acknowledge AssistNow messages
    ];
    let n = ubx::valset(ubx::LAYER_RAM, &items, &mut out).unwrap();
    send(uart, &out[..n]).await;
    let cfg_ack = wait_for(
        uart,
        parser,
        ubx::CLASS_ACK,
        ubx::ACK_ACK,
        Duration::from_millis(1500),
    )
    .await;
    event(
        "gnss_cfg",
        format_args!("baud={} ack={}", baud, cfg_ack.is_some()),
    );

    // 3. Faster UART last; the ACK for this one may be lost in the switch.
    if baud != RUN_BAUD {
        let n = ubx::valset(
            ubx::LAYER_RAM,
            &[(key::UART1_BAUDRATE, RUN_BAUD as u64)],
            &mut out,
        )
        .unwrap();
        send(uart, &out[..n]).await;
        Timer::after(Duration::from_millis(150)).await;
        set_baud(uart, RUN_BAUD);
    }
    cfg_ack.is_some()
}

/// Keep a whole identity frame (rebuilt from its payload) for the console's
/// `gnssid` command and AssistNow registration.
fn store_identity(mon_ver: bool, class: u8, id: u8, payload: &[u8]) {
    let mut out = [0u8; 320];
    if let Some(n) = ubx::frame(class, id, payload, &mut out) {
        crate::shared::GNSS_IDENTITY.lock(|g| {
            let mut g = g.borrow_mut();
            if mon_ver {
                g.mon_ver.clear();
                let _ = g.mon_ver.extend_from_slice(&out[..n]);
            } else {
                g.uniqid.clear();
                let _ = g.uniqid.extend_from_slice(&out[..n.min(48)]);
            }
        });
    }
}

/// Record a NAV-PVT; returns its validity flags.
fn handle_pvt(p: &[u8]) -> Option<u8> {
    let pvt = NavPvt::parse(p)?;
    let mut utc: String<32> = String::new();
    let _ = write!(
        utc,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        pvt.year,
        pvt.month,
        pvt.day,
        pvt.hour,
        pvt.min,
        pvt.sec,
        (pvt.nano.max(0) / 1_000_000)
    );
    let has_pos = pvt.has_position();
    emit(
        Stream::Gnss,
        format_args!(
            "{},{},{},{},{},{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.2},{:.3},{:.2},{:.2},{}",
            pvt.itow_ms,
            utc,
            pvt.valid,
            pvt.t_acc_ns,
            pvt.fix_type,
            pvt.gnss_fix_ok() as u8,
            pvt.psm_state(),
            pvt.num_sv,
            // Coordinates as exact decimal degrees from the integer 1e-7 value.
            Deg(pvt.lat_e7),
            Deg(pvt.lon_e7),
            pvt.height_mm as f32 / 1000.0,
            pvt.hmsl_mm as f32 / 1000.0,
            pvt.h_acc_mm as f32 / 1000.0,
            pvt.v_acc_mm as f32 / 1000.0,
            pvt.vel_n_mms as f32 / 1000.0,
            pvt.vel_e_mms as f32 / 1000.0,
            pvt.vel_d_mms as f32 / 1000.0,
            pvt.g_speed_mms as f32 / 1000.0,
            pvt.head_mot_e5 as f32 / 1e5,
            pvt.s_acc_mms as f32 / 1000.0,
            pvt.head_acc_e5 as f32 / 1e5,
            pvt.pdop_e2 as f32 / 100.0,
            pvt.invalid_llh() as u8,
        ),
    );
    let was_fix = crate::shared::status().fix_ok;
    with_status(|s| {
        s.gnss_seen = true;
        s.pvt_count = s.pvt_count.wrapping_add(1);
        s.fix_type = pvt.fix_type;
        s.fix_ok = has_pos;
        s.num_sv = pvt.num_sv;
        if has_pos {
            s.lat_e7 = pvt.lat_e7;
            s.lon_e7 = pvt.lon_e7;
            s.hmsl_mm = pvt.hmsl_mm;
            s.height_mm = pvt.height_mm;
            s.h_acc_mm = pvt.h_acc_mm;
            s.v_acc_mm = pvt.v_acc_mm;
            s.speed_mms = pvt.g_speed_mms;
        }
        if pvt.time_resolved() {
            s.utc = Some(DateTime {
                year: pvt.year,
                month: pvt.month,
                day: pvt.day,
                hour: pvt.hour,
                min: pvt.min,
                sec: pvt.sec,
            });
        }
    });
    if has_pos != was_fix {
        event(
            if has_pos { "fix_acquired" } else { "fix_lost" },
            // The uptime at the first fix is the time to first fix the
            // priming and AssistNow are meant to shorten.
            format_args!(
                "fix_type={} sv={} h_acc_m={:.1} uptime_s={:.1}",
                pvt.fix_type,
                pvt.num_sv,
                pvt.h_acc_mm as f32 / 1000.0,
                crate::shared::now_ms() as f32 / 1000.0
            ),
        );
    }
    Some(pvt.valid)
}

/// How long after boot priming may stay undecided (no card, no RTC answer)
/// before AssistNow data is let through without it.
const PRIME_WAIT: Duration = Duration::from_secs(20);

/// Decide priming once per boot, on the first NAV-PVT: a receiver that woke
/// with a valid date and time kept its state on the backup supply (the 18650
/// feeds V_BCKP) and knows better than the RTC and PRIME.TXT, so nothing is
/// sent; one that woke without gets the planned time and position. Returns
/// whether priming is decided.
async fn prime_step(uart: &mut GnssUart, pvt_valid: Option<u8>) -> bool {
    use crate::shared::PrimeState;
    let state = crate::shared::status().prime;
    match state {
        PrimeState::Sent | PrimeState::Skipped | PrimeState::Nothing => return true,
        PrimeState::Pending => {
            if Instant::now().as_millis() > PRIME_WAIT.as_millis() {
                with_status(|s| s.prime = PrimeState::Nothing);
                event(
                    "prime",
                    format_args!("not planned within {} s; skipped", PRIME_WAIT.as_secs()),
                );
                return true;
            }
            return false;
        }
        PrimeState::Planned => {}
    }
    let Some(valid) = pvt_valid else {
        return false;
    };
    let frames = crate::shared::PRIME_FRAMES.lock(|f| core::mem::take(&mut *f.borrow_mut()));
    if valid & 0x03 == 0x03 {
        with_status(|s| s.prime = PrimeState::Skipped);
        event(
            "prime",
            format_args!(
                "receiver woke with date and time (backup supply): {} frames not sent",
                frames.len()
            ),
        );
    } else {
        // The time is read off the RTC now, at sending: the message means
        // "it is this time on receipt".
        let acc_ms = crate::shared::status().prime_time_acc_ms;
        let mut sent = 0;
        if acc_ms > 0
            && let Some(now) = crate::shared::rtc_now()
        {
            let mut raw = [0u8; 40];
            let t = trail_core::pcf8563::from_unix(now);
            if let Some(n) = ubx::mga_ini_time_utc(&t, 0, acc_ms, &mut raw) {
                send(uart, &raw[..n]).await;
                sent += 1;
                Timer::after(Duration::from_millis(8)).await;
            }
        }
        for f in &frames {
            send(uart, f).await;
            sent += 1;
            Timer::after(Duration::from_millis(8)).await;
        }
        with_status(|s| {
            s.assist_sent += sent;
            s.prime = PrimeState::Sent
        });
        let s = crate::shared::status();
        event(
            "prime",
            format_args!(
                "receiver woke without time: sent {} frames (time_acc_ms={} pos_acc_m={})",
                sent, s.prime_time_acc_ms, s.prime_pos_acc_m
            ),
        );
    }
    true
}

/// Decimal degrees from a 1e-7 integer, without float rounding.
struct Deg(i32);
impl core::fmt::Display for Deg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let v = self.0 as i64;
        let sign = if v < 0 { "-" } else { "" };
        let a = v.unsigned_abs();
        write!(f, "{}{}.{:07}", sign, a / 10_000_000, a % 10_000_000)
    }
}

fn handle_sat(p: &[u8]) {
    let Some((itow, n)) = ubx::nav_sat_header(p) else {
        return;
    };
    let mut used = 0u8;
    for i in 0..n as usize {
        if let Some(s) = ubx::nav_sat(p, i) {
            if s.used() {
                used += 1;
            }
            emit(
                Stream::Sat,
                format_args!(
                    "{},{},{},{},{},{},{:.1},{},{},{}",
                    itow,
                    s.gnss_id,
                    s.sv_id,
                    s.cno_dbhz,
                    s.elev_deg,
                    s.azim_deg,
                    s.pr_res_dm as f32 / 10.0,
                    s.quality(),
                    s.used() as u8,
                    s.health()
                ),
            );
        }
    }
    with_status(|s| s.sats_visible = n);
    log::debug!("NAV-SAT: {} visible, {} used", n, used);
}

#[embassy_executor::task]
pub async fn gnss_task(
    uart1: esp_hal::peripherals::UART1<'static>,
    tx: esp_hal::peripherals::GPIO8<'static>,
    rx: esp_hal::peripherals::GPIO9<'static>,
) {
    // Built here, on the interrupt executor, because the driver is not Send.
    let mut uart: GnssUart = Uart::new(uart1, Config::default().with_baudrate(9600))
        .unwrap()
        .with_tx(tx)
        .with_rx(rx)
        .into_async();
    let mut parser = Parser::new();
    // The receiver needs a moment after ALDO4 comes up.
    Timer::after(Duration::from_millis(500)).await;

    loop {
        let Some(baud) = probe(&mut uart, &mut parser).await else {
            event(
                "gnss",
                format_args!("no answer at any baud rate; retrying in 10 s"),
            );
            Timer::after(Duration::from_secs(10)).await;
            continue;
        };
        // MON-VER: software and hardware version; 000A0000 is an M10.
        let mut poll = [0u8; 8];
        let n = ubx::poll(ubx::CLASS_MON, ubx::MON_VER, &mut poll).unwrap();
        send(&mut uart, &poll[..n]).await;
        let mut buf = [0u8; 128];
        let deadline = Instant::now() + Duration::from_millis(1500);
        'ver: while Instant::now() < deadline {
            if let Either::First(Ok(k)) = select(
                uart.read_async(&mut buf),
                Timer::after(Duration::from_millis(200)),
            )
            .await
            {
                for &b in &buf[..k] {
                    if let Some(f) = parser.push(b)
                        && f.class == ubx::CLASS_MON
                        && f.id == ubx::MON_VER
                    {
                        let p = parser.payload(&f);
                        store_identity(true, ubx::CLASS_MON, ubx::MON_VER, p);
                        if let Some((sw, hw)) = ubx::mon_ver(p) {
                            let mut ext: String<160> = String::new();
                            for i in 0..6 {
                                if let Some(e) = ubx::mon_ver_ext(p, i) {
                                    let _ = write!(ext, "{};", e);
                                }
                            }
                            event(
                                "gnss_version",
                                format_args!("baud={} sw={} hw={} {}", baud, sw, hw, ext),
                            );
                            with_status(|s| {
                                let n = hw.len().min(10);
                                s.gnss_model[..n].copy_from_slice(&hw.as_bytes()[..n]);
                            });
                        }
                        break 'ver;
                    }
                }
            }
        }

        // SEC-UNIQID: the chip's unique ID, which u-blox AssistNow registration
        // binds its credential to.
        let n = ubx::poll(ubx::CLASS_SEC, ubx::SEC_UNIQID, &mut poll).unwrap();
        send(&mut uart, &poll[..n]).await;
        match wait_for(
            &mut uart,
            &mut parser,
            ubx::CLASS_SEC,
            ubx::SEC_UNIQID,
            Duration::from_millis(1500),
        )
        .await
        {
            Some((len, keep)) => {
                let p = &keep[..len.min(64)];
                store_identity(false, ubx::CLASS_SEC, ubx::SEC_UNIQID, p);
                event("gnss_uniqid", format_args!("{:02x?}", p));
            }
            None => event("gnss_uniqid", format_args!("no answer")),
        }

        configure(&mut uart, &mut parser).await;

        // Run: decode everything; reconfigure if PVT stops for 10 s.
        let mut last_pvt = Instant::now();
        let mut pvt_valid: Option<u8> = None;
        loop {
            // AssistNow data from the card, ONE frame per pass, then back to
            // reading: at 9600 baud a frame takes ~80 ms to send, and sixteen
            // in a row (1.3 s without reading) overflowed the 128-byte receive
            // FIFO, losing the MGA-ACKs and corrupting NAV frames. Held until
            // priming is decided: time and position go first.
            while let Ok(frame) = crate::shared::GNSS_CMD.try_receive() {
                send(&mut uart, &frame).await;
            }
            let primed = prime_step(&mut uart, pvt_valid).await;
            if primed && let Ok(frame) = crate::shared::ASSIST.try_receive() {
                if frame.is_empty() {
                    let s = crate::shared::status();
                    event(
                        "assist_sent",
                        format_args!(
                            "{} frames sent; accepted so far {}, not used {}",
                            s.assist_sent, s.assist_accepted, s.assist_rejected
                        ),
                    );
                } else {
                    send(&mut uart, &frame).await;
                    with_status(|s| s.assist_sent += 1);
                }
            }
            match select(
                uart.read_async(&mut buf),
                Timer::after(Duration::from_secs(1)),
            )
            .await
            {
                Either::First(Ok(k)) => {
                    for &b in &buf[..k] {
                        if let Some(f) = parser.push(b) {
                            let p = parser.payload(&f);
                            match (f.class, f.id) {
                                (ubx::CLASS_NAV, ubx::NAV_PVT) => {
                                    last_pvt = Instant::now();
                                    let v = handle_pvt(p);
                                    pvt_valid = pvt_valid.or(v);
                                }
                                (ubx::CLASS_NAV, ubx::NAV_SAT) => handle_sat(p),
                                (ubx::CLASS_MGA, ubx::MGA_ACK) => {
                                    if let Some((ok, _)) = ubx::mga_ack(p) {
                                        with_status(|s| {
                                            if ok {
                                                s.assist_accepted += 1
                                            } else {
                                                s.assist_rejected += 1
                                            }
                                        });
                                    }
                                }
                                (ubx::CLASS_ACK, ubx::ACK_NAK) => {
                                    event("gnss_nak", format_args!("{:02x?}", ubx::ack_target(p)))
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Either::First(Err(e)) => {
                    log::warn!("gnss uart: {:?}", e);
                }
                Either::Second(()) => {}
            }
            with_status(|s| s.ubx_checksum_errors = parser.stats.checksum_errors);
            if last_pvt.elapsed() > Duration::from_secs(10) {
                event(
                    "gnss",
                    format_args!(
                        "no NAV-PVT for 10 s (frames={} checksum_errors={}); reconfiguring",
                        parser.stats.frames, parser.stats.checksum_errors
                    ),
                );
                break;
            }
        }
    }
}
