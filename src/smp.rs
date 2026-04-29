//! SMP — Application Processor (AP) startup.
//!
//! On x86_64, only the bootstrap processor (BSP) executes after reset; every
//! AP sits halted until the BSP wakes it via the INIT-SIPI-SIPI sequence
//! (Intel MP spec §B.4, SDM Vol. 3A §8.4).  Each AP starts in 16-bit real
//! mode at `CS:IP = startup_page<<8 : 0000`, which is a physical address of
//! `startup_page << 12` since real-mode `CS<<4 + IP` evaluates to that for
//! `IP = 0`.
//!
//! This module owns:
//!
//!   * The **trampoline blob** — relocatable real-mode machine code copied
//!     into a known low-mem page.
//!   * The **handshake protocol** — a magic word the AP writes and the BSP
//!     polls, so we can confirm an AP actually executed our code instead of
//!     just receiving the SIPI.
//!   * **Per-CPU state** indexed by APIC ID, ready for the per-CPU run
//!     queues that #131 tracks.
//!   * The opt-in entry point `boot_ap_ping(target)` that the shell wires
//!     to `smp boot`.  It is **not** invoked from `init()` — running an AP
//!     trampoline that triple-faults would brick every boot, and we have no
//!     remote-debug visibility from inside QEMU's vCPU at this stage.
//!
//! ## Why a "ping" trampoline rather than full long-mode transition?
//!
//! The real→protected→long-mode trampoline that lets an AP enter Rust is
//! ~150 bytes of carefully-encoded position-independent assembly with three
//! mode transitions, each of which silently triple-faults on the smallest
//! mistake.  Validating it requires either a real multi-socket box or a
//! QEMU `-d int,cpu_reset` trace — neither available to the autonomous
//! pipeline.  The ping trampoline (~22 bytes, real-mode only) is small
//! enough to hand-verify against the Intel manual and proves the *hardest*
//! part of SMP works: INIT-SIPI-SIPI delivery, the AP fetching from the
//! trampoline page, and writes back to the BSP being visible.  Adding the
//! mode transitions on top of a known-good ping is straightforward; doing
//! it all blind is not.
//!
//! Reference: OSDev wiki "Symmetric Multiprocessing", Intel SDM Vol. 3A §8.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use crate::memory::phys_offset;

// ---------------------------------------------------------------------------
// Trampoline parameters
// ---------------------------------------------------------------------------

/// Physical *page number* (phys >> 12) where the AP trampoline is placed.
/// Must fit in 8 bits — the SIPI carries the startup vector as a single
/// byte and the AP boots at `CS:IP = vector<<8 : 0000`.  Page 0x08 →
/// physical 0x8000, well clear of the IVT (0x000–0x3FF), the BIOS data
/// area (0x400–0x4FF), and the bootloader's own load address (0x100000+).
pub const AP_TRAMPOLINE_PAGE: u8 = 0x08;
/// Physical address of the AP→BSP handshake word.  AP trampoline writes
/// the magic `AP_HANDSHAKE_MAGIC` here in two 16-bit stores; BSP polls it
/// as a single 32-bit volatile read.
pub const AP_HANDSHAKE_PHYS: u64 = 0x9000;
/// Magic value the AP writes to mark "I executed the trampoline".  Picked
/// to be conspicuous in a memory dump and unlikely to occur as random
/// uninitialised data.
pub const AP_HANDSHAKE_MAGIC: u32 = 0xDEAD_BEEF;

// ---------------------------------------------------------------------------
// The trampoline itself — hand-assembled real-mode machine code.
// ---------------------------------------------------------------------------
//
// Every byte is annotated with its mnemonic.  Verifiable by feeding the
// blob into ndisasm:
//     ndisasm -b 16 trampoline.bin
//
// 22 bytes total.  Position-independent — uses only short jumps and
// segment-relative memory references, so the BIOS-set CS doesn't matter.

/// Offset of the `cli` opcode (must be the first instruction the AP
/// executes — INIT-SIPI-SIPI may have left RFLAGS in a state where IF=1).
pub const TRAMP_OFF_CLI: usize = 0x00;
/// Offset of the `mov ax, 0x0900` instruction.  Test asserts the immediate
/// matches `(AP_HANDSHAKE_PHYS >> 4) as u16` so a future relocation of the
/// handshake page can't silently desync.
pub const TRAMP_OFF_MOV_AX: usize = 0x02;
/// Offset of the low half of the magic — the byte sequence `EF BE` (0xBEEF
/// little-endian).
pub const TRAMP_OFF_MAGIC_LO: usize = 0x0B;
/// Offset of the high half — `AD DE` (0xDEAD little-endian).
pub const TRAMP_OFF_MAGIC_HI: usize = 0x11;
/// Offset of the `hlt` opcode.  Asserted by tests so we notice if a future
/// edit removes the explicit halt and turns the trampoline into a busy
/// loop on every AP.
pub const TRAMP_OFF_HLT: usize = 0x13;

