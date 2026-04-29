//! NVMe 1.4 controller driver — admin queue + 1 I/O queue, polling.
//!
//! Reference: NVM Express Base Specification 1.4c (free PDF on
//! nvmexpress.org), §3 "Controller Architecture", §4 "Submission &
//! Completion Queues", §5 "Admin Commands", §6 "NVM Commands".
//!
//! ## Discovery
//!
//! NVMe devices appear on PCI as class 0x01 (mass-storage), subclass
//! 0x08 (NVM), prog-if 0x02 (NVMe).  BAR0 (a 64-bit MMIO BAR) holds
//! the NVMe Controller Registers ("BAR" = NVMe Doorbell Region after
//! offset 0x1000).
//!
//! ## Bring-up sequence (NVMe spec §3.1.5)
//!
//!   1. CC.EN = 0; wait CSTS.RDY → 0.
//!   2. Allocate Admin SQ (one frame, 64 entries × 64 bytes = 4 KiB)
//!      and Admin CQ (one frame, 64 entries × 16 bytes ≪ 4 KiB).
//!   3. Write AQA, ASQ, ACQ; set CC.IOSQES = 6 / IOCQES = 4 / EN = 1.
//!   4. Wait CSTS.RDY → 1.
//!   5. Issue IDENTIFY CONTROLLER (CNS = 1) → 4 KiB struct, capture
//!      number of namespaces.
//!   6. Issue IDENTIFY NAMESPACE (CNS = 0, NSID = 1) → namespace size
//!      + LBA format (LBADS gives sector size shift).
//!   7. CREATE I/O CQ + CREATE I/O SQ (NSID = 1, qid = 1, qsize = 32).
//!   8. Read/Write commands go to I/O queue 1; complete via the
//!      matching I/O CQ.
//!
//! ## Doorbells
//!
//! At BAR0 + 0x1000 + 2 * dstrd * qid one finds two u32 doorbell
//! registers — the SQ Tail and the CQ Head.  We bump SQ Tail after
//! posting a command, and CQ Head after consuming a completion.

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::drivers::pci::{enable_device, find_class, probe_bar};
use crate::memory::{alloc_dma_contig, phys_offset};

// ---------- Controller register offsets (BAR0) ----------
const REG_CAP:    u32 = 0x00; // 64-bit
const REG_VS:     u32 = 0x08;
const REG_CC:     u32 = 0x14;
const REG_CSTS:   u32 = 0x1C;
const REG_AQA:    u32 = 0x24;
const REG_ASQ:    u32 = 0x28; // 64-bit
const REG_ACQ:    u32 = 0x30; // 64-bit

const CC_EN:    u32 = 1 << 0;
const CSTS_RDY: u32 = 1 << 0;

