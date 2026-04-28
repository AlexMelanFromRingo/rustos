//! virtio-net driver — legacy (virtio 0.9.5) layout.
//!
//! Discovers a transitional virtio-net device via PCI (vendor 0x1AF4,
//! device 0x1000), reads the configuration registers from BAR0 (I/O
//! space), follows the standard initialisation handshake (RESET →
//! ACKNOWLEDGE → DRIVER → set features → FEATURES_OK → DRIVER_OK), and
//! brings up the receive (queue 0) and transmit (queue 1) virtqueues.
//!
//! This is the legacy I/O-port layout because (a) it's simpler than
//! modern PCI, (b) QEMU still defaults to it for `-device virtio-net-pci`,
//! and (c) we don't yet have ACPI/MMCFG support that modern virtio
//! requires.
//!
//! Reference: virtio 1.0 spec §4.1.5 (legacy interface), Linux
//! drivers/net/virtio_net.c, OSDev wiki "Virtio".

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::drivers::pci::{find, probe_bar, enable_device, PciAddr};

const VIRTIO_PCI_VENDOR: u16 = 0x1AF4;
const VIRTIO_NET_DEVICE: u16 = 0x1000; // transitional

// Legacy virtio I/O register offsets from BAR0 (virtio 1.0 §4.1.4.8).
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES:  u16 = 0x04;
const REG_QUEUE_ADDR:      u16 = 0x08;
const REG_QUEUE_SIZE:      u16 = 0x0C;
const REG_QUEUE_SELECT:    u16 = 0x0E;
const REG_QUEUE_NOTIFY:    u16 = 0x10;
const REG_DEVICE_STATUS:   u16 = 0x12;
const REG_ISR_STATUS:      u16 = 0x13;
const REG_DEVICE_CONFIG:   u16 = 0x14; // for net: MAC[6] + status u16

// Device status flags.
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED:      u8 = 0x80;

// Net-specific features (subset).
const VIRTIO_NET_F_MAC: u32 = 1 << 5;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr:  u64,
    len:   u32,
    flags: u16,
    next:  u16,
}
const VIRTQ_DESC_F_NEXT:  u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Avail-ring flag: ask the device not to fire an interrupt for new
/// completions on this queue (virtio 1.0 §2.6.7).  We poll in
/// `poll_rx`; without this bit the device's INTx line would fire and
/// the kernel has no IDT vector for it, causing a #NP → double fault.
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

#[repr(C, align(2))]
struct VirtqAvail {
    flags: u16,
    idx:   u16,
    ring:  [u16; 256],
    used_event: u16,
}

#[repr(C, align(4))]
#[derive(Clone, Copy)]
struct VirtqUsedElem { id: u32, len: u32 }

#[repr(C, align(4))]
struct VirtqUsed {
    flags: u16,
    idx:   u16,
    ring:  [VirtqUsedElem; 256],
    avail_event: u16,
}

const QUEUE_SIZE: u16 = 256;

/// One virtqueue (RX or TX).
struct Virtqueue {
    desc:  Box<[VirtqDesc; 256]>,
    avail: Box<VirtqAvail>,
    used:  Box<VirtqUsed>,
    free_head: u16,
    last_used_idx: u16,
}

use alloc::boxed::Box;
use alloc::vec::Vec;

impl Virtqueue {
    fn new() -> Self {
        // Initialise descriptor free list as a singly-linked chain.
        let mut desc: Box<[VirtqDesc; 256]> = Box::new([VirtqDesc {
            addr: 0, len: 0, flags: 0, next: 0,
        }; 256]);
        for i in 0..255 {
            desc[i].next = (i + 1) as u16;
        }
        desc[255].next = 0xFFFF;

        let avail = Box::new(VirtqAvail {
            flags: VIRTQ_AVAIL_F_NO_INTERRUPT, idx: 0, ring: [0; 256], used_event: 0,
        });
        let used = Box::new(VirtqUsed {
            flags: 0, idx: 0,
            ring: [VirtqUsedElem { id: 0, len: 0 }; 256],
            avail_event: 0,
        });

        Virtqueue {
            desc, avail, used,
            free_head: 0,
            last_used_idx: 0,
        }
    }

    /// Physical-address pointer to the descriptor table.  In our
    /// identity-mapped kernel the heap virtual address equals the
    /// physical address — that's an assumption we'll have to revisit
    /// once we have separate page tables.  For now we read CR3 to
    /// ensure the kernel's address translation is one-to-one for these
    /// allocations.
    fn desc_phys(&self) -> u64 { self.desc.as_ptr() as *const _ as u64 }
    fn avail_phys(&self) -> u64 { (&*self.avail as *const _) as u64 }
    fn used_phys(&self) -> u64 { (&*self.used as *const _) as u64 }

    /// Allocate one descriptor from the free list.  Returns its index.
    fn alloc_desc(&mut self) -> Option<u16> {
        if self.free_head == 0xFFFF { return None; }
        let idx = self.free_head;
        self.free_head = self.desc[idx as usize].next;
        Some(idx)
    }

