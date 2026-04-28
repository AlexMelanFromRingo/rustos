/// FAT32 filesystem implementation
///
/// This module provides read-only support for FAT32 filesystems on disk.
/// It implements the VFS FileSystem trait for integration with the virtual filesystem.

use alloc::vec::Vec;
use alloc::string::String;
use alloc::vec;
use core::mem;
use spin::Mutex;

use crate::drivers::ata::{ATA_DRIVE, SECTOR_SIZE};
use crate::fs::vfs::{FileSystem, FileInfo, VfsError, VfsResult};

/// FAT32 BIOS Parameter Block (Boot Sector)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct BiosParameterBlock {
    /// Jump instruction (3 bytes)
    jmp_boot: [u8; 3],
    /// OEM identifier (8 bytes)
    oem_name: [u8; 8],
    /// Bytes per sector (usually 512)
    bytes_per_sector: u16,
    /// Sectors per cluster (must be power of 2)
    sectors_per_cluster: u8,
    /// Number of reserved sectors (including boot sector)
    reserved_sectors: u16,
    /// Number of FAT copies (usually 2)
    num_fats: u8,
    /// Maximum number of root directory entries (0 for FAT32)
    root_entry_count: u16,
    /// Total sectors (0 for FAT32, use total_sectors_32)
    total_sectors_16: u16,
    /// Media descriptor
    media: u8,
    /// Sectors per FAT (0 for FAT32, use fat_size_32)
    fat_size_16: u16,
    /// Sectors per track
    sectors_per_track: u16,
    /// Number of heads
    num_heads: u16,
    /// Hidden sectors
    hidden_sectors: u32,
    /// Total sectors (32-bit)
    total_sectors_32: u32,
}

/// FAT32 Extended Boot Record
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Fat32ExtendedBootRecord {
    /// Sectors per FAT (32-bit)
    fat_size_32: u32,
    /// FAT flags
    ext_flags: u16,
    /// Filesystem version
    fs_version: u16,
    /// Root directory first cluster
    root_cluster: u32,
    /// FSInfo sector number
    fs_info: u16,
    /// Backup boot sector
    backup_boot_sector: u16,
    /// Reserved
    reserved: [u8; 12],
    /// Drive number
    drive_number: u8,
    /// Reserved
    reserved1: u8,
    /// Boot signature
    boot_signature: u8,
    /// Volume ID
    volume_id: u32,
    /// Volume label
    volume_label: [u8; 11],
    /// Filesystem type (should be "FAT32   ")
    fs_type: [u8; 8],
}

/// Complete FAT32 boot sector
#[repr(C, packed)]
/// Complete FAT32 boot sector structure (reserved for future use)
#[allow(dead_code)]
struct Fat32BootSector {
    bpb: BiosParameterBlock,
    ebr: Fat32ExtendedBootRecord,
    /// Boot code
    boot_code: [u8; 420],
    /// Boot signature (0x55AA)
    signature: u16,
}

/// FAT32 directory entry attributes
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

/// FAT32 directory entry (32 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct DirectoryEntry {
    /// Short filename (8.3 format)
    name: [u8; 11],
    /// File attributes
    attributes: u8,
    /// Reserved for Windows NT
    nt_reserved: u8,
    /// Creation time (tenths of second)
    creation_time_tenths: u8,
    /// Creation time
    creation_time: u16,
    /// Creation date
    creation_date: u16,
    /// Last access date
    last_access_date: u16,
    /// High word of first cluster
    first_cluster_high: u16,
    /// Last modification time
    write_time: u16,
    /// Last modification date
    write_date: u16,
    /// Low word of first cluster
    first_cluster_low: u16,
    /// File size in bytes
    file_size: u32,
}

impl DirectoryEntry {
    /// Get the first cluster of this entry
    fn first_cluster(&self) -> u32 {
        ((self.first_cluster_high as u32) << 16) | (self.first_cluster_low as u32)
    }

    /// Check if this is a directory
    fn is_directory(&self) -> bool {
        (self.attributes & ATTR_DIRECTORY) != 0
    }

    /// Check if this is a volume label
    fn is_volume_label(&self) -> bool {
        (self.attributes & ATTR_VOLUME_ID) != 0
    }

    /// Check if this is a long file name entry
    fn is_long_name(&self) -> bool {
        (self.attributes & ATTR_LONG_NAME) == ATTR_LONG_NAME
    }

    /// Check if this entry is free
    fn is_free(&self) -> bool {
        self.name[0] == 0xE5 || self.name[0] == 0x00
    }

    /// Check if this is the last entry
    fn is_last(&self) -> bool {
        self.name[0] == 0x00
    }

    /// Get the short filename as a string
    fn get_name(&self) -> String {
        let mut name = String::new();

        // Add name part (up to 8 chars, trimming spaces)
        for i in 0..8 {
            if self.name[i] == b' ' {
                break;
            }
            name.push(self.name[i] as char);
        }

        // Add extension if present
        if self.name[8] != b' ' {
            name.push('.');
            for i in 8..11 {
                if self.name[i] == b' ' {
                    break;
                }
                name.push(self.name[i] as char);
            }
        }

        name
    }
}

/// FAT32 filesystem instance
pub struct Fat32 {
    /// First data sector
    first_data_sector: u32,
    /// Sectors per cluster
    sectors_per_cluster: u32,
    /// Bytes per sector
    bytes_per_sector: u32,
    /// First FAT sector
    first_fat_sector: u32,
    /// Root directory first cluster
    root_cluster: u32,
    /// Sectors per FAT
    sectors_per_fat: u32,
    /// Total sectors on the volume
    total_sectors: u32,
    /// Total clusters (data area)
    total_clusters: u32,
    /// Number of FAT copies (BPB num_fats; usually 2).  We mirror writes
    /// across all of them so a recovery tool sees consistent data.
    num_fats: u8,
    /// FSInfo sector LBA (BPB fs_info; usually 1).
    fs_info_sector: u32,
}

impl Fat32 {
    /// Create a new FAT32 filesystem from the primary ATA drive
    pub fn new() -> Result<Self, &'static str> {
        let mut drive = ATA_DRIVE.lock();

