//! Slab allocator for fixed-size kernel objects.
//!
//! Implements an object cache in the spirit of Jeff Bonwick's USENIX'94
//! "The Slab Allocator: An Object-Caching Kernel Memory Allocator". Each
//! [`SlabCache`] manages objects of one size: it carves slab-sized chunks of
//! memory (obtained from the global allocator) into fixed-size objects, threads
//! the free objects through an embedded free list, and hands them out in O(1)
//! time without traversing any data structures.
//!
//! This allocator complements — but does not replace — the global heap.  It is
//! intended for kernel objects that are allocated and freed often (process
//! control blocks, file descriptor entries, dentry-cache nodes, etc.) where
//! the cost of going through the global allocator is measurable.
//!
//! Reference: <https://www.usenix.org/legacy/publications/library/proceedings/bos94/full_papers/bonwick.a>

use core::marker::PhantomData;
use core::mem;
use core::ptr::NonNull;
use alloc::alloc::{alloc, dealloc, Layout};
use alloc::vec::Vec;
use spin::Mutex;

/// A free-object record, threaded through the unused space of a free slot.
#[repr(C)]
struct FreeNode {
    next: Option<NonNull<FreeNode>>,
}

/// Backing slab: a single chunk of memory carved into `obj_count` objects.
struct Slab {
    /// Raw allocation pointer (must be freed with the matching layout).
    base: NonNull<u8>,
    /// Backing layout, kept so we can free the slab.
    layout: Layout,
    /// How many objects from this slab are currently in use.
    used: usize,
    /// Total number of objects this slab can hold.
    capacity: usize,
}

impl Slab {
    /// Allocate a new slab capable of holding `capacity` objects of the given
    /// effective object size and alignment.  Returns the slab plus an iterator
    /// over freshly initialised free-object pointers.
    fn new(obj_stride: usize, obj_align: usize, capacity: usize) -> Option<Self> {
        let bytes = obj_stride.checked_mul(capacity)?;
        let layout = Layout::from_size_align(bytes, obj_align).ok()?;
        let raw = unsafe { alloc(layout) };
        let base = NonNull::new(raw)?;
        Some(Slab { base, layout, used: 0, capacity })
    }

    /// Iterate every slot's start address.
    fn slot_addrs(&self, obj_stride: usize) -> SlabSlotIter {
        SlabSlotIter {
            base: self.base.as_ptr(),
            stride: obj_stride,
            remaining: self.capacity,
        }
    }

    /// True if the given pointer belongs to this slab's allocation range.
    fn contains(&self, ptr: *mut u8) -> bool {
        let start = self.base.as_ptr() as usize;
        let end = start + self.layout.size();
        let p = ptr as usize;
        p >= start && p < end
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        // Caller must have already removed all free-list entries pointing into
        // this slab; we just return the backing memory.
        unsafe { dealloc(self.base.as_ptr(), self.layout) }
    }
}

struct SlabSlotIter {
    base: *mut u8,
    stride: usize,
    remaining: usize,
}

impl Iterator for SlabSlotIter {
    type Item = *mut u8;
    fn next(&mut self) -> Option<*mut u8> {
        if self.remaining == 0 {
            None
        } else {
            let p = self.base;
            self.base = unsafe { self.base.add(self.stride) };
            self.remaining -= 1;
            Some(p)
        }
    }
}

/// Statistics for a [`SlabCache`].  Useful for `/proc/slabinfo`-style reporting.
#[derive(Debug, Clone, Copy, Default)]
pub struct SlabStats {
    pub obj_size: usize,
    pub objs_per_slab: usize,
    pub total_slabs: usize,
    pub total_objs: usize,
    pub free_objs: usize,
    pub used_objs: usize,
    pub allocs: u64,
    pub frees: u64,
    pub grew: u64,
}

struct SlabCacheInner {
    /// Effective per-object stride (size rounded up to alignment).
    obj_stride: usize,
    obj_align: usize,
    /// Objects per slab.
    objs_per_slab: usize,
    free_list: Option<NonNull<FreeNode>>,
    slabs: Vec<Slab>,
    allocs: u64,
    frees: u64,
    grew: u64,
}

unsafe impl Send for SlabCacheInner {}

impl SlabCacheInner {
    fn new(obj_size: usize, obj_align: usize, objs_per_slab: usize) -> Self {
        let raw_align = obj_align.max(mem::align_of::<FreeNode>()).next_power_of_two();
        let raw_size = obj_size.max(mem::size_of::<FreeNode>());
        let stride = (raw_size + raw_align - 1) & !(raw_align - 1);
        SlabCacheInner {
            obj_stride: stride,
            obj_align: raw_align,
            objs_per_slab: objs_per_slab.max(1),
            free_list: None,
            slabs: Vec::new(),
            allocs: 0,
            frees: 0,
            grew: 0,
        }
    }

    /// Add a new slab and push its slots onto the free list.
    fn grow(&mut self) -> bool {
        let mut slab = match Slab::new(self.obj_stride, self.obj_align, self.objs_per_slab) {
            Some(s) => s,
            None => return false,
        };
        let slots: Vec<*mut u8> = slab.slot_addrs(self.obj_stride).collect();
        slab.used = 0;
        self.slabs.push(slab);

        for slot in slots.into_iter() {
            let node_ptr = slot as *mut FreeNode;
            unsafe {
                node_ptr.write(FreeNode { next: self.free_list });
                self.free_list = Some(NonNull::new_unchecked(node_ptr));
            }
        }
        self.grew += 1;
        true
    }

