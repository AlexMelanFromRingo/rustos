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
            drop(fd_table);
            // Read from keyboard scancode queue
            // For STDIN, we block until at least one byte is available
            if let Ok(queue) = crate::task::keyboard::SCANCODE_QUEUE.try_get() {
                let mut bytes_read = 0usize;
                let dest = unsafe {
                    core::slice::from_raw_parts_mut(buf_ptr as *mut u8, count)
                };

                // Try to get at least one byte; return what's available
                while bytes_read < count {
                    if let Some(scancode) = queue.pop() {
                        // Convert scancode to ASCII using a simple mapping
                        // Full conversion requires keyboard state machine;
                        // for syscall-level reads, we pass raw scancodes
                        dest[bytes_read] = scancode;
                        bytes_read += 1;
                    } else if bytes_read > 0 {
                        // Got some data, return it
                        break;
                    } else {
                        // No data available, return 0 (would block)
                        return 0;
                    }
                }

                bytes_read as isize
            } else {
                0 // Queue not initialized
            }
        }
        _ => {
            // Get file info
            let file = match fd_table.get(fd) {
                Some(f) => f,
                None => return SyscallError::BadFileDescriptor.as_isize(),
            };

            let kind = file.kind.clone();
            let path = file.path.clone();
            let offset = file.offset;
            drop(fd_table);

            match kind {
                filedesc::FileKind::PipeRead(pipe_id) => {
                    // Read from pipe
                    let mut buf = unsafe {
                        core::slice::from_raw_parts_mut(buf_ptr as *mut u8, count)
                    };
                    match super::pipe::pipe_read(pipe_id, &mut buf) {
                        Ok(n) => n as isize,
                        Err(super::pipe::PipeError::WouldBlock) => 0,
                        Err(_) => SyscallError::InvalidArgument.as_isize(),
                    }
                }
                filedesc::FileKind::PipeWrite(_) => {
                    // Can't read from write end of pipe
                    SyscallError::BadFileDescriptor.as_isize()
                }
                filedesc::FileKind::Regular => {
                    // Try to read from filesystem
                    let data = {
                        if let Some(ref fs) = *FAT32.lock() {
                            fs.read(&path).ok()
                        } else {
                            RAMDISK.lock().read(&path).ok()
                        }
                    };

                    match data {
                        Some(file_data) => {
                            let available = file_data.len().saturating_sub(offset);
                            let to_read = count.min(available);

                            if to_read == 0 {
                                return 0;  // EOF
                            }

                            let slice = unsafe {
                                core::slice::from_raw_parts_mut(buf_ptr as *mut u8, to_read)
                            };
                            slice.copy_from_slice(&file_data[offset..offset + to_read]);

                            filedesc::get_fd_table().get_mut(fd).unwrap().offset += to_read;

                            to_read as isize
                        }
                        None => SyscallError::FileNotFound.as_isize(),
                    }
                }
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

            // Write to serial directly (avoids VGA → serial lock nesting deadlock
            // that occurs when called from SYSCALL handler with IF=0)
            use x86_64::instructions::port::Port;
            for &byte in slice {
                unsafe {
                    // Wait for UART TX buffer ready
                    let mut status_port = Port::<u8>::new(0x3F8 + 5);
                    while status_port.read() & 0x20 == 0 {}
                    // Write byte
                    let mut data_port = Port::<u8>::new(0x3F8);
                    data_port.write(byte);
                }
            }

            count as isize
        }
        _ => {
            // Write to file or pipe
            let file = match fd_table.get(fd) {
                Some(f) => f,
                None => return SyscallError::BadFileDescriptor.as_isize(),
            };

            let kind = file.kind.clone();
            let path = file.path.clone();
            let _is_append = (file.flags & flags::O_APPEND) != 0;

            drop(fd_table);

            // Get data to write
            let data = unsafe {
                core::slice::from_raw_parts(buf_ptr as *const u8, count)
            };

            // Handle pipe writes
            match kind {
                filedesc::FileKind::PipeWrite(pipe_id) => {
                    return match super::pipe::pipe_write(pipe_id, data) {
                        Ok(n) => n as isize,
                        Err(super::pipe::PipeError::BrokenPipe) => SyscallError::InvalidArgument.as_isize(),
                        Err(super::pipe::PipeError::WouldBlock) => 0,
                        Err(_) => SyscallError::InvalidArgument.as_isize(),
                    };
                }
                filedesc::FileKind::PipeRead(_) => {
                    return SyscallError::BadFileDescriptor.as_isize();
                }
                filedesc::FileKind::Regular => {}
            }

            // Try to write to filesystem
            let result = {
                let _fat32_locked = FAT32.lock();
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

    let path = unsafe { read_user_string(path_ptr) };
    let path = match path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
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
    let mut pm = PROCESS_MANAGER.lock();
    let pid = pm.current_pid;
    if let Some(pid) = pid {
        pm.exit(pid, exit_code as i32);
        pm.current_pid = None;
    }
    drop(pm);

    // Also dequeue from scheduler
    if let Some(pid) = pid {
        crate::process::scheduler::SCHEDULER.lock().dequeue(pid);
    }

    // If in preemptive mode, enable interrupts and enter HLT loop.
    // The timer ISR will dispatch the next user process or clear
    // PREEMPTIVE_MODE when none remain, returning to the original
    // idle loop via KERNEL_RETURN_FRAME.
    //
    // IMPORTANT: The SYSCALL instruction clears IF via SFMASK,
    // so we must re-enable interrupts before HLT.
    if crate::interrupts::PREEMPTIVE_MODE.load(core::sync::atomic::Ordering::SeqCst) {
        // Enable interrupts so timer ISR can fire and dispatch next process
        x86_64::instructions::interrupts::enable();
        loop {
            x86_64::instructions::hlt();
        }
    }

    // Non-preemptive: try cooperative schedule, then restore kernel context
    crate::process::scheduler::schedule();
    unsafe {
        crate::userspace::restore_kernel_context_and_return();
    }
}

/// sys_fork - create child process (copy of current)
pub fn sys_fork() -> isize {
    let mut pm = PROCESS_MANAGER.lock();

    // Get current process ID
    let parent_pid = match pm.current_pid {
        Some(pid) => pid,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    // Fork the process
    match pm.fork(parent_pid) {
        Ok(child_pid) => {
            println!("fork: parent={} -> child={}", parent_pid, child_pid);
            child_pid as isize
        }
        Err(e) => {
            println!("fork failed: {}", e);
            SyscallError::OutOfMemory.as_isize()
        }
    }
}

/// sys_execve - execute program
pub fn sys_execve(filename: usize, _argv: usize, _envp: usize) -> isize {
    // Get filename string
    if filename == 0 {
        crate::println!("execve: invalid filename pointer");
        return SyscallError::InvalidArgument.as_isize();
    }

    // Read filename (max 256 bytes)
    let mut path_buf = [0u8; 256];
    let mut len = 0;
    unsafe {
        let ptr = filename as *const u8;
        for i in 0..256 {
            let c = ptr.add(i).read();
            if c == 0 {
                break;
            }
            path_buf[i] = c;
            len = i + 1;
        }
    }

    let path = core::str::from_utf8(&path_buf[..len])
        .unwrap_or("");

    println!("execve: {}", path);

    // Load and execute ELF file
    // This will return after user program exits (via context restore magic)
    crate::elf::load_and_exec(path);

    // Success - program executed and exited
    0
}

/// sys_wait4 - wait for child process to exit (blocks if WNOHANG not set)
pub fn sys_wait4(pid: usize, wstatus: usize, options: usize) -> isize {
    const WNOHANG: usize = 1;

    // Bounded retry loop so we don't busy-spin forever (drops back to the
    // executor between checks).  Skipped when WNOHANG is set.
    let want_block = options & WNOHANG == 0;
    let max_iters: u64 = if want_block { 200_000 } else { 1 };

    for _ in 0..max_iters {
        let mut pm = PROCESS_MANAGER.lock();
        let parent_pid = match pm.current_pid {
            Some(pid) => pid,
            None => return SyscallError::InvalidArgument.as_isize(),
        };
        let result = if pid == (-1isize) as usize {
            pm.wait(parent_pid)
        } else {
            pm.wait(parent_pid).filter(|(child_pid, _)| *child_pid == pid)
        };
        drop(pm);

        match result {
            Some((child_pid, exit_code)) => {
                if wstatus != 0 {
                    unsafe { *(wstatus as *mut i32) = exit_code << 8; }
                }
                return child_pid as isize;
            }
            None => {
                if !want_block {
                    return 0;
                }
                // Yield to interrupts so the timer can advance.
                x86_64::instructions::interrupts::enable_and_hlt();
            }
        }
    }
    // Hit retry ceiling without any child exiting; behave like a non-blocking
    // wait (Linux returns 0 here when no children are eligible).
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
    if buf == 0 || size < 2 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let cwd = crate::process::current_cwd();
    let bytes = cwd.as_bytes();
    let needed = bytes.len() + 1; // +1 for NUL
    if needed > size {
        return SyscallError::InvalidArgument.as_isize();
    }

    unsafe {
        let dest = core::slice::from_raw_parts_mut(buf as *mut u8, needed);
        dest[..bytes.len()].copy_from_slice(bytes);
        dest[bytes.len()] = 0;
    }

    needed as isize
}

/// sys_lseek - reposition file offset
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> isize {
    if fd < 3 {
        // Can't seek on stdin/stdout/stderr
        return SyscallError::InvalidArgument.as_isize();
    }

    let mut fd_table = filedesc::get_fd_table();

    if !fd_table.is_valid(fd) {
        return SyscallError::BadFileDescriptor.as_isize();
    }

    // Get file size for SEEK_END
    let file_size = {
        let file = match fd_table.get(fd) {
            Some(f) => f,
            None => return SyscallError::BadFileDescriptor.as_isize(),
        };
        let path = file.path.clone();
        drop(fd_table);

        let data = {
            if let Some(ref fs) = *FAT32.lock() {
                fs.read(&path).ok()
            } else {
                RAMDISK.lock().read(&path).ok()
            }
        };

        let size = data.map(|d| d.len()).unwrap_or(0);
        fd_table = filedesc::get_fd_table();
        size
    };

    match fd_table.lseek(fd, offset, whence, file_size) {
        Some(new_offset) => new_offset as isize,
        None => SyscallError::InvalidArgument.as_isize(),
    }
}

/// sys_dup - duplicate file descriptor
pub fn sys_dup(old_fd: usize) -> isize {
    let mut fd_table = filedesc::get_fd_table();

    match fd_table.dup(old_fd) {
        Some(new_fd) => new_fd as isize,
        None => SyscallError::BadFileDescriptor.as_isize(),
    }
}

/// sys_dup2 - duplicate file descriptor to specific target
pub fn sys_dup2(old_fd: usize, new_fd: usize) -> isize {
    let mut fd_table = filedesc::get_fd_table();

    match fd_table.dup2(old_fd, new_fd) {
        Some(fd) => fd as isize,
        None => SyscallError::BadFileDescriptor.as_isize(),
    }
}

/// sys_mkdir - create a directory
pub fn sys_mkdir(path_ptr: usize, _mode: usize) -> isize {
    if path_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let path = unsafe { read_user_string(path_ptr) };
    let path = match path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    use crate::fs::vfs::{VfsContext, VfsError};
    match VfsContext::mkdir(&path) {
        Ok(_) => 0,
        Err(VfsError::FileExists) => SyscallError::InvalidArgument.as_isize(), // EEXIST
        Err(VfsError::FileNotFound) => SyscallError::FileNotFound.as_isize(), // ENOENT (parent)
        Err(_) => SyscallError::PermissionDenied.as_isize(),
    }
}

/// sys_rmdir - remove a directory
pub fn sys_rmdir(path_ptr: usize) -> isize {
    if path_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let path = unsafe { read_user_string(path_ptr) };
    let path = match path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    use crate::fs::vfs::{VfsContext, VfsError};
    match VfsContext::rmdir(&path) {
        Ok(_) => 0,
        Err(VfsError::FileNotFound) => SyscallError::FileNotFound.as_isize(),
        Err(VfsError::DirectoryNotEmpty) => SyscallError::InvalidArgument.as_isize(),
        Err(_) => SyscallError::PermissionDenied.as_isize(),
    }
}

/// sys_unlink - delete a file
pub fn sys_unlink(path_ptr: usize) -> isize {
    if path_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let path = unsafe { read_user_string(path_ptr) };
    let path = match path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    use crate::fs::vfs::{VfsContext, VfsError};

    // Don't allow unlinking directories (use rmdir for that)
    if VfsContext::is_directory(&path) {
        return SyscallError::PermissionDenied.as_isize(); // EISDIR
    }

    match VfsContext::delete(&path) {
        Ok(_) => 0,
        Err(VfsError::FileNotFound) => SyscallError::FileNotFound.as_isize(),
        Err(_) => SyscallError::PermissionDenied.as_isize(),
    }
}

/// sys_rename - rename/move a file or directory
pub fn sys_rename(old_ptr: usize, new_ptr: usize) -> isize {
    if old_ptr == 0 || new_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let old_path = unsafe { read_user_string(old_ptr) };
    let old_path = match old_path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    let new_path = unsafe { read_user_string(new_ptr) };
    let new_path = match new_path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    use crate::fs::vfs::{VfsContext, VfsError};
    match VfsContext::rename(&old_path, &new_path) {
        Ok(_) => 0,
        Err(VfsError::FileNotFound) => SyscallError::FileNotFound.as_isize(),
        Err(VfsError::FileExists) => SyscallError::InvalidArgument.as_isize(),
        Err(_) => SyscallError::PermissionDenied.as_isize(),
    }
}

/// Helper: read a null-terminated string from user space (max 256 bytes)
unsafe fn read_user_string(ptr: usize) -> Option<String> {
    let mut len = 0;
    let raw = ptr as *const u8;
    while len < 256 {
        if unsafe { *raw.add(len) } == 0 {
            break;
        }
        len += 1;
    }
    let slice = unsafe { core::slice::from_raw_parts(raw, len) };
    core::str::from_utf8(slice).ok().map(String::from)
}

/// sys_nanosleep - suspend execution for a specified time
///
/// req: pointer to struct { tv_sec: i64, tv_nsec: i64 }
/// rem: pointer to struct for remaining time (ignored for now)
pub fn sys_nanosleep(req: usize, _rem: usize) -> isize {
    if req == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // Read timespec from user space
    let (seconds, _nsec) = unsafe {
        let ts = req as *const [i64; 2];
        ((*ts)[0], (*ts)[1])
    };

    if seconds < 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // Convert to timer ticks (~18.2 Hz PIT)
    // 1 second ≈ 18 ticks
    let ticks = (seconds as u64) * 18;

    // Put process to sleep via scheduler
    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    sched.sleep_ticks(ticks);
    drop(sched);

    // Trigger reschedule so another process can run
    crate::process::scheduler::schedule();

    0
}

/// sys_clock_gettime - get time from a specified clock
///
/// clock_id: 0=CLOCK_REALTIME, 1=CLOCK_MONOTONIC
/// timespec_ptr: pointer to struct { tv_sec: i64, tv_nsec: i64 }
pub fn sys_clock_gettime(clock_id: usize, timespec_ptr: usize) -> isize {
    if timespec_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    match clock_id {
        0 => {
            // CLOCK_REALTIME - wall clock from RTC
            let dt = crate::drivers::rtc::read_datetime();
            let unix_secs = dt.to_unix_timestamp();

            unsafe {
                let ts = timespec_ptr as *mut [i64; 2];
                (*ts)[0] = unix_secs as i64;  // tv_sec
                (*ts)[1] = 0;                  // tv_nsec (RTC only has second precision)
            }
            0
        }
        1 => {
            // CLOCK_MONOTONIC - prefer the calibrated TSC for nanosecond
            // precision; fall back to PIT-derived seconds if uncalibrated.
            let (sec, nsec) = if crate::tsc::is_calibrated() {
                let ns = crate::tsc::ns_since_boot();
                ((ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as i64)
            } else {
                let ticks = crate::task::timer::current_ticks();
                let seconds = ticks / 18;
                let remainder_ticks = ticks % 18;
                let nsec = (remainder_ticks * 1_000_000_000) / 18;
                (seconds as i64, nsec as i64)
            };

            unsafe {
                let ts = timespec_ptr as *mut [i64; 2];
                (*ts)[0] = sec;
                (*ts)[1] = nsec;
            }
            0
        }
        _ => SyscallError::InvalidArgument.as_isize(),
    }
}

/// sys_pipe - create a pipe
///
/// pipefd: pointer to int[2], filled with [read_fd, write_fd]
pub fn sys_pipe(pipefd: usize) -> isize {
    if pipefd == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // Create kernel pipe
    let pipe_id = match super::pipe::create_pipe() {
        Some(id) => id,
        None => return SyscallError::OutOfMemory.as_isize(),
    };

    // Allocate file descriptors for both ends
    let mut fd_table = filedesc::get_fd_table();

    let read_fd = match fd_table.open_pipe_read(pipe_id) {
        Some(fd) => fd,
        None => {
            super::pipe::pipe_close_read(pipe_id);
            super::pipe::pipe_close_write(pipe_id);
            return SyscallError::OutOfMemory.as_isize();
        }
    };

    let write_fd = match fd_table.open_pipe_write(pipe_id) {
        Some(fd) => fd,
        None => {
            fd_table.close(read_fd);
            super::pipe::pipe_close_write(pipe_id);
            return SyscallError::OutOfMemory.as_isize();
        }
    };

    drop(fd_table);

    // Write fds to user space
    unsafe {
        let fds = pipefd as *mut [i32; 2];
        (*fds)[0] = read_fd as i32;
        (*fds)[1] = write_fd as i32;
    }

    0
}

/// Linux x86_64 stat structure layout (144 bytes)
/// See: man 2 stat, struct stat in <sys/stat.h>
#[repr(C)]
struct LinuxStat {
    st_dev: u64,        // Device ID
    st_ino: u64,        // Inode number
    st_nlink: u64,      // Number of hard links
    st_mode: u32,       // File type and mode
    st_uid: u32,        // User ID
    st_gid: u32,        // Group ID
    __pad0: u32,
    st_rdev: u64,       // Device ID (if special file)
    st_size: i64,       // Total size in bytes
    st_blksize: i64,    // Block size for filesystem I/O
    st_blocks: i64,     // Number of 512B blocks allocated
    st_atime: i64,      // Access time (seconds)
    st_atime_nsec: i64, // Access time (nanoseconds)
    st_mtime: i64,      // Modification time (seconds)
    st_mtime_nsec: i64, // Modification time (nanoseconds)
    st_ctime: i64,      // Status change time (seconds)
    st_ctime_nsec: i64, // Status change time (nanoseconds)
    __unused: [i64; 3],
}

// File type bits for st_mode
const S_IFDIR: u32 = 0o040000;  // Directory
const S_IFREG: u32 = 0o100000;  // Regular file
const S_IFLNK: u32 = 0o120000;  // Symbolic link
const S_IFCHR: u32 = 0o020000;  // Character device

/// Fill a LinuxStat struct for a given path
fn fill_stat(path: &str, stat_ptr: usize) -> isize {
    use crate::fs::vfs::{VfsContext, VfsFileType};

    let info = match VfsContext::stat(path) {
        Ok(info) => info,
        Err(_) => return SyscallError::FileNotFound.as_isize(),
    };

    let type_bits = match info.file_type {
        VfsFileType::Directory => S_IFDIR,
        VfsFileType::Regular => S_IFREG,
        VfsFileType::Symlink => S_IFLNK,
        VfsFileType::CharDevice => S_IFCHR,
        VfsFileType::BlockDevice => 0o060000,
    };
    let mode = type_bits | (info.mode as u32);

    let file_size = info.size as i64;
    let blocks = (file_size + 511) / 512;

    // Simple inode: hash the path
    let ino = path.bytes().fold(1u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));

    // Use file timestamps if available, otherwise fall back to RTC
    let (atime, mtime, ctime) = if info.mtime > 0 || info.ctime > 0 {
        (info.atime as i64, info.mtime as i64, info.ctime as i64)
    } else {
        let dt = crate::drivers::rtc::read_datetime();
        let now = dt.to_unix_timestamp() as i64;
        (now, now, now)
    };

    let stat = LinuxStat {
        st_dev: 0,
        st_ino: ino,
        st_nlink: 1,
        st_mode: mode,
        st_uid: info.uid,
        st_gid: info.gid,
        __pad0: 0,
        st_rdev: 0,
        st_size: file_size,
        st_blksize: 4096,
        st_blocks: blocks,
        st_atime: atime,
        st_atime_nsec: 0,
        st_mtime: mtime,
        st_mtime_nsec: 0,
        st_ctime: ctime,
        st_ctime_nsec: 0,
        __unused: [0; 3],
    };

    unsafe {
        let dest = stat_ptr as *mut LinuxStat;
        core::ptr::write(dest, stat);
    }

    0
}

/// sys_stat - get file status by path
pub fn sys_stat(path_ptr: usize, stat_ptr: usize) -> isize {
    if path_ptr == 0 || stat_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let path = unsafe { read_user_string(path_ptr) };
    let path = match path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    fill_stat(&path, stat_ptr)
}

/// sys_fstat - get file status by file descriptor
pub fn sys_fstat(fd: usize, stat_ptr: usize) -> isize {
    if stat_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // stdin/stdout/stderr are character devices
    if fd < 3 {
        let stat = LinuxStat {
            st_dev: 0,
            st_ino: fd as u64,
            st_nlink: 1,
            st_mode: 0o020000 | 0o666, // S_IFCHR | rw-rw-rw-
            st_uid: 0,
            st_gid: 0,
            __pad0: 0,
            st_rdev: 0,
            st_size: 0,
            st_blksize: 1024,
            st_blocks: 0,
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
            __unused: [0; 3],
        };
        unsafe {
            let dest = stat_ptr as *mut LinuxStat;
            core::ptr::write(dest, stat);
        }
        return 0;
    }

    let fd_table = filedesc::get_fd_table();
    let file = match fd_table.get(fd) {
        Some(f) => f,
        None => return SyscallError::BadFileDescriptor.as_isize(),
    };
    let path = file.path.clone();
    drop(fd_table);

    fill_stat(&path, stat_ptr)
}

/// sys_chdir - change current working directory
pub fn sys_chdir(path_ptr: usize) -> isize {
    if path_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let path = unsafe { read_user_string(path_ptr) };
    let path = match path {
        Some(p) => p,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    use crate::fs::vfs::VfsContext;

    // Verify the directory exists
    if path != "/" && !VfsContext::is_directory(&path) {
        return SyscallError::FileNotFound.as_isize();
    }

    crate::process::set_current_cwd(&path);
    0
}

/// sys_brk - adjust program break (heap end)
///
/// In Linux, brk(0) returns the current break. brk(addr) sets the break.
/// We track a simple per-kernel program break for user programs.
pub fn sys_brk(addr: usize) -> isize {
    use core::sync::atomic::{AtomicUsize, Ordering};

    // Simple program break tracking
    // User programs start at 0x400000, heap starts after program text
    static PROGRAM_BREAK: AtomicUsize = AtomicUsize::new(0x800000); // Default heap start at 8 MiB

    if addr == 0 {
        // Return current break
        return PROGRAM_BREAK.load(Ordering::Relaxed) as isize;
    }

    // Don't allow break below initial value or above reasonable limit
    const MIN_BREAK: usize = 0x800000;    // 8 MiB
    const MAX_BREAK: usize = 0x10000000;  // 256 MiB

    if addr < MIN_BREAK || addr > MAX_BREAK {
        return SyscallError::OutOfMemory.as_isize();
    }

    PROGRAM_BREAK.store(addr, Ordering::Relaxed);
    addr as isize
}

/// sys_kill - send signal to a process
pub fn sys_kill(pid: usize, sig: usize) -> isize {
    let mut pm = PROCESS_MANAGER.lock();
    match pm.send_signal(pid, sig as u32) {
        Ok(_) => 0,
        Err(_) => SyscallError::InvalidArgument.as_isize(),
    }
}

/// sys_getuid - get user ID (always 0 = root)
pub fn sys_getuid() -> isize {
    0
}

/// sys_getgid - get group ID (always 0 = root)
pub fn sys_getgid() -> isize {
    0
}

/// mmap flags
const _MAP_PRIVATE: usize = 0x02;
const MAP_ANONYMOUS: usize = 0x20;
const MAP_FIXED: usize = 0x10;

/// mmap protection flags
const PROT_READ: usize = 0x1;
const PROT_WRITE: usize = 0x2;

/// sys_mmap - map virtual memory
///
/// Currently only supports anonymous private mappings (MAP_ANONYMOUS | MAP_PRIVATE).
/// File-backed mappings are not yet implemented.
pub fn sys_mmap(addr: usize, length: usize, prot: usize, flags: usize, fd: isize, _offset: usize) -> isize {
    use core::sync::atomic::{AtomicUsize, Ordering};

    // Virtual address allocator for mmap region (above program break area)
    static MMAP_BASE: AtomicUsize = AtomicUsize::new(0x1000_0000); // Start at 256 MiB

    if length == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // Only support anonymous private mappings for now
    if flags & MAP_ANONYMOUS == 0 {
        // File-backed mapping requested
        if fd < 0 {
            return SyscallError::InvalidArgument.as_isize();
        }
        // Not implemented yet
        return SyscallError::NotImplemented.as_isize();
    }

    // Round length up to page boundary
    let page_size = 4096usize;
    let aligned_length = (length + page_size - 1) & !(page_size - 1);

    // Determine mapping address
    let map_addr = if flags & MAP_FIXED != 0 {
        // MAP_FIXED: use requested address exactly
        if addr == 0 || addr % page_size != 0 {
            return SyscallError::InvalidArgument.as_isize();
        }
        addr
    } else if addr != 0 {
        // Hint address — try to use it, but we just use our allocator
        let base = MMAP_BASE.fetch_add(aligned_length, Ordering::Relaxed);
        base
    } else {
        // No address preference — allocate from mmap region
        let base = MMAP_BASE.fetch_add(aligned_length, Ordering::Relaxed);
        base
    };

    // Sanity check: don't allow mappings above a reasonable limit
    if map_addr + aligned_length > 0x8000_0000_0000 {
        return SyscallError::OutOfMemory.as_isize();
    }

    // For anonymous mappings, we need to allocate physical frames and map them.
    // Currently all processes share the kernel page table, so we map into it.
    let result = crate::memory::with_frame_allocator(|frame_alloc| {
        use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB};
        use x86_64::VirtAddr;

        let mut mapper = unsafe { crate::memory::get_mapper() };
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        let flags = if prot & PROT_READ == 0 { flags } else { flags }; // All pages readable
        let flags = if prot & PROT_WRITE != 0 { flags | PageTableFlags::WRITABLE } else {
            flags & !PageTableFlags::WRITABLE
        };
        let flags = flags | PageTableFlags::USER_ACCESSIBLE;

        let start_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(map_addr as u64));
        let num_pages = aligned_length / page_size;

        for i in 0..num_pages {
            let page = start_page + i as u64;
            let frame = frame_alloc.allocate_frame()
                .ok_or(SyscallError::OutOfMemory)?;
            unsafe {
                mapper.map_to(page, frame, flags, frame_alloc)
                    .map_err(|_| SyscallError::OutOfMemory)?
                    .flush();
            }
        }

        // Zero the mapped memory (anonymous mappings must be zeroed)
        unsafe {
            core::ptr::write_bytes(map_addr as *mut u8, 0, aligned_length);
        }

        Ok::<usize, SyscallError>(map_addr)
    });

    match result {
        Some(Ok(addr)) => addr as isize,
        Some(Err(e)) => e.as_isize(),
        None => SyscallError::OutOfMemory.as_isize(),
    }
}

/// sys_munmap - unmap virtual memory
pub fn sys_munmap(addr: usize, length: usize) -> isize {
    if addr == 0 || length == 0 || addr % 4096 != 0 {
        return SyscallError::InvalidArgument.as_isize();
    }

    let page_size = 4096usize;
    let aligned_length = (length + page_size - 1) & !(page_size - 1);
    let num_pages = aligned_length / page_size;

    use x86_64::structures::paging::{Mapper, Page, Size4KiB};
    use x86_64::VirtAddr;

    let mut mapper = unsafe { crate::memory::get_mapper() };
    let start_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(addr as u64));

    for i in 0..num_pages {
        let page = start_page + i as u64;
        if let Ok((frame, flush)) = mapper.unmap(page) {
            flush.flush();
            // Return the frame to the allocator
            crate::memory::with_frame_allocator(|alloc| {
                unsafe { alloc.deallocate_frame(frame); }
            });
        }
        // Ignore pages that weren't mapped
    }

    0 // Success
}

/// sys_ioctl - device control
///
/// Basic ioctl support. Currently handles terminal-related ioctls.
pub fn sys_ioctl(fd: usize, request: usize, _arg: usize) -> isize {
    // Common ioctl request codes
    const TCGETS: usize = 0x5401;      // Get terminal attributes
    const TCSETS: usize = 0x5402;      // Set terminal attributes
    const TIOCGWINSZ: usize = 0x5413;  // Get window size
    const _TIOCSWINSZ: usize = 0x5414;  // Set window size
    const FIONREAD: usize = 0x541B;    // Bytes available to read

    match request {
        TIOCGWINSZ => {
            // Return terminal window size (80x25 VGA text mode)
            if _arg != 0 {
                // struct winsize { unsigned short ws_row, ws_col, ws_xpixel, ws_ypixel; }
                let winsize = unsafe { &mut *(_arg as *mut [u16; 4]) };
                winsize[0] = 25;  // rows
                winsize[1] = 80;  // cols
                winsize[2] = 0;   // xpixel (unused)
                winsize[3] = 0;   // ypixel (unused)
            }
            0
        }

        TCGETS | TCSETS => {
            // Terminal attributes — return success (stub)
            // Real implementation would manage terminal modes (raw, cooked, etc.)
            if fd <= 2 {
                0 // Success for stdin/stdout/stderr
            } else {
                SyscallError::InvalidArgument.as_isize()
            }
        }

        FIONREAD => {
            // Return number of bytes available to read
            if fd == 0 {
                // STDIN: check keyboard queue
                let count = crate::task::keyboard::SCANCODE_QUEUE
                    .try_get()
                    .map(|q| q.len())
                    .unwrap_or(0);
                if _arg != 0 {
                    unsafe { *(_arg as *mut i32) = count as i32; }
                }
                0
            } else {
                0
            }
        }

        _ => {
            // Unknown ioctl — return ENOTTY for non-terminal fds
            SyscallError::NotImplemented.as_isize()
        }
    }
}

/// rt_sigaction(sig, *act, *oldact)
///
/// Linux ABI: act is { sa_handler: u64, sa_flags: u32, sa_restorer: u64,
/// sa_mask: u64 }.  We only honour the handler pointer; mask/flags are
/// stored but otherwise ignored.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigAction {
    pub sa_handler: u64,
    pub sa_flags:   u32,
    pub _padding:   u32,
    pub sa_restorer: u64,
    pub sa_mask:    u64,
}

