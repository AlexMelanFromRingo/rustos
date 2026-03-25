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
    println!("\nProcess exited with code: {}", exit_code);

    // Exit current process (becomes zombie for parent to reap)
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(pid) = pm.current_pid {
        pm.exit(pid, exit_code as i32);
        pm.current_pid = None;  // Clear current process
    }
    drop(pm);

    // Yield to scheduler - this should switch to another process
    crate::process::scheduler::schedule();

    // If we reach here, there are no other processes to run
    // Restore kernel context and return to shell
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

/// sys_wait4 - wait for child process to exit
pub fn sys_wait4(pid: usize, wstatus: usize, _options: usize) -> isize {
    let mut pm = PROCESS_MANAGER.lock();

    // Get current process ID
    let parent_pid = match pm.current_pid {
        Some(pid) => pid,
        None => return SyscallError::InvalidArgument.as_isize(),
    };

    // Wait for any child if pid == -1
    let result = if pid == (-1isize) as usize {
        pm.wait(parent_pid)
    } else {
        // Wait for specific child
        pm.wait(parent_pid).filter(|(child_pid, _)| *child_pid == pid)
    };

    match result {
        Some((child_pid, exit_code)) => {
            println!("wait4: child {} exited with code {}", child_pid, exit_code);

            // Write exit status if pointer provided
            if wstatus != 0 {
                unsafe {
                    let status_ptr = wstatus as *mut i32;
                    *status_ptr = exit_code << 8;  // Linux wait status format
                }
            }

            child_pid as isize
        }
        None => {
            // No zombie children yet
            // In a real OS, we would block the process here
            // For now, just return 0 (would wait)
            0
        }
    }
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
            // CLOCK_MONOTONIC - uptime in ticks converted to seconds
            let ticks = crate::task::timer::current_ticks();
            let seconds = ticks / 18; // ~18.2 Hz PIT
            let remainder_ticks = ticks % 18;
            let nsec = (remainder_ticks * 1_000_000_000) / 18;

            unsafe {
                let ts = timespec_ptr as *mut [i64; 2];
                (*ts)[0] = seconds as i64;
                (*ts)[1] = nsec as i64;
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

/// Fill a LinuxStat struct for a given path
fn fill_stat(path: &str, stat_ptr: usize) -> isize {
    use crate::fs::vfs::VfsContext;

    // Check if path exists and get info
    let is_dir = VfsContext::is_directory(path);
    let file_size = if is_dir {
        0i64
    } else {
        // Try to read file to get size
        let data = {
            if let Some(ref fs) = *FAT32.lock() {
                fs.read(path).ok()
            } else {
                RAMDISK.lock().read(path).ok()
            }
        };
        match data {
            Some(d) => d.len() as i64,
            None => {
                if !is_dir {
                    return SyscallError::FileNotFound.as_isize();
                }
                0
            }
        }
    };

    // Get current time from RTC for timestamps
    let dt = crate::drivers::rtc::read_datetime();
    let now = dt.to_unix_timestamp() as i64;

    let mode = if is_dir {
        S_IFDIR | 0o755
    } else {
        S_IFREG | 0o644
    };

    let blocks = (file_size + 511) / 512;

    // Simple inode: hash the path
    let ino = path.bytes().fold(1u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));

    let stat = LinuxStat {
        st_dev: 0,
        st_ino: ino,
        st_nlink: 1,
        st_mode: mode,
        st_uid: 0,   // root
        st_gid: 0,   // root
        __pad0: 0,
        st_rdev: 0,
        st_size: file_size,
        st_blksize: 4096,
        st_blocks: blocks,
        st_atime: now,
        st_atime_nsec: 0,
        st_mtime: now,
        st_mtime_nsec: 0,
        st_ctime: now,
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

    // Accept the syscall (per-process cwd tracking is handled by shell's current_dir)
    0
}
