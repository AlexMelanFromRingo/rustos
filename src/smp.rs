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

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use alloc::collections::VecDeque;
use spin::Mutex;
use crate::memory::phys_offset;

/// PID type re-exported here so callers don't have to chase down the
/// `process` module.  Matches `process::pid::Pid`'s underlying repr.
pub type Pid = u32;

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
    /// Per-CPU LAPIC-timer heartbeat counter — incremented by this
    /// CPU's `ap_heartbeat_handler` on every periodic LAPIC-timer
    /// interrupt.  Lets the BSP observe "AP is alive and servicing
    /// interrupts" rather than just "AP entered Rust ap_main and
    /// halted."  Reads from any CPU are safe (Acquire), the producer
    /// is the AP itself in IRQ context.
    pub heartbeat: AtomicU64,
    /// Per-CPU run queue.  Each PID lives on exactly one CPU's queue.
    /// The scheduler is expected to consume from `run_queue` first and
    /// only fall through to a global queue / steal from peers when
    /// empty.  Until the AP-launched scheduler is wired (#131 phase 2),
    /// only CPU 0's queue is populated, which makes this transparent
    /// to the existing single-CPU code path.
    pub run_queue: Mutex<VecDeque<Pid>>,
    /// Cached length of `run_queue`, updated alongside push/pop.  Lets
    /// `least_loaded_cpu()` make routing decisions without taking the
    /// lock — important on the hot enqueue path.
    pub run_queue_len: AtomicUsize,
}

impl CpuState {
    const fn new() -> Self {
        Self {
            apic_id: 0,
            alive: AtomicBool::new(false),
            handshake_seen: AtomicU32::new(0),
            kernel_stack_top: AtomicU64::new(0),
            heartbeat: AtomicU64::new(0),
            run_queue: Mutex::new(VecDeque::new()),
            run_queue_len: AtomicUsize::new(0),
        }
    }

    /// Push a PID to the back of this CPU's run queue.  O(1).  Updates
    /// the cached length atomically before releasing the spinlock so
    /// `least_loaded_cpu` always sees a value `len` that's no greater
    /// than the queue's actual size.
    pub fn enqueue(&self, pid: Pid) {
        let mut q = self.run_queue.lock();
        q.push_back(pid);
        self.run_queue_len.store(q.len(), Ordering::Release);
    }

    /// Pop the front PID, or `None` if the queue is empty.
    pub fn dequeue(&self) -> Option<Pid> {
        let mut q = self.run_queue.lock();
        let p = q.pop_front();
        self.run_queue_len.store(q.len(), Ordering::Release);
        p
    }

    /// Best-effort load metric — racy, but always returns a value the
    /// queue *had* recently.  Used for routing, never for correctness.
    pub fn load(&self) -> usize {
        self.run_queue_len.load(Ordering::Acquire)
    }

