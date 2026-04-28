//! Minimal PCI configuration-space scan via legacy CF8/CFC ports.
//!
//! Walks every (bus, device, function) tuple and records vendor:device
//! matches.  No support yet for MMCFG (PCIe extended config) — that
//! requires the ACPI MCFG table, which we haven't parsed.  All real
//! virtio devices in QEMU still answer on legacy ports, so this is
//! enough to probe and configure them.

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;

const CONFIG_ADDR: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

#[derive(Debug, Clone, Copy)]
pub struct PciAddr {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciAddr {
    fn cf8_value(&self, off: u8) -> u32 {
        0x8000_0000
            | ((self.bus as u32) << 16)
            | ((self.dev as u32) << 11)
            | ((self.func as u32) << 8)
            | (off as u32 & 0xFC)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub addr:        PciAddr,
    pub vendor_id:   u16,
    pub device_id:   u16,
    pub class_code:  u8,
    pub subclass:    u8,
    pub prog_if:     u8,
    pub revision:    u8,
    pub header_type: u8,
    pub bars:        [u32; 6],
    pub interrupt_line: u8,
    pub interrupt_pin:  u8,
    pub subsys_vendor:  u16,
    pub subsys_id:      u16,
    /// Offset (in PCI config space) of the MSI capability, or 0 if absent.
    pub msi_cap:    u8,
    /// Offset of the MSI-X capability, or 0 if absent.
    pub msix_cap:   u8,
}

/// PCI capability IDs (PCI Local Bus 3.0 Appendix H).
pub const CAP_ID_MSI:  u8 = 0x05;
pub const CAP_ID_MSIX: u8 = 0x11;
pub const CAP_ID_VENDOR: u8 = 0x09;

unsafe fn config_read32(addr: PciAddr, off: u8) -> u32 {
    unsafe {
        let mut a: Port<u32> = Port::new(CONFIG_ADDR);
        let mut d: Port<u32> = Port::new(CONFIG_DATA);
        a.write(addr.cf8_value(off));
        d.read()
    }
}

unsafe fn config_write32(addr: PciAddr, off: u8, val: u32) {
    unsafe {
        let mut a: Port<u32> = Port::new(CONFIG_ADDR);
        let mut d: Port<u32> = Port::new(CONFIG_DATA);
        a.write(addr.cf8_value(off));
        d.write(val);
    }
}

unsafe fn config_read16(addr: PciAddr, off: u8) -> u16 {
    let dword = unsafe { config_read32(addr, off & 0xFC) };
    ((dword >> ((off & 2) * 8)) & 0xFFFF) as u16
}

unsafe fn config_read8(addr: PciAddr, off: u8) -> u8 {
    let dword = unsafe { config_read32(addr, off & 0xFC) };
    ((dword >> ((off & 3) * 8)) & 0xFF) as u8
}

fn read_device(addr: PciAddr) -> Option<PciDevice> {
    let vendor_id = unsafe { config_read16(addr, 0x00) };
    if vendor_id == 0xFFFF { return None; }
    let device_id = unsafe { config_read16(addr, 0x02) };
    let revision  = unsafe { config_read8(addr, 0x08) };
    let prog_if   = unsafe { config_read8(addr, 0x09) };
    let subclass  = unsafe { config_read8(addr, 0x0A) };
    let class_code = unsafe { config_read8(addr, 0x0B) };
    let header_type = unsafe { config_read8(addr, 0x0E) } & 0x7F;

    let mut bars = [0u32; 6];
    if header_type == 0 {
        for i in 0..6 {
            bars[i] = unsafe { config_read32(addr, 0x10 + (i as u8) * 4) };
        }
    }
    let subsys_vendor = unsafe { config_read16(addr, 0x2C) };
    let subsys_id     = unsafe { config_read16(addr, 0x2E) };
    let interrupt_line = unsafe { config_read8(addr, 0x3C) };
    let interrupt_pin  = unsafe { config_read8(addr, 0x3D) };

    let (msi_cap, msix_cap) = walk_capabilities(addr, header_type);

    Some(PciDevice {
        addr, vendor_id, device_id, class_code, subclass, prog_if, revision,
        header_type, bars, interrupt_line, interrupt_pin, subsys_vendor, subsys_id,
        msi_cap, msix_cap,
    })
}

/// Walk the PCI capability list rooted at config offset 0x34 (header
/// type 0 / 1; type 2 PCI cardbus has it at 0x14 but we don't see those
/// on QEMU).  Each capability is `<id:u8> <next:u8> ...`; chain ends
/// when next == 0.  Returns (msi_offset, msix_offset), 0 if absent.
///
/// Note: header status register bit 4 (Capabilities List) must be set
/// for this list to exist.  We check it; otherwise return zeros.
fn walk_capabilities(addr: PciAddr, header_type: u8) -> (u8, u8) {
    let status = unsafe { config_read16(addr, 0x06) };
    if status & 0x10 == 0 { return (0, 0); } // No capabilities list
    if header_type > 1 { return (0, 0); }

    let mut cur = unsafe { config_read8(addr, 0x34) } & 0xFC;
    let mut msi: u8 = 0;
    let mut msix: u8 = 0;
    let mut hops = 0;
    while cur != 0 && hops < 48 {
        let id   = unsafe { config_read8(addr, cur) };
        let next = unsafe { config_read8(addr, cur + 1) } & 0xFC;
        match id {
            CAP_ID_MSI  => msi = cur,
            CAP_ID_MSIX => msix = cur,
            _ => {}
        }
        if next == cur { break; } // self-loop guard
        cur = next;
        hops += 1;
    }
    (msi, msix)
}

static DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());

/// Walk bus 0..1, dev 0..32, func 0..8 in legacy mode.  QEMU exposes its
/// virtio devices on bus 0 by default; we don't yet cross host bridges
/// to other buses.  Records every responsive (vendor != 0xFFFF) function.
pub fn scan() {
    let mut found = Vec::new();
    for bus in 0u8..2 {
        for dev in 0u8..32 {
            // Function 0 first; if header_type bit 7 is set the device is
            // multi-function and we probe 1..8 too.
            let addr = PciAddr { bus, dev, func: 0 };
            let dev0 = match read_device(addr) { Some(d) => d, None => continue };
            let multi = unsafe { config_read8(addr, 0x0E) } & 0x80 != 0;
            found.push(dev0);
            if multi {
                for f in 1u8..8 {
                    let a = PciAddr { bus, dev, func: f };
                    if let Some(d) = read_device(a) { found.push(d); }
                }
            }
        }
    }

    crate::klog_info!("PCI: enumerated {} device(s) on bus 0/1", found.len());
    for d in &found {
        let mut caps = alloc::string::String::new();
        if d.msi_cap  != 0 { caps.push_str(" msi"); }
        if d.msix_cap != 0 { caps.push_str(" msix"); }
        crate::klog_info!(
            "  {:02x}:{:02x}.{}  vendor={:04x} device={:04x} class={:02x}.{:02x}.{:02x}{}",
            d.addr.bus, d.addr.dev, d.addr.func,
            d.vendor_id, d.device_id, d.class_code, d.subclass, d.prog_if,
            caps,
        );
    }
    *DEVICES.lock() = found;
}

pub fn list() -> Vec<PciDevice> { DEVICES.lock().clone() }

pub fn find(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    DEVICES.lock().iter()
        .find(|d| d.vendor_id == vendor_id && d.device_id == device_id)
        .copied()
}

pub fn find_class(class_code: u8, subclass: u8) -> Vec<PciDevice> {
    DEVICES.lock().iter()
        .filter(|d| d.class_code == class_code && d.subclass == subclass)
        .copied()
        .collect()
}

/// Enable bus mastering + memory space + IO space for a device by
/// setting bits 0..2 of the command register at offset 0x04.
pub fn enable_device(addr: PciAddr) {
    unsafe {
        let cmd = config_read32(addr, 0x04);
        config_write32(addr, 0x04, cmd | 0x0007);
    }
}

/// Probe BAR `idx` to determine size: write all-1s, read back to see
/// which bits are R/O (= size mask), restore original.  Returns
/// (base_addr, size_bytes, is_io).
pub fn probe_bar(addr: PciAddr, idx: u8) -> Option<(u64, u64, bool)> {
    if idx >= 6 { return None; }
    let off = 0x10 + idx * 4;
    let original = unsafe { config_read32(addr, off) };
    if original == 0 { return None; }
    let is_io = (original & 1) == 1;
    let mask: u32 = if is_io { 0xFFFF_FFFC } else { 0xFFFF_FFF0 };

    unsafe { config_write32(addr, off, 0xFFFF_FFFF); }
    let probe = unsafe { config_read32(addr, off) };
    unsafe { config_write32(addr, off, original); }

    if probe == 0 { return None; }
    let size = (!(probe & mask)).wrapping_add(1) as u64;
    let base = (original & mask) as u64;
    Some((base, size, is_io))
}
