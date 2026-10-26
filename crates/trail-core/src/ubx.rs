//! u-blox UBX protocol: a streaming frame parser, frame builders for the
//! messages the firmware sends, and decoders for the messages it reads.
//!
//! The MAX-M10S is configured with UBX-CFG-VALSET only; the legacy CFG-PRT,
//! CFG-MSG and CFG-RATE messages are not used (see docs/prior-art.md §2).

pub const SYNC1: u8 = 0xB5;
pub const SYNC2: u8 = 0x62;

pub const CLASS_NAV: u8 = 0x01;
pub const CLASS_ACK: u8 = 0x05;
pub const CLASS_CFG: u8 = 0x06;
pub const CLASS_MON: u8 = 0x0A;

pub const NAV_PVT: u8 = 0x07;
pub const NAV_SAT: u8 = 0x35;
pub const ACK_NAK: u8 = 0x00;
pub const ACK_ACK: u8 = 0x01;
pub const CFG_CFG: u8 = 0x09;
pub const CFG_VALSET: u8 = 0x8A;
pub const MON_VER: u8 = 0x04;

/// Largest payload the parser keeps. NAV-SAT is 8 + 12 bytes per satellite;
/// 1024 bytes holds 84 satellites, more than an M10 tracks.
pub const MAX_PAYLOAD: usize = 1024;

/// Configuration keys used by the firmware, from the u-blox M10 SPG 5.10
/// interface description. The size of each value is encoded in bits 28..30.
pub mod key {
    pub const UART1_BAUDRATE: u32 = 0x4052_0001;
    pub const UART1INPROT_UBX: u32 = 0x1073_0001;
    pub const UART1INPROT_NMEA: u32 = 0x1073_0002;
    pub const UART1OUTPROT_UBX: u32 = 0x1074_0001;
    pub const UART1OUTPROT_NMEA: u32 = 0x1074_0002;
    pub const MSGOUT_NAV_PVT_UART1: u32 = 0x2091_0007;
    pub const MSGOUT_NAV_SAT_UART1: u32 = 0x2091_0016;
    pub const NAVSPG_DYNMODEL: u32 = 0x2011_0021;
    pub const RATE_MEAS: u32 = 0x3021_0001;
    pub const RATE_NAV: u32 = 0x3021_0002;
    pub const PM_OPERATEMODE: u32 = 0x20D0_0001;
}

/// `CFG-NAVSPG-DYNMODEL` values.
pub mod dynmodel {
    pub const PORTABLE: u64 = 0;
    pub const PEDESTRIAN: u64 = 3;
    pub const AUTOMOTIVE: u64 = 4;
}

/// Fletcher-8 over class, id, length and payload, as UBX specifies.
pub fn checksum(bytes: &[u8]) -> (u8, u8) {
    let mut a: u8 = 0;
    let mut b: u8 = 0;
    for &x in bytes {
        a = a.wrapping_add(x);
        b = b.wrapping_add(a);
    }
    (a, b)
}

/// Write a complete UBX frame into `out`, returning its length, or `None` if
/// `out` is too small.
pub fn frame(class: u8, id: u8, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = payload.len() + 8;
    if out.len() < n || payload.len() > u16::MAX as usize {
        return None;
    }
    out[0] = SYNC1;
    out[1] = SYNC2;
    out[2] = class;
    out[3] = id;
    out[4..6].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    out[6..6 + payload.len()].copy_from_slice(payload);
    let (a, b) = checksum(&out[2..6 + payload.len()]);
    out[n - 2] = a;
    out[n - 1] = b;
    Some(n)
}

/// A poll request: the message with an empty payload.
pub fn poll(class: u8, id: u8, out: &mut [u8]) -> Option<usize> {
    frame(class, id, &[], out)
}

/// Bytes a configuration value occupies, from the key's size field.
pub fn value_size(key: u32) -> usize {
    match (key >> 28) & 0x7 {
        1 | 2 => 1,
        3 => 2,
        4 => 4,
        5 => 8,
        _ => 0,
    }
}

/// Layer bits for CFG-VALSET.
pub const LAYER_RAM: u8 = 0x01;
pub const LAYER_BBR: u8 = 0x02;

/// UBX-CFG-VALSET writing `items` to `layers`.
pub fn valset(layers: u8, items: &[(u32, u64)], out: &mut [u8]) -> Option<usize> {
    let mut payload = [0u8; 256];
    payload[0] = 0; // version
    payload[1] = layers;
    let mut n = 4;
    for &(k, v) in items {
        let size = value_size(k);
        if size == 0 || n + 4 + size > payload.len() {
            return None;
        }
        payload[n..n + 4].copy_from_slice(&k.to_le_bytes());
        payload[n + 4..n + 4 + size].copy_from_slice(&v.to_le_bytes()[..size]);
        n += 4 + size;
    }
    frame(CLASS_CFG, CFG_VALSET, &payload[..n], out)
}

