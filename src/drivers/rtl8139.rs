//! Realtek RTL8139 NIC driver — PCI vendor 10ec, device 8139.
//!
//! The RTL8139 is one of the most common PCI Ethernet NICs on x86 — QEMU,
//! VirtualBox, and many physical machines support it.  Programming model
//! is straightforward I/O-port access plus DMA buffers for RX/TX.
//!
//! Reference: Realtek RTL8139C(L) datasheet (OSDev wiki "RTL8139").
//!
//! ## Register map (BAR0, I/O space)
//!
//!     0x00-0x05  MAC address (6 bytes, read-only — burned in by EEPROM)
//!     0x10-0x1F  TxAddr 0..3      (32-bit phys addr of next packet to TX)
//!     0x20-0x2F  TxStatus 0..3    (32-bit length + status; write to send)
//!     0x30       RxBufferStart    (32-bit phys addr of RX ring)
//!     0x37       Command          (8-bit: bit 4 Reset, bit 3 RE, bit 2 TE)
//!     0x38       CAPR             (16-bit: current address of pkt read)
//!     0x3A       CBR              (16-bit: hardware-side write pointer)
//!     0x3C       IMR              (16-bit: interrupt mask)
//!     0x3E       ISR              (16-bit: interrupt status, write 1 to clear)
//!     0x40       TCR              (32-bit: TX configuration)
//!     0x44       RCR              (32-bit: RX configuration)
//!     0x52       Config1          (8-bit: power management)
//!
//! ## Init handshake
//!
//! 1. PCI: enable bus master + I/O space (`pci::enable_device`).
//! 2. Power on (`Config1 = 0`).
//! 3. Software reset: set Command bit 4, wait until it clears.
//! 4. Allocate an 8 KiB + 16 B + 1500 B RX ring in physically-contiguous
//!    DMA memory.  The +1500 lets a wrapped packet straddle the end
//!    without splitting (RCR WRAP bit).
//! 5. Write RX ring's physical address to RxBufferStart.
//! 6. Mask all interrupts (`IMR = 0`).  We poll instead — the kernel has
//!    no IDT vector wired for the RTL8139 INTx line, so an unmasked IRQ
//!    would trip a #NP cascade just like virtio-blk did before
//!    VIRTQ_AVAIL_F_NO_INTERRUPT was set.
//! 7. Configure RCR: AB | AM | APM | AAP (accept broadcast / multicast /
//!    physical match / promiscuous) plus WRAP and 8 KiB length.
//! 8. Configure TCR: 32-byte burst, retry threshold default.
//! 9. Enable RX + TX (`Command = TE | RE`).
//!
//! ## Send
//!
//! Each TX descriptor (TxAddr/TxStatus pair, 4 of them) is owned by the
//! NIC after we kick it.  We allocate a per-descriptor 1.5 KiB DMA bounce
//! buffer up front and copy each outgoing frame into it; we then write
//! the buffer's phys addr to TxAddr and the packet length to TxStatus.
//! Hardware sets bit 13 (OWN) when finished.
//!
//! ## Receive
//!
//! The NIC writes incoming packets into the RX ring and bumps CBR.  We
//! advance CAPR as we consume them.  Each packet is preceded by a 4-byte
//! header: 16-bit status, 16-bit length (including the 4-byte CRC, which
//! we trim).
//!
//! ## Locking / safety
//!
//! All RTL8139 access happens behind the global Mutex; the device is
//! assumed to be single-instance, single-threaded.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::drivers::pci::{find, probe_bar, enable_device};
use crate::memory::alloc_dma_contig;

const RTL_VENDOR: u16 = 0x10EC;
const RTL_DEVICE: u16 = 0x8139;

const REG_MAC0:        u16 = 0x00;
const REG_TX_ADDR0:    u16 = 0x20;  // four 32-bit slots starting here
const REG_TX_STATUS0:  u16 = 0x10;  // four 32-bit slots starting here
const REG_RX_BUF_START: u16 = 0x30;
const REG_COMMAND:     u16 = 0x37;
const REG_CAPR:        u16 = 0x38;
const REG_IMR:         u16 = 0x3C;
const REG_ISR:         u16 = 0x3E;
const REG_TCR:         u16 = 0x40;
const REG_RCR:         u16 = 0x44;
const REG_CONFIG_1:    u16 = 0x52;

const CMD_RESET:       u8 = 0x10;
const CMD_RX_ENABLE:   u8 = 0x08;
const CMD_TX_ENABLE:   u8 = 0x04;
const CMD_BUFFER_EMPTY:u8 = 0x01;

// Receive Configuration bits.
const RCR_AAP:  u32 = 0x0001; // Accept all packets (promiscuous)
const RCR_APM:  u32 = 0x0002; // Accept physical match
const RCR_AM:   u32 = 0x0004; // Accept multicast
const RCR_AB:   u32 = 0x0008; // Accept broadcast
const RCR_WRAP: u32 = 0x0080; // Wrap allowed (don't split packets at ring end)
const RCR_RXFTH_NONE: u32 = 0x0000_0000; // No RX FIFO threshold
const RCR_RBLEN_8K:   u32 = 0x0000_0000; // 8 KB ring (+16 + 1500 head pad)