        // Check if drive exists before attempting to read
        if !drive.exists() {
            return Err("No ATA drive detected");
        }

        // IMPORTANT: Small delay after IDENTIFY before first READ
        // The drive needs time to reset after IDENTIFY command
        for _ in 0..1000 {
            core::hint::spin_loop();
        }

        // Read boot sector
        let mut boot_sector = [0u8; SECTOR_SIZE];
        drive.read_sector(0, &mut boot_sector)?;

        drop(drive);

        // Parse boot sector
        let bpb = unsafe {
            &*(boot_sector.as_ptr() as *const BiosParameterBlock)
        };

        let ebr = unsafe {
            &*((boot_sector.as_ptr() as usize + mem::size_of::<BiosParameterBlock>()) as *const Fat32ExtendedBootRecord)
        };

        // Verify FAT32 signature
        let signature = u16::from_le_bytes([boot_sector[510], boot_sector[511]]);
        if signature != 0xAA55 {
            return Err("Invalid boot sector signature");
        }

        // Calculate first data sector
        let root_dir_sectors = ((bpb.root_entry_count as u32 * 32) + (bpb.bytes_per_sector as u32 - 1)) / bpb.bytes_per_sector as u32;
        let fat_size = if bpb.fat_size_16 != 0 {
            bpb.fat_size_16 as u32
        } else {
            ebr.fat_size_32
        };

        let first_data_sector = bpb.reserved_sectors as u32 + (bpb.num_fats as u32 * fat_size) + root_dir_sectors;
        let first_fat_sector = bpb.reserved_sectors as u32;

        // Get total sectors
        let total_sectors = if bpb.total_sectors_16 != 0 {
            bpb.total_sectors_16 as u32
        } else {
            bpb.total_sectors_32
        };

        // Calculate total clusters in data area
        let data_sectors = total_sectors - first_data_sector;
        let total_clusters = data_sectors / (bpb.sectors_per_cluster as u32);

        // Filesystem is valid and initialized

        Ok(Fat32 {
            first_data_sector,
            sectors_per_cluster: bpb.sectors_per_cluster as u32,
            bytes_per_sector: bpb.bytes_per_sector as u32,
            first_fat_sector,
            root_cluster: ebr.root_cluster,
            sectors_per_fat: fat_size,
            total_sectors,
            total_clusters,
            num_fats: bpb.num_fats,
            fs_info_sector: ebr.fs_info as u32,
        })
    }

    /// Get the sector number for a given cluster
    fn cluster_to_sector(&self, cluster: u32) -> u32 {
        ((cluster - 2) * self.sectors_per_cluster) + self.first_data_sector
    }

    /// Read a cluster from disk
    fn read_cluster(&self, cluster: u32) -> Result<Vec<u8>, &'static str> {
        let sector = self.cluster_to_sector(cluster);
        let cluster_size = (self.sectors_per_cluster * self.bytes_per_sector) as usize;
        let mut buffer = vec![0u8; cluster_size];

        let mut drive = ATA_DRIVE.lock();
        drive.read_sectors(sector, self.sectors_per_cluster as usize, &mut buffer)?;
        drop(drive);

        Ok(buffer)
    }

    /// Write a cluster to disk
    fn write_cluster(&mut self, cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        let sector = self.cluster_to_sector(cluster);
        let cluster_size = (self.sectors_per_cluster * self.bytes_per_sector) as usize;

        if data.len() > cluster_size {
            return Err("Data exceeds cluster size");
        }

        // Prepare buffer (pad with zeros if needed)
        let mut buffer = vec![0u8; cluster_size];
        buffer[..data.len()].copy_from_slice(data);

        let mut drive = ATA_DRIVE.lock();
        drive.write_sectors(sector, self.sectors_per_cluster as usize, &buffer)?;
        drop(drive);

        Ok(())
    }

    /// Write data to a cluster chain
    fn write_cluster_chain(&mut self, start_cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        let cluster_size = (self.sectors_per_cluster * self.bytes_per_sector) as usize;
        let mut offset = 0;
        let mut current_cluster = start_cluster;

        while offset < data.len() {
            let chunk_size = core::cmp::min(cluster_size, data.len() - offset);
            let chunk = &data[offset..offset + chunk_size];

            self.write_cluster(current_cluster, chunk)?;

            offset += chunk_size;

            if offset < data.len() {
                // Need next cluster
                match self.get_next_cluster(current_cluster)? {
                    Some(next) => current_cluster = next,
                    None => return Err("Cluster chain too short for data"),
                }
            }
        }

        Ok(())
    }

    /// Get the next cluster in the FAT chain
    fn get_next_cluster(&self, cluster: u32) -> Result<Option<u32>, &'static str> {
        // Calculate FAT entry offset
        let fat_offset = cluster * 4;
        let fat_sector = self.first_fat_sector + (fat_offset / self.bytes_per_sector);
        let entry_offset = (fat_offset % self.bytes_per_sector) as usize;

        // Read FAT sector
        let mut sector_buffer = [0u8; SECTOR_SIZE];
        let mut drive = ATA_DRIVE.lock();
        drive.read_sector(fat_sector, &mut sector_buffer)?;
        drop(drive);

        // Get FAT entry (mask off top 4 bits)
        let next_cluster = u32::from_le_bytes([
            sector_buffer[entry_offset],
            sector_buffer[entry_offset + 1],
            sector_buffer[entry_offset + 2],
            sector_buffer[entry_offset + 3],
        ]) & 0x0FFFFFFF;

        // Check for end of chain
        if next_cluster >= 0x0FFFFFF8 {
            Ok(None)
        } else if next_cluster < 2 {
            Err("Invalid cluster number in FAT")
        } else {
            Ok(Some(next_cluster))
        }
    }

    /// Read all clusters in a chain
    fn read_cluster_chain(&self, start_cluster: u32) -> Result<Vec<u8>, &'static str> {
        let mut data = Vec::new();
        let mut current_cluster = start_cluster;

        loop {
            let cluster_data = self.read_cluster(current_cluster)?;
            data.extend_from_slice(&cluster_data);

            match self.get_next_cluster(current_cluster)? {
                Some(next) => current_cluster = next,
                None => break,
            }
        }

        Ok(data)
    }

    /// Read directory entries from a cluster
    fn read_directory(&self, cluster: u32) -> Result<Vec<DirectoryEntry>, &'static str> {
        // Same as read_directory_with_lfn but discards the long-name
        // strings; existing callers that index by short name keep working.
        Ok(self.read_directory_with_lfn(cluster)?
            .into_iter()
            .map(|(e, _)| e)
            .collect())
    }

    /// Walk a directory's data, returning each short-name `DirectoryEntry`
    /// paired with its decoded VFAT long filename (if any).  Implements
    /// the on-disk LFN format (Microsoft FAT 3.5+):
    ///   * LFN entries precede the short entry in reverse order
    ///   * each LFN slot holds 13 UTF-16LE chars at offsets 1..11, 14..26, 28..32
    ///   * sequence number in byte 0 with bit 6 marking the *first*
    ///     written (i.e. the *last* logically) entry; bits 0..4 are the
    ///     position counted from 1
    ///   * checksum at byte 13 must match the SFN's checksum
    fn read_directory_with_lfn(&self, cluster: u32)
        -> Result<Vec<(DirectoryEntry, Option<String>)>, &'static str>
    {
        let data = self.read_cluster_chain(cluster)?;
        let entry_size = mem::size_of::<DirectoryEntry>();
        let num_entries = data.len() / entry_size;

        let mut out = Vec::new();
        // Slot accumulator: chunks of 13 UTF-16 chars indexed by ord-1.
        let mut lfn_parts: alloc::vec::Vec<(u8, [u16; 13])> = alloc::vec::Vec::new();
        let mut lfn_checksum: u8 = 0;

        for i in 0..num_entries {
            let offset = i * entry_size;
            let raw = &data[offset..offset + entry_size];
            let entry = unsafe { &*(raw.as_ptr() as *const DirectoryEntry) };

            if entry.is_last() { break; }
            if entry.is_free() {
                lfn_parts.clear();
                continue;
            }
            if entry.is_long_name() {
                // Re-interpret the slot as an LFN entry.
                let ord = raw[0];
                let chk = raw[13];
                let mut chars = [0u16; 13];
                // chars[0..5] at offsets 1..11
                for j in 0..5 {
                    chars[j] = u16::from_le_bytes([raw[1 + j * 2], raw[2 + j * 2]]);
                }
                // chars[5..11] at offsets 14..26
                for j in 0..6 {
                    chars[5 + j] = u16::from_le_bytes([raw[14 + j * 2], raw[15 + j * 2]]);
                }
                // chars[11..13] at offsets 28..32
                for j in 0..2 {
                    chars[11 + j] = u16::from_le_bytes([raw[28 + j * 2], raw[29 + j * 2]]);
                }
                let pos = (ord & 0x1F).saturating_sub(1);
                lfn_parts.push((pos, chars));
                lfn_checksum = chk;
                continue;
            }
            if entry.is_volume_label() {
                lfn_parts.clear();
                continue;
            }

            // We're at a short-name entry.  Try to assemble the LFN chain
            // collected for it.
            let lfn = if !lfn_parts.is_empty() {
                let want_chk = lfn_checksum;
                if Self::sfn_checksum(&entry.name) == want_chk {
                    lfn_parts.sort_by_key(|(ord, _)| *ord);
                    let mut s = String::new();
                    for (_, chunk) in &lfn_parts {
                        for &ch in chunk.iter() {
                            if ch == 0 || ch == 0xFFFF { break; }
                            if let Some(c) = char::from_u32(ch as u32) { s.push(c); }
                        }
                    }
                    if s.is_empty() { None } else { Some(s) }
                } else {
                    None
                }
            } else {
                None
            };

            out.push((*entry, lfn));
            lfn_parts.clear();
        }
        Ok(out)
    }

    /// Microsoft VFAT short-name checksum (Microsoft FAT spec §7.2):
    /// rotate-right + add for each of the 11 SFN bytes.
    fn sfn_checksum(name: &[u8; 11]) -> u8 {
        let mut sum: u8 = 0;
        for &b in name.iter() {
            sum = (sum.rotate_right(1)).wrapping_add(b);
        }
        sum
    }

    /// Find a file in a directory
    fn find_file_in_directory(&self, dir_cluster: u32, filename: &str) -> Result<Option<DirectoryEntry>, &'static str> {
        let entries = self.read_directory_with_lfn(dir_cluster)?;
        let search_upper = filename.to_uppercase();

        for (entry, lfn) in entries {
            // Match by long filename (case-sensitive, then case-insensitive)
            if let Some(name) = &lfn {
                if name == filename || name.to_uppercase() == search_upper {
                    return Ok(Some(entry));
                }
            }
            // Match by short filename (always case-insensitive)
            if entry.get_name().to_uppercase() == search_upper {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Find a file by path
    fn find_file(&self, path: &str) -> Result<Option<DirectoryEntry>, &'static str> {
        let path = path.trim_start_matches('/');

        if path.is_empty() {
            // Return root directory entry (create a pseudo-entry)
            return Ok(None);
        }

        // Parse path into components
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return Ok(None);
        }

        // Navigate to the parent directory
        let mut current_cluster = self.root_cluster;

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;

            if let Some(entry) = self.find_file_in_directory(current_cluster, component)? {
                if is_last {
                    // Found the target file/directory
                    return Ok(Some(entry));
                } else {
                    // This should be a directory, navigate into it
                    if entry.is_directory() {
                        current_cluster = entry.first_cluster();
                    } else {
                        return Err("Not a directory in path");
                    }
                }
            } else {
                // File/directory not found in path
                return Ok(None);
            }
        }

        Ok(None)
    }

    /// Find a directory by path and return its cluster
    fn find_directory_cluster(&self, path: &str) -> Result<u32, &'static str> {
        let path = path.trim_start_matches('/');

        if path.is_empty() {
            return Ok(self.root_cluster);
        }

        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_cluster = self.root_cluster;

        for component in components {
            if let Some(entry) = self.find_file_in_directory(current_cluster, component)? {
                if entry.is_directory() {
                    current_cluster = entry.first_cluster();
                } else {
                    return Err("Not a directory");
                }
            } else {
                return Err("Directory not found");
            }
        }

        Ok(current_cluster)
    }

    /// Split path into parent directory and filename
    fn split_path(path: &str) -> (&str, &str) {
        let path = path.trim_start_matches('/');

        if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        }
    }

    /// Find a free cluster in the FAT table
    /// Returns the cluster number of the first free cluster found
    fn find_free_cluster(&self) -> Result<Option<u32>, &'static str> {
        // Start from cluster 2 (first data cluster)
        let start_cluster = 2;
        let end_cluster = self.total_clusters + 2;

        for cluster in start_cluster..end_cluster {
            // Calculate which sector contains this cluster's FAT entry
            let fat_offset = cluster * 4;
            let fat_sector = self.first_fat_sector + (fat_offset / self.bytes_per_sector);
            let entry_offset = (fat_offset % self.bytes_per_sector) as usize;

            // Read FAT sector
            let mut sector_buffer = [0u8; SECTOR_SIZE];
            let mut drive = ATA_DRIVE.lock();
            drive.read_sector(fat_sector, &mut sector_buffer)?;
            drop(drive);

            let fat_entry = u32::from_le_bytes([
                sector_buffer[entry_offset],
                sector_buffer[entry_offset + 1],
                sector_buffer[entry_offset + 2],
                sector_buffer[entry_offset + 3],
            ]) & 0x0FFFFFFF;

            // Check if cluster is free
            if fat_entry == 0x00000000 {
                return Ok(Some(cluster));
            }
        }

        // No free clusters found
        Ok(None)
    }

    /// Write a FAT entry for a given cluster.  Mirrors the update across
    /// every FAT copy declared in the BPB (Microsoft FAT spec §4: all
    /// FATs MUST be kept identical so chkdsk-class tools can use any of
    /// them as a recovery source).
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), &'static str> {
        let fat_offset = cluster * 4;
        let entry_offset = (fat_offset % self.bytes_per_sector) as usize;
        let masked_value = value & 0x0FFFFFFF;

        let mut drive = ATA_DRIVE.lock();
        for fat_idx in 0..self.num_fats as u32 {
            let fat_base = self.first_fat_sector + fat_idx * self.sectors_per_fat;
            let fat_sector = fat_base + (fat_offset / self.bytes_per_sector);
            let mut sector_buffer = [0u8; SECTOR_SIZE];
            drive.read_sector(fat_sector, &mut sector_buffer)?;

            // Preserve the top 4 reserved bits of the existing entry.
            let existing = u32::from_le_bytes([
                sector_buffer[entry_offset],
                sector_buffer[entry_offset + 1],
                sector_buffer[entry_offset + 2],
                sector_buffer[entry_offset + 3],
            ]);
            let merged = (existing & 0xF000_0000) | masked_value;
            sector_buffer[entry_offset..entry_offset + 4].copy_from_slice(&merged.to_le_bytes());

            drive.write_sector(fat_sector, &sector_buffer)?;
        }
        drop(drive);

        Ok(())
    }

    /// FSInfo (Microsoft FAT spec §5.1) is a 512-byte sector with two
    /// hint fields: free cluster count and last-allocated cluster.  Update
    /// both so OSes that use the hint converge faster.  Signatures live
    /// at offsets 0, 484, 508; we recompute neither — only the data.
    fn update_fsinfo(&mut self, free_count_delta: i32, last_alloc_hint: u32) -> Result<(), &'static str> {
        if self.fs_info_sector == 0 || self.fs_info_sector == 0xFFFF {
            return Ok(()); // FSInfo not present
        }
        let mut drive = ATA_DRIVE.lock();
        let mut buf = [0u8; SECTOR_SIZE];
        drive.read_sector(self.fs_info_sector, &mut buf)?;
        // Verify lead signature 0x41615252 at offset 0.
        let sig = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if sig != 0x41615252 { return Ok(()); }

        // Free cluster count at offset 488; last-allocated at 492.  0xFFFFFFFF
        // means "unknown" — leave alone if so when only adjusting by delta.
        let cur_free = u32::from_le_bytes([buf[488], buf[489], buf[490], buf[491]]);
        if cur_free != 0xFFFF_FFFF {
            let new_free = if free_count_delta < 0 {
                cur_free.saturating_sub((-free_count_delta) as u32)
            } else {
                cur_free.saturating_add(free_count_delta as u32)
            };
            buf[488..492].copy_from_slice(&new_free.to_le_bytes());
        }
        if last_alloc_hint != 0 {
            buf[492..496].copy_from_slice(&last_alloc_hint.to_le_bytes());
        }
        drive.write_sector(self.fs_info_sector, &buf)?;
        Ok(())
    }

    /// Allocate a cluster chain for a file.  Returns the first cluster
    /// number on success.  On any allocation failure mid-chain the partial
    /// chain is rolled back (every cluster we'd already linked is freed)
    /// before returning the error, so the FAT never contains an orphan
    /// chain.
    fn allocate_clusters(&mut self, num_clusters: u32) -> Result<u32, &'static str> {
        if num_clusters == 0 {
            return Err("Cannot allocate zero clusters");
        }

        let first_cluster = match self.find_free_cluster()? {
            Some(cluster) => cluster,
            None => return Err("No free space available"),
        };

        // Reserve the first cluster immediately so a subsequent
        // find_free_cluster doesn't hand it out again.
        self.write_fat_entry(first_cluster, 0x0FFFFFFF)?;
        let mut chain: alloc::vec::Vec<u32> = alloc::vec![first_cluster];

        for _ in 1..num_clusters {
            let next_cluster = match self.find_free_cluster()? {
                Some(c) => c,
                None => {
                    // Roll back: free every cluster we'd already grabbed.
                    for &c in &chain {
                        let _ = self.write_fat_entry(c, 0x00000000);
                    }
                    return Err("Insufficient space");
                }
            };
            // Link previous → next in the FAT.
            let prev = *chain.last().unwrap();
            if let Err(e) = self.write_fat_entry(prev, next_cluster) {
                for &c in &chain {
                    let _ = self.write_fat_entry(c, 0x00000000);
                }
                return Err(e);
            }
            // Reserve next so we don't re-find it.
            if let Err(e) = self.write_fat_entry(next_cluster, 0x0FFFFFFF) {
                for &c in &chain {
                    let _ = self.write_fat_entry(c, 0x00000000);
                }
                return Err(e);
            }
            chain.push(next_cluster);
        }

        // Last cluster is already marked end-of-chain by the reservation.
        Ok(first_cluster)
    }

    /// Free a cluster chain
    fn free_cluster_chain(&mut self, start_cluster: u32) -> Result<(), &'static str> {
        let mut current_cluster = start_cluster;
        let mut freed = 0i32;
        loop {
            let next_cluster = self.get_next_cluster(current_cluster)?;
            self.write_fat_entry(current_cluster, 0x00000000)?;
            freed += 1;
            match next_cluster {
                Some(next) => current_cluster = next,
                None => break,
            }
        }
        // Update FSInfo's free-cluster hint.
        let _ = self.update_fsinfo(freed, 0);
        Ok(())
    }

    /// Truncate a cluster chain to `keep_clusters` clusters: free everything
    /// past that point and patch the FAT to mark `keep_clusters - 1` as
    /// end-of-chain.  Returns the number of clusters freed.
    pub fn truncate_chain(&mut self, start_cluster: u32, keep_clusters: u32)
        -> Result<u32, &'static str>
    {
        if keep_clusters == 0 {
            // Free entire chain.
            self.free_cluster_chain(start_cluster)?;
            return Ok(u32::MAX); // unknown count returned to caller
        }

        // Walk the chain to find the cluster at index keep_clusters-1.
        let mut current = start_cluster;
        let mut idx = 0u32;
        while idx + 1 < keep_clusters {
            let next = self.get_next_cluster(current)?
                .ok_or("chain shorter than keep_clusters")?;
            current = next;
            idx += 1;
        }

        // Tail starts at the cluster *after* `current`.
        let tail = self.get_next_cluster(current)?;
        // Mark our cluster as the new end-of-chain.
        self.write_fat_entry(current, 0x0FFFFFFF)?;

        // Free the tail.
        if let Some(t) = tail {
            let mut freed = 0u32;
            let mut cur = t;
            loop {
                let n = self.get_next_cluster(cur)?;
                self.write_fat_entry(cur, 0x00000000)?;
                freed += 1;
                match n { Some(nn) => cur = nn, None => break }
            }
            let _ = self.update_fsinfo(freed as i32, 0);
            Ok(freed)
        } else {
            Ok(0)
        }
    }

    /// Create a new file entry in root directory
    fn create_file_entry(&mut self, filename: &str, first_cluster: u32, file_size: u32) -> Result<(), &'static str> {
        // Convert filename to 8.3 format
        let short_name = self.to_short_name(filename)?;

        // Create directory entry
        let entry = DirectoryEntry {
            name: short_name,
            attributes: ATTR_ARCHIVE,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((first_cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (first_cluster & 0xFFFF) as u16,
            file_size,
        };

        // Find free slot in root directory
        self.write_directory_entry_to_root(&entry)
    }

    /// Create a directory entry in root directory
    fn create_dir_entry(&mut self, dirname: &str, first_cluster: u32) -> Result<(), &'static str> {
        let short_name = self.to_short_name(dirname)?;

        let entry = DirectoryEntry {
            name: short_name,
            attributes: ATTR_DIRECTORY,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((first_cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (first_cluster & 0xFFFF) as u16,
            file_size: 0,
        };

        self.write_directory_entry_to_root(&entry)
    }

    /// Update an existing directory entry
    fn update_directory_entry(&mut self, filename: &str, entry: &DirectoryEntry) -> Result<(), &'static str> {
        let short_name = self.to_short_name(filename)?;

        // Read root directory
        let data = self.read_cluster_chain(self.root_cluster)?;
        let entry_size = mem::size_of::<DirectoryEntry>();
        let num_entries = data.len() / entry_size;

        for i in 0..num_entries {
            let offset = i * entry_size;
            let existing_entry = unsafe {
                &*(data.as_ptr().add(offset) as *const DirectoryEntry)
            };

            if !existing_entry.is_free() && !existing_entry.is_long_name() {
                if existing_entry.name == short_name {
                    // Found it - update this entry
                    let mut new_data = data.clone();
                    let entry_bytes = unsafe {
                        core::slice::from_raw_parts(
                            entry as *const DirectoryEntry as *const u8,
                            entry_size
                        )
                    };
                    new_data[offset..offset + entry_size].copy_from_slice(entry_bytes);

                    // Write back
                    return self.write_cluster_chain(self.root_cluster, &new_data);
                }
            }
        }

        Err("File not found in directory")
    }

    /// Delete a directory entry (mark as deleted)
    fn delete_directory_entry(&mut self, filename: &str) -> Result<(), &'static str> {
        let short_name = self.to_short_name(filename)?;

        // Read root directory
        let data = self.read_cluster_chain(self.root_cluster)?;
        let entry_size = mem::size_of::<DirectoryEntry>();
        let num_entries = data.len() / entry_size;

        for i in 0..num_entries {
            let offset = i * entry_size;
            let existing_entry = unsafe {
                &*(data.as_ptr().add(offset) as *const DirectoryEntry)
            };

            if !existing_entry.is_free() && !existing_entry.is_long_name() {
                if existing_entry.name == short_name {
                    // Found it - mark as deleted
                    let mut new_data = data.clone();
                    new_data[offset] = 0xE5; // Deleted marker

                    // Write back
                    return self.write_cluster_chain(self.root_cluster, &new_data);
                }
            }
        }

        Err("File not found in directory")
    }

    /// Write directory entry to first free slot in root directory
    fn write_directory_entry_to_root(&mut self, entry: &DirectoryEntry) -> Result<(), &'static str> {
        // Read root directory
        let data = self.read_cluster_chain(self.root_cluster)?;
        let entry_size = mem::size_of::<DirectoryEntry>();
        let num_entries = data.len() / entry_size;

        for i in 0..num_entries {
            let offset = i * entry_size;
            let existing_entry = unsafe {
                &*(data.as_ptr().add(offset) as *const DirectoryEntry)
            };

            if existing_entry.is_free() || existing_entry.is_last() {
                // Found free slot
                let mut new_data = data.clone();
                let entry_bytes = unsafe {
                    core::slice::from_raw_parts(
                        entry as *const DirectoryEntry as *const u8,
                        entry_size
                    )
                };
                new_data[offset..offset + entry_size].copy_from_slice(entry_bytes);

                // Write back
                return self.write_cluster_chain(self.root_cluster, &new_data);
            }
        }

        Err("No free directory entries")
    }

    /// Convert filename to 8.3 format
    fn to_short_name(&self, filename: &str) -> Result<[u8; 11], &'static str> {
        let mut short_name = [b' '; 11];

        if let Some(dot_pos) = filename.rfind('.') {
            // Has extension
            let name = &filename[..dot_pos];
            let ext = &filename[dot_pos + 1..];

            if name.len() > 8 || ext.len() > 3 {
                return Err("Filename too long for 8.3 format");
            }

            for (i, c) in name.bytes().enumerate() {
                short_name[i] = c.to_ascii_uppercase();
            }
            for (i, c) in ext.bytes().enumerate() {
                short_name[8 + i] = c.to_ascii_uppercase();
            }
        } else {
            // No extension
            if filename.len() > 8 {
                return Err("Filename too long for 8.3 format");
            }

            for (i, c) in filename.bytes().enumerate() {
                short_name[i] = c.to_ascii_uppercase();
            }
        }

        Ok(short_name)
    }

    /// Count used clusters by traversing the FAT table
    /// This is relatively slow but accurate
    fn count_used_clusters(&self) -> Result<u32, &'static str> {
        let mut used_count = 0u32;
        let entries_per_sector = self.bytes_per_sector / 4; // Each FAT32 entry is 4 bytes

        // Read FAT table sector by sector
        for sector_offset in 0..self.sectors_per_fat {
            let fat_sector = self.first_fat_sector + sector_offset;
            let mut sector_buffer = [0u8; SECTOR_SIZE];

            let mut drive = ATA_DRIVE.lock();
            drive.read_sector(fat_sector, &mut sector_buffer)?;
            drop(drive);

            // Check each FAT entry in this sector
            for entry_idx in 0..entries_per_sector {
                let offset = (entry_idx * 4) as usize;
                if offset + 4 > SECTOR_SIZE {
                    break;
                }

                let fat_entry = u32::from_le_bytes([
                    sector_buffer[offset],
                    sector_buffer[offset + 1],
                    sector_buffer[offset + 2],
                    sector_buffer[offset + 3],
                ]) & 0x0FFFFFFF; // Mask off top 4 bits

                // Check if cluster is in use
                // 0x00000000 = free
                // 0x00000001 = reserved
                // 0x00000002 - 0x0FFFFFEF = used (data)
                // 0x0FFFFFF0 - 0x0FFFFFF6 = reserved
                // 0x0FFFFFF7 = bad cluster
                // 0x0FFFFFF8 - 0x0FFFFFFF = end of chain
                if fat_entry >= 0x00000002 && fat_entry <= 0x0FFFFFFF {
                    used_count += 1;
                }
            }
        }

        Ok(used_count)
    }

    /// Create a new directory
    pub fn create_directory(&mut self, path: &str) -> Result<(), &'static str> {
        let path = path.trim_start_matches('/');

        if path.is_empty() {
            return Err("Cannot create root directory");
        }

        // Split path into parent and dirname
        let (parent_path, dirname) = Self::split_path(path);

        // Find parent directory cluster
        let parent_cluster = if parent_path.is_empty() {
            self.root_cluster
        } else {
            self.find_directory_cluster(parent_path)?
        };

        // Check if directory already exists
        if self.find_file_in_directory(parent_cluster, dirname)?.is_some() {
            return Err("Directory already exists");
        }

        // Allocate one cluster for the new directory
        let dir_cluster = self.allocate_clusters(1)?;

        // Initialize directory with . and .. entries
        self.initialize_directory(dir_cluster, parent_cluster)?;

        // Create directory entry in parent
        let short_name = self.to_short_name(dirname)?;
        let entry = DirectoryEntry {
            name: short_name,
            attributes: ATTR_DIRECTORY,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((dir_cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (dir_cluster & 0xFFFF) as u16,
            file_size: 0,  // Directories have size 0
        };

        self.add_directory_entry(parent_cluster, &entry)?;

        Ok(())
    }

    /// Initialize a new directory with . and .. entries
    fn initialize_directory(&mut self, dir_cluster: u32, parent_cluster: u32) -> Result<(), &'static str> {
        // Create . (current directory) entry
        let dot_entry = DirectoryEntry {
            name: [b'.', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' '],
            attributes: ATTR_DIRECTORY,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((dir_cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (dir_cluster & 0xFFFF) as u16,
            file_size: 0,
        };

        // Create .. (parent directory) entry
        let dotdot_entry = DirectoryEntry {
            name: [b'.', b'.', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' '],
            attributes: ATTR_DIRECTORY,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((parent_cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (parent_cluster & 0xFFFF) as u16,
            file_size: 0,
        };

        // Write . and .. entries to the directory cluster
        let cluster_size = (self.sectors_per_cluster * self.bytes_per_sector) as usize;
        let mut cluster_data = vec![0u8; cluster_size];

        // Copy dot entry
        unsafe {
            let dot_bytes = core::slice::from_raw_parts(
                &dot_entry as *const DirectoryEntry as *const u8,
                core::mem::size_of::<DirectoryEntry>(),
            );
            cluster_data[0..32].copy_from_slice(dot_bytes);
        }

        // Copy dotdot entry
        unsafe {
            let dotdot_bytes = core::slice::from_raw_parts(
                &dotdot_entry as *const DirectoryEntry as *const u8,
                core::mem::size_of::<DirectoryEntry>(),
            );
            cluster_data[32..64].copy_from_slice(dotdot_bytes);
        }

        // Fill rest with zeros (empty entries)
        // Already done since vec is initialized with zeros

        // Write cluster to disk
        self.write_cluster(dir_cluster, &cluster_data)?;

        Ok(())
    }

    /// Add a directory entry to a directory
    fn add_directory_entry(&mut self, dir_cluster: u32, entry: &DirectoryEntry) -> Result<(), &'static str> {
        // Read directory
        let cluster_data = self.read_cluster(dir_cluster)?;
        let entries_per_cluster = cluster_data.len() / 32;

        // Find first free entry
        for i in 0..entries_per_cluster {
            let offset = i * 32;
            let first_byte = cluster_data[offset];

            // 0x00 = never used, 0xE5 = deleted
            if first_byte == 0x00 || first_byte == 0xE5 {
                // Found free entry, write it
                let mut new_cluster_data = cluster_data.clone();

                unsafe {
                    let entry_bytes = core::slice::from_raw_parts(
                        entry as *const DirectoryEntry as *const u8,
                        core::mem::size_of::<DirectoryEntry>(),
                    );
                    new_cluster_data[offset..offset + 32].copy_from_slice(entry_bytes);
                }

                self.write_cluster(dir_cluster, &new_cluster_data)?;
                return Ok(());
            }
        }

        Err("Directory is full")
    }

    /// List directory contents
    pub fn list_directory(&self, path: &str) -> Result<Vec<FileInfo>, &'static str> {
        let path = path.trim_start_matches('/');

        // Find directory cluster
        let dir_cluster = if path.is_empty() {
            self.root_cluster
        } else {
            self.find_directory_cluster(path)?
        };

        // Read directory entries
        let entries = self.read_directory(dir_cluster)?;

        // Convert to FileInfo
        let mut files = Vec::new();
        for entry in entries {
            let name = entry.get_name();

            // Skip . and .. entries
            if name == "." || name == ".." {
                continue;
            }

            // Use special size for directories (show as 0 or could show <DIR>)
            let size = if entry.is_directory() {
                0
            } else {
                entry.file_size as usize
            };

            files.push(FileInfo::new(name, size));
        }

        Ok(files)
    }

    /// Remove an empty directory
    pub fn remove_directory(&mut self, path: &str) -> Result<(), &'static str> {
        let path = path.trim_start_matches('/');

        if path.is_empty() {
            return Err("Cannot remove root directory");
        }

        // Find directory entry
        match self.find_file(path)? {
            Some(entry) => {
                if !entry.is_directory() {
                    return Err("Not a directory");
                }

                // Check if directory is empty (only . and .. entries)
                let entries = self.read_directory(entry.first_cluster())?;
                if entries.len() > 2 {
                    return Err("Directory not empty");
                }

                // Free cluster chain
                if entry.first_cluster() != 0 {
                    self.free_cluster_chain(entry.first_cluster())?;
                }

                // Mark directory entry as deleted
                self.delete_directory_entry(path)?;

                Ok(())
            }
            None => Err("Directory not found"),
        }
    }
}

