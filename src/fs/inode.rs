//! Lightweight inode + dentry layer on top of the existing VFS.
//!
//! The current filesystem implementations (ramdisk, fat32, procfs, ...)
//! key everything off paths and don't expose stable inode numbers.  This
//! module sits beside them and assigns a stable inode number to every
//! path the kernel touches, plus caches recent path → inode lookups
//! ("dentries").
//!
//! Inode numbers come from a monotonically-increasing counter, which means
//! they are stable for the lifetime of the kernel run but are not
//! filesystem-rooted (real Linux inodes come from the on-disk filesystem).

use crate::fs::vfs::{VfsContext, VfsFileType};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Per-inode metadata.  Mirrors a subset of `FileInfo` plus stable identity.
#[derive(Debug, Clone)]
pub struct Inode {
    pub ino:        u64,
    pub path:       String,
    pub file_type:  VfsFileType,
    pub mode:       u16,
    pub uid:        u32,
    pub gid:        u32,
    pub size:       usize,
    pub nlink:      u32,
    pub last_seen:  u64,  // tick when we last refreshed from VFS
}

static NEXT_INO: AtomicU64 = AtomicU64::new(1);

fn fresh_ino() -> u64 { NEXT_INO.fetch_add(1, Ordering::Relaxed) }

/// path → Inode map.  Acts as the dentry cache.
pub struct DentryCache {
    entries: BTreeMap<String, Inode>,
}

impl DentryCache {
    pub const fn new() -> Self { DentryCache { entries: BTreeMap::new() } }

    /// Look up `path`, refreshing from VFS if not cached.  Returns a clone.
    pub fn lookup(&mut self, path: &str) -> Option<Inode> {
        if let Some(existing) = self.entries.get(path).cloned() {
            return Some(existing);
        }
        let info = VfsContext::stat(path).ok()?;
        let inode = Inode {
            ino: fresh_ino(),
            path: path.to_string(),
            file_type: info.file_type,
            mode: info.mode,
            uid: info.uid,
            gid: info.gid,
            size: info.size,
            nlink: 1, // we don't track real link counts
            last_seen: crate::task::timer::current_ticks(),
        };
        self.entries.insert(path.to_string(), inode.clone());
        Some(inode)
    }

    /// Force a refresh of `path` from VFS; stale entries get the fresh data
    /// while keeping their ino number.
    pub fn refresh(&mut self, path: &str) -> Option<Inode> {
        let info = VfsContext::stat(path).ok()?;
        let now = crate::task::timer::current_ticks();
        if let Some(existing) = self.entries.get_mut(path) {
            existing.file_type = info.file_type;
            existing.mode = info.mode;
            existing.uid = info.uid;
            existing.gid = info.gid;
            existing.size = info.size;
            existing.last_seen = now;
            return Some(existing.clone());
        }
        let inode = Inode {
            ino: fresh_ino(),
            path: path.to_string(),
            file_type: info.file_type,
            mode: info.mode,
            uid: info.uid,
            gid: info.gid,
            size: info.size,
            nlink: 1,
            last_seen: now,
        };
        self.entries.insert(path.to_string(), inode.clone());
        Some(inode)
    }

    pub fn invalidate(&mut self, path: &str) {
        self.entries.remove(path);
    }

    pub fn snapshot(&self) -> Vec<Inode> {
        self.entries.values().cloned().collect()
    }

    pub fn len(&self) -> usize { self.entries.len() }
}

pub static DCACHE: Mutex<DentryCache> = Mutex::new(DentryCache::new());

/// Public convenience wrapper.
pub fn lookup(path: &str) -> Option<Inode> { DCACHE.lock().lookup(path) }
pub fn refresh(path: &str) -> Option<Inode> { DCACHE.lock().refresh(path) }
pub fn invalidate(path: &str) { DCACHE.lock().invalidate(path) }
pub fn snapshot() -> Vec<Inode> { DCACHE.lock().snapshot() }
