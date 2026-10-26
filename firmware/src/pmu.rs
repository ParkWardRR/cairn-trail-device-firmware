//! X-Powers AXP2101 power management unit, on I²C bus 1 at 0x34.
//!
//! Register facts are from X-Powers' MIT-licensed XPowersLib
//! (`src/REG/AXP2101Constants.h`, `src/XPowersAXP2101.tpp`) by Lewis He; see
//! docs/hardware.md. The rail map is from LilyGO's schematics.

use embedded_hal::i2c::I2c;

pub const ADDR: u8 = 0x34;

const STATUS1: u8 = 0x00;
const STATUS2: u8 = 0x01;
const IC_TYPE: u8 = 0x03;
const IRQ_OFF_ON_LEVEL: u8 = 0x27;
const ADC_CHANNEL_CTRL: u8 = 0x30;
const ADC_VBAT_H: u8 = 0x34;
const ADC_VBUS_H: u8 = 0x38;
const ADC_VSYS_H: u8 = 0x3A;
const ADC_TEMP_H: u8 = 0x3C;
const INTEN1: u8 = 0x40;
const INTSTS1: u8 = 0x48;
const INTSTS2: u8 = 0x49;
const ICC_CHG_SET: u8 = 0x62;
const CV_CHG_VOL_SET: u8 = 0x64;
const BAT_DET_CTRL: u8 = 0x68;
const DC_ONOFF: u8 = 0x80;
const LDO_ONOFF0: u8 = 0x90;
const LDO_VOL_BASE: u8 = 0x92; // ALDO1..4, BLDO1..2 at 0x92..=0x97
const BAT_PERCENT: u8 = 0xA4;

pub const IC_TYPE_AXP2101: u8 = 0x4A;

/// LDO enable bits in register 0x90, named by what they feed on this board.
pub mod ldo {
    /// ALDO1: OLED, BME280, magnetometer and I²C bus 0's pull-ups.
    pub const ALDO1_SENSORS: u8 = 1 << 0;
    /// ALDO2: expansion header; vendor code keeps it on.
    pub const ALDO2: u8 = 1 << 1;
    /// ALDO3: the LoRa module and nothing else. Never enabled by Cairn.
    pub const ALDO3_LORA: u8 = 1 << 2;
    /// ALDO4: GNSS receiver.
    pub const ALDO4_GNSS: u8 = 1 << 3;
    /// BLDO1: microSD.
    pub const BLDO1_SD: u8 = 1 << 4;
}

const IRQ_PWR_SHORT: u8 = 1 << 3;
const IRQ_PWR_LONG: u8 = 1 << 2;

