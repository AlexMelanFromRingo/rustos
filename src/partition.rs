//! Partition-table parsers: MBR (DOS) and GPT (UEFI).
//!
//! MBR is the first 512-byte sector of a disk.  Bytes 0..446 are
//! bootstrap code, 446..510 are four 16-byte partition entries, and the
//! last two bytes are the 0x55AA boot signature.
//!
//! GPT lives at LBA 1 (right after a "protective MBR" entry of type 0xEE
//! at LBA 0).  The header is 92 bytes; partition entry array starts at
//! the LBA pointed to by the header and contains up to 128 entries of
//! 128 bytes each (UEFI 2.x §5.3).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const MBR_SIGNATURE_OFFSET: usize = 510;
pub const MBR_SIGNATURE: u16 = 0xAA55;

#[derive(Debug, Clone, Copy)]
pub struct Partition {
    pub bootable:  bool,
    pub kind:      u8,
    pub start_lba: u32,
    pub sectors:   u32,
}

impl Partition {
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            0x00 => "empty",
            0x01 => "FAT12",
            0x04 | 0x06 => "FAT16",
            0x07 => "NTFS/exFAT",
            0x0B | 0x0C => "FAT32",
            0x0E => "FAT16-LBA",
            0x82 => "Linux swap",
            0x83 => "Linux",
            0x8E => "Linux LVM",
            0xA5 => "FreeBSD",
            0xAF => "HFS",
            0xEE => "GPT protective",
            0xEF => "EFI system",
            0xFD => "Linux RAID",
            _ => "unknown",
        }
    }
}

/// Parse the four partition entries.  Returns Err if the boot signature
/// is wrong.  Empty entries (type 0) are kept so indexing is preserved.
pub fn parse_mbr(sector: &[u8]) -> Result<[Partition; 4], &'static str> {
    if sector.len() < 512 { return Err("short sector"); }
    let sig = u16::from_le_bytes([sector[MBR_SIGNATURE_OFFSET], sector[MBR_SIGNATURE_OFFSET + 1]]);
    if sig != MBR_SIGNATURE { return Err("bad MBR signature"); }

    let mut out = [Partition { bootable: false, kind: 0, start_lba: 0, sectors: 0 }; 4];
    for i in 0..4 {
        let off = 446 + i * 16;
        let bootable = sector[off] == 0x80;
        let kind = sector[off + 4];
        let start = u32::from_le_bytes([
            sector[off + 8], sector[off + 9], sector[off + 10], sector[off + 11],
        ]);
        let count = u32::from_le_bytes([
            sector[off + 12], sector[off + 13], sector[off + 14], sector[off + 15],
        ]);
        out[i] = Partition { bootable, kind, start_lba: start, sectors: count };
    }
    Ok(out)
}

/// Look at the first entry's type to decide whether this is actually a GPT
/// disk hiding behind a protective MBR.
pub fn is_gpt_protective(parts: &[Partition; 4]) -> bool {
    parts[0].kind == 0xEE
}

/// Pretty-print a MBR parse result.
pub fn format_table(parts: &[Partition; 4]) -> Vec<alloc::string::String> {
    let mut out = Vec::new();
    out.push(alloc::string::String::from("idx  boot  type  start_lba    sectors      label"));
    for (i, p) in parts.iter().enumerate() {
        if p.kind == 0 && p.sectors == 0 { continue; }
        out.push(alloc::format!(
            "{:>3}  {:<4}  0x{:02x}  {:>10}  {:>10}   {}",
            i + 1,
            if p.bootable { "yes" } else { "no" },
            p.kind,
            p.start_lba,
            p.sectors,
            p.type_name(),
        ));
    }
    out
}

// =====================================================================
// GPT
// =====================================================================

/// GPT header signature: ASCII "EFI PART" (8 bytes, little-endian).
pub const GPT_SIGNATURE: u64 = 0x5452_4150_2049_4645;

/// Parsed GPT header fields we care about.
#[derive(Debug, Clone, Copy)]
pub struct GptHeader {
    pub revision:           u32,
    pub header_size:        u32,
    pub header_crc32:       u32,
    pub current_lba:        u64,
    pub backup_lba:         u64,
    pub first_usable_lba:   u64,
    pub last_usable_lba:    u64,
    pub disk_guid:          [u8; 16],
    pub partition_entry_lba:    u64,
    pub num_partition_entries:  u32,
    pub size_of_partition_entry: u32,
    pub partition_array_crc32:  u32,
}

impl GptHeader {
    pub fn parse(buf: &[u8]) -> Result<Self, &'static str> {
        if buf.len() < 92 { return Err("short header"); }
        let sig = u64::from_le_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]);
        if sig != GPT_SIGNATURE { return Err("bad signature (not GPT)"); }

        let mut guid = [0u8; 16];
        guid.copy_from_slice(&buf[56..72]);

        Ok(GptHeader {
            revision:                u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            header_size:             u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            header_crc32:            u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            current_lba:             u64_le(&buf[24..32]),
            backup_lba:              u64_le(&buf[32..40]),
            first_usable_lba:        u64_le(&buf[40..48]),
            last_usable_lba:         u64_le(&buf[48..56]),
            disk_guid:               guid,
            partition_entry_lba:     u64_le(&buf[72..80]),
            num_partition_entries:   u32::from_le_bytes([buf[80], buf[81], buf[82], buf[83]]),
            size_of_partition_entry: u32::from_le_bytes([buf[84], buf[85], buf[86], buf[87]]),
            partition_array_crc32:   u32::from_le_bytes([buf[88], buf[89], buf[90], buf[91]]),
        })
    }

    /// Verify the header CRC32 (per UEFI 2.x).  The CRC32 field itself
    /// must be zeroed during the calculation; the rest of the header up
    /// to `header_size` is included.
    pub fn verify_crc(&self, raw: &[u8]) -> bool {
        if (raw.len() as u32) < self.header_size || self.header_size < 92 {
            return false;
        }
        let mut copy: alloc::vec::Vec<u8> = raw[..self.header_size as usize].to_vec();
        // Zero the CRC32 field at offset 16..20 before recomputing.
        copy[16..20].copy_from_slice(&[0u8; 4]);
        crc32(&copy) == self.header_crc32
    }
}

fn u64_le(s: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&s[..8]);
    u64::from_le_bytes(a)
}

/// One entry from the GPT partition array.
#[derive(Debug, Clone)]
pub struct GptPartition {
    pub type_guid:   [u8; 16],
    pub unique_guid: [u8; 16],
    pub start_lba:   u64,
    pub end_lba:     u64,
    pub attributes:  u64,
    pub name:        String,
}

impl GptPartition {
    pub fn parse(entry: &[u8]) -> Option<Self> {
        if entry.len() < 128 { return None; }
        // type_guid all zeros = unused entry.
        if entry[..16].iter().all(|&b| b == 0) { return None; }
        let mut tg = [0u8; 16]; tg.copy_from_slice(&entry[..16]);
        let mut ug = [0u8; 16]; ug.copy_from_slice(&entry[16..32]);
        // Name is UTF-16LE, up to 36 chars at offset 56..128 (72 bytes).
        let mut name = String::new();
        let mut i = 56;
        while i + 1 < 128 {
            let lo = entry[i];
            let hi = entry[i + 1];
            let ch = u16::from_le_bytes([lo, hi]);
            if ch == 0 { break; }
            // Append as char if BMP, else as '?'.
            if let Some(c) = char::from_u32(ch as u32) {
                name.push(c);
            }
            i += 2;
        }
        Some(GptPartition {
            type_guid:   tg,
            unique_guid: ug,
            start_lba:   u64_le(&entry[32..40]),
            end_lba:     u64_le(&entry[40..48]),
            attributes:  u64_le(&entry[48..56]),
            name,
        })
    }

