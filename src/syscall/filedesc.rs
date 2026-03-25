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

/// What kind of underlying object a file descriptor refers to
#[derive(Clone, Debug)]
pub enum FileKind {
    /// Regular file on the filesystem
    Regular,
    /// Read end of a pipe
    PipeRead(usize),  // pipe_id
    /// Write end of a pipe
    PipeWrite(usize), // pipe_id
}

/// Opened file information
pub struct OpenFile {
    pub path: String,
    pub flags: usize,
    pub offset: usize,
    pub is_open: bool,
    pub kind: FileKind,
}

impl OpenFile {
    fn new(path: String, flags: usize) -> Self {
        OpenFile {
            path,
            flags,
            offset: 0,
            is_open: true,
            kind: FileKind::Regular,
        }
    }

    fn new_pipe_read(pipe_id: usize) -> Self {
        OpenFile {
            path: String::from("[pipe]"),
            flags: flags::O_RDONLY,
            offset: 0,
            is_open: true,
            kind: FileKind::PipeRead(pipe_id),
        }
    }

    fn new_pipe_write(pipe_id: usize) -> Self {
        OpenFile {
            path: String::from("[pipe]"),
            flags: flags::O_WRONLY,
            offset: 0,
            is_open: true,
            kind: FileKind::PipeWrite(pipe_id),
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
            self.close_with_pipe_cleanup(fd);
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

    /// Duplicate a file descriptor (dup). Returns new fd pointing to same file.
    pub fn dup(&mut self, old_fd: Fd) -> Option<Fd> {
        if !self.is_valid(old_fd) {
            return None;
        }

        // Find the lowest available fd >= 3
        for new_fd in 3..MAX_OPEN_FILES {
            if self.files[new_fd].is_none() {
                let old_file = self.files[old_fd].as_ref().unwrap();
                self.files[new_fd] = Some(OpenFile {
                    path: old_file.path.clone(),
                    flags: old_file.flags,
                    offset: old_file.offset,
                    is_open: true,
                    kind: old_file.kind.clone(),
                });
                return Some(new_fd);
            }
        }
        None
    }

    /// Duplicate fd to a specific target fd (dup2).
    /// If new_fd is already open, it is closed first.
    pub fn dup2(&mut self, old_fd: Fd, new_fd: Fd) -> Option<Fd> {
        if !self.is_valid(old_fd) || new_fd >= MAX_OPEN_FILES {
            return None;
        }

        // If old_fd == new_fd, just return (POSIX behavior)
        if old_fd == new_fd {
            return Some(new_fd);
        }

        // Close new_fd if it's open (silently, as per POSIX)
        self.close_with_pipe_cleanup(new_fd);

        let old_file = self.files[old_fd].as_ref().unwrap();
        self.files[new_fd] = Some(OpenFile {
            path: old_file.path.clone(),
            flags: old_file.flags,
            offset: old_file.offset,
            is_open: true,
            kind: old_file.kind.clone(),
        });
        Some(new_fd)
    }

    /// Open a pipe read end, returns fd
    pub fn open_pipe_read(&mut self, pipe_id: usize) -> Option<Fd> {
        for fd in 3..MAX_OPEN_FILES {
            if self.files[fd].is_none() {
                self.files[fd] = Some(OpenFile::new_pipe_read(pipe_id));
                return Some(fd);
            }
        }
        None
    }

    /// Open a pipe write end, returns fd
    pub fn open_pipe_write(&mut self, pipe_id: usize) -> Option<Fd> {
        for fd in 3..MAX_OPEN_FILES {
            if self.files[fd].is_none() {
                self.files[fd] = Some(OpenFile::new_pipe_write(pipe_id));
                return Some(fd);
            }
        }
        None
    }

    /// Close fd with pipe cleanup (closes pipe ends)
    fn close_with_pipe_cleanup(&mut self, fd: Fd) {
        if fd >= MAX_OPEN_FILES {
            return;
        }
        if let Some(file) = self.files[fd].take() {
            match file.kind {
                FileKind::PipeRead(pipe_id) => {
                    super::pipe::pipe_close_read(pipe_id);
                }
                FileKind::PipeWrite(pipe_id) => {
                    super::pipe::pipe_close_write(pipe_id);
                }
                FileKind::Regular => {}
            }
        }
    }

    /// Seek in a file. Returns new offset or error.
    ///
    /// whence: 0=SEEK_SET, 1=SEEK_CUR, 2=SEEK_END
    pub fn lseek(&mut self, fd: Fd, offset: isize, whence: usize, file_size: usize) -> Option<usize> {
        let file = self.files[fd].as_mut()?;

        let new_offset = match whence {
            0 => {
                // SEEK_SET
                if offset < 0 { return None; }
                offset as usize
            }
            1 => {
                // SEEK_CUR
                if offset < 0 {
                    file.offset.checked_sub((-offset) as usize)?
                } else {
                    file.offset + offset as usize
                }
            }
            2 => {
                // SEEK_END
                if offset < 0 {
                    file_size.checked_sub((-offset) as usize)?
                } else {
                    file_size + offset as usize
                }
            }
            _ => return None,
        };

        file.offset = new_offset;
        Some(new_offset)
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
