//! The bundle `device_counter` (format/v3 §5.1, key 26): starts at 1, and
//! must never be issued twice — every frame of a bundle is sealed against it,
//! so a repeat after a power cut would put two bundles under one identity.
//!
//! It lives in two 4 KiB flash sectors as an append-only log of 16-byte slots
//! (`CNTR` | value u64 LE | CRC-32 of the first 12 bytes). The current value
//! is the largest valid slot in either sector. A new value goes in the next
//! erased slot of the sector holding the current one; when that sector is
//! full, the *other* sector is erased and written. The sector holding the
//! current value is never erased, so a power cut at any point leaves either
//! the old value or the new one — never neither. Erased flash reads 0xFF.
//!
//! Callers write the next value **before** using it: a value that was
//! written but never used is a harmless gap; one used but not written would
//! be issued again.

pub const SECTOR: usize = 4096;
pub const SLOT: usize = 16;
pub const SLOTS: usize = SECTOR / SLOT;
const MAGIC: &[u8; 4] = b"CNTR";

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
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
    !crc
}

pub fn encode(value: u64) -> [u8; SLOT] {
    let mut s = [0u8; SLOT];
    s[0..4].copy_from_slice(MAGIC);
    s[4..12].copy_from_slice(&value.to_le_bytes());
    let c = crc32(&s[..12]);
    s[12..16].copy_from_slice(&c.to_le_bytes());
    s
}

/// The value in a slot, if it is a whole, valid one.
pub fn decode(s: &[u8]) -> Option<u64> {
    if s.len() != SLOT || &s[0..4] != MAGIC {
        return None;
    }
    let c = u32::from_le_bytes([s[12], s[13], s[14], s[15]]);
    (crc32(&s[..12]) == c).then(|| u64::from_le_bytes(s[4..12].try_into().unwrap()))
}

fn erased(s: &[u8]) -> bool {
    s.iter().all(|&b| b == 0xFF)
}

/// What the log holds: the current value (0 if none yet) and where to write
/// the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub current: u64,
    /// Sector (0 or 1) and slot index for the next value.
    pub next_sector: usize,
    pub next_slot: usize,
    /// The next value goes into a sector that must be erased first.
    pub erase_first: bool,
}

/// Read both sectors' state. A torn slot (neither erased nor valid) is
/// skipped over: the slot after it is used, never the torn one.
pub fn state(a: &[u8], b: &[u8]) -> State {
    let scan = |sec: &[u8]| {
        let mut best: Option<u64> = None;
        let mut first_free = None;
        for (i, s) in sec.chunks_exact(SLOT).enumerate() {
            if let Some(v) = decode(s) {
                best = Some(best.map_or(v, |b: u64| b.max(v)));
                first_free = None;
            } else if erased(s) {
                if first_free.is_none() {
                    first_free = Some(i);
                }
            } else {
                first_free = None; // torn: writing continues after it
            }
        }
        (best, first_free)
    };
    let (va, fa) = scan(a);
    let (vb, fb) = scan(b);
    let (current, sector, free) = match (va, vb) {
        (None, None) => (0, 0, fa),
        (Some(x), None) => (x, 0, fa),
        (None, Some(y)) => (y, 1, fb),
        (Some(x), Some(y)) if x >= y => (x, 0, fa),
        (Some(_), Some(y)) => (y, 1, fb),
    };
    match free {
        Some(slot) if !(current == 0 && sector == 0 && va.is_none() && !erased(a)) => State {
            current,
            next_sector: sector,
            next_slot: slot,
            erase_first: false,
        },
        _ => State {
            current,
            next_sector: if current == 0 { 0 } else { 1 - sector },
            next_slot: 0,
            erase_first: true,
        },
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Two sectors of simulated NOR flash.
    struct Flash {
        s: [Vec<u8>; 2],
    }

    impl Flash {
        fn new() -> Self {
            Self {
                s: [vec![0xFF; SECTOR], vec![0xFF; SECTOR]],
            }
        }
        /// Issue the next value, as the firmware does; `cut` stops part-way
        /// through the step it names to model a power cut.
        fn bump(&mut self, cut: Option<&str>) -> u64 {
            let st = state(&self.s[0], &self.s[1]);
            let next = st.current + 1;
            if st.erase_first {
                self.s[st.next_sector].fill(0xFF);
                if cut == Some("after-erase") {
                    return st.current;
                }
            }
            let slot = encode(next);
            let o = st.next_slot * SLOT;
            if cut == Some("torn-write") {
                self.s[st.next_sector][o..o + 7].copy_from_slice(&slot[..7]);
                return st.current;
            }
            self.s[st.next_sector][o..o + SLOT].copy_from_slice(&slot);
            next
        }
        fn current(&self) -> u64 {
            state(&self.s[0], &self.s[1]).current
        }
    }

    #[test]
    fn starts_at_one_and_counts_up_across_sector_flips() {
        let mut f = Flash::new();
        assert_eq!(f.current(), 0);
        for want in 1..=(3 * SLOTS as u64 + 5) {
            assert_eq!(f.bump(None), want);
            assert_eq!(f.current(), want);
        }
    }

    #[test]
    fn a_power_cut_never_loses_the_current_value() {
        for cut in ["after-erase", "torn-write"] {
            let mut f = Flash::new();
            // Fill the first sector so the next bump has to erase the other.
            for _ in 0..SLOTS {
                f.bump(None);
            }
            let before = f.current();
            assert_eq!(before, SLOTS as u64);
            f.bump(Some(cut));
            assert_eq!(f.current(), before, "{cut}: value lost or moved");
            // And the next clean bump issues the next value, never a repeat.
            assert_eq!(f.bump(None), before + 1, "{cut}");
        }
    }

    #[test]
    fn a_torn_slot_in_the_middle_is_skipped() {
        let mut f = Flash::new();
        f.bump(None);
        f.bump(Some("torn-write"));
        assert_eq!(f.current(), 1);
        assert_eq!(f.bump(None), 2);
        assert_eq!(f.bump(None), 3);
        assert_eq!(f.current(), 3);
    }

    #[test]
    fn a_corrupted_sector_never_reads_as_valid() {
        let mut s = encode(7);
        s[5] ^= 1;
        assert_eq!(decode(&s), None);
        assert_eq!(decode(&encode(7)), Some(7));
    }
}
