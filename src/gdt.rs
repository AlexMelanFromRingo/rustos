use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;
use lazy_static::lazy_static;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Model Specific Register numbers for SYSCALL/SYSRET
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            let stack_end = stack_start + STACK_SIZE;
            stack_end
        };
        tss
    };
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();

        // Segment order is CRITICAL for SYSCALL/SYSRET:
        // 1. Null descriptor (index 0)
        // 2. Kernel code (index 1) - used by SYSCALL
        // 3. Kernel data (index 2)
        // 4. User code (index 3) - used by SYSRET
        // 5. User data (index 4)
        // 6. TSS

        let kernel_code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_code_selector = gdt.add_entry(Descriptor::user_code_segment());
        let user_data_selector = gdt.add_entry(Descriptor::user_data_segment());
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));

        (gdt, Selectors {
            kernel_code_selector,
            kernel_data_selector,
            user_code_selector,
            user_data_selector,
            tss_selector,
        })
    };
}

struct Selectors {
    kernel_code_selector: SegmentSelector,
    kernel_data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

/// Initialize GDT and TSS
pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{CS, Segment};

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.kernel_code_selector);
        load_tss(GDT.1.tss_selector);
    }
}

/// Initialize SYSCALL/SYSRET support
///
/// This configures the MSRs needed for fast system calls:
/// - IA32_STAR: Segment selectors for kernel and user mode
/// - IA32_LSTAR: Address of syscall handler entry point
/// - IA32_FMASK: RFLAGS mask (clears interrupt flag during syscall)
pub fn init_syscall() {
    // Initialize kernel stack
    init_kernel_stack();

    unsafe {
        use x86_64::registers::model_specific::Msr;

        // STAR format (64 bits):
        // Bits 63:48 - User CS and SS (User Code = STAR[63:48] + 16, User Data = STAR[63:48] + 8)
        // Bits 47:32 - Kernel CS and SS (Kernel Code = STAR[47:32], Kernel Data = STAR[47:32] + 8)
        // Bits 31:0  - Reserved

        // Get raw selector values (must be shifted appropriately)
        let kernel_cs = GDT.1.kernel_code_selector.0 as u64;
        let user_cs = (GDT.1.user_code_selector.0 as u64).wrapping_sub(16); // SYSRET adds 16

        let star_value = (user_cs << 48) | (kernel_cs << 32);

        Msr::new(IA32_STAR).write(star_value);

        // LSTAR - syscall handler entry point
        Msr::new(IA32_LSTAR).write(syscall_handler as u64);

        // FMASK - mask RFLAGS.IF (bit 9) during syscall to disable interrupts
        Msr::new(IA32_FMASK).write(0x200); // IF flag
    }
}

/// Get user code selector (for transitioning to ring 3)
pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code_selector
}

/// Get user data selector (for transitioning to ring 3)
pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data_selector
}

/// Temporary storage for user RSP during syscall
static mut USER_RSP: u64 = 0;

/// Kernel stack for syscall handling
static mut SYSCALL_STACK: [u8; 4096 * 4] = [0; 4096 * 4];

/// Get kernel stack pointer for syscalls
fn get_kernel_stack_ptr() -> u64 {
    unsafe {
        let stack_start = SYSCALL_STACK.as_ptr() as u64;
        stack_start + (SYSCALL_STACK.len() as u64)
    }
}

/// Syscall handler entry point
///
/// This is invoked when userspace executes SYSCALL instruction.
/// SYSCALL has already saved RIP->RCX and RFLAGS->R11.
#[unsafe(naked)]
extern "C" fn syscall_handler() {
    core::arch::naked_asm!(
        // Save user RSP
        "mov qword ptr [rip + {USER_RSP}], rsp",

        // Switch to kernel stack
        "mov rsp, qword ptr [rip + {KERNEL_STACK_PTR}]",

        // Save context for SYSRET
        "push qword ptr [rip + {USER_RSP}]",  // User RSP
        "push r11",                            // User RFLAGS
        "push rcx",                            // User RIP

        // Save callee-saved registers
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Syscall ABI -> System V ABI conversion
        // IN:  rax=num, rdi=arg1, rsi=arg2, rdx=arg3, r10=arg4, r8=arg5, r9=arg6
        // OUT: rdi=num, rsi=arg1, rdx=arg2, rcx=arg3, r8=arg4, r9=arg5, stack=arg6

        "push r9",       // arg6 (will be on stack)
        "mov r9, r8",    // arg5: r8 -> r9
        "mov r8, r10",   // arg4: r10 -> r8
        "mov rcx, rdx",  // arg3: rdx -> rcx
        "mov rdx, rsi",  // arg2: rsi -> rdx
        "mov rsi, rdi",  // arg1: rdi -> rsi
        "mov rdi, rax",  // num: rax -> rdi

        // Call dispatcher (result in RAX)
        "call {syscall_dispatcher}",

        // Clean up arg6 from stack
        "add rsp, 8",

        // Restore callee-saved registers
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",

        // Restore for SYSRET
        "pop rcx",       // User RIP
        "pop r11",       // User RFLAGS
        "pop rsp",       // User RSP

        // Return to user mode
        "sysretq",

        syscall_dispatcher = sym crate::syscall::syscall_dispatcher,
        USER_RSP = sym USER_RSP,
        KERNEL_STACK_PTR = sym KERNEL_STACK_PTR,
    )
}

/// Kernel stack pointer (initialized at boot)
static mut KERNEL_STACK_PTR: u64 = 0;

/// Initialize kernel stack pointer for syscalls
fn init_kernel_stack() {
    unsafe {
        KERNEL_STACK_PTR = get_kernel_stack_ptr();
    }
}
