//! Bootloader-info shim — single point of contact between the kernel and
//! whichever bootloader handed us off.
//!
//! Right now we boot via `bootloader 0.9.x` (BIOS only, kernel-as-ELF
//! load model).  Upgrading to `bootloader_api 0.11+` (UEFI + simpler
//! API) or moving to Limine should change exactly one file: this one.
//! Everything else in the kernel reads through `memory::phys_offset()`
//! (a cached static) and never imports the bootloader's types.
//!
//! ## Why this exists
//!
//! The earlier scattered approach had `boot_info.physical_memory_offset`
//! reads in `main.rs` and `tests/kernel_tests.rs`, plus an implicit
//! contract about the BootInfo struct shape.  Future migration meant
//! patching every call site.  Centralising here means a future
//! migration patches `KernelBootInfo::from_bootloader_v0_9()` and
//! adds a sibling `from_bootloader_v0_11()` (or `from_limine()`),
//! while the rest of the kernel stays untouched.
//!
//! ## What this carries
//!
//! Just the fields the kernel actually consumes — keeping the surface
//! small means each migration target can implement the conversion in
//! a few dozen lines.  Currently:
//!   * `phys_mem_offset` — virtual base where every physical address is
//!     mapped.  Bootloader 0.9 sets this via `map_physical_memory`;
//!     0.11+ exposes it as `BootInfo.physical_memory_offset:
//!     Optional<u64>` (None on UEFI without a request).
//!   * `memory_map_ptr` / `memory_map_len` — opaque pointer + length
//!     into the bootloader's region descriptor list.  We don't promise
//!     a layout; the per-bootloader code in this file converts to the
//!     kernel's `BitmapFrameAllocator` view.

use bootloader::BootInfo;

/// Kernel-side view of what the bootloader tells us at boot.  Owns no
/// references — bootloader-specific structs live behind raw pointers
/// the kernel doesn't dereference itself.
#[derive(Clone, Copy)]
pub struct KernelBootInfo {
    pub phys_mem_offset: u64,
    pub memory_map_ptr: *const (),
    pub memory_map_len: usize,
}

unsafe impl Send for KernelBootInfo {}
unsafe impl Sync for KernelBootInfo {}

impl KernelBootInfo {
    /// Convert from the bootloader 0.9 BootInfo struct.  This is the
    /// only place outside `main.rs` and the test harness that imports
    /// the bootloader crate's types.
    pub fn from_bootloader_v0_9(bi: &'static BootInfo) -> Self {
        Self {
            phys_mem_offset: bi.physical_memory_offset,
            memory_map_ptr: &bi.memory_map as *const _ as *const (),
            // bootloader 0.9 doesn't expose len directly — the iterator
            // is opaque.  Carry the &BootInfo through, callers reach
            // into bi.memory_map themselves.
            memory_map_len: 0,
        }
    }

    /// Reserved hook for the future bootloader 0.11 / bootloader_api
    /// migration.  Current build doesn't use this — it's compiled-out
    /// stub so the migration is a one-file diff:
    ///
    ///     pub fn from_bootloader_v0_11(bi: &'static bootloader_api::BootInfo) -> Self {
    ///         Self {
    ///             phys_mem_offset: bi.physical_memory_offset.into_option()
    ///                 .expect("kernel requires phys-memory map"),
    ///             memory_map_ptr: bi.memory_regions.as_ptr() as *const (),
    ///             memory_map_len: bi.memory_regions.len(),
    ///         }
    ///     }
    ///
    /// Reserved for parity with `from_limine()` once Limine path is wired.
    #[doc(hidden)]
    pub fn _v0_11_placeholder() {}

    /// Sanity bounds check: a real direct-physmap base is non-zero and
    /// well above any plausible user-space address.  Empirically
    /// `bootloader 0.9.34` chooses 0x0000_1800_0000_0000 by default
    /// (24 TiB into the lower half) — it doesn't bother going to the
    /// canonical kernel half because the kernel's own load address
    /// already lives there.  We tolerate either: any non-zero offset
    /// at or above 4 GiB is accepted; everything below is rejected as
    /// either uninitialised or a user-half pointer.
    pub fn is_phys_offset_sane(&self) -> bool {
        self.phys_mem_offset >= 0x1_0000_0000
    }
}
