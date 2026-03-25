/// RAM-based filesystem with directory support
///
/// Files are stored with full paths (e.g., "/home/test.txt").
/// Directories are tracked as entries with is_directory=true and no content.
/// The root directory "/" always exists implicitly.
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;
use crate::fs::vfs::{FileSystem, FileInfo, VfsError, VfsResult};

const MAX_FILES: usize = 256;
const MAX_FILE_SIZE: usize = 1024 * 1024; // 1 MiB per file
const MAX_FILENAME_LEN: usize = 255;

#[derive(Clone)]
pub struct File {
    pub name: String, // Full path (e.g. "/home/test.txt" or just "test.txt" for root)
    pub content: Vec<u8>,
    pub is_directory: bool,
}

impl File {
    pub fn new(name: String, content: Vec<u8>) -> Result<Self, &'static str> {
        if name.len() > MAX_FILENAME_LEN {
            return Err("Filename too long");
        }
        if content.len() > MAX_FILE_SIZE {
            return Err("File too large");
        }
        Ok(File { name, content, is_directory: false })
    }

    pub fn new_directory(name: String) -> Result<Self, &'static str> {
        if name.len() > MAX_FILENAME_LEN {
            return Err("Filename too long");
        }
        Ok(File { name, content: Vec::new(), is_directory: true })
    }

    pub fn size(&self) -> usize {
        self.content.len()
    }
}

/// Normalize a path: strip trailing slashes, ensure no double slashes
fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path == "/" || path.is_empty() {
        return String::from("/");
    }
    // Remove trailing slash
    let path = path.trim_end_matches('/');
    // Remove leading slash for storage (we store relative to root)
    let path = path.trim_start_matches('/');
    String::from(path)
}

/// Get the parent directory of a path
fn parent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) if idx > 0 => &path[..idx],
        Some(_) => "/",
        None => "/",
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

        let normalized = normalize_path(name);

        let file = File::new(normalized.clone(), content)
            .map_err(|e| match e {
                "Filename too long" => VfsError::InvalidName,
                "File too large" => VfsError::FileTooLarge,
                _ => VfsError::IoError,
            })?;

        // Check if a directory with this name exists
        if self.files.iter().any(|f| f.name == normalized && f.is_directory) {
            return Err(VfsError::IsADirectory);
        }

        // Check if file exists
        if let Some(existing) = self.files.iter_mut().find(|f| f.name == normalized && !f.is_directory) {
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
        let normalized = normalize_path(name);
        self.files
            .iter()
            .find(|f| f.name == normalized && !f.is_directory)
            .map(|f| f.content.clone())
            .ok_or(VfsError::FileNotFound)
    }

    /// Delete a file
    pub fn delete_file(&mut self, name: &str) -> VfsResult<()> {
        let normalized = normalize_path(name);
        let index = self
            .files
            .iter()
            .position(|f| f.name == normalized && !f.is_directory)
            .ok_or(VfsError::FileNotFound)?;

        self.files.remove(index);
        Ok(())
    }

    /// Check if file or directory exists
    pub fn file_exists(&self, name: &str) -> bool {
        if name == "/" {
            return true;
        }
        let normalized = normalize_path(name);
        self.files.iter().any(|f| f.name == normalized)
    }

    /// List all files (flat, for backwards compatibility)
    pub fn list_files(&self) -> Vec<FileInfo> {
        self.files
            .iter()
            .map(|f| {
                let mut info = FileInfo::new(f.name.clone(), f.size());
                info.is_directory = f.is_directory;
                info
            })
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

    /// Create a directory
    pub fn create_directory(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Ok(()); // Root always exists
        }

        // Check if already exists
        if self.files.iter().any(|f| f.name == normalized) {
            return Err(VfsError::FileExists);
        }

        // Check parent directory exists
        let parent = parent_path(&normalized);
        if parent != "/" && !self.files.iter().any(|f| f.name == parent && f.is_directory) {
            return Err(VfsError::FileNotFound);
        }

        if self.files.len() >= MAX_FILES {
            return Err(VfsError::TooManyFiles);
        }

        let dir = File::new_directory(normalized)
            .map_err(|_| VfsError::InvalidName)?;
        self.files.push(dir);
        Ok(())
    }

    /// Remove an empty directory
    pub fn remove_directory(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Err(VfsError::PermissionDenied);
        }

        // Check it exists and is a directory
        let idx = self.files.iter().position(|f| f.name == normalized && f.is_directory)
            .ok_or(VfsError::FileNotFound)?;

        // Check if empty (no children with this prefix)
        let prefix = if normalized.ends_with('/') {
            normalized.clone()
        } else {
            let mut p = normalized.clone();
            p.push('/');
            p
        };

        let has_children = self.files.iter().any(|f| f.name.starts_with(&prefix));
        if has_children {
            return Err(VfsError::DirectoryNotEmpty);
        }

        self.files.remove(idx);
        Ok(())
    }

    /// List entries in a specific directory
    pub fn list_directory(&self, path: &str) -> VfsResult<Vec<FileInfo>> {
        let normalized = normalize_path(path);

        // Verify directory exists
        if normalized != "/" {
            if !self.files.iter().any(|f| f.name == normalized && f.is_directory) {
                return Err(VfsError::NotADirectory);
            }
        }

        let prefix = if normalized == "/" {
            String::new()
        } else {
            let mut p = normalized;
            p.push('/');
            p
        };

        let mut entries = Vec::new();

        for f in &self.files {
            // For root: entries with no '/' in name (direct children)
            // For subdirs: entries that start with prefix and have no additional '/'
            let child_name = if prefix.is_empty() {
                // Root directory: files with no '/' are direct children
                if !f.name.contains('/') {
                    Some(f.name.as_str())
                } else {
                    None
                }
            } else if f.name.starts_with(&prefix) {
                let remainder = &f.name[prefix.len()..];
                // Direct child: no more '/' in the remainder
                if !remainder.contains('/') {
                    Some(remainder)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(name) = child_name {
                let mut info = FileInfo::new(name.to_string(), f.size());
                info.is_directory = f.is_directory;
                entries.push(info);
            }
        }

        Ok(entries)
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

    fn mkdir(&mut self, path: &str) -> VfsResult<()> {
        self.create_directory(path)
    }

    fn rmdir(&mut self, path: &str) -> VfsResult<()> {
        self.remove_directory(path)
    }

    fn list_dir(&self, path: &str) -> VfsResult<Vec<FileInfo>> {
        self.list_directory(path)
    }

    fn is_directory(&self, path: &str) -> bool {
        if path == "/" {
            return true;
        }
        let normalized = normalize_path(path);
        self.files.iter().any(|f| f.name == normalized && f.is_directory)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_norm = normalize_path(old_path);
        let new_norm = normalize_path(new_path);

        // Check destination doesn't already exist
        if self.files.iter().any(|f| f.name == new_norm) {
            return Err(VfsError::FileExists);
        }

        // Find and rename the file/directory
        let file = self.files.iter_mut().find(|f| f.name == old_norm);
        match file {
            Some(f) => {
                f.name = new_norm;
                Ok(())
            }
            None => Err(VfsError::FileNotFound),
        }
    }
}

// Global RAM disk instance
pub static RAMDISK: Mutex<RamDisk> = Mutex::new(RamDisk::new());
