//! Cairn `format/v3` bundles on the card, beside the session's CSV files
//! (T2). Every frame is sealed by `cairn-format` — the dongle emulator's Rust
//! implementation, pinned by commit — under a key derived from this device's
//! storage root; each bundle is closed by a signed manifest.
//!
//! **On the card** a bundle is a directory in the root named for its counter,
//! `B` + seven hex digits (`B0000001`). The FAT library here writes 8.3 names
//! only and cannot rename, so two things differ from the spec's §2 layout,
//! neither of which reaches the bundle's bytes or its manifest:
//!
//! | Spec (§2) | On this card |
//! |---|---|
//! | `<bundle_id>.open` / `.sealed` directory | `B0000001`; sealed when `MANIFEST.SIG` is present |
//! | `seg-00000000.seg`, `journal.seg` | `S0000000.SEG`, `JOURNAL.SEG` |
//! | `manifest.cbor`, `manifest.sig` | `MANIFEST.CBO`, `MANIFEST.SIG` |
//!
//! The manifest names members by their canonical names; `trail bundle pull`
//! writes them back under those names. Sealing follows §2.1's order:
//! segments flushed, rescanned with the key, journal written, manifest
//! written, signature written last — so a signature on the card means the
//! manifest beside it is whole. A bundle without one is sealed at the next
//! boot, from the bytes on the card alone.
//!
//! **What is written:** `GNSS_SAMPLE` for every epoch with a fix, `GNSS_GAP`
//! for every stretch without one (never a run of empty samples),
//! `IMU_SUMMARY` once a second, `DEVICE_HEALTH` once a minute, and
//! `TIME_OBSERVATION`s — GNSS, and the RTC once it has been checked. The first
//! trustworthy time is adopted as the manifest's `utc_basis_ms`.
//!
//! Until enrolment (T3) the vehicle and assignment ids are zero; the bundles
//! are structurally valid and verify with the device's public key, but a
//! server will not take them into a trip until the device is enrolled and
//! assigned.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use cairn_format::bind::{JOURNAL_MEMBER_NAME, segment_member_name};
use cairn_format::manifest::{MANIFEST_VERSION, SIGNATURE_ALGORITHM_ED25519, device_key_id};
use cairn_format::merkle::sort_members;
use cairn_format::payload::parse_time_observation;
use cairn_format::{
    ChunkDescriptor, ENCRYPTION_SUITE_V1, JOURNAL_SEGMENT_INDEX, KeyProvider, Manifest, Member,
    RecordType, RecoveryState, RootKeyProvider, ScanState, SegmentHeader, SegmentWriter,
    StopReason, content_root, scan_segment,
};
use embedded_sdmmc::{Mode, RawDirectory, RawFile};
use heapless::String as HString;
use sha2::{Digest, Sha256};
use trail_core::records::{
    DeviceHealth, GAP_NO_FIX, GnssGap, ImuWindow, StateTransition, TIME_SOURCE_GNSS,
    TIME_SOURCE_RTC, TimeObservation, UNKNOWN_U16,
};

use crate::ident::Identity;
use crate::shared::{self, GnssEpoch, event};
use crate::storage::Vm;

/// A segment is closed and the next opened past this size, which also bounds
/// the RAM a segment holds while it is written.
const SEGMENT_ROLL: usize = 32 * 1024;
/// A bundle is sealed and the next opened after this long.
pub const BUNDLE_ROLL_MS: u64 = 30 * 60 * 1000;
/// Transfer chunk size (manifest key 16): small, so an upload resumes cheaply.
const CHUNK: usize = 8192;
const HEALTH_EVERY_MS: u64 = 60_000;
const TIME_OBS_EVERY_MS: u64 = 600_000;
const IMU_WINDOW_SAMPLES: u32 = 50;
/// No fix for this long is a gap worth a `GNSS_GAP` record.
const GAP_AFTER_MS: u64 = 2_000;

const JOURNAL_FILE: &str = "JOURNAL.SEG";
const MANIFEST_FILE: &str = "MANIFEST.CBO";
const SIG_FILE: &str = "MANIFEST.SIG";
/// Left in a bundle cut before its first frame reached the card: nothing to
/// seal, and not to be retried at every boot.
const EMPTY_FILE: &str = "EMPTY";
const EMPTY: &str = "empty: no frame reached the card";