/// UBX-CFG-CFG clearing every configuration section from the battery-backed
/// layer and loading the result, which returns the receiver to its defaults
/// without touching navigation data (ephemeris, almanac) kept for hot start.
pub fn cfg_reset_to_defaults(out: &mut [u8]) -> Option<usize> {
    let mut p = [0u8; 13];
    p[0..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // clearMask
    p[4..8].copy_from_slice(&0u32.to_le_bytes()); // saveMask
    p[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // loadMask
    p[12] = 0x01 | 0x02; // devBBR | devFlash (the M10 has no flash; harmless)
    frame(CLASS_CFG, CFG_CFG, &p, out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Sync1,
    Sync2,
    Class,
    Id,
    Len1,
    Len2,
    Payload,
    CkA,
    CkB,
}

/// Counters a reader reports, so lost data is visible rather than silent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParserStats {
    pub frames: u32,
    pub checksum_errors: u32,
    pub oversize: u32,
}

/// A streaming parser. Feed it every byte from the UART, in any chunking;
/// bytes that are not part of a UBX frame (NMEA, a reset banner) are skipped.
pub struct Parser {
    state: State,
    class: u8,
    id: u8,
    len: usize,
    idx: usize,
    ck_a: u8,
    ck_b: u8,
    rx_a: u8,
    buf: [u8; MAX_PAYLOAD],
    pub stats: ParserStats,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// A complete frame: class, id and the payload length in the parser's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub class: u8,
    pub id: u8,
    pub len: usize,
}

impl Parser {
    pub const fn new() -> Self {
        Self {
            state: State::Sync1,
            class: 0,
            id: 0,
            len: 0,
            idx: 0,
            ck_a: 0,
            ck_b: 0,
            rx_a: 0,
            buf: [0; MAX_PAYLOAD],
            stats: ParserStats {
                frames: 0,
                checksum_errors: 0,
                oversize: 0,
            },
        }
    }

    fn add(&mut self, b: u8) {
        self.ck_a = self.ck_a.wrapping_add(b);
        self.ck_b = self.ck_b.wrapping_add(self.ck_a);
    }

    /// The payload of the frame most recently returned by [`Parser::push`].
    /// Valid until the next byte is pushed.
    pub fn payload(&self, info: &FrameInfo) -> &[u8] {
        &self.buf[..info.len]
    }

    /// Push one byte. Returns a frame when its second checksum byte arrives
    /// and both checksum bytes match.
    pub fn push(&mut self, b: u8) -> Option<FrameInfo> {
        match self.state {
            State::Sync1 => {
                if b == SYNC1 {
                    self.state = State::Sync2;
                }
            }
            State::Sync2 => {
                self.state = match b {
                    SYNC2 => State::Class,
                    SYNC1 => State::Sync2,
                    _ => State::Sync1,
                };
                self.ck_a = 0;
                self.ck_b = 0;
            }
            State::Class => {
                self.class = b;
                self.add(b);
                self.state = State::Id;
            }
            State::Id => {
                self.id = b;
                self.add(b);
                self.state = State::Len1;
            }
            State::Len1 => {
                self.len = b as usize;
                self.add(b);
                self.state = State::Len2;
            }
            State::Len2 => {
                self.len |= (b as usize) << 8;
                self.add(b);
                self.idx = 0;
                if self.len > MAX_PAYLOAD {
                    self.stats.oversize += 1;
                    self.state = State::Sync1;
                } else if self.len == 0 {
                    self.state = State::CkA;
                } else {
                    self.state = State::Payload;
                }
            }
            State::Payload => {
                self.buf[self.idx] = b;
                self.idx += 1;
                self.add(b);
                if self.idx == self.len {
                    self.state = State::CkA;
                }
            }
            State::CkA => {
                self.rx_a = b;
                self.state = State::CkB;
            }
            State::CkB => {
                self.state = State::Sync1;
                if self.rx_a == self.ck_a && b == self.ck_b {
                    self.stats.frames += 1;
                    return Some(FrameInfo {
                        class: self.class,
                        id: self.id,
                        len: self.len,
                    });
                }
                self.stats.checksum_errors += 1;
            }
        }
        None
    }
}

fn u16le(p: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([p[o], p[o + 1]])
}
fn i16le(p: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([p[o], p[o + 1]])
}
fn u32le(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}
fn i32le(p: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

/// UBX-NAV-PVT, 92 bytes. Units are the receiver's; nothing is rounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavPvt {
    pub itow_ms: u32,
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
    /// bit 0 validDate, 1 validTime, 2 fullyResolved, 3 validMag
    pub valid: u8,
    pub t_acc_ns: u32,
    pub nano: i32,
    /// 0 none, 1 dead reckoning, 2 2D, 3 3D, 4 GNSS + DR, 5 time only
    pub fix_type: u8,
    /// bit 0 gnssFixOK, 1 diffSoln, 2..4 psmState, 5 headVehValid, 6..7 carrSoln
    pub flags: u8,
    pub flags2: u8,
    pub num_sv: u8,
    pub lon_e7: i32,
    pub lat_e7: i32,
    pub height_mm: i32,
    pub hmsl_mm: i32,
    pub h_acc_mm: u32,
    pub v_acc_mm: u32,
    pub vel_n_mms: i32,
    pub vel_e_mms: i32,
    pub vel_d_mms: i32,
    pub g_speed_mms: i32,
    pub head_mot_e5: i32,
    pub s_acc_mms: u32,
    pub head_acc_e5: u32,
    pub pdop_e2: u16,
    /// bit 0 invalidLlh
    pub flags3: u16,
}

impl NavPvt {
    pub const LEN: usize = 92;

    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < Self::LEN {
            return None;
        }
        Some(Self {
            itow_ms: u32le(p, 0),
            year: u16le(p, 4),
            month: p[6],
            day: p[7],
            hour: p[8],
            min: p[9],
            sec: p[10],
            valid: p[11],
            t_acc_ns: u32le(p, 12),
            nano: i32le(p, 16),
            fix_type: p[20],
            flags: p[21],
            flags2: p[22],
            num_sv: p[23],
            lon_e7: i32le(p, 24),
            lat_e7: i32le(p, 28),
            height_mm: i32le(p, 32),
            hmsl_mm: i32le(p, 36),
            h_acc_mm: u32le(p, 40),
            v_acc_mm: u32le(p, 44),
            vel_n_mms: i32le(p, 48),
            vel_e_mms: i32le(p, 52),
            vel_d_mms: i32le(p, 56),
            g_speed_mms: i32le(p, 60),
            head_mot_e5: i32le(p, 64),
            s_acc_mms: u32le(p, 68),
            head_acc_e5: u32le(p, 72),
            pdop_e2: u16le(p, 76),
            flags3: u16le(p, 78),
        })
    }

    pub fn gnss_fix_ok(&self) -> bool {
        self.flags & 0x01 != 0
    }

    pub fn invalid_llh(&self) -> bool {
        self.flags3 & 0x01 != 0
    }

    pub fn psm_state(&self) -> u8 {
        (self.flags >> 2) & 0x07
    }

    /// A position worth recording: the receiver says the fix is good, it is
    /// 2D or 3D, and the coordinates are in range in both directions.
    pub fn has_position(&self) -> bool {
        self.gnss_fix_ok()
            && (self.fix_type == 2 || self.fix_type == 3 || self.fix_type == 4)
            && !self.invalid_llh()
            && (-900_000_000..=900_000_000).contains(&self.lat_e7)
            && (-1_800_000_000..=1_800_000_000).contains(&self.lon_e7)
    }

    /// Date and time both valid and fully resolved.
    pub fn time_resolved(&self) -> bool {
        self.valid & 0x07 == 0x07
    }
}

/// One satellite from UBX-NAV-SAT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SatInfo {
    pub gnss_id: u8,
    pub sv_id: u8,
    pub cno_dbhz: u8,
    pub elev_deg: i8,
    pub azim_deg: i16,
    pub pr_res_dm: i16,
    pub flags: u32,
}

