/// Simple RAM-based filesystem
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;
use crate::fs::vfs::{FileSystem, FileInfo, VfsError, VfsResult};

const MAX_FILES: usize = 64;
const MAX_FILE_SIZE: usize = 4096; // 4KB per file
const MAX_FILENAME_LEN: usize = 32;

#[derive(Clone)]
pub struct File {
    pub name: String,
    pub content: Vec<u8>,
}

impl File {
    pub fn new(name: String, content: Vec<u8>) -> Result<Self, &'static str> {
        if name.len() > MAX_FILENAME_LEN {
            return Err("Filename too long");
        }
        if content.len() > MAX_FILE_SIZE {
            return Err("File too large");
        }
        Ok(File { name, content })
    }

    pub fn size(&self) -> usize {
        self.content.len()
    }
}

pub struct RamDisk {
    files: Vec<File>,
}

impl RamDisk {
    pub const fn new() -> Self {
        RamDisk {
            files: Vec::new(),
        }
    }

    /// Create or overwrite a file
    pub fn write_file(&mut self, name: &str, content: Vec<u8>) -> VfsResult<()> {
        if name.is_empty() {
            return Err(VfsError::InvalidName);
        }

        let file = File::new(name.to_string(), content)
            .map_err(|e| match e {
                "Filename too long" => VfsError::InvalidName,
                "File too large" => VfsError::FileTooLarge,
                _ => VfsError::IoError,
            })?;

        // Check if file exists
        if let Some(existing) = self.files.iter_mut().find(|f| f.name == name) {
            // Overwrite existing file
            *existing = file;
        } else {
            // Create new file
            if self.files.len() >= MAX_FILES {
                return Err(VfsError::TooManyFiles);
            }
            self.files.push(file);
        }

        Ok(())
    }

    /// Read a file
    pub fn read_file(&self, name: &str) -> VfsResult<Vec<u8>> {
        self.files
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.content.clone())
            .ok_or(VfsError::FileNotFound)
    }

    /// Delete a file
    pub fn delete_file(&mut self, name: &str) -> VfsResult<()> {
        let index = self
            .files
            .iter()
            .position(|f| f.name == name)
            .ok_or(VfsError::FileNotFound)?;

        self.files.remove(index);
        Ok(())
    }

    /// Check if file exists
    pub fn file_exists(&self, name: &str) -> bool {
        self.files.iter().any(|f| f.name == name)
    }

    /// List all files
    pub fn list_files(&self) -> Vec<FileInfo> {
        self.files
            .iter()
            .map(|f| FileInfo::new(f.name.clone(), f.size()))
            .collect()
    }

    /// Get total used space
    pub fn space_used(&self) -> usize {
        self.files.iter().map(|f| f.size()).sum()
    }

    /// Get total available space
    pub fn space_total(&self) -> usize {
        MAX_FILES * MAX_FILE_SIZE
    }
}

// Implement VFS trait for RamDisk
impl FileSystem for RamDisk {
    fn read(&self, path: &str) -> VfsResult<Vec<u8>> {
        self.read_file(path)
    }

    fn write(&mut self, path: &str, data: Vec<u8>) -> VfsResult<()> {
        self.write_file(path, data)
    }

    fn delete(&mut self, path: &str) -> VfsResult<()> {
        self.delete_file(path)
    }

    fn list(&self) -> Vec<FileInfo> {
        self.list_files()
    }

    fn exists(&self, path: &str) -> bool {
        self.file_exists(path)
    }

    fn used_space(&self) -> usize {
        self.space_used()
    }

    fn total_space(&self) -> usize {
        self.space_total()
    }
}

// Global RAM disk instance
pub static RAMDISK: Mutex<RamDisk> = Mutex::new(RamDisk::new());
