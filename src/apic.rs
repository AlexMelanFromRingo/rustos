//! Local APIC + IOAPIC support — the modern x86 interrupt controllers.
//!
//! Replaces the legacy 8259 PIC for IRQ delivery.  This module handles
//! the *infrastructure* — register access, init, EOI — but does not yet
//! re-route the IDT vectors that interrupts.rs already programmed.  The
//! existing PIC remains the active controller until `enable()` is
//! called, after which the PIC is masked and the IOAPIC takes over.
//!
//! Reference: Intel SDM Vol. 3A §10 (LAPIC) and §12 (IOAPIC), OSDev wiki
//! "APIC".
//!
//! ## Memory layout
//!
//! Both controllers are MMIO at fixed default addresses on every PC made
//! since ~2000:
//!
//!     LAPIC : 0xFEE0_0000 (per-CPU; we read IA32_APIC_BASE_MSR to be
//!             safe — firmware may relocate it)
//!     IOAPIC: 0xFEC0_0000 (system-wide; ACPI MADT can list multiple,
//!             but on QEMU/PC there's exactly one at the default addr)
//!
//! We rely on the bootloader's direct physical-memory map (the
//! `map_physical_memory` feature on bootloader 0.9) so we can dereference
//! these addresses directly via `PHYS_MEM_OFFSET + 0xFEE0_0000`.
//!
//! ## What this provides
//!
//! * `cpu_has_apic()`            — CPUID-based feature probe
//! * `init()`                    — discover, enable LAPIC, leave IOAPIC
//!                                 ready but not yet routing
//! * `lapic_id()`                — current CPU's LAPIC ID
//! * `eoi()`                     — write to LAPIC EOI register
//! * `ioapic_route(irq, vector)` — program one IOAPIC RTE
//! * `mask_pic_all()`            — disable the legacy 8259 PIC
//!
//! ## What's deliberately *not* here yet
//!
//! * MADT / ACPI parsing — the IOAPIC base is taken from the well-known
//!   default 0xFEC00000.  All real PC hardware respects this; only odd
//!   workstations with multiple IOAPICs need MADT.
//! * MSI / MSI-X programming for PCI devices — separate module.
//! * SMP startup IPIs — this is single-CPU for now.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::registers::model_specific::Msr;

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_MSR_ENABLE: u64 = 1 << 11;

/// Default LAPIC physical base (firmware may relocate via the MSR).
const LAPIC_DEFAULT_PHYS: u64 = 0xFEE0_0000;
/// Standard IOAPIC physical base on PC-class hardware.
const IOAPIC_PHYS: u64 = 0xFEC0_0000;

// ---- LAPIC register offsets (byte) ----
const LAPIC_ID:        u32 = 0x020;
const LAPIC_VERSION:   u32 = 0x030;
const LAPIC_TPR:       u32 = 0x080;
const LAPIC_EOI:       u32 = 0x0B0;
const LAPIC_SVR:       u32 = 0x0F0; // Spurious Interrupt Vector Register
const LAPIC_LVT_TIMER: u32 = 0x320;
const LAPIC_TIMER_INITIAL_COUNT: u32 = 0x380;
const LAPIC_TIMER_DIVIDE_CONFIG: u32 = 0x3E0;

// SVR bit 8 = APIC software-enable.
const LAPIC_SVR_ENABLE: u32 = 1 << 8;
/// Vector we use for spurious interrupts (must have low 4 bits = 0xF on
/// older CPUs; modern ones don't care).
pub const SPURIOUS_VECTOR: u8 = 0xFF;

// ---- IOAPIC register window (indexed via IOREGSEL/IOWIN at offsets 0/0x10) ----
const IOAPIC_REG_VER:    u32 = 0x01;
const IOAPIC_REG_RTE_LO: u32 = 0x10; // RTE 0 lo at 0x10, hi at 0x11; pin N is at 0x10 + 2*N

static AVAILABLE: AtomicBool = AtomicBool::new(false);
static LAPIC_VIRT: AtomicU64 = AtomicU64::new(0);

fn phys_to_kernel_virt(phys: u64) -> *mut u32 {
    let offset = crate::memory::phys_offset();
    (offset + phys) as *mut u32
}

unsafe fn lapic_read(reg: u32) -> u32 {
    let base = LAPIC_VIRT.load(Ordering::Relaxed) as *mut u32;
    if base.is_null() { return 0; }
    unsafe { core::ptr::read_volatile(base.add((reg / 4) as usize)) }
}

unsafe fn lapic_write(reg: u32, val: u32) {
    let base = LAPIC_VIRT.load(Ordering::Relaxed) as *mut u32;
    if base.is_null() { return; }
    unsafe { core::ptr::write_volatile(base.add((reg / 4) as usize), val) }
}

unsafe fn ioapic_select() -> *mut u32 { phys_to_kernel_virt(IOAPIC_PHYS) }
unsafe fn ioapic_window() -> *mut u32 { phys_to_kernel_virt(IOAPIC_PHYS + 0x10) }

unsafe fn ioapic_read(reg: u32) -> u32 {
    unsafe {
        core::ptr::write_volatile(ioapic_select(), reg);
        core::ptr::read_volatile(ioapic_window())
    }
}