impl SatInfo {
    pub fn quality(&self) -> u8 {
        (self.flags & 0x07) as u8
    }
    pub fn used(&self) -> bool {
        self.flags & 0x08 != 0
    }
    pub fn health(&self) -> u8 {
        ((self.flags >> 4) & 0x03) as u8
    }
}

/// UBX-NAV-SAT header: iTOW and the satellite count.
pub fn nav_sat_header(p: &[u8]) -> Option<(u32, u8)> {
    if p.len() < 8 {
        return None;
    }
    Some((u32le(p, 0), p[5]))
}

/// The `i`th satellite in a NAV-SAT payload.
pub fn nav_sat(p: &[u8], i: usize) -> Option<SatInfo> {
    let o = 8 + 12 * i;
    if p.len() < o + 12 {
        return None;
    }
    Some(SatInfo {
        gnss_id: p[o],
        sv_id: p[o + 1],
        cno_dbhz: p[o + 2],
        elev_deg: p[o + 3] as i8,
        azim_deg: i16le(p, o + 4),
        pr_res_dm: i16le(p, o + 6),
        flags: u32le(p, o + 8),
    })
}

/// A fixed-width, NUL-padded ASCII field as text, trimmed.
pub fn ascii_field(p: &[u8]) -> &str {
    let end = p.iter().position(|&c| c == 0).unwrap_or(p.len());
    core::str::from_utf8(&p[..end]).unwrap_or("?").trim()
}

