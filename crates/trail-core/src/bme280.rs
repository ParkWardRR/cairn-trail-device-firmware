//! Bosch BME280: calibration parsing and the integer compensation routines
//! from the datasheet (BST-BME280-DS002, §4.2.3 and §8.2).

pub const CHIP_ID_REG: u8 = 0xD0;
pub const CHIP_ID: u8 = 0x60;
pub const RESET_REG: u8 = 0xE0;
pub const RESET_CMD: u8 = 0xB6;
pub const CALIB_A_REG: u8 = 0x88; // 26 bytes, 0x88..=0xA1
pub const CALIB_B_REG: u8 = 0xE1; // 7 bytes, 0xE1..=0xE7
pub const CTRL_HUM: u8 = 0xF2;
pub const STATUS: u8 = 0xF3;
pub const CTRL_MEAS: u8 = 0xF4;
pub const CONFIG: u8 = 0xF5;
pub const DATA_REG: u8 = 0xF7; // 8 bytes: press[3], temp[3], hum[2]

/// Humidity oversampling x1.
pub const CTRL_HUM_VALUE: u8 = 0x01;
/// Temperature x2, pressure x16, normal mode — Bosch's "indoor navigation"
/// recommendation, which suits a walking altimeter.
pub const CTRL_MEAS_VALUE: u8 = (0b010 << 5) | (0b101 << 2) | 0b11;
/// Standby 0.5 ms, IIR filter coefficient 16.
pub const CONFIG_VALUE: u8 = 0b100 << 2; // t_sb = 0b000 in bits 7:5

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Calib {
    pub t1: u16,
    pub t2: i16,
    pub t3: i16,
    pub p1: u16,
    pub p2: i16,
    pub p3: i16,
    pub p4: i16,
    pub p5: i16,
    pub p6: i16,
    pub p7: i16,
    pub p8: i16,
    pub p9: i16,
    pub h1: u8,
    pub h2: i16,
    pub h3: u8,
    pub h4: i16,
    pub h5: i16,
    pub h6: i8,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn i16le(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

impl Calib {
    /// `a` is the 26 bytes from 0x88, `b` the 7 bytes from 0xE1.
    pub fn from_regs(a: &[u8; 26], b: &[u8; 7]) -> Self {
        Self {
            t1: u16le(a, 0),
            t2: i16le(a, 2),
            t3: i16le(a, 4),
            p1: u16le(a, 6),
            p2: i16le(a, 8),
            p3: i16le(a, 10),
            p4: i16le(a, 12),
            p5: i16le(a, 14),
            p6: i16le(a, 16),
            p7: i16le(a, 18),
            p8: i16le(a, 20),
            p9: i16le(a, 22),
            h1: a[25],
            h2: i16le(b, 0),
            h3: b[2],
            h4: ((b[3] as i8 as i16) << 4) | (b[4] & 0x0F) as i16,
            h5: ((b[5] as i8 as i16) << 4) | (b[4] >> 4) as i16,
            h6: b[6] as i8,
        }
    }
}

/// Raw ADC values from the 8 data bytes at 0xF7.
pub fn raw(data: &[u8; 8]) -> (i32, i32, i32) {
    let p = ((data[0] as i32) << 12) | ((data[1] as i32) << 4) | ((data[2] as i32) >> 4);
    let t = ((data[3] as i32) << 12) | ((data[4] as i32) << 4) | ((data[5] as i32) >> 4);
    let h = ((data[6] as i32) << 8) | data[7] as i32;
    (t, p, h)
}

/// A compensated reading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// Hundredths of a degree Celsius.
    pub temp_c100: i32,
    /// Pascals in Q24.8 (divide by 256).
    pub pressure_q8: u32,
    /// %RH in Q22.10 (divide by 1024).
    pub humidity_q10: u32,
}

impl Reading {
    pub fn temp_c(&self) -> f32 {
        self.temp_c100 as f32 / 100.0
    }
    pub fn pressure_pa(&self) -> f32 {
        self.pressure_q8 as f32 / 256.0
    }
    pub fn humidity_pct(&self) -> f32 {
        self.humidity_q10 as f32 / 1024.0
    }
}

impl Calib {
    /// The datasheet's `t_fine` and temperature in 0.01 °C.
    pub fn temperature(&self, adc_t: i32) -> (i32, i32) {
        let var1 = (((adc_t >> 3) - ((self.t1 as i32) << 1)) * self.t2 as i32) >> 11;
        let d = (adc_t >> 4) - self.t1 as i32;
        let var2 = (((d * d) >> 12) * self.t3 as i32) >> 14;
        let t_fine = var1 + var2;
        (t_fine, (t_fine * 5 + 128) >> 8)
    }

    /// Pressure in Pa as Q24.8, the datasheet's 64-bit routine. `None` would
    /// divide by zero (an unprogrammed calibration).
    pub fn pressure(&self, adc_p: i32, t_fine: i32) -> Option<u32> {
        let mut var1: i64 = t_fine as i64 - 128_000;
        let mut var2: i64 = var1 * var1 * self.p6 as i64;
        var2 += (var1 * self.p5 as i64) << 17;
        var2 += (self.p4 as i64) << 35;
        var1 = ((var1 * var1 * self.p3 as i64) >> 8) + ((var1 * self.p2 as i64) << 12);
        var1 = (((1i64 << 47) + var1) * self.p1 as i64) >> 33;
        if var1 == 0 {
            return None;
        }
        let mut p: i64 = 1_048_576 - adc_p as i64;
        p = (((p << 31) - var2) * 3125) / var1;
        let var1 = (self.p9 as i64 * (p >> 13) * (p >> 13)) >> 25;
        let var2 = (self.p8 as i64 * p) >> 19;
        p = ((p + var1 + var2) >> 8) + ((self.p7 as i64) << 4);
        Some(p as u32)
    }

    /// Relative humidity as Q22.10, the datasheet's 32-bit routine.
    pub fn humidity(&self, adc_h: i32, t_fine: i32) -> u32 {
        let mut v: i32 = t_fine - 76_800;
        v = ((((adc_h << 14) - ((self.h4 as i32) << 20) - (self.h5 as i32 * v)) + 16_384) >> 15)
            * (((((((v * self.h6 as i32) >> 10) * (((v * self.h3 as i32) >> 11) + 32_768))
                >> 10)
                + 2_097_152)
                * self.h2 as i32
                + 8_192)
                >> 14);
        v -= ((((v >> 15) * (v >> 15)) >> 7) * self.h1 as i32) >> 4;
        v = v.clamp(0, 419_430_400);
        (v >> 12) as u32
    }

    pub fn compensate(&self, data: &[u8; 8]) -> Option<Reading> {
        let (adc_t, adc_p, adc_h) = raw(data);
        // 0x80000 in temperature or pressure means the channel was skipped.
        if adc_t == 0x80000 || adc_p == 0x80000 {
            return None;
        }
        let (t_fine, temp_c100) = self.temperature(adc_t);
        let pressure_q8 = self.pressure(adc_p, t_fine)?;
        let humidity_q10 = if adc_h == 0x8000 {
            0
        } else {
            self.humidity(adc_h, t_fine)
        };
        Some(Reading {
            temp_c100,
            pressure_q8,
            humidity_q10,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The worked example in Bosch's BMP280 datasheet, whose temperature and
    // pressure routines the BME280 shares.
    fn bosch_example() -> Calib {
        Calib {
            t1: 27504,
            t2: 26435,
            t3: -1000,
            p1: 36477,
            p2: -10685,
            p3: 3024,
            p4: 2855,
            p5: 140,
            p6: -7,
            p7: 15500,
            p8: -14600,
            p9: 6000,
            ..Default::default()
        }
    }

    #[test]
    fn datasheet_temperature() {
        let (_, t) = bosch_example().temperature(519_888);
        assert_eq!(t, 2508);
    }

    #[test]
    fn datasheet_pressure() {
        let c = bosch_example();
        let (t_fine, _) = c.temperature(519_888);
        let p = c.pressure(415_148, t_fine).unwrap();
        // 100653.27 Pa in the datasheet's floating-point example.
        let pa = p as f64 / 256.0;
        assert!((pa - 100_653.27).abs() < 1.0, "{pa}");
    }

    #[test]
    fn raw_unpacks_twenty_bit_fields() {
        let data = [0x65, 0x5A, 0xC0, 0x7E, 0xED, 0x00, 0x6B, 0x21];
        let (t, p, h) = raw(&data);
        assert_eq!(p, 415_148);
        assert_eq!(t, 519_888);
        assert_eq!(h, 0x6B21);
    }

    #[test]
    fn calibration_nibbles_for_h4_h5() {
        let a = [0u8; 26];
        // H4 = 0x12 << 4 | 0x3, H5 = 0x45 << 4 | 0xA
        let b = [0, 0, 0, 0x12, 0xA3, 0x45, 0xF6];
        let c = Calib::from_regs(&a, &b);
        assert_eq!(c.h4, 0x123);
        assert_eq!(c.h5, 0x45A);
        assert_eq!(c.h6, -10);
    }

    #[test]
    fn humidity_stays_in_range() {
        // A real BME280's calibration (typical values) at 25 °C.
        let c = Calib {
            h1: 75,
            h2: 362,
            h3: 0,
            h4: 326,
            h5: 50,
            h6: 30,
            ..bosch_example()
        };
        let (t_fine, _) = c.temperature(519_888);
        for adc_h in [0, 20_000, 30_000, 40_000, 65_535] {
            let h = c.humidity(adc_h, t_fine) as f32 / 1024.0;
            assert!((0.0..=100.0).contains(&h), "{adc_h} -> {h}");
        }
    }

    #[test]
    fn skipped_channel_is_none() {
        let c = bosch_example();
        let data = [0x80, 0x00, 0x00, 0x7E, 0xED, 0x00, 0x80, 0x00];
        assert!(c.compensate(&data).is_none());
    }

    #[test]
    fn mode_constants() {
        assert_eq!(CTRL_MEAS_VALUE, 0x57);
        assert_eq!(CONFIG_VALUE, 0x10);
    }
}
