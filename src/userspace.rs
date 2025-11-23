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

/// Global storage for saved kernel stack pointer
///
/// When we context switch to user mode, all callee-saved registers
/// are pushed onto the stack, and RSP is saved here.
/// On return from user mode, we restore RSP and pop all registers.
static mut SAVED_RSP: u64 = 0;

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

/// Restore kernel context and return from user mode
///
/// This function restores RSP and pops all callee-saved registers,
/// then returns to the caller of exec_with_return_proper.
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
        // Restore stack pointer
        "lea rax, [rip + {saved_rsp}]",
        "mov rsp, [rax]",
        // Pop all callee-saved registers in reverse order
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        // Return to caller of exec_with_return_proper
        // (return address is now on top of stack)
        "ret",
        saved_rsp = sym SAVED_RSP,
        options(noreturn)
    );
}

/// Execute user code with proper context saving
///
/// Naked function to completely control stack layout.
/// Saves all callee-saved registers, then jumps to user mode via IRETQ.
///
/// Arguments (System V ABI):
/// - rdi: entry_point
/// - rsi: stack_bottom
/// - rdx: stack_size
#[unsafe(naked)]
pub unsafe extern "C" fn exec_with_return_proper(entry_point: u64, stack_bottom: u64, stack_size: u64) -> ! {
    core::arch::naked_asm!(
        // Save all callee-saved registers (System V ABI)
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Save current stack pointer
        "lea rax, [rip + {saved_rsp}]",
        "mov [rax], rsp",

        // Set context available flag
        "lea rax, [rip + {has_context}]",
        "mov byte ptr [rax], 1",

        // Calculate stack top: rsi (stack_bottom) + rdx (stack_size)
        "mov rax, rsi",
        "add rax, rdx",           // rax = stack_top

        // Get user segment selectors (match GDT layout for SYSRET)
        // GDT order: null(0), kernel_code(1), kernel_data(2), user_data(3), user_code(4), TSS(5)
        // CRITICAL: user_data BEFORE user_code for SYSRET to work!
        // Selectors: index * 8 + RPL(3)
        "mov r8, 0x1b",           // User data selector: index 3, RPL=3 → 3*8+3 = 0x1b
        "mov r9, 0x23",           // User code selector: index 4, RPL=3 → 4*8+3 = 0x23

        // Set user data segments
        "mov ds, r8w",
        "mov es, r8w",
        "mov fs, r8w",
        "mov gs, r8w",

        // Build IRETQ frame on stack
        "push r8",                // SS (user data)
        "push rax",               // RSP (stack_top)
        "push 0x202",             // RFLAGS (IF=1, reserved=1)
        "push r9",                // CS (user code)
        "push rdi",               // RIP (entry_point)

        // Jump to user mode
        "iretq",

        saved_rsp = sym SAVED_RSP,
        has_context = sym HAS_SAVED_CONTEXT,
    );
}
