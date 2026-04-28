//! Intel 82540EM (E1000) gigabit NIC driver — PCI 8086:100E.
//!
//! QEMU's `-device e1000` defaults to this card.  Programming model
//! is MMIO registers + DMA descriptor rings, similar in spirit to
//! virtio-net but with a fixed register layout.
//!
//! Reference: Intel 82540EM datasheet "PCI/PCI-X Family of Gigabit
//! Ethernet Controllers Software Developer's Manual" (319.pdf), §13
//! (initialisation), §14 (transmit), §3 (registers).
//!
//! ## Register map (BAR0, MMIO; offsets in bytes)
//!
//!     0x0000  CTRL        Device control
//!     0x0008  STATUS      Device status
//!     0x0014  EERD        EEPROM read
//!     0x0100  RCTL        Receive control
//!     0x0400  TCTL        Transmit control
//!     0x2800  RDBAL       RX descriptor base lo
//!     0x2804  RDBAH       RX descriptor base hi
//!     0x2808  RDLEN       RX descriptor ring length (bytes)
//!     0x2810  RDH         RX head (NIC writes; SW reads)
//!     0x2818  RDT         RX tail (SW writes)
//!     0x3800  TDBAL       TX descriptor base lo
//!     0x3804  TDBAH       TX descriptor base hi
//!     0x3808  TDLEN       TX descriptor ring length (bytes)
//!     0x3810  TDH         TX head
//!     0x3818  TDT         TX tail
//!     0x5400  RAL[0]      Receive address low (MAC bits 0..31)
//!     0x5404  RAH[0]      Receive address high (bit 31 = AV)
//!     0x00D0  IMS         Interrupt mask set (we leave at 0 — polling)
//!
//! ## Init handshake
//!
//! 1. PCI: enable bus master + memory space (`pci::enable_device`).
//! 2. Read MAC: prefer EEPROM via EERD (data bit-shift), else RAL/RAH.
//! 3. Allocate RX descriptor ring (256 * 16 bytes) + per-descriptor
//!    2 KiB buffers, all in DMA-contig memory; populate descriptor
//!    addr/len fields and write RDBAL/RDBAH/RDLEN, RDH=0, RDT=N-1.
//! 4. Allocate TX descriptor ring (256 * 16 bytes); write TDBAL/TDBAH
//!    /TDLEN, TDH=0, TDT=0.
//! 5. RCTL = EN | BAM | SECRC | LPE_off | BSIZE_2048.
//! 6. TCTL = EN | PSP | CT_default | COLD_default.
//! 7. Mask all interrupts (IMS=0) — we have no IDT vector for the
//!    E1000 IRQ line yet, so polling is the safe model (same trick as
//!    virtio-blk's NO_INTERRUPT and rtl8139's IMR=0).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::drivers::pci::{find, probe_bar, enable_device};
use crate::memory::{alloc_dma_contig, phys_offset};

const E1000_VENDOR: u16 = 0x8086;
const E1000_DEVICE: u16 = 0x100E; // 82540EM (QEMU default)

const REG_CTRL:   u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EERD:   u32 = 0x0014;
const REG_IMS:    u32 = 0x00D0;
const REG_RCTL:   u32 = 0x0100;
const REG_TCTL:   u32 = 0x0400;
const REG_RDBAL:  u32 = 0x2800;
const REG_RDBAH:  u32 = 0x2804;
const REG_RDLEN:  u32 = 0x2808;
const REG_RDH:    u32 = 0x2810;
const REG_RDT:    u32 = 0x2818;
const REG_TDBAL:  u32 = 0x3800;
const REG_TDBAH:  u32 = 0x3804;
const REG_TDLEN:  u32 = 0x3808;
const REG_TDH:    u32 = 0x3810;
const REG_TDT:    u32 = 0x3818;
const REG_RAL0:   u32 = 0x5400;
const REG_RAH0:   u32 = 0x5404;

const CTRL_RST:    u32 = 1 << 26;
const CTRL_SLU:    u32 = 1 << 6;  // set link up
const CTRL_ASDE:   u32 = 1 << 5;  // auto-speed detection enable

