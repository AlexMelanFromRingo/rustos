//! MBR (DOS) partition table parser.
//!
//! The MBR is the first 512-byte sector of a disk.  Bytes 0..446 are
//! bootstrap code, 446..510 are four 16-byte partition entries, and the
//! last two bytes are the 0x55AA boot signature.  We only parse the
//! partition table; bootstrap code is ignored.
//!
//! GPT (which lives at LBA 1 with a special "protective MBR" of type 0xEE
//! at LBA 0) is detected by looking at the first entry's type and is left
//! as a TODO.

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

/// Pretty-print a parse result.
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
