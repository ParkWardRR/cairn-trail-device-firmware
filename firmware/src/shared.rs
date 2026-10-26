//! State shared between tasks: the record channel every producer writes into
//! and the storage task drains, and a snapshot of the latest readings for the
//! display and the console.

use core::cell::RefCell;
use core::fmt::Write;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Instant;
use heapless::String;

/// The files a session writes. The discriminant indexes the storage task's
/// buffers; the name is the 8.3 file name on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Stream {
    Gnss = 0,
    Sat = 1,
    Imu = 2,
    Baro = 3,
    Mag = 4,
    Power = 5,
    Event = 6,
    Pps = 7,
}

pub const STREAMS: usize = 8;

impl Stream {
    pub const ALL: [Stream; STREAMS] = [
        Stream::Gnss,
        Stream::Sat,
        Stream::Imu,
        Stream::Baro,
        Stream::Mag,
        Stream::Power,
        Stream::Event,
        Stream::Pps,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            Stream::Gnss => "GNSS.CSV",
            Stream::Sat => "SAT.CSV",
            Stream::Imu => "IMU.CSV",
            Stream::Baro => "BARO.CSV",
            Stream::Mag => "MAG.CSV",
            Stream::Power => "POWER.CSV",
            Stream::Event => "EVENTS.CSV",
            Stream::Pps => "PPS.CSV",
        }
    }

    /// The CSV header. Every row starts with `t_ms`, milliseconds since boot,
    /// which orders rows across files; UTC is a column, never the index.
    pub fn header(self) -> &'static str {
        match self {
            Stream::Gnss => {
                "t_ms,itow_ms,utc,valid,t_acc_ns,fix_type,fix_ok,psm,num_sv,lat,lon,height_m,hmsl_m,\
                 h_acc_m,v_acc_m,vel_n_ms,vel_e_ms,vel_d_ms,speed_ms,heading_deg,s_acc_ms,head_acc_deg,pdop,invalid_llh"
            }
            Stream::Sat => {
                "t_ms,itow_ms,gnss_id,sv_id,cno_dbhz,elev_deg,azim_deg,pr_res_m,quality,used,health"
            }
            Stream::Imu => "t_ms,ax_mg,ay_mg,az_mg,gx_dps,gy_dps,gz_dps,temp_c",
            Stream::Baro => {
                "t_ms,pressure_pa,temp_c,humidity_pct,qnh_pa,anchored,elevation_m,gain_m,loss_m"
            }
            Stream::Mag => "t_ms,mx,my,mz,range_g,heading_deg_uncal",
            Stream::Power => {
                "t_ms,vbat_mv,vbus_mv,vsys_mv,battery,charging,vbus_good,soc_gauge_pct,soc_curve_pct,die_temp_c,dcdc,ldo"
            }
            Stream::Event => "t_ms,event,detail",
            Stream::Pps => "t_ms,t_us",
        }
    }
}

/// One CSV row (without the trailing newline) bound for one file.
pub struct Record {
    pub stream: Stream,
    pub line: String<240>,
}

/// Every producer writes here; the storage task drains it. Producers never
/// block: if the channel is full the record is dropped and counted.
pub static RECORDS: Channel<CriticalSectionRawMutex, Record, 192> = Channel::new();

pub fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Send a row. The `t_ms` column is prepended here.
pub fn emit(stream: Stream, args: core::fmt::Arguments) {
    let mut line: String<240> = String::new();
    let _ = write!(line, "{},", now_ms());
    if line.write_fmt(args).is_err() {
        // Truncated: keep what fits and say so in the row itself.
        let _ = line.push('~');
    }
    if RECORDS.try_send(Record { stream, line }).is_err() {
        with_status(|s| s.dropped_records += 1);
    }
}

/// An event row: a name and free text. Also logged.
pub fn event(name: &str, args: core::fmt::Arguments) {
    log::info!("[{}] {}", name, args);
    emit(Stream::Event, format_args!("{},\"{}\"", name, args));
}

/// The latest of everything, for the display and the `stat` command.
#[derive(Clone, Copy, Default)]
pub struct Status {
    pub session: u16,
    pub sd_ok: bool,
    pub sd_bytes: u64,
    pub recording: bool,
    pub dropped_records: u32,
    pub write_errors: u32,

    pub gnss_seen: bool,
    pub gnss_model: [u8; 10],
    pub fix_type: u8,
    pub fix_ok: bool,
    pub num_sv: u8,
    pub sats_visible: u8,
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub hmsl_mm: i32,
    pub h_acc_mm: u32,
    pub v_acc_mm: u32,
    pub speed_mms: i32,
    pub utc: Option<trail_core::pcf8563::DateTime>,
    pub pvt_count: u32,
    pub ubx_checksum_errors: u32,