const RCTL_EN:     u32 = 1 << 1;
const RCTL_BAM:    u32 = 1 << 15; // accept broadcast
const RCTL_SECRC:  u32 = 1 << 26; // strip ethernet CRC
const RCTL_BSIZE_2048: u32 = 0; // bits 16..17 = 00 → 2048 with no BSEX

const TCTL_EN:     u32 = 1 << 1;
const TCTL_PSP:    u32 = 1 << 3;  // pad short packets
// CT (collision threshold) bits 4..11 = 0x10 (default), COLD bits 12..21 = 0x40.
const TCTL_CT_DEF: u32 = 0x10 << 4;
const TCTL_COLD_HD: u32 = 0x40 << 12;

const N_RX_DESC: usize = 32;
const N_TX_DESC: usize = 32;
const RX_BUF_SIZE: usize = 2048;
const TX_BUF_SIZE: usize = 2048;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RxDesc {
    addr:     u64,
    length:   u16,
    checksum: u16,
    status:   u8,
    errors:   u8,
    special:  u16,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDesc {
    addr:     u64,
    length:   u16,
    cso:      u8,
    cmd:      u8,
    status:   u8,
    css:      u8,
    special:  u16,
}

const TX_CMD_EOP: u8 = 1 << 0;  // End of packet
const TX_CMD_IFCS: u8 = 1 << 1; // Insert FCS
const TX_CMD_RS: u8 = 1 << 3;   // Report status
const TX_STAT_DD: u8 = 1 << 0;  // Descriptor done

const RX_STAT_DD: u8 = 1 << 0;
const RX_STAT_EOP: u8 = 1 << 1;

#[allow(dead_code)]
pub struct E1000 {
    mmio_virt: usize,
    rx_ring_phys: u64,
    tx_ring_phys: u64,
    rx_ring_virt: usize,
    tx_ring_virt: usize,
    rx_buf_phys: [u64; N_RX_DESC],
    rx_buf_virt: [usize; N_RX_DESC],
    tx_buf_phys: [u64; N_TX_DESC],
    tx_buf_virt: [usize; N_TX_DESC],
    tx_next: usize,
    rx_next: usize,
    pub mac: [u8; 6],
    rx_packets: u64,
    tx_packets: u64,
}

unsafe impl Send for E1000 {}

unsafe fn mmio_read(base: usize, off: u32) -> u32 {
    unsafe { core::ptr::read_volatile((base + off as usize) as *const u32) }
}
unsafe fn mmio_write(base: usize, off: u32, val: u32) {
    unsafe { core::ptr::write_volatile((base + off as usize) as *mut u32, val) }
}

impl E1000 {
    pub fn init() -> Result<Self, &'static str> {
        let dev = find(E1000_VENDOR, E1000_DEVICE).ok_or("e1000: device not found")?;
        enable_device(dev.addr);
        let (base, _size, is_io) = probe_bar(dev.addr, 0).ok_or("e1000: BAR0 missing")?;
        if is_io { return Err("e1000: BAR0 is I/O, expected MMIO"); }
        // Map the MMIO BAR through the bootloader's direct phys-memory map.
        let mmio_virt = (phys_offset() + base) as usize;

        unsafe {
            // Soft reset: set CTRL.RST, wait for it to clear.
            let ctrl = mmio_read(mmio_virt, REG_CTRL);
            mmio_write(mmio_virt, REG_CTRL, ctrl | CTRL_RST);
            for _ in 0..100_000u32 {
                if mmio_read(mmio_virt, REG_CTRL) & CTRL_RST == 0 { break; }
            }
            if mmio_read(mmio_virt, REG_CTRL) & CTRL_RST != 0 {
                return Err("e1000: reset stuck");
            }

            // Set link up, enable auto-speed.
            let ctrl = mmio_read(mmio_virt, REG_CTRL);
            mmio_write(mmio_virt, REG_CTRL, ctrl | CTRL_SLU | CTRL_ASDE);

            // Read MAC.  Try EEPROM first; if EERD never reports DONE
            // (QEMU sometimes leaves EEPROM disabled), fall back to
            // RAL[0]/RAH[0] which are pre-populated by hardware.
            let mac = match Self::read_mac_eeprom(mmio_virt) {
                Some(m) => m,
                None => Self::read_mac_ral(mmio_virt),
            };

            // Allocate RX ring + 32 RX buffers in one DMA-contig block.
            let rx_ring_size = N_RX_DESC * core::mem::size_of::<RxDesc>(); // 256 bytes
            let (rx_ring_v, rx_ring_p) = alloc_dma_contig(rx_ring_size)
                .ok_or("e1000: RX ring alloc failed")?;
            let rx_ring_virt = rx_ring_v as usize;

            let mut rx_buf_phys = [0u64; N_RX_DESC];
            let mut rx_buf_virt = [0usize; N_RX_DESC];
            for i in 0..N_RX_DESC {
                let (v, p) = alloc_dma_contig(RX_BUF_SIZE)
                    .ok_or("e1000: RX buffer alloc failed")?;
                rx_buf_virt[i] = v as usize;
                rx_buf_phys[i] = p;
                let desc_ptr = (rx_ring_virt + i * 16) as *mut RxDesc;
                core::ptr::write_volatile(desc_ptr, RxDesc {
                    addr: p, length: 0, checksum: 0,
                    status: 0, errors: 0, special: 0,
                });
            }

            mmio_write(mmio_virt, REG_RDBAL, rx_ring_p as u32);
            mmio_write(mmio_virt, REG_RDBAH, (rx_ring_p >> 32) as u32);
            mmio_write(mmio_virt, REG_RDLEN, rx_ring_size as u32);
            mmio_write(mmio_virt, REG_RDH, 0);
            mmio_write(mmio_virt, REG_RDT, (N_RX_DESC - 1) as u32);

            // Allocate TX ring + 32 TX bounce buffers.
            let tx_ring_size = N_TX_DESC * core::mem::size_of::<TxDesc>();
            let (tx_ring_v, tx_ring_p) = alloc_dma_contig(tx_ring_size)
                .ok_or("e1000: TX ring alloc failed")?;
            let tx_ring_virt = tx_ring_v as usize;
            let mut tx_buf_phys = [0u64; N_TX_DESC];
            let mut tx_buf_virt = [0usize; N_TX_DESC];
            for i in 0..N_TX_DESC {
                let (v, p) = alloc_dma_contig(TX_BUF_SIZE)
                    .ok_or("e1000: TX buffer alloc failed")?;
                tx_buf_virt[i] = v as usize;
                tx_buf_phys[i] = p;
                let desc_ptr = (tx_ring_virt + i * 16) as *mut TxDesc;
                core::ptr::write_volatile(desc_ptr, TxDesc {
                    addr: 0, length: 0, cso: 0, cmd: 0,
                    status: TX_STAT_DD, css: 0, special: 0,
                });
            }
            mmio_write(mmio_virt, REG_TDBAL, tx_ring_p as u32);
            mmio_write(mmio_virt, REG_TDBAH, (tx_ring_p >> 32) as u32);
            mmio_write(mmio_virt, REG_TDLEN, tx_ring_size as u32);
            mmio_write(mmio_virt, REG_TDH, 0);
            mmio_write(mmio_virt, REG_TDT, 0);

            // Mask all interrupts — we poll.
            mmio_write(mmio_virt, REG_IMS, 0);

            // Configure RCTL: enable, accept broadcast, strip CRC, 2 KiB buffers.
            mmio_write(mmio_virt, REG_RCTL,
                RCTL_EN | RCTL_BAM | RCTL_SECRC | RCTL_BSIZE_2048);

            // Configure TCTL: enable + pad short + default thresholds.
            mmio_write(mmio_virt, REG_TCTL, TCTL_EN | TCTL_PSP | TCTL_CT_DEF | TCTL_COLD_HD);

            crate::klog_info!(
                "e1000: initialised, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

            Ok(E1000 {
                mmio_virt, rx_ring_phys: rx_ring_p, tx_ring_phys: tx_ring_p,
                rx_ring_virt, tx_ring_virt,
                rx_buf_phys, rx_buf_virt, tx_buf_phys, tx_buf_virt,
                tx_next: 0, rx_next: 0,
                mac, rx_packets: 0, tx_packets: 0,
            })
        }
    }

    /// Try to read the MAC from the EEPROM via the EERD register.
    /// Each word holds two MAC bytes (low byte first).  Returns None if
    /// the EEPROM never signals DONE (bit 4 of EERD) — common on QEMU.
    unsafe fn read_mac_eeprom(mmio: usize) -> Option<[u8; 6]> {
        let mut mac = [0u8; 6];
        for i in 0..3usize {
            unsafe { mmio_write(mmio, REG_EERD, ((i as u32) << 8) | 1); }
            let mut tmp = 0u32;
            let mut ok = false;
            for _ in 0..1_000u32 {
                tmp = unsafe { mmio_read(mmio, REG_EERD) };
                if tmp & (1 << 4) != 0 { ok = true; break; }
            }
            if !ok { return None; }
            let word = (tmp >> 16) as u16;
            mac[i * 2]     = (word & 0xFF) as u8;
            mac[i * 2 + 1] = (word >> 8)   as u8;
        }
        Some(mac)
    }

    /// Read the MAC out of RAL[0]/RAH[0] — pre-loaded by hardware on
    /// reset and the canonical QEMU path.
    unsafe fn read_mac_ral(mmio: usize) -> [u8; 6] {
        let lo = unsafe { mmio_read(mmio, REG_RAL0) };
        let hi = unsafe { mmio_read(mmio, REG_RAH0) };
        [
            (lo & 0xFF) as u8,
            ((lo >> 8) & 0xFF) as u8,
            ((lo >> 16) & 0xFF) as u8,
            ((lo >> 24) & 0xFF) as u8,
            (hi & 0xFF) as u8,
            ((hi >> 8) & 0xFF) as u8,
        ]
    }

    /// Send one Ethernet frame.  Copies into the next TX descriptor's
    /// bounce buffer, sets cmd = EOP|IFCS|RS, advances TDT.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.len() > TX_BUF_SIZE {
            return Err("e1000: frame too large");
        }
        let i = self.tx_next;
        let desc_ptr = (self.tx_ring_virt + i * 16) as *mut TxDesc;
        unsafe {
            // Wait for descriptor to be free (status.DD set).
            for _ in 0..1_000_000u32 {
                let st = core::ptr::read_volatile(&(*desc_ptr).status);
                if st & TX_STAT_DD != 0 || self.tx_packets == 0 { break; }
                core::hint::spin_loop();
            }

            let dst = core::slice::from_raw_parts_mut(self.tx_buf_virt[i] as *mut u8, TX_BUF_SIZE);
            dst[..frame.len()].copy_from_slice(frame);
            core::ptr::write_volatile(desc_ptr, TxDesc {
                addr: self.tx_buf_phys[i],
                length: frame.len() as u16,
                cso: 0,
                cmd: TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS,
                status: 0,
                css: 0,
                special: 0,
            });
            self.tx_next = (i + 1) % N_TX_DESC;
            mmio_write(self.mmio_virt, REG_TDT, self.tx_next as u32);
            self.tx_packets += 1;
        }
        Ok(())
    }

    /// Drain the RX ring.  Each descriptor whose status.DD bit is set
    /// has a packet at desc.addr of desc.length bytes.  We copy out,
    /// reset status, and advance RDT.
    pub fn poll_rx(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        unsafe {
            loop {
                let i = self.rx_next;
                let desc_ptr = (self.rx_ring_virt + i * 16) as *mut RxDesc;
                let st = core::ptr::read_volatile(&(*desc_ptr).status);
                if st & RX_STAT_DD == 0 { break; }
                if st & RX_STAT_EOP != 0 {
                    let len = core::ptr::read_volatile(&(*desc_ptr).length) as usize;
                    let pkt = core::slice::from_raw_parts(self.rx_buf_virt[i] as *const u8, len)
                        .to_vec();
                    out.push(pkt);
                    self.rx_packets += 1;
                }
                // Re-arm the descriptor.
                (*desc_ptr).status = 0;
                self.rx_next = (i + 1) % N_RX_DESC;
                mmio_write(self.mmio_virt, REG_RDT, self.rx_next as u32);
            }
        }
        out
    }

    pub fn rx_packets(&self) -> u64 { self.rx_packets }
    pub fn tx_packets(&self) -> u64 { self.tx_packets }
}

pub static E1000: Mutex<Option<E1000>> = Mutex::new(None);
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    match E1000::init() {
        Ok(d) => {
            *E1000.lock() = Some(d);
            AVAILABLE.store(true, Ordering::Release);
        }
        Err(e) => crate::klog_warn!("e1000: not initialised: {}", e),
    }
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }
