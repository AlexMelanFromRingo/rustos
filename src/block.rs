//! Generic block-device abstraction over virtio-blk / AHCI / NVMe.
//!
//! Each driver wraps itself in `BlockDevice` so the rest of the kernel
//! (Ext2, FAT32, partition table parsers) can read and write 512-byte
//! sectors without caring about the underlying transport.
//!
//! All operations are synchronous and polling-driven; concurrency is
//! the caller's responsibility (each backing driver already serialises
//! through its own Mutex).

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// One 512-byte logical sector.  All block devices in this kernel
/// expose 512-byte LBAs even when the underlying media uses larger
/// blocks (NVMe namespaces with 4 KiB LBA, etc.) — the device wrapper
/// handles the translation.
pub const SECTOR_BYTES: usize = 512;

pub trait BlockDevice: Send + Sync {
    /// Human-readable name (e.g. "vblk0", "sda", "nvme0n1").
    fn name(&self) -> &str;
    /// Total number of 512-byte LBAs.
    fn sectors(&self) -> u64;
    /// Read `count` sectors starting at `lba` into `buf`.  `buf.len()`
    /// must be >= count * 512.
    fn read(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str>;
    /// Write `count` sectors starting at `lba` from `buf`.
    fn write(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str>;

    /// Stat counters for /proc/diskstats.  Returns
    /// (reads, sectors_read, writes, sectors_written).
    fn stats(&self) -> (u64, u64, u64, u64) { (0, 0, 0, 0) }
}

/// Common stat counters every driver embeds in its wrapper.
#[derive(Default)]
pub struct Stats {
    pub reads: AtomicU64,
    pub sectors_read: AtomicU64,
    pub writes: AtomicU64,
    pub sectors_written: AtomicU64,
}

impl Stats {
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.reads.load(Ordering::Relaxed),
            self.sectors_read.load(Ordering::Relaxed),
            self.writes.load(Ordering::Relaxed),
            self.sectors_written.load(Ordering::Relaxed),
        )
    }
}

// -----------------------------------------------------------------------------
// virtio-blk wrapper
// -----------------------------------------------------------------------------

pub struct VirtioBlkDev {
    pub name: String,
    pub stats: Stats,
}

