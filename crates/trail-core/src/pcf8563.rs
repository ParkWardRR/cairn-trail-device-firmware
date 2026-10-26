//! NXP PCF8563 real-time clock: decoding and encoding its BCD time registers,
//! and the plausibility rule for using it as a time source.

pub const ADDR: u8 = 0x51;
pub const CTRL1: u8 = 0x00;
pub const SECONDS: u8 = 0x02; // seven registers, 0x02..=0x08

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

fn bcd(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0F)
}
fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Decode the seven registers from 0x02. The second value is the voltage-low
/// flag: when set, the clock lost power and its time is not to be trusted.
pub fn decode(r: &[u8; 7]) -> (DateTime, bool) {
    let vl = r[0] & 0x80 != 0;
    (
        DateTime {
            sec: bcd(r[0] & 0x7F),
            min: bcd(r[1] & 0x7F),
            hour: bcd(r[2] & 0x3F),
            day: bcd(r[3] & 0x3F),
            month: bcd(r[5] & 0x1F),
            year: 2000 + bcd(r[6]) as u16,
        },
        vl,
    )
}

/// Day of week, 0 = Sunday (Sakamoto's method).
pub fn weekday(y: u16, m: u8, d: u8) -> u8 {
    const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as u16) % 7) as u8
}

/// Encode for writing to 0x02..=0x08. Writing the seconds register clears
/// the voltage-low flag.
pub fn encode(t: &DateTime) -> [u8; 7] {
    [
        to_bcd(t.sec),
        to_bcd(t.min),
        to_bcd(t.hour),
        to_bcd(t.day),
        weekday(t.year, t.month, t.day),
        to_bcd(t.month),
        to_bcd((t.year % 100) as u8),
    ]
}

/// Earliest year a clock reading is believed: older than the firmware is
/// impossible. Invariant 6 — every clock can lie in a way that parses.
pub const MIN_YEAR: u16 = 2026;
pub const MAX_YEAR: u16 = 2099;

pub fn plausible(t: &DateTime, voltage_low: bool) -> bool {
    !voltage_low
        && (MIN_YEAR..=MAX_YEAR).contains(&t.year)
        && (1..=12).contains(&t.month)
        && (1..=31).contains(&t.day)
        && t.hour < 24
        && t.min < 60
        && t.sec < 60
}

/// Seconds since the Unix epoch, for comparing two clocks.
pub fn unix_seconds(t: &DateTime) -> i64 {
    // Days from civil, Howard Hinnant's algorithm.
    let y = t.year as i64 - if t.month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = t.month as i64;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + t.day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + t.hour as i64 * 3_600 + t.min as i64 * 60 + t.sec as i64
}

/// The inverse of `unix_seconds` (Hinnant's civil-from-days).
pub fn from_unix(secs: i64) -> DateTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + if month <= 2 { 1 } else { 0 }) as u16;
    DateTime {
        year,
        month,
        day,
        hour: (rem / 3_600) as u8,
        min: (rem % 3_600 / 60) as u8,
        sec: (rem % 60) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = DateTime {
            year: 2026,
            month: 10,
            day: 25,
            hour: 21,
            min: 5,
            sec: 9,
        };
        let r = encode(&t);
        assert_eq!(decode(&r), (t, false));
        assert_eq!(r[4], 0); // 2026-10-25 is a Sunday
    }

    #[test]
    fn voltage_low_flag_is_reported_and_distrusted() {
        let t = DateTime {
            year: 2026,
            month: 1,
            day: 1,
            hour: 0,
            min: 0,
            sec: 0,
        };
        let mut r = encode(&t);
        r[0] |= 0x80;
        let (d, vl) = decode(&r);
        assert!(vl);
        assert!(!plausible(&d, vl));
    }

    #[test]
    fn year_two_thousand_is_not_plausible() {
        let r = [0, 0, 0, 1, 6, 1, 0];
        let (d, vl) = decode(&r);
        assert_eq!(d.year, 2000);
        assert!(!plausible(&d, vl));
    }

    #[test]
    fn unix_seconds_known_value() {
        let t = DateTime {
            year: 2026,
            month: 10,
            day: 25,
            hour: 0,
            min: 0,
            sec: 0,
        };
        assert_eq!(unix_seconds(&t), 1_792_886_400);
    }

    #[test]
    fn from_unix_inverts_unix_seconds() {
        for s in [
            0i64,
            951_782_400,
            1_792_886_400,
            1_792_972_799,
            4_102_444_800,
        ] {
            assert_eq!(unix_seconds(&from_unix(s)), s);
        }
        let t = from_unix(1_792_886_400 + 21 * 3600 + 5 * 60 + 9);
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.min, t.sec),
            (2026, 10, 25, 21, 5, 9)
        );
    }
}
