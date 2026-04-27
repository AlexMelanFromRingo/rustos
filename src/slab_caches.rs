//! Standard kernel slab caches.
//!
//! Holds singleton [`SlabCache`]s used by other subsystems plus the kmalloc
//! family of generic-size caches.  Registered with [`SLAB_REGISTRY`] for
//! enumeration via /proc/slabinfo.

use crate::slab::{GenericSlabCache, SlabCache, SlabStats, KMALLOC_SIZES, SLAB_REGISTRY};
use spin::Once;

static SMALL_OBJ: Once<SlabCache<[u8; 16]>> = Once::new();
static MEDIUM_OBJ: Once<SlabCache<[u8; 64]>> = Once::new();
static LARGE_OBJ: Once<SlabCache<[u8; 256]>> = Once::new();
static KMALLOC_CACHES: Once<&'static [GenericSlabCache]> = Once::new();

fn small() -> &'static SlabCache<[u8; 16]> {
    SMALL_OBJ.call_once(|| SlabCache::new(64))
}
fn medium() -> &'static SlabCache<[u8; 64]> {
    MEDIUM_OBJ.call_once(|| SlabCache::new(32))
}
fn large() -> &'static SlabCache<[u8; 256]> {
    LARGE_OBJ.call_once(|| SlabCache::new(16))
}

fn kmalloc_caches() -> &'static [GenericSlabCache] {
    KMALLOC_CACHES.call_once(|| {
        let v: alloc::vec::Vec<GenericSlabCache> = KMALLOC_SIZES
            .iter()
            .map(|&sz| GenericSlabCache::new(sz, 8, slab_count_for(sz)))
            .collect();
        // Leak the Vec to obtain a 'static slice.  These caches live for the
        // entire kernel lifetime.
        alloc::boxed::Box::leak(v.into_boxed_slice())
    })
}

/// Heuristic: smaller objects pack more per slab.
fn slab_count_for(size: usize) -> usize {
    match size {
        0..=64 => 64,
        65..=256 => 32,
        257..=1024 => 16,
        _ => 8,
    }
}

/// Allocate `size` bytes from the kmalloc cache that fits.  Returns `None` if
/// `size` exceeds [`KMALLOC_SIZES`].  Caller must `kfree` with the same `size`.
pub fn kmalloc(size: usize) -> Option<core::ptr::NonNull<u8>> {
    let caches = kmalloc_caches();
    for (i, &sz) in KMALLOC_SIZES.iter().enumerate() {
        if size <= sz {
            return caches[i].alloc();
        }
    }
    None
}

/// Free a kmalloc'd pointer.  `size` must match the original request.
pub fn kfree(ptr: core::ptr::NonNull<u8>, size: usize) {
    let caches = kmalloc_caches();
    for (i, &sz) in KMALLOC_SIZES.iter().enumerate() {
        if size <= sz {
            caches[i].free(ptr);
            return;
        }
    }
}

fn small_stats()  -> SlabStats { small().stats() }
fn medium_stats() -> SlabStats { medium().stats() }
fn large_stats()  -> SlabStats { large().stats() }

macro_rules! km_stats_fn {
    ($name:ident, $idx:literal) => {
        fn $name() -> SlabStats { kmalloc_caches()[$idx].stats() }
    };
}
km_stats_fn!(km_stats_0, 0);
km_stats_fn!(km_stats_1, 1);
km_stats_fn!(km_stats_2, 2);
km_stats_fn!(km_stats_3, 3);
km_stats_fn!(km_stats_4, 4);
km_stats_fn!(km_stats_5, 5);
km_stats_fn!(km_stats_6, 6);
km_stats_fn!(km_stats_7, 7);
km_stats_fn!(km_stats_8, 8);

/// Register all caches with the global registry and run a self-test.
pub fn init() {
    // Force initialisation so the caches show up in slabinfo even before any
    // real workload exercises them.
    let _ = small();
    let _ = medium();
    let _ = large();
    let _ = kmalloc_caches();

    SLAB_REGISTRY.register("obj-16",      small_stats);
    SLAB_REGISTRY.register("obj-64",      medium_stats);
    SLAB_REGISTRY.register("obj-256",     large_stats);
    SLAB_REGISTRY.register("kmalloc-16",   km_stats_0);
    SLAB_REGISTRY.register("kmalloc-32",   km_stats_1);
    SLAB_REGISTRY.register("kmalloc-64",   km_stats_2);
    SLAB_REGISTRY.register("kmalloc-128",  km_stats_3);
    SLAB_REGISTRY.register("kmalloc-256",  km_stats_4);
    SLAB_REGISTRY.register("kmalloc-512",  km_stats_5);
    SLAB_REGISTRY.register("kmalloc-1024", km_stats_6);
    SLAB_REGISTRY.register("kmalloc-2048", km_stats_7);
    SLAB_REGISTRY.register("kmalloc-4096", km_stats_8);

    self_test();
}

/// Allocate-then-free a few objects to confirm the cache is wired up correctly.
fn self_test() {
    // Round-trip 8 small objects.
    let mut handles = alloc::vec::Vec::new();
    for _ in 0..8 {
        if let Some(p) = small().alloc() {
            handles.push(p);
        }
    }
    for p in handles {
        small().free(p);
    }

    // Round-trip a kmalloc-128 sized buffer.
    if let Some(p) = kmalloc(100) {
        unsafe {
            for i in 0..100u8 {
                p.as_ptr().add(i as usize).write(i);
            }
            for i in 0..100u8 {
                debug_assert_eq!(p.as_ptr().add(i as usize).read(), i);
            }
        }
        kfree(p, 100);
    }
}