    pub fn type_name(&self) -> &'static str {
        // A handful of well-known type GUIDs (UEFI Annex A).  Match by
        // raw bytes since GUIDs use mixed endianness.
        // EFI System Partition: c12a7328-f81f-11d2-ba4b-00a0c93ec93b
        const EFI_SYSTEM:    [u8; 16] = [0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11,
                                          0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b];
        // Linux filesystem:    0fc63daf-8483-4772-8e79-3d69d8477de4
        const LINUX_FS:      [u8; 16] = [0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47,
                                          0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4];
        // Linux swap:          0657fd6d-a4ab-43c4-84e5-0933c84b4f4f
        const LINUX_SWAP:    [u8; 16] = [0x6d, 0xfd, 0x57, 0x06, 0xab, 0xa4, 0xc4, 0x43,
                                          0x84, 0xe5, 0x09, 0x33, 0xc8, 0x4b, 0x4f, 0x4f];
        // Linux LVM:           e6d6d379-f507-44c2-a23c-238f2a3df928
        const LINUX_LVM:     [u8; 16] = [0x79, 0xd3, 0xd6, 0xe6, 0x07, 0xf5, 0xc2, 0x44,
                                          0xa2, 0x3c, 0x23, 0x8f, 0x2a, 0x3d, 0xf9, 0x28];
        // Microsoft Basic Data: ebd0a0a2-b9e5-4433-87c0-68b6b72699c7
        const MS_DATA:       [u8; 16] = [0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44,
                                          0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7];

        match self.type_guid {
            EFI_SYSTEM => "EFI System",
            LINUX_FS   => "Linux filesystem",
            LINUX_SWAP => "Linux swap",
            LINUX_LVM  => "Linux LVM",
            MS_DATA    => "Microsoft basic data",
            _          => "unknown",
        }
    }
}

/// Read the GPT header from the second LBA, then the partition array.
/// `header_lba_buf` is sector 1 (512 bytes).  `entries_buf` is the
/// partition array contents read from `partition_entry_lba`.
pub fn parse_gpt(header_lba_buf: &[u8], entries_buf: &[u8]) -> Result<(GptHeader, Vec<GptPartition>), &'static str> {
    let hdr = GptHeader::parse(header_lba_buf)?;
    if hdr.size_of_partition_entry < 128 { return Err("entry size too small"); }
    let stride = hdr.size_of_partition_entry as usize;
    let count = hdr.num_partition_entries as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let off = i * stride;
        if off + stride > entries_buf.len() { break; }
        if let Some(p) = GptPartition::parse(&entries_buf[off..off + stride]) {
            out.push(p);
        }
    }
    Ok((hdr, out))
}

/// CRC32 in IEEE 802.3 / zlib variant (the one UEFI uses).
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: [u32; 256] = build_crc_table();
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        let idx = ((crc ^ b as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[idx];
    }
    !crc
}

const fn build_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i: u32 = 0;
    while i < 256 {
        let mut c = i;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i as usize] = c;
        i += 1;
    }
    t
}

/// Format a GUID as the standard 8-4-4-4-12 hyphenated form (with the
/// first three groups in mixed endianness, per UEFI).
pub fn fmt_guid(g: &[u8; 16]) -> String {
    alloc::format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g[3], g[2], g[1], g[0],
        g[5], g[4],
        g[7], g[6],
        g[8], g[9],
        g[10], g[11], g[12], g[13], g[14], g[15],
    )
}

/// Pretty-print a GPT parse result.
pub fn format_gpt(hdr: &GptHeader, parts: &[GptPartition]) -> Vec<String> {
    let mut out = Vec::new();
    out.push(alloc::format!("Disk GUID: {}", fmt_guid(&hdr.disk_guid)));
    out.push(alloc::format!("First usable LBA: {}, last: {}, entries: {}",
        hdr.first_usable_lba, hdr.last_usable_lba, parts.len()));
    out.push(String::from("idx  start_lba    end_lba      type                  name"));
    for (i, p) in parts.iter().enumerate() {
        out.push(alloc::format!(
            "{:>3}  {:>10}  {:>10}  {:<22}  {}",
            i + 1, p.start_lba, p.end_lba, p.type_name(),
            if p.name.is_empty() { "-".to_string() } else { p.name.clone() },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vector() {
        // Standard zlib/PNG CRC32 vector: CRC32("123456789") = 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }
}
