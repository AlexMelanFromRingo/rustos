//! Minimal seccomp-style syscall filter.
//!
//! Each process can install one filter: a per-syscall action lookup table.
//! When a syscall is dispatched and the calling process has a filter
//! installed, the filter is consulted before the syscall runs.
//!
//! Supported actions: ALLOW (default), KILL (terminate process), ERRNO
//! (return -EPERM), LOG (allow but record).  No BPF program execution —
//! the filter is a flat byte map.

use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Action {
    Allow = 0,
    Errno = 1,
    Kill  = 2,
    Log   = 3,
}

impl Action {
    pub fn from_u8(b: u8) -> Self {
        match b {
            1 => Action::Errno,
            2 => Action::Kill,
            3 => Action::Log,
            _ => Action::Allow,
        }
    }
}

/// Filter table: 512 entries (covers our entire syscall number space).
const NUM_SYSCALLS: usize = 512;

#[derive(Debug, Clone, Copy)]
pub struct Filter {
    actions: [u8; NUM_SYSCALLS],
    /// Default action for syscalls not explicitly mentioned.
    default: u8,
    /// Process this filter belongs to.  0 means "global default".
    pub pid: usize,
}

impl Filter {
    pub fn new(default: Action) -> Self {
        Filter { actions: [default as u8; NUM_SYSCALLS], default: default as u8, pid: 0 }
    }
    pub fn set(&mut self, syscall_num: usize, action: Action) {
        if syscall_num < NUM_SYSCALLS { self.actions[syscall_num] = action as u8; }
    }
    pub fn lookup(&self, syscall_num: usize) -> Action {
        let raw = self.actions.get(syscall_num).copied().unwrap_or(self.default);
        Action::from_u8(raw)
    }
}

/// Per-process filters.  We use a small fixed slot table rather than a
/// BTreeMap to avoid allocation in the hot syscall path.
const MAX_FILTERS: usize = 16;

pub struct FilterTable {
    slots: [Option<Filter>; MAX_FILTERS],
}

impl FilterTable {
    pub const fn new() -> Self {
        const N: Option<Filter> = None;
        FilterTable { slots: [N; MAX_FILTERS] }
    }

    pub fn install(&mut self, pid: usize, f: Filter) -> bool {
        // Replace if already installed for this pid.
        for slot in self.slots.iter_mut() {
            if let Some(existing) = slot {
                if existing.pid == pid { *existing = Filter { pid, ..f }; return true; }
            }
        }
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                let mut filt = f;
                filt.pid = pid;
                *slot = Some(filt);
                return true;
            }
        }
        false
    }

    pub fn for_pid(&self, pid: usize) -> Option<Filter> {
        self.slots.iter().filter_map(|s| *s).find(|f| f.pid == pid)
    }

    pub fn remove(&mut self, pid: usize) {
        for slot in self.slots.iter_mut() {
            if let Some(f) = slot { if f.pid == pid { *slot = None; return; } }
        }
    }

    pub fn count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

pub static FILTERS: Mutex<FilterTable> = Mutex::new(FilterTable::new());

/// Look up the action for the current process+syscall.  Allow if no filter
/// is installed.  Used by the syscall dispatcher.
pub fn check(pid: usize, syscall_num: usize) -> Action {
    let table = FILTERS.lock();
    match table.for_pid(pid) {
        Some(f) => f.lookup(syscall_num),
        None => Action::Allow,
    }
}
