//! AHCI 1.3.1 SATA driver — discovery, port init, READ/WRITE_DMA_EXT.
//!
//! Reference: Intel "Serial ATA Advanced Host Controller Interface
//! 1.3.1" specification.  We implement the subset needed to read and
//! write 512-byte sectors on SATA disks attached to QEMU's `-device
//! ahci`.
//!
//! ## Discovery
//!
//! AHCI controllers identify on PCI as class 0x01 (mass-storage),
//! subclass 0x06 (SATA), prog-if 0x01 (AHCI).  BAR5 (offset 0x24)
//! holds the MMIO base of the HBA registers.
//!
//! ## HBA register layout
//!
//!     0x00  CAP        host capabilities (bit-31 sector support,
//!                      bits 0..4 = NP, # of ports - 1)
//!     0x04  GHC        global host control (bit 0 = HBA reset, bit
//!                      31 = AHCI enable, bit 1 = INT enable — left
//!                      0 since we poll)
//!     0x08  IS         interrupt status (write-1-to-clear)
//!     0x0C  PI         ports implemented (bitmap)
//!     0x10  VS         AHCI version
//!     0x100 + N*0x80   port N register block
//!
//! ## Per-port register block
//!
//!     +0x00 CLB / CLBU   command list base (1024-byte aligned, 32
//!                        slots × 32 bytes)
//!     +0x08 FB  / FBU    FIS receive area (256-byte aligned)
//!     +0x10 IS           interrupt status
//!     +0x14 IE           interrupt enable
//!     +0x18 CMD          command + status (bit 0 ST = start, bit 4
//!                        FRE = FIS receive enable, bit 14 FR = FIS
//!                        running, bit 15 CR = command list running)
//!     +0x20 TFD          task file data (low byte = ATA status)
//!     +0x24 SIG          device signature (0x101 = SATA, 0xEB14 =
//!                        SATAPI, 0xC33C = SEMB, 0x96690101 =
//!                        port-multiplier, 0xFFFFFFFF = no device)
//!     +0x28 SSTS         SATA status (bits 0..3 DET = device detect,
//!                        bits 8..11 IPM = power-management; we want
//!                        DET == 3 + IPM == 1)
//!     +0x30 SERR         SATA error (write-1-to-clear)
//!     +0x34 SACT         SATA active
//!     +0x38 CI           command issue (bit per slot — set to start,
//!                        cleared by HBA on completion)

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::structures::paging::PhysFrame;

use crate::drivers::pci::{enable_device, find_class, probe_bar};
use crate::memory::{alloc_dma_contig, phys_offset};

const HBA_CAP:  u32 = 0x00;
const HBA_GHC:  u32 = 0x04;
const HBA_PI:   u32 = 0x0C;

const PORT_BASE: u32 = 0x100;
const PORT_STRIDE: u32 = 0x80;

const PRT_CLB:  u32 = 0x00;
const PRT_CLBU: u32 = 0x04;
const PRT_FB:   u32 = 0x08;
const PRT_FBU:  u32 = 0x0C;
const PRT_IS:   u32 = 0x10;
const PRT_IE:   u32 = 0x14;
const PRT_CMD:  u32 = 0x18;
const PRT_TFD:  u32 = 0x20;
const PRT_SIG:  u32 = 0x24;
const PRT_SSTS: u32 = 0x28;
const PRT_SERR: u32 = 0x30;
const PRT_CI:   u32 = 0x38;

const CMD_ST:  u32 = 1 << 0;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR:  u32 = 1 << 14;
const CMD_CR:  u32 = 1 << 15;

const SIG_SATA: u32 = 0x0000_0101;

const ATA_CMD_READ_DMA_EX:  u8 = 0x25;
const ATA_CMD_WRITE_DMA_EX: u8 = 0x35;
const ATA_CMD_IDENTIFY:     u8 = 0xEC;

const FIS_TYPE_REG_H2D: u8 = 0x27;

