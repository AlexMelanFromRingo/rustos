//! virtio-blk driver — legacy I/O port layout (virtio 1.0 §4.1.5).
//!
//! Discovers a transitional virtio-blk device (vendor 0x1AF4,
//! device 0x1001), runs the standard handshake, sets up the request
//! queue using the size the device reports, and exposes synchronous
//! read_sector / write_sector.
//!
//! On-the-wire request layout (virtio 1.0 §5.2):
//!
//!     struct virtio_blk_req {
//!         le32 type;       // 0=READ, 1=WRITE, 4=FLUSH
//!         le32 reserved;
//!         le64 sector;
//!         u8   data[len];  // device-write for READ, device-read for WRITE
//!         u8   status;     // 0=OK 1=IOERR 2=UNSUPP — device-write
//!     };
//!
//! Each request uses three descriptors chained with VIRTQ_DESC_F_NEXT:
//! header (RO), data buffer (R/W), status byte (WO).
//!
//! ## Queue layout (virtio 1.0 §2.6.5 legacy)
//!
//!     offset 0                  : descriptor table  (16 * qsize bytes)
//!     offset 16*qsize           : avail ring        (6 + 2*qsize bytes)
//!     offset align_up(...,4096) : used ring         (6 + 8*qsize bytes)
//!
//! The queue size is read-only on legacy interfaces; the driver MUST
//! use whatever the device reports, so all offsets are computed at
//! init time.
//!
//! ## DMA discipline
//!
//! Legacy virtio expects (a) the queue rings live in one physically
//! contiguous region whose page frame number is written to QUEUE_ADDR,
//! and (b) every descriptor `addr` field is a *physical* address.
//! Both come from `crate::memory::alloc_dma_contig`, which carves
//! contiguous frames out of the bitmap allocator and returns the
//! matching direct-map virtual pointer.

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::drivers::pci::{find, probe_bar, enable_device};
use crate::memory::alloc_dma_contig;

const VIRTIO_PCI_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE: u16 = 0x1001;

const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES:  u16 = 0x04;
const REG_QUEUE_ADDR:      u16 = 0x08;
const REG_QUEUE_SIZE:      u16 = 0x0C;
const REG_QUEUE_SELECT:    u16 = 0x0E;
const REG_QUEUE_NOTIFY:    u16 = 0x10;
const REG_DEVICE_STATUS:   u16 = 0x12;
const REG_DEVICE_CONFIG:   u16 = 0x14; // for blk: capacity[u64] at off 0

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED:      u8 = 0x80;

const VIRTIO_BLK_T_IN:  u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

const VIRTQ_DESC_F_NEXT:  u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Bit in the avail-ring `flags` field that asks the device to skip
/// raising an interrupt for completions on this queue (virtio 1.0
/// §2.6.7 Used Buffer Notification Suppression).  We poll the used
/// ring synchronously, and we don't have an IDT vector wired up for
/// the device's PCI interrupt, so set this so a stray IRQ from the
/// device never trips a #NP → double-fault cascade.
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

const QUEUE_ALIGN: usize = 4096;
const DESC_SIZE: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc { addr: u64, len: u32, flags: u16, next: u16 }

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqUsedElem { id: u32, len: u32 }

fn align_up_4k(x: usize) -> usize { (x + (QUEUE_ALIGN - 1)) & !(QUEUE_ALIGN - 1) }

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct BlkReqHeader {
    type_:    u32,
    reserved: u32,
    sector:   u64,
}

#[repr(C)]
pub struct VirtioBlk {
    bar0: u16,
    queue_virt: usize,           // direct-map virtual base of the queue region
    queue_phys: u64,             // physical base — fed to QUEUE_ADDR
    queue_size: usize,           // entries reported by device
    queue_total_size: usize,     // bytes allocated for queue (for cleanup / asserts)
    avail_offset: usize,
    used_offset: usize,
    last_used_idx: u16,
    pub capacity_sectors: u64,
}

// SAFETY: device is single-instance, only ever touched while the
// VIRTIO_BLK Mutex is held; the raw pointer below is stable for the
// lifetime of the kernel.
unsafe impl Send for VirtioBlk {}

