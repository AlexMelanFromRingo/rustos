use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use x86_64::VirtAddr;
use lazy_static::lazy_static;
use crate::println;
use pic8259::ChainedPics;
use spin;
use core::sync::atomic::{AtomicBool, Ordering};

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    Serial1 = PIC_1_OFFSET + 4,
    PrimaryATA = PIC_2_OFFSET + 6,
    SecondaryATA = PIC_2_OFFSET + 7,
    /// PS/2 auxiliary device — mouse on IRQ 12 (slave PIC pin 4).
    Mouse = PIC_2_OFFSET + 4,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.machine_check.set_handler_fn(machine_check_handler);

        // Timer: use naked handler for preemptive context switching
        unsafe {
            idt[InterruptIndex::Timer.as_usize()]
                .set_handler_addr(VirtAddr::new(timer_isr_naked as *const () as u64));
        }

        idt[InterruptIndex::Keyboard.as_usize()]
            .set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Serial1.as_usize()]
            .set_handler_fn(serial1_interrupt_handler);
        idt[InterruptIndex::PrimaryATA.as_usize()]
            .set_handler_fn(primary_ata_interrupt_handler);
        idt[InterruptIndex::SecondaryATA.as_usize()]
            .set_handler_fn(secondary_ata_interrupt_handler);
        idt[InterruptIndex::Mouse.as_usize()]
            .set_handler_fn(mouse_interrupt_handler);

        // AP heartbeat — vector 0x70.  Programmed from `apic::
        // program_lapic_timer` on each AP after it loads the IDT.
        // The handler increments that CPU's per-CPU heartbeat
        // counter and writes EOI; no scheduling decisions yet.
        idt[AP_HEARTBEAT_VECTOR as usize]
            .set_handler_fn(ap_heartbeat_handler);

        // Spurious interrupt — LAPIC delivers this when it has
        // nothing else to fire (Intel SDM Vol. 3A §10.9).  Without
        // a handler the CPU triple-faults on first occurrence,
        // which is exactly what we hit when an AP STI'd before
        // this entry existed.  Don't EOI — spurious is by definition
        // never officially acknowledged by the LAPIC.
        idt[0xFF].set_handler_fn(spurious_handler);
        idt
    };
}

extern "x86-interrupt" fn spurious_handler(_sf: x86_64::structures::idt::InterruptStackFrame) {
    // No-op: per Intel SDM, the spurious vector must NOT issue EOI.
    // Just return and let the CPU resume.
}

/// Vector for the per-CPU LAPIC-timer heartbeat.  Picked above the
/// PIC-mapped range (0x20..0x30) and any IRQ assignments, well below
/// the spurious vector 0xFF.
pub const AP_HEARTBEAT_VECTOR: u8 = 0x70;

/// Global "any AP heartbeat fired" tripwire — incremented unconditionally
/// from the handler so we can tell "handler invoked, but per-CPU dispatch
/// broken" apart from "handler never invoked."
pub static AP_HEARTBEAT_TOTAL: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// AP heartbeat ISR.  Runs in interrupt context on the AP that fired
/// the timer.  Reads its own LAPIC ID to update the right per-CPU slot.
extern "x86-interrupt" fn ap_heartbeat_handler(_sf: x86_64::structures::idt::InterruptStackFrame) {
    AP_HEARTBEAT_TOTAL.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    let id = crate::apic::lapic_id();
    if let Some(slot) = crate::smp::cpu_state(id) {
        slot.heartbeat.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    }
    crate::apic::eoi();
}

pub fn init_idt() {
    IDT.load();
}

pub fn init_pics() {
    unsafe {
        PICS.lock().initialize();
        PICS.lock().write_masks(0, 0);
    }
}

// =============================================================================
// Preemptive scheduling support — kernel idle ↔ user mode switching
// =============================================================================

/// When true, the timer ISR will dispatch ready user processes even from kernel mode.
/// Set by `enter_preemptive_idle()`, cleared when all user processes finish.
pub static PREEMPTIVE_MODE: AtomicBool = AtomicBool::new(false);

/// When true, the system delivers IRQs through IOAPIC + LAPIC; ISRs
/// must EOI via `apic::eoi()` instead of the legacy 8259 PIC.  Set by
/// `switch_to_apic()` after the IOAPIC has been programmed and the
/// PIC masked.
pub static APIC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Issue an EOI for the currently-being-handled IRQ.  Picks the right
/// controller (LAPIC vs 8259) based on `APIC_ACTIVE`.
fn eoi_for(vector: u8) {
    if APIC_ACTIVE.load(Ordering::Relaxed) {
        crate::apic::eoi();
    } else {
        unsafe { PICS.lock().notify_end_of_interrupt(vector); }
    }
}

/// Migrate IRQ delivery from the 8259 PIC to the IOAPIC.  Reads the
/// MADT for IOAPIC base + Interrupt Source Override entries, programs
/// each in-use ISA IRQ → IDT vector via `apic::ioapic_route`, masks the
/// PIC, and flips `APIC_ACTIVE`.  Returns Err with a reason if any
/// step fails (the caller stays on PIC).
pub fn switch_to_apic() -> Result<(), &'static str> {
    if !crate::apic::is_available() { return Err("apic not initialised"); }
    let madt = crate::acpi::parse_madt().ok_or("no MADT")?;
    if madt.ioapics.is_empty() { return Err("MADT has no IOAPIC"); }

    // The IRQs we currently use, paired with their installed IDT vectors.
    let pairs: &[(u8, u8)] = &[
        (0,  InterruptIndex::Timer.as_u8()),         // PIT timer
        (1,  InterruptIndex::Keyboard.as_u8()),
        (4,  InterruptIndex::Serial1.as_u8()),
        (12, InterruptIndex::Mouse.as_u8()),
        (14, InterruptIndex::PrimaryATA.as_u8()),
        (15, InterruptIndex::SecondaryATA.as_u8()),
    ];
    for (irq, vector) in pairs {
        let (gsi, _polarity, _trigger) = crate::acpi::resolve_irq(&madt, *irq);
        // GSIs are global; for QEMU PIIX they fit in the single IOAPIC's
        // 24-pin range, so gsi == pin number.  A multi-IOAPIC system
        // would dispatch on madt.ioapics[].gsi_base.
        crate::apic::ioapic_route(gsi as u8, *vector);
    }
    crate::apic::mask_pic_all();
    APIC_ACTIVE.store(true, Ordering::Release);
    Ok(())
}

/// Saved kernel TrapFrame for returning to shell after all user processes exit.
/// Only valid when PREEMPTIVE_MODE is true.
static mut KERNEL_RETURN_FRAME: crate::process::context::TrapFrame = crate::process::context::TrapFrame::empty();

/// Whether KERNEL_RETURN_FRAME has been saved (only save once from the idle loop).
static KERNEL_FRAME_SAVED: AtomicBool = AtomicBool::new(false);

/// Enter preemptive idle loop. Called from shell after spawning user processes.
/// Saves kernel state and enters HLT loop; timer ISR dispatches user processes.
/// Returns when all user processes have exited.
pub fn enter_preemptive_idle() {
    KERNEL_FRAME_SAVED.store(false, Ordering::SeqCst);
    PREEMPTIVE_MODE.store(true, Ordering::SeqCst);

    // HLT loop — timer interrupts wake us, ISR handles user process dispatch.
    // When PREEMPTIVE_MODE is cleared (all processes done), we break out.
    while PREEMPTIVE_MODE.load(Ordering::SeqCst) {
        x86_64::instructions::hlt();
    }
}

// =============================================================================
// Naked timer ISR — saves full TrapFrame for preemptive multitasking
// =============================================================================
//
// When the CPU takes interrupt 32 (timer), it pushes:
//   SS, RSP, RFLAGS, CS, RIP  (the "interrupt frame")
//
// Our naked handler then pushes all 15 GPRs to form a complete TrapFrame.
// We pass RSP (pointing to the TrapFrame on the kernel stack) to the Rust
// handler, which decides whether to context switch.

