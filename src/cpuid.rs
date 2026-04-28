//! CPUID-driven CPU identification.
//!
//! `cpu_info()` returns a snapshot of vendor, brand, family/model/stepping
//! and feature flags by issuing CPUID leaves 0, 1, 7 (subleaf 0), and the
//! extended brand-string leaves 0x80000002..4.  The result feeds
//! /proc/cpuinfo.
//!
//! Reference: Intel SDM Vol 2A "CPUID — CPU Identification".

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct CpuInfo {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub flags: Vec<&'static str>,
}

/// Bit table for leaf 1 EDX flags (Intel SDM Vol 2A, "CPUID — feature
/// information").  Names match `/proc/cpuinfo` on Linux.
const LEAF1_EDX: &[(u32, &str)] = &[
    (0,  "fpu"),  (1,  "vme"),  (2,  "de"),   (3,  "pse"),
    (4,  "tsc"),  (5,  "msr"),  (6,  "pae"),  (7,  "mce"),
    (8,  "cx8"),  (9,  "apic"), (11, "sep"),  (12, "mtrr"),
    (13, "pge"),  (14, "mca"),  (15, "cmov"), (16, "pat"),
    (17, "pse36"),(18, "pn"),   (19, "clflush"),
    (23, "mmx"),  (24, "fxsr"), (25, "sse"),  (26, "sse2"),
    (28, "ht"),   (29, "tm"),   (31, "pbe"),
];

/// Leaf 1 ECX flags.
const LEAF1_ECX: &[(u32, &str)] = &[
    (0,  "pni"),  (1,  "pclmulqdq"), (3,  "monitor"),
    (5,  "vmx"),  (9,  "ssse3"), (12, "fma"), (13, "cx16"),
    (19, "sse4_1"), (20, "sse4_2"),
    (22, "movbe"), (23, "popcnt"), (25, "aes"), (26, "xsave"),
    (27, "osxsave"), (28, "avx"), (29, "f16c"), (30, "rdrand"),
    (31, "hypervisor"),
];

/// Leaf 7 EBX (subleaf 0).
const LEAF7_EBX: &[(u32, &str)] = &[
    (0,  "fsgsbase"), (3,  "bmi1"), (5, "avx2"),
    (7, "smep"),   (8, "bmi2"),
    (16, "avx512f"), (18, "rdseed"), (19, "adx"),
    (20, "smap"),  (29, "sha_ni"),
];

/// Leaf 0x80000001 EDX.
const LEAF8001_EDX: &[(u32, &str)] = &[
    (11, "syscall"), (20, "nx"),
    (26, "pdpe1gb"), (27, "rdtscp"), (29, "lm"),
];

fn cpuid(leaf: u32) -> core::arch::x86_64::CpuidResult {
    unsafe { core::arch::x86_64::__cpuid(leaf) }
}
fn cpuid_count(leaf: u32, sub: u32) -> core::arch::x86_64::CpuidResult {
    unsafe { core::arch::x86_64::__cpuid_count(leaf, sub) }
}

fn vendor_string() -> String {
    let r = cpuid(0);
    let mut bytes = [0u8; 12];
    bytes[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    bytes[4..8].copy_from_slice(&r.edx.to_le_bytes());
    bytes[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    core::str::from_utf8(&bytes).unwrap_or("Unknown").to_string()
}

/// Brand string from leaves 0x80000002..4 (48 chars total, padded with NULs).
fn brand_string() -> String {
    let max = cpuid(0x8000_0000).eax;
    if max < 0x8000_0004 { return String::from("Unknown CPU"); }
    let mut bytes = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004].iter().enumerate() {
        let r = cpuid(*leaf);
        bytes[i * 16..i * 16 + 4].copy_from_slice(&r.eax.to_le_bytes());
        bytes[i * 16 + 4..i * 16 + 8].copy_from_slice(&r.ebx.to_le_bytes());
        bytes[i * 16 + 8..i * 16 + 12].copy_from_slice(&r.ecx.to_le_bytes());
        bytes[i * 16 + 12..i * 16 + 16].copy_from_slice(&r.edx.to_le_bytes());
    }
    let s = core::str::from_utf8(&bytes).unwrap_or("");
    s.trim_end_matches(char::from(0)).trim().to_string()
}

pub fn cpu_info() -> CpuInfo {
    let vendor = vendor_string();
    let brand = brand_string();

    let leaf1 = cpuid(1);
    let stepping = leaf1.eax & 0xF;
    let model_lo = (leaf1.eax >> 4) & 0xF;
    let family_lo = (leaf1.eax >> 8) & 0xF;
    let model_hi = (leaf1.eax >> 16) & 0xF;
    let family_hi = (leaf1.eax >> 20) & 0xFF;
    let family = if family_lo == 0xF { family_lo + family_hi } else { family_lo };
    let model = if family_lo == 0xF || family_lo == 6 {
        (model_hi << 4) | model_lo
    } else {
        model_lo
    };

    let mut flags: Vec<&'static str> = Vec::new();
    for &(bit, name) in LEAF1_EDX { if leaf1.edx & (1 << bit) != 0 { flags.push(name); } }
    for &(bit, name) in LEAF1_ECX { if leaf1.ecx & (1 << bit) != 0 { flags.push(name); } }

    // Leaf 7 only present if max-leaf ≥ 7.
    let max_leaf = cpuid(0).eax;
    if max_leaf >= 7 {
        let l7 = cpuid_count(7, 0);
        for &(bit, name) in LEAF7_EBX { if l7.ebx & (1 << bit) != 0 { flags.push(name); } }
    }
    // Extended leaves.
    let max_ext = cpuid(0x8000_0000).eax;
    if max_ext >= 0x8000_0001 {
        let l8 = cpuid(0x8000_0001);
        for &(bit, name) in LEAF8001_EDX { if l8.edx & (1 << bit) != 0 { flags.push(name); } }
    }

    CpuInfo { vendor, brand, family, model, stepping, flags }
}

/// Convenience: format a /proc/cpuinfo block for one logical CPU.
pub fn proc_cpuinfo_block(cpu: u32) -> String {
    let info = cpu_info();
    let mut s = String::new();
    s.push_str(&alloc::format!("processor\t: {}\n", cpu));
    s.push_str(&alloc::format!("vendor_id\t: {}\n", info.vendor));
    s.push_str(&alloc::format!("cpu family\t: {}\n", info.family));
    s.push_str(&alloc::format!("model\t\t: {}\n", info.model));
    s.push_str(&alloc::format!("model name\t: {}\n", info.brand));
    s.push_str(&alloc::format!("stepping\t: {}\n", info.stepping));
    let mhz = crate::tsc::freq_hz() / 1_000_000;
    let khz = (crate::tsc::freq_hz() / 1_000) % 1_000;
    s.push_str(&alloc::format!("cpu MHz\t\t: {}.{:03}\n", mhz, khz));
    s.push_str(&alloc::format!("flags\t\t: {}\n", info.flags.join(" ")));
    s.push_str("bogomips\t: 0.00\n");
    s
}