/// Journal states (region 2, bundle).
const STATE_OPEN: u8 = 1;
const STATE_SEALED: u8 = 2;
/// Journal triggers: sealed in the normal course, or at boot after a cut.
const TRIGGER_OPENED: u8 = 1;
const TRIGGER_SEALED: u8 = 2;
const TRIGGER_RECOVERED: u8 = 3;

pub fn dir_name(counter: u64) -> HString<12> {
    let mut s = HString::new();
    let _ = write!(s, "B{:07X}", counter & 0x0FFF_FFFF);
    s
}

fn seg_file(index: u32) -> HString<12> {
    let mut s = HString::new();
    let _ = write!(s, "S{:07}.SEG", index);
    s
}

/// Frame nonces from the hardware RNG (24 random bytes a frame, §3.6).
fn nonces() -> Result<Box<dyn rand_core::RngCore>, &'static str> {
    Ok(Box::new(
        esp_hal::rng::Trng::try_new().map_err(|_| "no TRNG")?,
    ))
}

/// The device id: provisioned, or the dongle's rule — SHA-256 of the Wi-Fi
/// MAC, first 16 bytes — which is what provisioning writes anyway.
pub fn device_id() -> [u8; 16] {
    if let Some(id) = crate::keys::get(crate::keys::tag::DEVICE_ID)
        && let Ok(a) = <[u8; 16]>::try_from(id.as_slice())
    {
        return a;
    }
    let d = Sha256::digest(crate::mac_bytes());
    let mut a = [0u8; 16];
    a.copy_from_slice(&d[..16]);
    a
}

pub struct OpenBundle {
    pub counter: u64,
    pub name: HString<12>,
    dir: RawDirectory,
    file: RawFile,
    seg_index: u32,
    writer: SegmentWriter,
    written: usize,
    header: SegmentHeader,
    keys: RootKeyProvider,
    pub opened_ms: u64,
    /// UTC at capture start (ms) and its accuracy, once adopted.
    basis: Option<(u64, u32)>,
    no_fix_since: Option<u64>,
    imu: ImuWindow,
    last_health_ms: u64,
    last_time_obs_ms: Option<u64>,
    pub frames: u32,
}

