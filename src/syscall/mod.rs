/// System call interface for user space programs
///
/// This module provides the syscall infrastructure for RustOS,
/// allowing user space programs (ring 3) to request services
/// from the kernel (ring 0).

pub mod numbers;
pub mod handler;
pub mod filedesc;
pub mod pipe;
pub mod poll;
pub mod epoll;

/// System call numbers
/// Based on Linux syscall ABI for x86_64
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum SyscallNumber {
    Read = 0,
    Write = 1,
    Open = 2,
    Close = 3,
    Stat = 4,
    Fstat = 5,
    Lseek = 8,
    Brk = 12,
    Pipe = 22,
    Dup = 32,
    Dup2 = 33,
    GetPid = 39,
    Fork = 57,
    Execve = 59,
    Exit = 60,
    Kill = 62,
    Wait4 = 61,
    GetCwd = 79,
    Chdir = 80,
    Rename = 82,
    Mkdir = 83,
    Rmdir = 84,
    Unlink = 87,
    GetUid = 102,
    GetGid = 104,
    GetEuid = 107,
    GetEgid = 108,
    Mmap = 9,
    Munmap = 11,
    Ioctl = 16,
    Nanosleep = 35,
    ClockGetTime = 228,
    Poll = 7,
    Select = 23,
    EpollCreate = 213,
    EpollCtl = 233,
    EpollWait = 232,
    GetRlimit = 97,
    SetRlimit = 160,
    SetPgid = 109,
    GetPgid = 121,
    GetSid = 124,
    SetSid = 112,
    RtSigaction = 13,
    Signal = 48, // not the linux number but close enough; treated as compat alias
}

impl SyscallNumber {
    pub fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(SyscallNumber::Read),
            1 => Some(SyscallNumber::Write),
            2 => Some(SyscallNumber::Open),
            3 => Some(SyscallNumber::Close),
            4 => Some(SyscallNumber::Stat),
            5 => Some(SyscallNumber::Fstat),
            8 => Some(SyscallNumber::Lseek),
            9 => Some(SyscallNumber::Mmap),
            11 => Some(SyscallNumber::Munmap),
            12 => Some(SyscallNumber::Brk),
            16 => Some(SyscallNumber::Ioctl),
            22 => Some(SyscallNumber::Pipe),
            32 => Some(SyscallNumber::Dup),
            33 => Some(SyscallNumber::Dup2),
            35 => Some(SyscallNumber::Nanosleep),
            39 => Some(SyscallNumber::GetPid),
            57 => Some(SyscallNumber::Fork),
            59 => Some(SyscallNumber::Execve),
            60 => Some(SyscallNumber::Exit),
            62 => Some(SyscallNumber::Kill),
            61 => Some(SyscallNumber::Wait4),
            79 => Some(SyscallNumber::GetCwd),
            80 => Some(SyscallNumber::Chdir),
            82 => Some(SyscallNumber::Rename),
            83 => Some(SyscallNumber::Mkdir),
            84 => Some(SyscallNumber::Rmdir),
            87 => Some(SyscallNumber::Unlink),
            102 => Some(SyscallNumber::GetUid),
            104 => Some(SyscallNumber::GetGid),
            107 => Some(SyscallNumber::GetEuid),
            108 => Some(SyscallNumber::GetEgid),
            228 => Some(SyscallNumber::ClockGetTime),
            7 => Some(SyscallNumber::Poll),
            23 => Some(SyscallNumber::Select),
            213 => Some(SyscallNumber::EpollCreate),
            232 => Some(SyscallNumber::EpollWait),
            233 => Some(SyscallNumber::EpollCtl),
            97 => Some(SyscallNumber::GetRlimit),
            160 => Some(SyscallNumber::SetRlimit),
            109 => Some(SyscallNumber::SetPgid),
            121 => Some(SyscallNumber::GetPgid),
            124 => Some(SyscallNumber::GetSid),
            112 => Some(SyscallNumber::SetSid),
            13 => Some(SyscallNumber::RtSigaction),
            48 => Some(SyscallNumber::Signal),
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
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    // Seccomp gate: if the current process has a filter installed, consult
    // it before dispatching.  Errno → return -EPERM; Kill → terminate.
    {
        let pid = crate::process::PROCESS_MANAGER.lock().current_pid.unwrap_or(0);
        match crate::seccomp::check(pid, syscall_num) {
            crate::seccomp::Action::Allow => {}
            crate::seccomp::Action::Log => {
                crate::syslog::log(
                    crate::syslog::Facility::Authpriv,
                    crate::syslog::Severity::Notice,
                    "seccomp",
                    alloc::format!("pid={} sc={}", pid, syscall_num),
                );
            }
            crate::seccomp::Action::Errno => {
                return SyscallError::PermissionDenied.as_isize();
            }
            crate::seccomp::Action::Kill => {
                let mut pm = crate::process::PROCESS_MANAGER.lock();
                pm.exit(pid, 137);
                return SyscallError::PermissionDenied.as_isize();
            }
        }
    }

    let syscall = match SyscallNumber::from_usize(syscall_num) {
        Some(sc) => sc,
        None => return SyscallError::NotImplemented.as_isize(),
    };

    match syscall {
        SyscallNumber::Read => handler::sys_read(arg1, arg2, arg3),
        SyscallNumber::Write => handler::sys_write(arg1, arg2, arg3),
        SyscallNumber::Open => handler::sys_open(arg1, arg2),
        SyscallNumber::Close => handler::sys_close(arg1),
        SyscallNumber::Stat => handler::sys_stat(arg1, arg2),
        SyscallNumber::Fstat => handler::sys_fstat(arg1, arg2),
        SyscallNumber::Lseek => handler::sys_lseek(arg1, arg2 as isize, arg3),
        SyscallNumber::Mmap => handler::sys_mmap(arg1, arg2, arg3, arg4, arg5 as isize, arg6),
        SyscallNumber::Munmap => handler::sys_munmap(arg1, arg2),
        SyscallNumber::Brk => handler::sys_brk(arg1),
        SyscallNumber::Ioctl => handler::sys_ioctl(arg1, arg2, arg3),
        SyscallNumber::Pipe => handler::sys_pipe(arg1),
        SyscallNumber::Dup => handler::sys_dup(arg1),
        SyscallNumber::Dup2 => handler::sys_dup2(arg1, arg2),
        SyscallNumber::Nanosleep => handler::sys_nanosleep(arg1, arg2),
        SyscallNumber::GetPid => handler::sys_getpid(),
        SyscallNumber::Fork => handler::sys_fork(),
        SyscallNumber::Execve => handler::sys_execve(arg1, arg2, arg3),
        SyscallNumber::Exit => handler::sys_exit(arg1),
        SyscallNumber::Kill => handler::sys_kill(arg1, arg2),
        SyscallNumber::Wait4 => handler::sys_wait4(arg1, arg2, arg3),
        SyscallNumber::GetCwd => handler::sys_getcwd(arg1, arg2),
        SyscallNumber::Chdir => handler::sys_chdir(arg1),
        SyscallNumber::Mkdir => handler::sys_mkdir(arg1, arg2),
        SyscallNumber::Rename => handler::sys_rename(arg1, arg2),
        SyscallNumber::Rmdir => handler::sys_rmdir(arg1),
        SyscallNumber::Unlink => handler::sys_unlink(arg1),
        SyscallNumber::GetUid => handler::sys_getuid(),
        SyscallNumber::GetGid => handler::sys_getgid(),
        SyscallNumber::GetEuid => handler::sys_getuid(),   // Same as getuid (single user)
        SyscallNumber::GetEgid => handler::sys_getgid(),   // Same as getgid (single user)
        SyscallNumber::ClockGetTime => handler::sys_clock_gettime(arg1, arg2),
        SyscallNumber::Poll => handler::sys_poll(arg1, arg2, arg3 as i32),
        SyscallNumber::Select => handler::sys_select(arg1, arg2, arg3, arg4, arg5),
        SyscallNumber::EpollCreate => handler::sys_epoll_create(),
        SyscallNumber::EpollCtl => handler::sys_epoll_ctl(arg1 as i32, arg2 as i32, arg3 as i32, arg4),
        SyscallNumber::EpollWait => handler::sys_epoll_wait(arg1 as i32, arg2, arg3, arg4 as i32),
        SyscallNumber::GetRlimit => handler::sys_getrlimit(arg1 as u32, arg2),
        SyscallNumber::SetRlimit => handler::sys_setrlimit(arg1 as u32, arg2),
        SyscallNumber::SetPgid => handler::sys_setpgid(arg1, arg2),
        SyscallNumber::GetPgid => handler::sys_getpgid(arg1),
        SyscallNumber::GetSid => handler::sys_getsid(arg1),
        SyscallNumber::SetSid => handler::sys_setsid(),
        SyscallNumber::RtSigaction => handler::sys_rt_sigaction(arg1 as u32, arg2, arg3),
        SyscallNumber::Signal => handler::sys_signal(arg1 as u32, arg2 as u64),
    }
}
