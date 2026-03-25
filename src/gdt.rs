use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;
use lazy_static::lazy_static;
use spin::Mutex;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Model Specific Register numbers for SYSCALL/SYSRET
const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

/// Per-process kernel stack size (16 KiB)
pub const KERNEL_STACK_SIZE: usize = 4096 * 4;

/// Maximum concurrent processes with kernel stacks
pub const MAX_PROCESSES: usize = 64;

/// Kernel stack pool — heap-allocated to avoid BSS bloat that overlaps user space.
struct KernelStackPool {
    /// Each slot: Option<Vec<u8>> holding the stack memory
    stacks: [Option<alloc::vec::Vec<u8>>; MAX_PROCESSES],
    /// Cached stack top addresses
    tops: [u64; MAX_PROCESSES],
}

impl KernelStackPool {
    fn new() -> Self {
        KernelStackPool {
            stacks: core::array::from_fn(|_| None),
            tops: [0u64; MAX_PROCESSES],
        }
    }

    fn allocate(&mut self) -> Option<(u64, u64, usize)> {
        for i in 0..MAX_PROCESSES {
            if self.stacks[i].is_none() {
                let stack = alloc::vec![0u8; KERNEL_STACK_SIZE];
                let bottom = stack.as_ptr() as u64;
                let top = bottom + KERNEL_STACK_SIZE as u64;
                self.tops[i] = top;
                self.stacks[i] = Some(stack);
                return Some((bottom, top, i));
            }
        }
        None
    }

    fn free(&mut self, slot: usize) {
        if slot < MAX_PROCESSES {
            self.stacks[slot] = None;
            self.tops[slot] = 0;
        }
    }

    fn stack_top(&self, slot: usize) -> u64 {
        self.tops[slot]
    }
}

/// Global kernel stack pool — initialized lazily on first use
static KERNEL_STACK_POOL: Mutex<Option<KernelStackPool>> = Mutex::new(None);

fn with_pool<F, R>(f: F) -> R
where F: FnOnce(&mut KernelStackPool) -> R {
    let mut guard = KERNEL_STACK_POOL.lock();
    if guard.is_none() {
        *guard = Some(KernelStackPool::new());
    }
    f(guard.as_mut().unwrap())
}

/// Allocate a per-process kernel stack. Returns (stack_top, slot_index).
pub fn allocate_kernel_stack() -> Option<(u64, usize)> {
    with_pool(|pool| pool.allocate().map(|(_, top, slot)| (top, slot)))
}

/// Free a per-process kernel stack.
pub fn free_kernel_stack(slot: usize) {
    with_pool(|pool| pool.free(slot));
}

/// Get the stack top for a kernel stack slot.
pub fn kernel_stack_top(slot: usize) -> u64 {
    with_pool(|pool| pool.stack_top(slot))
}

// =============================================================================
// TSS — mutable via UnsafeCell so we can update RSP0 on context switch
// =============================================================================

struct TssStorage(core::cell::UnsafeCell<TaskStateSegment>);
unsafe impl Sync for TssStorage {}

static TSS_STORAGE: TssStorage = TssStorage(core::cell::UnsafeCell::new(TaskStateSegment::new()));

/// Boot-time IST stack for double faults (never changes)
static mut IST_STACK: [u8; 4096 * 5] = [0; 4096 * 5];

/// Boot-time privilege stack (used as initial RSP0 before any process runs)
static mut BOOT_PRIVILEGE_STACK: [u8; 4096 * 5] = [0; 4096 * 5];

/// Initialize TSS fields. Called once at boot before GDT.load().
fn init_tss() {
    let tss = unsafe { &mut *TSS_STORAGE.0.get() };

    // RSP0: kernel stack for Ring 3 → Ring 0 transitions
    let priv_stack_start = core::ptr::addr_of!(BOOT_PRIVILEGE_STACK) as u64;
    tss.privilege_stack_table[0] = VirtAddr::new(priv_stack_start + 4096 * 5);

    // IST[0]: double fault handler stack
    let ist_stack_start = core::ptr::addr_of!(IST_STACK) as u64;
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
        VirtAddr::new(ist_stack_start + 4096 * 5);
}

