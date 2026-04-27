//! Advisory file locking (flock(2)).
//!
//! Tracks per-path lock state in a global table.  Only advisory: nobody is
//! prevented from reading or writing a file that has a flock — programs
//! co-operate by checking themselves.
//!
//! Lock kinds:
//!   * LOCK_SH — shared (any number of holders, no exclusive holder)
//!   * LOCK_EX — exclusive (one holder, no other holders)
//!   * LOCK_UN — release whatever the caller holds
//!   * LOCK_NB — non-blocking modifier; returns EWOULDBLOCK on contention

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

pub const LOCK_SH: u32 = 1;
pub const LOCK_EX: u32 = 2;
pub const LOCK_UN: u32 = 8;
pub const LOCK_NB: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone)]
pub struct LockHolder {
    pub owner: usize, // FD or process id; whatever the caller wants to track
    pub kind:  LockKind,
}

pub struct LockTable {
    locks: BTreeMap<String, Vec<LockHolder>>,
}

impl LockTable {
    pub const fn new() -> Self { LockTable { locks: BTreeMap::new() } }

    fn would_conflict(holders: &[LockHolder], owner: usize, kind: LockKind) -> bool {
        match kind {
            LockKind::Shared => {
                // Can coexist with other shared, but not with any exclusive
                // by another owner.
                holders.iter().any(|h| h.owner != owner && h.kind == LockKind::Exclusive)
            }
            LockKind::Exclusive => {
                // Cannot coexist with any other holder.
                holders.iter().any(|h| h.owner != owner)
            }
        }
    }

    pub fn try_lock(&mut self, path: &str, owner: usize, kind: LockKind) -> Result<(), &'static str> {
        let entry = self.locks.entry(path.to_string()).or_default();
        if Self::would_conflict(entry, owner, kind) {
            return Err("would block");
        }
        // Replace or upgrade an existing holder of this owner.
        if let Some(h) = entry.iter_mut().find(|h| h.owner == owner) {
            h.kind = kind;
        } else {
            entry.push(LockHolder { owner, kind });
        }
        Ok(())
    }

    pub fn unlock(&mut self, path: &str, owner: usize) -> bool {
        if let Some(holders) = self.locks.get_mut(path) {
            let before = holders.len();
            holders.retain(|h| h.owner != owner);
            let removed = holders.len() != before;
            if holders.is_empty() {
                self.locks.remove(path);
            }
            return removed;
        }
        false
    }

    pub fn holders(&self, path: &str) -> Vec<LockHolder> {
        self.locks.get(path).cloned().unwrap_or_default()
    }

    pub fn snapshot(&self) -> Vec<(String, Vec<LockHolder>)> {
        self.locks.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

pub static LOCKS: Mutex<LockTable> = Mutex::new(LockTable::new());

/// Public convenience: acquire/upgrade/release based on `op` flags.
/// Returns Ok on success, Err with a static error string on failure.
pub fn flock(path: &str, owner: usize, op: u32) -> Result<(), &'static str> {
    let mut t = LOCKS.lock();
    if op & LOCK_UN != 0 {
        t.unlock(path, owner);
        return Ok(());
    }
    let kind = if op & LOCK_EX != 0 {
        LockKind::Exclusive
    } else if op & LOCK_SH != 0 {
        LockKind::Shared
    } else {
        return Err("invalid op");
    };
    let _nonblock = op & LOCK_NB != 0;

    // Always non-blocking in this implementation (no wait queue yet).
    t.try_lock(path, owner, kind)
}
