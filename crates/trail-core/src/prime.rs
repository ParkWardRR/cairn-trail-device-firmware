//! Priming the GNSS receiver at power-on with what the logger last knew: its
//! position and the time. A receiver that wakes with neither searches the
//! whole sky blind (a cold start, minutes); told roughly where and when it is,
//! it looks only for the satellites that should be up.
//!
//! The record lives in `PRIME.TXT` in the card's root, plain text so it can be
//! read and edited on any computer:
//!
//! ```text
//! lat=37.4219980
//! lon=-122.0840570
//! alt_m=12.30
//! acc_m=8.00
//! fix_unix=1792886400
//! rtc_unix=1792886400
//! source=gnss
//! ```
//!
//! `alt_m` is height above the WGS84 ellipsoid (what the receiver wants, not
//! height above sea level). `fix_unix` is when the position was true;
//! `rtc_unix` is when the RTC was last checked against a clock worth
//! believing (GNSS or the computer). Either is 0 when unknown. Write
//! `source=manual` and a position by hand to tell the logger where it is.
//!
//! What is sent follows from how much each piece can be trusted — see
//! [`plan`]. The receiver weighs every hint by the accuracy given with it, so
//! the accuracy is the honest part: a position from yesterday is given with an
//! uncertainty as large as the distance a car could have covered since.

use core::fmt;

/// Where the record came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Gnss,
    Manual,
    Wifi,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Gnss => "gnss",
            Source::Manual => "manual",
            Source::Wifi => "wifi",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prime {
    pub lat_e7: i32,
    pub lon_e7: i32,
    /// Height above the WGS84 ellipsoid, centimetres.
    pub alt_cm: i32,
    /// Horizontal uncertainty of the position when it was true, centimetres.
    pub acc_cm: u32,
    /// When the position was true; 0 if unknown.
    pub fix_unix: i64,
    /// When the RTC was last checked against a trusted clock; 0 if never.
    pub rtc_unix: i64,
    pub source: Source,
}

/// Accuracy given with a hand-entered position that names none: 1 km.
pub const MANUAL_ACC_CM: u32 = 100_000;
/// How fast the logger is assumed to have moved while off: a car on a
/// highway, 25 m/s.
pub const TRAVEL_CM_PER_S: u64 = 2_500;
/// Beyond this uncertainty a position says nothing useful: 1000 km.
pub const MAX_POS_ACC_CM: u64 = 100_000_000;
/// Accuracy for a position whose age cannot be known (no trusted clock): 300 km.
pub const UNKNOWN_AGE_ACC_CM: u32 = 30_000_000;
/// The RTC is believed for this long after it was last checked: 180 days.
pub const RTC_TRUST_S: i64 = 180 * 86_400;
/// RTC uncertainty right after a check, and its growth per day. The PCF8563's
/// crystal is good to tens of ppm; 4 s a day is about 46 ppm.
pub const RTC_BASE_ACC_MS: u64 = 2_000;
pub const RTC_DRIFT_MS_PER_DAY: u64 = 4_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeAid {
    pub unix: i64,
    pub acc_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PosAid {
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub alt_cm: i32,
    pub acc_cm: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub time: Option<TimeAid>,
    pub pos: Option<PosAid>,
}

/// What to tell the receiver, given the record (if any) and the RTC reading
/// now (`None` if the RTC is implausible or unreadable).
///
/// - **Time** only if the record says the RTC was checked against a trusted
///   clock, the RTC has not gone backwards since, and the check is under 180
///   days old. An RTC set at the factory reads plausibly and can still be
///   weeks wrong (this board's was 25 days), so a plausible reading alone is
///   not enough.
/// - **Position** with its accuracy grown by the time since it was true, at
///   highway speed, when that time is known; a hand-entered position with no
///   time is taken as current; any other position of unknown age is given a
///   300 km uncertainty. Nothing is sent once the uncertainty passes 1000 km.
pub fn plan(rec: Option<&Prime>, rtc_now: Option<i64>) -> Plan {
    let Some(r) = rec else {
        return Plan::default();
    };
    let trusted_now = rtc_now
        .filter(|&now| r.rtc_unix > 0 && now >= r.rtc_unix - 2 && now - r.rtc_unix <= RTC_TRUST_S);
    let time = trusted_now.map(|now| {
        let days = ((now - r.rtc_unix).max(0) as u64).div_ceil(86_400);
        TimeAid {
            unix: now,
            acc_ms: (RTC_BASE_ACC_MS + days * RTC_DRIFT_MS_PER_DAY).min(u32::MAX as u64) as u32,
        }
    });

    let valid_pos = (-900_000_000..=900_000_000).contains(&r.lat_e7)
        && (-1_800_000_000..=1_800_000_000).contains(&r.lon_e7)
        && !(r.lat_e7 == 0 && r.lon_e7 == 0);
    let acc_cm: Option<u64> = if !valid_pos {
        None
    } else if r.fix_unix == 0 && r.source == Source::Manual {
        Some(if r.acc_cm == 0 {
            MANUAL_ACC_CM
        } else {
            r.acc_cm
        } as u64)
    } else if let Some(now) = trusted_now.filter(|&now| r.fix_unix > 0 && now >= r.fix_unix) {
        Some(r.acc_cm as u64 + (now - r.fix_unix) as u64 * TRAVEL_CM_PER_S)
    } else {
        Some(UNKNOWN_AGE_ACC_CM.max(r.acc_cm) as u64)
    };
    let pos = acc_cm.filter(|&a| a <= MAX_POS_ACC_CM).map(|a| PosAid {
        lat_e7: r.lat_e7,
        lon_e7: r.lon_e7,
        alt_cm: r.alt_cm,
        acc_cm: a as u32,
    });
    Plan { time, pos }
}

/// A fixed-point decimal ("-122.0840570") as an integer scaled by 10^`digits`,
/// without floats. Extra fraction digits are truncated.
pub fn parse_fixed(s: &str, digits: u32) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for c in int.chars() {
        v = v.checked_mul(10)?.checked_add(c.to_digit(10)? as i64)?;
    }
    let mut taken = 0;
    for c in frac.chars() {
        let d = c.to_digit(10)? as i64;
        if taken < digits {
            v = v.checked_mul(10)?.checked_add(d)?;
            taken += 1;
        }
    }
    for _ in taken..digits {
        v = v.checked_mul(10)?;
    }
    Some(if neg { -v } else { v })
}

impl Prime {
    /// Parse a `PRIME.TXT`. Unknown keys and `#` comments are ignored; lat and
    /// lon are required.
    pub fn parse(text: &str) -> Option<Prime> {
        let mut lat = None;
        let mut lon = None;
        let mut p = Prime {
            lat_e7: 0,
            lon_e7: 0,
            alt_cm: 0,
            acc_cm: 0,
            fix_unix: 0,
            rtc_unix: 0,
            source: Source::Manual,
        };
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "lat" => lat = parse_fixed(v, 7).and_then(|x| i32::try_from(x).ok()),
                "lon" => lon = parse_fixed(v, 7).and_then(|x| i32::try_from(x).ok()),
                "alt_m" => p.alt_cm = parse_fixed(v, 2).and_then(|x| i32::try_from(x).ok())?,
                "acc_m" => p.acc_cm = parse_fixed(v, 2).and_then(|x| u32::try_from(x).ok())?,
                "fix_unix" => p.fix_unix = v.parse().ok()?,
                "rtc_unix" => p.rtc_unix = v.parse().ok()?,
                "source" => {
                    p.source = match v {
                        "gnss" => Source::Gnss,
                        "wifi" => Source::Wifi,
                        _ => Source::Manual,
                    }
                }
                _ => {}
            }
        }
        p.lat_e7 = lat?;
        p.lon_e7 = lon?;
        Some(p)
    }
}

/// Signed fixed-point with `digits` fraction digits.
struct Fixed(i64, u32);
impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scale = 10i64.pow(self.1);
        let sign = if self.0 < 0 { "-" } else { "" };
        let a = self.0.unsigned_abs();
        write!(
            f,
            "{}{}.{:0w$}",
            sign,
            a / scale as u64,
            a % scale as u64,
            w = self.1 as usize
        )
    }
}

/// The file as the logger writes it.
impl fmt::Display for Prime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "# Cairn trail logger: what it last knew, to prime the GNSS receiver at power-on."
        )?;
        writeln!(f, "# Edit lat/lon (alt_m, acc_m optional) and set source=manual, fix_unix=0 to give a position.")?;
        writeln!(f, "lat={}", Fixed(self.lat_e7 as i64, 7))?;
        writeln!(f, "lon={}", Fixed(self.lon_e7 as i64, 7))?;
        writeln!(f, "alt_m={}", Fixed(self.alt_cm as i64, 2))?;
        writeln!(f, "acc_m={}", Fixed(self.acc_cm as i64, 2))?;
        writeln!(f, "fix_unix={}", self.fix_unix)?;
        writeln!(f, "rtc_unix={}", self.rtc_unix)?;
        writeln!(f, "source={}", self.source.as_str())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;

    const T0: i64 = 1_792_886_400; // 2026-10-25T00:00:00Z

    fn rec() -> Prime {
        Prime {
            lat_e7: 374_219_980,
            lon_e7: -1_220_840_570,
            alt_cm: 1_230,
            acc_cm: 800,
            fix_unix: T0,
            rtc_unix: T0,
            source: Source::Gnss,
        }
    }

    #[test]
    fn round_trips_through_text() {
        let r = rec();
        let text = r.to_string();
        assert!(text.contains("lat=37.4219980\n"));
        assert!(text.contains("lon=-122.0840570\n"));
        assert!(text.contains("alt_m=12.30\n"));
        assert_eq!(Prime::parse(&text), Some(r));
    }

    #[test]
    fn small_negative_values_keep_their_sign() {
        let r = Prime {
            lat_e7: -5,
            lon_e7: 1,
            alt_cm: -3,
            ..rec()
        };
        let text = r.to_string();
        assert!(text.contains("lat=-0.0000005\n"));
        assert!(text.contains("alt_m=-0.03\n"));
        assert_eq!(Prime::parse(&text), Some(r));
    }

    #[test]
    fn a_hand_written_file_parses() {
        let p = Prime::parse("# home\nlat = 47.6\nlon=-122.33 # Seattle\nsource=manual\n").unwrap();
        assert_eq!(
            (p.lat_e7, p.lon_e7, p.fix_unix, p.source),
            (476_000_000, -1_223_300_000, 0, Source::Manual)
        );
        assert_eq!(Prime::parse("lat=47.6\n"), None);
        assert_eq!(Prime::parse("lat=47.6\nlon=abc\n"), None);
    }

    #[test]
    fn no_record_means_nothing_to_send() {
        assert_eq!(plan(None, Some(T0)), Plan::default());
    }

    #[test]
    fn a_checked_rtc_gives_time_and_an_aged_position() {
        // One hour later: 2 s + one day's drift; 8 m + 3600 s at 25 m/s.
        let p = plan(Some(&rec()), Some(T0 + 3600));
        assert_eq!(
            p.time,
            Some(TimeAid {
                unix: T0 + 3600,
                acc_ms: 6_000
            })
        );
        assert_eq!(p.pos.unwrap().acc_cm, 800 + 3600 * 2_500);
    }

    #[test]
    fn a_never_checked_rtc_is_not_believed() {
        let r = Prime {
            rtc_unix: 0,
            ..rec()
        };
        let p = plan(Some(&r), Some(T0 + 60));
        assert_eq!(p.time, None);
        assert_eq!(p.pos.unwrap().acc_cm, UNKNOWN_AGE_ACC_CM);
    }

    #[test]
    fn an_rtc_that_went_backwards_is_not_believed() {
        let p = plan(Some(&rec()), Some(T0 - 3600));
        assert_eq!(p.time, None);
    }

    #[test]
    fn an_rtc_checked_long_ago_is_not_believed() {
        let p = plan(Some(&rec()), Some(T0 + RTC_TRUST_S + 1));
        assert_eq!(p.time, None);
    }

    #[test]
    fn a_position_too_old_to_help_is_dropped() {
        // 12 hours at 25 m/s is 1080 km.
        let p = plan(Some(&rec()), Some(T0 + 12 * 3600));
        assert!(p.time.is_some());
        assert_eq!(p.pos, None);
    }

    #[test]
    fn a_manual_position_is_taken_as_current() {
        let r = Prime {
            fix_unix: 0,
            rtc_unix: 0,
            acc_cm: 0,
            source: Source::Manual,
            ..rec()
        };
        let p = plan(Some(&r), None);
        assert_eq!(p.pos.unwrap().acc_cm, MANUAL_ACC_CM);
    }

    #[test]
    fn null_island_is_not_a_position() {
        let r = Prime {
            lat_e7: 0,
            lon_e7: 0,
            ..rec()
        };
        assert_eq!(plan(Some(&r), Some(T0)).pos, None);
    }
}
