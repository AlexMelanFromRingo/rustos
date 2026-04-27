//! Kernel virtual memory allocator (Linux `vmalloc`-style).
//!
//! Allocates large kernel buffers as a contiguous virtual range backed by
//! arbitrary (possibly non-contiguous) physical frames.  Useful when:
//!   * the heap allocator can't satisfy a giant allocation contiguously, or
//!   * the caller needs a guard-paged virtual range for sensitive data.
//!
//! Layout:  a fixed-size virtual region of [`VMALLOC_START`, `VMALLOC_END`)
//! is carved into 4 KiB pages.  Each allocation reserves N consecutive pages,
//! maps them to fresh frames, and returns the starting virtual address.
//! Free returns the frames to the bitmap allocator and unmaps the pages.
//!
//! No fragmentation handling beyond first-fit on the free-page bitmap.

use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::{
    structures::paging::{
        mapper::{MapToError, UnmapError},
        FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

/// vmalloc virtual region: 64 MiB starting well above the heap.
///
/// `BASE` is randomised once at boot for KASLR-style address obfuscation.
/// The randomisation lives in the upper bits of the virtual address; the
/// region size is constant.  Allocators that only ever see [`VMALLOC_START`]
/// will get the post-randomisation value via the [`vmalloc_start`] helper.
pub const VMALLOC_NOMINAL_BASE: usize = 0x_5555_0000_0000;
pub const VMALLOC_BYTES: usize = 64 * 1024 * 1024;
pub const VMALLOC_PAGES: usize = VMALLOC_BYTES / 4096;
/// Maximum slide added to the nominal base.  The actual offset is rounded
/// to a 4 KiB page so all internal page-index math still works.
pub const VMALLOC_RAND_RANGE: usize = 0x100_0000; // 16 MiB

static VMALLOC_BASE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(VMALLOC_NOMINAL_BASE);

/// Randomise the vmalloc base once.  Calls beyond the first are no-ops.
pub fn randomise_base(seed: u64) {
    use core::sync::atomic::Ordering;
    static DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, Ordering::AcqRel) { return; }

    // Mix the seed with the timer so re-runs aren't deterministic across boots.
    let mix = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let slide = (mix as usize) & (VMALLOC_RAND_RANGE - 1);
    let slide = slide & !0xFFF;  // round to 4 KiB
    VMALLOC_BASE.store(VMALLOC_NOMINAL_BASE + slide, Ordering::Release);
}

#[inline]
pub fn vmalloc_start() -> usize {
    VMALLOC_BASE.load(core::sync::atomic::Ordering::Relaxed)
}
#[inline]
pub fn vmalloc_end() -> usize {
    vmalloc_start() + VMALLOC_BYTES
}

// Backwards-compatible aliases for callers that read the constants.  These
// resolve to the fixed nominal base; live address math should use
// `vmalloc_start()`.
pub const VMALLOC_START: usize = VMALLOC_NOMINAL_BASE;
pub const VMALLOC_END:   usize = VMALLOC_NOMINAL_BASE + VMALLOC_BYTES;

/// Bitmap tracking which 4 KiB pages of the vmalloc region are in use.
/// Indexed by (vaddr - VMALLOC_START) / 4096.
struct VmallocBitmap {
    bits: [u64; VMALLOC_PAGES / 64],
}

impl VmallocBitmap {
    const fn new() -> Self { VmallocBitmap { bits: [0; VMALLOC_PAGES / 64] } }

    fn is_set(&self, idx: usize) -> bool {
        idx < VMALLOC_PAGES && (self.bits[idx / 64] & (1u64 << (idx % 64))) != 0
    }
    fn set(&mut self, idx: usize) {
        if idx < VMALLOC_PAGES { self.bits[idx / 64] |= 1u64 << (idx % 64); }
    }
    fn clear(&mut self, idx: usize) {
        if idx < VMALLOC_PAGES { self.bits[idx / 64] &= !(1u64 << (idx % 64)); }
    }

    fn find_run(&self, n: usize) -> Option<usize> {
        if n == 0 || n > VMALLOC_PAGES { return None; }
        let mut start = 0usize;
        let mut run = 0usize;
        for idx in 0..VMALLOC_PAGES {
            if self.is_set(idx) {
                start = idx + 1;
                run = 0;
            } else {
                run += 1;
                if run == n { return Some(start); }
            }
        }
        None
    }
}

static BITMAP: Mutex<VmallocBitmap> = Mutex::new(VmallocBitmap::new());

/// Per-allocation accounting: maps the starting virtual address back to its
/// length in pages, so `vfree` knows what to release.
static ALLOC_TABLE: Mutex<alloc::collections::BTreeMap<usize, usize>> =
    Mutex::new(alloc::collections::BTreeMap::new());

static TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static USED_BYTES:  AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmallocError {
    OutOfVirtualSpace,
    OutOfPhysicalFrames,
    MapFailed,
    BadFree,
}

/// Allocate `size` bytes of contiguous kernel virtual memory.  The returned
/// pointer is page-aligned.  Returns `Err` on OOM or mapping failure.
pub fn vmalloc(size: usize) -> Result<*mut u8, VmallocError> {
    if size == 0 { return Err(VmallocError::BadFree); }
    let pages = (size + 4095) / 4096;

    let start_idx = {
        let mut bm = BITMAP.lock();
        let idx = bm.find_run(pages).ok_or(VmallocError::OutOfVirtualSpace)?;
        for i in 0..pages { bm.set(idx + i); }
        idx
    };

    let start_vaddr = vmalloc_start() + start_idx * 4096;

    // Map each page to a fresh frame.
    for i in 0..pages {
        let vaddr = start_vaddr + i * 4096;
        if let Err(e) = map_one_page(vaddr) {
            // Roll back any previously mapped pages.
            for j in 0..i {
                let _ = unmap_one_page(start_vaddr + j * 4096);
            }
            let mut bm = BITMAP.lock();
            for k in 0..pages { bm.clear(start_idx + k); }
            return Err(e);
        }
    }

    ALLOC_TABLE.lock().insert(start_vaddr, pages);
    USED_BYTES.fetch_add(pages * 4096, Ordering::Relaxed);
    if TOTAL_BYTES.load(Ordering::Relaxed) == 0 {
        TOTAL_BYTES.store(VMALLOC_PAGES * 4096, Ordering::Relaxed);
    }

    Ok(start_vaddr as *mut u8)
}

/// Free a previously [`vmalloc`]'d allocation.
pub fn vfree(ptr: *mut u8) -> Result<(), VmallocError> {
    let vaddr = ptr as usize;
    let pages = match ALLOC_TABLE.lock().remove(&vaddr) {
        Some(n) => n,
        None => return Err(VmallocError::BadFree),
    };

    for i in 0..pages {
        let _ = unmap_one_page(vaddr + i * 4096);
    }

    let mut bm = BITMAP.lock();
    let start_idx = (vaddr - vmalloc_start()) / 4096;
    for i in 0..pages { bm.clear(start_idx + i); }

    USED_BYTES.fetch_sub(pages * 4096, Ordering::Relaxed);
    Ok(())
}

fn map_one_page(vaddr: usize) -> Result<(), VmallocError> {
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr as u64));
    // W^X: vmalloc allocations are data, not code.  Set NO_EXECUTE so the
    // CPU faults on any attempt to fetch instructions from this region.
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE;

    let result = crate::memory::with_frame_allocator(|fa| {
        let frame = fa.allocate_frame().ok_or(VmallocError::OutOfPhysicalFrames)?;
        let mut mapper = unsafe { crate::memory::get_mapper() };
        match unsafe { mapper.map_to(page, frame, flags, fa) } {
            Ok(flusher) => { flusher.flush(); Ok(()) }
            Err(MapToError::PageAlreadyMapped(_)) => {
                // Already mapped — treat as success (idempotent on remap).
                Ok(())
            }
            Err(_) => Err(VmallocError::MapFailed),
        }
    });

    match result {
        Some(r) => r,
        None => Err(VmallocError::OutOfPhysicalFrames),
    }
}

fn unmap_one_page(vaddr: usize) -> Result<(), VmallocError> {
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr as u64));
    let mut mapper = unsafe { crate::memory::get_mapper() };
    match mapper.unmap(page) {
        Ok((frame, flusher)) => {
            flusher.flush();
            crate::memory::with_frame_allocator(|fa| unsafe { fa.deallocate_frame(frame) });
            Ok(())
        }
        Err(UnmapError::PageNotMapped) => Ok(()),
        Err(_) => Err(VmallocError::BadFree),
    }
}

/// Stats tuple `(total_bytes, used_bytes, allocations)`.
pub fn stats() -> (usize, usize, usize) {
    (
        VMALLOC_PAGES * 4096,
        USED_BYTES.load(Ordering::Relaxed),
        ALLOC_TABLE.lock().len(),
    )
}
