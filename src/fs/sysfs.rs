/// /sys virtual filesystem
///
/// Exposes kernel and hardware information as a hierarchical virtual filesystem.
/// Modeled after Linux sysfs with simplified structure.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use crate::fs::vfs::FileInfo;

/// Read a /sys virtual file. Returns content as bytes, or None if not found.
pub fn read_sys(path: &str) -> Option<Vec<u8>> {
    let path = path.trim_start_matches("/sys");
    let path = path.trim_start_matches('/');

    match path {
        "" | "." => {
            Some("kernel\ndevices\n".as_bytes().to_vec())
        }

        // /sys/kernel directory
        "kernel" => {
            Some("hostname\nostype\nosrelease\nversion\n".as_bytes().to_vec())
        }

        "kernel/hostname" => {
            Some("rustos\n".as_bytes().to_vec())
        }

        "kernel/ostype" => {
            Some("RustOS\n".as_bytes().to_vec())
        }

        "kernel/osrelease" => {
            Some("0.1.0\n".as_bytes().to_vec())
        }

        "kernel/version" => {
            let ticks = crate::task::timer::current_ticks();
            let uptime = ticks / 18;
            Some(format!("RustOS 0.1.0 (uptime {}s)\n", uptime).into_bytes())
        }

        // /sys/devices directory
        "devices" => {
            Some("platform\nvirtual\n".as_bytes().to_vec())
        }

        "devices/platform" => {
            Some("serial0\nkeyboard0\ntimer0\nrtc0\n".as_bytes().to_vec())
        }

        "devices/platform/serial0" => {
            Some("type: 16550A\nport: 0x3F8\nirq: 4\nbaud: 115200\n".as_bytes().to_vec())
        }

        "devices/platform/keyboard0" => {
            Some("type: PS/2\nport: 0x60\nirq: 1\nlayout: US104\n".as_bytes().to_vec())
        }

        "devices/platform/timer0" => {
            let ticks = crate::task::timer::current_ticks();
            Some(format!("type: PIT 8254\nfrequency: 18 Hz\nticks: {}\n", ticks).into_bytes())
        }

        "devices/platform/rtc0" => {
            Some("type: MC146818\nport: 0x70-0x71\nirq: 8\n".as_bytes().to_vec())
        }

        "devices/virtual" => {
            Some("vga0\n".as_bytes().to_vec())
        }

        "devices/virtual/vga0" => {
            Some("type: VGA text mode\nresolution: 80x25\nbase: 0xB8000\n".as_bytes().to_vec())
        }

        _ => None,
    }
}

/// Check if a path is a /sys path
pub fn is_sys_path(path: &str) -> bool {
    path == "/sys" || path.starts_with("/sys/")
}

/// Check if a /sys path exists
pub fn exists(path: &str) -> bool {
    read_sys(path).is_some()
}

/// Check if a /sys path is a directory
pub fn is_directory(path: &str) -> bool {
    let path = path.trim_start_matches("/sys");
    let path = path.trim_start_matches('/');

    matches!(path,
        "" | "." | "kernel" | "devices" | "devices/platform" | "devices/virtual"
    )
}

/// List entries in a /sys directory
pub fn list_sys(path: &str) -> Vec<FileInfo> {
    let path = path.trim_start_matches("/sys");
    let path = path.trim_start_matches('/');

    match path {
        "" | "." => vec![
            FileInfo::directory(String::from("kernel")),
            FileInfo::directory(String::from("devices")),
        ],
        "kernel" => vec![
            FileInfo::new(String::from("hostname"), 0),
            FileInfo::new(String::from("ostype"), 0),
            FileInfo::new(String::from("osrelease"), 0),
            FileInfo::new(String::from("version"), 0),
        ],
        "devices" => vec![
            FileInfo::directory(String::from("platform")),
            FileInfo::directory(String::from("virtual")),
        ],
        "devices/platform" => vec![
            FileInfo::new(String::from("serial0"), 0),
            FileInfo::new(String::from("keyboard0"), 0),
            FileInfo::new(String::from("timer0"), 0),
            FileInfo::new(String::from("rtc0"), 0),
        ],
        "devices/virtual" => vec![
            FileInfo::new(String::from("vga0"), 0),
        ],
        _ => Vec::new(),
    }
}
