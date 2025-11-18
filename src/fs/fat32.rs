/// FAT32 filesystem implementation
///
/// This module provides read-only support for FAT32 filesystems on disk.
/// It implements the VFS FileSystem trait for integration with the virtual filesystem.

use alloc::vec::Vec;
use alloc::string::{String, ToString};
use alloc::vec;
use core::mem;
use spin::Mutex;

use crate::drivers::ata::{AtaDrive, ATA_DRIVE, SECTOR_SIZE};
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
}

impl Fat32 {
    /// Create a new FAT32 filesystem from the primary ATA drive
    pub fn new() -> Result<Self, &'static str> {
        let mut drive = ATA_DRIVE.lock();

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

        Ok(Fat32 {
            first_data_sector,
            sectors_per_cluster: bpb.sectors_per_cluster as u32,
            bytes_per_sector: bpb.bytes_per_sector as u32,
            first_fat_sector,
            root_cluster: ebr.root_cluster,
            sectors_per_fat: fat_size,
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
        let data = self.read_cluster_chain(cluster)?;
        let entry_size = mem::size_of::<DirectoryEntry>();
        let num_entries = data.len() / entry_size;

        let mut entries = Vec::new();

        for i in 0..num_entries {
            let offset = i * entry_size;
            let entry = unsafe {
                &*(data.as_ptr().add(offset) as *const DirectoryEntry)
            };

            if entry.is_last() {
                break;
            }

            if !entry.is_free() && !entry.is_long_name() && !entry.is_volume_label() {
                entries.push(*entry);
            }
        }

        Ok(entries)
    }

    /// Find a file in a directory
    fn find_file_in_directory(&self, dir_cluster: u32, filename: &str) -> Result<Option<DirectoryEntry>, &'static str> {
        let entries = self.read_directory(dir_cluster)?;

        for entry in entries {
            let entry_name = entry.get_name().to_uppercase();
            let search_name = filename.to_uppercase();

            if entry_name == search_name {
                return Ok(Some(entry));
            }
        }

        Ok(None)
    }

    /// Find a file by path
    fn find_file(&self, path: &str) -> Result<Option<DirectoryEntry>, &'static str> {
        // For now, only support files in root directory
        let path = path.trim_start_matches('/');

        if path.contains('/') {
            return Err("Subdirectories not yet supported");
        }

        self.find_file_in_directory(self.root_cluster, path)
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

    fn write(&mut self, _path: &str, _data: Vec<u8>) -> VfsResult<()> {
        // FAT32 is read-only for now
        Err(VfsError::PermissionDenied)
    }

    fn delete(&mut self, _path: &str) -> VfsResult<()> {
        // FAT32 is read-only for now
        Err(VfsError::PermissionDenied)
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
        // TODO: Calculate actual used space by traversing FAT
        0
    }

    fn total_space(&self) -> usize {
        // TODO: Calculate total space from BPB
        0
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