#[unsafe(naked)]
extern "C" fn timer_isr_naked() {
    core::arch::naked_asm!(
        // CPU has already pushed: SS, RSP, RFLAGS, CS, RIP
        // Now push all GPRs to complete the TrapFrame (order must match TrapFrame struct)
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Pass pointer to TrapFrame (RSP) as first argument (RDI)
        "mov rdi, rsp",

        // Call Rust handler — may modify RDI to point to a DIFFERENT TrapFrame
        // (if context switch happened)
        "call {timer_handler}",

        // RAX now contains pointer to TrapFrame to restore (returned by handler)
        // Set RSP to point to that TrapFrame
        "mov rsp, rax",

        // Pop all GPRs from the (possibly different) TrapFrame
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",

        // IRETQ pops: RIP, CS, RFLAGS, RSP, SS
        "iretq",

        timer_handler = sym timer_preempt_handler,
    )
}

/// Rust-level timer handler called from naked ISR.
///
/// `frame` points to the TrapFrame on the current kernel stack.
/// Returns a pointer to the TrapFrame to restore (may be same or different process).
#[unsafe(no_mangle)]
extern "C" fn timer_preempt_handler(frame: *mut crate::process::context::TrapFrame) -> *mut crate::process::context::TrapFrame {
    use crate::process::PROCESS_MANAGER;
    use crate::process::scheduler::SCHEDULER;

    // Always tick the system timer
    crate::task::timer::tick();

    // Send EOI early so we don't miss the next timer tick.
    crate::interrupt_stats::bump(0);
    eoi_for(InterruptIndex::Timer.as_u8());

    // Check if we interrupted a user-mode process (CS RPL=3)
    let trap = unsafe { &*frame };
    let from_user = trap.cs & 3 == 3;

    if !from_user {
        // Kernel mode — run scheduler tick (sleep wakeups, signal delivery)
        let mut sched = SCHEDULER.lock();
        sched.tick_kernel_only();

        // If preemptive mode is active and there are user processes ready, dispatch one
        if PREEMPTIVE_MODE.load(Ordering::Relaxed) {
            let mut pm = PROCESS_MANAGER.lock();
            let ready = sched.ready_count();
            if pm.current_pid.is_none() && ready > 0 {
                // Save the kernel idle frame ONLY on first dispatch —
                // subsequent dispatches from sys_exit's HLT must NOT
                // overwrite it, or we'll return to the wrong stack.
                if !KERNEL_FRAME_SAVED.load(Ordering::Relaxed) {
                    unsafe { KERNEL_RETURN_FRAME = *frame; }
                    KERNEL_FRAME_SAVED.store(true, Ordering::Relaxed);
                }

                // Pick the next user process (use _with_pm to avoid re-locking PM)
                if let Some(next_pid) = sched.dequeue_next_with_pm(&pm) {
                    let valid = pm.get_process(next_pid)
                        .map(|p| p.is_user && p.has_trap_frame)
                        .unwrap_or(false);
                    if valid {
                        let next = pm.get_process_mut(next_pid).unwrap();
                        next.state = crate::process::ProcessState::Running;
                        let kstack_top = next.kernel_stack_top;
                        let cr3 = next.cr3;
                        let tf_ptr = &mut next.trap_frame as *mut crate::process::context::TrapFrame;
                        pm.current_pid = Some(next_pid);
                        if kstack_top != 0 {
                            unsafe { crate::gdt::set_tss_rsp0(kstack_top); }
                        }
                        // Switch to this process's private page table.
                        // No-op when cr3 == 0 (process not yet given its
                        // own PML4 — falls back to kernel CR3 = ok).
                        unsafe { crate::memory::pagetable::switch_cr3(cr3); }
                        drop(pm);
                        drop(sched);
                        return tf_ptr;
                    }
                    sched.enqueue(next_pid);
                }
                drop(pm);
            } else if pm.current_pid.is_none() && ready == 0 {
                drop(pm);
                drop(sched);
                PREEMPTIVE_MODE.store(false, Ordering::SeqCst);
                KERNEL_FRAME_SAVED.store(false, Ordering::SeqCst);
                // Return the saved kernel idle frame, not the current frame
                // (which may be from sys_exit's HLT on the SYSCALL stack).
                let kf = core::ptr::addr_of_mut!(KERNEL_RETURN_FRAME);
                return kf;
            } else {
                drop(pm);
            }
        }

        drop(sched);
        return frame; // Return same frame — no switch
    }

    // === User mode preemption ===

    // Save the current TrapFrame into the current process
    let mut pm = PROCESS_MANAGER.lock();
    let current_pid = pm.current_pid;

    if let Some(pid) = current_pid {
        if let Some(proc) = pm.get_process_mut(pid) {
            // Copy TrapFrame from stack into process struct
            proc.trap_frame = unsafe { *frame };
            proc.has_trap_frame = true;
        }
    }
    drop(pm);

    // Run scheduler tick (quantum counting, preemption decision)
    let mut sched = SCHEDULER.lock();
    sched.tick();
    drop(sched);

    // Check if scheduler changed the current process
    let mut pm = PROCESS_MANAGER.lock();
    let new_pid = pm.current_pid;

    if let Some(pid) = new_pid {
        if let Some(proc) = pm.get_process_mut(pid) {
            if proc.is_user && proc.has_trap_frame {
                // Switch TSS.RSP0 to the new process's kernel stack
                if proc.kernel_stack_top != 0 {
                    unsafe { crate::gdt::set_tss_rsp0(proc.kernel_stack_top); }
                }
                let cr3 = proc.cr3;
                // Return pointer to the new process's TrapFrame
                let tf_ptr = &mut proc.trap_frame as *mut crate::process::context::TrapFrame;
                unsafe { crate::memory::pagetable::switch_cr3(cr3); }
                drop(pm);
                return tf_ptr;
            }
        }
    }

    // No switch or not a user process — restore original frame
    // Make sure TSS.RSP0 is correct for current process
    if let Some(pid) = current_pid {
        if let Some(proc) = pm.get_process(pid) {
            if proc.kernel_stack_top != 0 {
                unsafe { crate::gdt::set_tss_rsp0(proc.kernel_stack_top); }
            }
        }
    }
    drop(pm);
    frame
}