pub fn sys_rt_sigaction(sig: u32, act_ptr: usize, oldact_ptr: usize) -> isize {
    if sig == 0 || sig > 31 {
        return SyscallError::InvalidArgument.as_isize();
    }
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    let pid = pm.current_pid.unwrap_or(0);
    let proc_ = match pm.get_process_mut(pid) {
        Some(p) => p,
        None => return SyscallError::FileNotFound.as_isize(),
    };

    if oldact_ptr != 0 {
        let old = SigAction {
            sa_handler: proc_.sigactions[sig as usize],
            sa_flags: 0,
            _padding: 0,
            sa_restorer: 0,
            sa_mask: 0,
        };
        unsafe { core::ptr::write(oldact_ptr as *mut SigAction, old); }
    }
    if act_ptr != 0 {
        let new = unsafe { core::ptr::read(act_ptr as *const SigAction) };
        proc_.sigactions[sig as usize] = new.sa_handler;
    }
    0
}

/// signal(sig, handler) — convenience: drop directly into the handler slot.
pub fn sys_signal(sig: u32, handler: u64) -> isize {
    if sig == 0 || sig > 31 {
        return SyscallError::InvalidArgument.as_isize();
    }
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    let pid = pm.current_pid.unwrap_or(0);
    let proc_ = match pm.get_process_mut(pid) {
        Some(p) => p,
        None => return SyscallError::FileNotFound.as_isize(),
    };
    let prev = proc_.sigactions[sig as usize];
    proc_.sigactions[sig as usize] = handler;
    prev as isize
}

/// setpgid(pid, pgid) — change a process' group ID.  pid=0 means caller.
pub fn sys_setpgid(pid: usize, pgid: usize) -> isize {
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    let target_pid = if pid == 0 {
        pm.current_pid.unwrap_or(0)
    } else {
        pid
    };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };
    match pm.get_process_mut(target_pid) {
        Some(p) => { p.pgid = new_pgid; 0 }
        None => SyscallError::FileNotFound.as_isize(),
    }
}

/// getpgid(pid) — read pgid of `pid` (or caller when pid=0).
pub fn sys_getpgid(pid: usize) -> isize {
    let pm = crate::process::PROCESS_MANAGER.lock();
    let target_pid = if pid == 0 { pm.current_pid.unwrap_or(0) } else { pid };
    match pm.get_process(target_pid) {
        Some(p) => p.pgid as isize,
        None => SyscallError::FileNotFound.as_isize(),
    }
}

/// getsid(pid) — read sid of `pid` (or caller when pid=0).
pub fn sys_getsid(pid: usize) -> isize {
    let pm = crate::process::PROCESS_MANAGER.lock();
    let target_pid = if pid == 0 { pm.current_pid.unwrap_or(0) } else { pid };
    match pm.get_process(target_pid) {
        Some(p) => p.sid as isize,
        None => SyscallError::FileNotFound.as_isize(),
    }
}