#[repr(C)]
struct CmdHeader {
    /// bits 0..4 PRDTL, bit 5 PMP, bit 6 C, bit 7 B, bit 8 R, bit 9 P,
    /// bit 10 W (write), bit 11 A, bit 12 PMP port mux, bits 16..31 PRDTL
    flags: u16,
    prdtl: u16,
    prdbc: u32,
    ctba: u32,
    ctbau: u32,
    _reserved: [u32; 4],
}

#[repr(C)]
struct CmdTable {
    /// Command FIS (64 bytes max).
    cfis: [u8; 64],
    /// ATAPI command (16 bytes).
    acmd: [u8; 16],
    _reserved: [u8; 48],
    /// Physical Region Descriptor Table — variable length, but we
    /// only ever issue one PRD entry per command.
    prdt: [PrdtEntry; 1],
}

#[repr(C)]
struct PrdtEntry {
    dba:  u32,
    dbau: u32,
    _reserved: u32,
    /// bits 0..21 = byte count - 1 (max 4 MiB - 2), bit 31 = interrupt-on-complete
    dbc: u32,
}

/// Host-to-device register FIS for ATA commands.
#[repr(C)]
struct FisRegH2D {
    fis_type: u8,
    pm_port:  u8, // bit 7 = command/control flag
    command:  u8,
    featurel: u8,

    lba0: u8,
    lba1: u8,
    lba2: u8,
    device: u8,

    lba3: u8,
    lba4: u8,
    lba5: u8,
    featureh: u8,

    countl:  u8,
    counth:  u8,
    icc:     u8,
    control: u8,

    _reserved: [u8; 4],
}

#[allow(dead_code)]
pub struct AhciPort {
    /// Direct-map virt of HBA MMIO.
    hba_virt: usize,
    /// Port number (0..32).
    pub port: u32,
    /// Phys + virt of command list (1 KiB).
    clist_virt: usize,
    clist_phys: u64,
    /// Phys + virt of FIS receive area (256 B).
    fis_virt: usize,
    fis_phys: u64,
    /// Phys + virt of one command table (slot 0; 256 B + 16 B PRDT).
    ctbl_virt: usize,
    ctbl_phys: u64,
    /// Number of 512-byte sectors reported by IDENTIFY DEVICE.
    pub sectors: u64,
}

unsafe fn mmio_read(base: usize, off: u32) -> u32 {
    unsafe { core::ptr::read_volatile((base + off as usize) as *const u32) }
}
unsafe fn mmio_write(base: usize, off: u32, val: u32) {
    unsafe { core::ptr::write_volatile((base + off as usize) as *mut u32, val) }
}

fn port_off(port: u32, reg: u32) -> u32 { PORT_BASE + port * PORT_STRIDE + reg }

unsafe impl Send for AhciPort {}

impl AhciPort {
    fn stop(&self) {
        unsafe {
            let cmd = mmio_read(self.hba_virt, port_off(self.port, PRT_CMD));
            mmio_write(self.hba_virt, port_off(self.port, PRT_CMD),
                cmd & !(CMD_ST | CMD_FRE));
            // Wait for FR and CR to clear (≤ 500 ms per spec).
            for _ in 0..500_000u32 {
                let c = mmio_read(self.hba_virt, port_off(self.port, PRT_CMD));
                if c & (CMD_FR | CMD_CR) == 0 { break; }
            }
        }
    }
    fn start(&self) {
        unsafe {
            // Wait for CR clear.
            for _ in 0..500_000u32 {
                let c = mmio_read(self.hba_virt, port_off(self.port, PRT_CMD));
                if c & CMD_CR == 0 { break; }
            }
            let cmd = mmio_read(self.hba_virt, port_off(self.port, PRT_CMD));
            mmio_write(self.hba_virt, port_off(self.port, PRT_CMD),
                cmd | CMD_FRE | CMD_ST);
        }
    }

