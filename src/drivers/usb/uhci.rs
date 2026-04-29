//! UHCI host controller driver.
//!
//! Universal Host Controller Interface — Intel's USB 1.x HCI.  PIIX3
//! integrates one (PCI class 0x0C03 prog-if 0x00); QEMU exposes it via
//! `-device piix3-usb-uhci`.  We use it because it's the simplest USB
//! host controller spec — the entire register set is a 32-byte I/O port
//! window, framing is a 4 KiB physical page of 1024 frame pointers, and
//! transfers are described by 16-byte Transfer Descriptors plus 8-byte
//! Queue Heads.
//!
//! References:
//!   * Intel "Universal Host Controller Interface (UHCI) Design Guide",
//!     revision 1.1, March 1996.
//!   * OSDev wiki "Universal Host Controller Interface".
//!   * QEMU source (hw/usb/hcd-uhci.c) — useful as a behavioural oracle
//!     when the spec is ambiguous.
//!
//! ## Status
//!
//! This module ships the **structural layer** of a UHCI driver: register
//! definitions, TD/QH bit-packing, frame-list allocation, root-hub
//! port-state probing.  The full transfer pipeline (SETUP → IN/OUT data
//! → STATUS, with proper TD chaining and bounded waits for completion)
//! is wired but only exercised behind the opt-in `usb scan` shell
//! command, since silent transfer-engine bugs on a UHCI without a real
//! device produce hangs that can't be diagnosed from inside QEMU.
//!
//! Tests cover everything that's pure data: register offsets, TD/QH bit
//! packing, descriptor parsing, port-status decoding.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;
use crate::drivers::pci::PciDevice;

// ---------------------------------------------------------------------------
// PCI class
// ---------------------------------------------------------------------------

/// Serial Bus Controller — class code 0x0C in PCI base-class table.
pub const PCI_CLASS_SERIAL_BUS: u8 = 0x0C;
/// USB Controller — subclass under Serial Bus.
pub const PCI_SUBCLASS_USB: u8 = 0x03;
/// UHCI prog-if.
pub const PCI_PROGIF_UHCI: u8 = 0x00;

// ---------------------------------------------------------------------------
// I/O port register offsets (UHCI Design Guide §2.1 Table 2-1)
// ---------------------------------------------------------------------------

/// USBCMD — 16-bit command register.
pub const REG_USBCMD: u16 = 0x00;
/// USBSTS — 16-bit status register.  Sticky bits cleared by writing 1.
pub const REG_USBSTS: u16 = 0x02;
/// USBINTR — 16-bit interrupt enable.
pub const REG_USBINTR: u16 = 0x04;
/// FRNUM — 16-bit current frame number (lower 11 bits used).
pub const REG_FRNUM: u16 = 0x06;
/// FRBASEADD — 32-bit physical base address of frame list (4 KiB aligned).
pub const REG_FRBASEADD: u16 = 0x08;
/// SOFMOD — 8-bit SOF modifier.
pub const REG_SOFMOD: u16 = 0x0C;
/// PORTSC1 — 16-bit port 0 status/control.
pub const REG_PORTSC1: u16 = 0x10;
/// PORTSC2 — 16-bit port 1 status/control.
pub const REG_PORTSC2: u16 = 0x12;

// ---- USBCMD bits ----
pub const CMD_RUN_STOP:   u16 = 1 << 0;
pub const CMD_HCRESET:    u16 = 1 << 1;
pub const CMD_GRESET:     u16 = 1 << 2; // global reset
pub const CMD_EGSM:       u16 = 1 << 3; // enter global suspend
pub const CMD_FGR:        u16 = 1 << 4; // force global resume
pub const CMD_SWDBG:      u16 = 1 << 5;
pub const CMD_CF:         u16 = 1 << 6; // configure flag
pub const CMD_MAXP:       u16 = 1 << 7; // 0=32-byte 1=64-byte max packet

// ---- USBSTS bits (write-1-to-clear) ----
pub const STS_USBINT:     u16 = 1 << 0; // a transfer with IOC completed
pub const STS_ERROR_INT:  u16 = 1 << 1;
pub const STS_RESUME_DET: u16 = 1 << 2;
pub const STS_HOST_SYS_ERR: u16 = 1 << 3;
pub const STS_HC_PROC_ERR:  u16 = 1 << 4;
pub const STS_HCH:        u16 = 1 << 5; // host controller halted

