//! The device's bundle identity and counter, in the `cairn` partition after
//! the provisioning store (`keys.rs`):
//!
//! | Offset | Size | |
//! |---|---|---|
//! | 0x12000 | 8 KiB | provisioning store (`keys.rs`) |
//! | 0x14000 | 4 KiB | identity: Ed25519 signing seed and storage root |
//! | 0x16000 | 2 × 4 KiB | the `device_counter` log (`trail_core::counter`) |
//!
//! The identity is generated on the board from the hardware TRNG the first
//! time a bundle is opened, written once, and never leaves the device except
//! by enrolment (`enrolment/v1`, T3), which escrows the storage root with the
//! server. Until then bundles verify structurally only — which is all T2 asks.

extern crate alloc;

use trail_core::counter;

use crate::keys;

const IDENT_OFFSET: u32 = keys::OFFSET + 0x2000;
const COUNTER_OFFSET: [u32; 2] = [keys::OFFSET + 0x4000, keys::OFFSET + 0x5000];
const MAGIC: &[u8; 4] = b"CRNI";
const VERSION: u8 = 1;
const RECORD: usize = 80;

#[derive(Clone)]
pub struct Identity {
    /// Ed25519 signing key seed (the manifest signer).
    pub seed: [u8; 32],
    /// K_root, the storage root every segment key is derived from (§3.6).
    pub root: [u8; 32],
    pub key_version: u32,
}

fn encode(id: &Identity) -> [u8; RECORD] {
    let mut r = [0u8; RECORD];
    r[0..4].copy_from_slice(MAGIC);
    r[4] = VERSION;
    r[8..40].copy_from_slice(&id.seed);
    r[40..72].copy_from_slice(&id.root);
    r[72..76].copy_from_slice(&id.key_version.to_le_bytes());
    let c = counter::crc32(&r[..76]);
    r[76..80].copy_from_slice(&c.to_le_bytes());
    r
}

fn decode(r: &[u8; RECORD]) -> Option<Identity> {
    if &r[0..4] != MAGIC || r[4] != VERSION {
        return None;
    }
    if counter::crc32(&r[..76]) != u32::from_le_bytes([r[76], r[77], r[78], r[79]]) {
        return None;
    }
    Some(Identity {
        seed: r[8..40].try_into().ok()?,
        root: r[40..72].try_into().ok()?,
        key_version: u32::from_le_bytes([r[72], r[73], r[74], r[75]]),
    })
}

fn read_identity() -> Option<Identity> {
    let mut r = [0u8; RECORD];
    keys::with_flash(|f| f.read(IDENT_OFFSET, &mut r).ok())??;
    decode(&r)
}

/// The identity, generated and written on first use.
pub fn load_or_create() -> Result<Identity, &'static str> {
    if let Some(id) = read_identity() {
        return Ok(id);
    }
    let rng = esp_hal::rng::Trng::try_new().map_err(|_| "no TRNG")?;
    let mut id = Identity {
        seed: [0; 32],
        root: [0; 32],
        key_version: 1,
    };
    rng.read(&mut id.seed);
    rng.read(&mut id.root);
    let rec = encode(&id);
    keys::with_flash(|f| {
        f.erase(IDENT_OFFSET, IDENT_OFFSET + 4096)
            .map_err(|_| "identity erase failed")?;
        f.write_nor(IDENT_OFFSET, &rec)
            .map_err(|_| "identity write failed")
    })
    .ok_or("no flash")??;
    let back = read_identity().ok_or("identity read-back failed")?;
    crate::shared::event(
        "identity",
        format_args!(
            "bundle identity generated on the board (key version {})",
            back.key_version
        ),
    );
    Ok(back)
}

/// The current counter, without issuing one.
pub fn counter_now() -> Option<u64> {
    let mut a = alloc::vec![0u8; counter::SECTOR];
    let mut b = alloc::vec![0u8; counter::SECTOR];
    keys::with_flash(|f| {
        f.read(COUNTER_OFFSET[0], &mut a).ok()?;
        f.read(COUNTER_OFFSET[1], &mut b).ok()
    })??;
    Some(counter::state(&a, &b).current)
}

/// Issue the next `device_counter`: written to flash before it is returned,
/// so it is never issued twice (a power cut can only skip one).
pub fn next_counter() -> Result<u64, &'static str> {
    let mut a = alloc::vec![0u8; counter::SECTOR];
    let mut b = alloc::vec![0u8; counter::SECTOR];
    keys::with_flash(|f| -> Result<u64, &'static str> {
        f.read(COUNTER_OFFSET[0], &mut a)
            .map_err(|_| "counter read")?;
        f.read(COUNTER_OFFSET[1], &mut b)
            .map_err(|_| "counter read")?;
        let st = counter::state(&a, &b);
        let next = st.current + 1;
        let base = COUNTER_OFFSET[st.next_sector];
        if st.erase_first {
            f.erase(base, base + counter::SECTOR as u32)
                .map_err(|_| "counter erase")?;
        }
        let at = base + (st.next_slot * counter::SLOT) as u32;
        // write_nor: a raw program into erased flash. FlashStorage::write is a
        // read-modify-erase-write of the whole sector, which would put the
        // current value at risk on every increment.
        f.write_nor(at, &counter::encode(next))
            .map_err(|_| "counter write")?;
        let mut back = [0u8; counter::SLOT];
        f.read(at, &mut back).map_err(|_| "counter read-back")?;
        (counter::decode(&back) == Some(next))
            .then_some(next)
            .ok_or("counter read-back mismatch")
    })
    .ok_or("no flash")?
}