impl BlockDevice for VirtioBlkDev {
    fn name(&self) -> &str { &self.name }
    fn sectors(&self) -> u64 {
        let g = crate::drivers::virtio_blk::VIRTIO_BLK.lock();
        g.as_ref().map(|d| d.capacity_sectors).unwrap_or(0)
    }
    fn read(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let mut g = crate::drivers::virtio_blk::VIRTIO_BLK.lock();
        let d = g.as_mut().ok_or("virtio-blk: not available")?;
        for i in 0..count as u64 {
            let off = (i as usize) * 512;
            let mut sec = [0u8; 512];
            d.read_sector(lba + i, &mut sec)?;
            buf[off..off + 512].copy_from_slice(&sec);
        }
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats.sectors_read.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }
    fn write(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str> {
        let mut g = crate::drivers::virtio_blk::VIRTIO_BLK.lock();
        let d = g.as_mut().ok_or("virtio-blk: not available")?;
        for i in 0..count as u64 {
            let off = (i as usize) * 512;
            let mut sec = [0u8; 512];
            sec.copy_from_slice(&buf[off..off + 512]);
            d.write_sector(lba + i, &sec)?;
        }
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats.sectors_written.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }
    fn stats(&self) -> (u64, u64, u64, u64) { self.stats.snapshot() }
}

// -----------------------------------------------------------------------------
// AHCI wrapper — port 0 only for now
// -----------------------------------------------------------------------------

pub struct AhciDev {
    pub name: String,
    pub port_index: usize,
    pub stats: Stats,
}

impl BlockDevice for AhciDev {
    fn name(&self) -> &str { &self.name }
    fn sectors(&self) -> u64 {
        let g = crate::drivers::ahci::AHCI.lock();
        g.as_ref().and_then(|a| a.ports.get(self.port_index)).map(|p| p.sectors).unwrap_or(0)
    }
    fn read(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let g = crate::drivers::ahci::AHCI.lock();
        let p = g.as_ref().and_then(|a| a.ports.get(self.port_index))
            .ok_or("ahci: port not available")?;
        if count > 65535 { return Err("ahci: count too large"); }
        p.read_sectors(lba, count as u16, buf)?;
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats.sectors_read.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }
    fn write(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str> {
        let g = crate::drivers::ahci::AHCI.lock();
        let p = g.as_ref().and_then(|a| a.ports.get(self.port_index))
            .ok_or("ahci: port not available")?;
        if count > 65535 { return Err("ahci: count too large"); }
        p.write_sectors(lba, count as u16, buf)?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats.sectors_written.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }
    fn stats(&self) -> (u64, u64, u64, u64) { self.stats.snapshot() }
}

// -----------------------------------------------------------------------------
// NVMe wrapper — namespace 1, 4 KiB ⇆ 512 B byte translation
// -----------------------------------------------------------------------------

pub struct NvmeDev {
    pub name: String,
    pub stats: Stats,
}

impl BlockDevice for NvmeDev {
    fn name(&self) -> &str { &self.name }
    fn sectors(&self) -> u64 {
        let g = crate::drivers::nvme::NVME.lock();
        g.as_ref().map(|n| {
            let block_bytes = 1u64 << n.lba_shift;
            n.sectors * (block_bytes / 512)
        }).unwrap_or(0)
    }
    fn read(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let mut g = crate::drivers::nvme::NVME.lock();
        let n = g.as_mut().ok_or("nvme: not available")?;
        let block_bytes = 1usize << n.lba_shift;
        if block_bytes == 512 {
            n.read(lba, count as u16, buf)?;
        } else {
            // Aggregate 512-byte sectors into N-byte NVMe blocks.
            let bs = block_bytes as u64;
            let sectors_per_block = bs / 512;
            // Find the starting NVMe block + offset.
            let start_block = lba / sectors_per_block;
            let start_off = (lba % sectors_per_block) * 512;
            let total_bytes = (count as usize) * 512;
            let blocks_needed =
                ((start_off as usize + total_bytes + block_bytes - 1) / block_bytes) as u64;
            let mut tmp = alloc::vec![0u8; (blocks_needed as usize) * block_bytes];
            n.read(start_block, blocks_needed as u16, &mut tmp)?;
            let src_start = start_off as usize;
            buf[..total_bytes].copy_from_slice(&tmp[src_start..src_start + total_bytes]);
        }
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats.sectors_read.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }
    fn write(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str> {
        let mut g = crate::drivers::nvme::NVME.lock();
        let n = g.as_mut().ok_or("nvme: not available")?;
        let block_bytes = 1usize << n.lba_shift;
        if block_bytes == 512 {
            n.write(lba, count as u16, buf)?;
        } else {
            // RMW for sub-block writes — bring in the whole NVMe blocks
            // first, splice the payload, write back.
            let bs = block_bytes as u64;
            let sectors_per_block = bs / 512;
            let start_block = lba / sectors_per_block;
            let start_off = (lba % sectors_per_block) * 512;
            let total_bytes = (count as usize) * 512;
            let blocks_needed =
                ((start_off as usize + total_bytes + block_bytes - 1) / block_bytes) as u64;
            let mut tmp = alloc::vec![0u8; (blocks_needed as usize) * block_bytes];
            n.read(start_block, blocks_needed as u16, &mut tmp)?;
            let dst_start = start_off as usize;
            tmp[dst_start..dst_start + total_bytes].copy_from_slice(&buf[..total_bytes]);
            n.write(start_block, blocks_needed as u16, &tmp)?;
        }
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats.sectors_written.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }
    fn stats(&self) -> (u64, u64, u64, u64) { self.stats.snapshot() }
}

// -----------------------------------------------------------------------------
// Global registry of block devices
// -----------------------------------------------------------------------------

pub static DEVICES: Mutex<Vec<Box<dyn BlockDevice>>> = Mutex::new(Vec::new());

pub fn register(dev: Box<dyn BlockDevice>) {
    DEVICES.lock().push(dev);
}

/// Populate `DEVICES` from whichever drivers came up.  Called once
/// from main after all driver inits.
pub fn discover() {
    if crate::drivers::virtio_blk::is_available() {
        register(Box::new(VirtioBlkDev {
            name: "vblk0".to_string(),
            stats: Stats::default(),
        }));
    }
    if crate::drivers::ahci::is_available() {
        let g = crate::drivers::ahci::AHCI.lock();
        if let Some(a) = g.as_ref() {
            for i in 0..a.ports.len() {
                drop(&a);
                register(Box::new(AhciDev {
                    name: alloc::format!("sd{}", char::from(b'a' + i as u8)),
                    port_index: i,
                    stats: Stats::default(),
                }));
            }
        }
    }
    if crate::drivers::nvme::is_available() {
        register(Box::new(NvmeDev {
            name: "nvme0n1".to_string(),
            stats: Stats::default(),
        }));
    }
}

/// Snapshot of (name, sectors, stats) for every registered device.
pub fn snapshot() -> Vec<(String, u64, (u64, u64, u64, u64))> {
    let g = DEVICES.lock();
    g.iter().map(|d| (d.name().to_string(), d.sectors(), d.stats())).collect()
}
