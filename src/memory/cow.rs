//! Copy-on-write physical-frame reference counting.
//!
//! When `fork()` clones a parent process's user mappings into the
//! child, both processes' L1 entries point at the same physical
//! frames but with the WRITABLE flag cleared and a "COW" tag set on
//! the OS-available bits of the L1 entry.  The first write either
//! side performs takes a #PF, and the page-fault handler:
//!
//!   1. Looks up the faulting address in the current process's L1.
//!   2. Confirms the COW tag is set.
//!   3. Calls `cow::resolve(frame)` to either:
//!         (a) drop the refcount and re-mark the entry WRITABLE in
//!             place, when this is the last reference, or
//!         (b) allocate a new frame, copy the page, drop the refcount
//!             on the old frame, and rewrite the L1 entry to point
//!             at the new frame with WRITABLE set.
//!
//! We track refcounts in a flat `Vec<AtomicU32>` indexed by frame
//! number (`phys / 4096`).  The vector grows on demand to cover every
//! frame the bitmap allocator can hand out (≤ 4 GiB of RAM = 1 M
//! entries × 4 B = 4 MiB worst case).

use core::sync::atomic::{AtomicU32, Ordering};
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::{
    structures::paging::{PhysFrame, PageTableFlags, FrameAllocator, Size4KiB},
    PhysAddr, VirtAddr,
};

/// OS-available bit (#9) we use to mark "this PTE was COW-shared and
/// the kernel needs to copy on write".  The page-fault handler sets
/// WRITABLE only after the page is privately owned again.  Bit 9 is
/// one of the four "Available to OS" bits in the x86-64 PTE format.
pub const PTE_COW_BIT: u64 = 1 << 9;

/// Refcount table — index = phys frame number (phys / 4096).
/// Lazily grown.  A frame the kernel allocates outside of fork+COW
/// path has refcount 0 (== "not tracked"); only frames *placed* in
/// the COW pool by `register_shared` have positive counts.
static REFCOUNTS: Mutex<Vec<AtomicU32>> = Mutex::new(Vec::new());

fn ensure_capacity(idx: usize) {
    let mut g = REFCOUNTS.lock();
    let cur = g.len();
    if idx >= cur {
        g.reserve(idx + 1 - cur);
        while g.len() <= idx {
            g.push(AtomicU32::new(0));
        }
    }
}

/// Increment the refcount on `frame`, registering it as COW-shared
/// if it wasn't already.  Called once per L1 entry that fork()
/// duplicates.
pub fn share(frame: PhysFrame) {
    let idx = (frame.start_address().as_u64() / 4096) as usize;
    ensure_capacity(idx);
    let g = REFCOUNTS.lock();
    g[idx].fetch_add(1, Ordering::AcqRel);
}

/// Decrement the refcount.  Returns true if this was the last
/// reference and the caller is now the sole owner.
pub fn release(frame: PhysFrame) -> bool {
    let idx = (frame.start_address().as_u64() / 4096) as usize;
    let g = REFCOUNTS.lock();
    if idx >= g.len() { return true; }
    let prev = g[idx].fetch_sub(1, Ordering::AcqRel);
    prev <= 1
}

/// Read-only inspector: how many processes currently hold this frame
/// shared via COW?
pub fn refcount(frame: PhysFrame) -> u32 {
    let idx = (frame.start_address().as_u64() / 4096) as usize;
    let g = REFCOUNTS.lock();
    g.get(idx).map(|c| c.load(Ordering::Acquire)).unwrap_or(0)
}

/// Resolve a copy-on-write fault on `va` in the page table whose root
/// physical address is `cr3`.  Returns Ok if the entry was rewritten
/// to a private writable mapping, Err if no COW PTE was present (=
/// the fault was something else and the caller should escalate).
///
/// # Safety
/// Walks the page table at `cr3` directly; relies on the bootloader's
/// phys-memory map for access.  Must be called only from the page
/// fault handler with interrupts disabled or with the MM-lock held.
pub unsafe fn resolve(cr3: u64, va: VirtAddr) -> Result<(), &'static str> {
    let l4_idx = (va.as_u64() >> 39) as usize & 0x1FF;
    let l3_idx = (va.as_u64() >> 30) as usize & 0x1FF;
    let l2_idx = (va.as_u64() >> 21) as usize & 0x1FF;
    let l1_idx = (va.as_u64() >> 12) as usize & 0x1FF;

    let phys_off = crate::memory::phys_offset();
    let table_at = |phys: u64| -> &mut x86_64::structures::paging::PageTable {
        unsafe { &mut *((phys_off + phys) as *mut x86_64::structures::paging::PageTable) }
    };

    let l4 = table_at(cr3);
    let l4e = &l4[l4_idx];
    if l4e.is_unused() { return Err("cow: no L4 entry"); }
    let l3 = table_at(l4e.addr().as_u64());
    let l3e = &l3[l3_idx];
    if l3e.is_unused() { return Err("cow: no L3 entry"); }
    let l2 = table_at(l3e.addr().as_u64());
    let l2e = &l2[l2_idx];
    if l2e.is_unused() { return Err("cow: no L2 entry"); }
    let l1 = table_at(l2e.addr().as_u64());
    let l1e = &mut l1[l1_idx];
    if l1e.is_unused() { return Err("cow: no L1 entry"); }

    let raw = unsafe { core::ptr::read_unaligned(l1e as *const _ as *const u64) };
    if raw & PTE_COW_BIT == 0 { return Err("cow: PTE not COW-marked"); }
    let old_phys = l1e.addr();
    let old_frame = PhysFrame::<Size4KiB>::containing_address(old_phys);

    // Decide: last reference → reuse the page in place; else copy.
    let last = release(old_frame);
    if last {
        // We're the sole owner now: just clear COW and set WRITABLE.
        let new_raw = (raw & !PTE_COW_BIT) | PageTableFlags::WRITABLE.bits();
        unsafe { core::ptr::write_unaligned(l1e as *mut _ as *mut u64, new_raw); }
        x86_64::instructions::tlb::flush(va);
        return Ok(());
    }

    // Otherwise allocate a fresh frame, copy the contents, rewrite PTE.
    let new_frame = crate::memory::with_frame_allocator(|fa| fa.allocate_frame())
        .flatten()
        .ok_or("cow: out of frames")?;
    unsafe {
        let dst = (phys_off + new_frame.start_address().as_u64()) as *mut u8;
        let src = (phys_off + old_phys.as_u64()) as *const u8;
        core::ptr::copy_nonoverlapping(src, dst, 4096);
    }
    let mut flags = l1e.flags();
    flags.insert(PageTableFlags::WRITABLE);
    let new_raw = new_frame.start_address().as_u64() | flags.bits();
    let new_raw = new_raw & !PTE_COW_BIT;
    unsafe { core::ptr::write_unaligned(l1e as *mut _ as *mut u64, new_raw); }
    x86_64::instructions::tlb::flush(va);
    Ok(())
}

/// Walk the user portion of the page table at `cr3` and mark every
/// writable user PTE as COW (cleared WRITABLE bit, OS-bit 9 set) and
/// register a refcount share.  Used after `pagetable::create_user_pagetable`
/// in the fork() flow so both parent and child see the pages as COW.
///
/// Returns the number of PTEs we re-marked.
pub fn mark_user_pages_cow(cr3: u64) -> usize {
    let l4_idx = (crate::memory::userspace::USER_SPACE_START >> 39) as usize & 0x1FF;
    let phys_off = crate::memory::phys_offset();
    let l4 = unsafe { &mut *((phys_off + cr3) as *mut x86_64::structures::paging::PageTable) };
    let l4e = &l4[l4_idx];
    if l4e.is_unused() { return 0; }
    let mut count = 0usize;
    let l3 = unsafe { &mut *((phys_off + l4e.addr().as_u64())
        as *mut x86_64::structures::paging::PageTable) };
    for l3_idx in 0..512 {
        let l3e = &l3[l3_idx];
        if l3e.is_unused() { continue; }
        if l3e.flags().contains(PageTableFlags::HUGE_PAGE) { continue; }
        let l2 = unsafe { &mut *((phys_off + l3e.addr().as_u64())
            as *mut x86_64::structures::paging::PageTable) };
        for l2_idx in 0..512 {
            let l2e = &l2[l2_idx];
            if l2e.is_unused() { continue; }
            if l2e.flags().contains(PageTableFlags::HUGE_PAGE) { continue; }
            let l1 = unsafe { &mut *((phys_off + l2e.addr().as_u64())
                as *mut x86_64::structures::paging::PageTable) };
            for l1_idx in 0..512 {
                let l1e = &mut l1[l1_idx];
                if l1e.is_unused() { continue; }
                let flags = l1e.flags();
                if !flags.contains(PageTableFlags::USER_ACCESSIBLE) { continue; }
                if !flags.contains(PageTableFlags::WRITABLE) { continue; }
                // Share the frame, then strip WRITABLE and set COW bit.
                let frame = PhysFrame::<Size4KiB>::containing_address(l1e.addr());
                share(frame);
                let raw_ptr = l1e as *mut _ as *mut u64;
                let raw = unsafe { core::ptr::read_unaligned(raw_ptr) };
                let new_raw = (raw & !PageTableFlags::WRITABLE.bits()) | PTE_COW_BIT;
                unsafe { core::ptr::write_unaligned(raw_ptr, new_raw); }
                count += 1;
            }
        }
    }
    count
}

/// Convenience wrapper: returns the physical address backing a user
/// virtual address in the given CR3 (used by tests).
pub fn user_va_to_phys(cr3: u64, va: VirtAddr) -> Option<PhysAddr> {
    let phys_off = crate::memory::phys_offset();
    let l4 = unsafe { &*((phys_off + cr3) as *const x86_64::structures::paging::PageTable) };
    let l4e = &l4[(va.as_u64() >> 39) as usize & 0x1FF];
    if l4e.is_unused() { return None; }
    let l3 = unsafe { &*((phys_off + l4e.addr().as_u64())
        as *const x86_64::structures::paging::PageTable) };
    let l3e = &l3[(va.as_u64() >> 30) as usize & 0x1FF];
    if l3e.is_unused() { return None; }
    let l2 = unsafe { &*((phys_off + l3e.addr().as_u64())
        as *const x86_64::structures::paging::PageTable) };
    let l2e = &l2[(va.as_u64() >> 21) as usize & 0x1FF];
    if l2e.is_unused() { return None; }
    let l1 = unsafe { &*((phys_off + l2e.addr().as_u64())
        as *const x86_64::structures::paging::PageTable) };
    let l1e = &l1[(va.as_u64() >> 12) as usize & 0x1FF];
    if l1e.is_unused() { return None; }
    Some(l1e.addr() + (va.as_u64() & 0xFFF))
}
