//! `format/v3` conformance vectors checked on the board (console: `vectors`).
//!
//! The host runs all 62 vectors through the emulator's conformance runner at
//! the `cairn-format` commit this firmware pins. What only the board can show
//! is that the same code gives the same bytes here: HKDF, XChaCha20-Poly1305,
//! Ed25519 and SHA-256 compiled for Xtensa. Three vectors cover all of them;
//! the expectations below are copied from each vector's `expected.json`
//! (`firmware/vectors/`, from contracts-v0.9.0).

extern crate alloc;

use alloc::string::String;
use cairn_format::{
    KeyProvider, Manifest, RootKeyProvider, ScanState, derive_segment_key, hex,
    manifest::verify_manifest, scan_segment, unhex_array,
};
use sha2::{Digest, Sha256};

/// The vectors' storage root, version 1 — a PUBLIC TEST KEY, protecting nothing.
const TEST_ROOT_HEX: &str = "7b1a2c5cc080bc43df4922975a48987d92e3209ba6a4bb8853a23651ad9b3821";

const GNSS_GAP: &[u8] = include_bytes!("../vectors/gnss-gap/segment.bin");
const AUTH_TAMPERED: &[u8] = include_bytes!("../vectors/auth-tag-tampered/segment.bin");
const MANIFEST: &[u8] = include_bytes!("../vectors/manifest-members-match/manifest.cbor");
const MANIFEST_SIG: &[u8; 64] = include_bytes!("../vectors/manifest-members-match/manifest.sig");

const SEGMENT_KEY_HEX: &str = "24f7191a4156d6ac88a626e1922ecf5ce32157734499a3055b9d8b4ebfcae979";

/// One segment vector's expectation, structural and keyed.
struct SegmentVector {
    name: &'static str,
    bytes: &'static [u8],
    scan_stop: &'static str,
    scan_frames: usize,
    scan_offset: usize,
    /// (record type name, count) for the structural scan.
    counts: &'static [(&'static str, usize)],
    keyed_stop: &'static str,
    keyed_frames: usize,
    keyed_offset: usize,
    plaintext_sha256: &'static str,
}

const SEGMENTS: [SegmentVector; 2] = [
    SegmentVector {
        name: "gnss-gap",
        bytes: GNSS_GAP,
        scan_stop: "EOF",
        scan_frames: 3,
        scan_offset: 408,
        counts: &[("GNSS_GAP", 1), ("GNSS_SAMPLE", 2)],
        keyed_stop: "EOF",
        keyed_frames: 3,
        keyed_offset: 408,
        plaintext_sha256: "002771b2355c3831a592b423310e873424d6dbe2708f99cdb48ac93ca3e80376",
    },
    SegmentVector {
        name: "auth-tag-tampered",
        bytes: AUTH_TAMPERED,
        scan_stop: "EOF",
        scan_frames: 4,
        scan_offset: 528,
        counts: &[("GNSS_SAMPLE", 4)],
        keyed_stop: "AUTH_FAILED",
        keyed_frames: 3,
        keyed_offset: 428,
        plaintext_sha256: "177a6c983755b275695cfeb41aa2ec858d91d91273c6892687276edc12834ccd",
    },
];

const MANIFEST_KEY_HEX: &str = "f876e06990f013620b0c41bb19cb6acb367d615cb52c3d8980a57582925a4e13";
const MANIFEST_ROOT_HEX: &str = "1da40231cc5be5532621a4997057f9d5846a109fa312de46c6f3b57420a803fd";
const MANIFEST_CANONICAL_SHA256: &str =
    "c8b5565f15b7e799dea0289d29d8371c452081b4b979e539c6b84aedfa26801a";