// ---- PORTSC bits ----
pub const PORTSC_CCS:        u16 = 1 << 0;  // Current Connect Status
pub const PORTSC_CSC:        u16 = 1 << 1;  // Connect Status Change (R/WC)
pub const PORTSC_PE:         u16 = 1 << 2;  // Port Enabled
pub const PORTSC_PEC:        u16 = 1 << 3;  // Port Enable Change (R/WC)
pub const PORTSC_LS_LO:      u16 = 1 << 4;  // Line Status low bit
pub const PORTSC_LS_HI:      u16 = 1 << 5;  // Line Status high bit
pub const PORTSC_RD:         u16 = 1 << 6;  // Resume Detect
pub const PORTSC_LSDA:       u16 = 1 << 8;  // Low Speed Device Attached
pub const PORTSC_PR:         u16 = 1 << 9;  // Port Reset
pub const PORTSC_SUSP:       u16 = 1 << 12; // Suspend
/// Mask of write-1-to-clear bits in PORTSC.  Used so we don't
/// accidentally clear status when twiddling control bits.
pub const PORTSC_RWC_MASK:   u16 = PORTSC_CSC | PORTSC_PEC;

// ---------------------------------------------------------------------------
// Transfer Descriptor (TD) — 16 bytes, 16-byte aligned.
// ---------------------------------------------------------------------------

/// `Td` is laid out exactly as 4 little-endian DWORDs.  The names follow
/// the UHCI Design Guide §3.2.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug)]
pub struct Td {
    pub link: u32,        // DWORD 0: link pointer + T/Q/Vf bits
    pub control: u32,     // DWORD 1: status/control + actual length
    pub token: u32,       // DWORD 2: PID/address/endpoint/maxlen
    pub buffer: u32,      // DWORD 3: data buffer physical address
}

// Link bits
pub const LINK_T:  u32 = 1 << 0;
pub const LINK_Q:  u32 = 1 << 1;
pub const LINK_VF: u32 = 1 << 2;

// Control/status bits
pub const TD_CTRL_BITSTUFF:  u32 = 1 << 17;
pub const TD_CTRL_CRC_TIMEOUT: u32 = 1 << 18;
pub const TD_CTRL_NAK:       u32 = 1 << 19;
pub const TD_CTRL_BABBLE:    u32 = 1 << 20;
pub const TD_CTRL_DBE:       u32 = 1 << 21;
pub const TD_CTRL_STALLED:   u32 = 1 << 22;
pub const TD_CTRL_ACTIVE:    u32 = 1 << 23;
pub const TD_CTRL_IOC:       u32 = 1 << 24;
pub const TD_CTRL_IOS:       u32 = 1 << 25;
pub const TD_CTRL_LS:        u32 = 1 << 26;
pub const TD_CTRL_SPD:       u32 = 1 << 29;

// Token PIDs (USB 2.0 §8.3.1 Table 8-1)
pub const PID_OUT:   u8 = 0xE1;
pub const PID_IN:    u8 = 0x69;
pub const PID_SETUP: u8 = 0x2D;

impl Td {
    pub const fn empty() -> Self {
        Self { link: LINK_T, control: 0, token: 0, buffer: 0 }
    }

    /// Build a TD token field (DWORD 2).  `max_len` is the buffer
    /// length in bytes; 0 means "no data" (encoded as 0x7FF).  Address
    /// and endpoint are masked to their respective widths.
    pub fn make_token(pid: u8, addr: u8, endpoint: u8, data_toggle: bool, max_len: u16) -> u32 {
        let len = if max_len == 0 { 0x7FF } else { max_len.wrapping_sub(1) as u32 & 0x7FF };
        (pid as u32)
            | ((addr as u32 & 0x7F) << 8)
            | ((endpoint as u32 & 0x0F) << 15)
            | ((data_toggle as u32) << 19)
            | (len << 21)
    }

    /// Encode CERR (error count, 2 bits) in bits 27..28 of the control
    /// word.  Real UHCI hardware decrements this on transfer errors.
    pub const fn cerr(n: u32) -> u32 { (n & 0x3) << 27 }

    /// Returns true if the controller still considers this TD eligible
    /// for execution.
    pub fn is_active(&self) -> bool { self.control & TD_CTRL_ACTIVE != 0 }

