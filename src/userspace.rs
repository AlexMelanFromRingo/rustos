/// User space support - transitioning to Ring 3 and running user code
///
/// This module provides functions for:
/// - Switching from kernel mode (Ring 0) to user mode (Ring 3)
/// - Running user space code with restricted privileges
/// - Making system calls from user space

use crate::gdt;
use crate::memory::user_allocator;
use x86_64::VirtAddr;
use core::sync::atomic::{AtomicBool, Ordering};

/// Saved kernel context for returning from user mode
///
/// This structure holds all callee-saved registers according to
/// the System V AMD64 ABI calling convention.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct KernelContext {
    // Callee-saved registers (must be preserved across function calls)
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rsp: u64,
    rip: u64,  // Return address
}

/// Global storage for saved kernel context
static mut SAVED_CONTEXT: KernelContext = KernelContext {
    rbx: 0, rbp: 0, r12: 0, r13: 0, r14: 0, r15: 0, rsp: 0, rip: 0,
};

/// Flag indicating whether we have a saved context
static HAS_SAVED_CONTEXT: AtomicBool = AtomicBool::new(false);

/// Copy code to user space
///
/// Copies kernel code to identity-mapped user space so it can run in Ring 3.
unsafe fn copy_to_user_space(code_ptr: *const u8, code_size: usize) -> Option<VirtAddr> {
    // Allocate user space memory
    let user_addr = user_allocator::allocate_user_code(code_size)?;

    // Copy code to user space
    let user_ptr = user_addr.as_u64() as *mut u8;
    core::ptr::copy_nonoverlapping(code_ptr, user_ptr, code_size);

    Some(user_addr)
}

/// Jump to user mode and execute a function
///
/// This function transitions the CPU from Ring 0 (kernel) to Ring 3 (user)
/// and executes the provided function in user mode.
///
/// The function is copied to identity-mapped user space and executed there.
///
/// # Safety
/// This is unsafe because:
/// - It manipulates CPU privilege levels
/// - It switches stacks
/// - The user function must be position-independent
pub unsafe fn jump_to_usermode(kernel_fn: usize, fn_size: usize) -> ! {
    // Copy function to user space
    let user_fn_addr = copy_to_user_space(kernel_fn as *const u8, fn_size)
        .expect("Failed to allocate user space for code");

    // Allocate user stack in identity-mapped region
    let (stack_bottom, stack_size) = user_allocator::allocate_user_stack()
        .expect("Failed to allocate user stack");

    let stack_top = stack_bottom.as_u64() + stack_size;

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
        rsp = in(reg) stack_top,
        rflags = in(reg) rflags,
        cs = in(reg) user_cs,
        rip = in(reg) user_fn_addr.as_u64(),
        options(noreturn)
    );
}

/// Example user mode function that makes syscalls
///
/// This runs in Ring 3 and can only access kernel via syscalls.
/// Must be position-independent (no absolute addresses).
#[no_mangle]
pub extern "C" fn user_mode_demo() {
    // Static strings are in .rodata, which is in kernel space
    // We need to use stack-allocated arrays instead

    // Message 1
    let msg1 = *b"Hello from Ring 3 user space!\n\0";
    syscall_write(1, msg1.as_ptr(), msg1.len() - 1);

    // Message 2
    let msg2 = *b"System calls are working!\n\0";
    syscall_write(1, msg2.as_ptr(), msg2.len() - 1);

    // Get PID
    let _pid = syscall_getpid();
    // Can't print PID easily without kernel functions, so just call it

    // Message 3
    let msg3 = *b"Exiting from user mode...\n\0";
    syscall_write(1, msg3.as_ptr(), msg3.len() - 1);

    // Exit
    syscall_exit(0);
}

/// Syscall helper: write
#[inline(always)]
fn syscall_write(fd: usize, buf: *const u8, count: usize) -> isize {
    let result: isize;
    unsafe {
        core::arch::asm!(
            "mov rax, 1",              // SYS_WRITE
            "mov rdi, {fd}",
            "mov rsi, {buf}",
            "mov rdx, {count}",
            "syscall",
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

/// Syscall helper: getpid
#[inline(always)]
fn syscall_getpid() -> isize {
    let result: isize;
    unsafe {
        core::arch::asm!(
            "mov rax, 39",             // SYS_GETPID
            "syscall",
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }
    result
}

/// Syscall helper: exit
#[inline(always)]
fn syscall_exit(code: usize) -> ! {
    unsafe {
        core::arch::asm!(
            "mov rax, 60",             // SYS_EXIT
            "mov rdi, {code}",
            "syscall",
            code = in(reg) code,
            options(noreturn)
        );
    }
}

/// Get size of user_mode_demo function
/// This is approximate - we use a fixed size for now
pub fn get_demo_size() -> usize {
    // The function is small, 4KB should be enough
    4096
}

/// Jump to Ring 3 with arbitrary entry point and stack
///
/// This is a more generic version of jump_to_usermode that doesn't
/// copy code - it assumes the code is already in user space.
///
/// # Arguments
/// * `entry_point` - Virtual address of code to execute (must be in user space)
/// * `stack_addr` - Virtual address of stack bottom (must be in user space)
/// * `stack_size` - Size of stack in bytes
///
/// # Safety
/// - entry_point must point to valid user space code
/// - stack_addr must point to valid user space stack
/// - Both addresses must be in identity-mapped region
pub unsafe fn jump_to_ring3(entry_point: u64, stack_addr: u64, stack_size: u64) -> ! {
    let stack_top = stack_addr + stack_size;

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

    // IRETQ stack frame
    let rflags: u64 = 0x202; // IF=1, Reserved bit=1

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
        rsp = in(reg) stack_top,
        rflags = in(reg) rflags,
        cs = in(reg) user_cs,
        rip = in(reg) entry_point,
        options(noreturn)
    );
}

/// Save current kernel context before jumping to user mode
///
/// This function saves all callee-saved registers so we can return
/// to the exact same point after the user process exits.
///
/// # Safety
/// Must be called from a function that will later call jump_to_ring3
unsafe fn save_kernel_context_full() {
    core::arch::asm!(
        // Save all callee-saved registers to SAVED_CONTEXT
        "lea rax, [rip + {saved_context}]",
        "mov [rax + 0], rbx",      // offset 0: rbx
        "mov [rax + 8], rbp",      // offset 8: rbp
        "mov [rax + 16], r12",     // offset 16: r12
        "mov [rax + 24], r13",     // offset 24: r13
        "mov [rax + 32], r14",     // offset 32: r14
        "mov [rax + 40], r15",     // offset 40: r15
        "mov [rax + 48], rsp",     // offset 48: rsp
        // Save return address (top of stack)
        "mov rcx, [rsp]",
        "mov [rax + 56], rcx",     // offset 56: rip (return address)
        saved_context = sym SAVED_CONTEXT,
        out("rax") _,
        out("rcx") _,
    );
    
    HAS_SAVED_CONTEXT.store(true, Ordering::SeqCst);
}

/// Restore kernel context and return from user mode
///
/// This function restores all saved registers and jumps back to
/// the point where save_kernel_context_full was called.
///
/// # Safety
/// Must only be called from sys_exit when process has cleanly exited
pub unsafe fn restore_kernel_context_and_return() -> ! {
    if !HAS_SAVED_CONTEXT.load(Ordering::SeqCst) {
        // No saved context - halt system
        crate::println!("\n[Kernel] No saved context. System halted.");
        crate::hlt_loop();
    }
    
    HAS_SAVED_CONTEXT.store(false, Ordering::SeqCst);
    
    core::arch::asm!(
        // Load address of SAVED_CONTEXT
        "lea rax, [rip + {saved_context}]",
        // Restore all callee-saved registers
        "mov rbx, [rax + 0]",
        "mov rbp, [rax + 8]",
        "mov r12, [rax + 16]",
        "mov r13, [rax + 24]",
        "mov r14, [rax + 32]",
        "mov r15, [rax + 40]",
        "mov rsp, [rax + 48]",
        "mov rcx, [rax + 56]",    // Load return address
        // Jump to return address (back to exec_with_return_proper)
        "jmp rcx",
        saved_context = sym SAVED_CONTEXT,
        options(noreturn)
    );
}

/// Execute user code with proper context saving
///
/// This wrapper saves full kernel context before jumping to ring 3,
/// allowing sys_exit to restore everything and return cleanly.
pub unsafe fn exec_with_return_proper(entry_point: u64, stack_bottom: u64, stack_size: u64) {
    // Save ALL callee-saved registers
    save_kernel_context_full();
    
    // Jump to user mode - if process calls exit, restore_kernel_context_and_return()
    // will restore our registers and jump back here
    jump_to_ring3(entry_point, stack_bottom, stack_size);
    
    // We'll return here after process exits!
    // jump_to_ring3 is marked noreturn, but we manually jump back
}
