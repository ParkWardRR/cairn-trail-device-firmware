//! The device's identity and network settings, in their own flash partition
//! (`cairn`, 0x12000, see `partitions.csv`) rather than on the removable card:
//! the TLS client certificate and key, the Cairn CA, the device id, Wi-Fi and
//! the server address. Written once over USB by `scripts/provision.py`.
//!
//! Layout: `CRNK` | version u8 | 3 reserved | payload length u32 LE | payload |
//! CRC-32 of the payload. The payload is a run of entries: tag u8, length u16
//! LE, value. A blank (erased) or damaged partition reads as "not provisioned".

extern crate alloc;

use alloc::vec::Vec;
use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use esp_storage::FlashStorage;

pub const OFFSET: u32 = 0x12000;
/// The first 8 KiB of the 24 KiB `cairn` partition (a provisioned store is
/// ~1.2 KiB); the rest holds the bundle identity and counter (`ident.rs`).
pub const SIZE: usize = 0x2000;
const MAGIC: &[u8; 4] = b"CRNK";
const VERSION: u8 = 1;
const HEADER: usize = 12;

/// Entry tags. Values are bytes as stored; text is UTF-8.
pub mod tag {
    pub const DEVICE_ID: u8 = 1; // 16 bytes
    pub const CLIENT_CERT: u8 = 2; // DER
    pub const CLIENT_KEY: u8 = 3; // SEC1 DER, P-256
    pub const CA_CERT: u8 = 4; // DER
    pub const WIFI_SSID: u8 = 5;
    pub const WIFI_PASSWORD: u8 = 6;
    pub const WIFI_BSSID: u8 = 7; // 6 bytes, optional: pin one access point
    pub const WIFI_CHANNEL: u8 = 8; // 1 byte, optional
    pub const SERVER_HOST: u8 = 9; // TLS server name, e.g. cairn.alpina.casa
    pub const SERVER_IP: u8 = 10; // 4 bytes, optional: skip DNS
    pub const SERVER_PORT: u8 = 11; // u16 LE
    pub const RECEIPT_KEY: u8 = 12; // the server's receipt-signing public key
}

static FLASH: Mutex<CriticalSectionRawMutex, RefCell<Option<FlashStorage<'static>>>> =
    Mutex::new(RefCell::new(None));
static PAYLOAD: Mutex<CriticalSectionRawMutex, RefCell<Option<Vec<u8>>>> =
    Mutex::new(RefCell::new(None));

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

/// The flash, for the other records in the `cairn` partition (`ident.rs`).
/// None until `init`.
pub fn with_flash<R>(f: impl FnOnce(&mut FlashStorage<'static>) -> R) -> Option<R> {
    FLASH.lock(|fl| fl.borrow_mut().as_mut().map(f))
}

/// Take the flash and load the store. Returns whether a valid store was found.
pub fn init(flash: FlashStorage<'static>) -> bool {
    FLASH.lock(|f| f.replace(Some(flash)));
    reload()
}

fn reload() -> bool {
    let mut header = [0u8; HEADER];
    let ok = FLASH.lock(|f| {
        let mut f = f.borrow_mut();
        let flash = f.as_mut()?;
        flash.read(OFFSET, &mut header).ok()?;
        if &header[0..4] != MAGIC || header[4] != VERSION {
            return None;
        }
        let len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        if len == 0 || len + HEADER + 4 > SIZE {
            return None;
        }
        let mut body = alloc::vec![0u8; len + 4];
        flash.read(OFFSET + HEADER as u32, &mut body).ok()?;
        let crc = u32::from_le_bytes([body[len], body[len + 1], body[len + 2], body[len + 3]]);
        body.truncate(len);
        (crc32(&body) == crc).then_some(body)
    });
    let found = ok.is_some();
    PAYLOAD.lock(|p| p.replace(ok));
    found
}

/// Write a new payload (entries only) and reload it.
pub fn write(payload: &[u8]) -> Result<(), &'static str> {
    if payload.is_empty() || payload.len() + HEADER + 4 > SIZE {
        return Err("payload size");
    }
    parse(payload).ok_or("malformed entries")?;
    let mut image = Vec::with_capacity(payload.len() + HEADER + 4);
    image.extend_from_slice(MAGIC);
    image.extend_from_slice(&[VERSION, 0, 0, 0]);
    image.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    image.extend_from_slice(payload);
    image.extend_from_slice(&crc32(payload).to_le_bytes());
    FLASH.lock(|f| {
        let mut f = f.borrow_mut();
        let flash = f.as_mut().ok_or("no flash")?;
        flash
            .write(OFFSET, &image)
            .map_err(|_| "flash write failed")
    })?;
    if reload() {
        Ok(())
    } else {
        Err("read-back failed")
    }
}

/// Wipe the store (the header is enough).
pub fn erase() -> Result<(), &'static str> {
    FLASH.lock(|f| {
        let mut f = f.borrow_mut();
        let flash = f.as_mut().ok_or("no flash")?;
        flash
            .write(OFFSET, &[0xFF; HEADER])
            .map_err(|_| "flash write failed")
    })?;
    reload();
    Ok(())
}

/// Every entry of a payload, or None if it does not parse exactly.
fn parse(payload: &[u8]) -> Option<Vec<(u8, core::ops::Range<usize>)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        if i + 3 > payload.len() {
            return None;
        }
        let t = payload[i];
        let len = u16::from_le_bytes([payload[i + 1], payload[i + 2]]) as usize;
        let start = i + 3;
        if start + len > payload.len() {
            return None;
        }
        out.push((t, start..start + len));
        i = start + len;
    }
    Some(out)
}

/// A copy of one entry's value.
pub fn get(t: u8) -> Option<Vec<u8>> {
    PAYLOAD.lock(|p| {
        let p = p.borrow();
        let payload = p.as_ref()?;
        let entries = parse(payload)?;
        entries
            .into_iter()
            .find(|(tt, _)| *tt == t)
            .map(|(_, r)| payload[r].to_vec())
    })
}

pub fn get_str(t: u8) -> Option<alloc::string::String> {
    get(t).and_then(|v| alloc::string::String::from_utf8(v).ok())
}

pub fn provisioned() -> bool {
    PAYLOAD.lock(|p| p.borrow().is_some())
}

/// A one-line summary that names what is present without printing secrets.
pub fn summary() -> alloc::string::String {
    use core::fmt::Write;
    let mut s = alloc::string::String::new();
    if !provisioned() {
        return "not provisioned".into();
    }
    if let Some(id) = get(tag::DEVICE_ID) {
        let _ = write!(s, "device_id=");
        for b in id {
            let _ = write!(s, "{:02x}", b);
        }
    }
    let have = |t: u8| get(t).map(|v| v.len()).unwrap_or(0);
    let _ = write!(
        s,
        " cert={}B key={}B ca={}B ssid={} password={} bssid={} host={} ip={:?} port={} receipt_key={}B",
        have(tag::CLIENT_CERT),
        have(tag::CLIENT_KEY),
        have(tag::CA_CERT),
        get_str(tag::WIFI_SSID).unwrap_or_default(),
        if have(tag::WIFI_PASSWORD) > 0 {
            "set"
        } else {
            "none"
        },
        if have(tag::WIFI_BSSID) == 6 {
            "pinned"
        } else {
            "any"
        },
        get_str(tag::SERVER_HOST).unwrap_or_default(),
        get(tag::SERVER_IP),
        get(tag::SERVER_PORT)
            .filter(|v| v.len() == 2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .unwrap_or(0),
        have(tag::RECEIPT_KEY),
    );
    s
}