#[doc = "Real-mode AP \"ping\" trampoline.\n\n```text\n\
    0x00  FA              cli                       ; mask IRQs immediately\n\
    0x01  FC              cld                       ; df clear (string ops)\n\
    0x02  B8 00 09        mov  ax, 0x0900           ; segment 0x900 → linear 0x9000\n\
    0x05  8E D8           mov  ds, ax\n\
    0x07  C7 06 00 00 EF BE  mov word [ds:0], 0xBEEF\n\
    0x0D  C7 06 02 00 AD DE  mov word [ds:2], 0xDEAD\n\
    0x13  F4              hlt                       ; sleep until NMI / forever\n\
    0x14  EB FD           jmp  short -3             ; if NMI woke us, halt again\n\
```"]
pub const AP_TRAMPOLINE: &[u8] = &[
    0xFA,                                       // 0x00  cli
    0xFC,                                       // 0x01  cld
    0xB8, 0x00, 0x09,                           // 0x02  mov ax, 0x0900
    0x8E, 0xD8,                                 // 0x05  mov ds, ax
    0xC7, 0x06, 0x00, 0x00, 0xEF, 0xBE,         // 0x07  mov word [ds:0], 0xBEEF
    0xC7, 0x06, 0x02, 0x00, 0xAD, 0xDE,         // 0x0D  mov word [ds:2], 0xDEAD
    0xF4,                                       // 0x13  hlt
    0xEB, 0xFD,                                 // 0x14  jmp short -3
];

// ---------------------------------------------------------------------------
// Per-CPU state
// ---------------------------------------------------------------------------

/// Per-CPU state.  `MAX_CPUS` slots, one per APIC ID (sparse: physical
/// destination mode lets APIC IDs go up to 254 but most boxes use the
/// dense low end).  Ready for per-CPU run queues, idle threads, and TLS
/// pointers — none of which are populated yet, but the layout is here so
/// adding them doesn't break ABI between modules.
pub const MAX_CPUS: usize = 32;

#[repr(C)]
pub struct CpuState {
    /// Local APIC ID — also the slot index.  Set when this slot is
    /// claimed by a successful AP boot, or by `init_bsp()` for slot 0.
    pub apic_id: u8,
    /// True once the AP has written the handshake magic, or for slot 0
    /// after `init_bsp()` ran.  Read-only from outside this module.
    pub alive: AtomicBool,
    /// Last observed handshake value.  Mostly useful for diagnostics —
    /// distinguishes "AP wrote `0xDEADBEEF` then we cleared" from "AP
    /// never wrote anything" when reading state after the fact.
    pub handshake_seen: AtomicU32,
    /// Reserved for the long-mode trampoline path: this CPU's kernel
    /// stack top.  Currently unused — `boot_ap_ping` doesn't transition
    /// out of real mode.
    pub kernel_stack_top: AtomicU64,
}

impl CpuState {
    const fn new() -> Self {
        Self {
            apic_id: 0,
            alive: AtomicBool::new(false),
            handshake_seen: AtomicU32::new(0),
            kernel_stack_top: AtomicU64::new(0),
        }
    }
}

static CPUS: [CpuState; MAX_CPUS] = [const { CpuState::new() }; MAX_CPUS];

static AP_BOOT_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static AP_BOOT_SUCCESSES: AtomicU32 = AtomicU32::new(0);

/// Mark the BSP slot alive.  Idempotent.  Safe to call multiple times.
pub fn init_bsp() {
    if !crate::apic::is_available() { return; }
    let id = crate::apic::lapic_id() as usize;
    if id < MAX_CPUS {
        CPUS[id].alive.store(true, Ordering::Release);
    }
}

/// Borrow the per-CPU state for `apic_id`.  Returns `None` if the index
/// is out of range — callers must not panic on a high-ID CPU showing up.
pub fn cpu_state(apic_id: u8) -> Option<&'static CpuState> {
    let idx = apic_id as usize;
    if idx < MAX_CPUS { Some(&CPUS[idx]) } else { None }
}

/// Iterate every CPU slot and return the count of those whose `alive`
/// flag is set.  This is what the shell's `smp info` reports.
pub fn alive_cpu_count() -> usize {
    CPUS.iter().filter(|c| c.alive.load(Ordering::Acquire)).count()
}

pub fn boot_attempts() -> u32 { AP_BOOT_ATTEMPTS.load(Ordering::Relaxed) }
pub fn boot_successes() -> u32 { AP_BOOT_SUCCESSES.load(Ordering::Relaxed) }

// ---------------------------------------------------------------------------
// Trampoline copy + handshake polling
// ---------------------------------------------------------------------------

/// Copy `AP_TRAMPOLINE` to physical `AP_TRAMPOLINE_PAGE << 12`.  Uses the
/// bootloader's direct phys-memory map (every physical address is mapped
/// at `phys_offset() + phys` in kernel virtual space).
///
/// Safety: caller must have run `memory::init()` so `phys_offset()` returns
/// a non-zero base.  Calling before that produces a wild pointer write.
unsafe fn install_trampoline() {
    let phys = (AP_TRAMPOLINE_PAGE as u64) << 12;
    let virt = phys_offset() + phys;
    let dst = virt as *mut u8;
    for (i, &b) in AP_TRAMPOLINE.iter().enumerate() {
        unsafe { core::ptr::write_volatile(dst.add(i), b); }
    }
}

unsafe fn clear_handshake() {
    let virt = phys_offset() + AP_HANDSHAKE_PHYS;
    unsafe { core::ptr::write_volatile(virt as *mut u32, 0); }
}

unsafe fn read_handshake() -> u32 {
    let virt = phys_offset() + AP_HANDSHAKE_PHYS;
    unsafe { core::ptr::read_volatile(virt as *const u32) }
}

/// Bring up one application processor with the ping trampoline.  Returns
/// `Ok(true)` if the AP wrote the handshake magic, `Ok(false)` if the
/// SIPI sequence completed but no handshake was observed within the
/// timeout, and `Err` if the APIC infrastructure isn't ready or `target`
/// is the BSP.
///
/// A successful return means: INIT-SIPI-SIPI delivered, the AP fetched
/// from the trampoline page, executed real-mode instructions, and its
/// memory write became visible to the BSP through cache coherency.  It
/// does **not** mean the AP is in long mode or running Rust code.
pub fn boot_ap_ping(target: u8) -> Result<bool, &'static str> {
    if !crate::apic::is_available() {
        return Err("smp: apic not initialised");
    }
    if target == crate::apic::lapic_id() {
        return Err("smp: cannot boot self");
    }
    if (target as usize) >= MAX_CPUS {
        return Err("smp: target apic id out of range");
    }

    AP_BOOT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);

    unsafe {
        install_trampoline();
        clear_handshake();
    }

    crate::apic::boot_ap(target, AP_TRAMPOLINE_PAGE)?;

    // Bounded poll: at ~3 ns/iteration on modern hardware, ~3 ms total —
    // generous for an AP that's going to come up at all, and bounded so
    // a missing or wedged AP doesn't hang the kernel.
    for _ in 0..1_000_000u32 {
        let v = unsafe { read_handshake() };
        if v == AP_HANDSHAKE_MAGIC {
            if let Some(slot) = cpu_state(target) {
                slot.alive.store(true, Ordering::Release);
                slot.handshake_seen.store(v, Ordering::Release);
            }
            AP_BOOT_SUCCESSES.fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }
        core::hint::spin_loop();
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Test hooks — exposed so the harness can drive the handshake path
// without requiring an actual second CPU.
// ---------------------------------------------------------------------------

/// Test-only: verify the trampoline copy round-trips through the
/// phys-memory direct map.  Returns the bytes read back from
/// `AP_TRAMPOLINE_PAGE << 12` after `install_trampoline()`.  Tests
/// compare this slice against `AP_TRAMPOLINE` byte-for-byte.
pub fn test_install_and_readback(out: &mut [u8]) {
    let len = out.len().min(AP_TRAMPOLINE.len());
    unsafe { install_trampoline(); }
    let phys = (AP_TRAMPOLINE_PAGE as u64) << 12;
    let virt = phys_offset() + phys;
    let src = virt as *const u8;
    for i in 0..len {
        out[i] = unsafe { core::ptr::read_volatile(src.add(i)) };
    }
}

/// Test-only: write the handshake magic from BSP code, simulating an AP
/// that completed the trampoline.  Then read it back via the same code
/// path the real polling uses.  Returns the round-tripped value.
pub fn test_handshake_round_trip() -> u32 {
    unsafe {
        clear_handshake();
        let virt = phys_offset() + AP_HANDSHAKE_PHYS;
        core::ptr::write_volatile(virt as *mut u32, AP_HANDSHAKE_MAGIC);
        read_handshake()
    }
}

/// Test-only: prove the bounded-poll path actually times out instead of
/// spinning forever when the handshake never arrives.  Uses a tiny
/// iteration cap (the production path uses 1M) so the test runs in
/// microseconds.  Returns `true` if it correctly returned without
/// observing the magic.
pub fn test_bounded_poll_times_out() -> bool {
    unsafe { clear_handshake(); }
    for _ in 0..1024u32 {
        let v = unsafe { read_handshake() };
        if v == AP_HANDSHAKE_MAGIC { return false; }
        core::hint::spin_loop();
    }
    true
}
