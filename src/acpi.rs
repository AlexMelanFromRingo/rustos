//! ACPI table discovery — RSDP, RSDT/XSDT, generic SDT iteration.
//!
//! Foundation for the MADT consumer (#113) and any future SDT readers
//! (FADT, MCFG for PCIe ECAM, HPET, etc.).
//!
//! Discovery on legacy BIOS (ACPI 6.5 §5.2.5):
//!   1. Look at byte 0x40E of the BIOS Data Area for the EBDA segment;
//!      scan the first KiB of the EBDA for the 8-byte signature
//!      "RSD PTR ".
//!   2. Otherwise scan 0xE0000..0x100000 in 16-byte strides.
//!   3. The 20-byte v1 RSDP must checksum to zero; v2+ extends it to
//!      36 bytes with its own ext_checksum, valid only if revision ≥ 2.
//!
//! We rely on the bootloader's direct phys-memory map for read access.

use alloc::vec::Vec;
use spin::Mutex;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Rsdp {
    pub signature: [u8; 8],
    pub checksum: u8,
    pub oem_id:   [u8; 6],
    pub revision: u8,
    pub rsdt_addr: u32,
    // Following fields valid only if revision >= 2:
    pub length:    u32,
    pub xsdt_addr: u64,
    pub ext_checksum: u8,
    pub _reserved: [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length:    u32,
    pub revision:  u8,
    pub checksum:  u8,
    pub oem_id:    [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id:   u32,
    pub creator_revision: u32,
}

/// Cached pointers, populated by `init`.  Both physical addresses; turn
/// into virt by adding `crate::memory::phys_offset()`.
pub struct AcpiTables {
    pub rsdp_phys: u64,
    pub rsdt_phys: u64, // 0 if XSDT used
    pub xsdt_phys: u64, // 0 if v1
    pub sdt_phys: Vec<u64>, // every entry pointer in RSDT/XSDT
}

pub static ACPI: Mutex<Option<AcpiTables>> = Mutex::new(None);

unsafe fn read_phys<T: Copy>(phys: u64) -> T {
    let virt = crate::memory::phys_offset() + phys;
    unsafe { core::ptr::read_unaligned(virt as *const T) }
}

unsafe fn slice_at_phys(phys: u64, len: usize) -> &'static [u8] {
    let virt = crate::memory::phys_offset() + phys;
    unsafe { core::slice::from_raw_parts(virt as *const u8, len) }
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0
}

/// Scan a physical range for the "RSD PTR " signature, 16-byte aligned.
/// Returns the physical address of the first matching candidate whose
/// checksum is valid.
unsafe fn scan_for_rsdp(start: u64, end: u64) -> Option<u64> {
    let mut p = start & !0xF;
    while p + 36 <= end {
        let sig = unsafe { read_phys::<[u8; 8]>(p) };
        if &sig == b"RSD PTR " {
            // v1 checksum is over first 20 bytes.
            let bytes = unsafe { slice_at_phys(p, 20) };
            if checksum_ok(bytes) {
                return Some(p);
            }
        }
        p += 16;
    }
    None
}

/// Find the RSDP — first try the EBDA pointer at 0x40E, fall back to
/// the BIOS scan range 0xE0000..0x100000.
pub fn find_rsdp() -> Option<u64> {
    unsafe {
        let ebda_seg = read_phys::<u16>(0x40E) as u64;
        if ebda_seg != 0 {
            let ebda = ebda_seg << 4;
            if let Some(p) = scan_for_rsdp(ebda, ebda + 1024) {
                return Some(p);
            }
        }
        scan_for_rsdp(0xE_0000, 0x10_0000)
    }
}

pub fn init() {
    let rsdp_phys = match find_rsdp() {
        Some(p) => p,
        None => {
            crate::klog_warn!("acpi: no RSDP found");
            return;
        }
    };
    let rsdp: Rsdp = unsafe { read_phys(rsdp_phys) };
    let revision = rsdp.revision;
    let rsdt_addr = rsdp.rsdt_addr as u64;
    let xsdt_addr = if revision >= 2 { rsdp.xsdt_addr } else { 0 };

    // Pick whichever table we'll walk.
    let (header_phys, use_xsdt) = if xsdt_addr != 0 {
        (xsdt_addr, true)
    } else {
        (rsdt_addr, false)
    };
    if header_phys == 0 {
        crate::klog_warn!("acpi: RSDP has no RSDT/XSDT pointer");
        return;
    }
    let header: SdtHeader = unsafe { read_phys(header_phys) };
    let length = header.length;
    if length < 36 || length > 0x10_0000 {
        crate::klog_warn!("acpi: bad RSDT/XSDT length {}", length);
        return;
    }
    // Validate checksum.
    let bytes = unsafe { slice_at_phys(header_phys, length as usize) };
    if !checksum_ok(bytes) {
        crate::klog_warn!("acpi: RSDT/XSDT checksum failed");
        return;
    }
    // Walk entries: 4 bytes each for RSDT, 8 bytes each for XSDT.
    let entries_off = 36usize;
    let entry_size = if use_xsdt { 8 } else { 4 };
    let mut sdt_phys = Vec::new();
    let mut i = entries_off;
    while i + entry_size <= length as usize {
        let phys = if use_xsdt {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&bytes[i..i + 8]);
            u64::from_le_bytes(buf)
        } else {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&bytes[i..i + 4]);
            u32::from_le_bytes(buf) as u64
        };
        sdt_phys.push(phys);
        i += entry_size;
    }

    crate::klog_info!(
        "acpi: RSDP @ {:#x} rev {}, {} {} entries",
        rsdp_phys, revision,
        if use_xsdt { "XSDT" } else { "RSDT" },
        sdt_phys.len(),
    );
    for &p in &sdt_phys {
        let h: SdtHeader = unsafe { read_phys(p) };
        let sig = core::str::from_utf8(&h.signature).unwrap_or("????");
        let len = h.length;
        crate::klog_info!("  {} @ {:#x} ({} bytes)", sig, p, len);
    }

    *ACPI.lock() = Some(AcpiTables {
        rsdp_phys,
        rsdt_phys: if use_xsdt { 0 } else { header_phys },
        xsdt_phys: if use_xsdt { header_phys } else { 0 },
        sdt_phys,
    });
}

/// Find the SDT with the given 4-byte signature (e.g. b"APIC" for the
/// MADT).  Returns the physical address + parsed header.
pub fn find_sdt(signature: &[u8; 4]) -> Option<(u64, SdtHeader)> {
    let g = ACPI.lock();
    let tables = g.as_ref()?;
    for &p in &tables.sdt_phys {
        let h: SdtHeader = unsafe { read_phys(p) };
        if &h.signature == signature {
            return Some((p, h));
        }
    }
    None
}

/// Borrow the body of an SDT (everything after the 36-byte header).
pub fn sdt_body(phys: u64) -> &'static [u8] {
    let h: SdtHeader = unsafe { read_phys(phys) };
    let len = h.length as usize;
    if len <= 36 { return &[]; }
    unsafe {
        let virt = crate::memory::phys_offset() + phys + 36;
        core::slice::from_raw_parts(virt as *const u8, len - 36)
    }
}