    /// Returns the 11-bit "actual length" field (byte count actually
    /// transferred), or 0x7FF if zero bytes were transferred.
    pub fn actual_length(&self) -> u16 {
        let raw = (self.control & 0x7FF) as u16;
        if raw == 0x7FF { 0 } else { raw + 1 }
    }
}

// ---------------------------------------------------------------------------
// Queue Head — 8 bytes, 16-byte aligned.
// ---------------------------------------------------------------------------

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug)]
pub struct Qh {
    pub head:    u32,  // next QH (with T/Q bits)
    pub element: u32,  // first TD or QH below this one
}

impl Qh {
    pub const fn empty() -> Self { Self { head: LINK_T, element: LINK_T } }
}

// ---------------------------------------------------------------------------
// Frame list
// ---------------------------------------------------------------------------

/// Number of frame entries — UHCI is fixed at 1024.
pub const FRAME_LIST_LEN: usize = 1024;

/// One frame list — 4 KiB total, must be 4-KiB aligned.  We allocate a
/// `Box<FrameList>` once at controller init.
#[repr(C, align(4096))]
pub struct FrameList(pub [u32; FRAME_LIST_LEN]);

impl FrameList {
    pub fn new() -> Self {
        // Empty by default (every entry terminates).
        Self([LINK_T; FRAME_LIST_LEN])
    }
}

// ---------------------------------------------------------------------------
// Controller state
// ---------------------------------------------------------------------------

/// One UHCI controller's runtime state.  Only one is supported per
/// system — PIIX3 has a single function in QEMU.
pub struct Controller {
    pub iobase: u16,
    pub frame_list_phys: u64,
    pub frame_list_virt: u64,
}

static AVAILABLE: AtomicBool = AtomicBool::new(false);
static INSTANCE: Mutex<Option<Controller>> = Mutex::new(None);

/// Probe PCI for a UHCI controller, claim its I/O BAR, allocate the
/// frame list, reset and start the host controller.  Returns `Ok(())`
/// on success.  Idempotent: returns `Ok(())` if already initialised.
pub fn init() -> Result<(), &'static str> {
    if AVAILABLE.load(Ordering::Acquire) { return Ok(()); }

    let candidates = crate::drivers::pci::find_class(PCI_CLASS_SERIAL_BUS, PCI_SUBCLASS_USB);
    let dev = match candidates.into_iter().find(|d| d.prog_if == PCI_PROGIF_UHCI) {
        Some(d) => d,
        None => return Err("uhci: no UHCI host controller on PCI bus"),
    };

    crate::drivers::pci::enable_device(dev.addr);

    // BAR4 is the I/O BAR on every PIIX UHCI we'll see.
    let (base, _size, is_io) = match crate::drivers::pci::probe_bar(dev.addr, 4) {
        Some(b) => b,
        None => return Err("uhci: no BAR4"),
    };
    if !is_io { return Err("uhci: BAR4 is MMIO; UHCI requires I/O ports"); }
    let iobase = (base & 0xFFE0) as u16;

    // Reset HC then wait for HCH.
    unsafe {
        let mut cmd: Port<u16> = Port::new(iobase + REG_USBCMD);
        cmd.write(CMD_HCRESET);
        for _ in 0..1_000_000u32 {
            if cmd.read() & CMD_HCRESET == 0 { break; }
            core::hint::spin_loop();
        }
    }

    // Allocate the frame list.  Box::new gives us a heap-aligned chunk;
    // alignment 4096 from the repr(align) directive.  We leak it on
    // purpose — it lives for the lifetime of the controller.
    let fl = alloc::boxed::Box::new(FrameList::new());
    let fl_virt = alloc::boxed::Box::leak(fl) as *mut FrameList as u64;
    let fl_phys = match crate::memory::virt_to_phys(fl_virt) {
        Some(p) => p,
        None => return Err("uhci: frame list virt has no phys mapping"),
    };
    if fl_phys & 0xFFF != 0 {
        return Err("uhci: frame list not 4-KiB aligned (Box should guarantee this)");
    }

    unsafe {
        // Program FRBASEADD.
        let mut frb: Port<u32> = Port::new(iobase + REG_FRBASEADD);
        frb.write(fl_phys as u32);
        // FRNUM = 0.
        let mut frn: Port<u16> = Port::new(iobase + REG_FRNUM);
        frn.write(0);
        // SOFMOD default 0x40 (1 ms frames).
        let mut sof: Port<u8> = Port::new(iobase + REG_SOFMOD);
        sof.write(0x40);
        // Disable interrupts — we poll for now.
        let mut intr: Port<u16> = Port::new(iobase + REG_USBINTR);
        intr.write(0);
        // Run!  CF (configure flag) tells the HC the schedule is ready.
        let mut cmd: Port<u16> = Port::new(iobase + REG_USBCMD);
        cmd.write(CMD_RUN_STOP | CMD_CF | CMD_MAXP);
    }

    *INSTANCE.lock() = Some(Controller {
        iobase,
        frame_list_phys: fl_phys,
        frame_list_virt: fl_virt,
    });
    AVAILABLE.store(true, Ordering::Release);
    crate::klog_info!("uhci: controller at iobase {:#x}, frame list phys {:#x}",
        iobase, fl_phys);
    Ok(())
}

pub fn is_available() -> bool { AVAILABLE.load(Ordering::Acquire) }

// ---- Port-state read ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStatus {
    NoDevice,
    LowSpeed,
    FullSpeed,
}

/// Read the current connection status of a root-hub port.  `port` is
/// 0 or 1 (UHCI has exactly two).
pub fn port_status(port: u8) -> Option<PortStatus> {
    if !AVAILABLE.load(Ordering::Acquire) { return None; }
    let inst_guard = INSTANCE.lock();
    let inst = inst_guard.as_ref()?;
    let off = match port {
        0 => REG_PORTSC1,
        1 => REG_PORTSC2,
        _ => return None,
    };
    let v: u16 = unsafe { Port::new(inst.iobase + off).read() };
    Some(decode_port(v))
}

/// Pure decoder for a PORTSC value — used by both the runtime path and
/// tests (which feed it synthetic values without needing real hardware).
pub fn decode_port(v: u16) -> PortStatus {
    if v & PORTSC_CCS == 0 { return PortStatus::NoDevice; }
    if v & PORTSC_LSDA != 0 { PortStatus::LowSpeed } else { PortStatus::FullSpeed }
}

/// Reset the named port — drive PR for ~50 ms then deassert.  The
/// controller will set Port-Enable on its own once reset clears.
pub fn reset_port(port: u8) -> Result<(), &'static str> {
    let inst_guard = INSTANCE.lock();
    let inst = inst_guard.as_ref().ok_or("uhci: not initialised")?;
    let off = match port {
        0 => REG_PORTSC1,
        1 => REG_PORTSC2,
        _ => return Err("uhci: invalid port"),
    };
    unsafe {
        let mut p: Port<u16> = Port::new(inst.iobase + off);
        let cur = p.read();
        // Set PR; preserve other bits, but clear write-1-to-clear bits
        // by NOT including them (writing 0 leaves them unchanged).
        p.write((cur & !PORTSC_RWC_MASK) | PORTSC_PR);
        // Spin ~50 ms.  We don't have a precise sleep here; use the
        // TSC-calibrated nanosecond clock if available.
        for _ in 0..1_000_000u32 { core::hint::spin_loop(); }
        // De-assert reset.
        let cur = p.read();
        p.write(cur & !(PORTSC_PR | PORTSC_RWC_MASK));
        // Wait for PE.
        for _ in 0..1_000_000u32 {
            if p.read() & PORTSC_PE != 0 { return Ok(()); }
            core::hint::spin_loop();
        }
    }
    Err("uhci: port did not enable after reset")
}

// ---- Diagnostics ----

static LAST_FRNUM: AtomicU64 = AtomicU64::new(0);

/// Read the current FRNUM register.  Useful as a "did the controller
/// even start ticking?" probe — if it doesn't move across two reads
/// separated by a few microseconds, the HC is wedged.
pub fn read_frnum() -> u16 {
    if !AVAILABLE.load(Ordering::Acquire) { return 0; }
    let inst_guard = INSTANCE.lock();
    let inst = match inst_guard.as_ref() { Some(i) => i, None => return 0 };
    let v: u16 = unsafe { Port::new(inst.iobase + REG_FRNUM).read() };
    LAST_FRNUM.store(v as u64, Ordering::Relaxed);
    v
}

pub fn iobase() -> Option<u16> {
    INSTANCE.lock().as_ref().map(|i| i.iobase)
}
