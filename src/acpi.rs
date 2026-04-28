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

// =============================================================================
// MADT — Multiple APIC Description Table (signature "APIC", ACPI 6.5 §5.2.12)
// =============================================================================
//
// MADT body layout, after the standard 36-byte SDT header:
//
//     u32 lapic_address       — physical base of the local APIC (32-bit
//                               low half; 64-bit override is in entry
//                               type 5 if present)
//     u32 flags               — bit 0 = PCAT_COMPAT (8259 present)
//     u8  entries[]           — variable-length entry stream
//
// Each entry begins with `u8 type, u8 length, ...payload`.  Types we
// honour:
//
//     0  Processor Local APIC: { acpi_proc_id, apic_id, flags }
//     1  IOAPIC:               { ioapic_id, _reserved, address, gsi_base }
//     2  Interrupt Source Override:
//                              { bus, source_irq, gsi, flags(u16) }
//     5  Local APIC Address Override (u64 phys)
//
// The ISO entries describe how legacy PC-AT IRQs are wired to the
// IOAPIC's Global System Interrupts — on PIIX QEMU, ISA IRQ 0 (timer)
// is routed to GSI 2, which is the canonical reason a naive
// "irq N → IOAPIC pin N" routing breaks.

/// One MADT IOAPIC record.  GSI base says "this IOAPIC handles GSIs
/// starting at this number"; combined with `redir_count` (which the
/// caller reads from IOAPICVER) it covers a contiguous range.
#[derive(Debug, Clone, Copy)]
pub struct MadtIoApic {
    pub id: u8,
    pub phys_addr: u32,
    pub gsi_base:  u32,
}

/// One MADT Interrupt Source Override.  `polarity` and `trigger` are
/// the two-bit subfields of `flags` (bits 1:0 polarity, 3:2 trigger).
#[derive(Debug, Clone, Copy)]
pub struct MadtIso {
    pub bus: u8,
    pub source_irq: u8,
    pub gsi: u32,
    pub polarity: u8, // 0=conforming, 1=high, 3=low
    pub trigger:  u8, // 0=conforming, 1=edge, 3=level
}

#[derive(Debug, Clone)]
pub struct Madt {
    pub lapic_phys: u64,
    pub pic_present: bool,
    pub ioapics: alloc::vec::Vec<MadtIoApic>,
    pub overrides: alloc::vec::Vec<MadtIso>,
    pub lapic_ids: alloc::vec::Vec<u8>, // LAPIC IDs of all enabled CPUs
}

/// Parse the MADT, if present.  Returns None if no APIC table was
/// listed in RSDT/XSDT.
pub fn parse_madt() -> Option<Madt> {
    let (phys, _hdr) = find_sdt(b"APIC")?;
    let body = sdt_body(phys);
    if body.len() < 8 { return None; }
    let lapic_addr_lo = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    let flags         = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let mut madt = Madt {
        lapic_phys: lapic_addr_lo as u64,
        pic_present: flags & 1 != 0,
        ioapics: alloc::vec::Vec::new(),
        overrides: alloc::vec::Vec::new(),
        lapic_ids: alloc::vec::Vec::new(),
    };

    // Walk entries.
    let mut pos = 8usize;
    while pos + 2 <= body.len() {
        let etype = body[pos];
        let elen  = body[pos + 1] as usize;
        if elen < 2 || pos + elen > body.len() { break; }
        let payload = &body[pos + 2..pos + elen];
        match etype {
            0 if payload.len() >= 6 => {
                // Processor Local APIC: bytes are
                // (proc_id, apic_id, flags:u32).  Bit 0 of flags = enabled.
                let apic_id = payload[1];
                let p_flags = u32::from_le_bytes([
                    payload[2], payload[3], payload[4], payload[5]]);
                if p_flags & 1 != 0 {
                    madt.lapic_ids.push(apic_id);
                }
            }
            1 if payload.len() >= 10 => {
                let id = payload[0];
                // payload[1] reserved
                let phys = u32::from_le_bytes([
                    payload[2], payload[3], payload[4], payload[5]]);
                let gsi  = u32::from_le_bytes([
                    payload[6], payload[7], payload[8], payload[9]]);
                madt.ioapics.push(MadtIoApic { id, phys_addr: phys, gsi_base: gsi });
            }
            2 if payload.len() >= 8 => {
                let bus = payload[0];
                let src = payload[1];
                let gsi = u32::from_le_bytes([
                    payload[2], payload[3], payload[4], payload[5]]);
                let f   = u16::from_le_bytes([payload[6], payload[7]]);
                madt.overrides.push(MadtIso {
                    bus, source_irq: src, gsi,
                    polarity: (f & 0x3) as u8,
                    trigger:  ((f >> 2) & 0x3) as u8,
                });
            }
            5 if payload.len() >= 10 => {
                // Local APIC Address Override: 2 reserved + u64 phys.
                let phys = u64::from_le_bytes([
                    payload[2], payload[3], payload[4], payload[5],
                    payload[6], payload[7], payload[8], payload[9],
                ]);
                madt.lapic_phys = phys;
            }
            _ => {}
        }
        pos += elen;
    }
    Some(madt)
}

/// Translate an ISA IRQ to the IOAPIC GSI, applying any matching
/// Interrupt Source Override.  Returns the GSI (== `irq` if no
/// override) plus the polarity/trigger overrides that should be
/// programmed into the redirection-table entry.
pub fn resolve_irq(madt: &Madt, irq: u8) -> (u32, u8, u8) {
    for iso in &madt.overrides {
        if iso.bus == 0 && iso.source_irq == irq {
            return (iso.gsi, iso.polarity, iso.trigger);
        }
    }
    (irq as u32, 0, 0)
}