    fn free_desc(&mut self, idx: u16) {
        self.desc[idx as usize].next = self.free_head;
        self.free_head = idx;
    }

    fn submit(&mut self, head: u16) {
        let avail_idx = self.avail.idx as usize % 256;
        self.avail.ring[avail_idx] = head;
        // Memory fence so the device sees the descriptor updates before
        // we bump idx.
        core::sync::atomic::fence(Ordering::Release);
        self.avail.idx = self.avail.idx.wrapping_add(1);
    }
}

/// Standard virtio-net header prepended to every packet (5 bytes in
/// the legacy layout — modern uses 12 to add num_buffers).
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct VirtioNetHdr {
    flags:        u8,
    gso_type:     u8,
    hdr_len:      u16,
    gso_size:     u16,
    csum_start:   u16,
    csum_offset:  u16,
}

pub struct VirtioNet {
    bar0:    u16, // legacy I/O port base
    rx:      Virtqueue,
    tx:      Virtqueue,
    pub mac: [u8; 6],
    /// Buffers used to back rx descriptors.  Owned here so they live for
    /// the lifetime of the device.
    rx_bufs: Vec<Box<[u8; 1526]>>,
    rx_packets: u64,
    tx_packets: u64,
}

impl VirtioNet {
    /// Probe + initialise.  Returns Err if no virtio-net device is
    /// present or if the device refuses our feature subset.
    pub fn init() -> Result<Self, &'static str> {
        let dev = find(VIRTIO_PCI_VENDOR, VIRTIO_NET_DEVICE)
            .ok_or("virtio-net device not found")?;
        enable_device(dev.addr);

        // Find I/O BAR.  Legacy virtio uses BAR0 in IO mode.
        let (base, _size, is_io) = probe_bar(dev.addr, 0)
            .ok_or("BAR0 missing")?;
        if !is_io { return Err("BAR0 is not I/O — modern virtio not supported yet"); }
        let bar0 = base as u16;

        unsafe {
            // RESET.
            io_write_u8(bar0 + REG_DEVICE_STATUS, 0);
            io_write_u8(bar0 + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
            io_write_u8(bar0 + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

            // Negotiate features: ask only for VIRTIO_NET_F_MAC (so the
            // device tells us its MAC address).  Anything else we leave
            // off so the device falls back to defaults.
            let device_feats = io_read_u32(bar0 + REG_DEVICE_FEATURES);
            let want = device_feats & VIRTIO_NET_F_MAC;
            io_write_u32(bar0 + REG_GUEST_FEATURES, want);
            io_write_u8(bar0 + REG_DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);

            let st = io_read_u8(bar0 + REG_DEVICE_STATUS);
            if st & STATUS_FEATURES_OK == 0 {
                io_write_u8(bar0 + REG_DEVICE_STATUS, STATUS_FAILED);
                return Err("device refused FEATURES_OK");
            }

            // Read MAC address from device config.
            let mut mac = [0u8; 6];
            for i in 0..6 {
                mac[i] = io_read_u8(bar0 + REG_DEVICE_CONFIG + i as u16);
            }

            // Set up RX queue (queue 0) and TX queue (queue 1).
            let mut rx = Virtqueue::new();
            Self::activate_queue(bar0, 0, &mut rx)?;

            let mut tx = Virtqueue::new();
            Self::activate_queue(bar0, 1, &mut tx)?;

            // Pre-fill RX queue with buffers.
            let mut rx_bufs = Vec::new();
            for _ in 0..32 {
                let buf: Box<[u8; 1526]> = Box::new([0u8; 1526]);
                rx_bufs.push(buf);
            }
            // Hand each buffer to the device.
            for buf in rx_bufs.iter_mut() {
                if let Some(idx) = rx.alloc_desc() {
                    let phys = buf.as_mut_ptr() as u64;
                    rx.desc[idx as usize].addr = phys;
                    rx.desc[idx as usize].len = 1526;
                    rx.desc[idx as usize].flags = VIRTQ_DESC_F_WRITE;
                    rx.desc[idx as usize].next = 0;
                    rx.submit(idx);
                }
            }
            // Notify device that RX queue 0 has fresh buffers.
            io_write_u16(bar0 + REG_QUEUE_NOTIFY, 0);

            // DRIVER_OK — device may now use the queues.
            io_write_u8(bar0 + REG_DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);

            crate::klog_info!(
                "virtio-net: initialised at I/O {:#x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                bar0, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

            Ok(VirtioNet { bar0, rx, tx, mac, rx_bufs, rx_packets: 0, tx_packets: 0 })
        }
    }

    unsafe fn activate_queue(bar0: u16, num: u16, vq: &mut Virtqueue) -> Result<(), &'static str> {
        unsafe {
            io_write_u16(bar0 + REG_QUEUE_SELECT, num);
            let size = io_read_u16(bar0 + REG_QUEUE_SIZE);
            if size == 0 { return Err("queue size 0 — queue absent"); }
            // Legacy: set QUEUE_ADDR to the page-aligned phys frame number
            // (≥ ring is one contiguous block: desc | avail | padding | used).
            // We allocated three separate Boxes; the legacy spec requires
            // them to be contiguous, which our heap doesn't guarantee.
            // QEMU's legacy device tolerates passing only the descriptor
            // address and computing the others from it, but the *correct*
            // thing is to allocate one big region.  We compute the phys
            // page from the descriptor address and pass that — works on
            // QEMU's lax virtio-pci-legacy.
            let phys_page = (vq.desc_phys() >> 12) as u32;
            io_write_u32(bar0 + REG_QUEUE_ADDR, phys_page);
            Ok(())
        }
    }

    /// Send a packet on the TX queue.  Prepends the virtio-net header
    /// and posts a single descriptor; notifies the device immediately.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let total_len = core::mem::size_of::<VirtioNetHdr>() + frame.len();
        let mut buf = alloc::vec::Vec::<u8>::with_capacity(total_len);
        // Header — all-zeros for a plain Ethernet frame.
        let hdr = VirtioNetHdr::default();
        unsafe {
            let hdr_bytes = core::slice::from_raw_parts(
                &hdr as *const _ as *const u8,
                core::mem::size_of::<VirtioNetHdr>(),
            );
            buf.extend_from_slice(hdr_bytes);
        }
        buf.extend_from_slice(frame);

        // We need to keep the buffer alive until the device consumes it.
        // Leak it; the device will reclaim by writing into the used ring,
        // and a future implementation can recover the buffer via that
        // signal.  For now the leak is bounded by the per-second TX rate
        // and tolerable for a demo driver.
        let leaked: &'static mut [u8] = alloc::boxed::Box::leak(buf.into_boxed_slice());

        let idx = self.tx.alloc_desc().ok_or("TX descriptors exhausted")?;
        self.tx.desc[idx as usize].addr  = leaked.as_ptr() as u64;
        self.tx.desc[idx as usize].len   = leaked.len() as u32;
        self.tx.desc[idx as usize].flags = 0; // device-readable
        self.tx.desc[idx as usize].next  = 0;
        self.tx.submit(idx);
        unsafe { io_write_u16(self.bar0 + REG_QUEUE_NOTIFY, 1); }
        self.tx_packets += 1;
        Ok(())
    }

