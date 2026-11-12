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

/// What `WHO_AM_I` reads once the part's SPI interface has latched: `0x3E`,
/// with `0x0A`/`0x0B` answering `0xDC` and `0x12`/`0x13` answering `0x70`
/// beside it — the same bytes in both SPI modes and from 100 kHz to 4 MHz.
///
/// Bring-up made this state on purpose on 2026-10-25 and found what causes it:
/// writing `CTRL1` whole with bit 5 clear (`0x40`, address auto-increment
/// alone). That is why [`Qmi8658::init`] only ever read-modify-writes that
/// register. Another firmware for this same board read the identical signature
/// on 2026-11-11 and spent a week on the SPI side before a cold power cycle
/// cleared it (docs/prior-art.md §10).
///
/// **No write clears it.** The part is on VDD3V3 (V3.1 carrier, UX1 pins 5 and
/// 8) — a rail no software here can drop, and one that stays up from whichever
/// of USB and the 18650 is still connected. So an ESP32 reset, a PMU rail
/// cycle, and unplugging one of the two supplies all do nothing. Only a cold
/// power cycle does: **USB out *and* the cell out**, together, for minutes.
pub const WHO_AM_I_LATCHED: u8 = 0x3E;

/// `CTRL1` bit 5, set at power-up: clearing it latches the interface, so every
/// write to `CTRL1` keeps it ([`WHO_AM_I_LATCHED`]).
const CTRL1_BE: u8 = 0x20;
/// `CTRL1` bit 6: address auto-increment, what a burst read needs.
const CTRL1_ADDR_AI: u8 = 0x40;
/// `CTRL1` bit 3: drive interrupts on INT1 (SensorLib's `enableINT`).
const CTRL1_INT1_EN: u8 = 0x08;
/// `CTRL7` bits 0 and 1: accelerometer and gyroscope enable. Data ready
/// reaches INT1 unless `CTRL7` bit 5 is set, which this firmware never sets
/// (SensorLib's `enableDataReadyINT`).
const CTRL7_ENABLE: u8 = 0x03;

/// What a `WHO_AM_I` byte means and what to do about it, so the answer travels
/// with the symptom instead of living in a document.
pub fn whoami_hint(id: u8) -> &'static str {
    match id {
        WHO_AM_I_VALUE => "QMI8658",
        WHO_AM_I_LATCHED => {
            "SPI interface LATCHED. No software clears this: cold power cycle, \
             USB out AND the cell out, minutes"
        }
        0xFF => "nothing answered: CS 34 is not reaching the part, or the bus idles high",
        0x00 => "MISO held low: another device selected on the shared bus, or no power",
        _ => "unknown reply: check CS 34, and that the card is deselected",
    }
}

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

impl Sample {
    /// |a| in mg: about 1000 on a healthy part at rest. The cheapest check
    /// that a reading is real rather than frozen, because a stuck or
    /// fabricated value does not land on gravity. `trail verify --stationary`
    /// applies the same test to a whole session after the fact; this is the
    /// one the board can report while it runs.
    pub fn magnitude_mg(&self) -> u16 {
        let sq: f32 = self
            .accel_mg
            .iter()
            .map(|&v| {
                let f = v as f32;
                f * f
            })
            .sum();
        libm::sqrtf(sq) as u16
    }
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
        // locked the SPI interface, and only a cold power cycle — USB out and
        // the cell out — brings it back. See `WHO_AM_I_LATCHED`.
        report("ctrl1_default", ctrl1[0], id);

        let steps: [(&str, u8, u8, u8); 5] = [
            ("ctrl1_addr_ai", CTRL1, CTRL1_ADDR_AI, CTRL1_ADDR_AI),
            ("ctrl2_accel", CTRL2, 0x7F, (ACC_RANGE << 4) | ODR_62HZ),
            ("ctrl3_gyro", CTRL3, 0x7F, (GYR_RANGE << 4) | ODR_62HZ),
            ("ctrl5_lpf", CTRL5, 0x77, 0x11),
            ("ctrl7_enable", CTRL7, CTRL7_ENABLE, CTRL7_ENABLE),
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

    /// Turn the data-ready interrupt on INT1 on or off, read-modify-writing
    /// only `CTRL1` bit 3. For the alive test on a part that answered: it
    /// should make INT 33 move, which is how the test earns the right to be
    /// believed when the part has *not* answered.
    pub fn int1_enable(&mut self, on: bool) -> Result<u8, S::Error> {
        self.update(CTRL1, CTRL1_INT1_EN, if on { CTRL1_INT1_EN } else { 0 })
    }

    /// Arm data-ready on INT1 without reading anything back.
    ///
    /// For a part whose `WHO_AM_I` is wrong, where every read is untrustworthy
    /// but a moving INT 33 would still prove it is powered, alive and taking
    /// writes — the one measurement left that does not depend on the read
    /// path. `CTRL1` is written whole here because a read-modify-write of a
    /// register that cannot be read is worse than useless, and it keeps bit 5
    /// set ([`WHO_AM_I_LATCHED`]). Every register it touches is volatile, and
    /// the cold power cycle that clears the state this diagnoses clears these
    /// too.
    pub fn int1_arm_blind(&mut self) -> Result<(), S::Error> {
        self.write(CTRL1, CTRL1_BE | CTRL1_ADDR_AI | CTRL1_INT1_EN)?;
        self.write(CTRL2, (ACC_RANGE << 4) | ODR_62HZ)?;
        self.write(CTRL7, CTRL7_ENABLE)
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
