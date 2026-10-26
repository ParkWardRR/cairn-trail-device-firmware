//! Reading a GUID partition table, so a card formatted by macOS Disk Utility
//! with its default scheme (GPT, an EFI partition, then the FAT32 data
//! partition) can be used by a FAT library that only understands MBR.
//!
//! The firmware reads block 0; if it is a protective MBR, it reads the GPT,
//! finds the first basic-data partition, and presents a synthesised MBR with
//! that one partition in place of block 0. The card itself is never written
//! by this: block 0 stays the protective MBR.

pub const BLOCK: usize = 512;

/// Basic data partition, EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, as stored
/// (the first three fields little-endian).
pub const BASIC_DATA: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

/// EFI system partition, C12A7328-F81F-11D2-BA4B-00A0C93EC93B, as stored.
pub const EFI_SYSTEM: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Block 0 is an MBR whose first partition is the GPT protective type 0xEE.
pub fn is_protective_mbr(b0: &[u8; BLOCK]) -> bool {
    b0[510] == 0x55 && b0[511] == 0xAA && b0[446 + 4] == 0xEE
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
}

/// Parse the GPT header in block 1.
pub fn header(b1: &[u8; BLOCK]) -> Option<Header> {
    if &b1[0..8] != b"EFI PART" {
        return None;
    }
    let h = Header {
        entries_lba: u64le(b1, 72),
        entry_count: u32le(b1, 80),
        entry_size: u32le(b1, 84),
    };
    if h.entry_size < 128
        || h.entry_size as usize > BLOCK
        || !BLOCK.is_multiple_of(h.entry_size as usize)
    {
        return None;
    }
    Some(h)
}

/// One partition entry: (first LBA, last LBA inclusive) if it is basic data.
pub fn basic_data_extent(entry: &[u8]) -> Option<(u64, u64)> {
    if entry.len() < 48 || entry[0..16] != BASIC_DATA {
        return None;
    }
    let (first, last) = (u64le(entry, 32), u64le(entry, 40));
    (last >= first).then_some((first, last))
}

/// An MBR with one FAT32 (LBA) partition covering `[first, first + count)`.
pub fn synth_mbr(first: u32, count: u32) -> [u8; BLOCK] {
    let mut m = [0u8; BLOCK];
    let e = 446;
    m[e] = 0x00; // not bootable
    m[e + 1..e + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]); // CHS: use LBA
    m[e + 4] = 0x0C; // FAT32 with LBA
    m[e + 5..e + 8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    m[e + 8..e + 12].copy_from_slice(&first.to_le_bytes());
    m[e + 12..e + 16].copy_from_slice(&count.to_le_bytes());
    m[510] = 0x55;
    m[511] = 0xAA;
    m
}

/// From an extent, the MBR to present — `None` if it does not fit in 32 bits.
pub fn mbr_for_extent(first: u64, last: u64) -> Option<[u8; BLOCK]> {
    let count = last.checked_sub(first)?.checked_add(1)?;
    Some(synth_mbr(
        u32::try_from(first).ok()?,
        u32::try_from(count).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(type_guid: [u8; 16], first: u64, last: u64) -> [u8; 128] {
        let mut e = [0u8; 128];
        e[0..16].copy_from_slice(&type_guid);
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        e
    }

    #[test]
    fn mac_style_layout() {
        // What Disk Utility writes: EFI at 40..409639, data after it.
        let mut b0 = [0u8; BLOCK];
        b0[446 + 4] = 0xEE;
        b0[510] = 0x55;
        b0[511] = 0xAA;
        assert!(is_protective_mbr(&b0));

        let mut b1 = [0u8; BLOCK];
        b1[0..8].copy_from_slice(b"EFI PART");
        b1[72..80].copy_from_slice(&2u64.to_le_bytes());
        b1[80..84].copy_from_slice(&128u32.to_le_bytes());
        b1[84..88].copy_from_slice(&128u32.to_le_bytes());
        let h = header(&b1).unwrap();
        assert_eq!(
            h,
            Header {
                entries_lba: 2,
                entry_count: 128,
                entry_size: 128
            }
        );

        let efi = entry(EFI_SYSTEM, 40, 409_639);
        let data = entry(BASIC_DATA, 409_640, 122_142_719);
        assert_eq!(basic_data_extent(&efi), None);
        let (f, l) = basic_data_extent(&data).unwrap();
        let mbr = mbr_for_extent(f, l).unwrap();
        assert_eq!(mbr[446 + 4], 0x0C);
        assert_eq!(u32le(&mbr, 446 + 8), 409_640);
        assert_eq!(u32le(&mbr, 446 + 12), 122_142_719 - 409_640 + 1);
        assert_eq!(&mbr[510..], &[0x55, 0xAA]);
        assert!(!is_protective_mbr(&mbr));
    }

    #[test]
    fn rejects_non_gpt_and_bad_sizes() {
        let b1 = [0u8; BLOCK];
        assert!(header(&b1).is_none());
        let mut b1 = [0u8; BLOCK];
        b1[0..8].copy_from_slice(b"EFI PART");
        b1[84..88].copy_from_slice(&100u32.to_le_bytes());
        assert!(header(&b1).is_none());
    }

    #[test]
    fn extent_beyond_32_bits_is_refused() {
        assert!(mbr_for_extent(1 << 33, (1 << 33) + 10).is_none());
        assert!(mbr_for_extent(10, 5).is_none());
    }
}