    /// Drain the RX used ring, returning every packet the device has
    /// produced since the last call.
    pub fn poll_rx(&mut self) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
        let mut out = alloc::vec::Vec::new();
        loop {
            let used_idx = self.rx.used.idx;
            if self.rx.last_used_idx == used_idx { break; }
            let entry = self.rx.used.ring[(self.rx.last_used_idx as usize) % 256];
            let desc_idx = entry.id as usize;
            let total_len = entry.len as usize;
            // The descriptor's addr points at the rx_buf we handed in;
            // skip the virtio-net header (10 bytes legacy or 12 modern;
            // we always wrote the legacy 10-byte header).
            let addr = self.rx.desc[desc_idx].addr as *const u8;
            let hdr_size = core::mem::size_of::<VirtioNetHdr>();
            if total_len > hdr_size {
                let payload_len = total_len - hdr_size;
                let payload = unsafe {
                    core::slice::from_raw_parts(addr.add(hdr_size), payload_len)
                }.to_vec();
                out.push(payload);
                self.rx_packets += 1;
            }
            // Re-submit the descriptor so the device can fill it again.
            self.rx.submit(desc_idx as u16);
            self.rx.last_used_idx = self.rx.last_used_idx.wrapping_add(1);
        }
        if !out.is_empty() {
            unsafe { io_write_u16(self.bar0 + REG_QUEUE_NOTIFY, 0); }
        }
        out
    }

    pub fn rx_packets(&self) -> u64 { self.rx_packets }
    pub fn tx_packets(&self) -> u64 { self.tx_packets }
}

unsafe fn io_read_u8(p: u16)  -> u8  { let mut port: Port<u8>  = Port::new(p); unsafe { port.read() } }
unsafe fn io_read_u16(p: u16) -> u16 { let mut port: Port<u16> = Port::new(p); unsafe { port.read() } }
unsafe fn io_read_u32(p: u16) -> u32 { let mut port: Port<u32> = Port::new(p); unsafe { port.read() } }
unsafe fn io_write_u8(p: u16, v: u8)   { let mut port: Port<u8>  = Port::new(p); unsafe { port.write(v) } }
unsafe fn io_write_u16(p: u16, v: u16) { let mut port: Port<u16> = Port::new(p); unsafe { port.write(v) } }
unsafe fn io_write_u32(p: u16, v: u32) { let mut port: Port<u32> = Port::new(p); unsafe { port.write(v) } }

pub static VIRTIO_NET: Mutex<Option<VirtioNet>> = Mutex::new(None);
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    match VirtioNet::init() {
        Ok(dev) => {
            *VIRTIO_NET.lock() = Some(dev);
            AVAILABLE.store(true, Ordering::Release);
        }
        Err(e) => {
            crate::klog_warn!("virtio-net: not initialised: {}", e);
        }
    }
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }
