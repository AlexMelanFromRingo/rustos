/// System call interface for user space programs
///
/// This module provides the syscall infrastructure for RustOS,
/// allowing user space programs (ring 3) to request services
/// from the kernel (ring 0).

pub mod numbers;
pub mod handler;
pub mod filedesc;

/// System call numbers
/// Based on Linux syscall ABI for x86_64
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum SyscallNumber {
    Read = 0,
    Write = 1,
    Open = 2,
    Close = 3,
    Lseek = 8,
    Dup = 32,
    Dup2 = 33,
    GetPid = 39,
    Fork = 57,
    Execve = 59,
    Exit = 60,
    Wait4 = 61,
    GetCwd = 79,
    Chdir = 80,
    Mkdir = 83,
    Rmdir = 84,
    ClockGetTime = 228,
}

impl SyscallNumber {
    pub fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(SyscallNumber::Read),
            1 => Some(SyscallNumber::Write),
            2 => Some(SyscallNumber::Open),
            3 => Some(SyscallNumber::Close),
            8 => Some(SyscallNumber::Lseek),
            32 => Some(SyscallNumber::Dup),
            33 => Some(SyscallNumber::Dup2),
            39 => Some(SyscallNumber::GetPid),
            57 => Some(SyscallNumber::Fork),
            59 => Some(SyscallNumber::Execve),
            60 => Some(SyscallNumber::Exit),
            61 => Some(SyscallNumber::Wait4),
            79 => Some(SyscallNumber::GetCwd),
            80 => Some(SyscallNumber::Chdir),
            83 => Some(SyscallNumber::Mkdir),
            84 => Some(SyscallNumber::Rmdir),
            228 => Some(SyscallNumber::ClockGetTime),
            _ => None,
        }
    }
}

/// System call result
pub type SyscallResult = Result<usize, SyscallError>;

/// System call errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(isize)]
pub enum SyscallError {
    BadFileDescriptor = -9,
    InvalidArgument = -22,
    PermissionDenied = -13,
    FileNotFound = -2,
    OutOfMemory = -12,
    NotImplemented = -38,
}

impl SyscallError {
    pub fn as_isize(self) -> isize {
        self as isize
    }
}

/// File descriptor type
pub type Fd = usize;

/// Standard file descriptors
pub const STDIN: Fd = 0;
pub const STDOUT: Fd = 1;
pub const STDERR: Fd = 2;

/// Syscall dispatcher - called from assembly syscall handler
///
/// Arguments follow x86_64 syscall ABI:
/// - rax: syscall number
/// - rdi: arg1
/// - rsi: arg2
/// - rdx: arg3
/// - r10: arg4
/// - r8: arg5
/// - r9: arg6
///
/// Returns value in rax (or negative error code)
pub fn syscall_dispatcher(
    syscall_num: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    _arg4: usize,
    _arg5: usize,
    _arg6: usize,
) -> isize {
    let syscall = match SyscallNumber::from_usize(syscall_num) {
        Some(sc) => sc,
        None => return SyscallError::NotImplemented.as_isize(),
    };

    match syscall {
        SyscallNumber::Read => handler::sys_read(arg1, arg2, arg3),
        SyscallNumber::Write => handler::sys_write(arg1, arg2, arg3),
        SyscallNumber::Open => handler::sys_open(arg1, arg2),
        SyscallNumber::Close => handler::sys_close(arg1),
        SyscallNumber::Lseek => handler::sys_lseek(arg1, arg2 as isize, arg3),
        SyscallNumber::Dup => handler::sys_dup(arg1),
        SyscallNumber::Dup2 => handler::sys_dup2(arg1, arg2),
        SyscallNumber::GetPid => handler::sys_getpid(),
        SyscallNumber::Fork => handler::sys_fork(),
        SyscallNumber::Execve => handler::sys_execve(arg1, arg2, arg3),
        SyscallNumber::Exit => handler::sys_exit(arg1),
        SyscallNumber::Wait4 => handler::sys_wait4(arg1, arg2, arg3),
        SyscallNumber::GetCwd => handler::sys_getcwd(arg1, arg2),
        SyscallNumber::Chdir => handler::sys_chdir(arg1),
        SyscallNumber::Mkdir => handler::sys_mkdir(arg1, arg2),
        SyscallNumber::Rmdir => handler::sys_rmdir(arg1),
        SyscallNumber::ClockGetTime => handler::sys_clock_gettime(arg1, arg2),
    }
}
