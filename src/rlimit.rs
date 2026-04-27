//! Resource limits (Linux `rlimit` family).
//!
//! Each process has a table of (resource → (soft, hard)) limits.  `getrlimit`
//! reads them, `setrlimit` updates them subject to the rule that you can
//! never raise the hard limit, and the soft limit is bounded above by the
//! hard limit.
//!
//! This module owns a per-process limits table; the actual *enforcement*
//! lives in the subsystems that consume the values (e.g. the FD allocator
//! reads RLIMIT_NOFILE).  For now the only enforcement hook is a query
//! API; it's wired up where it makes sense and ignored elsewhere.

use alloc::collections::BTreeMap;
use spin::Mutex;

pub const RLIM_INFINITY: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Resource {
    Cpu       = 0,  // RLIMIT_CPU       — seconds of CPU time
    Fsize     = 1,  // RLIMIT_FSIZE     — max file size, bytes
    Data      = 2,  // RLIMIT_DATA      — process data segment
    Stack     = 3,  // RLIMIT_STACK     — stack size
    Core      = 4,  // RLIMIT_CORE      — max core file size
    Rss       = 5,  // RLIMIT_RSS       — resident set size
    Nproc     = 6,  // RLIMIT_NPROC     — number of processes
    NoFile    = 7,  // RLIMIT_NOFILE    — number of open file descriptors
    Memlock   = 8,  // RLIMIT_MEMLOCK   — locked pages
    As        = 9,  // RLIMIT_AS        — address-space size
    Locks     = 10, // RLIMIT_LOCKS     — file locks
    Sigpending= 11, // RLIMIT_SIGPENDING
    Msgqueue  = 12, // RLIMIT_MSGQUEUE
    Nice      = 13, // RLIMIT_NICE
    Rtprio    = 14, // RLIMIT_RTPRIO
}

impl Resource {
    pub fn from_u32(n: u32) -> Option<Self> {
        match n {
            0 => Some(Resource::Cpu),
            1 => Some(Resource::Fsize),
            2 => Some(Resource::Data),
            3 => Some(Resource::Stack),
            4 => Some(Resource::Core),
            5 => Some(Resource::Rss),
            6 => Some(Resource::Nproc),
            7 => Some(Resource::NoFile),
            8 => Some(Resource::Memlock),
            9 => Some(Resource::As),
            10 => Some(Resource::Locks),
            11 => Some(Resource::Sigpending),
            12 => Some(Resource::Msgqueue),
            13 => Some(Resource::Nice),
            14 => Some(Resource::Rtprio),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Resource::Cpu => "cpu",
            Resource::Fsize => "fsize",
            Resource::Data => "data",
            Resource::Stack => "stack",
            Resource::Core => "core",
            Resource::Rss => "rss",
            Resource::Nproc => "nproc",
            Resource::NoFile => "nofile",
            Resource::Memlock => "memlock",
            Resource::As => "as",
            Resource::Locks => "locks",
            Resource::Sigpending => "sigpending",
            Resource::Msgqueue => "msgqueue",
            Resource::Nice => "nice",
            Resource::Rtprio => "rtprio",
        }
    }
}

/// Linux struct rlimit { rlim_t rlim_cur; rlim_t rlim_max; }.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Rlimit {
    pub soft: u64,
    pub hard: u64,
}

/// Per-process limits table (single global since we don't have full
/// per-process tables wired everywhere yet).
pub struct LimitsTable {
    by_resource: BTreeMap<u32, Rlimit>,
}

impl LimitsTable {
    pub const fn new() -> Self { LimitsTable { by_resource: BTreeMap::new() } }

    pub fn install_defaults(&mut self) {
        // Pick conservative-but-realistic defaults.
        self.by_resource.insert(Resource::Cpu as u32,        Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Fsize as u32,      Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Data as u32,       Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Stack as u32,      Rlimit { soft: 8 * 1024 * 1024, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Core as u32,       Rlimit { soft: 0, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Rss as u32,        Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Nproc as u32,      Rlimit { soft: 256, hard: 1024 });
        self.by_resource.insert(Resource::NoFile as u32,     Rlimit { soft: 1024, hard: 4096 });
        self.by_resource.insert(Resource::Memlock as u32,    Rlimit { soft: 64 * 1024, hard: 64 * 1024 });
        self.by_resource.insert(Resource::As as u32,         Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Locks as u32,      Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY });
        self.by_resource.insert(Resource::Sigpending as u32, Rlimit { soft: 1024, hard: 1024 });
        self.by_resource.insert(Resource::Msgqueue as u32,   Rlimit { soft: 819200, hard: 819200 });
        self.by_resource.insert(Resource::Nice as u32,       Rlimit { soft: 0, hard: 0 });
        self.by_resource.insert(Resource::Rtprio as u32,     Rlimit { soft: 0, hard: 0 });
    }

    pub fn get(&self, res: Resource) -> Rlimit {
        self.by_resource.get(&(res as u32)).copied().unwrap_or(Rlimit { soft: RLIM_INFINITY, hard: RLIM_INFINITY })
    }

    pub fn set(&mut self, res: Resource, new: Rlimit, is_root: bool) -> Result<(), &'static str> {
        let cur = self.get(res);
        // The hard limit can only go down for non-root.
        if !is_root && new.hard > cur.hard {
            return Err("permission denied");
        }
        if new.soft > new.hard {
            return Err("invalid argument (soft > hard)");
        }
        self.by_resource.insert(res as u32, new);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = (Resource, Rlimit)> + '_ {
        self.by_resource.iter().filter_map(|(&k, &v)| {
            Resource::from_u32(k).map(|r| (r, v))
        })
    }
}

pub static LIMITS: Mutex<LimitsTable> = Mutex::new(LimitsTable::new());

pub fn init() {
    LIMITS.lock().install_defaults();
}

/// Format an u64 limit for display: prints `unlimited` when it's
/// RLIM_INFINITY.
pub fn fmt_limit(v: u64) -> alloc::string::String {
    if v == RLIM_INFINITY { alloc::string::String::from("unlimited") }
    else { alloc::format!("{}", v) }
}