/// Update TSS.RSP0 to point to a new kernel stack top.
/// Called on every context switch to a user process.
///
/// # Safety
/// Must be called with interrupts disabled.
pub unsafe fn set_tss_rsp0(stack_top: u64) {
    let tss = unsafe { &mut *TSS_STORAGE.0.get() };
    tss.privilege_stack_table[0] = VirtAddr::new(stack_top);
}

// =============================================================================
// GDT
// =============================================================================

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();

        // Segment order is CRITICAL for SYSCALL/SYSRET:
        // 1. Null descriptor (index 0)
        // 2. Kernel code (index 1)
        // 3. Kernel data (index 2)
        // 4. User data (index 3) — MUST be before user code for SYSRET
        // 5. User code (index 4)
        // 6. TSS

        let kernel_code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.add_entry(Descriptor::user_data_segment());
        let user_code_selector = gdt.add_entry(Descriptor::user_code_segment());
        // TSS descriptor points to our mutable TSS storage
        let tss_ref = unsafe { &*TSS_STORAGE.0.get() };
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(tss_ref));

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

    // Initialize TSS fields before loading GDT
    init_tss();

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.kernel_code_selector);
        load_tss(GDT.1.tss_selector);
    }
}

/// Initialize SYSCALL/SYSRET support
pub fn init_syscall() {
    init_kernel_stack();

    unsafe {
        use x86_64::registers::model_specific::Msr;

        let mut efer = Msr::new(IA32_EFER);
        let efer_value = efer.read();
        efer.write(efer_value | 1);

        let kernel_cs = GDT.1.kernel_code_selector.0 as u64;
        let user_cs = (GDT.1.user_code_selector.0 as u64).wrapping_sub(16);
        let star_value = (user_cs << 48) | (kernel_cs << 32);

        Msr::new(IA32_STAR).write(star_value);
        Msr::new(IA32_LSTAR).write(syscall_handler as *const () as u64);
        Msr::new(IA32_FMASK).write(0x200);
    }
}

/// Get user code selector
pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code_selector
}

/// Get user data selector
pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data_selector
}

/// Get user code selector raw value (for TrapFrame)
pub fn user_cs_value() -> u64 {
    GDT.1.user_code_selector.0 as u64
}

/// Get user data selector raw value (for TrapFrame SS)
pub fn user_ss_value() -> u64 {
    GDT.1.user_data_selector.0 as u64
}

// =============================================================================
// SYSCALL handler
// =============================================================================

/// Temporary storage for user RSP during syscall
static mut USER_RSP: u64 = 0;

/// Kernel stack for syscall handling
static mut SYSCALL_STACK: [u8; 4096 * 4] = [0; 4096 * 4];

fn get_kernel_stack_ptr() -> u64 {
    let stack_start = core::ptr::addr_of!(SYSCALL_STACK) as u64;
    stack_start + (4096 * 4)
}

/// Syscall handler entry point
///
/// Invoked when userspace executes SYSCALL instruction.
/// CPU saves RIP→RCX, RFLAGS→R11. RSP is NOT saved by CPU.
#[unsafe(naked)]
extern "C" fn syscall_handler() {
    core::arch::naked_asm!(
        "mov qword ptr [rip + {USER_RSP}], rsp",
        "mov rsp, qword ptr [rip + {KERNEL_STACK_PTR}]",

        "push qword ptr [rip + {USER_RSP}]",
        "push r11",
        "push rcx",

        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Syscall ABI → System V ABI
        "push r9",
        "mov r9, r8",
        "mov r8, r10",
        "mov rcx, rdx",
        "mov rdx, rsi",
        "mov rsi, rdi",
        "mov rdi, rax",

        "call {syscall_dispatcher}",

        "add rsp, 8",

        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",

        "pop rcx",
        "pop r11",
        "pop rsp",

        "sysretq",

        syscall_dispatcher = sym crate::syscall::syscall_dispatcher,
        USER_RSP = sym USER_RSP,
        KERNEL_STACK_PTR = sym KERNEL_STACK_PTR,
    )
}

/// Kernel stack pointer (initialized at boot)
static mut KERNEL_STACK_PTR: u64 = 0;

fn init_kernel_stack() {
    unsafe {
        KERNEL_STACK_PTR = get_kernel_stack_ptr();
    }
}
