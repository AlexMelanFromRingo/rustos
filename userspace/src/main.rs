#![no_std]
#![no_main]

/// Simple hello world program for RustOS
/// Compiled as static ELF binary

use core::panic::PanicInfo;

/// Syscall numbers
const SYS_WRITE: usize = 1;
const SYS_EXIT: usize = 60;

/// File descriptors
const STDOUT: usize = 1;

/// Panic handler (required for no_std)
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    syscall_exit(1);
}

/// Entry point
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let msg = b"Hello from userspace ELF program!\n";
    syscall_write(STDOUT, msg.as_ptr(), msg.len());

    let msg2 = b"This is a real ELF binary loaded from filesystem!\n";
    syscall_write(STDOUT, msg2.as_ptr(), msg2.len());

    syscall_exit(0);
}

/// Write syscall
#[inline(always)]
fn syscall_write(fd: usize, buf: *const u8, count: usize) -> isize {
    let result: isize;
    unsafe {
        core::arch::asm!(
            "mov rax, {syscall}",
            "mov rdi, {fd}",
            "mov rsi, {buf}",
            "mov rdx, {count}",
            "syscall",
            syscall = const SYS_WRITE,
            fd = in(reg) fd,
            buf = in(reg) buf,
            count = in(reg) count,
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }
    result
}

/// Exit syscall
#[inline(always)]
fn syscall_exit(code: usize) -> ! {
    unsafe {
        core::arch::asm!(
            "mov rax, {syscall}",
            "mov rdi, {code}",
            "syscall",
            syscall = const SYS_EXIT,
            code = in(reg) code,
            options(noreturn)
        );
    }
}