    pub pressure_pa: f32,
    pub temp_c: f32,
    pub humidity_pct: f32,
    pub elevation_m: f32,
    pub gain_m: f32,
    pub loss_m: f32,
    pub qnh_anchored: bool,

    pub imu_ok: bool,
    pub mag_ok: bool,
    pub baro_ok: bool,
    pub heading_deg: f32,
    pub accel_mg: [i16; 3],

    pub vbat_mv: u16,
    pub vbus_mv: u16,
    pub battery: bool,
    pub charging: bool,
    pub soc_pct: u8,

    pub marks: u16,
    pub display_on: bool,

    pub wifi_up: bool,
    pub checkins: u32,
    pub checkin_errors: u32,

    pub assist_queued: u32,
    pub assist_sent: u32,
    pub assist_accepted: u32,
    pub assist_rejected: u32,
}

pub static STATUS: Mutex<CriticalSectionRawMutex, RefCell<Status>> =
    Mutex::new(RefCell::new(Status {
        session: 0,
        sd_ok: false,
        sd_bytes: 0,
        recording: false,
        dropped_records: 0,
        write_errors: 0,
        gnss_seen: false,
        gnss_model: [0; 10],
        fix_type: 0,
        fix_ok: false,
        num_sv: 0,
        sats_visible: 0,
        lat_e7: 0,
        lon_e7: 0,
        hmsl_mm: 0,
        h_acc_mm: 0,
        v_acc_mm: 0,
        speed_mms: 0,
        utc: None,
        pvt_count: 0,
        ubx_checksum_errors: 0,
        pressure_pa: 0.0,
        temp_c: 0.0,
        humidity_pct: 0.0,
        elevation_m: 0.0,
        gain_m: 0.0,
        loss_m: 0.0,
        qnh_anchored: false,
        imu_ok: false,
        mag_ok: false,
        baro_ok: false,
        heading_deg: 0.0,
        accel_mg: [0; 3],
        vbat_mv: 0,
        vbus_mv: 0,
        battery: false,
        charging: false,
        soc_pct: 0,
        marks: 0,
        display_on: true,
        wifi_up: false,
        checkins: 0,
        checkin_errors: 0,
        assist_queued: 0,
        assist_sent: 0,
        assist_accepted: 0,
        assist_rejected: 0,
    }));

pub fn with_status<R>(f: impl FnOnce(&mut Status) -> R) -> R {
    STATUS.lock(|s| f(&mut s.borrow_mut()))
}

pub fn status() -> Status {
    STATUS.lock(|s| *s.borrow())
}

/// Commands from the console to the storage task.
pub enum StorageCmd {
    List,
    Cat(String<40>),
    Flush,
    Stop,
    Start,
}

pub static STORAGE_CMDS: Channel<CriticalSectionRawMutex, StorageCmd, 4> = Channel::new();

/// The receiver's identity frames, whole (sync bytes to checksum), as u-blox
/// AssistNow registration wants them: UBX-MON-VER and UBX-SEC-UNIQID.
pub struct GnssIdentity {
    pub mon_ver: heapless::Vec<u8, 320>,
    pub uniqid: heapless::Vec<u8, 48>,
}

pub static GNSS_IDENTITY: Mutex<CriticalSectionRawMutex, RefCell<GnssIdentity>> =
    Mutex::new(RefCell::new(GnssIdentity {
        mon_ver: heapless::Vec::new(),
        uniqid: heapless::Vec::new(),
    }));

/// AssistNow data (UBX-MGA frames) read from ASSIST.UBX on the card, on its
/// way to the receiver. An empty frame marks the end of the file.
pub type AssistFrame = heapless::Vec<u8, 512>;
pub static ASSIST: Channel<CriticalSectionRawMutex, AssistFrame, 8> = Channel::new();

/// The firmware's identity, as printed at boot and written to INFO.TXT.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Unix time of the commit this was built from.
pub const BUILD_EPOCH: u64 = const_parse(env!("CAIRN_BUILD_EPOCH"));
const fn const_parse(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut v = 0u64;
    while i < b.len() {
        v = v * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    v
}
pub const COMMIT: &str = env!("CAIRN_GIT_COMMIT");
pub const DIRTY: &str = env!("CAIRN_GIT_DIRTY");
#[cfg(feature = "verbose")]
pub const FLAVOUR: &str = "verbose";
#[cfg(not(feature = "verbose"))]
pub const FLAVOUR: &str = "release";