impl VirtioBlk {
    pub fn init() -> Result<Self, &'static str> {
        let dev = find(VIRTIO_PCI_VENDOR, VIRTIO_BLK_DEVICE)
            .ok_or("virtio-blk device not found")?;
        enable_device(dev.addr);
        let (base, _size, is_io) = probe_bar(dev.addr, 0).ok_or("BAR0 missing")?;
        if !is_io { return Err("legacy virtio-blk requires I/O BAR0"); }
        let bar0 = base as u16;

        unsafe {
            io_write_u8(bar0 + REG_DEVICE_STATUS, 0);
            io_write_u8(bar0 + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
            io_write_u8(bar0 + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

            // Negotiate no features — basic sector I/O is unconditional.
            let _device_feats = io_read_u32(bar0 + REG_DEVICE_FEATURES);
            io_write_u32(bar0 + REG_GUEST_FEATURES, 0);
            io_write_u8(bar0 + REG_DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
            let st = io_read_u8(bar0 + REG_DEVICE_STATUS);
            if st & STATUS_FEATURES_OK == 0 {
                io_write_u8(bar0 + REG_DEVICE_STATUS, STATUS_FAILED);
                return Err("device refused FEATURES_OK");
            }

            // Capacity (in 512-byte sectors) at config offset 0..8.
            let mut capacity: u64 = 0;
            for i in 0..8 {
                capacity |= (io_read_u8(bar0 + REG_DEVICE_CONFIG + i as u16) as u64) << (i * 8);
            }

            // Set up queue 0.
            io_write_u16(bar0 + REG_QUEUE_SELECT, 0);
            let qsize = io_read_u16(bar0 + REG_QUEUE_SIZE) as usize;
            if qsize == 0 { return Err("queue 0 size 0"); }
            if !qsize.is_power_of_two() || qsize > 1024 {
                return Err("device reported unreasonable queue size");
            }

            // Compute legacy layout for the actual qsize.
            let desc_size = DESC_SIZE * qsize;
            let avail_size = 6 + 2 * qsize;
            let avail_offset = desc_size;
            let used_offset = align_up_4k(avail_offset + avail_size);
            let used_size = 6 + 8 * qsize;
            let total = align_up_4k(used_offset + used_size);

            let (queue_virt, queue_phys) = alloc_dma_contig(total)
                .ok_or("DMA: failed to allocate queue region")?;
            let queue_virt = queue_virt as usize;

            // QUEUE_ADDR holds the page frame number (PFN), not byte address.
            io_write_u32(bar0 + REG_QUEUE_ADDR, (queue_phys >> 12) as u32);

            // Pre-set NO_INTERRUPT in avail.flags so the device never
            // fires an IRQ on this queue (we have no IDT entry for it).
            let avail_flags_ptr = (queue_virt + avail_offset) as *mut u16;
            core::ptr::write_volatile(avail_flags_ptr, VIRTQ_AVAIL_F_NO_INTERRUPT);

            io_write_u8(bar0 + REG_DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);

            crate::klog_info!(
                "virtio-blk: I/O {:#x}, qsize {}, queue phys {:#x} ({} bytes), capacity {} sectors ({} MiB)",
                bar0, qsize, queue_phys, total,
                capacity, (capacity * 512) / (1024 * 1024));

            Ok(VirtioBlk {
                bar0,
                queue_virt,
                queue_phys,
                queue_size: qsize,
                queue_total_size: total,
                avail_offset,
                used_offset,
                last_used_idx: 0,
                capacity_sectors: capacity,
            })
        }
    }

    fn desc_ptr(&self, i: usize) -> *mut VirtqDesc {
        debug_assert!(i < self.queue_size);
        (self.queue_virt + i * DESC_SIZE) as *mut VirtqDesc
    }
    fn avail_flags_ptr(&self) -> *mut u16 {
        (self.queue_virt + self.avail_offset) as *mut u16
    }
    fn avail_idx_ptr(&self) -> *mut u16 {
        (self.queue_virt + self.avail_offset + 2) as *mut u16
    }
    fn avail_ring_ptr(&self, i: usize) -> *mut u16 {
        (self.queue_virt + self.avail_offset + 4 + i * 2) as *mut u16
    }
    fn used_idx_ptr(&self) -> *const u16 {
        (self.queue_virt + self.used_offset + 2) as *const u16
    }

    /// Submit a 3-descriptor request and wait for completion.
    fn submit_and_wait(
        &mut self,
        hdr_phys: u64,
        data_phys: u64,
        data_write: bool,
        status_phys: u64,
    ) -> Result<(), &'static str> {
        unsafe {
            // Descriptor 0: header (device-readable)
            *self.desc_ptr(0) = VirtqDesc {
                addr: hdr_phys,
                len: core::mem::size_of::<BlkReqHeader>() as u32,
                flags: VIRTQ_DESC_F_NEXT,
                next: 1,
            };
            // Descriptor 1: data (read or write depending on direction)
            *self.desc_ptr(1) = VirtqDesc {
                addr: data_phys,
                len: 512,
                flags: VIRTQ_DESC_F_NEXT | if data_write { VIRTQ_DESC_F_WRITE } else { 0 },
                next: 2,
            };
            // Descriptor 2: status byte (device-writable)
            *self.desc_ptr(2) = VirtqDesc {
                addr: status_phys,
                len: 1,
                flags: VIRTQ_DESC_F_WRITE,
                next: 0,
            };

            // Suppress device-side interrupts before publishing the new
            // descriptor — we poll the used ring directly.
            core::ptr::write_volatile(self.avail_flags_ptr(), VIRTQ_AVAIL_F_NO_INTERRUPT);

            // Set the avail ring slot to point at descriptor head 0.
            let cur_avail = core::ptr::read_volatile(self.avail_idx_ptr());
            let slot = (cur_avail as usize) & (self.queue_size - 1);
            core::ptr::write_volatile(self.avail_ring_ptr(slot), 0);
            // Release fence so the device sees descriptor + ring updates
            // before it sees the bumped avail.idx.
            core::sync::atomic::fence(Ordering::Release);
            core::ptr::write_volatile(self.avail_idx_ptr(), cur_avail.wrapping_add(1));
            io_write_u16(self.bar0 + REG_QUEUE_NOTIFY, 0);

            // Pure-iteration spin.  QEMU's virtio-blk responds within
            // microseconds, well under 100 K iterations.  We bound the
            // wait at 50 M to avoid hanging if the device never replies.
            for _ in 0..50_000_000u64 {
                let used_idx = core::ptr::read_volatile(self.used_idx_ptr());
                if used_idx != self.last_used_idx {
                    self.last_used_idx = used_idx;
                    return Ok(());
                }
                core::hint::spin_loop();
            }
            Err("virtio-blk request timed out")
        }
    }

    /// One DMA-allocated scratch frame holds header at +0, data at +0x100,
    /// and status at +0x800.  All inside one physically-contiguous page
    /// so virt_to_phys is unnecessary and there's no risk of the device
    /// DMA-writing into the kernel heap if heap → phys translation is
    /// stale.
    const HDR_OFF: usize = 0x000;     // 16 bytes
    const DATA_OFF: usize = 0x100;    // 512 bytes ends at 0x300
    const STATUS_OFF: usize = 0x800;  // 1 byte

    /// Read one 512-byte sector synchronously.
    pub fn read_sector(&mut self, lba: u64, out: &mut [u8; 512]) -> Result<(), &'static str> {
        let (scratch_virt, scratch_phys) = alloc_dma_contig(4096)
            .ok_or("DMA: scratch buffer allocation failed")?;
        let res = self.do_request(scratch_virt, scratch_phys, lba, VIRTIO_BLK_T_IN, true);
        if res.is_ok() {
            unsafe {
                let src = core::slice::from_raw_parts(
                    scratch_virt.add(Self::DATA_OFF), 512);
                out.copy_from_slice(src);
            }
        }
        let status = unsafe { *scratch_virt.add(Self::STATUS_OFF) };
        unsafe { crate::memory::dealloc_dma_contig(scratch_phys, 4096); }
        res?;
        if status != 0 { return Err("virtio-blk: device returned IOERR on read"); }
        Ok(())
    }

