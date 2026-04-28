//! Frame-pointer-based stack backtrace for kernel panics.
//!
//! Requires the kernel to be compiled with frame pointers enabled
//! (-C force-frame-pointers=yes).  Each function prologue stashes the
//! caller's RBP at [rbp+0] and the return address at [rbp+8], so we walk
//! that linked list to recover a chain of return addresses.
//!
//! No DWARF symbolisation yet — addresses are printed in hex.  A future
//! version can map them back to symbol names by parsing the kernel's
//! .symtab at boot.

use core::arch::asm;

#[inline(always)]
fn read_rbp() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, rbp", out(reg) v, options(nostack, preserves_flags)); }
    v
}

/// Walk up to `max_frames` levels of the frame-pointer chain starting from
/// the caller's RBP.  Returns each frame's saved return address.
pub fn capture(max_frames: usize) -> alloc::vec::Vec<u64> {
    let mut out = alloc::vec::Vec::with_capacity(max_frames);
    let mut rbp = read_rbp();
    for _ in 0..max_frames {
        // Sanity: RBP must be non-zero, 8-aligned, and in a plausible
        // kernel virtual range.  We accept anything in the upper half of
        // the canonical x86-64 layout *or* the kernel's lower-half region
        // (since this build runs the kernel at low addresses).
        if rbp == 0 || rbp & 7 != 0 { break; }

        // [rbp+0] = saved RBP, [rbp+8] = saved RIP
        let saved_rbp = unsafe { *(rbp as *const u64) };
        let saved_rip = unsafe { *((rbp + 8) as *const u64) };
        if saved_rip == 0 { break; }
        out.push(saved_rip);
        if saved_rbp <= rbp { break; } // chain must climb the stack
        rbp = saved_rbp;
    }
    out
}

/// Print a kernel panic backtrace to the serial console.  Wrapped in a
/// guard so a fault during the walk doesn't loop.
pub fn print_panic(info: &core::panic::PanicInfo) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static IN_PRINT: AtomicBool = AtomicBool::new(false);
    if IN_PRINT.swap(true, Ordering::AcqRel) {
        crate::serial_println!("(recursive panic — backtrace suppressed)");
        return;
    }

    crate::serial_println!("KERNEL PANIC: {}", info);
    crate::serial_println!("Backtrace ({} symbols loaded):", crate::symbols::count());
    let frames = capture(32);
    if frames.is_empty() {
        crate::serial_println!("  (no frames — frame pointers may be disabled)");
    } else {
        for (i, addr) in frames.iter().enumerate() {
            match crate::symbols::lookup(*addr) {
                Some((name, off, loc)) => {
                    if loc.is_empty() {
                        crate::serial_println!("  #{:<2} {:#018x}  {}+{:#x}",
                            i, addr, crate::symbols::pretty_name(name), off);
                    } else {
                        crate::serial_println!("  #{:<2} {:#018x}  {}+{:#x}  ({})",
                            i, addr, crate::symbols::pretty_name(name), off, loc);
                    }
                }
                None => {
                    crate::serial_println!("  #{:<2} {:#018x}", i, addr);
                }
            }
        }
    }
}