impl FileSystem for Fat32 {
    fn read(&self, path: &str) -> VfsResult<Vec<u8>> {
        match self.find_file(path) {
            Ok(Some(entry)) => {
                if entry.is_directory() {
                    return Err(VfsError::PermissionDenied);
                }

                let data = self.read_cluster_chain(entry.first_cluster())
                    .map_err(|_| VfsError::IoError)?;

                // Truncate to actual file size
                let file_size = entry.file_size as usize;
                Ok(data[..file_size].to_vec())
            }
            Ok(None) => Err(VfsError::FileNotFound),
            Err(_) => Err(VfsError::IoError),
        }
    }

    fn write(&mut self, path: &str, data: Vec<u8>) -> VfsResult<()> {
        let path = path.trim_start_matches('/');

        // Calculate required clusters
        let cluster_size = (self.sectors_per_cluster * self.bytes_per_sector) as usize;
        let num_clusters = ((data.len() + cluster_size - 1) / cluster_size) as u32;
        let num_clusters = core::cmp::max(num_clusters, 1); // At least one cluster

        // Check if file exists
        match self.find_file(path).map_err(|_| VfsError::IoError)? {
            Some(mut entry) => {
                // File exists - update it
                // Free old cluster chain
                if entry.first_cluster() != 0 {
                    self.free_cluster_chain(entry.first_cluster())
                        .map_err(|_| VfsError::IoError)?;
                }

                // Allocate new cluster chain
                let first_cluster = self.allocate_clusters(num_clusters)
                    .map_err(|_| VfsError::NoSpace)?;

                // Write data
                self.write_cluster_chain(first_cluster, &data)
                    .map_err(|_| VfsError::IoError)?;

                // Update directory entry
                entry.first_cluster_low = (first_cluster & 0xFFFF) as u16;
                entry.first_cluster_high = ((first_cluster >> 16) & 0xFFFF) as u16;
                entry.file_size = data.len() as u32;

                self.update_directory_entry(path, &entry)
                    .map_err(|_| VfsError::IoError)?;

                Ok(())
            }
            None => {
                // File doesn't exist - create it
                // Allocate cluster chain
                let first_cluster = self.allocate_clusters(num_clusters)
                    .map_err(|_| VfsError::NoSpace)?;

                // Write data
                self.write_cluster_chain(first_cluster, &data)
                    .map_err(|_| VfsError::IoError)?;

                // Create directory entry
                self.create_file_entry(path, first_cluster, data.len() as u32)
                    .map_err(|_| VfsError::IoError)?;

                Ok(())
            }
        }
    }