impl OpenBundle {
    /// Open the next bundle: a fresh counter (written to flash first), its
    /// directory, and the first segment.
    pub fn open(
        vm: &Vm,
        root: RawDirectory,
        id: &Identity,
        boot_id: [u8; 16],
        now_ms: u64,
    ) -> Result<Self, &'static str> {
        let counter = crate::ident::next_counter()?;
        let name = dir_name(counter);
        vm.make_dir_in_dir(root, name.as_str())
            .map_err(|_| "bundle mkdir")?;
        let dir = vm
            .open_dir(root, name.as_str())
            .map_err(|_| "bundle open dir")?;
        let keys = RootKeyProvider::new(id.root, id.key_version);
        let header = SegmentHeader {
            device_id: device_id(),
            boot_id,
            vehicle_id: [0; 16],
            assignment_id: [0; 16],
            segment_index: 0,
            // One monotonic origin for the whole bundle: every segment opens
            // at the bundle's start, so a frame's monotonic_ms is ms since it.
            opened_monotonic_us: now_ms * 1000,
            storage_key_version: id.key_version,
            device_counter: counter,
            ..SegmentHeader::default()
        };
        let writer = SegmentWriter::new(header.clone(), ScanState::default(), &keys, nonces()?)
            .map_err(|_| "segment writer")?;
        let file = vm
            .open_file_in_dir(dir, seg_file(0).as_str(), Mode::ReadWriteCreateOrTruncate)
            .map_err(|_| "segment create")?;
        let mut b = Self {
            counter,
            name,
            dir,
            file,
            seg_index: 0,
            writer,
            written: 0,
            header,
            keys,
            opened_ms: now_ms,
            basis: None,
            no_fix_since: Some(now_ms),
            imu: ImuWindow::default(),
            last_health_ms: now_ms,
            last_time_obs_ms: None,
            frames: 0,
        };
        b.write_new(vm)?;
        // A checked RTC dates the bundle from its first frame.
        b.rtc_time(vm, now_ms)?;
        // The header durable at once, so a cut in the first seconds leaves a
        // bundle that recovery can read rather than an empty file.
        b.flush(vm)?;
        Ok(b)
    }

    fn write_new(&mut self, vm: &Vm) -> Result<(), &'static str> {
        let bytes = &self.writer.bytes()[self.written..];
        if !bytes.is_empty() {
            vm.write(self.file, bytes).map_err(|_| "segment write")?;
            self.written = self.writer.bytes().len();
        }
        Ok(())
    }

    fn append(
        &mut self,
        vm: &Vm,
        rt: RecordType,
        payload: &[u8],
        at_ms: u64,
    ) -> Result<(), &'static str> {
        let mono = at_ms.saturating_sub(self.opened_ms).min(u32::MAX as u64) as u32;
        self.writer
            .append(rt, 1, 0, mono, payload)
            .map_err(|_| "seal frame")?;
        self.frames += 1;
        self.write_new(vm)?;
        if self.writer.bytes().len() >= SEGMENT_ROLL {
            self.roll_segment(vm)?;
        }
        Ok(())
    }

    fn roll_segment(&mut self, vm: &Vm) -> Result<(), &'static str> {
        vm.close_file(self.file).map_err(|_| "segment close")?;
        let state = self.writer.next_state();
        self.seg_index += 1;
        let mut h = self.header.clone();
        h.segment_index = self.seg_index;
        self.writer =
            SegmentWriter::new(h, state, &self.keys, nonces()?).map_err(|_| "segment writer")?;
        self.written = 0;
        self.file = vm
            .open_file_in_dir(
                self.dir,
                seg_file(self.seg_index).as_str(),
                Mode::ReadWriteCreateOrTruncate,
            )
            .map_err(|_| "segment create")?;
        self.write_new(vm)
    }

    /// Record a wall-clock reading; the first one adopted becomes the basis.
    fn observe_time(
        &mut self,
        vm: &Vm,
        utc_ms: u64,
        acc_ms: u32,
        source: u8,
        at_ms: u64,
    ) -> Result<(), &'static str> {
        let adopt = self.basis.is_none();
        if adopt {
            let since_start = at_ms.saturating_sub(self.opened_ms);
            self.basis = Some((utc_ms.saturating_sub(since_start), acc_ms));
        }
        let obs = TimeObservation {
            utc_ms,
            acc_ms: Some(acc_ms),
            source,
            adopted: adopt,
        };
        self.append(vm, RecordType::TIME_OBSERVATION, &obs.encode(), at_ms)?;
        self.last_time_obs_ms = Some(at_ms);
        Ok(())
    }

    /// One IMU sample (accelerometer mg, gyroscope 0.1 °/s); a summary once a
    /// second.
    pub fn imu(
        &mut self,
        vm: &Vm,
        now_ms: u64,
        accel_mg: [i16; 3],
        gyro_dps_e1: [i16; 3],
    ) -> Result<(), &'static str> {
        self.imu.add(now_ms, accel_mg, gyro_dps_e1);
        if self.imu.len() >= IMU_WINDOW_SAMPLES {
            let span = (self.imu.span_ms() + 20).min(u16::MAX as u64) as u16;
            if let Some(s) = self.imu.take(span) {
                self.append(vm, RecordType::IMU_SUMMARY, &s.encode(), now_ms)?;
            }
        }
        Ok(())
    }

    /// One NAV-PVT epoch.
    pub fn gnss(&mut self, vm: &Vm, e: &GnssEpoch) -> Result<(), &'static str> {
        let t_acc_ms = if e.t_acc_ns == u32::MAX {
            u32::MAX
        } else {
            e.t_acc_ns.div_ceil(1_000_000)
        };
        if !e.fix {
            if self.no_fix_since.is_none() {
                self.no_fix_since = Some(e.t_ms);
            }
            return Ok(());
        }
        if let Some(utc) = e.utc_ms {
            let due = self
                .last_time_obs_ms
                .is_none_or(|t| e.t_ms.saturating_sub(t) >= TIME_OBS_EVERY_MS);
            // A GNSS time replaces nothing already adopted; it is evidence
            // beside it, now and then.
            if self.basis.is_none() || due {
                self.observe_time(vm, utc, t_acc_ms, TIME_SOURCE_GNSS, e.t_ms)?;
            }
        }
        if let Some(since) = self.no_fix_since.take() {
            self.gap(vm, since, e.t_ms)?;
        }
        let mut s = e.sample;
        if let (Some(utc), Some((basis, _))) = (e.utc_ms, self.basis) {
            s.utc_offset_ms =
                (utc as i64 - basis as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            s.utc_acc_ms = t_acc_ms.min(UNKNOWN_U16 as u32 - 1) as u16;
        }
        self.append(vm, RecordType::GNSS_SAMPLE, &s.encode(), e.t_ms)
    }

    fn gap(&mut self, vm: &Vm, since_ms: u64, until_ms: u64) -> Result<(), &'static str> {
        let dur = until_ms.saturating_sub(since_ms);
        if dur < GAP_AFTER_MS {
            return Ok(());
        }
        let g = GnssGap {
            duration_ms: dur.min(u32::MAX as u64) as u32,
            expected_samples: (dur / 1000).min(u16::MAX as u64) as u16,
            cause: GAP_NO_FIX,
        };
        self.append(vm, RecordType::GNSS_GAP, &g.encode(), until_ms)
    }

    /// A checked RTC, as soon as it is known to be one, dates an undated bundle.
    fn rtc_time(&mut self, vm: &Vm, now_ms: u64) -> Result<(), &'static str> {
        if self.basis.is_some() {
            return Ok(());
        }
        let st = shared::status();
        let acc = if st.rtc_checked_unix > 0 {
            Some(2_000)
        } else {
            (st.prime_time_acc_ms > 0).then_some(st.prime_time_acc_ms)
        };
        if let (Some(acc), Some(unix)) = (acc, shared::rtc_now()) {
            self.observe_time(vm, unix as u64 * 1000, acc, TIME_SOURCE_RTC, now_ms)?;
        }
        Ok(())
    }

    /// Once a minute: the device's state.
    pub fn tick(&mut self, vm: &Vm, now_ms: u64) -> Result<(), &'static str> {
        self.rtc_time(vm, now_ms)?;
        if now_ms.saturating_sub(self.last_health_ms) < HEALTH_EVERY_MS {
            return Ok(());
        }
        self.last_health_ms = now_ms;
        let st = shared::status();
        let h = DeviceHealth {
            battery_mv: if st.battery { st.vbat_mv } else { st.vbus_mv },
            sd_write_errors: st.write_errors.min(u16::MAX as u32) as u16,
            sd_free_mib: UNKNOWN_U16,
            device_temp_c: st.temp_c.clamp(-127.0, 127.0) as i8,
            rssi_dbm: -128,
            ext_sensor_1: 0,
            ext_sensor_2: 0,
            health_state: 0,
            reboot_count: 0,
        };
        self.append(vm, RecordType::DEVICE_HEALTH, &h.encode(), now_ms)
    }

    pub fn flush(&self, vm: &Vm) -> Result<(), &'static str> {
        vm.flush_file(self.file).map_err(|_| "segment flush")
    }

    /// Close the capture: the open gap, if any, written; files closed. Returns
    /// the directory name to seal.
    pub fn finish(mut self, vm: &Vm, now_ms: u64) -> HString<12> {
        if let Some(since) = self.no_fix_since.take() {
            let _ = self.gap(vm, since, now_ms);
        }
        let _ = vm.close_file(self.file);
        let _ = vm.close_dir(self.dir);
        self.name
    }
}

