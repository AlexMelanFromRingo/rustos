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

/// path → Inode map.  Acts as the dentry cache.  The inode number on each
/// entry comes from the backing filesystem (`VfsContext::ino()`), so two
/// paths that resolve to the same inode share its number.
pub struct DentryCache {
    entries: BTreeMap<String, Inode>,
}

impl DentryCache {
    pub const fn new() -> Self { DentryCache { entries: BTreeMap::new() } }

    fn build_from_vfs(path: &str) -> Option<Inode> {
        let info = VfsContext::stat(path).ok()?;
        let ino = if info.ino != 0 {
            info.ino
        } else {
            VfsContext::ino(path)
        };
        let nlink = if info.nlink != 0 { info.nlink } else { VfsContext::nlink(path) };
        Some(Inode {
            ino,
            path: path.to_string(),
            file_type: info.file_type,
            mode: info.mode,
            uid: info.uid,
            gid: info.gid,
            size: info.size,
            nlink,
            last_seen: crate::task::timer::current_ticks(),
        })
    }

    pub fn lookup(&mut self, path: &str) -> Option<Inode> {
        if let Some(existing) = self.entries.get(path).cloned() {
            return Some(existing);
        }
        let inode = Self::build_from_vfs(path)?;
        self.entries.insert(path.to_string(), inode.clone());
        Some(inode)
    }

    pub fn refresh(&mut self, path: &str) -> Option<Inode> {
        let inode = Self::build_from_vfs(path)?;
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
