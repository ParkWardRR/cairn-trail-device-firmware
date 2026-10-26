//! `format/v3` record payloads the logger writes (spec §4), encoded here and
//! sealed by `cairn-format`. Each has a decoder too, so the tests can take the
//! payloads out of the committed conformance vectors, decode and re-encode
//! them, and require the same bytes: the layouts are checked against the
//! vectors, not against this file's reading of the spec.
//!
//! All integers little-endian. "Unknown" sentinels are the spec's: quality
//! fields are never synthesised.

/// §4.1 `GNSS_SAMPLE`, 32 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GnssSample {
    pub lat_e7: i32,
    pub lon_e7: i32,
    /// Above the WGS84 ellipsoid.
    pub alt_cm: i32,
    pub speed_cmps: u16,
    pub heading_cdeg: u16,
    pub hdop_e2: u16,
    pub h_acc_cm: u16,
    pub v_acc_cm: u16,
    /// 0 none, 1 2D, 2 3D, 3 DGPS, 4 RTK-float, 5 RTK-fixed.
    pub fix_type: u8,
    pub sats_used: u8,
    pub sats_visible: u8,
    /// bit 0 GPS, 1 GLONASS, 2 Galileo, 3 BeiDou, 4 dead-reckoned, 5 external.
    pub source_flags: u8,
    pub utc_offset_ms: i32,
    pub utc_acc_ms: u16,
}

pub const UNKNOWN_U16: u16 = 0xFFFF;

impl GnssSample {
    pub const LEN: usize = 32;

    pub fn encode(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0..4].copy_from_slice(&self.lat_e7.to_le_bytes());
        b[4..8].copy_from_slice(&self.lon_e7.to_le_bytes());
        b[8..12].copy_from_slice(&self.alt_cm.to_le_bytes());
        b[12..14].copy_from_slice(&self.speed_cmps.to_le_bytes());
        b[14..16].copy_from_slice(&self.heading_cdeg.to_le_bytes());
        b[16..18].copy_from_slice(&self.hdop_e2.to_le_bytes());
        b[18..20].copy_from_slice(&self.h_acc_cm.to_le_bytes());
        b[20..22].copy_from_slice(&self.v_acc_cm.to_le_bytes());
        b[22] = self.fix_type;
        b[23] = self.sats_used;
        b[24] = self.sats_visible;
        b[25] = self.source_flags;
        b[26..30].copy_from_slice(&self.utc_offset_ms.to_le_bytes());
        b[30..32].copy_from_slice(&self.utc_acc_ms.to_le_bytes());
        b
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() != Self::LEN {
            return None;
        }
        let i32_at = |o: usize| i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
        let u16_at = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
        Some(Self {
            lat_e7: i32_at(0),
            lon_e7: i32_at(4),
            alt_cm: i32_at(8),
            speed_cmps: u16_at(12),
            heading_cdeg: u16_at(14),
            hdop_e2: u16_at(16),
            h_acc_cm: u16_at(18),
            v_acc_cm: u16_at(20),
            fix_type: p[22],
            sats_used: p[23],
            sats_visible: p[24],
            source_flags: p[25],
            utc_offset_ms: i32_at(26),
            utc_acc_ms: u16_at(30),
        })
    }
}

/// Saturate a millimetre quantity into a u16 centimetre field, with the
/// unknown sentinel reserved: a real 655.35 m accuracy is written as 655.34.
pub fn cm_u16(mm: u32) -> u16 {
    (mm / 10).min(UNKNOWN_U16 as u32 - 1) as u16
}

/// §4.2 `IMU_SUMMARY`, 20 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImuSummary {
    pub window_ms: u16,
    pub accel_rms_mg: u16,
    pub accel_peak_x_mg: i16,
    pub accel_peak_y_mg: i16,
    pub accel_peak_z_mg: i16,
    pub gyro_peak_dps_e1: i16,
    pub variance: u16,
    pub sample_count: u16,
    pub event_flags: u8,
}

impl ImuSummary {
    pub const LEN: usize = 20;

    pub fn encode(&self) -> [u8; 20] {
        let mut b = [0u8; 20];
        b[0..2].copy_from_slice(&self.window_ms.to_le_bytes());
        b[2..4].copy_from_slice(&self.accel_rms_mg.to_le_bytes());
        b[4..6].copy_from_slice(&self.accel_peak_x_mg.to_le_bytes());
        b[6..8].copy_from_slice(&self.accel_peak_y_mg.to_le_bytes());
        b[8..10].copy_from_slice(&self.accel_peak_z_mg.to_le_bytes());
        b[10..12].copy_from_slice(&self.gyro_peak_dps_e1.to_le_bytes());
        b[12..14].copy_from_slice(&self.variance.to_le_bytes());
        b[14..16].copy_from_slice(&self.sample_count.to_le_bytes());
        b[16] = self.event_flags;
        b
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() != Self::LEN || p[17..20] != [0, 0, 0] {
            return None;
        }
        let u = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
        let i = |o: usize| i16::from_le_bytes([p[o], p[o + 1]]);
        Some(Self {
            window_ms: u(0),
            accel_rms_mg: u(2),
            accel_peak_x_mg: i(4),
            accel_peak_y_mg: i(6),
            accel_peak_z_mg: i(8),
            gyro_peak_dps_e1: i(10),
            variance: u(12),
            sample_count: u(14),
            event_flags: p[16],
        })
    }
}