    fn delete(&mut self, path: &str) -> VfsResult<()> {
        let path = path.trim_start_matches('/');

        // Find the file
        match self.find_file(path).map_err(|_| VfsError::IoError)? {
            Some(entry) => {
                if entry.is_directory() {
                    return Err(VfsError::PermissionDenied);
                }

                // Free cluster chain
                if entry.first_cluster() != 0 {
                    self.free_cluster_chain(entry.first_cluster())
                        .map_err(|_| VfsError::IoError)?;
                }

                // Mark directory entry as deleted
                self.delete_directory_entry(path)
                    .map_err(|_| VfsError::IoError)?;

                Ok(())
            }
            None => Err(VfsError::FileNotFound),
        }
    }

    fn list(&self) -> Vec<FileInfo> {
        let mut files = Vec::new();

        match self.read_directory(self.root_cluster) {
            Ok(entries) => {
                for entry in entries {
                    if !entry.is_directory() {
                        files.push(FileInfo::new(
                            entry.get_name(),
                            entry.file_size as usize,
                        ));
                    }
                }
            }
            Err(_) => {}
        }

        files
    }

    fn exists(&self, path: &str) -> bool {
        matches!(self.find_file(path), Ok(Some(_)))
    }

    fn used_space(&self) -> usize {
        // Count used clusters by traversing FAT table
        match self.count_used_clusters() {
            Ok(used_clusters) => {
                // Calculate space: clusters × sectors_per_cluster × bytes_per_sector
                (used_clusters as usize) * (self.sectors_per_cluster as usize) * (self.bytes_per_sector as usize)
            }
            Err(_) => 0, // Return 0 on error
        }
    }