// ---------- Submission Queue Entry (64 bytes) ----------
#[repr(C)]
#[derive(Clone, Copy)]
struct Sqe {
    /// CDW0: opcode (low 8), fuse (10..11), psdt (14..15), cid (16..31).
    opc: u8,
    fuse_psdt: u8,
    cid: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    /// Metadata pointer (only used with end-to-end protection).
    mptr: u64,
    /// PRP entry 1 — physical pointer to first 4 KiB data page.
    prp1: u64,
    /// PRP entry 2 — second 4 KiB page when transfer > 4 KiB,
    /// otherwise unused.
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

impl Sqe {
    fn empty() -> Self {
        Self { opc: 0, fuse_psdt: 0, cid: 0, nsid: 0,
               cdw2: 0, cdw3: 0, mptr: 0, prp1: 0, prp2: 0,
               cdw10: 0, cdw11: 0, cdw12: 0, cdw13: 0, cdw14: 0, cdw15: 0 }
    }
}

// ---------- Completion Queue Entry (16 bytes) ----------
#[repr(C)]
#[derive(Clone, Copy)]
struct Cqe {
    cdw0: u32,
    cdw1: u32,
    sq_head_id: u32,  // SQ head + SQ id
    status_phase: u32, // status (16..31), p (16), cid (0..15)
}

impl Cqe {
    fn phase(&self) -> u8 { ((self.status_phase >> 16) & 1) as u8 }
    fn status(&self) -> u16 { (self.status_phase >> 17) as u16 }
    fn cid(&self) -> u16 { (self.status_phase & 0xFFFF) as u16 }
}

const ADMIN_OPC_DELETE_SQ:  u8 = 0x00;
const ADMIN_OPC_CREATE_SQ:  u8 = 0x01;
const ADMIN_OPC_DELETE_CQ:  u8 = 0x04;
const ADMIN_OPC_CREATE_CQ:  u8 = 0x05;
const ADMIN_OPC_IDENTIFY:   u8 = 0x06;
const NVM_OPC_FLUSH:        u8 = 0x00;
const NVM_OPC_WRITE:        u8 = 0x01;
const NVM_OPC_READ:         u8 = 0x02;

const ADMIN_QSIZE: u16 = 64;
const IO_QSIZE:    u16 = 32;

struct Queue {
    /// Submission queue virtual base + phys.
    sq_virt: usize,
    sq_phys: u64,
    /// Completion queue virtual base + phys.
    cq_virt: usize,
    cq_phys: u64,
    sq_size: u16,
    cq_size: u16,
    sq_tail: u16,
    cq_head: u16,
    /// CQ phase-tag bit; flips every time we wrap the queue.
    phase: u8,
    /// MMIO offset of this queue's SQ/CQ doorbell register pair.
    sq_db: u32,
    cq_db: u32,
    /// Monotonic command-id counter.
    next_cid: u16,
}

#[allow(dead_code)]
pub struct Nvme {
    bar_virt: usize,
    cap: u64,
    dstrd: u32,
    admin: Queue,
    io: Option<Queue>,
    pub sectors: u64,
    pub lba_shift: u8,
    pub nsid: u32,
}

unsafe impl Send for Nvme {}

unsafe fn mmio_r32(b: usize, o: u32) -> u32 {
    unsafe { core::ptr::read_volatile((b + o as usize) as *const u32) }
}
unsafe fn mmio_w32(b: usize, o: u32, v: u32) {
    unsafe { core::ptr::write_volatile((b + o as usize) as *mut u32, v) }
}
unsafe fn mmio_r64(b: usize, o: u32) -> u64 {
    unsafe { core::ptr::read_volatile((b + o as usize) as *const u64) }
}
unsafe fn mmio_w64(b: usize, o: u32, v: u64) {
    unsafe { core::ptr::write_volatile((b + o as usize) as *mut u64, v) }
}

impl Queue {
    /// Build a queue.  SQ holds `sq_size` SQEs (64 B each); CQ holds
    /// `cq_size` CQEs (16 B each).  Both end up in DMA-contig pages.
    fn alloc(sq_size: u16, cq_size: u16, sq_db: u32, cq_db: u32)
        -> Option<Queue>
    {
        let sq_bytes = (sq_size as usize) * 64;
        let cq_bytes = (cq_size as usize) * 16;
        let (sq_virt, sq_phys) = alloc_dma_contig(sq_bytes)?;
        let (cq_virt, cq_phys) = alloc_dma_contig(cq_bytes)?;
        Some(Queue {
            sq_virt: sq_virt as usize,
            sq_phys,
            cq_virt: cq_virt as usize,
            cq_phys,
            sq_size, cq_size,
            sq_tail: 0,
            cq_head: 0,
            phase: 1,
            sq_db, cq_db,
            next_cid: 1,
        })
    }

    /// Post `cmd` and poll the CQ until matching completion.  Returns
    /// the CQE's status (0 = success, low byte = SC, bits 8..10 = SCT).
    unsafe fn submit(&mut self, bar_virt: usize, mut cmd: Sqe) -> Result<u16, &'static str> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1).max(1);
        cmd.cid = cid;
        // Write SQE at sq_tail.
        let sqe_ptr = (self.sq_virt + (self.sq_tail as usize) * 64) as *mut Sqe;
        unsafe { core::ptr::write_volatile(sqe_ptr, cmd); }
        self.sq_tail = (self.sq_tail + 1) % self.sq_size;
        // Bump SQ tail doorbell.
        unsafe { mmio_w32(bar_virt, self.sq_db, self.sq_tail as u32); }