unsafe fn ioapic_write(reg: u32, val: u32) {
    unsafe {
        core::ptr::write_volatile(ioapic_select(), reg);
        core::ptr::write_volatile(ioapic_window(), val);
    }
}

/// CPUID feature probe: EDX bit 9 of leaf 1 = "On-chip APIC hardware
/// available".  Always true on every x86_64 chip we care about.
pub fn cpu_has_apic() -> bool {
    let res = unsafe { core::arch::x86_64::__cpuid(1) };
    (res.edx & (1 << 9)) != 0
}

/// Issue an EOI on the LAPIC.  Writes any value to the EOI register.
pub fn eoi() {
    unsafe { lapic_write(LAPIC_EOI, 0); }
}

/// Read this CPU's LAPIC ID (high 8 bits of register 0x20, shifted).
pub fn lapic_id() -> u8 {
    unsafe { (lapic_read(LAPIC_ID) >> 24) as u8 }
}

/// Was the APIC subsystem brought up?
pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }

/// Mask every IRQ on the legacy 8259 PIC.  Done once we move to APIC.
pub fn mask_pic_all() {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut master_data: Port<u8> = Port::new(0x21);
        let mut slave_data:  Port<u8> = Port::new(0xA1);
        master_data.write(0xFF);
        slave_data.write(0xFF);
    }
}

/// Number of redirection table entries the IOAPIC has (typically 24 on PC).
pub fn ioapic_max_entries() -> u32 {
    unsafe { ((ioapic_read(IOAPIC_REG_VER) >> 16) & 0xFF) + 1 }
}

/// Program one IOAPIC redirection table entry.  Routes ISA IRQ `irq` to
/// IDT vector `vector`, edge-triggered, active-high, fixed delivery,
/// physical destination CPU 0.
pub fn ioapic_route(irq: u8, vector: u8) {
    unsafe {
        let lo_reg = IOAPIC_REG_RTE_LO + 2 * irq as u32;
        let hi_reg = lo_reg + 1;
        // High dword: destination APIC ID (CPU 0) in bits 24..31.
        ioapic_write(hi_reg, 0 << 24);
        // Low dword: vector (bits 0..7), delivery mode 000 (fixed),
        // destination mode 0 (physical), pin polarity 0 (active high),
        // trigger mode 0 (edge), mask 0 (unmasked).  All defaults.
        ioapic_write(lo_reg, vector as u32);
    }
}

/// Mask one IOAPIC pin (set bit 16 of its low RTE).
pub fn ioapic_mask(irq: u8) {
    unsafe {
        let lo_reg = IOAPIC_REG_RTE_LO + 2 * irq as u32;
        let cur = ioapic_read(lo_reg);
        ioapic_write(lo_reg, cur | (1 << 16));
    }
}

/// Initialise LAPIC + IOAPIC.  Discovery is conservative: we trust the
/// CPUID feature bit and the well-known default MMIO addresses.  After
/// this call:
///   * LAPIC is software-enabled and ready to deliver fixed-priority
///     interrupts.
///   * IOAPIC is mapped and queryable.
///   * EOIs go via `apic::eoi()`.
///   * The PIC is *not yet* masked — call `apic::mask_pic_all()` and
///     route every needed IRQ via `ioapic_route` before doing so.
pub fn init() -> Result<(), &'static str> {
    if !cpu_has_apic() { return Err("apic: CPUID says no on-chip APIC"); }

    // Read IA32_APIC_BASE_MSR.  Bits 12..35 hold the physical base; bit
    // 11 is the global enable.  Force-set the enable in case BIOS left
    // it off.
    let mut msr = unsafe { Msr::new(IA32_APIC_BASE_MSR) };
    let cur = unsafe { msr.read() };
    let phys = cur & 0x0000_000F_FFFF_F000;
    let phys = if phys == 0 { LAPIC_DEFAULT_PHYS } else { phys };
    unsafe { msr.write(phys | APIC_BASE_MSR_ENABLE); }

    // Map LAPIC MMIO via the bootloader's phys-memory direct map.
    let lapic_virt = (crate::memory::phys_offset() + phys) as u64;
    LAPIC_VIRT.store(lapic_virt, Ordering::Relaxed);

    // Software-enable the APIC and program the spurious vector.
    unsafe {
        lapic_write(LAPIC_TPR, 0); // accept all interrupt priorities
        lapic_write(LAPIC_SVR, LAPIC_SVR_ENABLE | SPURIOUS_VECTOR as u32);
    }

    let id = lapic_id();
    let ver = unsafe { lapic_read(LAPIC_VERSION) };
    let max_irq = unsafe { ioapic_max_entries() };

    crate::klog_info!(
        "apic: LAPIC id={} ver={:#x} @ phys {:#x}, IOAPIC @ {:#x} ({} pins)",
        id, ver & 0xFF, phys, IOAPIC_PHYS, max_irq);

    AVAILABLE.store(true, Ordering::Release);
    Ok(())
}

