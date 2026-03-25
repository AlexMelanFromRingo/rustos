/// CPU context structures for task switching
///
/// Two context types:
/// - `Context`: Callee-saved registers only, for kernel-to-kernel cooperative switching
/// - `TrapFrame`: Full CPU state pushed by interrupt, for preemptive user-mode switching

/// Callee-saved context for kernel cooperative switching
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Context {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

impl Context {
    pub fn new(entry_point: usize, stack_pointer: usize) -> Self {
        Context {
            r15: 0, r14: 0, r13: 0, r12: 0, rbx: 0, rbp: 0,
            rip: entry_point as u64,
            rsp: stack_pointer as u64,
            rflags: 0x200,
        }
    }

    pub const fn default() -> Self {
        Context {
            r15: 0, r14: 0, r13: 0, r12: 0, rbx: 0, rbp: 0,
            rip: 0, rsp: 0, rflags: 0x200,
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Context::default()
    }
}

/// Full trap frame saved on interrupt from user mode.
///
/// Layout matches what our naked timer ISR pushes onto the kernel stack:
/// First we push all GPRs (in a fixed order), then the CPU-pushed interrupt frame
/// is below them on the stack.
///
/// Stack layout (low address = top of struct):
///   [GPRs pushed by software]  <- saved by our ISR
///   [interrupt frame pushed by CPU: RIP, CS, RFLAGS, RSP, SS]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TrapFrame {
    // Software-saved general purpose registers (pushed in this order by ISR)
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,

    // Hardware-pushed interrupt frame (pushed by CPU on interrupt)
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    /// Create a trap frame for a new user process.
    /// Sets up the interrupt frame so IRETQ will jump to `entry` in Ring 3.
    pub fn new_user(entry: u64, user_stack_top: u64, user_cs: u64, user_ss: u64) -> Self {
        TrapFrame {
            // GPRs all zero
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0,
            r9: 0, r8: 0, rbp: 0, rdi: 0, rsi: 0, rdx: 0,
            rcx: 0, rbx: 0, rax: 0,
            // Interrupt frame: IRETQ pops these to enter Ring 3
            rip: entry,
            cs: user_cs,
            rflags: 0x202, // IF=1, reserved bit 1
            rsp: user_stack_top,
            ss: user_ss,
        }
    }

    pub const fn empty() -> Self {
        TrapFrame {
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0,
            r9: 0, r8: 0, rbp: 0, rdi: 0, rsi: 0, rdx: 0,
            rcx: 0, rbx: 0, rax: 0,
            rip: 0, cs: 0, rflags: 0x202, rsp: 0, ss: 0,
        }
    }

    /// Returns true if this trap frame was from user mode (RPL=3 in CS)
    pub fn is_user(&self) -> bool {
        self.cs & 3 == 3
    }
}

/// Switch from the current context to the next context (kernel cooperative)
///
/// # Safety
/// This function directly manipulates CPU registers.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(current: *mut Context, next: *const Context) {
    core::arch::naked_asm!(
        // Save current context
        "mov [rdi + 0x00], r15",
        "mov [rdi + 0x08], r14",
        "mov [rdi + 0x10], r13",
        "mov [rdi + 0x18], r12",
        "mov [rdi + 0x20], rbx",
        "mov [rdi + 0x28], rbp",
        "mov rax, [rsp]",
        "mov [rdi + 0x30], rax",      // Save RIP (return address)
        "lea rax, [rsp + 8]",
        "mov [rdi + 0x38], rax",      // Save RSP
        "pushfq",
        "pop rax",
        "mov [rdi + 0x40], rax",      // Save RFLAGS

        // Load next context
        "mov r15, [rsi + 0x00]",
        "mov r14, [rsi + 0x08]",
        "mov r13, [rsi + 0x10]",
        "mov r12, [rsi + 0x18]",
        "mov rbx, [rsi + 0x20]",
        "mov rbp, [rsi + 0x28]",
        "mov rax, [rsi + 0x40]",
        "push rax",
        "popfq",
        "mov rsp, [rsi + 0x38]",
        "mov rax, [rsi + 0x30]",
        "jmp rax",
    )
}
