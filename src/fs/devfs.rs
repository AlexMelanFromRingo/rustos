/// /dev virtual filesystem (devtmpfs-style auto-population).
///
/// Subsystems register device nodes at boot; the table is read by VFS
/// listing/exists/read paths.  Each entry carries a kind (Char/Block) and
/// major/minor numbers so `ls -l /dev` can display Linux-style metadata.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevKind {
    Char,
    Block,
}

#[derive(Debug, Clone)]
pub struct DevNode {
    pub name: String,
    pub kind: DevKind,
    pub major: u32,
    pub minor: u32,
}

static DEV_NODES: Mutex<Vec<DevNode>> = Mutex::new(Vec::new());

/// Register a device node.  Idempotent on `name`.
pub fn register(name: &str, kind: DevKind, major: u32, minor: u32) {
    let mut t = DEV_NODES.lock();
    if t.iter().any(|n| n.name == name) { return; }
    t.push(DevNode { name: name.to_string(), kind, major, minor });
}

/// Look up a node by name.
pub fn lookup(name: &str) -> Option<DevNode> {
    DEV_NODES.lock().iter().find(|n| n.name == name).cloned()
}

/// Snapshot the registry.
pub fn list_nodes() -> Vec<DevNode> {
    DEV_NODES.lock().clone()
}

/// Bring up the standard /dev population at boot — Linux-style major/minor.
pub fn install_default_nodes() {
    register("null",    DevKind::Char, 1, 3);
    register("zero",    DevKind::Char, 1, 5);
    register("random",  DevKind::Char, 1, 8);
    register("urandom", DevKind::Char, 1, 9);
    register("kmsg",    DevKind::Char, 1, 11);
    register("mem",     DevKind::Char, 1, 1);
    register("console", DevKind::Char, 5, 1);
    register("tty",     DevKind::Char, 5, 0);
    register("tty0",    DevKind::Char, 4, 0);
    register("ptmx",    DevKind::Char, 5, 2);
}

/// Read a /dev virtual file. Returns content as bytes, or None if not found.
pub fn read_dev(path: &str) -> Option<Vec<u8>> {
    let path = path.trim_start_matches("/dev");
    let path = path.trim_start_matches('/');

    match path {
        "" | "." => {
            // List /dev directory from the device registry.
            let mut content = String::new();
            for n in list_nodes() {
                content.push_str(&n.name);
                content.push('\n');
            }
            Some(content.into_bytes())
        }

        // /dev/null — reads return empty (EOF)
        "null" => Some(Vec::new()),

        // /dev/zero — reads return zeros
        "zero" => {
            let buf = alloc::vec![0u8; 4096];
            Some(buf)
        }

        // /dev/random, /dev/urandom — reads return pseudo-random bytes
        "random" | "urandom" => {
            let mut buf = Vec::with_capacity(256);
            // Use PIT ticks + TSC for entropy
            let ticks = crate::task::timer::current_ticks();
            let mut state = ticks.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);

            for _ in 0..256 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                buf.push((state >> 33) as u8);
            }
            Some(buf)
        }

        // /dev/console, /dev/tty, /dev/tty0 — reads from TTY input buffer
        "console" | "tty" | "tty0" => {
            let mut tty = crate::tty::TTY0.lock();
            let mut buf = [0u8; 4096];
            let n = tty.read(&mut buf);
            Some(buf[..n].to_vec())
        }

        // /dev/kmsg — kernel log ring buffer (same as dmesg)
        "kmsg" => {
            let entries = crate::klog::read_all();
            let mut content = String::new();
            for entry in &entries {
                content.push_str(entry);
                content.push('\n');
            }
            Some(content.into_bytes())
        }

        // /dev/mem — show memory layout info
        "mem" => {
            let (total, allocated, free) = crate::memory::memory_stats();
            let mut content = String::new();
            content.push_str(&format!("Physical frames: total={}, allocated={}, free={}\n",
                total, allocated, free));
            content.push_str(&format!("Physical memory: {} MiB total, {} MiB free\n",
                total * 4096 / (1024 * 1024),
                free * 4096 / (1024 * 1024)));
            Some(content.into_bytes())
        }

        _ => None,
    }
}

/// Write to a /dev virtual file. Returns Ok(()) if accepted, None if not found.
pub fn write_dev(path: &str, data: &[u8]) -> Option<Result<(), &'static str>> {
    let path = path.trim_start_matches("/dev");
    let path = path.trim_start_matches('/');

    match path {
        // /dev/null — writes are silently discarded
        "null" => Some(Ok(())),

        // /dev/console, /dev/tty — writes go to VGA + serial
        "console" | "tty" => {
            if let Ok(s) = core::str::from_utf8(data) {
                crate::print!("{}", s);
            } else {
                for &byte in data {
                    crate::print!("{}", byte as char);
                }
            }
            Some(Ok(()))
        }

        // /dev/kmsg — writes go to kernel log
        "kmsg" => {
            if let Ok(s) = core::str::from_utf8(data) {
                crate::klog_info!("/dev/kmsg: {}", s.trim_end());
            }
            Some(Ok(()))
        }

        // /dev/zero, /dev/random, /dev/urandom — writes discarded
        "zero" | "random" | "urandom" => Some(Ok(())),

        _ => None,
    }
}

/// Check if a path is in /dev
pub fn is_dev_path(path: &str) -> bool {
    path == "/dev" || path.starts_with("/dev/")
}

/// Check if a /dev path exists
pub fn exists(path: &str) -> bool {
    let path = path.trim_start_matches("/dev");
    let path = path.trim_start_matches('/');
    if path.is_empty() || path == "." { return true; }
    DEV_NODES.lock().iter().any(|n| n.name == path)
}

/// Check if a /dev path is a directory
pub fn is_directory(path: &str) -> bool {
    let path = path.trim_start_matches("/dev");
    let path = path.trim_start_matches('/');
    path.is_empty() || path == "."
}

/// List /dev entries (returns FileInfos with the right file_type set so
/// `ls -l` shows `c` / `b`).
pub fn list_dev() -> Vec<crate::fs::vfs::FileInfo> {
    use crate::fs::vfs::{FileInfo, VfsFileType};

    list_nodes().into_iter().map(|n| {
        let mut info = FileInfo::new(n.name, 0);
        info.file_type = match n.kind {
            DevKind::Char  => VfsFileType::CharDevice,
            DevKind::Block => VfsFileType::BlockDevice,
        };
        info.mode = 0o660;
        info
    }).collect()
}
