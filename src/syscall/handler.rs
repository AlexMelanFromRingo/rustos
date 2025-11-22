/// System call handlers
///
/// Each function implements a specific system call.
/// All handlers run in kernel mode (ring 0) on behalf of user processes.

use super::{SyscallError, STDIN, STDOUT, STDERR};
use super::filedesc::{self, flags};
use crate::println;
use crate::process::PROCESS_MANAGER;
use crate::fs::ramdisk::RAMDISK;
use crate::fs::fat32::FAT32;
use crate::fs::vfs::FileSystem;
use alloc::string::String;
use alloc::vec::Vec;

/// sys_read - read from file descriptor
pub fn sys_read(fd: usize, buf_ptr: usize, count: usize) -> isize {
    if buf_ptr == 0 || count == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let fd_table = filedesc::get_fd_table();

    // Check if fd is valid
    if !fd_table.is_valid(fd) {
        return SyscallError::BadFileDescriptor.as_isize();
    }

    match fd {
        STDIN => {
            // TODO: Implement keyboard input buffering
            0  // EOF for now
        }
        _ => {
            // Get file info
            let file = match fd_table.get(fd) {
                Some(f) => f,
                None => return SyscallError::BadFileDescriptor.as_isize(),
            };

            let path = file.path.clone();
            let offset = file.offset;

            // Try to read from filesystem
            let data = {
                // Try FAT32 first
                if let Some(ref fs) = *FAT32.lock() {
                    fs.read(&path).ok()
                } else {
                    // Fall back to RAMDISK
                    RAMDISK.lock().read(&path).ok()
                }
            };

            match data {
                Some(file_data) => {
                    // Calculate how much to read
                    let available = file_data.len().saturating_sub(offset);
                    let to_read = count.min(available);

                    if to_read == 0 {
                        return 0;  // EOF
                    }

                    // Copy to user buffer
                    let slice = unsafe {
                        core::slice::from_raw_parts_mut(buf_ptr as *mut u8, to_read)
                    };
                    slice.copy_from_slice(&file_data[offset..offset + to_read]);

                    // Update offset
                    drop(fd_table);
                    filedesc::get_fd_table().get_mut(fd).unwrap().offset += to_read;

                    to_read as isize
                }
                None => SyscallError::FileNotFound.as_isize(),
            }
        }
    }
}

/// sys_write - write to file descriptor
pub fn sys_write(fd: usize, buf_ptr: usize, count: usize) -> isize {
    if buf_ptr == 0 || count == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let fd_table = filedesc::get_fd_table();

    if !fd_table.is_valid(fd) {
        return SyscallError::BadFileDescriptor.as_isize();
    }

    match fd {
        STDOUT | STDERR => {
            drop(fd_table);

            // Copy data from user space
            let slice = unsafe {
                core::slice::from_raw_parts(buf_ptr as *const u8, count)
            };

            // Print to console
            match core::str::from_utf8(slice) {
                Ok(s) => crate::print!("{}", s),
                Err(_) => {
                    // Print as hex if not UTF-8
                    for byte in slice {
                        crate::print!("{:02x}", byte);
                    }
                }
            }

            count as isize
        }
        _ => {
            // Write to file
            let file = match fd_table.get(fd) {
                Some(f) => f,
                None => return SyscallError::BadFileDescriptor.as_isize(),
            };

            let path = file.path.clone();
            let _is_append = (file.flags & flags::O_APPEND) != 0;

            drop(fd_table);

            // Get data to write
            let data = unsafe {
                core::slice::from_raw_parts(buf_ptr as *const u8, count)
            };

            // Try to write to filesystem
            let result = {
                // Try FAT32 first (read-only for now)
                let _fat32_locked = FAT32.lock();

                // Write to RAMDISK (convert slice to Vec)
                RAMDISK.lock().write(&path, data.to_vec())
            };

            match result {
                Ok(_) => count as isize,
                Err(_) => SyscallError::PermissionDenied.as_isize(),
            }
        }
    }
}

/// sys_open - open file
pub fn sys_open(path_ptr: usize, flags: usize) -> isize {
    if path_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // Read path string from user space
    let path = unsafe {
        let mut len = 0;
        let ptr = path_ptr as *const u8;
        while len < 256 && *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = core::slice::from_raw_parts(ptr, len);
        match core::str::from_utf8(slice) {
            Ok(s) => String::from(s),
            Err(_) => return SyscallError::InvalidArgument.as_isize(),
        }
    };

    // Check if file exists (if not O_CREAT, fail)
    let exists = {
        if let Some(ref fs) = *FAT32.lock() {
            fs.exists(&path)
        } else {
            RAMDISK.lock().exists(&path)
        }
    };

    if !exists && (flags & flags::O_CREAT) == 0 {
        return SyscallError::FileNotFound.as_isize();
    }

    // Create file if needed
    if !exists && (flags & flags::O_CREAT) != 0 {
        let _ = RAMDISK.lock().write(&path, Vec::new());
    }

    // Open file descriptor
    let fd = filedesc::get_fd_table().open(path, flags);

    match fd {
        Some(fd) => fd as isize,
        None => SyscallError::OutOfMemory.as_isize(),  // No free fds
    }
}

/// sys_close - close file descriptor
pub fn sys_close(fd: usize) -> isize {
    let success = filedesc::get_fd_table().close(fd);

    if success {
        0
    } else {
        SyscallError::BadFileDescriptor.as_isize()
    }
}

/// sys_exit - terminate current process
pub fn sys_exit(exit_code: usize) -> isize {
    println!("\nProcess exited with code: {}", exit_code);

    // Terminate current process
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(pid) = pm.current_pid {
        pm.terminate(pid);
    }
    drop(pm);

    // Yield to scheduler
    crate::process::scheduler::schedule();

    // Should not reach here
    0
}

/// sys_getpid - get process ID
pub fn sys_getpid() -> isize {
    let pm = PROCESS_MANAGER.lock();
    match pm.current_pid {
        Some(pid) => pid as isize,
        None => 0,
    }
}

/// sys_getcwd - get current working directory
pub fn sys_getcwd(buf: usize, size: usize) -> isize {
    // TODO: Implement per-process working directory
    // For now, return root "/"
    if buf == 0 || size < 2 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let cwd = b"/\0";
    let to_copy = cwd.len().min(size);

    unsafe {
        let dest = core::slice::from_raw_parts_mut(buf as *mut u8, to_copy);
        dest.copy_from_slice(&cwd[..to_copy]);
    }

    to_copy as isize
}

/// sys_chdir - change current working directory
pub fn sys_chdir(_path: usize) -> isize {
    // TODO: Implement per-process working directory
    SyscallError::NotImplemented.as_isize()
}