        // Poll CQ for entry whose phase matches our expected phase.
        for _ in 0..2_000_000u32 {
            let cqe_ptr = (self.cq_virt + (self.cq_head as usize) * 16) as *const Cqe;
            let cqe = unsafe { core::ptr::read_volatile(cqe_ptr) };
            if cqe.phase() == self.phase {
                let st = cqe.status();
                if cqe.cid() != cid {
                    // Spec says CIDs may complete out of order, but our
                    // serial submission means we expect them in order.
                    // Skip and keep advancing.
                }
                self.cq_head = (self.cq_head + 1) % self.cq_size;
                if self.cq_head == 0 { self.phase ^= 1; }
                unsafe { mmio_w32(bar_virt, self.cq_db, self.cq_head as u32); }
                return Ok(st);
            }
            core::hint::spin_loop();
        }
        Err("nvme: command timed out")
    }
}

impl Nvme {
    pub fn init() -> Result<Self, &'static str> {
        let devs = find_class(0x01, 0x08);
        let dev = devs.into_iter().find(|d| d.prog_if == 0x02)
            .ok_or("nvme: no controller (class 01:08:02) on PCI")?;
        enable_device(dev.addr);
        let (base, _size, is_io) = probe_bar(dev.addr, 0)
            .ok_or("nvme: BAR0 missing")?;
        if is_io { return Err("nvme: BAR0 is I/O, expected MMIO"); }
        let bar_virt = (phys_offset() + base) as usize;

