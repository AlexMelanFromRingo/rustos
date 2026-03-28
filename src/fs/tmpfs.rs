/// /tmp filesystem (tmpfs)
///
/// RAM-backed temporary filesystem. Data is lost on reboot.
/// Separate from the main RAMDISK — provides isolated temporary storage.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;
use crate::fs::vfs::{FileInfo, VfsError, VfsResult};

const MAX_TMP_FILES: usize = 128;
const MAX_TMP_FILE_SIZE: usize = 512 * 1024; // 512 KiB per file

struct TmpFile {
    name: String,    // Relative path within /tmp (e.g. "foo.txt")
    content: Vec<u8>,
    is_directory: bool,
}

struct TmpFs {
    files: Vec<TmpFile>,
}

impl TmpFs {
    const fn new() -> Self {
        TmpFs { files: Vec::new() }
    }
}

static TMPFS: Mutex<TmpFs> = Mutex::new(TmpFs::new());

/// Normalize a /tmp path to a relative name within tmpfs
fn normalize(path: &str) -> &str {
    let path = path.trim_start_matches("/tmp");
    let path = path.trim_start_matches('/');
    if path.is_empty() { "." } else { path }
}

/// Check if a path is a /tmp path
pub fn is_tmp_path(path: &str) -> bool {
    path == "/tmp" || path.starts_with("/tmp/")
}

/// Check if a /tmp path exists
pub fn exists(path: &str) -> bool {
    let name = normalize(path);
    if name == "." { return true; }
    let fs = TMPFS.lock();
    fs.files.iter().any(|f| f.name == name)
}

/// Check if a /tmp path is a directory
pub fn is_directory(path: &str) -> bool {
    let name = normalize(path);
    if name == "." { return true; }
    let fs = TMPFS.lock();
    fs.files.iter().any(|f| f.name == name && f.is_directory)
}

/// Read a file from /tmp
pub fn read(path: &str) -> VfsResult<Vec<u8>> {
    let name = normalize(path);
    if name == "." {
        // List directory as text
        let fs = TMPFS.lock();
        let mut content = String::new();
        for f in &fs.files {
            content.push_str(&f.name);
            content.push('\n');
        }
        return Ok(content.into_bytes());
    }

    let fs = TMPFS.lock();
    match fs.files.iter().find(|f| f.name == name && !f.is_directory) {
        Some(file) => Ok(file.content.clone()),
        None => Err(VfsError::FileNotFound),
    }
}

/// Write a file to /tmp
pub fn write(path: &str, data: Vec<u8>) -> VfsResult<()> {
    let name = normalize(path);
    if name == "." { return Err(VfsError::NotAFile); }
    if data.len() > MAX_TMP_FILE_SIZE { return Err(VfsError::IoError); }

    let mut fs = TMPFS.lock();

    // Update existing file
    if let Some(file) = fs.files.iter_mut().find(|f| f.name == name && !f.is_directory) {
        file.content = data;
        return Ok(());
    }

    // Create new file
    if fs.files.len() >= MAX_TMP_FILES {
        return Err(VfsError::IoError);
    }

    fs.files.push(TmpFile {
        name: name.to_string(),
        content: data,
        is_directory: false,
    });
    Ok(())
}

/// Delete a file from /tmp
pub fn delete(path: &str) -> VfsResult<()> {
    let name = normalize(path);
    if name == "." { return Err(VfsError::NotAFile); }

    let mut fs = TMPFS.lock();
    let before = fs.files.len();
    fs.files.retain(|f| f.name != name);
    if fs.files.len() < before {
        Ok(())
    } else {
        Err(VfsError::FileNotFound)
    }
}

/// Create a directory in /tmp
pub fn mkdir(path: &str) -> VfsResult<()> {
    let name = normalize(path);
    if name == "." { return Err(VfsError::AlreadyExists); }

    let mut fs = TMPFS.lock();
    if fs.files.iter().any(|f| f.name == name) {
        return Err(VfsError::AlreadyExists);
    }
    if fs.files.len() >= MAX_TMP_FILES {
        return Err(VfsError::IoError);
    }

    fs.files.push(TmpFile {
        name: name.to_string(),
        content: Vec::new(),
        is_directory: true,
    });
    Ok(())
}

/// List entries in a /tmp directory
pub fn list_dir(path: &str) -> VfsResult<Vec<FileInfo>> {
    let dir_name = normalize(path);

    let fs = TMPFS.lock();
    let mut entries = Vec::new();

    let prefix = if dir_name == "." {
        String::new()
    } else {
        let mut p = dir_name.to_string();
        p.push('/');
        p
    };

    for f in &fs.files {
        let child_name = if prefix.is_empty() {
            // Root of /tmp: entries with no '/' are direct children
            if !f.name.contains('/') {
                Some(f.name.as_str())
            } else {
                None
            }
        } else if f.name.starts_with(&prefix) {
            let remainder = &f.name[prefix.len()..];
            if !remainder.contains('/') {
                Some(remainder)
            } else {
                None
            }
        } else {
            None
        };

        if let Some(name) = child_name {
            if f.is_directory {
                entries.push(FileInfo::directory(name.to_string()));
            } else {
                entries.push(FileInfo::new(name.to_string(), f.content.len()));
            }
        }
    }

    Ok(entries)
}
