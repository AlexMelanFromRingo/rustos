/// Virtual File System - abstraction layer for different filesystems
///
/// This module provides traits and types for a unified interface
/// to work with different filesystem implementations (RAM disk, FAT32, etc.)

use alloc::vec::Vec;
use alloc::string::String;
use crate::fs::fat32::FAT32;
use crate::fs::ramdisk::RAMDISK;

/// Information about a file or directory in the filesystem
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub size: usize,
    pub is_directory: bool,
}

impl FileInfo {
    pub fn new(name: String, size: usize) -> Self {
        FileInfo { name, size, is_directory: false }
    }

    pub fn directory(name: String) -> Self {
        FileInfo { name, size: 0, is_directory: true }
    }
}

/// Errors that can occur during filesystem operations
#[derive(Debug, Clone, Copy)]
pub enum VfsError {
    /// File not found
    FileNotFound,
    /// File already exists
    FileExists,
    /// No space left on device
    NoSpace,
    /// Invalid filename
    InvalidName,
    /// File too large
    FileTooLarge,
    /// Too many files
    TooManyFiles,
    /// Permission denied
    PermissionDenied,
    /// Path is not a directory
    NotADirectory,
    /// Path is a directory (operation requires a file)
    IsADirectory,
    /// Directory is not empty
    DirectoryNotEmpty,
    /// Generic I/O error
    IoError,
}

pub type VfsResult<T> = Result<T, VfsError>;

/// Virtual File System trait - common interface for all filesystems
pub trait FileSystem {
    /// Read the entire contents of a file
    fn read(&self, path: &str) -> VfsResult<Vec<u8>>;

    /// Write data to a file (create or overwrite)
    fn write(&mut self, path: &str, data: Vec<u8>) -> VfsResult<()>;

    /// Delete a file
    fn delete(&mut self, path: &str) -> VfsResult<()>;

    /// List all files in the filesystem
    fn list(&self) -> Vec<FileInfo>;

    /// Check if a file exists
    fn exists(&self, path: &str) -> bool;

    /// Get total space used by files (in bytes)
    fn used_space(&self) -> usize;

    /// Get total available space (in bytes)
    fn total_space(&self) -> usize;

    /// Create a directory
    fn mkdir(&mut self, path: &str) -> VfsResult<()>;

    /// Remove an empty directory
    fn rmdir(&mut self, path: &str) -> VfsResult<()>;

    /// List files/directories in a specific directory path
    fn list_dir(&self, path: &str) -> VfsResult<Vec<FileInfo>>;

    /// Check if path is a directory
    fn is_directory(&self, path: &str) -> bool;

    /// Get free space (in bytes)
    fn free_space(&self) -> usize {
        self.total_space().saturating_sub(self.used_space())
    }
}

/// Unified VFS context that automatically selects between FAT32 and RAMDISK
///
/// This provides a single interface for filesystem operations, automatically
/// using FAT32 if mounted, falling back to RAMDISK otherwise.
pub struct VfsContext;

impl VfsContext {
    /// Read a file from the active filesystem
    pub fn read(path: &str) -> VfsResult<Vec<u8>> {
        // Try FAT32 first if mounted
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            match fs.read(path) {
                Ok(data) => return Ok(data),
                Err(VfsError::FileNotFound) => {
                    // File not found in FAT32, try RAMDISK
                }
                Err(e) => return Err(e),
            }
        }
        drop(fat32);

        // Fall back to RAMDISK
        RAMDISK.lock().read(path)
    }

    /// Write a file to the active filesystem
    pub fn write(path: &str, data: Vec<u8>) -> VfsResult<()> {
        // Try FAT32 first if mounted
        let mut fat32 = FAT32.lock();
        if let Some(ref mut fs) = *fat32 {
            return fs.write(path, data);
        }
        drop(fat32);

        // Fall back to RAMDISK
        RAMDISK.lock().write(path, data)
    }

    /// Delete a file from the active filesystem
    pub fn delete(path: &str) -> VfsResult<()> {
        // Try FAT32 first if mounted
        let mut fat32 = FAT32.lock();
        if let Some(ref mut fs) = *fat32 {
            match fs.delete(path) {
                Ok(()) => return Ok(()),
                Err(VfsError::FileNotFound) => {
                    // File not found in FAT32, try RAMDISK
                }
                Err(e) => return Err(e),
            }
        }
        drop(fat32);

        // Fall back to RAMDISK
        RAMDISK.lock().delete(path)
    }

    /// List files from the active filesystem
    pub fn list() -> Vec<FileInfo> {
        // Try FAT32 first if mounted
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            let files = fs.list();
            drop(fat32);
            return files;
        }
        drop(fat32);

        // Fall back to RAMDISK
        RAMDISK.lock().list()
    }

    /// Check if a file exists in either filesystem
    pub fn exists(path: &str) -> bool {
        // Check FAT32 first
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            if fs.exists(path) {
                return true;
            }
        }
        drop(fat32);

        // Check RAMDISK
        RAMDISK.lock().exists(path)
    }

    /// Get used space from the active filesystem
    pub fn used_space() -> usize {
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            let space = fs.used_space();
            drop(fat32);
            return space;
        }
        drop(fat32);

        RAMDISK.lock().used_space()
    }

    /// Get total space from the active filesystem
    pub fn total_space() -> usize {
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            let space = fs.total_space();
            drop(fat32);
            return space;
        }
        drop(fat32);

        RAMDISK.lock().total_space()
    }

    /// Create a directory
    pub fn mkdir(path: &str) -> VfsResult<()> {
        let mut fat32 = FAT32.lock();
        if let Some(ref mut fs) = *fat32 {
            return fs.mkdir(path);
        }
        drop(fat32);

        RAMDISK.lock().mkdir(path)
    }

    /// Remove an empty directory
    pub fn rmdir(path: &str) -> VfsResult<()> {
        let mut fat32 = FAT32.lock();
        if let Some(ref mut fs) = *fat32 {
            return fs.rmdir(path);
        }
        drop(fat32);

        RAMDISK.lock().rmdir(path)
    }

    /// List files/directories in a specific directory
    pub fn list_dir(path: &str) -> VfsResult<Vec<FileInfo>> {
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            let result = fs.list_dir(path);
            drop(fat32);
            return result;
        }
        drop(fat32);

        RAMDISK.lock().list_dir(path)
    }

    /// Check if path is a directory
    pub fn is_directory(path: &str) -> bool {
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            let result = fs.is_directory(path);
            drop(fat32);
            return result;
        }
        drop(fat32);

        RAMDISK.lock().is_directory(path)
    }

    /// Check if FAT32 is currently mounted
    pub fn is_fat32_mounted() -> bool {
        FAT32.lock().is_some()
    }

    /// Get the name of the active filesystem
    pub fn filesystem_name() -> &'static str {
        if Self::is_fat32_mounted() {
            "FAT32"
        } else {
            "RAMDISK"
        }
    }
}