fn check_segment(v: &SegmentVector, keys: &RootKeyProvider) -> Result<(), String> {
    use core::fmt::Write;
    let mut why = String::new();
    let s = scan_segment(v.bytes, ScanState::default(), None)
        .map_err(|e| alloc::format!("structural scan: {e}"))?;
    if s.stop.name() != v.scan_stop
        || s.frames.len() != v.scan_frames
        || s.stop_offset != v.scan_offset
    {
        let _ = write!(
            why,
            "structural {} {} frames @{}, want {} {} @{}",
            s.stop.name(),
            s.frames.len(),
            s.stop_offset,
            v.scan_stop,
            v.scan_frames,
            v.scan_offset
        );
        return Err(why);
    }
    for (name, want) in v.counts {
        let got = s
            .frames
            .iter()
            .filter(|f| f.record_type.name() == *name)
            .count();
        if got != *want {
            return Err(alloc::format!("{name} count {got}, want {want}"));
        }
    }
    let k = scan_segment(
        v.bytes,
        ScanState::default(),
        Some(keys as &dyn KeyProvider),
    )
    .map_err(|e| alloc::format!("keyed scan: {e}"))?;
    let seg_key = hex(&derive_segment_key(&keys.root, &k.header));
    if seg_key != SEGMENT_KEY_HEX {
        return Err(alloc::format!("segment key {seg_key}"));
    }
    if k.stop.name() != v.keyed_stop
        || k.frames.len() != v.keyed_frames
        || k.stop_offset != v.keyed_offset
    {
        return Err(alloc::format!(
            "keyed {} {} frames @{}, want {} {} @{}",
            k.stop.name(),
            k.frames.len(),
            k.stop_offset,
            v.keyed_stop,
            v.keyed_frames,
            v.keyed_offset
        ));
    }
    let mut h = Sha256::new();
    for f in &k.frames {
        let pt = f
            .payload
            .as_ref()
            .ok_or("a keyed frame without plaintext")?;
        h.update((pt.len() as u32).to_le_bytes());
        h.update(pt);
    }
    let digest = hex(&h.finalize());
    if digest != v.plaintext_sha256 {
        return Err(alloc::format!("plaintext digest {digest}"));
    }
    Ok(())
}

fn check_manifest() -> Result<(), String> {
    let key_bytes: [u8; 32] = unhex_array(MANIFEST_KEY_HEX).ok_or("key hex")?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| String::from("device public key does not decode"))?;
    let m: Manifest = verify_manifest(MANIFEST, MANIFEST_SIG, &key)
        .map_err(|e| alloc::format!("signature/decode: {e}"))?;
    let root: [u8; 32] = unhex_array(MANIFEST_ROOT_HEX).ok_or("root hex")?;
    if m.content_root != root {
        return Err(alloc::format!("content root {}", hex(&m.content_root)));
    }
    m.verify_content_root()
        .map_err(|e| alloc::format!("verify_content_root: {e}"))?;
    let re = m.to_cbor().map_err(|e| alloc::format!("to_cbor: {e}"))?;
    let digest = hex(&Sha256::digest(&re));
    if digest != MANIFEST_CANONICAL_SHA256 || re != MANIFEST {
        return Err(alloc::format!("re-encode not byte-identical ({digest})"));
    }
    // And a flipped bit in the signed bytes must be refused.
    let mut bad = alloc::vec::Vec::from(MANIFEST);
    bad[10] ^= 0x01;
    if verify_manifest(&bad, MANIFEST_SIG, &key).is_ok() {
        return Err("a tampered manifest verified".into());
    }
    Ok(())
}

/// Run every check; print one line each and a total. Returns whether all passed.
pub fn run() -> bool {
    let started = embassy_time::Instant::now();
    let root: [u8; 32] = unhex_array(TEST_ROOT_HEX).unwrap_or([0; 32]);
    let keys = RootKeyProvider::new(root, 1);
    let mut passed = 0;
    let mut total = 0;
    for v in &SEGMENTS {
        total += 1;
        match check_segment(v, &keys) {
            Ok(()) => {
                passed += 1;
                esp_println::println!("vectors: {} ok", v.name);
            }
            Err(e) => esp_println::println!("vectors: {} FAIL {}", v.name, e),
        }
    }
    total += 1;
    match check_manifest() {
        Ok(()) => {
            passed += 1;
            esp_println::println!("vectors: manifest-members-match ok");
        }
        Err(e) => esp_println::println!("vectors: manifest-members-match FAIL {}", e),
    }
    esp_println::println!(
        "vectors: {}/{} pass on the ESP32-S3 in {} ms (cairn-format, format/v3)",
        passed,
        total,
        started.elapsed().as_millis()
    );
    passed == total
}