    /// Issue command at slot 0 and poll PxCI for completion.
    fn run_slot0(&self) -> Result<(), &'static str> {
        unsafe {
            mmio_write(self.hba_virt, port_off(self.port, PRT_CI), 1);
            for _ in 0..1_000_000u32 {
                let ci = mmio_read(self.hba_virt, port_off(self.port, PRT_CI));
                if ci & 1 == 0 {
                    let tfd = mmio_read(self.hba_virt, port_off(self.port, PRT_TFD));
                    if tfd & 0x01 != 0 { return Err("ahci: ATA ERR"); }
                    return Ok(());
                }
                let tfd = mmio_read(self.hba_virt, port_off(self.port, PRT_TFD));
                if tfd & 0x01 != 0 { return Err("ahci: ATA ERR mid-cmd"); }
                core::hint::spin_loop();
            }
            Err("ahci: command timed out")
        }
    }

    /// Build a register H2D FIS at the start of the command table for
    /// slot 0; `lba` is in 512-byte sectors, `count` is sector count
    /// (0..65535), `cmd` is the ATA opcode.
    fn build_fis(&self, cmd: u8, lba: u64, count: u16) {
        unsafe {
            let cfis = self.ctbl_virt as *mut FisRegH2D;
            (*cfis).fis_type = FIS_TYPE_REG_H2D;
            (*cfis).pm_port  = 1 << 7; // command flag
            (*cfis).command  = cmd;
            (*cfis).featurel = 0;
            (*cfis).lba0 = (lba & 0xFF) as u8;
            (*cfis).lba1 = ((lba >> 8) & 0xFF) as u8;
            (*cfis).lba2 = ((lba >> 16) & 0xFF) as u8;
            (*cfis).device = 1 << 6; // LBA mode
            (*cfis).lba3 = ((lba >> 24) & 0xFF) as u8;
            (*cfis).lba4 = ((lba >> 32) & 0xFF) as u8;
            (*cfis).lba5 = ((lba >> 40) & 0xFF) as u8;
            (*cfis).featureh = 0;
            (*cfis).countl = (count & 0xFF) as u8;
            (*cfis).counth = ((count >> 8) & 0xFF) as u8;
            (*cfis).icc = 0;
            (*cfis).control = 0;
            (*cfis)._reserved = [0; 4];
        }
    }

    fn run_ata(&self, cmd: u8, lba: u64, count: u16,
               buf_phys: u64, buf_bytes: u32, write: bool)
        -> Result<(), &'static str>
    {
        // Populate command header: PRDTL = 1, CFL = 5 (FIS length / 4),
        // bit 6 = W (write).
        unsafe {
            let hdr = self.clist_virt as *mut CmdHeader;
            let mut flags: u16 = 5; // CFL = 5 dwords (20 bytes)
            if write { flags |= 1 << 6; }
            (*hdr).flags = flags;
            (*hdr).prdtl = 1;
            (*hdr).prdbc = 0;
            (*hdr).ctba = self.ctbl_phys as u32;
            (*hdr).ctbau = (self.ctbl_phys >> 32) as u32;

            // PRDT entry 0: physical address + byte count.
            let prdt = (self.ctbl_virt + 128) as *mut PrdtEntry;
            (*prdt).dba  = buf_phys as u32;
            (*prdt).dbau = (buf_phys >> 32) as u32;
            (*prdt)._reserved = 0;
            // bits 0..21 = bytes - 1.
            (*prdt).dbc = (buf_bytes - 1) & 0x3F_FFFF;
        }
        self.build_fis(cmd, lba, count);
        self.run_slot0()
    }

    pub fn read_sectors(&self, lba: u64, count: u16, out: &mut [u8])
        -> Result<(), &'static str>
    {
        if (count as usize) * 512 > out.len() {
            return Err("ahci: out buffer too small");
        }
        let bytes = (count as u32) * 512;
        let (data_virt, data_phys) = alloc_dma_contig(bytes as usize)
            .ok_or("ahci: DMA alloc failed")?;
        let res = self.run_ata(ATA_CMD_READ_DMA_EX, lba, count, data_phys, bytes, false);
        if res.is_ok() {
            unsafe {
                let src = core::slice::from_raw_parts(data_virt, bytes as usize);
                out[..bytes as usize].copy_from_slice(src);
            }
        }
        unsafe { crate::memory::dealloc_dma_contig(data_phys, bytes as usize); }
        res
    }

    pub fn write_sectors(&self, lba: u64, count: u16, data: &[u8])
        -> Result<(), &'static str>
    {
        let bytes = (count as u32) * 512;
        if data.len() < bytes as usize {
            return Err("ahci: data shorter than count");
        }
        let (buf_virt, buf_phys) = alloc_dma_contig(bytes as usize)
            .ok_or("ahci: DMA alloc failed")?;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(buf_virt, bytes as usize);
            dst.copy_from_slice(&data[..bytes as usize]);
        }
        let res = self.run_ata(ATA_CMD_WRITE_DMA_EX, lba, count, buf_phys, bytes, true);
        unsafe { crate::memory::dealloc_dma_contig(buf_phys, bytes as usize); }
        res
    }

    /// IDENTIFY DEVICE — 512-byte response describing the disk.
    /// Bytes 200..207 hold the 64-bit user-addressable LBA48 sector
    /// count (offset 100 in u16 words).
    fn identify(&mut self) -> Result<(), &'static str> {
        let (buf_virt, buf_phys) = alloc_dma_contig(512)
            .ok_or("ahci: identify DMA alloc")?;
        let res = self.run_ata(ATA_CMD_IDENTIFY, 0, 1, buf_phys, 512, false);
        if res.is_ok() {
            unsafe {
                // u64 LBA48 count at byte offset 200.
                let p = buf_virt.add(200) as *const u64;
                self.sectors = core::ptr::read_unaligned(p);
                if self.sectors == 0 {
                    // Fallback: 28-bit LBA at byte offset 120.
                    let p32 = buf_virt.add(120) as *const u32;
                    self.sectors = core::ptr::read_unaligned(p32) as u64;
                }
            }
        }
        unsafe { crate::memory::dealloc_dma_contig(buf_phys, 512); }
        res
    }
}

