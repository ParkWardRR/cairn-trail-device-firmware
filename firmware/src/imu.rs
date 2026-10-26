//! QST QMI8658 6-axis IMU on the shared SPI bus (CS 34), beside the SD card.
//!
//! Register facts are from the MIT-licensed SensorLib (`SensorQMI8658.hpp`)
//! by Lewis He; see docs/hardware.md.

use embedded_hal::spi::{Operation, SpiDevice};

const WHO_AM_I: u8 = 0x00;
const REVISION: u8 = 0x01;
const CTRL1: u8 = 0x02;
const CTRL2: u8 = 0x03;
const CTRL3: u8 = 0x04;
const CTRL5: u8 = 0x06;
const CTRL7: u8 = 0x08;
const TEMP_L: u8 = 0x33;
const AX_L: u8 = 0x35;
const RESET: u8 = 0x60;
const RESET_STATUS: u8 = 0x4D;

pub const WHO_AM_I_VALUE: u8 = 0x05;

/// ±8 g.
const ACC_RANGE: u8 = 2;
/// ±512 °/s.
const GYR_RANGE: u8 = 5;
/// 62.5 Hz accelerometer / 56 Hz gyroscope output data rate.
const ODR_62HZ: u8 = 7;

pub struct Qmi8658<S> {
    spi: S,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub accel_mg: [i16; 3],
    /// Tenths of a degree per second.
    pub gyro_ddps: [i16; 3],
    pub temp_c: f32,
}

impl<S: SpiDevice> Qmi8658<S> {
    pub fn new(spi: S) -> Self {
        Self { spi }
    }

    fn read(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), S::Error> {
        self.spi
            .transaction(&mut [Operation::Write(&[reg | 0x80]), Operation::Read(buf)])
    }

    fn write(&mut self, reg: u8, val: u8) -> Result<(), S::Error> {
        self.spi.write(&[reg & 0x7F, val])
    }

    /// The first 16 registers, for diagnostics.
    #[cfg(feature = "verbose")]
    pub fn dump(&mut self) -> Result<[u8; 16], S::Error> {
        let mut r = [0u8; 16];
        for (i, v) in r.iter_mut().enumerate() {
            let mut b = [0u8];
            self.read(i as u8, &mut b)?;
            *v = b[0];
        }
        Ok(r)
    }

    fn whoami(&mut self) -> Result<u8, S::Error> {
        let mut id = [0u8];
        self.read(WHO_AM_I, &mut id)?;
        Ok(id[0])
    }

    fn update(&mut self, reg: u8, mask: u8, val: u8) -> Result<u8, S::Error> {
        let mut v = [0u8];
        self.read(reg, &mut v)?;
        let n = (v[0] & !mask) | (val & mask);
        self.write(reg, n)?;
        Ok(n)
    }

    /// Reset and configure, following SensorLib's sequence: soft reset, then
    /// only the address-auto-increment bit of CTRL1 is set (read-modify-write,
    /// keeping the chip's other defaults), then each sensor's range, rate,
    /// filter and enable bits. After every write the chip is asked who it is,
    /// and `report` hears each step, so a write that breaks the interface is
    /// named. Returns (WHO_AM_I, revision).
    pub fn init(
        &mut self,
        delay: &mut impl embedded_hal::delay::DelayNs,
        mut report: impl FnMut(&str, u8, u8),
    ) -> Result<(u8, u8), S::Error> {
        let id = self.whoami()?;
        report("before_reset", 0, id);
        self.write(RESET, 0xB0)?;
        let mut rst = 0u8;
        for _ in 0..50 {
            delay.delay_ms(10);
            let mut r = [0u8];
            self.read(RESET_STATUS, &mut r)?;
            rst = r[0];
            if rst == 0x80 {
                break;
            }
        }
        let id = self.whoami()?;
        report("reset_result", rst, id);
        if id != WHO_AM_I_VALUE {
            return Ok((id, 0));
        }
        let mut rev = [0u8];
        self.read(REVISION, &mut rev)?;
        let mut ctrl1 = [0u8];
        self.read(CTRL1, &mut ctrl1)?;
        // 0x20 on this part. Bit 5 must stay set: overwriting CTRL1 with 0x40
        // locked the SPI interface until the board was powered off.
        report("ctrl1_default", ctrl1[0], id);

        let steps: [(&str, u8, u8, u8); 5] = [
            ("ctrl1_addr_ai", CTRL1, 0x40, 0x40),
            ("ctrl2_accel", CTRL2, 0x7F, (ACC_RANGE << 4) | ODR_62HZ),
            ("ctrl3_gyro", CTRL3, 0x7F, (GYR_RANGE << 4) | ODR_62HZ),
            ("ctrl5_lpf", CTRL5, 0x77, 0x11),
            ("ctrl7_enable", CTRL7, 0x03, 0x03),
        ];
        for (name, reg, mask, val) in steps {
            let written = self.update(reg, mask, val)?;
            let id = self.whoami()?;
            report(name, written, id);
            if id != WHO_AM_I_VALUE {
                return Ok((id, rev[0]));
            }
        }
        Ok((WHO_AM_I_VALUE, rev[0]))
    }

    pub fn sample(&mut self) -> Result<Sample, S::Error> {
        let mut d = [0u8; 14];
        self.read(TEMP_L, &mut d[..2])?;
        self.read(AX_L, &mut d[2..])?;
        let w = |i: usize| i16::from_le_bytes([d[i], d[i + 1]]) as i32;
        let acc = |i: usize| (w(i) * 8000 / 32768) as i16;
        let gyr = |i: usize| (w(i) * 5120 / 32768) as i16;
        Ok(Sample {
            temp_c: w(0) as f32 / 256.0,
            accel_mg: [acc(2), acc(4), acc(6)],
            gyro_ddps: [gyr(8), gyr(10), gyr(12)],
        })
    }
}