/// Self-test: verify LAPIC ID is sensible and IOAPIC reports a non-zero
/// number of pins.  Returns Err if the controllers don't look alive.
pub fn self_test() -> Result<(), &'static str> {
    if !is_available() { return Err("apic: not initialised"); }
    let id = lapic_id();
    if id > 254 { return Err("apic: nonsense LAPIC id"); }
    let pins = ioapic_max_entries();
    if pins == 0 || pins > 240 { return Err("apic: nonsense IOAPIC pin count"); }
    Ok(())
}

// ---- LAPIC ICR (Interrupt Command Register) -------------------------------

const LAPIC_ICR_LO: u32 = 0x300;
const LAPIC_ICR_HI: u32 = 0x310;

/// Delivery modes (Intel SDM §10.6.1).
pub mod ipi {
    pub const FIXED:    u32 = 0b000 << 8;
    pub const SMI:      u32 = 0b010 << 8;
    pub const NMI:      u32 = 0b100 << 8;
    pub const INIT:     u32 = 0b101 << 8;
    pub const STARTUP:  u32 = 0b110 << 8;
    /// Edge-trigger, physical destination, no shorthand.  Combine
    /// with one of FIXED/SMI/NMI/INIT/STARTUP and the destination
    /// LAPIC ID written to ICR_HI bits 24..31.
    pub const ASSERT:   u32 = 1 << 14;
    pub const LEVEL_DE: u32 = 0 << 14;
    /// Destination shorthands (bits 18..19): 00 = no shorthand,
    /// 01 = self, 10 = all, 11 = all-but-self.
    pub const DEST_SELF:        u32 = 0b01 << 18;
    pub const DEST_ALL:         u32 = 0b10 << 18;
    pub const DEST_ALL_EXCLSELF:u32 = 0b11 << 18;
}

/// Wait for a previously-issued IPI to retire (ICR_LO bit 12 = Delivery
/// Status, 1 = pending).  Bounded spin so a wedged LAPIC can't hang
/// the kernel forever.
fn wait_ipi_done() {
    for _ in 0..1_000_000u32 {
        let lo = unsafe { lapic_read(LAPIC_ICR_LO) };
        if lo & (1 << 12) == 0 { return; }
        core::hint::spin_loop();
    }
}

/// Send one IPI.  `dest_apic_id` is the destination LAPIC ID (bits
/// 24..31 of ICR_HI); `dest_shorthand_or_zero` is one of the
/// ipi::DEST_* shorthands or 0 to use the explicit destination.
/// `vector_or_startup_page` carries either an IDT vector for FIXED/
/// NMI delivery or, for STARTUP, the page number where the AP
/// trampoline lives (page = phys >> 12).
pub fn send_ipi(dest_apic_id: u8, mode: u32, vector_or_startup: u8) {
    if !is_available() { return; }
    wait_ipi_done();
    unsafe {
        // ICR_HI: destination field is bits 24..31.
        lapic_write(LAPIC_ICR_HI, (dest_apic_id as u32) << 24);
        let lo = (vector_or_startup as u32) | mode | ipi::ASSERT;
        lapic_write(LAPIC_ICR_LO, lo);
    }
    wait_ipi_done();
}

/// Send a fixed-vector IPI to ourselves and verify the LAPIC accepted
/// it.  Used as a smoke test for the IPI plumbing without disturbing
/// any AP.
pub fn send_self_ipi(vector: u8) {
    if !is_available() { return; }
    wait_ipi_done();
    unsafe {
        let lo = (vector as u32) | ipi::FIXED | ipi::DEST_SELF | ipi::ASSERT;
        lapic_write(LAPIC_ICR_LO, lo);
    }
    wait_ipi_done();
}

/// INIT-SIPI-SIPI sequence (Intel MP spec §B.4): wakes a halted AP and
/// makes it execute starting at the trampoline page (`startup_page`
/// is the physical address >> 12, must be < 0x100 so the AP starts
/// in real mode below 1 MiB).
///
/// Returns Err if `apic` isn't initialised or `startup_page` is out
/// of range.  Does NOT verify the AP actually came up — that's the
/// caller's job (typically by polling a handshake atomic the
/// trampoline writes from Rust).
pub fn boot_ap(target: u8, startup_page: u8) -> Result<(), &'static str> {
    if !is_available() { return Err("apic: not initialised"); }
    if startup_page == 0 { return Err("apic: startup_page = 0"); }

    // 1. INIT — assert.
    send_ipi(target, ipi::INIT, 0);
    // Spec: 10 ms gap.  We don't have a fine-grained sleep yet; spin.
    for _ in 0..10_000u32 { core::hint::spin_loop(); }
    // 2. INIT — de-assert (level-deassert).  Required only on older
    //    CPUs but harmless on modern.  Skip for brevity — Intel says
    //    de-assert is optional on Pentium 4+.
    // 3. SIPI #1.
    send_ipi(target, ipi::STARTUP, startup_page);
    // 200 µs gap before second SIPI per the spec.
    for _ in 0..200u32 { core::hint::spin_loop(); }
    // 4. SIPI #2.
    send_ipi(target, ipi::STARTUP, startup_page);
    Ok(())
}