    fn total_space(&self) -> usize {
        // Total space is total_sectors × bytes_per_sector
        (self.total_sectors as usize) * (self.bytes_per_sector as usize)
    }

    fn mkdir(&mut self, path: &str) -> VfsResult<()> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Err(VfsError::FileExists); // Root always exists
        }

        // Check if already exists
        if self.find_file(path).ok().flatten().is_some() {
            return Err(VfsError::FileExists);
        }

        // Allocate one cluster for the directory
        let cluster = self.allocate_clusters(1)
            .map_err(|_| VfsError::NoSpace)?;

        // Initialize directory cluster with . and .. entries
        let cluster_size = (self.sectors_per_cluster as usize) * (self.bytes_per_sector as usize);
        let mut dir_data = alloc::vec![0u8; cluster_size];

        // Create "." entry
        let dot_entry = DirectoryEntry {
            name: *b".          ",
            attributes: ATTR_DIRECTORY,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (cluster & 0xFFFF) as u16,
            file_size: 0,
        };

        // Resolve the parent directory's cluster.  When the new directory
        // is at the root the spec calls for first_cluster=0 (Microsoft FAT
        // spec §6.5.3), otherwise it must point at the parent's first
        // cluster.  We compute that from the path.
        let (parent_path, _new_name) = Self::split_path(path);
        let parent_cluster = if parent_path.trim_start_matches('/').is_empty() {
            0
        } else {
            self.find_directory_cluster(parent_path)
                .unwrap_or(self.root_cluster)
        };
        let dotdot_entry = DirectoryEntry {
            name: *b"..         ",
            attributes: ATTR_DIRECTORY,
            nt_reserved: 0,
            creation_time_tenths: 0,
            creation_time: 0,
            creation_date: 0,
            last_access_date: 0,
            first_cluster_high: ((parent_cluster >> 16) & 0xFFFF) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: (parent_cluster & 0xFFFF) as u16,
            file_size: 0,
        };

        // Write . and .. entries
        let entry_size = core::mem::size_of::<DirectoryEntry>();
        let dot_bytes = unsafe {
            core::slice::from_raw_parts(&dot_entry as *const DirectoryEntry as *const u8, entry_size)
        };
        let dotdot_bytes = unsafe {
            core::slice::from_raw_parts(&dotdot_entry as *const DirectoryEntry as *const u8, entry_size)
        };
        dir_data[..entry_size].copy_from_slice(dot_bytes);
        dir_data[entry_size..entry_size * 2].copy_from_slice(dotdot_bytes);

        self.write_cluster_chain(cluster, &dir_data)
            .map_err(|_| VfsError::IoError)?;

        // Create directory entry in parent
        self.create_dir_entry(path, cluster)
            .map_err(|_| VfsError::IoError)?;

        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> VfsResult<()> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Err(VfsError::PermissionDenied);
        }

        match self.find_file(path).map_err(|_| VfsError::IoError)? {
            Some(entry) => {
                if !entry.is_directory() {
                    return Err(VfsError::NotADirectory);
                }

                // Check if directory is empty (only . and .. entries)
                let entries = self.read_directory(entry.first_cluster())
                    .map_err(|_| VfsError::IoError)?;
                let non_meta_entries = entries.iter().filter(|e| {
                    let name = e.get_name();
                    name != "." && name != ".."
                }).count();
                if non_meta_entries > 0 {
                    return Err(VfsError::DirectoryNotEmpty);
                }

                // Free cluster chain
                self.free_cluster_chain(entry.first_cluster())
                    .map_err(|_| VfsError::IoError)?;

                // Remove directory entry
                self.delete_directory_entry(path)
                    .map_err(|_| VfsError::IoError)?;

                Ok(())
            }
            None => Err(VfsError::FileNotFound),
        }
    }

    fn list_dir(&self, path: &str) -> VfsResult<Vec<FileInfo>> {
        let cluster = if path == "/" || path.is_empty() {
            self.root_cluster
        } else {
            let path = path.trim_start_matches('/');
            match self.find_file(path).map_err(|_| VfsError::IoError)? {
                Some(entry) if entry.is_directory() => entry.first_cluster(),
                Some(_) => return Err(VfsError::NotADirectory),
                None => return Err(VfsError::FileNotFound),
            }
        };

        let entries = self.read_directory(cluster)
            .map_err(|_| VfsError::IoError)?;

        let mut files = Vec::new();
        for entry in entries {
            let name = entry.get_name();
            if name == "." || name == ".." {
                continue;
            }
            let mut info = FileInfo::new(name, entry.file_size as usize);
            info.is_directory = entry.is_directory();
            files.push(info);
        }

        Ok(files)
    }

    fn is_directory(&self, path: &str) -> bool {
        if path == "/" || path.is_empty() {
            return true;
        }
        let path = path.trim_start_matches('/');
        match self.find_file(path) {
            Ok(Some(entry)) => entry.is_directory(),
            _ => false,
        }
    }

    fn rename(&mut self, _old_path: &str, _new_path: &str) -> VfsResult<()> {
        // FAT32 rename requires updating directory entries in-place
        // Not yet supported on FAT32
        Err(VfsError::PermissionDenied)
    }
}

/// Global FAT32 filesystem instance
pub static FAT32: Mutex<Option<Fat32>> = Mutex::new(None);

/// Initialize FAT32 filesystem
pub fn init() -> Result<(), &'static str> {
    match Fat32::new() {
        Ok(fs) => {
            *FAT32.lock() = Some(fs);
            Ok(())
        }
        Err(e) => Err(e)
    }
}
