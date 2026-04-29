//! Per-IRQ counters surfaced through /proc/interrupts.
//!
//! Each ISR bumps `bump(irq)` after it does its real work; the stats
//! reader returns a snapshot of (irq_number, friendly_name, count)
//! tuples sorted by IRQ number.  We keep the counter table flat (16
//! ISA + a few extra slots for future MSI vectors) so the bump path
//! is one atomic add.

use core::sync::atomic::{AtomicU64, Ordering};

const N_IRQS: usize = 64;
static COUNTERS: [AtomicU64; N_IRQS] = {
    // Workaround for the "AtomicU64 cannot be Copy in a const array
    // initialiser" rule: use a const fn helper.
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; N_IRQS]
};

const NAMES: &[(u8, &str)] = &[
    (0,  "PIT/HPET timer"),
    (1,  "PS/2 keyboard"),
    (4,  "Serial port 1"),
    (12, "PS/2 mouse"),
    (14, "Primary ATA"),
    (15, "Secondary ATA"),
];

/// Increment the counter for an IRQ vector (or 0..15 for raw IRQ
/// numbers; both work since the array is indexed flat).
pub fn bump(idx: u8) {
    if (idx as usize) < N_IRQS {
        COUNTERS[idx as usize].fetch_add(1, Ordering::Relaxed);
    }
}

/// Snapshot of every non-zero IRQ counter for /proc/interrupts.
pub fn snapshot() -> alloc::vec::Vec<(u8, alloc::string::String, u64)> {
    let mut out = alloc::vec::Vec::new();
    for (i, c) in COUNTERS.iter().enumerate() {
        let v = c.load(Ordering::Relaxed);
        if v == 0 { continue; }
        let name = NAMES.iter().find(|(n, _)| *n as usize == i)
            .map(|(_, s)| (*s).into())
            .unwrap_or_else(|| alloc::format!("IRQ {}", i));
        out.push((i as u8, name, v));
    }
    out
}
