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

/// Per-process kernel stack size (16 KiB = 4 pages)
pub const KERNEL_STACK_SIZE: usize = 4096 * 4;

/// Number of pages per kernel stack (not counting guard page)
const STACK_PAGES: usize = KERNEL_STACK_SIZE / 4096;

/// Maximum concurrent processes with kernel stacks
pub const MAX_PROCESSES: usize = 64;

/// Virtual address base for kernel stack region.
/// Each slot occupies (1 guard + STACK_PAGES) pages.
const KSTACK_VIRT_BASE: u64 = 0x5555_5555_0000;

/// Total pages per slot: 1 guard + STACK_PAGES
const PAGES_PER_SLOT: usize = 1 + STACK_PAGES;

/// Kernel stack pool — uses virtual memory pages with guard pages.
/// Each slot has an unmapped guard page at the bottom, then STACK_PAGES
/// of mapped memory. Stack overflow hits the guard page → page fault.
struct KernelStackPool {
    /// Whether each slot is allocated
    in_use: [bool; MAX_PROCESSES],
}

impl KernelStackPool {
    fn new() -> Self {
        KernelStackPool {
            in_use: [false; MAX_PROCESSES],
        }
    }

    /// Virtual address of the guard page for a given slot
    fn slot_guard_addr(slot: usize) -> u64 {
        KSTACK_VIRT_BASE + (slot as u64) * (PAGES_PER_SLOT as u64) * 4096
    }

    /// Virtual address of the stack bottom (first usable page) for a given slot
    fn slot_stack_bottom(slot: usize) -> u64 {
        Self::slot_guard_addr(slot) + 4096 // skip guard page
    }

    /// Virtual address of the stack top for a given slot
    fn slot_stack_top(slot: usize) -> u64 {
        Self::slot_stack_bottom(slot) + KERNEL_STACK_SIZE as u64
    }

    fn allocate(&mut self) -> Option<(u64, u64, usize)> {
        for i in 0..MAX_PROCESSES {
            if !self.in_use[i] {
                // Map stack pages (skip guard page — leave it unmapped)
                let mapped = self.map_stack_pages(i);
                if !mapped {
                    return None;
                }
                self.in_use[i] = true;
                let bottom = Self::slot_stack_bottom(i);
                let top = Self::slot_stack_top(i);
                return Some((bottom, top, i));
            }
        }
        None
    }

    fn free(&mut self, slot: usize) {
        if slot < MAX_PROCESSES && self.in_use[slot] {
            self.unmap_stack_pages(slot);
            self.in_use[slot] = false;
        }
    }

    fn stack_top(&self, slot: usize) -> u64 {
        Self::slot_stack_top(slot)
    }

    /// Map the stack pages for a slot (guard page is NOT mapped)
    fn map_stack_pages(&self, slot: usize) -> bool {
        use x86_64::structures::paging::{Page, PageTableFlags, Mapper, Size4KiB, FrameAllocator};

        let stack_bottom = Self::slot_stack_bottom(slot);
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        // Get mapper and frame allocator
        let mut mapper = unsafe { crate::memory::get_mapper() };

        crate::memory::with_frame_allocator(|alloc| {
            for page_idx in 0..STACK_PAGES {
                let virt = stack_bottom + (page_idx as u64) * 4096;
                let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt));
                let frame = alloc.allocate_frame().expect("out of physical frames for kernel stack");
                unsafe {
                    mapper.map_to(page, frame, flags, alloc)
                        .expect("failed to map kernel stack page")
                        .flush();
                }
            }
            // Zero the stack memory
            unsafe {
                core::ptr::write_bytes(stack_bottom as *mut u8, 0, KERNEL_STACK_SIZE);
            }
        }).is_some()
    }

    /// Unmap and deallocate the stack pages for a slot
    fn unmap_stack_pages(&self, slot: usize) {
        use x86_64::structures::paging::{Page, Mapper, Size4KiB};

        let stack_bottom = Self::slot_stack_bottom(slot);
        let mut mapper = unsafe { crate::memory::get_mapper() };

        crate::memory::with_frame_allocator(|alloc| {
            for page_idx in 0..STACK_PAGES {
                let virt = stack_bottom + (page_idx as u64) * 4096;
                let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt));
                if let Ok((frame, flush)) = mapper.unmap(page) {
                    flush.flush();
                    unsafe { alloc.deallocate_frame(frame); }
                }
            }
        });
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
/// The stack has an unmapped guard page at the bottom for overflow detection.
pub fn allocate_kernel_stack() -> Option<(u64, usize)> {
    with_pool(|pool| pool.allocate().map(|(_, top, slot)| (top, slot)))
}

/// Free a per-process kernel stack and return its pages to the frame allocator.
pub fn free_kernel_stack(slot: usize) {
    with_pool(|pool| pool.free(slot));
}

/// Get the stack top for a kernel stack slot.
pub fn kernel_stack_top(slot: usize) -> u64 {
    with_pool(|pool| pool.stack_top(slot))
}

/// Check if a virtual address falls within a kernel stack guard page.
pub fn is_guard_page_address(addr: u64) -> bool {
    if addr < KSTACK_VIRT_BASE {
        return false;
    }
    let offset = addr - KSTACK_VIRT_BASE;
    let slot_size = (PAGES_PER_SLOT as u64) * 4096;
    let total_region = slot_size * MAX_PROCESSES as u64;
    if offset >= total_region {
        return false;
    }
    // Within a slot, the guard page is the first page (offset 0..4095)
    let offset_in_slot = offset % slot_size;
    offset_in_slot < 4096
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
pub static mut BOOT_PRIVILEGE_STACK: [u8; 4096 * 5] = [0; 4096 * 5];

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