// =============================================================================
// Standard interrupt handlers (unchanged)
// =============================================================================

extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame, error_code: u64)
{
    panic!("EXCEPTION: GENERAL PROTECTION FAULT (error code: {})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame, error_code: x86_64::structures::idt::PageFaultErrorCode)
{
    use x86_64::registers::control::Cr2;

    let fault_addr = Cr2::read();

    // Copy-on-write resolution: a write to a user page that's
    // present-but-not-writable is the COW path.  We try to resolve
    // it before printing any panic banner — if successful, the
    // faulting instruction simply retries with the now-private page.
    let is_user = error_code.contains(x86_64::structures::idt::PageFaultErrorCode::USER_MODE);
    let is_write = error_code.contains(x86_64::structures::idt::PageFaultErrorCode::CAUSED_BY_WRITE);
    let is_present = error_code.contains(x86_64::structures::idt::PageFaultErrorCode::PROTECTION_VIOLATION);
    if is_user && is_write && is_present {
        let cur_cr3: u64;
        unsafe {
            core::arch::asm!("mov {}, cr3", out(reg) cur_cr3, options(nomem, nostack));
        }
        if unsafe { crate::memory::cow::resolve(cur_cr3, fault_addr) }.is_ok() {
            return;
        }
    }

    crate::println!("\n╔══════════════════════════════════════════╗");
    crate::println!("║       EXCEPTION: PAGE FAULT             ║");
    crate::println!("╚══════════════════════════════════════════╝");
    crate::println!("Accessed Address: {:?}", fault_addr);
    crate::println!("Error Code: {:?}", error_code);
    crate::println!("  - Present: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::PROTECTION_VIOLATION));
    crate::println!("  - Write: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::CAUSED_BY_WRITE));
    crate::println!("  - User: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::USER_MODE));
    crate::println!("  - Reserved Write: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::MALFORMED_TABLE));
    crate::println!("  - Instruction Fetch: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::INSTRUCTION_FETCH));
    crate::println!("{:#?}", stack_frame);

    let addr_u64 = fault_addr.as_u64();

    // Check if this is a kernel stack guard page hit (stack overflow)
    if crate::gdt::is_guard_page_address(addr_u64) {
        crate::println!("╔══════════════════════════════════════════╗");
        crate::println!("║     KERNEL STACK OVERFLOW DETECTED!     ║");
        crate::println!("╚══════════════════════════════════════════╝");
        crate::println!("Address 0x{:X} is in a kernel stack guard page.", addr_u64);
        panic!("Kernel stack overflow");
    }

    if addr_u64 >= crate::memory::userspace::USER_SPACE_START &&
       addr_u64 < crate::memory::userspace::USER_SPACE_END {
        crate::println!("Fault in USER SPACE range (0x{:X} - 0x{:X})",
            crate::memory::userspace::USER_SPACE_START,
            crate::memory::userspace::USER_SPACE_END);
    }

    panic!("Page fault");
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);

    crate::interrupt_stats::bump(1);
    eoi_for(InterruptIndex::Keyboard.as_u8());
}

/// PS/2 auxiliary (mouse) — IRQ 12.  Reads a byte from port 0x60 and
/// hands it to the mouse packet-decoder state machine.
extern "x86-interrupt" fn mouse_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use x86_64::instructions::port::Port;
    let mut port = Port::<u8>::new(0x60);
    let byte: u8 = unsafe { port.read() };
    crate::drivers::mouse::input_byte(byte);
    crate::interrupt_stats::bump(12);
    eoi_for(InterruptIndex::Mouse.as_u8());
}

/// Non-maskable interrupt — used by hardware to flag irrecoverable
/// conditions (parity errors, watchdog timeouts, IPMI events).  We log
/// and continue rather than halt, on the theory that whatever started
/// the NMI also wanted us alive long enough to record it.
extern "x86-interrupt" fn nmi_handler(stack_frame: InterruptStackFrame) {
    crate::klog_warn!("NMI received at RIP={:#x}",
        stack_frame.instruction_pointer.as_u64());
}

/// #MC — Machine Check Exception.  Indicates the CPU detected an
/// uncorrectable hardware error.  Recovery on a one-shot kernel is not
/// realistic: we record and halt.
extern "x86-interrupt" fn machine_check_handler(stack_frame: InterruptStackFrame) -> ! {
    crate::klog_err!("MCE at RIP={:#x} — halting",
        stack_frame.instruction_pointer.as_u64());
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn serial1_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use x86_64::instructions::port::Port;

    let mut port = Port::<u8>::new(0x3F8);
    let byte: u8 = unsafe { port.read() };
    crate::task::keyboard::add_serial_byte(byte);

    crate::interrupt_stats::bump(4);
    eoi_for(InterruptIndex::Serial1.as_u8());
}

extern "x86-interrupt" fn primary_ata_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    crate::interrupt_stats::bump(14);
    eoi_for(InterruptIndex::PrimaryATA.as_u8());
}

extern "x86-interrupt" fn secondary_ata_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    crate::interrupt_stats::bump(15);
    eoi_for(InterruptIndex::SecondaryATA.as_u8());
}

#[cfg(test)]
mod tests {
    #[test_case]
    fn test_breakpoint_exception() {
        x86_64::instructions::interrupts::int3();
    }
}