pub struct Pmu<I> {
    i2c: I,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Telemetry {
    pub vbat_mv: u16,
    pub vbus_mv: u16,
    pub vsys_mv: u16,
    pub battery: bool,
    pub vbus_good: bool,
    pub charging: bool,
    pub soc_gauge_pct: u8,
    pub die_temp_c: f32,
    pub dcdc: u8,
    pub ldo: u8,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Buttons {
    pub short: bool,
    pub long: bool,
}

impl<I: I2c> Pmu<I> {
    pub fn new(i2c: I) -> Self {
        Self { i2c }
    }

    fn read(&mut self, reg: u8) -> Result<u8, I::Error> {
        let mut b = [0u8];
        self.i2c.write_read(ADDR, &[reg], &mut b)?;
        Ok(b[0])
    }

    fn write(&mut self, reg: u8, val: u8) -> Result<(), I::Error> {
        self.i2c.write(ADDR, &[reg, val])
    }

    fn update(&mut self, reg: u8, mask: u8, val: u8) -> Result<(), I::Error> {
        let v = self.read(reg)?;
        self.write(reg, (v & !mask) | (val & mask))
    }

    fn read_14(&mut self, reg_h: u8, mask: u8) -> Result<u16, I::Error> {
        let mut b = [0u8; 2];
        self.i2c.write_read(ADDR, &[reg_h], &mut b)?;
        Ok((((b[0] & mask) as u16) << 8) | b[1] as u16)
    }

    pub fn ic_type(&mut self) -> Result<u8, I::Error> {
        self.read(IC_TYPE)
    }

    /// Rail enables as found: (DCDC bits, LDO bits).
    pub fn rails(&mut self) -> Result<(u8, u8), I::Error> {
        Ok((self.read(DC_ONOFF)?, self.read(LDO_ONOFF0)?))
    }

    /// LDO voltage settings in mV: ALDO1..4, BLDO1..2.
    pub fn ldo_voltages(&mut self) -> Result<[u16; 6], I::Error> {
        let mut v = [0u16; 6];
        for (i, out) in v.iter_mut().enumerate() {
            *out = 500 + 100 * (self.read(LDO_VOL_BASE + i as u8)? & 0x1F) as u16;
        }
        Ok(v)
    }

    pub fn power_key_timing(&mut self) -> Result<u8, I::Error> {
        self.read(IRQ_OFF_ON_LEVEL)
    }

    /// Bring the rails to what the capture firmware needs. The DCDC rails are
    /// left exactly as found: DCDC1 is the CPU, and DCDC3–5 feed only the
    /// expansion socket.
    ///
    /// `cold` power-cycles the sensor and SD rails for 250 ms first, so a
    /// wedged card or magnetometer lets go of its bus.
    pub fn configure(
        &mut self,
        cold: bool,
        delay: &mut impl embedded_hal::delay::DelayNs,
    ) -> Result<(), I::Error> {
        const MV_3300: u8 = ((3300u16 - 500) / 100) as u8; // 0x1C
        // The radio rail goes first, and stays off.
        self.update(LDO_ONOFF0, ldo::ALDO3_LORA, 0)?;

        if cold {
            self.update(LDO_ONOFF0, ldo::ALDO1_SENSORS | ldo::BLDO1_SD, 0)?;
            delay.delay_ms(250);
        }
        // ALDO1, ALDO2, ALDO4, BLDO1 at 3.3 V.
        for idx in [0u8, 1, 3, 4] {
            self.update(LDO_VOL_BASE + idx, 0x1F, MV_3300)?;
        }
        let on = ldo::ALDO1_SENSORS | ldo::ALDO2 | ldo::ALDO4_GNSS | ldo::BLDO1_SD;
        self.update(LDO_ONOFF0, on | ldo::ALDO3_LORA, on)?;

        // ADCs: VBAT, VBUS, VSYS, die temperature on; TS off — there is no
        // thermistor, and TS measurement left on upsets charging.
        self.write(ADC_CHANNEL_CTRL, 0b0001_1101)?;
        // Battery detection on.
        self.update(BAT_DET_CTRL, 0x01, 0x01)?;
        // Charge 500 mA to 4.2 V.
        self.update(ICC_CHG_SET, 0x1F, 11)?;
        self.update(CV_CHG_VOL_SET, 0x07, 3)?;

        // Interrupts: disable all, clear all, then enable only the PWR key's
        // short and long press, read by polling (the IRQ line is not used as
        // a wake source: it misfires with no battery or a full one).
        for r in 0..3 {
            self.write(INTEN1 + r, 0)?;
            self.write(INTSTS1 + r, 0xFF)?;
        }
        self.write(INTEN1 + 1, IRQ_PWR_SHORT | IRQ_PWR_LONG)?;
        Ok(())
    }

    /// Whether the radio rail is off, as the register says.
    pub fn lora_rail_off(&mut self) -> Result<bool, I::Error> {
        Ok(self.read(LDO_ONOFF0)? & ldo::ALDO3_LORA == 0)
    }

    pub fn telemetry(&mut self) -> Result<Telemetry, I::Error> {
        let s1 = self.read(STATUS1)?;
        let s2 = self.read(STATUS2)?;
        let battery = s1 & (1 << 3) != 0;
        let vbus_good = s1 & (1 << 5) != 0;
        let raw_t = self.read_14(ADC_TEMP_H, 0x3F)?;
        let (dcdc, ldo) = self.rails()?;
        Ok(Telemetry {
            vbat_mv: if battery {
                self.read_14(ADC_VBAT_H, 0x1F)?
            } else {
                0
            },
            vbus_mv: if vbus_good {
                self.read_14(ADC_VBUS_H, 0x3F)?
            } else {
                0
            },
            vsys_mv: self.read_14(ADC_VSYS_H, 0x3F)?,
            battery,
            vbus_good,
            charging: (s2 >> 5) == 0x01,
            soc_gauge_pct: if battery { self.read(BAT_PERCENT)? } else { 0 },
            die_temp_c: 22.0 + (7274.0 - raw_t as f32) / 20.0,
            dcdc,
            ldo,
        })
    }

    /// PWR key presses since the last call; clears what it reports.
    pub fn buttons(&mut self) -> Result<Buttons, I::Error> {
        let s = self.read(INTSTS2)?;
        let b = Buttons {
            short: s & IRQ_PWR_SHORT != 0,
            long: s & IRQ_PWR_LONG != 0,
        };
        if s & (IRQ_PWR_SHORT | IRQ_PWR_LONG) != 0 {
            self.write(INTSTS2, s & (IRQ_PWR_SHORT | IRQ_PWR_LONG))?;
        }
        Ok(b)
    }

    pub fn i2c(&mut self) -> &mut I {
        &mut self.i2c
    }
}
