/// File descriptor management for syscalls
///
/// Provides file descriptor table and operations for open files.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// File descriptor type
pub type Fd = usize;

/// Standard file descriptors
pub const STDIN: Fd = 0;
pub const STDOUT: Fd = 1;
pub const STDERR: Fd = 2;

/// Maximum number of open files per process
const MAX_OPEN_FILES: usize = 256;

/// File open flags (simplified Linux O_ flags)
pub mod flags {
    pub const O_RDONLY: usize = 0x0000;
    pub const O_WRONLY: usize = 0x0001;
    pub const O_RDWR: usize = 0x0002;
    pub const O_CREAT: usize = 0x0040;
    pub const O_TRUNC: usize = 0x0200;
    pub const O_APPEND: usize = 0x0400;
}

/// Opened file information
pub struct OpenFile {
    pub path: String,
    pub flags: usize,
    pub offset: usize,
    pub is_open: bool,
}

impl OpenFile {
    fn new(path: String, flags: usize) -> Self {
        OpenFile {
            path,
            flags,
            offset: 0,
            is_open: true,
        }
    }
}

/// File descriptor table for current process
pub struct FileDescriptorTable {
    files: Vec<Option<OpenFile>>,
}

impl FileDescriptorTable {
    pub const fn new() -> Self {
        FileDescriptorTable {
            files: Vec::new(),
        }
    }

    /// Initialize with standard streams
    pub fn init(&mut self) {
        // Reserve space
        self.files.resize_with(MAX_OPEN_FILES, || None);

        // STDIN, STDOUT, STDERR are always open
        self.files[STDIN] = Some(OpenFile::new(String::from("/dev/stdin"), flags::O_RDONLY));
        self.files[STDOUT] = Some(OpenFile::new(String::from("/dev/stdout"), flags::O_WRONLY));
        self.files[STDERR] = Some(OpenFile::new(String::from("/dev/stderr"), flags::O_WRONLY));
    }

    /// Open a new file, returns fd
    pub fn open(&mut self, path: String, flags: usize) -> Option<Fd> {
        // Find free fd (skip 0, 1, 2)
        for fd in 3..MAX_OPEN_FILES {
            if self.files[fd].is_none() {
                self.files[fd] = Some(OpenFile::new(path, flags));
                return Some(fd);
            }
        }
        None  // No free descriptors
    }

    /// Close a file descriptor
    pub fn close(&mut self, fd: Fd) -> bool {
        // Don't allow closing stdin/stdout/stderr
        if fd < 3 || fd >= MAX_OPEN_FILES {
            return false;
        }

        if self.files[fd].is_some() {
            self.files[fd] = None;
            true
        } else {
            false
        }
    }

    /// Get mutable reference to open file
    pub fn get_mut(&mut self, fd: Fd) -> Option<&mut OpenFile> {
        if fd < MAX_OPEN_FILES {
            self.files[fd].as_mut()
        } else {
            None
        }
    }

    /// Get reference to open file
    pub fn get(&self, fd: Fd) -> Option<&OpenFile> {
        if fd < MAX_OPEN_FILES {
            self.files[fd].as_ref()
        } else {
            None
        }
    }

    /// Check if fd is valid and open
    pub fn is_valid(&self, fd: Fd) -> bool {
        if fd >= MAX_OPEN_FILES {
            return false;
        }
        self.files[fd].as_ref().map_or(false, |f| f.is_open)
    }
}

/// Global file descriptor table (per-process in the future)
pub static FD_TABLE: Mutex<FileDescriptorTable> = Mutex::new(FileDescriptorTable::new());

/// Initialize file descriptor table
pub fn init() {
    FD_TABLE.lock().init();
}

/// Get file descriptor table
pub fn get_fd_table() -> spin::MutexGuard<'static, FileDescriptorTable> {
    FD_TABLE.lock()
}
