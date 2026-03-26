use x86_64::{
    structures::paging::{PageTable, OffsetPageTable, PhysFrame, Size4KiB, FrameAllocator},
    VirtAddr,
    PhysAddr,
};
use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub mod userspace;
pub mod user_allocator;

/// Saved physical memory offset for creating mappers at runtime
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Global frame allocator, available after boot initialization
static FRAME_ALLOCATOR: spin::Mutex<Option<BitmapFrameAllocator>> = spin::Mutex::new(None);

/// Store the frame allocator globally so it can be used at runtime
/// (e.g., for mapping guard pages, per-process page tables).
pub fn store_frame_allocator(alloc: BitmapFrameAllocator) {
    *FRAME_ALLOCATOR.lock() = Some(alloc);
}

/// Run a closure with exclusive access to the global frame allocator.
pub fn with_frame_allocator<F, R>(f: F) -> Option<R>
where F: FnOnce(&mut BitmapFrameAllocator) -> R {
    FRAME_ALLOCATOR.lock().as_mut().map(f)
}

/// Create a new OffsetPageTable mapper for the active page table.
///
/// # Safety
/// Caller must ensure no other code is modifying the page tables concurrently.
pub unsafe fn get_mapper() -> OffsetPageTable<'static> {
    let offset = VirtAddr::new(PHYS_MEM_OFFSET.load(Ordering::Relaxed));
    let level_4_table = unsafe { active_level_4_table(offset) };
    unsafe { OffsetPageTable::new(level_4_table, offset) }
}