    /// Write one 512-byte sector synchronously.
    pub fn write_sector(&mut self, lba: u64, data: &[u8; 512]) -> Result<(), &'static str> {
        let (scratch_virt, scratch_phys) = alloc_dma_contig(4096)
            .ok_or("DMA: scratch buffer allocation failed")?;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(
                scratch_virt.add(Self::DATA_OFF), 512);
            dst.copy_from_slice(data);
        }
        let res = self.do_request(scratch_virt, scratch_phys, lba, VIRTIO_BLK_T_OUT, false);
        let status = unsafe { *scratch_virt.add(Self::STATUS_OFF) };
        unsafe { crate::memory::dealloc_dma_contig(scratch_phys, 4096); }
        res?;
        if status != 0 { return Err("virtio-blk: device returned IOERR on write"); }
        Ok(())
    }

    fn do_request(
        &mut self,
        scratch_virt: *mut u8,
        scratch_phys: u64,
        lba: u64,
        op_type: u32,
        data_device_writes: bool,
    ) -> Result<(), &'static str> {
        unsafe {
            // Lay out header in scratch.
            let hdr_ptr = scratch_virt.add(Self::HDR_OFF) as *mut BlkReqHeader;
            *hdr_ptr = BlkReqHeader { type_: op_type, reserved: 0, sector: lba };
            // Status byte zero before submission.
            *scratch_virt.add(Self::STATUS_OFF) = 0;
        }
        let hdr_phys = scratch_phys + Self::HDR_OFF as u64;
        let data_phys = scratch_phys + Self::DATA_OFF as u64;
        let status_phys = scratch_phys + Self::STATUS_OFF as u64;
        self.submit_and_wait(hdr_phys, data_phys, data_device_writes, status_phys)
    }
}

