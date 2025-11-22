/// User space support - transitioning to Ring 3 and running user code
///
/// This module provides functions for:
/// - Switching from kernel mode (Ring 0) to user mode (Ring 3)
/// - Running user space code with restricted privileges
/// - Making system calls from user space

use crate::gdt;
use alloc::vec::Vec;

/// User space stack size (16 KB)
const USER_STACK_SIZE: usize = 4096 * 4;

/// Jump to user mode and execute a function
///
/// This function transitions the CPU from Ring 0 (kernel) to Ring 3 (user)
/// and executes the provided function pointer in user mode.
///
/// # Safety
/// This is unsafe because:
/// - It manipulates CPU privilege levels
/// - It switches stacks
/// - The user function must be trusted not to corrupt kernel memory
pub unsafe fn jump_to_usermode(user_fn: usize) -> ! {
    // Allocate user stack
    let mut user_stack = Vec::with_capacity(USER_STACK_SIZE);
    user_stack.resize(USER_STACK_SIZE, 0);
    let user_stack_top = user_stack.as_ptr() as u64 + USER_STACK_SIZE as u64;

    // Leak the stack so it doesn't get dropped
    core::mem::forget(user_stack);

    // Get user segment selectors
    let user_cs = gdt::user_code_selector().0 as u64;
    let user_ds = gdt::user_data_selector().0 as u64;

    // Set user data segments
    core::arch::asm!(
        "mov ds, {0:x}",
        "mov es, {0:x}",
        "mov fs, {0:x}",
        "mov gs, {0:x}",
        in(reg) user_ds,
    );

    // IRETQ stack frame (from bottom to top):
    // 1. RIP (user function address)
    // 2. CS (user code selector)
    // 3. RFLAGS (with IF=1 for interrupts)
    // 4. RSP (user stack pointer)
    // 5. SS (user data selector)

    let rflags: u64 = 0x202; // IF=1 (interrupts enabled), Reserved bit=1

    core::arch::asm!(
        // Push IRETQ frame
        "push {ss}",           // SS
        "push {rsp}",          // RSP
        "push {rflags}",       // RFLAGS
        "push {cs}",           // CS
        "push {rip}",          // RIP

        // Execute IRETQ to jump to user mode
        "iretq",

        ss = in(reg) user_ds,
        rsp = in(reg) user_stack_top,
        rflags = in(reg) rflags,
        cs = in(reg) user_cs,
        rip = in(reg) user_fn,
        options(noreturn)
    );
}

/// Example user mode function that makes syscalls
///
/// This runs in Ring 3 and can only access kernel via syscalls
#[no_mangle]
pub extern "C" fn user_mode_demo() {
    // Make a sys_write syscall to print a message
    let message = b"Hello from user space!\n";

    // Syscall: write(STDOUT, message, len)
    // rax = syscall number (1 = write)
    // rdi = fd (1 = STDOUT)
    // rsi = buffer pointer
    // rdx = length
    let result: isize;
    unsafe {
        core::arch::asm!(
            "mov rax, 1",              // SYS_WRITE
            "mov rdi, 1",              // STDOUT
            "mov rsi, {buf}",          // message buffer
            "mov rdx, {len}",          // message length
            "syscall",                 // Make the system call
            buf = in(reg) message.as_ptr(),
            len = in(reg) message.len(),
            lateout("rax") result,
            lateout("rcx") _,  // SYSCALL clobbers RCX
            lateout("r11") _,  // SYSCALL clobbers R11
        );
    }

    // Make another syscall
    let message2 = b"This is a system call from Ring 3!\n";
    unsafe {
        core::arch::asm!(
            "mov rax, 1",
            "mov rdi, 1",
            "mov rsi, {buf}",
            "mov rdx, {len}",
            "syscall",
            buf = in(reg) message2.as_ptr(),
            len = in(reg) message2.len(),
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // Exit syscall (60 = sys_exit)
    unsafe {
        core::arch::asm!(
            "mov rax, 60",             // SYS_EXIT
            "mov rdi, 0",              // exit code 0
            "syscall",
            options(noreturn)
        );
    }
}