    fn alloc(&mut self) -> Option<NonNull<u8>> {
        if self.free_list.is_none() && !self.grow() {
            return None;
        }
        let node = self.free_list?;
        unsafe {
            self.free_list = node.as_ref().next;
            // Locate the slab that contains this object and bump its `used`.
            let raw = node.as_ptr() as *mut u8;
            for slab in self.slabs.iter_mut() {
                if slab.contains(raw) {
                    slab.used += 1;
                    break;
                }
            }
            self.allocs += 1;
            Some(NonNull::new_unchecked(raw))
        }
    }

    fn free(&mut self, ptr: NonNull<u8>) {
        unsafe {
            let raw = ptr.as_ptr();
            for slab in self.slabs.iter_mut() {
                if slab.contains(raw) {
                    slab.used = slab.used.saturating_sub(1);
                    break;
                }
            }
            let node_ptr = raw as *mut FreeNode;
            node_ptr.write(FreeNode { next: self.free_list });
            self.free_list = Some(NonNull::new_unchecked(node_ptr));
        }
        self.frees += 1;
    }

    fn stats(&self) -> SlabStats {
        let total_objs = self.slabs.iter().map(|s| s.capacity).sum();
        let used_objs: usize = self.slabs.iter().map(|s| s.used).sum();
        SlabStats {
            obj_size: self.obj_stride,
            objs_per_slab: self.objs_per_slab,
            total_slabs: self.slabs.len(),
            total_objs,
            used_objs,
            free_objs: total_objs - used_objs,
            allocs: self.allocs,
            frees: self.frees,
            grew: self.grew,
        }
    }
}

/// Thread-safe wrapper over a [`SlabCacheInner`].
pub struct SlabCache<T> {
    inner: Mutex<SlabCacheInner>,
    _phantom: PhantomData<T>,
}

impl<T> SlabCache<T> {
    /// Create a cache for type `T` with the given number of objects per slab.
    pub fn new(objs_per_slab: usize) -> Self {
        SlabCache {
            inner: Mutex::new(SlabCacheInner::new(
                mem::size_of::<T>(),
                mem::align_of::<T>(),
                objs_per_slab,
            )),
            _phantom: PhantomData,
        }
    }

    /// Allocate one object slot.  Returns `None` if the backing allocator fails.
    /// The returned pointer is uninitialised — callers must `.write()` before
    /// reading.
    pub fn alloc(&self) -> Option<NonNull<T>> {
        self.inner.lock().alloc().map(|p| p.cast())
    }

    /// Allocate and initialise an object via `init`.
    pub fn allocate(&self, init: T) -> Option<NonNull<T>> {
        let p = self.alloc()?;
        unsafe { core::ptr::write(p.as_ptr(), init) };
        Some(p)
    }

    /// Free an object back to the cache.  The object's `Drop` is NOT run.
    /// Use [`Self::free_with_drop`] when `T` has a non-trivial destructor.
    pub fn free(&self, ptr: NonNull<T>) {
        self.inner.lock().free(ptr.cast());
    }

    /// Free an object, running its `Drop` first.
    pub fn free_with_drop(&self, ptr: NonNull<T>) {
        unsafe { core::ptr::drop_in_place(ptr.as_ptr()) };
        self.free(ptr);
    }

    pub fn stats(&self) -> SlabStats {
        self.inner.lock().stats()
    }
}

/// Generic cache keyed by object size, similar to Linux's `kmalloc-NN` slabs.
/// Used for ad-hoc allocations that don't have a typed `SlabCache<T>`.
pub struct GenericSlabCache {
    inner: Mutex<SlabCacheInner>,
}

impl GenericSlabCache {
    pub fn new(obj_size: usize, obj_align: usize, objs_per_slab: usize) -> Self {
        GenericSlabCache {
            inner: Mutex::new(SlabCacheInner::new(obj_size, obj_align, objs_per_slab)),
        }
    }

    pub fn alloc(&self) -> Option<NonNull<u8>> {
        self.inner.lock().alloc()
    }

    pub fn free(&self, ptr: NonNull<u8>) {
        self.inner.lock().free(ptr);
    }

    pub fn stats(&self) -> SlabStats {
        self.inner.lock().stats()
    }
}

/// Standard kmalloc-style sizes (16, 32, 64, ..., 4096 bytes).
pub const KMALLOC_SIZES: &[usize] = &[16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

/// Lightweight registry for /proc/slabinfo-style enumeration.
pub struct SlabRegistry {
    entries: Mutex<Vec<(&'static str, fn() -> SlabStats)>>,
}

impl SlabRegistry {
    pub const fn new() -> Self {
        SlabRegistry { entries: Mutex::new(Vec::new()) }
    }

    pub fn register(&self, name: &'static str, stats_fn: fn() -> SlabStats) {
        self.entries.lock().push((name, stats_fn));
    }

    pub fn snapshot(&self) -> Vec<(&'static str, SlabStats)> {
        self.entries.lock().iter().map(|(n, f)| (*n, f())).collect()
    }
}

pub static SLAB_REGISTRY: SlabRegistry = SlabRegistry::new();