fn read_file(vm: &Vm, dir: RawDirectory, name: &str) -> Result<Vec<u8>, &'static str> {
    let f = vm
        .open_file_in_dir(dir, name, Mode::ReadOnly)
        .map_err(|_| "open member")?;
    let len = vm.file_length(f).unwrap_or(0) as usize;
    let mut v = alloc::vec![0u8; len];
    let mut n = 0;
    while n < len {
        match vm.read(f, &mut v[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let _ = vm.close_file(f);
    v.truncate(n);
    Ok(v)
}

fn write_file(vm: &Vm, dir: RawDirectory, name: &str, data: &[u8]) -> Result<(), &'static str> {
    let f = vm
        .open_file_in_dir(dir, name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| "create")?;
    let r = vm.write(f, data).map_err(|_| "write");
    let c = vm.close_file(f).map_err(|_| "close");
    r.and(c)
}

/// Whether a bundle directory holds a signature (sealed).
/// Whether a bundle directory is done with: signed, or marked empty.
fn is_sealed(vm: &Vm, dir: RawDirectory) -> bool {
    if let Ok(f) = vm.open_file_in_dir(dir, EMPTY_FILE, Mode::ReadOnly) {
        let _ = vm.close_file(f);
        return true;
    }
    match vm.open_file_in_dir(dir, SIG_FILE, Mode::ReadOnly) {
        Ok(f) => {
            let ok = vm.file_length(f).unwrap_or(0) == 64;
            let _ = vm.close_file(f);
            ok
        }
        Err(_) => false,
    }
}

pub struct Sealed {
    pub bundle_id: [u8; 16],
    pub frames: usize,
    pub segments: usize,
    pub bytes: u64,
    pub recovery: RecoveryState,
}

/// Seal a bundle directory from the bytes on the card alone (§2.1).
pub fn seal(
    vm: &Vm,
    root: RawDirectory,
    name: &str,
    id: &Identity,
    boot_id: [u8; 16],
    recovered: bool,
) -> Result<Sealed, &'static str> {
    let dir = vm.open_dir(root, name).map_err(|_| "open bundle dir")?;
    let r = seal_in(vm, dir, id, boot_id, recovered);
    if r.as_ref().err() == Some(&EMPTY) {
        let _ = write_file(vm, dir, EMPTY_FILE, b"no frame reached the card\n");
    }
    let _ = vm.close_dir(dir);
    r
}