unsafe fn io_read_u8(p: u16)  -> u8  { let mut port: Port<u8>  = Port::new(p); unsafe { port.read() } }
unsafe fn io_read_u16(p: u16) -> u16 { let mut port: Port<u16> = Port::new(p); unsafe { port.read() } }
unsafe fn io_read_u32(p: u16) -> u32 { let mut port: Port<u32> = Port::new(p); unsafe { port.read() } }
unsafe fn io_write_u8(p: u16, v: u8)   { let mut port: Port<u8>  = Port::new(p); unsafe { port.write(v) } }
unsafe fn io_write_u16(p: u16, v: u16) { let mut port: Port<u16> = Port::new(p); unsafe { port.write(v) } }
unsafe fn io_write_u32(p: u16, v: u32) { let mut port: Port<u32> = Port::new(p); unsafe { port.write(v) } }

pub static VIRTIO_BLK: Mutex<Option<VirtioBlk>> = Mutex::new(None);
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    match VirtioBlk::init() {
        Ok(d) => {
            *VIRTIO_BLK.lock() = Some(d);
            AVAILABLE.store(true, Ordering::Release);
        }
        Err(e) => crate::klog_warn!("virtio-blk: not initialised: {}", e),
    }
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }

/// Run a one-shot self-test against LBA 0: write a marker pattern,
/// read it back, compare, and print a verdict on the console.
///
/// Buffers live on the heap so a deep call stack at the call site
/// does not have to absorb 512-byte sector arrays.
pub fn self_test() {
    use alloc::boxed::Box;
    let mut g = VIRTIO_BLK.lock();
    let blk = match g.as_mut() {
        Some(b) => b,
        None => { crate::println!("virtio-blk: device gone before self-test"); return; }
    };
    crate::println!("virtio-blk: capacity {} sectors", blk.capacity_sectors);
    let marker = b"RUSTOS-VIRTIO-BLK-SELFTEST-OK";
    let mut sector: Box<[u8; 512]> = Box::new([0u8; 512]);
    sector[..marker.len()].copy_from_slice(marker);
    if let Err(e) = blk.write_sector(0, &sector) {
        crate::println!("virtio-blk self-test: write err {}", e);
        return;
    }
    let mut readback: Box<[u8; 512]> = Box::new([0u8; 512]);
    if let Err(e) = blk.read_sector(0, &mut readback) {
        crate::println!("virtio-blk self-test: read err {}", e);
        return;
    }
    if &readback[..marker.len()] == marker {
        crate::println!("virtio-blk self-test: OK ({} byte round-trip)",
            marker.len());
    } else {
        crate::println!("virtio-blk self-test: MISMATCH first8={:02x?}",
            &readback[..8]);
    }
}