/// UBX-MON-VER: software version (30 bytes), hardware version (10 bytes),
/// then 30-byte extension strings.
pub fn mon_ver(p: &[u8]) -> Option<(&str, &str)> {
    if p.len() < 40 {
        return None;
    }
    Some((ascii_field(&p[0..30]), ascii_field(&p[30..40])))
}

pub fn mon_ver_ext(p: &[u8], i: usize) -> Option<&str> {
    let o = 40 + 30 * i;
    if p.len() < o + 30 {
        return None;
    }
    Some(ascii_field(&p[o..o + 30]))
}

/// The class and id an ACK or NAK refers to.
pub fn ack_target(p: &[u8]) -> Option<(u8, u8)> {
    if p.len() < 2 {
        return None;
    }
    Some((p[0], p[1]))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn feed(p: &mut Parser, bytes: &[u8]) -> Vec<(FrameInfo, Vec<u8>)> {
        let mut out = Vec::new();
        for &b in bytes {
            if let Some(f) = p.push(b) {
                out.push((f, p.payload(&f).to_vec()));
            }
        }
        out
    }

    fn pvt_payload() -> [u8; 92] {
        let mut p = [0u8; 92];
        p[0..4].copy_from_slice(&123_456u32.to_le_bytes());
        p[4..6].copy_from_slice(&2026u16.to_le_bytes());
        p[6] = 10;
        p[7] = 25;
        p[8] = 21;
        p[9] = 5;
        p[10] = 9;
        p[11] = 0x07;
        p[20] = 3;
        p[21] = 0x01;
        p[23] = 11;
        p[24..28].copy_from_slice(&(-1_223_456_789i32).to_le_bytes());
        p[28..32].copy_from_slice(&(471_234_567i32).to_le_bytes());
        p[36..40].copy_from_slice(&(123_456i32).to_le_bytes());
        p[40..44].copy_from_slice(&(3_200u32).to_le_bytes());
        p[44..48].copy_from_slice(&(5_100u32).to_le_bytes());
        p[76..78].copy_from_slice(&(145u16).to_le_bytes());
        p
    }

    fn pvt_frame() -> Vec<u8> {
        let mut out = [0u8; 128];
        let n = frame(CLASS_NAV, NAV_PVT, &pvt_payload(), &mut out).unwrap();
        out[..n].to_vec()
    }

    #[test]
    fn checksum_of_known_poll() {
        // MON-VER poll is B5 62 0A 04 00 00 0E 34 in u-blox's own examples.
        let mut out = [0u8; 8];
        let n = poll(CLASS_MON, MON_VER, &mut out).unwrap();
        assert_eq!(&out[..n], &[0xB5, 0x62, 0x0A, 0x04, 0x00, 0x00, 0x0E, 0x34]);
    }

    #[test]
    fn parses_a_frame_at_every_split_point() {
        let f = pvt_frame();
        for split in 0..=f.len() {
            let mut p = Parser::new();
            let mut got = feed(&mut p, &f[..split]);
            got.extend(feed(&mut p, &f[split..]));
            assert_eq!(got.len(), 1, "split at {split}");
            let pvt = NavPvt::parse(&got[0].1).unwrap();
            assert_eq!(pvt.year, 2026);
            assert_eq!(pvt.num_sv, 11);
            assert_eq!(pvt.lat_e7, 471_234_567);
            assert_eq!(pvt.lon_e7, -1_223_456_789);
            assert!(pvt.has_position());
            assert!(pvt.time_resolved());
        }
    }

    #[test]
    fn rejects_bad_checksums_and_recovers() {
        let good = pvt_frame();
        for corrupt in [good.len() - 2, good.len() - 1, 10] {
            let mut bad = good.clone();
            bad[corrupt] ^= 0x5A;
            let mut p = Parser::new();
            let mut stream = bad.clone();
            stream.extend_from_slice(&good);
            let got = feed(&mut p, &stream);
            assert_eq!(got.len(), 1, "corrupting byte {corrupt}");
            assert_eq!(p.stats.checksum_errors, 1);
        }
    }

    #[test]
    fn skips_nmea_and_banners_between_frames() {
        let mut stream = b"$GNRMC,,V,,,,,,,,,,N*4D\r\nu-blox AG - www.u-blox.com\r\n".to_vec();
        stream.extend(pvt_frame());
        stream.push(SYNC1); // a stray sync byte right before a real frame
        stream.extend(pvt_frame());
        let mut p = Parser::new();
        assert_eq!(feed(&mut p, &stream).len(), 2);
    }

    #[test]
    fn sync_bytes_inside_a_payload_do_not_confuse_it() {
        let payload = [SYNC1, SYNC2, 0x01, 0x07, SYNC1, SYNC2];
        let mut out = [0u8; 32];
        let n = frame(0x10, 0x20, &payload, &mut out).unwrap();
        let mut p = Parser::new();
        let got = feed(&mut p, &out[..n]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, payload);
    }

    #[test]
    fn oversize_length_is_dropped_and_counted() {
        let mut p = Parser::new();
        let bytes = [SYNC1, SYNC2, 0x01, 0x35, 0xFF, 0xFF];
        assert!(feed(&mut p, &bytes).is_empty());
        assert_eq!(p.stats.oversize, 1);
        assert_eq!(feed(&mut p, &pvt_frame()).len(), 1);
    }

    #[test]
    fn out_of_range_coordinates_are_not_a_position() {
        let mut pl = pvt_payload();
        pl[28..32].copy_from_slice(&(-950_000_000i32).to_le_bytes());
        assert!(!NavPvt::parse(&pl).unwrap().has_position());
        let mut pl = pvt_payload();
        pl[21] = 0; // gnssFixOK clear
        assert!(!NavPvt::parse(&pl).unwrap().has_position());
    }

    #[test]
    fn valset_encodes_sizes_by_key() {
        let mut out = [0u8; 64];
        let n = valset(
            LAYER_RAM,
            &[
                (key::UART1_BAUDRATE, 115_200),
                (key::NAVSPG_DYNMODEL, 3),
                (key::RATE_MEAS, 1000),
            ],
            &mut out,
        )
        .unwrap();
        // header 6 + payload 4 + (4+4) + (4+1) + (4+2) + checksum 2
        assert_eq!(n, 6 + 4 + 8 + 5 + 6 + 2);
        assert_eq!(&out[6..10], &[0, LAYER_RAM, 0, 0]);
        assert_eq!(&out[10..14], &0x4052_0001u32.to_le_bytes());
        assert_eq!(&out[14..18], &115_200u32.to_le_bytes());
        assert_eq!(out[22], 3);
        assert_eq!(&out[27..29], &1000u16.to_le_bytes());
        let (a, b) = checksum(&out[2..n - 2]);
        assert_eq!((out[n - 2], out[n - 1]), (a, b));
    }

    #[test]
    fn nav_sat_decodes_each_satellite() {
        let mut p = std::vec![0u8; 8 + 24];
        p[5] = 2;
        p[8] = 0; // GPS
        p[9] = 12;
        p[10] = 41;
        p[11] = 37;
        p[12..14].copy_from_slice(&(271i16).to_le_bytes());
        p[16..20].copy_from_slice(&(0x0000_000Fu32).to_le_bytes());
        p[20] = 2; // Galileo
        p[21] = 7;
        assert_eq!(nav_sat_header(&p), Some((0, 2)));
        let s = nav_sat(&p, 0).unwrap();
        assert_eq!(
            (s.sv_id, s.cno_dbhz, s.elev_deg, s.azim_deg),
            (12, 41, 37, 271)
        );
        assert!(s.used());
        assert_eq!(s.quality(), 7);
        assert_eq!(nav_sat(&p, 1).unwrap().gnss_id, 2);
        assert!(nav_sat(&p, 2).is_none());
    }

    #[test]
    fn mon_ver_strings() {
        let mut p = std::vec![0u8; 70];
        p[..13].copy_from_slice(b"ROM SPG 5.10 ");
        p[30..38].copy_from_slice(b"000A0000");
        p[40..53].copy_from_slice(b"PROTVER=34.10");
        assert_eq!(mon_ver(&p), Some(("ROM SPG 5.10", "000A0000")));
        assert_eq!(mon_ver_ext(&p, 0), Some("PROTVER=34.10"));
    }
}
