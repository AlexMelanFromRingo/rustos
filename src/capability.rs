//! Linux-style capabilities.
//!
//! A capability is a single privileged operation a process is allowed to
//! perform.  By default root has all caps and non-root has none, which
//! matches Linux's traditional behaviour.  setcap can grant or drop
//! individual caps.
//!
//! This module owns a per-process capability bitmap (effective set only;
//! permitted/inheritable/bounding sets are not modelled separately yet).
//! Subsystems consult [`has_cap`] before performing privileged operations.

use spin::Mutex;
use alloc::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Capability {
    Chown          = 0,  // CAP_CHOWN
    DacOverride    = 1,  // CAP_DAC_OVERRIDE
    DacReadSearch  = 2,  // CAP_DAC_READ_SEARCH
    Fowner         = 3,  // CAP_FOWNER
    Fsetid         = 4,  // CAP_FSETID
    Kill           = 5,  // CAP_KILL
    Setgid         = 6,  // CAP_SETGID
    Setuid         = 7,  // CAP_SETUID
    Setpcap        = 8,  // CAP_SETPCAP
    NetBindService = 10, // CAP_NET_BIND_SERVICE
    NetRaw         = 13, // CAP_NET_RAW
    NetAdmin       = 12, // CAP_NET_ADMIN
    SysModule      = 16, // CAP_SYS_MODULE
    SysAdmin       = 21, // CAP_SYS_ADMIN
    SysBoot        = 22, // CAP_SYS_BOOT
    SysNice        = 23, // CAP_SYS_NICE
    SysResource    = 24, // CAP_SYS_RESOURCE
    SysTime        = 25, // CAP_SYS_TIME
    Mknod          = 27, // CAP_MKNOD
    Audit          = 30, // CAP_AUDIT_WRITE
}

impl Capability {
    pub fn parse(s: &str) -> Option<Self> {
        let lc = s.to_lowercase();
        let stripped = lc.strip_prefix("cap_").unwrap_or(&lc);
        match stripped {
            "chown" => Some(Capability::Chown),
            "dac_override" => Some(Capability::DacOverride),
            "dac_read_search" => Some(Capability::DacReadSearch),
            "fowner" => Some(Capability::Fowner),
            "fsetid" => Some(Capability::Fsetid),
            "kill" => Some(Capability::Kill),
            "setgid" => Some(Capability::Setgid),
            "setuid" => Some(Capability::Setuid),
            "setpcap" => Some(Capability::Setpcap),
            "net_bind_service" => Some(Capability::NetBindService),
            "net_raw" => Some(Capability::NetRaw),
            "net_admin" => Some(Capability::NetAdmin),
            "sys_module" => Some(Capability::SysModule),
            "sys_admin" => Some(Capability::SysAdmin),
            "sys_boot" => Some(Capability::SysBoot),
            "sys_nice" => Some(Capability::SysNice),
            "sys_resource" => Some(Capability::SysResource),
            "sys_time" => Some(Capability::SysTime),
            "mknod" => Some(Capability::Mknod),
            "audit_write" => Some(Capability::Audit),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Capability::Chown => "CAP_CHOWN",
            Capability::DacOverride => "CAP_DAC_OVERRIDE",
            Capability::DacReadSearch => "CAP_DAC_READ_SEARCH",
            Capability::Fowner => "CAP_FOWNER",
            Capability::Fsetid => "CAP_FSETID",
            Capability::Kill => "CAP_KILL",
            Capability::Setgid => "CAP_SETGID",
            Capability::Setuid => "CAP_SETUID",
            Capability::Setpcap => "CAP_SETPCAP",
            Capability::NetBindService => "CAP_NET_BIND_SERVICE",
            Capability::NetRaw => "CAP_NET_RAW",
            Capability::NetAdmin => "CAP_NET_ADMIN",
            Capability::SysModule => "CAP_SYS_MODULE",
            Capability::SysAdmin => "CAP_SYS_ADMIN",
            Capability::SysBoot => "CAP_SYS_BOOT",
            Capability::SysNice => "CAP_SYS_NICE",
            Capability::SysResource => "CAP_SYS_RESOURCE",
            Capability::SysTime => "CAP_SYS_TIME",
            Capability::Mknod => "CAP_MKNOD",
            Capability::Audit => "CAP_AUDIT_WRITE",
        }
    }
}

/// Bitmap of granted capabilities for one principal.
#[derive(Debug, Clone, Copy)]
pub struct CapSet { bits: u64 }

impl CapSet {
    pub const fn empty() -> Self { CapSet { bits: 0 } }
    pub const fn full()  -> Self { CapSet { bits: u64::MAX } }

    pub fn has(&self, c: Capability) -> bool { self.bits & (1u64 << (c as u32)) != 0 }
    pub fn grant(&mut self, c: Capability) { self.bits |= 1u64 << (c as u32); }
    pub fn revoke(&mut self, c: Capability) { self.bits &= !(1u64 << (c as u32)); }
    pub fn raw(&self) -> u64 { self.bits }
    pub fn from_raw(b: u64) -> Self { CapSet { bits: b } }
}

/// Per-uid capability set (Linux puts caps on processes; we put them on
/// uids since we don't track per-process state for non-running PIDs).
pub struct CapTable {
    by_uid: BTreeMap<u32, CapSet>,
}

impl CapTable {
    pub const fn new() -> Self { CapTable { by_uid: BTreeMap::new() } }

    pub fn install_defaults(&mut self) {
        // Root has all caps; everyone else starts with none.
        self.by_uid.insert(0, CapSet::full());
    }

    pub fn for_uid(&self, uid: u32) -> CapSet {
        if uid == 0 {
            return CapSet::full();
        }
        self.by_uid.get(&uid).copied().unwrap_or_else(CapSet::empty)
    }

    pub fn grant(&mut self, uid: u32, c: Capability) {
        self.by_uid.entry(uid).or_insert_with(CapSet::empty).grant(c);
    }
    pub fn revoke(&mut self, uid: u32, c: Capability) {
        self.by_uid.entry(uid).or_insert_with(CapSet::empty).revoke(c);
    }
}

pub static CAPS: Mutex<CapTable> = Mutex::new(CapTable::new());

pub fn init() { CAPS.lock().install_defaults(); }

/// True if the current credentials have `c`.
pub fn current_has(c: Capability) -> bool {
    let creds = crate::users::CURRENT_CREDS.lock();
    let uid = creds.euid;
    drop(creds);
    CAPS.lock().for_uid(uid).has(c)
}

/// Convenience: returns true if root, or has the named cap.
pub fn check(c: Capability) -> bool {
    let creds = crate::users::CURRENT_CREDS.lock();
    if creds.euid == 0 { return true; }
    drop(creds);
    current_has(c)
}