/// Builds an `IMU_SUMMARY` from samples (accelerometer in mg, gyroscope in
/// 0.1 °/s). Peaks are the sample with the largest magnitude per axis, sign
/// kept; `variance` is the variance of |a| in mg² / 16, saturated.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImuWindow {
    n: u32,
    sum_sq: u64,
    sum_mag: u64,
    sum_mag_sq: u64,
    peak: [i16; 3],
    gyro_peak: i16,
    first_ms: u64,
    last_ms: u64,
}

impl ImuWindow {
    pub fn add(&mut self, t_ms: u64, accel_mg: [i16; 3], gyro_dps_e1: [i16; 3]) {
        if self.n == 0 {
            self.first_ms = t_ms;
        }
        self.last_ms = t_ms;
        self.n += 1;
        let sq: u64 = accel_mg.iter().map(|&a| (a as i64 * a as i64) as u64).sum();
        self.sum_sq += sq;
        let mag = libm::sqrt(sq as f64) as u64;
        self.sum_mag += mag;
        self.sum_mag_sq += mag * mag;
        for (p, &a) in self.peak.iter_mut().zip(accel_mg.iter()) {
            if a.unsigned_abs() > p.unsigned_abs() {
                *p = a;
            }
        }
        for &g in &gyro_dps_e1 {
            if g.unsigned_abs() > self.gyro_peak.unsigned_abs() {
                self.gyro_peak = g;
            }
        }
    }

    pub fn len(&self) -> u32 {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The window's span in ms (first to last sample).
    pub fn span_ms(&self) -> u64 {
        self.last_ms.saturating_sub(self.first_ms)
    }

    /// The summary, and the window reset for the next.
    pub fn take(&mut self, window_ms: u16) -> Option<ImuSummary> {
        if self.n == 0 {
            return None;
        }
        let n = self.n as u64;
        let rms = libm::sqrt(self.sum_sq as f64 / n as f64);
        let mean = self.sum_mag as f64 / n as f64;
        let var = (self.sum_mag_sq as f64 / n as f64 - mean * mean).max(0.0) / 16.0;
        let s = ImuSummary {
            window_ms,
            accel_rms_mg: (rms as u32).min(u16::MAX as u32) as u16,
            accel_peak_x_mg: self.peak[0],
            accel_peak_y_mg: self.peak[1],
            accel_peak_z_mg: self.peak[2],
            gyro_peak_dps_e1: self.gyro_peak,
            variance: (var as u32).min(u16::MAX as u32) as u16,
            sample_count: self.n.min(u16::MAX as u32) as u16,
            event_flags: 0,
        };
        *self = Self::default();
        Some(s)
    }
}

/// §4.5 `DEVICE_HEALTH`, 16 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceHealth {
    /// The supply: on the logger, the cell (or USB when no cell is fitted).
    pub battery_mv: u16,
    pub sd_write_errors: u16,
    pub sd_free_mib: u16,
    pub device_temp_c: i8,
    /// `-128` when unknown.
    pub rssi_dbm: i8,
    pub ext_sensor_1: u16,
    pub ext_sensor_2: u16,
    pub health_state: u8,
    pub reboot_count: u8,
}

impl DeviceHealth {
    pub const LEN: usize = 16;

    pub fn encode(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..2].copy_from_slice(&self.battery_mv.to_le_bytes());
        b[2..4].copy_from_slice(&self.sd_write_errors.to_le_bytes());
        b[4..6].copy_from_slice(&self.sd_free_mib.to_le_bytes());
        b[6] = self.device_temp_c as u8;
        b[7] = self.rssi_dbm as u8;
        b[8..10].copy_from_slice(&self.ext_sensor_1.to_le_bytes());
        b[10..12].copy_from_slice(&self.ext_sensor_2.to_le_bytes());
        b[12] = self.health_state;
        b[13] = self.reboot_count;
        b
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() != Self::LEN || p[14..16] != [0, 0] {
            return None;
        }
        let u = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
        Some(Self {
            battery_mv: u(0),
            sd_write_errors: u(2),
            sd_free_mib: u(4),
            device_temp_c: p[6] as i8,
            rssi_dbm: p[7] as i8,
            ext_sensor_1: u(8),
            ext_sensor_2: u(10),
            health_state: p[12],
            reboot_count: p[13],
        })
    }
}

/// §4.8 `GNSS_GAP`, 12 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GnssGap {
    pub duration_ms: u32,
    pub expected_samples: u16,
    /// 1 no fix, 2 receiver reset, 3 powered down, 4 tunnel/obstruction, 5 time jump.
    pub cause: u8,
}

pub const GAP_NO_FIX: u8 = 1;

impl GnssGap {
    pub const LEN: usize = 12;

    pub fn encode(&self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..4].copy_from_slice(&self.duration_ms.to_le_bytes());
        b[4..6].copy_from_slice(&self.expected_samples.to_le_bytes());
        b[6] = self.cause;
        b
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() != Self::LEN || p[7..12] != [0; 5] {
            return None;
        }
        Some(Self {
            duration_ms: u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
            expected_samples: u16::from_le_bytes([p[4], p[5]]),
            cause: p[6],
        })
    }
}

/// §4.12 `TIME_OBSERVATION`, 16 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeObservation {
    pub utc_ms: u64,
    /// `None` = the source stated no accuracy (`0xFFFFFFFF`).
    pub acc_ms: Option<u32>,
    /// 1 GNSS, 2 phone, 3 modem network, 4 RTC.
    pub source: u8,
    /// Adopted as the manifest's `utc_basis_ms`.
    pub adopted: bool,
}

pub const TIME_SOURCE_GNSS: u8 = 1;
pub const TIME_SOURCE_RTC: u8 = 4;

impl TimeObservation {
    pub const LEN: usize = 16;

    pub fn encode(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..8].copy_from_slice(&self.utc_ms.to_le_bytes());
        b[8..12].copy_from_slice(&self.acc_ms.unwrap_or(u32::MAX).to_le_bytes());
        b[12] = self.source;
        b[13] = self.adopted as u8;
        b
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() != Self::LEN || p[14..16] != [0, 0] {
            return None;
        }
        let acc = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
        Some(Self {
            utc_ms: u64::from_le_bytes(p[0..8].try_into().ok()?),
            acc_ms: (acc != u32::MAX).then_some(acc),
            source: p[12],
            adopted: p[13] & 1 != 0,
        })
    }
}

/// §4.7 `STATE_TRANSITION`, 20 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StateTransition {
    /// 1 capture, 2 bundle, 3 connectivity, 4 health.
    pub region: u8,
    pub from_state: u8,
    pub to_state: u8,
    pub trigger_event: u8,
    pub reason_code: u8,
    pub policy_version: u8,
    pub start_score_e2: u16,
    pub stop_score_e2: u16,
    pub wake_cause: u32,
}

impl StateTransition {
    pub const LEN: usize = 20;