const TCR_IFG_NORMAL: u32 = 0x0300_0000; // 96-bit inter-frame gap

/// Total RX ring size.  RTL8139 supports 8K, 16K, 32K, 64K.  We use 8K
/// (smallest) and add the 16-byte header + 1500-byte WRAP slack.
const RX_RING_SIZE: usize = 8192;
const RX_RING_PAD:  usize = 16 + 1500;
const RX_RING_TOTAL: usize = RX_RING_SIZE + RX_RING_PAD;

const TX_BUF_SIZE: usize = 1792; // ≥ Ethernet max frame (1518) rounded up

#[allow(dead_code)] // queue_phys is reserved for cleanup paths.
pub struct Rtl8139 {
    bar0: u16,
    /// Direct-map virtual base of the RX ring.
    rx_virt: usize,
    /// Phys addr of the RX ring (fed to RxBufferStart).
    rx_phys: u64,
    /// Software CAPR — index into `rx_virt` of the next byte we'll read.
    rx_offset: usize,

    /// Per-TX-descriptor DMA bounce buffers.  4 of them, round-robin.
    tx_virt: [usize; 4],
    tx_phys: [u64; 4],
    /// Next TX slot to use.
    tx_next: u8,

    pub mac: [u8; 6],
    rx_packets: u64,
    tx_packets: u64,
}

unsafe impl Send for Rtl8139 {}

impl Rtl8139 {
    pub fn init() -> Result<Self, &'static str> {
        let dev = find(RTL_VENDOR, RTL_DEVICE).ok_or("rtl8139: device not found")?;
        enable_device(dev.addr);
        let (base, _size, is_io) = probe_bar(dev.addr, 0).ok_or("rtl8139: BAR0 missing")?;
        if !is_io { return Err("rtl8139: BAR0 not I/O"); }
        let bar0 = base as u16;

        unsafe {
            // Power on the device.
            io_w8(bar0 + REG_CONFIG_1, 0);

            // Soft reset.  Wait up to ~1M iterations for the bit to clear.
            io_w8(bar0 + REG_COMMAND, CMD_RESET);
            for _ in 0..1_000_000u32 {
                if io_r8(bar0 + REG_COMMAND) & CMD_RESET == 0 { break; }
                core::hint::spin_loop();
            }
            if io_r8(bar0 + REG_COMMAND) & CMD_RESET != 0 {
                return Err("rtl8139: soft reset stuck");
            }

            // Read MAC.
            let mut mac = [0u8; 6];
            for i in 0..6 {
                mac[i] = io_r8(bar0 + REG_MAC0 + i as u16);
            }

            // Allocate RX ring (DMA-contig, ≥ 8K + 16 + 1500 = 9708 bytes).
            let (rx_virt, rx_phys) = alloc_dma_contig(RX_RING_TOTAL)
                .ok_or("rtl8139: RX ring allocation failed")?;
            let rx_virt = rx_virt as usize;
            io_w32(bar0 + REG_RX_BUF_START, rx_phys as u32);

            // Mask all interrupts — we poll.  Without this the device's
            // INTx line would fire on the first packet and trip a #NP
            // (no IDT vector for that IRQ).
            io_w16(bar0 + REG_IMR, 0);

            // RCR: accept everything we care about, 8 KB ring, wrap enabled.
            let rcr = RCR_AAP | RCR_APM | RCR_AM | RCR_AB | RCR_WRAP
                    | RCR_RBLEN_8K | RCR_RXFTH_NONE;
            io_w32(bar0 + REG_RCR, rcr);

            // TCR: standard inter-frame gap, default everything else.
            io_w32(bar0 + REG_TCR, TCR_IFG_NORMAL);

            // Allocate per-TX bounce buffers up front.
            let mut tx_virt = [0usize; 4];
            let mut tx_phys = [0u64; 4];
            for i in 0..4 {
                let (v, p) = alloc_dma_contig(TX_BUF_SIZE)
                    .ok_or("rtl8139: TX buffer alloc failed")?;
                tx_virt[i] = v as usize;
                tx_phys[i] = p;
                // Pre-set the TxAddr register so we don't have to re-write
                // it on every send (only TxStatus needs to change).
                io_w32(bar0 + REG_TX_ADDR0 + (i as u16) * 4, p as u32);
            }

            // Enable RX + TX.  Order matters per the datasheet: configure
            // RCR/TCR first, *then* set RE/TE in Command.
            io_w8(bar0 + REG_COMMAND, CMD_RX_ENABLE | CMD_TX_ENABLE);

            crate::klog_info!(
                "rtl8139: initialised at I/O {:#x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                bar0, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

            Ok(Rtl8139 {
                bar0,
                rx_virt, rx_phys, rx_offset: 0,
                tx_virt, tx_phys, tx_next: 0,
                mac, rx_packets: 0, tx_packets: 0,
            })
        }
    }

