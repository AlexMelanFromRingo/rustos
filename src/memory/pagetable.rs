//! Per-process page table management.
//!
//! Each user process owns its own PML4 (top-level x86-64 page table).
//! Kernel mappings are *shared by reference* — every PML4 holds the
//! same physical-address entries for the L4 slots that contain only
//! kernel data, so modifying a kernel page (heap growth, vmalloc) is
//! immediately visible from every process without coordinated TLB
//! shoot-downs.
//!
//! User mappings (the L4 entry that contains USER_SPACE_START) are
//! *deep-cloned* per process, which lets `fork()` give parent and
//! child their own L1 entries (later marked copy-on-write — see
//! `crate::memory::cow`).
//!
//! Layout summary, bootloader 0.9 + this kernel:
//!
//!     L4[0]     mixed: kernel image (low MB) + user space (16 MB+)
//!     L4[3]     phys-memory direct map (PHYS_MEM_OFFSET)
//!     L4[N]     kernel heap (0x_4444_4444_0000)
//!     L4[other] kernel page tables, vmalloc, stack
//!
//! L4[0] is the only entry that mixes kernel and user content — for
//! that one we deep-clone the L3 → L2 → L1 path so user-space frames
//! become per-process while kernel frames within the same L3/L2 stay
//! shared.

use x86_64::{
    structures::paging::{
        PageTable, PageTableFlags, PhysFrame, Size4KiB, FrameAllocator,
    },
    PhysAddr,
};
use crate::memory::userspace::{USER_SPACE_START, USER_SPACE_END};

const ENTRIES: usize = 512;
const PAGE_SIZE: u64 = 4096;
const L4_USER_INDEX: usize = (USER_SPACE_START >> 39) as usize & 0x1FF;

/// Convert a frame's physical address into a `&mut PageTable` via the
/// bootloader's direct phys map.  Safe as long as no other code holds
/// a competing reference to the same frame (the caller orchestrates).
unsafe fn frame_as_table(frame: PhysFrame) -> &'static mut PageTable {
    let virt = crate::memory::phys_offset() + frame.start_address().as_u64();
    unsafe { &mut *(virt as *mut PageTable) }
}

/// Allocate one zeroed frame and return both the frame and a fresh
/// `&mut PageTable` view of it.
fn alloc_table<A: FrameAllocator<Size4KiB>>(fa: &mut A)
    -> Option<(PhysFrame, &'static mut PageTable)>
{
    let frame = fa.allocate_frame()?;
    let table = unsafe { frame_as_table(frame) };
    table.zero();
    Some((frame, table))
}

/// Deep-clone the L1 page table at `src_phys` into a freshly-allocated
/// L1 frame.  Every entry is copied verbatim; the caller is expected
/// to walk the new L1 afterwards if they want to mark entries
/// copy-on-write.  Returns the new frame.
fn clone_l1<A: FrameAllocator<Size4KiB>>(src_phys: PhysAddr, fa: &mut A)
    -> Option<PhysFrame>
{
    let src_frame = PhysFrame::containing_address(src_phys);
    let src = unsafe { frame_as_table(src_frame) };
    let (new_frame, dst) = alloc_table(fa)?;
    for i in 0..ENTRIES {
        dst[i] = src[i].clone();
    }
    Some(new_frame)
}

/// Deep-clone an L2: each present entry that is **not** a 2 MiB
/// huge-page leaf is recursed into; huge-page entries (with
/// HUGE_PAGE flag) are copied by reference (shared between processes,
/// since we don't yet COW 2 MiB pages).
fn clone_l2<A: FrameAllocator<Size4KiB>>(src_phys: PhysAddr,
    user_filter: bool, fa: &mut A) -> Option<PhysFrame>
{
    let src_frame = PhysFrame::containing_address(src_phys);
    let src = unsafe { frame_as_table(src_frame) };
    let (new_frame, dst) = alloc_table(fa)?;
    for i in 0..ENTRIES {
        let entry = &src[i];
        if entry.is_unused() { continue; }
        let flags = entry.flags();
        if flags.contains(PageTableFlags::HUGE_PAGE) {
            // Huge page leaf — copy by value (still shared physical frame).
            dst[i] = entry.clone();
            continue;
        }
        // Recurse into L1.
        let l1_phys = entry.addr();
        let user_here = flags.contains(PageTableFlags::USER_ACCESSIBLE);
        let new_l1 = if user_filter && user_here {
            clone_l1(l1_phys, fa)?
        } else {
            // Share kernel L1 by reference.
            PhysFrame::containing_address(l1_phys)
        };
        dst[i].set_addr(new_l1.start_address(), flags);
    }
    Some(new_frame)
}

fn clone_l3<A: FrameAllocator<Size4KiB>>(src_phys: PhysAddr,
    user_filter: bool, fa: &mut A) -> Option<PhysFrame>
{
    let src_frame = PhysFrame::containing_address(src_phys);
    let src = unsafe { frame_as_table(src_frame) };
    let (new_frame, dst) = alloc_table(fa)?;
    for i in 0..ENTRIES {
        let entry = &src[i];
        if entry.is_unused() { continue; }
        let flags = entry.flags();
        if flags.contains(PageTableFlags::HUGE_PAGE) {
            dst[i] = entry.clone();
            continue;
        }
        let l2_phys = entry.addr();
        let user_here = flags.contains(PageTableFlags::USER_ACCESSIBLE);
        let new_l2 = if user_filter && user_here {
            clone_l2(l2_phys, true, fa)?
        } else {
            PhysFrame::containing_address(l2_phys)
        };
        dst[i].set_addr(new_l2.start_address(), flags);
    }
    Some(new_frame)
}

/// Build a fresh PML4 for a new process by cloning the current kernel
/// PML4.  Kernel L4 entries are shared by reference (no copy of L3 /
/// L2 / L1 happens for them).  The L4 entry that holds user memory
/// (computed from `USER_SPACE_START`) is deep-cloned so user
/// mappings become per-process.
///
/// Returns the physical address of the new PML4, ready to be loaded
/// into CR3.
pub fn create_user_pagetable() -> Option<u64> {
    crate::memory::with_frame_allocator(|fa| -> Option<u64> {
        let (mapper_l4_phys, _) = x86_64::registers::control::Cr3::read();
        let src_l4 = unsafe { frame_as_table(mapper_l4_phys) };
        let (new_l4_frame, dst_l4) = alloc_table(fa)?;
        for i in 0..ENTRIES {
            let entry = &src_l4[i];
            if entry.is_unused() { continue; }
            // The user L4 slot needs to be deep-cloned so changes to
            // user mappings don't affect other processes.  Every other
            // slot is shared by reference.
            if i == L4_USER_INDEX {
                let new_l3 = clone_l3(entry.addr(), true, fa)?;
                dst_l4[i].set_addr(new_l3.start_address(), entry.flags());
            } else {
                dst_l4[i] = entry.clone();
            }
        }
        Some(new_l4_frame.start_address().as_u64())
    })?
}

/// Free every per-process L1/L2/L3/L4 frame this process allocated.
/// Pages whose physical frames were shared (no `USER_ACCESSIBLE` flag,
/// or huge-page leaves) are left alone — they belong to the kernel or
/// to a sibling process.
///
/// # Safety
/// The CR3 must not currently equal `cr3` — switch to the kernel
/// idle page table first.
pub unsafe fn destroy_user_pagetable(cr3: u64) {
    if cr3 == 0 { return; }
    crate::memory::with_frame_allocator(|fa| {
        let l4_frame = PhysFrame::containing_address(PhysAddr::new(cr3));
        unsafe { destroy_l4_recursive(l4_frame, fa); }
    });
}

unsafe fn destroy_l4_recursive(frame: PhysFrame, fa: &mut crate::memory::BitmapFrameAllocator) {
    let table = unsafe { frame_as_table(frame) };
    for i in 0..ENTRIES {
        let entry = &table[i];
        if entry.is_unused() { continue; }
        if i == L4_USER_INDEX {
            let l3 = PhysFrame::containing_address(entry.addr());
            unsafe { destroy_lower_recursive(l3, 3, fa); }
        }
        // Other L4 entries are shared kernel mappings; skip.
    }
    unsafe { fa.deallocate_frame(frame); }
}

unsafe fn destroy_lower_recursive(frame: PhysFrame, level: u8,
                                   fa: &mut crate::memory::BitmapFrameAllocator)
{
    if level == 1 {
        // L1 is leaves; we don't free leaf pages here — caller's
        // responsibility for user data frames since they may still be
        // shared (COW).  Just free the L1 page table itself.
        unsafe { fa.deallocate_frame(frame); }
        return;
    }
    let table = unsafe { frame_as_table(frame) };
    for i in 0..ENTRIES {
        let entry = &table[i];
        if entry.is_unused() { continue; }
        let flags = entry.flags();
        if flags.contains(PageTableFlags::HUGE_PAGE) { continue; }
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) { continue; }
        let child = PhysFrame::containing_address(entry.addr());
        unsafe { destroy_lower_recursive(child, level - 1, fa); }
    }
    unsafe { fa.deallocate_frame(frame); }
}

/// Switch to the given page table by writing CR3.  Reading CR3 first
/// to avoid the (small) cost of an unnecessary TLB flush when the
/// requested table is already active.
///
/// # Safety
/// `cr3` must be the physical address of a valid PML4 whose kernel
/// mappings cover at least the running code path's working set.
#[inline]
pub unsafe fn switch_cr3(cr3: u64) {
    if cr3 == 0 { return; }
    let cur: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cur, options(nomem, nostack));
        if cur != cr3 {
            core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
        }
    }
}