/// Returns a mutable reference to the active level 4 table.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr)
    -> &'static mut PageTable
{
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

/// Initialize a new OffsetPageTable.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    // Save offset for runtime mapper creation
    PHYS_MEM_OFFSET.store(physical_memory_offset.as_u64(), Ordering::Relaxed);

    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

// =============================================================================
// Bitmap Frame Allocator
// =============================================================================
//
// Replaces the old BootInfoFrameAllocator (O(n²) bump allocator with no dealloc).
// Uses a static bitmap to track which 4 KiB physical frames are free/used.
//
// Supports up to MAX_PHYSICAL_MEMORY (4 GiB) of physical RAM.
// Bitmap size: 4 GiB / 4 KiB / 8 bits = 131072 bytes = 128 KiB in BSS.

/// Maximum supported physical memory (4 GiB)
const MAX_PHYSICAL_MEMORY: u64 = 4 * 1024 * 1024 * 1024;

/// Maximum number of 4 KiB frames in MAX_PHYSICAL_MEMORY
const MAX_FRAMES: usize = (MAX_PHYSICAL_MEMORY / 4096) as usize;

/// Size of the bitmap in bytes
const BITMAP_SIZE: usize = MAX_FRAMES / 8;

/// Static bitmap stored in BSS — 128 KiB.
/// Bit 0 = free, bit 1 = used (or not-usable).
/// We initialize all bits to 1 (used) and then clear bits for usable frames.
///
/// Uses UnsafeCell for interior mutability (Rust 2024 forbids `static mut` refs).
/// Safety: all access is single-threaded (interrupts disabled during init,
/// and the allocator is behind a spin::Mutex in production use).
struct BitmapStorage(core::cell::UnsafeCell<[u8; BITMAP_SIZE]>);

// Safety: The bitmap is only accessed through the BitmapFrameAllocator which
// is protected by external synchronization (single-threaded init, mutex in use).
unsafe impl Sync for BitmapStorage {}

static FRAME_BITMAP: BitmapStorage = BitmapStorage(core::cell::UnsafeCell::new([0xFF; BITMAP_SIZE]));

/// Total number of usable frames detected from memory map
static TOTAL_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// Number of frames currently allocated
static ALLOCATED_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// Bitmap-based physical frame allocator with O(1) amortized alloc/dealloc.
///
/// The bitmap is stored in a static array (BSS segment) so it's available
/// before the kernel heap is initialized.
pub struct BitmapFrameAllocator {
    /// Hint for next free frame search (avoids rescanning from 0)
    next_free: usize,
    /// Total number of frames the bitmap tracks
    num_frames: usize,
}

impl BitmapFrameAllocator {
    /// Create a BitmapFrameAllocator from the bootloader's memory map.
    ///
    /// # Safety
    /// The caller must guarantee that the passed memory map is valid and that
    /// all frames marked as `Usable` are truly unused.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        // Bitmap starts as all-1s (all frames marked used/reserved).
        // We clear bits for frames that are in usable memory regions.
        let mut usable_count = 0usize;

        for region in memory_map.iter() {
            if region.region_type != MemoryRegionType::Usable {
                continue;
            }

            let start_addr = region.range.start_addr();
            let end_addr = region.range.end_addr();

            // Iterate over all 4 KiB-aligned frames in this region
            let start_frame = (start_addr + 4095) / 4096; // round up
            let end_frame = end_addr / 4096; // round down

            for frame_idx in start_frame..end_frame {
                let idx = frame_idx as usize;
                if idx < MAX_FRAMES {
                    // Mark frame as free (clear bit)
                    let bitmap = unsafe { &mut *FRAME_BITMAP.0.get() };
                    bitmap[idx / 8] &= !(1 << (idx % 8));
                    usable_count += 1;
                }
            }
        }

        TOTAL_FRAMES.store(usable_count, Ordering::Relaxed);
        ALLOCATED_FRAMES.store(0, Ordering::Relaxed);

        BitmapFrameAllocator {
            next_free: 0,
            num_frames: MAX_FRAMES,
        }
    }

    /// Find the next free frame starting from `self.next_free`.
    /// Scans the bitmap byte-by-byte for efficiency (skips 8 frames at a time
    /// when an entire byte is 0xFF = all used).
    fn find_free_frame(&mut self) -> Option<usize> {
        let start_byte = self.next_free / 8;
        let bitmap = unsafe { &*FRAME_BITMAP.0.get() };

        // Scan from current position to end
        for byte_idx in start_byte..BITMAP_SIZE {
            let byte = bitmap[byte_idx];
            if byte == 0xFF {
                continue; // All 8 frames in this byte are used
            }

            // Found a byte with at least one free frame — find which bit
            for bit in 0..8u8 {
                if (byte & (1 << bit)) == 0 {
                    let frame_idx = byte_idx * 8 + bit as usize;
                    if frame_idx < self.num_frames {
                        return Some(frame_idx);
                    }
                }
            }
        }

        // Wrap around: scan from 0 to start position
        for byte_idx in 0..start_byte {
            let byte = bitmap[byte_idx];
            if byte == 0xFF {
                continue;
            }

            for bit in 0..8u8 {
                if (byte & (1 << bit)) == 0 {
                    let frame_idx = byte_idx * 8 + bit as usize;
                    if frame_idx < self.num_frames {
                        return Some(frame_idx);
                    }
                }
            }
        }

        None // No free frames
    }

    /// Mark a frame as used in the bitmap.
    fn mark_used(&self, frame_idx: usize) {
        let bitmap = unsafe { &mut *FRAME_BITMAP.0.get() };
        bitmap[frame_idx / 8] |= 1 << (frame_idx % 8);
    }

    /// Mark a frame as free in the bitmap.
    fn mark_free(&self, frame_idx: usize) {
        let bitmap = unsafe { &mut *FRAME_BITMAP.0.get() };
        bitmap[frame_idx / 8] &= !(1 << (frame_idx % 8));
    }

    /// Deallocate a physical frame, returning it to the free pool.
    ///
    /// # Safety
    /// The caller must ensure that the frame is no longer in use (not mapped
    /// in any page table, not referenced by any data structure).
    pub unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        let frame_idx = (frame.start_address().as_u64() / 4096) as usize;
        if frame_idx < self.num_frames {
            self.mark_free(frame_idx);
            // Update hint so next allocation can find this quickly
            if frame_idx < self.next_free {
                self.next_free = frame_idx;
            }
            ALLOCATED_FRAMES.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame_idx = self.find_free_frame()?;

        // Mark as used
        self.mark_used(frame_idx);
        // Advance hint past this frame
        self.next_free = frame_idx + 1;

        ALLOCATED_FRAMES.fetch_add(1, Ordering::Relaxed);

        let addr = PhysAddr::new((frame_idx as u64) * 4096);
        Some(PhysFrame::containing_address(addr))
    }
}

/// Get memory statistics: (total_usable_frames, allocated_frames, free_frames)
pub fn memory_stats() -> (usize, usize, usize) {
    let total = TOTAL_FRAMES.load(Ordering::Relaxed);
    let allocated = ALLOCATED_FRAMES.load(Ordering::Relaxed);
    (total, allocated, total.saturating_sub(allocated))
}

/// Get total usable physical memory in bytes
pub fn total_usable_memory() -> u64 {
    TOTAL_FRAMES.load(Ordering::Relaxed) as u64 * 4096
}

/// Get free physical memory in bytes
pub fn free_memory() -> u64 {
    let (total, allocated, _) = memory_stats();
    (total.saturating_sub(allocated)) as u64 * 4096
}
