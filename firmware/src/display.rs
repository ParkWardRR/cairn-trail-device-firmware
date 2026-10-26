//! SH1106 128×64 OLED on I²C bus 0, drawn through a framebuffer with
//! embedded-graphics. The SH1106 has 132 columns of RAM; the panel shows
//! columns 2..130.

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_hal::i2c::I2c;

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
const COLUMN_OFFSET: u8 = 2;

pub struct Sh1106 {
    addr: u8,
    buf: [u8; WIDTH * HEIGHT / 8],
}

impl Sh1106 {
    pub const fn new(addr: u8) -> Self {
        Self {
            addr,
            buf: [0; WIDTH * HEIGHT / 8],
        }
    }

    fn cmds<I: I2c>(&self, i2c: &mut I, cmds: &[u8]) -> Result<(), I::Error> {
        let mut b = [0u8; 16];
        b[0] = 0x00; // Co = 0, D/C = 0: a run of commands
        b[1..=cmds.len()].copy_from_slice(cmds);
        i2c.write(self.addr, &b[..=cmds.len()])
    }

    pub fn init<I: I2c>(&mut self, i2c: &mut I) -> Result<(), I::Error> {
        self.cmds(i2c, &[0xAE])?; // display off
        self.cmds(i2c, &[0xD5, 0x80])?; // clock divide
        self.cmds(i2c, &[0xA8, 0x3F])?; // multiplex 64
        self.cmds(i2c, &[0xD3, 0x00])?; // display offset
        self.cmds(i2c, &[0x40])?; // start line 0
        self.cmds(i2c, &[0xAD, 0x8B])?; // DC-DC on
        self.cmds(i2c, &[0xA1])?; // segment remap
        self.cmds(i2c, &[0xC8])?; // COM scan descending
        self.cmds(i2c, &[0xDA, 0x12])?; // COM pins
        self.cmds(i2c, &[0x81, 0x80])?; // contrast
        self.cmds(i2c, &[0xD9, 0x22])?; // precharge
        self.cmds(i2c, &[0xDB, 0x35])?; // VCOM deselect
        self.cmds(i2c, &[0xA4, 0xA6])?; // follow RAM, normal
        self.clear();
        self.flush(i2c)?;
        self.cmds(i2c, &[0xAF]) // display on
    }

    pub fn set_on<I: I2c>(&mut self, i2c: &mut I, on: bool) -> Result<(), I::Error> {
        self.cmds(i2c, &[if on { 0xAF } else { 0xAE }])
    }

    pub fn clear(&mut self) {
        self.buf.fill(0);
    }

    pub fn flush<I: I2c>(&mut self, i2c: &mut I) -> Result<(), I::Error> {
        for page in 0..8u8 {
            self.cmds(
                i2c,
                &[
                    0xB0 | page,
                    COLUMN_OFFSET & 0x0F,
                    0x10 | (COLUMN_OFFSET >> 4),
                ],
            )?;
            let mut chunk = [0u8; 1 + WIDTH];
            chunk[0] = 0x40; // data
            let start = page as usize * WIDTH;
            chunk[1..].copy_from_slice(&self.buf[start..start + WIDTH]);
            i2c.write(self.addr, &chunk)?;
        }
        Ok(())
    }
}

impl OriginDimensions for Sh1106 {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Sh1106 {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<P>(&mut self, pixels: P) -> Result<(), Self::Error>
    where
        P: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if p.x < 0 || p.y < 0 || p.x >= WIDTH as i32 || p.y >= HEIGHT as i32 {
                continue;
            }
            let (x, y) = (p.x as usize, p.y as usize);
            let idx = (y / 8) * WIDTH + x;
            let bit = 1 << (y % 8);
            if c.is_on() {
                self.buf[idx] |= bit;
            } else {
                self.buf[idx] &= !bit;
            }
        }
        Ok(())
    }
}
