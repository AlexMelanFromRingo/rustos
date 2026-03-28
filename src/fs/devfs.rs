/// /dev virtual filesystem
///
/// Provides device nodes as virtual files.
/// Supports: null, zero, random, urandom, console, tty

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Read a /dev virtual file. Returns content as bytes, or None if not found.
pub fn read_dev(path: &str) -> Option<Vec<u8>> {
    let path = path.trim_start_matches("/dev");
    let path = path.trim_start_matches('/');

    match path {
        "" | "." => {
            // List /dev directory
            let mut content = String::new();
            content.push_str("null\n");
            content.push_str("zero\n");
            content.push_str("random\n");
            content.push_str("urandom\n");
            content.push_str("console\n");
            content.push_str("tty\n");
            content.push_str("kmsg\n");
            content.push_str("mem\n");
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

        // /dev/console, /dev/tty — reads return empty (no input buffer)
        "console" | "tty" => Some(Vec::new()),

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

    matches!(path,
        "" | "." | "null" | "zero" | "random" | "urandom" |
        "console" | "tty" | "kmsg" | "mem"
    )
}

/// Check if a /dev path is a directory
pub fn is_directory(path: &str) -> bool {
    let path = path.trim_start_matches("/dev");
    let path = path.trim_start_matches('/');
    path.is_empty() || path == "."
}

/// List /dev entries
pub fn list_dev() -> Vec<crate::fs::vfs::FileInfo> {
    use crate::fs::vfs::FileInfo;
    use alloc::string::ToString;

    alloc::vec![
        FileInfo::new("null".to_string(), 0),
        FileInfo::new("zero".to_string(), 0),
        FileInfo::new("random".to_string(), 0),
        FileInfo::new("urandom".to_string(), 0),
        FileInfo::new("console".to_string(), 0),
        FileInfo::new("tty".to_string(), 0),
        FileInfo::new("kmsg".to_string(), 0),
        FileInfo::new("mem".to_string(), 0),
    ]
}
