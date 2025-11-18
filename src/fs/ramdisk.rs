/// Simple RAM-based filesystem
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

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
    pub fn write(&mut self, name: &str, content: Vec<u8>) -> Result<(), &'static str> {
        if name.is_empty() {
            return Err("Filename cannot be empty");
        }

        let file = File::new(name.to_string(), content)?;

        // Check if file exists
        if let Some(existing) = self.files.iter_mut().find(|f| f.name == name) {
            // Overwrite existing file
            *existing = file;
        } else {
            // Create new file
            if self.files.len() >= MAX_FILES {
                return Err("Too many files");
            }
            self.files.push(file);
        }

        Ok(())
    }

    /// Read a file
    pub fn read(&self, name: &str) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.content.as_slice())
    }

    /// Delete a file
    pub fn delete(&mut self, name: &str) -> Result<(), &'static str> {
        let index = self
            .files
            .iter()
            .position(|f| f.name == name)
            .ok_or("File not found")?;

        self.files.remove(index);
        Ok(())
    }

    /// List all files
    pub fn list(&self) -> &[File] {
        &self.files
    }

    /// Get total used space
    pub fn used_space(&self) -> usize {
        self.files.iter().map(|f| f.size()).sum()
    }

    /// Get total free space
    pub fn free_space(&self) -> usize {
        MAX_FILES * MAX_FILE_SIZE - self.used_space()
    }
}

// Global RAM disk instance
pub static RAMDISK: Mutex<RamDisk> = Mutex::new(RamDisk::new());
