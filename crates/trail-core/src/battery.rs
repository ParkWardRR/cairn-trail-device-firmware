//! Battery state of charge from cell voltage, by an open-circuit-voltage
//! curve. Under load the cell reads low, so this is an estimate; the AXP2101's
//! own fuel gauge is recorded beside it.

/// Typical Li-ion 18650, 100 % down to 0 % in 10 % steps (millivolts).
/// The same shape Meshtastic uses; a straight line reads badly mid-range.
pub const OCV_MV: [u16; 11] = [
    4190, 4050, 3990, 3890, 3800, 3720, 3630, 3530, 3420, 3300, 3100,
];

/// State of charge in percent, 0–100.
pub fn percent(mv: u16) -> u8 {
    if mv >= OCV_MV[0] {
        return 100;
    }
    if mv <= OCV_MV[10] {
        return 0;
    }
    for i in 0..10 {
        let (hi, lo) = (OCV_MV[i], OCV_MV[i + 1]);
        if mv <= hi && mv > lo {
            let top = 100 - 10 * i as u32;
            let frac = (mv - lo) as u32 * 10 / (hi - lo) as u32;
            return (top - 10 + frac) as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_steps() {
        assert_eq!(percent(4300), 100);
        assert_eq!(percent(4190), 100);
        assert_eq!(percent(3800), 60);
        assert_eq!(percent(3100), 0);
        assert_eq!(percent(2800), 0);
    }

    #[test]
    fn monotonic() {
        let mut last = 0;
        for mv in 3000..4300 {
            let p = percent(mv);
            assert!(p >= last, "{mv}");
            last = p;
        }
    }
}