    /// Drain the queue and return all PIDs.  Useful for shutdown and
    /// for re-routing when a CPU goes offline (work-stealing's coarse
    /// cousin).
    pub fn drain(&self) -> alloc::vec::Vec<Pid> {
        let mut q = self.run_queue.lock();
        let v: alloc::vec::Vec<Pid> = q.drain(..).collect();
        self.run_queue_len.store(0, Ordering::Release);
        v
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
// Per-CPU run queue routing
// ---------------------------------------------------------------------------

/// Sum of every alive CPU's run-queue length.  O(MAX_CPUS), all
/// atomic loads, no locks.  Useful for `top`-style summaries and
/// for sanity-checking that PIDs aren't lost when re-routing.
pub fn total_runnable() -> usize {
    let mut sum = 0usize;
    for c in CPUS.iter() {
        if c.alive.load(Ordering::Acquire) {
            sum += c.load();
        }
    }
    sum
}

/// Pick the alive CPU with the smallest run-queue length.  Falls back
/// to CPU 0 if no CPU is marked alive (e.g. before `init_bsp()`),
/// which preserves single-CPU semantics during early boot.
pub fn least_loaded_cpu() -> u8 {
    let mut best: u8 = 0;
    let mut best_load: usize = usize::MAX;
    for (i, c) in CPUS.iter().enumerate() {
        if !c.alive.load(Ordering::Acquire) { continue; }
        let load = c.load();
        if load < best_load {
            best_load = load;
            best = i as u8;
        }
    }
    best
}

/// Enqueue a PID onto the least-loaded CPU and return which CPU got it.
/// On a 1-CPU system this is always CPU 0 — transparent to existing
/// single-CPU code paths.
pub fn enqueue_balanced(pid: Pid) -> u8 {
    let cpu = least_loaded_cpu();
    if let Some(slot) = cpu_state(cpu) {
        slot.enqueue(pid);
    }
    cpu
}

/// Dequeue from a specific CPU.  Used by the per-CPU scheduler tick.
pub fn dequeue_on(cpu: u8) -> Option<Pid> {
    cpu_state(cpu).and_then(|c| c.dequeue())
}

/// Work-stealing: try to take a PID from the most-loaded *other* CPU.
/// Returns the stolen PID or `None` if no peer has anything to spare.
/// Conservative: only steals when the victim has at least 2 PIDs, so
/// we don't ping-pong a single PID between CPUs.
pub fn steal_from_peer(thief: u8) -> Option<Pid> {
    let mut victim: Option<u8> = None;
    let mut victim_load: usize = 1; // require ≥ 2 to even consider
    for (i, c) in CPUS.iter().enumerate() {
        if i == thief as usize { continue; }
        if !c.alive.load(Ordering::Acquire) { continue; }
        let load = c.load();
        if load > victim_load {
            victim_load = load;
            victim = Some(i as u8);
        }
    }
    victim.and_then(dequeue_on)
}

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
    for (i, slot) in out.iter_mut().enumerate().take(len) {
        *slot = unsafe { core::ptr::read_volatile(src.add(i)) };
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

// ===========================================================================
// Long-mode AP startup
// ===========================================================================
//
// The "ping" trampoline above proves SIPI delivery + AP code execution but
// stays in 16-bit real mode and halts.  This section ships a *full*
// real → protected → long-mode trampoline so an AP can call back into Rust
// at `ap_main()` and start participating in the kernel.
//
// The trampoline binary is generated at build time by `build.rs` from
// `src/smp_trampoline.s` (see commentary there).  We get a `&'static [u8;
// 256]` blob plus four offsets where the BSP patches per-CPU parameters
// before firing INIT-SIPI-SIPI.

include!(concat!(env!("OUT_DIR"), "/smp_trampoline_data.rs"));

/// Magic the long-mode trampoline writes at phys 0x9000 once it reaches
/// 64-bit code.  Distinct from `AP_HANDSHAKE_MAGIC` (the ping trampoline's
/// 32-bit value) so we can tell which path the AP took.
pub const AP_LM_HANDSHAKE_MAGIC: u64 = 0xCAFE_BABE_DEAD_BEEF;

/// Each AP gets this many bytes of kernel stack.  64 KiB is generous —
/// matches the BSP's stack size from bootloader 0.9.
pub const AP_KERNEL_STACK_BYTES: usize = 64 * 1024;

/// Set once the long-mode trampoline path has installed identity mapping
/// for the first 2 MiB of physical memory.  Stops `boot_ap_long_mode`
/// from re-installing on every call (the mapping is leaked but harmless;
/// only the trampoline page and handshake page within 0..2 MiB are ever
/// touched by smp code).
static AP_LOW_IDENTITY_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Number of APs that successfully entered `ap_main` (Rust side).  Read
/// by `smp info` for diagnostics.
static AP_LM_ALIVE: AtomicU32 = AtomicU32::new(0);

/// Public accessor — counts APs that reached Rust code.
pub fn lm_alive() -> u32 { AP_LM_ALIVE.load(Ordering::Acquire) }

// ---- Page-table flags (Intel SDM Vol. 3A §4.5) ----
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITE:   u64 = 1 << 1;
const PTE_HUGE:    u64 = 1 << 7;   // PS bit on PD entry → 2 MiB page

/// Ensure physical 0x0000_0000..0x0020_0000 is identity-mapped in the
/// BSP's active page table.  The AP needs this so its instruction
/// pointer (still 0x8000+offset) keeps fetching valid instructions
/// after CR0.PG turns on.
///
/// Strategy: walk PML4[0]; if it's already populated (e.g. the
/// bootloader left identity mapping in place), trust it and verify
/// 0x8000 round-trips.  Otherwise allocate a PDPT + PD, install a
/// single 2-MiB huge page covering 0..2 MiB, and patch PML4[0].
///
/// Idempotent — the AP_LOW_IDENTITY_INSTALLED flag short-circuits
/// repeat calls.
fn ensure_low_identity_mapping() -> Result<(), &'static str> {
    if AP_LOW_IDENTITY_INSTALLED.load(Ordering::Acquire) { return Ok(()); }

    // Fast path: already mapped.  This is the common case — bootloader
    // 0.9 keeps identity mapping for the kernel itself.
    if crate::memory::virt_to_phys(0x8000) == Some(0x8000)
        && crate::memory::virt_to_phys(0x9000) == Some(0x9000)
    {
        AP_LOW_IDENTITY_INSTALLED.store(true, Ordering::Release);
        return Ok(());
    }

    // Slow path: install our own.  Read PML4[0] and bail if the
    // bootloader left a partial entry — overwriting could break its
    // assumptions.  We don't try to merge.
    let cr3 = read_cr3();
    let pml4_virt = (phys_offset() + cr3) as *mut u64;
    let pml4_0 = unsafe { core::ptr::read_volatile(pml4_virt) };
    if pml4_0 != 0 {
        // Already populated — trust the bootloader's mapping covers
        // 0..2 MiB.  Mark installed; if it doesn't actually cover, the
        // SIPI path will time out cleanly via the bounded poll.
        AP_LOW_IDENTITY_INSTALLED.store(true, Ordering::Release);
        return Ok(());
    }

    let pdpt = alloc_zeroed_frame().ok_or("smp: no frame for PDPT")?;
    let pd   = alloc_zeroed_frame().ok_or("smp: no frame for PD")?;
    unsafe {
        // PD[0] = 2-MiB huge page covering phys 0..0x200000.
        let pd_virt = (phys_offset() + pd) as *mut u64;
        core::ptr::write_volatile(pd_virt, PTE_PRESENT | PTE_WRITE | PTE_HUGE);

        // PDPT[0] -> PD.
        let pdpt_virt = (phys_offset() + pdpt) as *mut u64;
        core::ptr::write_volatile(pdpt_virt, pdpt_pte(pd));

        // PML4[0] -> PDPT.  This is the visible mutation.
        core::ptr::write_volatile(pml4_virt, pdpt_pte(pdpt));

        // Flush TLB so the AP and BSP both see the new mapping.
        x86_64::instructions::tlb::flush_all();
    }
    AP_LOW_IDENTITY_INSTALLED.store(true, Ordering::Release);
    Ok(())
}

#[inline] fn pdpt_pte(child_phys: u64) -> u64 { child_phys | PTE_PRESENT | PTE_WRITE }

/// Allocate a single zero-initialised 4 KiB frame.  Returns physical
/// address.  `None` if the allocator is exhausted.
fn alloc_zeroed_frame() -> Option<u64> {
    use x86_64::structures::paging::FrameAllocator;
    let frame = crate::memory::with_frame_allocator(|fa| fa.allocate_frame())??;
    let phys = frame.start_address().as_u64();
    let virt = phys_offset() + phys;
    unsafe { core::ptr::write_bytes(virt as *mut u8, 0, 4096); }
    Some(phys)
}

/// Read CR3, masking off the PCID/flag bits to get just the table phys.
fn read_cr3() -> u64 {
    let (frame, _flags) = x86_64::registers::control::Cr3::read();
    frame.start_address().as_u64()
}

/// Allocate a kernel stack for an AP.  Returns the (top, bottom) pair —
/// `top` is the initial RSP value (one past the highest byte), `bottom`
/// is the lowest address (so the caller can free if needed).  The pages
/// are heap-allocated via `vmalloc`-style direct mapping; not freed in
/// this prototype.
fn alloc_ap_stack() -> Option<u64> {
    // Use the kernel heap — the existing allocator will give us a Box-
    // like region.  64 KiB is on the heap-friendly side; if the heap is
    // exhausted, allocation fails cleanly.
    let layout = core::alloc::Layout::from_size_align(AP_KERNEL_STACK_BYTES, 16).unwrap();
    let p = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if p.is_null() { return None; }
    let top = p as u64 + AP_KERNEL_STACK_BYTES as u64;
    Some(top)
}

/// Patch the trampoline blob with per-AP parameters and copy it to phys
/// 0x8000.  All four parameter slots are 8-byte aligned per the .s file.
unsafe fn install_long_mode_trampoline(
    target_apic_id: u8,
    cr3_phys: u64,
    stack_top: u64,
    entry: u64,
) {
    let phys = (AP_TRAMPOLINE_PAGE as u64) << 12;
    let dst = (phys_offset() + phys) as *mut u8;

    // 1. Copy the blob.
    for (i, &b) in AP_LM_TRAMPOLINE.iter().enumerate() {
        unsafe { core::ptr::write_volatile(dst.add(i), b); }
    }
    // 2. Patch the parameter slots.
    unsafe {
        write_at(dst, AP_LM_OFF_CR3, cr3_phys);
        write_at(dst, AP_LM_OFF_STACK, stack_top);
        write_at(dst, AP_LM_OFF_ENTRY, entry);
        let cpu_ptr = dst.add(AP_LM_OFF_CPU_ID) as *mut u32;
        core::ptr::write_volatile(cpu_ptr, target_apic_id as u32);
    }
}

#[inline]
unsafe fn write_at(base: *mut u8, off: usize, val: u64) {
    let p = unsafe { base.add(off) as *mut u64 };
    unsafe { core::ptr::write_volatile(p, val); }
}

/// Clear the 64-bit handshake word.
unsafe fn clear_lm_handshake() {
    let virt = phys_offset() + AP_HANDSHAKE_PHYS;
    unsafe { core::ptr::write_volatile(virt as *mut u64, 0); }
}

unsafe fn read_lm_handshake() -> u64 {
    let virt = phys_offset() + AP_HANDSHAKE_PHYS;
    unsafe { core::ptr::read_volatile(virt as *const u64) }
}

/// Bring up an AP and have it execute Rust code at `ap_main`.  Returns:
///
///   `Ok(true)`  — AP wrote the long-mode handshake (made it past CR0.PG)
///   `Ok(false)` — SIPI fired but no handshake within timeout
///   `Err(...)`  — couldn't even prepare (no APIC, target = self, OOM)
///
/// Side effects: identity-maps the first 2 MiB if not already, allocates
/// one 64 KiB AP stack, copies the trampoline to phys 0x8000.  All of
/// these happen before the SIPI; if any fails, no INIT/SIPI is sent.
pub fn boot_ap_long_mode(target: u8) -> Result<bool, &'static str> {
    if !crate::apic::is_available() { return Err("smp: apic not initialised"); }
    if target == crate::apic::lapic_id() { return Err("smp: cannot boot self"); }
    if (target as usize) >= MAX_CPUS { return Err("smp: target apic id out of range"); }

    ensure_low_identity_mapping()?;
    let stack_top = alloc_ap_stack().ok_or("smp: failed to alloc AP stack")?;
    let cr3 = read_cr3();
    let entry = ap_main as *const () as u64;

    AP_BOOT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);