    /// Send one Ethernet frame.  Returns Err if all four TX descriptors
    /// are still busy.  `frame` MUST be ≤ 1518 bytes (incl. 14-byte
    /// Ethernet header but excl. CRC, which the NIC adds).
    pub fn send(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.len() > TX_BUF_SIZE { return Err("rtl8139: frame too large"); }
        if frame.len() < 60 {
            // Pad short frames to 60 bytes (Ethernet minimum); the NIC
            // adds CRC for a final 64.
        }
        let slot = self.tx_next as usize;
        unsafe {
            // Wait for slot's OWN bit (bit 13) — set when last send
            // finished.  Bit 13 = TOK or OWN-equivalent in RTL8139:
            // datasheet says the OWN bit is bit 13 of TxStatus and is
            // set by hardware on completion.
            let status_reg = self.bar0 + REG_TX_STATUS0 + (slot as u16) * 4;
            for _ in 0..1_000_000u32 {
                let st = io_r32(status_reg);
                // Bit 13 = OWN (set by NIC when TX done; cleared when
                // we kick the descriptor by writing the length).
                if st & (1 << 13) != 0 || st == 0 { break; }
                core::hint::spin_loop();
            }

            // Copy frame into the descriptor's bounce buffer.
            let dst = core::slice::from_raw_parts_mut(
                self.tx_virt[slot] as *mut u8, TX_BUF_SIZE);
            dst[..frame.len()].copy_from_slice(frame);
            // Pad to 60 bytes (Ethernet min) so the NIC doesn't reject.
            if frame.len() < 60 {
                for b in dst[frame.len()..60].iter_mut() { *b = 0; }
            }
            let len = frame.len().max(60);

            // Kick: writing length to TxStatus clears OWN and starts TX.
            // Bits 0..12 = size; we leave threshold (bits 16..21) at 0
            // which means transfer immediately.
            io_w32(status_reg, len as u32);
            self.tx_next = (self.tx_next + 1) & 3;
            self.tx_packets += 1;
            Ok(())
        }
    }

    /// Drain the RX ring, returning every packet that's accumulated since
    /// the last poll.  Each packet is the raw Ethernet frame (no FCS).
    pub fn poll_rx(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        unsafe {
            loop {
                if io_r8(self.bar0 + REG_COMMAND) & CMD_BUFFER_EMPTY != 0 {
                    break;
                }
                // Header at rx_virt + rx_offset: u16 status, u16 length.
                let base = (self.rx_virt + self.rx_offset) as *const u8;
                let status = u16::from_le_bytes([*base, *base.add(1)]);
                let length = u16::from_le_bytes([*base.add(2), *base.add(3)]) as usize;
                if length < 4 || length > 1518 + 4 {
                    // Garbage — reset CAPR to RX-engine's CBR and bail out.
                    let cbr = io_r16(self.bar0 + 0x3A);
                    self.rx_offset = cbr as usize;
                    break;
                }
                // Status bit 0 = ROK.  Other bits indicate errors.
                if status & 1 != 0 {
                    // payload bytes = length - 4 (drop CRC).
                    let payload_len = length - 4;
                    let pkt = core::slice::from_raw_parts(base.add(4), payload_len)
                        .to_vec();
                    out.push(pkt);
                    self.rx_packets += 1;
                }
                // Advance rx_offset past header (4) + length, 4-byte aligned.
                self.rx_offset = (self.rx_offset + 4 + length + 3) & !3;
                if self.rx_offset >= RX_RING_SIZE {
                    self.rx_offset -= RX_RING_SIZE;
                }
                // Update CAPR.  RTL8139 quirk: CAPR is "offset - 16".
                let capr = (self.rx_offset.wrapping_sub(16) & 0xFFFF) as u16;
                io_w16(self.bar0 + REG_CAPR, capr);

                // Acknowledge ROK in ISR (write-1-to-clear).
                io_w16(self.bar0 + REG_ISR, 1);
            }
        }
        out
    }

    pub fn rx_packets(&self) -> u64 { self.rx_packets }
    pub fn tx_packets(&self) -> u64 { self.tx_packets }
}

unsafe fn io_r8(p: u16) -> u8  { let mut port: Port<u8>  = Port::new(p); unsafe { port.read() } }
unsafe fn io_r16(p: u16) -> u16{ let mut port: Port<u16> = Port::new(p); unsafe { port.read() } }
unsafe fn io_r32(p: u16) -> u32{ let mut port: Port<u32> = Port::new(p); unsafe { port.read() } }
unsafe fn io_w8(p: u16, v: u8)   { let mut port: Port<u8>  = Port::new(p); unsafe { port.write(v) } }
unsafe fn io_w16(p: u16, v: u16) { let mut port: Port<u16> = Port::new(p); unsafe { port.write(v) } }
unsafe fn io_w32(p: u16, v: u32) { let mut port: Port<u32> = Port::new(p); unsafe { port.write(v) } }

pub static RTL8139: Mutex<Option<Rtl8139>> = Mutex::new(None);
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    match Rtl8139::init() {
        Ok(d) => {
            *RTL8139.lock() = Some(d);
            AVAILABLE.store(true, Ordering::Release);
        }
        Err(e) => crate::klog_warn!("rtl8139: not initialised: {}", e),
    }
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }
