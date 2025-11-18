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

        // Get total sectors
        let total_sectors = if bpb.total_sectors_16 != 0 {
            bpb.total_sectors_16 as u32
        } else {
            bpb.total_sectors_32
        };

        // Calculate total clusters in data area
        let data_sectors = total_sectors - first_data_sector;
        let total_clusters = data_sectors / (bpb.sectors_per_cluster as u32);

        Ok(Fat32 {
            first_data_sector,
            sectors_per_cluster: bpb.sectors_per_cluster as u32,
            bytes_per_sector: bpb.bytes_per_sector as u32,
            first_fat_sector,
            root_cluster: ebr.root_cluster,
            sectors_per_fat: fat_size,
            total_sectors,
            total_clusters,
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

    /// Write a FAT entry for a given cluster
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), &'static str> {
        let fat_offset = cluster * 4;
        let fat_sector = self.first_fat_sector + (fat_offset / self.bytes_per_sector);
        let entry_offset = (fat_offset % self.bytes_per_sector) as usize;

        // Read the sector containing this FAT entry
        let mut sector_buffer = [0u8; SECTOR_SIZE];
        let mut drive = ATA_DRIVE.lock();
        drive.read_sector(fat_sector, &mut sector_buffer)?;

        // Update the FAT entry (preserve top 4 bits)
        let masked_value = value & 0x0FFFFFFF;
        sector_buffer[entry_offset..entry_offset + 4].copy_from_slice(&masked_value.to_le_bytes());

        // Write back to disk
        // Note: In a real implementation, we should write to ALL FAT copies
        // For now, we'll just write to the first FAT
        drive.write_sector(fat_sector, &sector_buffer)?;
        drop(drive);

        Ok(())
    }

    /// Allocate a cluster chain for a file
    /// Returns the first cluster number
    fn allocate_clusters(&mut self, num_clusters: u32) -> Result<u32, &'static str> {
        if num_clusters == 0 {
            return Err("Cannot allocate zero clusters");
        }

        // Find first free cluster
        let first_cluster = match self.find_free_cluster()? {
            Some(cluster) => cluster,
            None => return Err("No free space available"),
        };

        let mut prev_cluster = first_cluster;

        // Allocate remaining clusters
        for _ in 1..num_clusters {
            let next_cluster = match self.find_free_cluster()? {
                Some(cluster) => cluster,
                None => {
                    // TODO: Clean up partially allocated chain
                    return Err("Insufficient space");
                }
            };

            // Link prev_cluster to next_cluster
            self.write_fat_entry(prev_cluster, next_cluster)?;
            prev_cluster = next_cluster;
        }

        // Mark last cluster as end of chain
        self.write_fat_entry(prev_cluster, 0x0FFFFFFF)?;

        Ok(first_cluster)
    }

    /// Free a cluster chain
    fn free_cluster_chain(&mut self, start_cluster: u32) -> Result<(), &'static str> {
        let mut current_cluster = start_cluster;

        loop {
            let next_cluster = self.get_next_cluster(current_cluster)?;

            // Mark current cluster as free
            self.write_fat_entry(current_cluster, 0x00000000)?;

            match next_cluster {
                Some(next) => current_cluster = next,
                None => break,
            }
        }

        Ok(())
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
        // For now, only support files in root directory
        let path = path.trim_start_matches('/');

        if path.contains('/') {
            return Err(VfsError::PermissionDenied); // Subdirectories not supported
        }

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
        // For now, only support files in root directory
        let path = path.trim_start_matches('/');

        if path.contains('/') {
            return Err(VfsError::PermissionDenied); // Subdirectories not supported
        }

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
