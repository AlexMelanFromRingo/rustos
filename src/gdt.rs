use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;
use lazy_static::lazy_static;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Model Specific Register numbers for SYSCALL/SYSRET
const IA32_EFER: u32 = 0xC000_0080;  // Extended Feature Enable Register
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
        // 4. User data (index 3) - MUST be before user code for SYSRET!
        // 5. User code (index 4) - used by SYSRET
        // 6. TSS
        //
        // SYSRET sets: CS = STAR[63:48] + 16, SS = STAR[63:48] + 8
        // So user_data must be 8 bytes before user_code in GDT

        let kernel_code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.add_entry(Descriptor::user_data_segment());
        let user_code_selector = gdt.add_entry(Descriptor::user_code_segment());
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));

        (gdt, Selectors {
            kernel_code_selector,
            _kernel_data_selector: kernel_data_selector,
            user_code_selector,
            user_data_selector,
            tss_selector,
        })
    };
}

struct Selectors {
    kernel_code_selector: SegmentSelector,
    _kernel_data_selector: SegmentSelector,
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

        // Enable SYSCALL/SYSRET by setting SCE bit (bit 0) in IA32_EFER
        let mut efer = Msr::new(IA32_EFER);
        let efer_value = efer.read();
        efer.write(efer_value | 1); // Set SCE bit (bit 0)

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
        Msr::new(IA32_LSTAR).write(syscall_handler as *const () as u64);

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
    let stack_start = core::ptr::addr_of!(SYSCALL_STACK) as u64;
    stack_start + (4096 * 4)
}

/// Syscall handler entry point
///
/// This is invoked when userspace executes SYSCALL instruction.
/// SYSCALL has already saved RIP->RCX and RFLAGS->R11.
#[unsafe(naked)]
extern "C" fn syscall_handler() {
    core::arch::naked_asm!(
        // CRITICAL FIX: Save user RSP BEFORE switching stacks!
        // According to https://cyp.sh/blog/syscallsysret and https://wiki.osdev.org/SWAPGS
        // SYSCALL does NOT save RSP - we must do it manually!
        "mov qword ptr [rip + {USER_RSP}], rsp",    // 1. Save user RSP
        "mov rsp, qword ptr [rip + {KERNEL_STACK_PTR}]",  // 2. Switch to kernel stack

        // Save context for SYSRET (now on kernel stack, safe to push)
        "push qword ptr [rip + {USER_RSP}]",  // User RSP (saved above)
        "push r11",                            // User RFLAGS (CPU saved in R11)
        "push rcx",                            // User RIP (CPU saved in RCX)

        // DEBUG: Log syscall entry (on kernel stack now, won't corrupt user space!)
        // Save ALL syscall argument registers because println! will clobber them!
        "push rax",   // syscall number
        "push rdi",   // arg1
        "push rsi",   // arg2
        "push rdx",   // arg3 - CRITICAL! println! clobbers this
        "push r10",   // arg4
        "push r8",    // arg5
        "push r9",    // arg6
        "mov rdi, qword ptr [rip + {USER_RSP}]",  // arg1: saved user RSP
        "mov rsi, rsp",                            // arg2: current kernel RSP
        "add rsi, 56",                             // Adjust for 7 pushes (7*8=56)
        "call {debug_syscall_entry}",
        "pop r9",
        "pop r8",
        "pop r10",
        "pop rdx",    // RESTORE arg3!
        "pop rsi",
        "pop rdi",
        "pop rax",

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

        // DEBUG: Print SYSRET info (stack: [user_rip, user_rflags, user_rsp])
        "push rax",              // Save result
        "push rbp",              // Save rbp (will be arg5)
        "mov rdi, [rsp + 16]",   // arg1: user_rip (skip rax, rbp)
        "mov rsi, rax",          // arg2: result
        "mov rdx, [rsp + 32]",   // arg3: user_rsp (skip rax, rbp, rip, rflags)
        "mov rcx, [rsp + 24]",   // arg4: user_rflags (skip rax, rbp, rip)
        "mov r8, rbp",           // arg5: user_rbp (from register)
        "call {debug_sysret}",
        "pop rbp",               // Restore rbp
        "pop rax",               // Restore result

        // Restore for SYSRET
        "pop rcx",       // User RIP
        "pop r11",       // User RFLAGS
        "pop rsp",       // User RSP

        // Return to user mode
        "sysretq",

        syscall_dispatcher = sym crate::syscall::syscall_dispatcher,
        debug_sysret = sym debug_print_sysret,
        debug_syscall_entry = sym debug_syscall_entry,
        USER_RSP = sym USER_RSP,
        KERNEL_STACK_PTR = sym KERNEL_STACK_PTR,
    )
}

/// Kernel stack pointer (initialized at boot)
static mut KERNEL_STACK_PTR: u64 = 0;

/// Debug: Log SYSCALL entry with RSP values
#[no_mangle]
extern "C" fn debug_syscall_entry(user_rsp: u64, kernel_rsp: u64) {
    crate::println!("[SYSCALL ENTRY] user_RSP={:#x}, kernel_RSP={:#x}",
                    user_rsp, kernel_rsp);
}

/// Debug: Print SYSRET info with full state
#[no_mangle]
extern "C" fn debug_print_sysret(user_rip: u64, result: isize, user_rsp: u64, user_rflags: u64, user_rbp: u64) {
    crate::println!("[SYSRET] RIP={:#x}, RSP={:#x}, RBP={:#x}, RFLAGS={:#x}, res={}",
                    user_rip, user_rsp, user_rbp, user_rflags, result);
    crate::println!("         Next instr will write to [RBP-8] = {:#x}", user_rbp.wrapping_sub(8));
}

/// Initialize kernel stack pointer for syscalls
fn init_kernel_stack() {
    unsafe {
        KERNEL_STACK_PTR = get_kernel_stack_ptr();
    }
}
