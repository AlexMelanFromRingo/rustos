//! HPET (High Precision Event Timer) — read-only support.
//!
//! HPET registers are MMIO at a base discovered via the ACPI HPET table.
//! Without an ACPI parser yet we look for the table address via known
//! defaults: QEMU/SeaBIOS exposes HPET at 0xFED0_0000.  Once the address
//! is known we can read:
//!
//!   * Capabilities register (offset 0x00, 8 bytes): bits 32..63 hold
//!     COUNTER_CLK_PERIOD in femtoseconds.
//!   * Main counter (offset 0xF0, 8 bytes): monotonically incrementing.
//!
//! We do **not** programme comparators or generate interrupts here —
//! that requires IOAPIC routing.  The PIT remains the system tick source.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::VirtAddr;

const HPET_DEFAULT_BASE: u64 = 0xFED0_0000;
const REG_CAPABILITIES:  u64 = 0x000;
const REG_GENERAL_CONFIG:u64 = 0x010;
const REG_MAIN_COUNTER:  u64 = 0x0F0;
const CONFIG_ENABLE_CNF: u64 = 0x1; // bit 0: overall enable

static HPET_BASE: AtomicU64 = AtomicU64::new(0);
static HPET_PERIOD_FS: AtomicU64 = AtomicU64::new(0); // femtoseconds per tick
static AVAILABLE: AtomicBool = AtomicBool::new(false);

unsafe fn read64(off: u64) -> u64 {
    let base = HPET_BASE.load(Ordering::Relaxed);
    let ptr = (base + off) as *const u64;
    unsafe { core::ptr::read_volatile(ptr) }
}

unsafe fn write64(off: u64, val: u64) {
    let base = HPET_BASE.load(Ordering::Relaxed);
    let ptr = (base + off) as *mut u64;
    unsafe { core::ptr::write_volatile(ptr, val); }
}

/// Probe HPET at the QEMU default address.  We don't yet have an ACPI
/// parser; if/when we do, this should consult the HPET table.
pub fn init() {
    // Identity-map the page so volatile reads land on real MMIO.
    let base = HPET_DEFAULT_BASE;
    let mapped = unsafe {
        use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
        use x86_64::PhysAddr;
        let mut mapper = crate::memory::get_mapper();
        let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(base));
        let frame = PhysFrame::containing_address(PhysAddr::new(base));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE
            | PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH;
        crate::memory::with_frame_allocator(|fa| {
            mapper.map_to(page, frame, flags, fa).map(|f| f.flush()).is_ok()
        }).unwrap_or(false)
    };
    if !mapped {
        crate::klog_warn!("HPET: unable to map MMIO at {:#x}", base);
        return;
    }
    HPET_BASE.store(base, Ordering::Relaxed);

    let caps = unsafe { read64(REG_CAPABILITIES) };
    let period_fs = caps >> 32;
    if period_fs == 0 || period_fs > 100_000_000 {
        crate::klog_warn!("HPET: invalid CLK_PERIOD ({} fs); disabling", period_fs);
        return;
    }
    HPET_PERIOD_FS.store(period_fs, Ordering::Relaxed);

    // Enable the main counter.  The General Configuration register has
    // bit 0 = ENABLE_CNF; once set the counter starts ticking.  Reset
    // counter to zero first so the first read corresponds to "boot+0".
    unsafe {
        write64(REG_MAIN_COUNTER, 0);
        let cur_cfg = read64(REG_GENERAL_CONFIG);
        write64(REG_GENERAL_CONFIG, cur_cfg | CONFIG_ENABLE_CNF);
    }
    AVAILABLE.store(true, Ordering::Relaxed);

    let freq_hz = if period_fs > 0 {
        1_000_000_000_000_000u64 / period_fs
    } else { 0 };
    crate::klog_info!("HPET: period={} fs (~{} Hz), id={:#x}, ENABLE_CNF set",
        period_fs, freq_hz, caps & 0xFF);
}

/// Current HPET main counter value, or 0 if unavailable.
pub fn read_counter() -> u64 {
    if !AVAILABLE.load(Ordering::Relaxed) { return 0; }
    unsafe { read64(REG_MAIN_COUNTER) }
}

/// Counter period in femtoseconds (10^-15 s) — `period_fs * counter`
/// equals nanoseconds × 1_000_000.
pub fn period_fs() -> u64 { HPET_PERIOD_FS.load(Ordering::Relaxed) }

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }

/// Convert a counter delta to nanoseconds.
pub fn counter_to_ns(counter_delta: u64) -> u64 {
    let p = period_fs();
    if p == 0 { return 0; }
    counter_delta.saturating_mul(p) / 1_000_000
}