fn seal_in(
    vm: &Vm,
    dir: RawDirectory,
    id: &Identity,
    boot_id: [u8; 16],
    recovered: bool,
) -> Result<Sealed, &'static str> {
    let mut indices: heapless::Vec<u32, 256> = heapless::Vec::new();
    vm.iterate_dir(dir, |e| {
        let base = e.name.base_name();
        let ext = e.name.extension();
        if base.len() == 8
            && base[0] == b'S'
            && ext == b"SEG"
            && let Ok(s) = core::str::from_utf8(&base[1..])
            && let Ok(i) = s.parse::<u32>()
        {
            let _ = indices.push(i);
        }
        core::ops::ControlFlow::Continue(())
    })
    .map_err(|_| "list bundle")?;
    indices.sort_unstable();
    if indices.is_empty() {
        return Err("no segments");
    }

    // Pass 1: rescan every segment with the key, as one chain.
    let keys = RootKeyProvider::new(id.root, id.key_version);
    let mut state = ScanState::default();
    let mut first: Option<SegmentHeader> = None;
    let mut counts: BTreeMap<RecordType, u32> = BTreeMap::new();
    let mut recovery = RecoveryState::Clean;
    let mut discarded = 0u32;
    let mut last: Option<(u32, u32)> = None; // (seq, monotonic_ms)
    let mut basis: Option<(u64, u32)> = None;
    let mut frames = 0usize;
    let mut kept: heapless::Vec<u32, 256> = heapless::Vec::new();
    for &i in &indices {
        let bytes = read_file(vm, dir, seg_file(i).as_str())?;
        // A segment whose directory entry was never flushed reads short:
        // the first one means an empty bundle; a later one ends the chain.
        if bytes.len() < cairn_format::SEGMENT_HEADER_SIZE {
            if first.is_none() {
                return Err(EMPTY);
            }
            break;
        }
        let res = scan_segment(&bytes, state, Some(&keys as &dyn KeyProvider))
            .map_err(|_| "segment does not scan")?;
        if matches!(res.stop, StopReason::AuthFailed) {
            return Err("a frame failed authentication; not signing over it");
        }
        let h = &res.header;
        if let Some(f) = &first {
            if h.device_id != f.device_id
                || h.device_counter != f.device_counter
                || h.storage_key_version != f.storage_key_version
            {
                return Err("a segment carries another identity");
            }
        } else {
            first = Some(h.clone());
        }
        for (rt, n) in &res.record_counts {
            *counts.entry(*rt).or_insert(0) += *n as u32;
        }
        for f in &res.frames {
            if basis.is_none()
                && f.record_type == RecordType::TIME_OBSERVATION
                && let Some(p) = &f.payload
                && let Ok(o) = parse_time_observation(p)
                && o.adopted
                && let Some(b) = o.implied_basis_ms(f.monotonic_ms)
            {
                basis = Some((b, o.accuracy_ms.unwrap_or(u32::MAX)));
            }
            last = Some((f.seq, f.monotonic_ms));
        }
        frames += res.frames.len();
        let _ = kept.push(i);
        if res.stop != StopReason::Eof {
            // A torn tail (a power cut mid-frame): kept, counted, and the
            // chain ends here — later segments cannot continue it.
            recovery = RecoveryState::RecoveredTail;
            discarded += res.discarded_tail_bytes;
            break;
        }
        state = res.next;
    }
    // Nothing captured: nothing to vouch for. (A manifest would still verify,
    // claiming seq 0..0 over no frames, and be uploaded as noise.)
    if frames == 0 {
        return Err(EMPTY);
    }
    let first = first.ok_or("no header")?;
    let (last_seq, last_ms) = last.unwrap_or((first.first_seq, 0));

    // The journal: opened, then sealed (or recovered at boot).
    let mut jh = first.clone();
    jh.segment_index = JOURNAL_SEGMENT_INDEX;
    jh.boot_id = boot_id;
    let mut jw = SegmentWriter::new(jh, ScanState::default(), &keys, nonces()?)
        .map_err(|_| "journal writer")?;
    let tr = |from, to, trigger| {
        StateTransition {
            region: 2,
            from_state: from,
            to_state: to,
            trigger_event: trigger,
            reason_code: 0,
            policy_version: 1,
            ..StateTransition::default()
        }
        .encode()
    };
    jw.append(
        RecordType::STATE_TRANSITION,
        1,
        0,
        0,
        &tr(0, STATE_OPEN, TRIGGER_OPENED),
    )
    .map_err(|_| "journal frame")?;
    let trigger = if recovered {
        TRIGGER_RECOVERED
    } else {
        TRIGGER_SEALED
    };
    jw.append(
        RecordType::STATE_TRANSITION,
        1,
        0,
        last_ms,
        &tr(STATE_OPEN, STATE_SEALED, trigger),
    )
    .map_err(|_| "journal frame")?;
    *counts.entry(RecordType::STATE_TRANSITION).or_insert(0) += 2;
    write_file(vm, dir, JOURNAL_FILE, jw.bytes())?;

    // Pass 2: members in canonical order, hashed; the transfer chunks over
    // their concatenation.
    let mut order: Vec<(String, HString<12>)> = Vec::new();
    order.push((
        String::from(JOURNAL_MEMBER_NAME),
        HString::try_from(JOURNAL_FILE).unwrap_or_default(),
    ));
    for &i in &kept {
        order.push((segment_member_name(i), seg_file(i)));
    }
    order.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut members: Vec<Member> = Vec::new();
    let mut chunks: Vec<ChunkDescriptor> = Vec::new();
    let mut pending: Vec<u8> = Vec::with_capacity(CHUNK);
    let mut total = 0u64;
    let emit = |piece: &[u8], chunks: &mut Vec<ChunkDescriptor>| {
        chunks.push(ChunkDescriptor {
            index: chunks.len() as u32,
            byte_length: piece.len() as u32,
            sha256: Sha256::digest(piece).into(),
        });
    };
    for (canonical, file) in &order {
        let bytes = read_file(vm, dir, file.as_str())?;
        total += bytes.len() as u64;
        members.push(Member {
            name: canonical.clone(),
            length: bytes.len() as u64,
            sha256: Sha256::digest(&bytes).into(),
        });
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            let take = (CHUNK - pending.len()).min(rest.len());
            pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if pending.len() == CHUNK {
                emit(&pending, &mut chunks);
                pending.clear();
            }
        }
    }
    if !pending.is_empty() {
        emit(&pending, &mut chunks);
    }
    sort_members(&mut members);
    let root = content_root(&members).map_err(|_| "content root")?;

    let signing = ed25519_dalek::SigningKey::from_bytes(&id.seed);
    let (basis_ms, basis_acc) = basis.unwrap_or((0, u32::MAX));
    let mut bundle_id = [0u8; 16];
    // A ULID: 48-bit ms timestamp (the basis, when there is one), 80 random bits.
    bundle_id[..6].copy_from_slice(&basis_ms.to_be_bytes()[2..8]);
    if let Ok(rng) = esp_hal::rng::Trng::try_new() {
        rng.read(&mut bundle_id[6..]);
    }
    let mut fw = String::new();
    let _ = write!(fw, "cairn-trail-{}+{}", shared::VERSION, shared::COMMIT);
    let manifest = Manifest {
        manifest_version: MANIFEST_VERSION,
        bundle_id,
        device_id: first.device_id,
        device_key_id: device_key_id(&signing.verifying_key()),
        boot_id: first.boot_id,
        firmware_version: fw,
        schema_version: 1,
        capture_started_monotonic_us: first.opened_monotonic_us,
        capture_ended_monotonic_us: first.opened_monotonic_us + last_ms as u64 * 1000,
        utc_basis_ms: basis_ms,
        utc_basis_acc_ms: basis_acc,
        first_seq: first.first_seq,
        last_seq,
        record_counts: counts,
        members,
        chunk_descriptors: chunks,
        content_root: root,
        previous_bundle_root: None,
        policy_version: 1,
        recovery_state: recovery,
        discarded_tail_bytes: discarded,
        signature_algorithm: SIGNATURE_ALGORITHM_ED25519.into(),
        trip_seq: None,
        engine_profile: None,
        vehicle_id: first.vehicle_id,
        assignment_id: first.assignment_id,
        device_counter: first.device_counter,
        storage_key_version: first.storage_key_version,
        encryption_suite: ENCRYPTION_SUITE_V1.into(),
    };
    let (bytes, sig) = manifest.sign(&signing).map_err(|_| "manifest encode")?;
    write_file(vm, dir, MANIFEST_FILE, &bytes)?;
    // The signature last: its presence means the manifest beside it is whole.
    write_file(vm, dir, SIG_FILE, &sig)?;
    Ok(Sealed {
        bundle_id,
        frames,
        segments: kept.len(),
        bytes: total,
        recovery,
    })
}