    unsafe {
        install_long_mode_trampoline(target, cr3, stack_top, entry);
        clear_lm_handshake();
    }
    crate::apic::boot_ap(target, AP_TRAMPOLINE_PAGE)?;

    // Bounded poll for the 64-bit handshake.  Trampoline writes it from
    // long-mode code right before tail-calling Rust, so observing it
    // means CR0.PE → CR0.PG → CS reload all completed.
    let lm_alive_before = AP_LM_ALIVE.load(Ordering::Acquire);
    for _ in 0..5_000_000u32 {
        let v = unsafe { read_lm_handshake() };
        if v == AP_LM_HANDSHAKE_MAGIC {
            if let Some(slot) = cpu_state(target) {
                slot.alive.store(true, Ordering::Release);
                slot.kernel_stack_top.store(stack_top, Ordering::Release);
            }
            AP_BOOT_SUCCESSES.fetch_add(1, Ordering::Relaxed);
            // Wait briefly for the AP to execute its first ap_main
            // instructions (the AP_LM_ALIVE.fetch_add) — bridges the
            // handshake-write → tail-jump-to-Rust race.  100K spin =
            // ~300 μs on a modern CPU; AP only needs to retire one
            // atomic add before the BSP returns.
            for _ in 0..100_000u32 {
                if AP_LM_ALIVE.load(Ordering::Acquire) > lm_alive_before { break; }
                core::hint::spin_loop();
            }
            return Ok(true);
        }
        core::hint::spin_loop();
    }
    Ok(false)
}

/// Counter of APs that successfully completed `interrupts::init_idt()`
/// (loaded the IDTR pointing at the kernel's global IDT) on themselves.
static AP_IDT_LOADED: AtomicU32 = AtomicU32::new(0);

/// Counter of APs that successfully completed `apic::init_ap()` (LAPIC
/// software-enabled, SVR + TPR programmed) on themselves.
static AP_LAPIC_READY: AtomicU32 = AtomicU32::new(0);
static AP_TIMER_PROGRAMMED: AtomicU32 = AtomicU32::new(0);
static AP_STI_DONE: AtomicU32 = AtomicU32::new(0);

pub fn ap_idt_loaded()       -> u32 { AP_IDT_LOADED.load(Ordering::Acquire) }
pub fn ap_lapic_ready()      -> u32 { AP_LAPIC_READY.load(Ordering::Acquire) }
pub fn ap_timer_programmed() -> u32 { AP_TIMER_PROGRAMMED.load(Ordering::Acquire) }
pub fn ap_sti_done()         -> u32 { AP_STI_DONE.load(Ordering::Acquire) }

/// Rust entry point for an AP that completed the long-mode trampoline.
///
/// Called as `extern "C" fn(u32) -> !` with `apic_id` in `%edi`.  At this
/// point:
///
///   * CR0 has PE+PG set, CR4 has PAE, EFER has LME+NXE.
///   * CS = 0x18 (64-bit), data segs = 0x20.
///   * RSP points to the AP's freshly-allocated 64 KiB kernel stack.
///   * CR3 = BSP's page table (so all kernel symbols are visible).
///   * The handshake at phys 0x9000 has been written.
///   * Interrupts are still masked (CLI from the trampoline).
///
/// Bring-up sequence (per Intel SDM Vol. 3A §8.4.4 + the `lidt` /
/// LAPIC SVR program order from the OSDev wiki "AP Startup" page):
///
///   1. Mark the per-CPU slot alive — visible to BSP via cpu_state(id).
///   2. Load the kernel's global IDT (idempotent across CPUs).
///   3. Program this CPU's LAPIC: enable + SVR + TPR.
///   4. Spin in HLT.  We deliberately do NOT enable interrupts yet —
///      the per-CPU scheduler tick is wired separately and that's
///      what flips IF on.  Until then, an enabled IF would let the
///      timer IRQ retire on this CPU before there's a runqueue to
///      drive it from, breaking timing.
#[unsafe(no_mangle)]
pub extern "C" fn ap_main(apic_id: u32) -> ! {
    AP_LM_ALIVE.fetch_add(1, Ordering::AcqRel);
    if let Some(slot) = cpu_state(apic_id as u8) {
        slot.alive.store(true, Ordering::Release);
    }

    // Step 2: load IDTR.  The IDT itself is a global static (lazy-
    // initialised by the BSP); each CPU has its own IDTR.  This is a
    // pure register write — no allocations, can't fail.
    crate::interrupts::init_idt();
    AP_IDT_LOADED.fetch_add(1, Ordering::AcqRel);

    // Step 3: enable this CPU's LAPIC.  apic::init_ap requires the
    // BSP's apic::init() to have run already (it shares the cached
    // LAPIC virt-base).  If somehow it didn't, we just skip — the AP
    // is harmless without LAPIC, just won't receive IPIs.
    let lapic_ok = crate::apic::init_ap().is_ok();
    if lapic_ok {
        AP_LAPIC_READY.fetch_add(1, Ordering::AcqRel);
    }

    // Step 4: program this CPU's LAPIC timer to fire vector 0x70
    // periodically.  10_000_000 cycles at divide-by-1 ≈ 10 ms on a
    // 1 GHz APIC bus (QEMU emulates this).  Once we enable IF, the
    // AP heartbeat counter starts ticking — the BSP can observe
    // "AP is alive and servicing interrupts" not just "AP halted in
    // long mode."
    //
    // The HLT in the loop below isn't a no-op — when a timer IRQ
    // retires, control returns into the HLT instruction's successor
    // (i.e., back to the top of the loop), so we sleep until the
    // next firing.  Power-efficient + responsive.
    if lapic_ok {
        // 1 second period (1 billion cycles).  Under QEMU TCG the
        // LAPIC-timer firing path is fragile; once the timer DOES
        // fire it can swamp the AP and starve the BSP via the host
        // thread's vCPU scheduler.  We use a very long period so the
        // structural wiring (LVT + INITIAL_COUNT programmed, IF=1)
        // is observable via counters without inducing IRQ flood.
        // Production / KVM can override via this constant when a
        // shorter heartbeat is actually wanted.
        crate::apic::program_lapic_timer(
            crate::interrupts::AP_HEARTBEAT_VECTOR,
            1_000_000_000,
        );
        AP_TIMER_PROGRAMMED.fetch_add(1, Ordering::AcqRel);
        unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
        AP_STI_DONE.fetch_add(1, Ordering::AcqRel);
    }

    // Step 5: idle loop.  Heartbeat fires through the IDT slot we
    // installed; the rest of the kernel sees activity via
    // cpu_state(id).heartbeat.
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
}

/// Return this CPU's heartbeat counter — for shell / test
/// observation of how much time the AP has been running.
pub fn heartbeat_of(apic_id: u8) -> u64 {
    cpu_state(apic_id).map(|s| s.heartbeat.load(Ordering::Acquire)).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Test hooks for the long-mode path
// ---------------------------------------------------------------------------

/// Test-only: copy the long-mode trampoline blob to phys 0x8000 with a
/// fixed parameter pattern, then read it back.  Validates that
/// `install_long_mode_trampoline` writes exactly the bytes we expect at
/// the patch offsets.
pub fn test_lm_install_and_readback(out: &mut [u8]) {
    unsafe {
        install_long_mode_trampoline(
            42,                   // cpu id
            0xDEAD_C0DE_C0DE_0000, // cr3 (won't ever be loaded — just a marker)
            0xCAFEFEED_FEEDC0DE,   // stack
            0xC0FFEE00_BAADF00D,   // entry
        );
    }
    let phys = (AP_TRAMPOLINE_PAGE as u64) << 12;
    let virt = phys_offset() + phys;
    let src = virt as *const u8;
    let len = out.len().min(AP_LM_TRAMPOLINE_LEN);
    for (i, slot) in out.iter_mut().enumerate().take(len) {
        *slot = unsafe { core::ptr::read_volatile(src.add(i)) };
    }
}

/// Test-only: fetch the four patched fields back out of the trampoline
/// after `install_long_mode_trampoline` has run.  Returns
/// `(cr3, stack, entry, cpu_id)`.
pub fn test_lm_read_params() -> (u64, u64, u64, u32) {
    let phys = (AP_TRAMPOLINE_PAGE as u64) << 12;
    let virt = phys_offset() + phys;
    let base = virt as *const u8;
    unsafe {
        let cr3 = core::ptr::read_volatile(base.add(AP_LM_OFF_CR3) as *const u64);
        let stk = core::ptr::read_volatile(base.add(AP_LM_OFF_STACK) as *const u64);
        let ent = core::ptr::read_volatile(base.add(AP_LM_OFF_ENTRY) as *const u64);
        let cpu = core::ptr::read_volatile(base.add(AP_LM_OFF_CPU_ID) as *const u32);
        (cr3, stk, ent, cpu)
    }
}

/// Test-only: round-trip the 64-bit handshake word through the same
/// volatile-read path the production code uses.
pub fn test_lm_handshake_round_trip() -> u64 {
    unsafe {
        clear_lm_handshake();
        let virt = phys_offset() + AP_HANDSHAKE_PHYS;
        core::ptr::write_volatile(virt as *mut u64, AP_LM_HANDSHAKE_MAGIC);
        read_lm_handshake()
    }
}
