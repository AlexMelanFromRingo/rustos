/// System call handlers
///
/// Each function implements a specific system call.
/// All handlers run in kernel mode (ring 0) on behalf of user processes.

use super::{SyscallError, STDIN, STDOUT, STDERR};
use crate::println;
use crate::process::PROCESS_MANAGER;

/// sys_read - read from file descriptor
///
/// Arguments:
/// - fd: file descriptor
/// - buf: pointer to user buffer (virtual address)
/// - count: number of bytes to read
///
/// Returns: number of bytes read or negative error code
pub fn sys_read(fd: usize, _buf: usize, _count: usize) -> isize {
    // For now, only support reading from STDIN
    match fd {
        STDIN => {
            // TODO: Implement actual keyboard input buffering
            // For now, return 0 (EOF) as placeholder
            0
        }
        _ => SyscallError::BadFileDescriptor.as_isize(),
    }
}

/// sys_write - write to file descriptor
///
/// Arguments:
/// - fd: file descriptor
/// - buf: pointer to user buffer (virtual address)
/// - count: number of bytes to write
///
/// Returns: number of bytes written or negative error code
pub fn sys_write(fd: usize, buf_ptr: usize, count: usize) -> isize {
    match fd {
        STDOUT | STDERR => {
            // Safety: For now, we trust the pointer from user space
            // TODO: Add proper virtual address validation and copying
            if buf_ptr == 0 || count == 0 {
                return SyscallError::InvalidArgument.as_isize();
            }

            // Copy data from user space (for now, we're in the same address space)
            let slice = unsafe {
                core::slice::from_raw_parts(buf_ptr as *const u8, count)
            };

            // Convert to string and print
            match core::str::from_utf8(slice) {
                Ok(s) => {
                    crate::print!("{}", s);
                    count as isize
                }
                Err(_) => {
                    // If not valid UTF-8, print as hex
                    for byte in slice {
                        crate::print!("{:02x}", byte);
                    }
                    count as isize
                }
            }
        }
        _ => SyscallError::BadFileDescriptor.as_isize(),
    }
}

/// sys_open - open file
///
/// Arguments:
/// - path: pointer to path string
/// - flags: open flags
///
/// Returns: file descriptor or negative error code
pub fn sys_open(path_ptr: usize, _flags: usize) -> isize {
    if path_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // TODO: Implement actual file opening
    // For now, return not implemented
    SyscallError::NotImplemented.as_isize()
}

/// sys_close - close file descriptor
///
/// Arguments:
/// - fd: file descriptor to close
///
/// Returns: 0 on success or negative error code
pub fn sys_close(fd: usize) -> isize {
    // Don't allow closing standard streams
    match fd {
        STDIN | STDOUT | STDERR => SyscallError::InvalidArgument.as_isize(),
        _ => {
            // TODO: Implement actual file closing
            SyscallError::NotImplemented.as_isize()
        }
    }
}

/// sys_exit - terminate current process
///
/// Arguments:
/// - exit_code: process exit code
///
/// Returns: does not return (terminates process)
pub fn sys_exit(exit_code: usize) -> isize {
    println!("\nProcess exited with code: {}", exit_code);

    // Terminate current process
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(pid) = pm.current_pid {
        pm.terminate(pid);
    }
    drop(pm);

    // Yield to scheduler to switch to another process
    crate::process::scheduler::schedule();

    // Should not reach here
    0
}

/// sys_getpid - get process ID
///
/// Returns: current process ID
pub fn sys_getpid() -> isize {
    let pm = PROCESS_MANAGER.lock();
    match pm.current_pid {
        Some(pid) => pid as isize,
        None => 0, // No current process
    }
}

/// sys_getcwd - get current working directory
///
/// Arguments:
/// - buf: buffer to store path
/// - size: buffer size
///
/// Returns: number of bytes written or negative error code
pub fn sys_getcwd(_buf: usize, _size: usize) -> isize {
    // TODO: Implement once we have per-process working directory
    SyscallError::NotImplemented.as_isize()
}

/// sys_chdir - change current working directory
///
/// Arguments:
/// - path: pointer to path string
///
/// Returns: 0 on success or negative error code
pub fn sys_chdir(_path: usize) -> isize {
    // TODO: Implement once we have per-process working directory
    SyscallError::NotImplemented.as_isize()
}