/// setsid() — create a new session: caller's sid and pgid both become its pid.
pub fn sys_setsid() -> isize {
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    let pid = match pm.current_pid {
        Some(p) => p,
        None => return SyscallError::FileNotFound.as_isize(),
    };
    if let Some(p) = pm.get_process_mut(pid) {
        p.sid = pid;
        p.pgid = pid;
    }
    pid as isize
}

/// getrlimit(resource, *rlimit) — read soft+hard.
pub fn sys_getrlimit(resource: u32, rlim_ptr: usize) -> isize {
    let res = match crate::rlimit::Resource::from_u32(resource) {
        Some(r) => r,
        None => return SyscallError::InvalidArgument.as_isize(),
    };
    if rlim_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }
    let lim = crate::rlimit::LIMITS.lock().get(res);
    unsafe { core::ptr::write(rlim_ptr as *mut crate::rlimit::Rlimit, lim) };
    0
}

/// setrlimit(resource, *rlimit) — update soft+hard.
pub fn sys_setrlimit(resource: u32, rlim_ptr: usize) -> isize {
    let res = match crate::rlimit::Resource::from_u32(resource) {
        Some(r) => r,
        None => return SyscallError::InvalidArgument.as_isize(),
    };
    if rlim_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }
    let new = unsafe { core::ptr::read(rlim_ptr as *const crate::rlimit::Rlimit) };
    let is_root = crate::users::CURRENT_CREDS.lock().uid == 0;
    match crate::rlimit::LIMITS.lock().set(res, new, is_root) {
        Ok(()) => 0,
        Err(_) => SyscallError::PermissionDenied.as_isize(),
    }
}