    pub fn encode(&self) -> [u8; 20] {
        let mut b = [0u8; 20];
        b[0] = self.region;
        b[1] = self.from_state;
        b[2] = self.to_state;
        b[3] = self.trigger_event;
        b[4] = self.reason_code;
        b[5] = self.policy_version;
        b[8..10].copy_from_slice(&self.start_score_e2.to_le_bytes());
        b[10..12].copy_from_slice(&self.stop_score_e2.to_le_bytes());
        b[12..16].copy_from_slice(&self.wake_cause.to_le_bytes());
        b
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() != Self::LEN || p[6..8] != [0, 0] || p[16..20] != [0; 4] {
            return None;
        }
        let u = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
        Some(Self {
            region: p[0],
            from_state: p[1],
            to_state: p[2],
            trigger_event: p[3],
            reason_code: p[4],
            policy_version: p[5],
            start_score_e2: u(8),
            stop_score_e2: u(10),
            wake_cause: u32::from_le_bytes([p[12], p[13], p[14], p[15]]),
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use cairn_format::{
        scan_segment, unhex_array, KeyProvider, RecordType, RootKeyProvider, ScanState,
    };
    use std::vec::Vec;

    /// The vectors' public test root (contracts/format/v3/vectors/README.md).
    const TEST_ROOT: &str = "7b1a2c5cc080bc43df4922975a48987d92e3209ba6a4bb8853a23651ad9b3821";

    /// Every (record type, plaintext) in a vector segment, decrypted.
    fn payloads(seg: &[u8]) -> Vec<(RecordType, Vec<u8>)> {
        let keys = RootKeyProvider::new(unhex_array(TEST_ROOT).unwrap(), 1);
        let r = scan_segment(seg, ScanState::default(), Some(&keys as &dyn KeyProvider)).unwrap();
        r.frames
            .into_iter()
            .map(|f| (f.record_type, f.payload.unwrap()))
            .collect()
    }

    /// Decode and re-encode every payload of one type; return how many.
    fn round_trip<T>(
        seg: &[u8],
        rt: RecordType,
        decode: fn(&[u8]) -> Option<T>,
        encode: fn(&T) -> Vec<u8>,
    ) -> usize {
        let mut n = 0;
        for (t, p) in payloads(seg) {
            if t == rt {
                let v =
                    decode(&p).unwrap_or_else(|| panic!("{} does not decode: {p:02x?}", rt.name()));
                assert_eq!(
                    encode(&v),
                    p,
                    "{} does not re-encode byte for byte",
                    rt.name()
                );
                n += 1;
            }
        }
        n
    }

    const GNSS_GAP_SEG: &[u8] = include_bytes!("../tests/vectors/gnss-gap.bin");
    const TIME_OBS: &[u8] = include_bytes!("../tests/vectors/time-observation.bin");
    const HEALTH: &[u8] = include_bytes!("../tests/vectors/health-bitmap.bin");
    const JOURNAL: &[u8] = include_bytes!("../tests/vectors/journal-segment.bin");

    #[test]
    fn gnss_sample_matches_the_vectors() {
        let n = round_trip(
            GNSS_GAP_SEG,
            RecordType::GNSS_SAMPLE,
            GnssSample::decode,
            |v| v.encode().to_vec(),
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn gnss_gap_matches_the_vector() {
        let n = round_trip(GNSS_GAP_SEG, RecordType::GNSS_GAP, GnssGap::decode, |v| {
            v.encode().to_vec()
        });
        assert_eq!(n, 1);
        let gap = payloads(GNSS_GAP_SEG)
            .into_iter()
            .find(|(t, _)| *t == RecordType::GNSS_GAP)
            .and_then(|(_, p)| GnssGap::decode(&p))
            .unwrap();
        assert_eq!(gap.duration_ms, 42_000, "the vector's 42-second tunnel");
    }

    #[test]
    fn health_matches_the_vector() {
        assert!(
            round_trip(
                HEALTH,
                RecordType::DEVICE_HEALTH,
                DeviceHealth::decode,
                |v| v.encode().to_vec()
            ) > 0
        );
    }

    #[test]
    fn time_observation_matches_the_vector() {
        assert!(
            round_trip(
                TIME_OBS,
                RecordType::TIME_OBSERVATION,
                TimeObservation::decode,
                |v| v.encode().to_vec()
            ) > 0
        );
    }

    #[test]
    fn state_transition_matches_the_journal_vector() {
        assert!(
            round_trip(
                JOURNAL,
                RecordType::STATE_TRANSITION,
                StateTransition::decode,
                |v| v.encode().to_vec()
            ) > 0
        );
    }

    /// No committed vector carries a real `IMU_SUMMARY` payload (`valid-minimal`
    /// has 16 zero bytes as a placeholder), so the layout is checked against the
    /// spec table offset by offset.
    #[test]
    fn imu_summary_layout_follows_the_spec_table() {
        let s = ImuSummary {
            window_ms: 0x0102,
            accel_rms_mg: 0x0304,
            accel_peak_x_mg: -2,
            accel_peak_y_mg: 0x0506,
            accel_peak_z_mg: 0x0708,
            gyro_peak_dps_e1: -3,
            variance: 0x090a,
            sample_count: 50,
            event_flags: 0b1001,
        };
        let b = s.encode();
        assert_eq!(
            b,
            [
                0x02, 0x01, 0x04, 0x03, 0xfe, 0xff, 0x06, 0x05, 0x08, 0x07, 0xfd, 0xff, 0x0a, 0x09,
                50, 0, 0b1001, 0, 0, 0
            ]
        );
        assert_eq!(ImuSummary::decode(&b), Some(s));
    }

    #[test]
    fn imu_window_summarises() {
        let mut w = ImuWindow::default();
        for i in 0..50u64 {
            w.add(i * 20, [0, 0, 1000], [0, 0, if i == 7 { -123 } else { 5 }]);
        }
        w.add(1000, [-1500, 300, 1000], [0, 0, 0]);
        assert_eq!(w.span_ms(), 1000);
        let s = w.take(1000).unwrap();
        assert_eq!(s.sample_count, 51);
        assert_eq!(
            (s.accel_peak_x_mg, s.accel_peak_y_mg, s.accel_peak_z_mg),
            (-1500, 300, 1000)
        );
        assert_eq!(s.gyro_peak_dps_e1, -123);
        assert!((1000..1100).contains(&s.accel_rms_mg), "{}", s.accel_rms_mg);
        assert!(w.is_empty());
        assert_eq!(w.take(1000), None);
    }

    #[test]
    fn centimetres_saturate_below_the_sentinel() {
        assert_eq!(cm_u16(1234), 123);
        assert_eq!(cm_u16(u32::MAX), UNKNOWN_U16 - 1);
    }
}