/// At boot: seal every bundle a power cut left open.
pub fn recover(vm: &Vm, root: RawDirectory, id: &Identity, boot_id: [u8; 16]) {
    let mut names: heapless::Vec<HString<12>, 32> = heapless::Vec::new();
    let _ = vm.iterate_dir(root, |e| {
        let base = e.name.base_name();
        if e.attributes.is_directory()
            && base.len() == 8
            && base[0] == b'B'
            && e.name.extension().is_empty()
        {
            let mut s = HString::new();
            let _ = write!(s, "{}", e.name);
            let _ = names.push(s);
        }
        core::ops::ControlFlow::Continue(())
    });
    for n in &names {
        let Ok(dir) = vm.open_dir(root, n.as_str()) else {
            continue;
        };
        let sealed = is_sealed(vm, dir);
        let _ = vm.close_dir(dir);
        if sealed {
            continue;
        }
        match seal(vm, root, n.as_str(), id, boot_id, true) {
            Ok(s) => event(
                "bundle_recovered",
                format_args!(
                    "{} sealed at boot: {} frames in {} segments, {:?}",
                    n, s.frames, s.segments, s.recovery
                ),
            ),
            Err(EMPTY) => event(
                "bundle_recovered",
                format_args!("{} {}; marked EMPTY", n, EMPTY),
            ),
            Err(e) => event("bundle_recovered", format_args!("{} not sealed: {}", n, e)),
        }
    }
}

/// The device's public key, for `trail bundle verify` (`cairn-verify -device-key`).
pub fn public_key_hex(id: &Identity) -> String {
    cairn_format::hex(
        ed25519_dalek::SigningKey::from_bytes(&id.seed)
            .verifying_key()
            .as_bytes(),
    )
}
