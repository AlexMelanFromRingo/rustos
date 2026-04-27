//! Linux-style capabilities.
//!
//! A capability is a single privileged operation a process is allowed to
//! perform.  Capabilities live on the process control block (effective
//! and permitted sets, mirroring the kernel's `task_struct.cred->cap_*`).
//! A separate per-uid table holds the *initial* set granted to a process
//! that runs as a given uid: when a process changes uid (setuid, su, sudo),
//! its effective set is recomputed from the per-uid seed.
//!
//! Subsystems consult [`current_has`] / [`check`] before performing
//! privileged operations.  Both look at the running process's
//! `cap_effective`, falling back to "uid 0 has all caps" when no process
//! is bound (e.g. early boot, kernel-mode shell command without a running
//! user process).

use spin::Mutex;
use alloc::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Capability {
    Chown          = 0,
    DacOverride    = 1,
    DacReadSearch  = 2,
    Fowner         = 3,
    Fsetid         = 4,
    Kill           = 5,
    Setgid         = 6,
    Setuid         = 7,
    Setpcap        = 8,
    NetBindService = 10,
    NetAdmin       = 12,
    NetRaw         = 13,
    SysModule      = 16,
    SysAdmin       = 21,
    SysBoot        = 22,
    SysNice        = 23,
    SysResource    = 24,
    SysTime        = 25,
    Mknod          = 27,
    Audit          = 30,
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

    pub fn all() -> &'static [Capability] {
        &[
            Capability::Chown, Capability::DacOverride, Capability::DacReadSearch,
            Capability::Fowner, Capability::Fsetid, Capability::Kill,
            Capability::Setgid, Capability::Setuid, Capability::Setpcap,
            Capability::NetBindService, Capability::NetRaw, Capability::NetAdmin,
            Capability::SysModule, Capability::SysAdmin, Capability::SysBoot,
            Capability::SysNice, Capability::SysResource, Capability::SysTime,
            Capability::Mknod, Capability::Audit,
        ]
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

/// Per-uid initial-cap table.  Acts as the seed used when a process is
/// created or its uid changes.  Linux's equivalent is the file capabilities
/// stored alongside binaries plus the process's `cap_inheritable` set; for
/// our model the per-uid seed is a reasonable approximation.
pub struct CapTable {
    by_uid: BTreeMap<u32, CapSet>,
}

impl CapTable {
    pub const fn new() -> Self { CapTable { by_uid: BTreeMap::new() } }

    pub fn install_defaults(&mut self) {
        // Root (uid 0) seeds with full caps; everyone else with empty set.
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

/// Re-seed the running process's capability sets from the per-uid table.
/// Called by setuid/su/sudo paths after switching CURRENT_CREDS.
pub fn reseed_for_uid(uid: u32) {
    let seed = CAPS.lock().for_uid(uid);
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    if let Some(proc_) = pm.current_process_mut() {
        proc_.cap_effective = seed.raw();
        proc_.cap_permitted = seed.raw();
    }
}

/// Return the running process's effective cap set, or — when there is no
/// running user process — the per-uid seed for the current credentials.
fn current_effective() -> CapSet {
    let pm = crate::process::PROCESS_MANAGER.lock();
    if let Some(proc_) = pm.current_process() {
        return CapSet::from_raw(proc_.cap_effective);
    }
    drop(pm);
    let creds = crate::users::CURRENT_CREDS.lock();
    let uid = creds.euid;
    drop(creds);
    CAPS.lock().for_uid(uid)
}

/// True if the current credentials' effective set holds `c`.
pub fn current_has(c: Capability) -> bool {
    current_effective().has(c)
}

/// Convenience check used by privileged subsystems.
/// Returns true for "uid 0 OR has the cap".
pub fn check(c: Capability) -> bool {
    {
        let creds = crate::users::CURRENT_CREDS.lock();
        if creds.euid == 0 { return true; }
    }
    current_has(c)
}

/// Drop a capability from the running process's effective + permitted sets.
/// CAP_SETPCAP is required to call this for any cap other than the one
/// being dropped from oneself (not enforced here yet).
pub fn drop_self(c: Capability) {
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    if let Some(proc_) = pm.current_process_mut() {
        let mut eff = CapSet::from_raw(proc_.cap_effective);
        let mut perm = CapSet::from_raw(proc_.cap_permitted);
        eff.revoke(c); perm.revoke(c);
        proc_.cap_effective = eff.raw();
        proc_.cap_permitted = perm.raw();
    }
}

/// Effective set of the running process or per-uid fallback.  Used by
/// the `getcap` shell command for display.
pub fn current_effective_raw() -> u64 { current_effective().raw() }