/// epoll_create1: create a new epoll instance, returning a non-negative epfd.
pub fn sys_epoll_create() -> isize {
    crate::syscall::epoll::epoll_create() as isize
}

/// epoll_ctl(epfd, op, fd, *event)
pub fn sys_epoll_ctl(epfd: i32, op: i32, fd: i32, event_ptr: usize) -> isize {
    if event_ptr == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }
    let ev = unsafe { core::ptr::read(event_ptr as *const crate::syscall::epoll::EpollEvent) };
    crate::syscall::epoll::epoll_ctl(epfd, op, fd, ev) as isize
}

/// epoll_wait(epfd, *events, maxevents, timeout_ms)
pub fn sys_epoll_wait(epfd: i32, events_ptr: usize, maxevents: usize, timeout_ms: i32) -> isize {
    if events_ptr == 0 || maxevents == 0 {
        return SyscallError::InvalidArgument.as_isize();
    }
    let out: &mut [crate::syscall::epoll::EpollEvent] = unsafe {
        core::slice::from_raw_parts_mut(events_ptr as *mut _, maxevents)
    };
    crate::syscall::epoll::epoll_wait(epfd, out, timeout_ms)
}

/// poll(struct pollfd *fds, nfds_t nfds, int timeout)
///
/// Performs the readiness check synchronously.  In our single-address-space
/// kernel the user pointer is treated as a kernel pointer (the userspace
/// region is identity-mapped).
pub fn sys_poll(fds_ptr: usize, nfds: usize, timeout_ms: i32) -> isize {
    use crate::syscall::poll::{do_poll, PollFd};

    if nfds == 0 {
        return 0;
    }
    if fds_ptr == 0 || nfds > 1024 {
        return SyscallError::InvalidArgument.as_isize();
    }

    // Safety: caller is trusted; identity-mapped user memory.
    let user_slice: &mut [PollFd] = unsafe {
        core::slice::from_raw_parts_mut(fds_ptr as *mut PollFd, nfds)
    };

    do_poll(user_slice, timeout_ms)
}

/// select(int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds,
///        struct timeval *timeout)
///
/// `timeout` here is interpreted as a millisecond integer pointer for
/// simplicity (a real Linux ABI would expect a timeval).  Pass 0 for
/// non-blocking, and a negative value for "wait forever".
pub fn sys_select(
    nfds: usize,
    readfds: usize,
    writefds: usize,
    exceptfds: usize,
    timeout_ms_ptr: usize,
) -> isize {
    use crate::syscall::poll::{do_select, FdSet};

    let mut empty = FdSet::empty();
    let r: *mut FdSet = if readfds == 0   { &mut empty } else { readfds as *mut FdSet };
    let w: *mut FdSet = if writefds == 0  { &mut empty } else { writefds as *mut FdSet };
    let e: *mut FdSet = if exceptfds == 0 { &mut empty } else { exceptfds as *mut FdSet };

    let timeout_ms: i32 = if timeout_ms_ptr == 0 {
        -1
    } else {
        unsafe { *(timeout_ms_ptr as *const i32) }
    };

    unsafe { do_select(nfds, &mut *r, &mut *w, &mut *e, timeout_ms) }
}
