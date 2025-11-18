/// Virtual File System - abstraction layer for different filesystems
///
/// This module provides traits and types for a unified interface
/// to work with different filesystem implementations (RAM disk, FAT32, etc.)

use alloc::vec::Vec;
use alloc::string::String;

/// Information about a file in the filesystem
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub size: usize,
}

impl FileInfo {
    pub fn new(name: String, size: usize) -> Self {
        FileInfo { name, size }
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

    /// Get free space (in bytes)
    fn free_space(&self) -> usize {
        self.total_space().saturating_sub(self.used_space())
    }
}