pub struct Ahci {
    pub hba_virt: usize,
    pub ports: Vec<AhciPort>,
}

unsafe impl Send for Ahci {}

impl Ahci {
    pub fn init() -> Result<Self, &'static str> {
        // PCI class 01:06:01 (SATA, AHCI).
        let devs = find_class(0x01, 0x06);
        let dev = devs.into_iter().find(|d| d.prog_if == 0x01)
            .ok_or("ahci: no AHCI controller (class 01:06:01) on PCI")?;
        enable_device(dev.addr);
        let (base, _size, is_io) = probe_bar(dev.addr, 5)
            .ok_or("ahci: BAR5 missing")?;
        if is_io { return Err("ahci: BAR5 is I/O, expected MMIO"); }
        let hba_virt = (phys_offset() + base) as usize;

        unsafe {
            // Set GHC.AE (bit 31) — leave HR (bit 0) alone since
            // QEMU's HBA is already in a usable state.
            let ghc = mmio_read(hba_virt, HBA_GHC);
            mmio_write(hba_virt, HBA_GHC, ghc | (1 << 31));

            let cap = mmio_read(hba_virt, HBA_CAP);
            let ports_impl = mmio_read(hba_virt, HBA_PI);
            let np = (cap & 0x1F) + 1;
            crate::klog_info!(
                "ahci: HBA @ {:#x} cap={:#x} pi={:#x} np={}", base, cap, ports_impl, np);

            let mut ports = Vec::new();
            for p in 0..32u32 {
                if ports_impl & (1 << p) == 0 { continue; }
                if let Ok(port) = Self::probe_port(hba_virt, p) {
                    ports.push(port);
                }
            }
            Ok(Ahci { hba_virt, ports })
        }
    }

    unsafe fn probe_port(hba_virt: usize, p: u32) -> Result<AhciPort, &'static str> {
        unsafe {
            // Wait for SSTS DET == 3 (device detected and PHY ready) +
            // IPM == 1 (active power state).  Skip if no device.
            let ssts = mmio_read(hba_virt, port_off(p, PRT_SSTS));
            let det = ssts & 0xF;
            let ipm = (ssts >> 8) & 0xF;
            if det != 3 || ipm != 1 { return Err("port: no device"); }
            let sig = mmio_read(hba_virt, port_off(p, PRT_SIG));
            if sig != SIG_SATA { return Err("port: not SATA disk"); }

            // Allocate command list (1 KiB), FIS receive area (256 B),
            // command table (256 B + PRDT).  All in one DMA region for
            // simplicity, sized at 4 KiB.
            let (region_virt, region_phys) = alloc_dma_contig(4096)
                .ok_or("ahci: per-port DMA alloc failed")?;
            let region_virt = region_virt as usize;
            let clist_virt = region_virt;
            let clist_phys = region_phys;
            let fis_virt   = region_virt + 0x400;
            let fis_phys   = region_phys + 0x400;
            let ctbl_virt  = region_virt + 0x500;
            let ctbl_phys  = region_phys + 0x500;

            // Stop the port before re-pointing CLB/FB.
            let cmd = mmio_read(hba_virt, port_off(p, PRT_CMD));
            mmio_write(hba_virt, port_off(p, PRT_CMD), cmd & !(CMD_ST | CMD_FRE));
            for _ in 0..500_000u32 {
                let c = mmio_read(hba_virt, port_off(p, PRT_CMD));
                if c & (CMD_FR | CMD_CR) == 0 { break; }
            }

            // Program command list and FIS receive area.
            mmio_write(hba_virt, port_off(p, PRT_CLB),  clist_phys as u32);
            mmio_write(hba_virt, port_off(p, PRT_CLBU), (clist_phys >> 32) as u32);
            mmio_write(hba_virt, port_off(p, PRT_FB),   fis_phys as u32);
            mmio_write(hba_virt, port_off(p, PRT_FBU),  (fis_phys >> 32) as u32);

            // Clear SERR (write-1-to-clear all bits) and IS.
            mmio_write(hba_virt, port_off(p, PRT_SERR), 0xFFFF_FFFF);
            mmio_write(hba_virt, port_off(p, PRT_IS),   0xFFFF_FFFF);
            // Disable interrupts — we poll.
            mmio_write(hba_virt, port_off(p, PRT_IE),   0);

            // Point command header 0 at our command table.
            let hdr = clist_virt as *mut CmdHeader;
            (*hdr).flags = 5;
            (*hdr).prdtl = 0;
            (*hdr).prdbc = 0;
            (*hdr).ctba  = ctbl_phys as u32;
            (*hdr).ctbau = (ctbl_phys >> 32) as u32;
            for i in 0..4 { (*hdr)._reserved[i] = 0; }

            let mut port = AhciPort {
                hba_virt, port: p,
                clist_virt, clist_phys,
                fis_virt, fis_phys,
                ctbl_virt, ctbl_phys,
                sectors: 0,
            };
            // Start FIS receive + command list processing.
            port.start();
            // IDENTIFY for capacity.
            port.identify()?;
            crate::klog_info!(
                "ahci: port {} SATA disk, {} sectors ({} MiB)",
                p, port.sectors, port.sectors / 2048);
            // Suppress unused-frame warning.
            let _ = PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
                x86_64::PhysAddr::new(region_phys));
            Ok(port)
        }
    }
}

pub static AHCI: Mutex<Option<Ahci>> = Mutex::new(None);
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    match Ahci::init() {
        Ok(d) => {
            *AHCI.lock() = Some(d);
            AVAILABLE.store(true, Ordering::Release);
        }
        Err(e) => crate::klog_warn!("ahci: not initialised: {}", e),
    }
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }
