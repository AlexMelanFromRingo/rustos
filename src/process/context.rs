/// CPU context for task switching
///
/// This structure holds all general-purpose registers that need to be
/// saved and restored during a context switch on x86_64.

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Context {
    // Callee-saved registers (must be preserved across function calls)
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,

    // Instruction pointer and stack pointer
    pub rip: u64,
    pub rsp: u64,

    // RFLAGS register
    pub rflags: u64,
}

impl Context {
    /// Create a new context with the given entry point and stack pointer
    pub fn new(entry_point: usize, stack_pointer: usize) -> Self {
        Context {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbx: 0,
            rbp: 0,
            rip: entry_point as u64,
            rsp: stack_pointer as u64,
            rflags: 0x200, // Interrupt enable flag set
        }
    }

    /// Create a default empty context
    pub const fn default() -> Self {
        Context {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbx: 0,
            rbp: 0,
            rip: 0,
            rsp: 0,
            rflags: 0x200,
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::default()
    }
}

/// Switch from the current context to the next context
///
/// # Safety
/// This function directly manipulates CPU registers and must only be called
/// with valid context pointers. The contexts must outlive the function call.
///
/// # Arguments
/// * `current` - Pointer to the current context (will be saved here)
/// * `next` - Pointer to the next context (will be loaded from here)
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(current: *mut Context, next: *const Context) {
    core::arch::naked_asm!(
        // Save current context
        "mov [rdi + 0x00], r15",      // Save r15
        "mov [rdi + 0x08], r14",      // Save r14
        "mov [rdi + 0x10], r13",      // Save r13
        "mov [rdi + 0x18], r12",      // Save r12
        "mov [rdi + 0x20], rbx",      // Save rbx
        "mov [rdi + 0x28], rbp",      // Save rbp

        // Save return address (rip) - the address after call instruction
        "mov rax, [rsp]",
        "mov [rdi + 0x30], rax",      // Save rip

        // Save stack pointer (before call)
        "lea rax, [rsp + 8]",         // Skip return address
        "mov [rdi + 0x38], rax",      // Save rsp

        // Save rflags
        "pushfq",
        "pop rax",
        "mov [rdi + 0x40], rax",      // Save rflags

        // Load next context
        "mov r15, [rsi + 0x00]",      // Restore r15
        "mov r14, [rsi + 0x08]",      // Restore r14
        "mov r13, [rsi + 0x10]",      // Restore r13
        "mov r12, [rsi + 0x18]",      // Restore r12
        "mov rbx, [rsi + 0x20]",      // Restore rbx
        "mov rbp, [rsi + 0x28]",      // Restore rbp

        // Restore rflags
        "mov rax, [rsi + 0x40]",
        "push rax",
        "popfq",

        // Restore stack pointer
        "mov rsp, [rsi + 0x38]",      // Restore rsp

        // Jump to next instruction pointer
        "mov rax, [rsi + 0x30]",
        "jmp rax",                     // Jump to rip (this also serves as ret)
    )
}