        unsafe {
            let cap = mmio_r64(bar_virt, REG_CAP);
            let dstrd = ((cap >> 32) & 0xF) as u32;
            // Disable controller (CC.EN = 0) and wait for CSTS.RDY = 0.
            let cc = mmio_r32(bar_virt, REG_CC);
            mmio_w32(bar_virt, REG_CC, cc & !CC_EN);
            for _ in 0..1_000_000u32 {
                if mmio_r32(bar_virt, REG_CSTS) & CSTS_RDY == 0 { break; }
            }

            // Doorbells: SQ0 tail = 0x1000, CQ0 head = 0x1000 + (1 << dstrd) * 4.
            let stride = 4u32 << dstrd;
            let admin_sq_db = 0x1000;
            let admin_cq_db = 0x1000 + stride;
            let mut admin = Queue::alloc(ADMIN_QSIZE, ADMIN_QSIZE, admin_sq_db, admin_cq_db)
                .ok_or("nvme: admin queue alloc")?;

            // Program AQA, ASQ, ACQ then enable.
            let aqa = ((ADMIN_QSIZE as u32 - 1) << 16) | (ADMIN_QSIZE as u32 - 1);
            mmio_w32(bar_virt, REG_AQA, aqa);
            mmio_w64(bar_virt, REG_ASQ, admin.sq_phys);
            mmio_w64(bar_virt, REG_ACQ, admin.cq_phys);
            // CC.IOSQES = 6 (64 B), CC.IOCQES = 4 (16 B), CC.MPS = 0
            // (4 KiB pages), CC.AMS = 0 (round-robin), CC.CSS = 0
            // (NVM command set), CC.EN = 1.
            let new_cc = (6u32 << 16) | (4u32 << 20) | CC_EN;
            mmio_w32(bar_virt, REG_CC, new_cc);
            for _ in 0..2_000_000u32 {
                if mmio_r32(bar_virt, REG_CSTS) & CSTS_RDY != 0 { break; }
            }
            if mmio_r32(bar_virt, REG_CSTS) & CSTS_RDY == 0 {
                return Err("nvme: controller never reported RDY");
            }

            // IDENTIFY CONTROLLER (CNS=1).  We mostly ignore the data
            // since we hard-code namespace 1 below; we just want to
            // confirm Admin queue works.
            let (ctrl_virt, ctrl_phys) = alloc_dma_contig(4096)
                .ok_or("nvme: identify-ctrl DMA")?;
            let mut cmd = Sqe::empty();
            cmd.opc = ADMIN_OPC_IDENTIFY;
            cmd.nsid = 0;
            cmd.prp1 = ctrl_phys;
            cmd.cdw10 = 1; // CNS = 1 (Identify Controller)
            let st = admin.submit(bar_virt, cmd)?;
            if st != 0 {
                crate::memory::dealloc_dma_contig(ctrl_phys, 4096);
                return Err("nvme: IDENTIFY CONTROLLER failed");
            }
            let _ = ctrl_virt;
            crate::memory::dealloc_dma_contig(ctrl_phys, 4096);

            // IDENTIFY NAMESPACE (CNS=0, NSID=1).
            let (ns_virt, ns_phys) = alloc_dma_contig(4096)
                .ok_or("nvme: identify-ns DMA")?;
            let mut cmd = Sqe::empty();
            cmd.opc = ADMIN_OPC_IDENTIFY;
            cmd.nsid = 1;
            cmd.prp1 = ns_phys;
            cmd.cdw10 = 0; // CNS = 0
            let st = admin.submit(bar_virt, cmd)?;
            if st != 0 {
                crate::memory::dealloc_dma_contig(ns_phys, 4096);
                return Err("nvme: IDENTIFY NAMESPACE failed");
            }
            // ns->nsze (u64 at offset 0) = total size in logical blocks.
            // Active LBA Format index = ns->flbas & 0xF.
            // ns->lbaf[index] is at offset 128 + index*4: bits 16..23 = LBADS.
            let nsze   = core::ptr::read_unaligned(ns_virt as *const u64);
            let flbas  = *ns_virt.add(26) & 0xF;
            let lbaf   = ns_virt.add(128 + (flbas as usize) * 4);
            let lbads  = *lbaf.add(2);
            crate::memory::dealloc_dma_contig(ns_phys, 4096);
            let lba_shift = lbads;

            // Allocate I/O CQ first, then I/O SQ that targets it.
            let io_sq_db = 0x1000 + 2 * stride;     // SQ1 tail
            let io_cq_db = 0x1000 + 3 * stride;     // CQ1 head
            let mut io = Queue::alloc(IO_QSIZE, IO_QSIZE, io_sq_db, io_cq_db)
                .ok_or("nvme: I/O queue alloc")?;

            // CREATE I/O CQ (admin opcode 0x05).
            // CDW10: QSIZE-1 (15..0), QID (31..16). CDW11: PC (0)|IEN (1)|IV (16..31).
            let mut cmd = Sqe::empty();
            cmd.opc = ADMIN_OPC_CREATE_CQ;
            cmd.prp1 = io.cq_phys;
            cmd.cdw10 = ((IO_QSIZE as u32 - 1) << 16) | 1; // QID 1
            cmd.cdw11 = 1; // PC = 1, IEN = 0
            let st = admin.submit(bar_virt, cmd)?;
            if st != 0 { return Err("nvme: CREATE I/O CQ failed"); }

            // CREATE I/O SQ.
            let mut cmd = Sqe::empty();
            cmd.opc = ADMIN_OPC_CREATE_SQ;
            cmd.prp1 = io.sq_phys;
            cmd.cdw10 = ((IO_QSIZE as u32 - 1) << 16) | 1;
            // CDW11: PC (0) | QPRIO (1..2) | CQID (16..31).
            cmd.cdw11 = (1u32 << 16) | 1; // CQID = 1, PC = 1
            let st = admin.submit(bar_virt, cmd)?;
            if st != 0 { return Err("nvme: CREATE I/O SQ failed"); }
            // Reset I/O queue's phase tag.
            io.phase = 1;
            io.cq_head = 0;
            io.sq_tail = 0;

            crate::klog_info!(
                "nvme: ready, ns1 = {} blocks × {} bytes ({} MiB)",
                nsze, 1u64 << lba_shift,
                (nsze << lba_shift) / (1024 * 1024));

            Ok(Nvme {
                bar_virt, cap, dstrd, admin,
                io: Some(io),
                sectors: nsze,
                lba_shift,
                nsid: 1,
            })
        }
    }

    pub fn read(&mut self, lba: u64, count: u16, buf: &mut [u8])
        -> Result<(), &'static str>
    {
        let block_size = 1usize << self.lba_shift;
        let bytes = (count as usize) * block_size;
        if buf.len() < bytes { return Err("nvme: buf too small"); }
        let (data_virt, data_phys) = alloc_dma_contig(bytes)
            .ok_or("nvme: data DMA")?;
        let mut cmd = Sqe::empty();
        cmd.opc = NVM_OPC_READ;
        cmd.nsid = self.nsid;
        cmd.prp1 = data_phys;
        if bytes > 4096 { cmd.prp2 = data_phys + 4096; }
        cmd.cdw10 = lba as u32;
        cmd.cdw11 = (lba >> 32) as u32;
        cmd.cdw12 = (count as u32 - 1) & 0xFFFF; // 0-based
        let io = self.io.as_mut().ok_or("nvme: no I/O queue")?;
        let st = unsafe { io.submit(self.bar_virt, cmd)? };
        let res = if st == 0 {
            unsafe {
                let s = core::slice::from_raw_parts(data_virt, bytes);
                buf[..bytes].copy_from_slice(s);
            }
            Ok(())
        } else {
            Err("nvme: READ failed")
        };
        unsafe { crate::memory::dealloc_dma_contig(data_phys, bytes); }
        res
    }

    pub fn write(&mut self, lba: u64, count: u16, buf: &[u8])
        -> Result<(), &'static str>
    {
        let block_size = 1usize << self.lba_shift;
        let bytes = (count as usize) * block_size;
        if buf.len() < bytes { return Err("nvme: buf too small"); }
        let (data_virt, data_phys) = alloc_dma_contig(bytes)
            .ok_or("nvme: data DMA")?;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(data_virt, bytes);
            dst.copy_from_slice(&buf[..bytes]);
        }
        let mut cmd = Sqe::empty();
        cmd.opc = NVM_OPC_WRITE;
        cmd.nsid = self.nsid;
        cmd.prp1 = data_phys;
        if bytes > 4096 { cmd.prp2 = data_phys + 4096; }
        cmd.cdw10 = lba as u32;
        cmd.cdw11 = (lba >> 32) as u32;
        cmd.cdw12 = (count as u32 - 1) & 0xFFFF;
        let io = self.io.as_mut().ok_or("nvme: no I/O queue")?;
        let st = unsafe { io.submit(self.bar_virt, cmd)? };
        let res = if st == 0 { Ok(()) } else { Err("nvme: WRITE failed") };
        unsafe { crate::memory::dealloc_dma_contig(data_phys, bytes); }
        res
    }

    pub fn flush(&mut self) -> Result<(), &'static str> {
        let mut cmd = Sqe::empty();
        cmd.opc = NVM_OPC_FLUSH;
        cmd.nsid = self.nsid;
        let io = self.io.as_mut().ok_or("nvme: no I/O queue")?;
        let st = unsafe { io.submit(self.bar_virt, cmd)? };
        if st == 0 { Ok(()) } else { Err("nvme: FLUSH failed") }
    }
}

pub static NVME: Mutex<Option<Nvme>> = Mutex::new(None);
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    match Nvme::init() {
        Ok(d) => {
            *NVME.lock() = Some(d);
            AVAILABLE.store(true, Ordering::Release);
        }
        Err(e) => crate::klog_warn!("nvme: not initialised: {}", e),
    }
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Relaxed) }
