//! Wi-Fi fingerprint positioning, learned by the logger itself and used with
//! no internet: where the GNSS has had a good fix, the access points heard
//! there are recorded with the position (a *fingerprint*); at power-on, a scan
//! is matched against the fingerprints by k-nearest-neighbours on signal
//! strength, and the result primes the receiver with an approximate position.
//!
//! The method is the fingerprinting-and-KNN approach of
//! [wireless_localization2](https://github.com/dmachinewhisperer/wireless_localization2)
//! (an ESP32 indoor-localization thesis project), turned around: that project
//! surveys a building by hand and runs the model on a server or on the chip;
//! here the GNSS does the survey, wherever the logger goes, and the chip does
//! the matching.
//!
//! One fingerprint per line of `WIFIFP.CSV` in the card's root:
//!
//! ```text
//! 1792886400,37.4219980,-122.0840570,8,88dc9707b8db:-63 a0b1c2d3e4f5:-80
//! ```
//!
//! `unix` (0 if unknown), latitude, longitude, the fix's horizontal accuracy in
//! metres, then up to 16 access points as BSSID:RSSI (dBm).

use core::fmt;

pub const MAX_APS: usize = 16;
/// RSSI given to an access point heard in one scan but not the other.
pub const MISSING_DBM: i32 = -100;
/// How close two fingerprints may be before the second adds nothing: 100 m.
pub const LEARN_SPACING_M: f32 = 100.0;
/// Accuracy floor of a Wi-Fi position: an access point is heard ~100 m away.
pub const BASE_ACC_M: f32 = 100.0;
/// Neighbours spread wider than this disagree (an access point that moved,
/// two places with one network name): no estimate. 5 km.
pub const MAX_SPREAD_M: f32 = 5_000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ap {
    pub bssid: [u8; 6],
    pub rssi: i8,
}

/// A BSSID with the locally-administered bit set: phone hotspots, randomised
/// and virtual interfaces. They move with their owners (often with the logger
/// itself), so they say nothing about where the logger is.
pub fn is_mobile(bssid: &[u8; 6]) -> bool {
    bssid[0] & 0x02 != 0
}

/// The strongest fixed access points of a scan, strongest first, at most
/// `MAX_APS`. Returns how many were written to `out`.
pub fn select(scan: &[Ap], out: &mut [Ap; MAX_APS]) -> usize {
    let mut n = 0;
    for ap in scan.iter().filter(|a| !is_mobile(&a.bssid)) {
        if out[..n].iter().any(|o| o.bssid == ap.bssid) {
            continue;
        }
        // Insertion into a list kept sorted by RSSI, strongest first.
        let pos = out[..n].iter().position(|o| ap.rssi > o.rssi).unwrap_or(n);
        if pos >= MAX_APS {
            continue;
        }
        let last = n.min(MAX_APS - 1);
        out.copy_within(pos..last, pos + 1);
        out[pos] = *ap;
        n = (n + 1).min(MAX_APS);
    }
    n
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub unix: i64,
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub acc_m: u16,
    pub aps: [Ap; MAX_APS],
    pub n: u8,
}

impl Fingerprint {
    pub fn aps(&self) -> &[Ap] {
        &self.aps[..self.n as usize]
    }

    /// Parse one line; None for anything malformed (a torn last line after a
    /// power cut, a comment).
    pub fn parse(line: &str) -> Option<Fingerprint> {
        let mut f = line.trim().split(',');
        let unix = f.next()?.parse().ok()?;
        let lat_e7 = i32::try_from(crate::prime::parse_fixed(f.next()?, 7)?).ok()?;
        let lon_e7 = i32::try_from(crate::prime::parse_fixed(f.next()?, 7)?).ok()?;
        let acc_m = f.next()?.parse().ok()?;
        let mut fp = Fingerprint {
            unix,
            lat_e7,
            lon_e7,
            acc_m,
            aps: [Ap::default(); MAX_APS],
            n: 0,
        };
        for item in f.next()?.split_whitespace() {
            let (b, r) = item.split_once(':')?;
            if b.len() != 12 || fp.n as usize >= MAX_APS {
                return None;
            }
            let mut bssid = [0u8; 6];
            for (i, byte) in bssid.iter_mut().enumerate() {
                *byte = u8::from_str_radix(b.get(2 * i..2 * i + 2)?, 16).ok()?;
            }
            fp.aps[fp.n as usize] = Ap {
                bssid,
                rssi: r.parse().ok()?,
            };
            fp.n += 1;
        }
        (fp.n > 0).then_some(fp)
    }
}

/// The line as written to the card (no newline).
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let deg = |v: i32| {
            let a = (v as i64).unsigned_abs();
            (if v < 0 { "-" } else { "" }, a / 10_000_000, a % 10_000_000)
        };
        let (s1, i1, f1) = deg(self.lat_e7);
        let (s2, i2, f2) = deg(self.lon_e7);
        write!(
            f,
            "{},{}{}.{:07},{}{}.{:07},{},",
            self.unix, s1, i1, f1, s2, i2, f2, self.acc_m
        )?;
        for (i, ap) in self.aps().iter().enumerate() {
            let b = ap.bssid;
            write!(
                f,
                "{}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}:{}",
                if i > 0 { " " } else { "" },
                b[0],
                b[1],
                b[2],
                b[3],
                b[4],
                b[5],
                ap.rssi
            )?;
        }
        Ok(())
    }
}

/// Metres between two points, by the equirectangular approximation (good to
/// well under 1 % at the distances that matter here).
pub fn distance_m(lat1_e7: i32, lon1_e7: i32, lat2_e7: i32, lon2_e7: i32) -> f32 {
    const M_PER_E7_DEG: f32 = 111_195.0 / 1e7;
    let mid = ((lat1_e7 as f64 + lat2_e7 as f64) / 2.0 / 1e7).to_radians();
    let dlat = (lat2_e7 as i64 - lat1_e7 as i64) as f32;
    let mut dlon = (lon2_e7 as i64 - lon1_e7 as i64) as f32;
    if dlon > 1.8e9 {
        dlon -= 3.6e9;
    } else if dlon < -1.8e9 {
        dlon += 3.6e9;
    }
    let x = dlon * libm::cos(mid) as f32;
    libm::sqrtf(x * x + dlat * dlat) * M_PER_E7_DEG
}

/// Signal-space distance between a scan and a fingerprint: root mean square
/// of the RSSI differences over every access point heard in either, an
/// unheard one counting as -100 dBm; and how many were heard in both.
pub fn signal_distance(scan: &[Ap], fp: &[Ap]) -> (f32, usize) {
    let mut sum = 0i64;
    let mut terms = 0i64;
    let mut common = 0;
    for a in scan {
        let other = fp
            .iter()
            .find(|b| b.bssid == a.bssid)
            .map(|b| b.rssi as i32);
        if other.is_some() {
            common += 1;
        }
        let d = a.rssi as i32 - other.unwrap_or(MISSING_DBM);
        sum += (d * d) as i64;
        terms += 1;
    }
    for b in fp
        .iter()
        .filter(|b| !scan.iter().any(|a| a.bssid == b.bssid))
    {
        let d = b.rssi as i32 - MISSING_DBM;
        sum += (d * d) as i64;
        terms += 1;
    }
    if terms == 0 {
        return (f32::MAX, 0);
    }
    (libm::sqrtf(sum as f32 / terms as f32), common)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub acc_m: f32,
    /// Neighbours used.
    pub k: usize,
    /// Signal distance of the best neighbour, dB.
    pub best_db: f32,
}

#[derive(Clone, Copy, Debug)]
struct Neighbour {
    db: f32,
    lat_e7: i32,
    lon_e7: i32,
}

/// k-nearest-neighbour matching, fed one fingerprint at a time so the file
/// never has to fit in memory.
pub struct Knn<const K: usize> {
    best: [Option<Neighbour>; K],
}

impl<const K: usize> Default for Knn<K> {
    fn default() -> Self {
        Self { best: [None; K] }
    }
}

impl<const K: usize> Knn<K> {
    /// Consider a fingerprint. It counts only if it shares at least two
    /// access points with the scan, or the scan heard just one and it shares
    /// that one.
    pub fn push(&mut self, scan: &[Ap], fp: &Fingerprint) {
        let (db, common) = signal_distance(scan, fp.aps());
        if !(common >= 2 || (common == 1 && scan.len() == 1)) {
            return;
        }
        let cand = Neighbour {
            db,
            lat_e7: fp.lat_e7,
            lon_e7: fp.lon_e7,
        };
        let slot = self.best.iter().position(|b| b.is_none_or(|b| db < b.db));
        if let Some(i) = slot {
            self.best.copy_within(i..K - 1, i + 1);
            self.best[i] = Some(cand);
        }
    }

    /// The neighbours' position, weighted by closeness in signal, with an
    /// accuracy of 100 m plus their spread; None if there are none or they
    /// disagree by more than 5 km.
    pub fn estimate(&self) -> Option<Estimate> {
        let found: &[Option<Neighbour>] = &self.best;
        let first = found[0]?;
        let (mut wsum, mut dlat, mut dlon, mut k) = (0f64, 0f64, 0f64, 0);
        for n in found.iter().flatten() {
            let w = 1.0 / (n.db as f64 + 1.0);
            // Relative to the first neighbour, so averaging cannot overflow
            // or straddle the antimeridian.
            let mut dl = (n.lon_e7 as i64 - first.lon_e7 as i64) as f64;
            if dl > 1.8e9 {
                dl -= 3.6e9;
            } else if dl < -1.8e9 {
                dl += 3.6e9;
            }
            dlat += w * (n.lat_e7 as i64 - first.lat_e7 as i64) as f64;
            dlon += w * dl;
            wsum += w;
            k += 1;
        }
        let lat_e7 = (first.lat_e7 as f64 + dlat / wsum) as i32;
        let mut lon = first.lon_e7 as f64 + dlon / wsum;
        if lon > 1.8e9 {
            lon -= 3.6e9;
        } else if lon < -1.8e9 {
            lon += 3.6e9;
        }
        let lon_e7 = lon as i32;
        let spread = found
            .iter()
            .flatten()
            .map(|n| distance_m(lat_e7, lon_e7, n.lat_e7, n.lon_e7))
            .fold(0f32, f32::max);
        if spread > MAX_SPREAD_M {
            return None;
        }
        Some(Estimate {
            lat_e7,
            lon_e7,
            acc_m: BASE_ACC_M + spread,
            k,
            best_db: first.db,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;

    fn ap(last: u8, rssi: i8) -> Ap {
        Ap {
            bssid: [0x88, 0xdc, 0x97, 0x07, 0xb8, last],
            rssi,
        }
    }

    fn fp(lat_e7: i32, lon_e7: i32, aps: &[Ap]) -> Fingerprint {
        let mut f = Fingerprint {
            unix: 0,
            lat_e7,
            lon_e7,
            acc_m: 5,
            aps: [Ap::default(); MAX_APS],
            n: aps.len() as u8,
        };
        f.aps[..aps.len()].copy_from_slice(aps);
        f
    }

    #[test]
    fn a_line_round_trips() {
        let f = Fingerprint {
            unix: 1_792_886_400,
            ..fp(374_219_980, -1_220_840_570, &[ap(0xdb, -63), ap(0x01, -80)])
        };
        let s = f.to_string();
        assert_eq!(
            s,
            "1792886400,37.4219980,-122.0840570,5,88dc9707b8db:-63 88dc9707b801:-80"
        );
        assert_eq!(Fingerprint::parse(&s), Some(f));
    }

    #[test]
    fn torn_and_foreign_lines_are_rejected() {
        assert_eq!(
            Fingerprint::parse("1792886400,37.42,-122.08,5,88dc9707b8"),
            None
        );
        assert_eq!(Fingerprint::parse("1792886400,37.42,-122.08,5,"), None);
        assert_eq!(Fingerprint::parse("# comment"), None);
    }

    #[test]
    fn select_keeps_the_strongest_fixed_aps() {
        let mut scan = std::vec::Vec::new();
        for i in 0..20u8 {
            scan.push(ap(i, -90 + i as i8));
        }
        // A phone hotspot (locally administered), loudest of all.
        scan.push(Ap {
            bssid: [0x02, 1, 2, 3, 4, 5],
            rssi: -30,
        });
        scan.push(ap(19, -71)); // a duplicate
        let mut out = [Ap::default(); MAX_APS];
        let n = select(&scan, &mut out);
        assert_eq!(n, MAX_APS);
        assert_eq!(out[0], ap(19, -71));
        assert_eq!(out[15], ap(4, -86));
        assert!(out.iter().all(|a| !is_mobile(&a.bssid)));
    }

    #[test]
    fn distance_is_about_right() {
        // 0.001 degree of latitude is ~111 m.
        let d = distance_m(374_000_000, -1_220_000_000, 374_010_000, -1_220_000_000);
        assert!((d - 111.2).abs() < 0.5, "{d}");
        // Across the antimeridian, 0.002 degrees of longitude at the equator.
        let d = distance_m(0, 1_799_990_000, 0, -1_799_990_000);
        assert!((d - 222.4).abs() < 1.0, "{d}");
    }

    #[test]
    fn knn_finds_the_matching_place() {
        let home = [ap(1, -50), ap(2, -60), ap(3, -70)];
        let work = [ap(7, -55), ap(8, -65), ap(9, -75)];
        let mut knn = Knn::<3>::default();
        knn.push(&home, &fp(476_062_000, -1_223_321_000, &home));
        knn.push(
            &home,
            &fp(476_063_000, -1_223_322_000, &[ap(1, -58), ap(2, -61)]),
        );
        knn.push(&home, &fp(476_200_000, -1_222_000_000, &work));
        let e = knn.estimate().unwrap();
        assert_eq!(e.k, 2, "the other place shares no access points");
        assert!(distance_m(e.lat_e7, e.lon_e7, 476_062_000, -1_223_321_000) < 20.0);
        assert!(
            e.acc_m >= BASE_ACC_M && e.acc_m < BASE_ACC_M + 20.0,
            "{}",
            e.acc_m
        );
    }

    #[test]
    fn one_shared_ap_is_not_enough_when_more_were_heard() {
        let scan = [ap(1, -50), ap(5, -60)];
        let mut knn = Knn::<3>::default();
        knn.push(
            &scan,
            &fp(476_062_000, -1_223_321_000, &[ap(1, -50), ap(9, -60)]),
        );
        assert_eq!(knn.estimate(), None);
    }

    #[test]
    fn neighbours_far_apart_give_no_estimate() {
        let scan = [ap(1, -50), ap(2, -60)];
        let mut knn = Knn::<3>::default();
        knn.push(&scan, &fp(476_062_000, -1_223_321_000, &scan));
        knn.push(&scan, &fp(455_000_000, -1_226_000_000, &scan)); // ~230 km south
        assert_eq!(knn.estimate(), None);
    }
}
