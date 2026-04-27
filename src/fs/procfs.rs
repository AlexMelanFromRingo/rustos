/// /proc virtual filesystem
///
/// Provides process and system information as virtual files.
/// All content is generated on-the-fly when read.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Read a /proc virtual file. Returns content as bytes, or None if not found.
pub fn read_proc(path: &str) -> Option<Vec<u8>> {
    let path = path.trim_start_matches("/proc");
    let path = path.trim_start_matches('/');

    match path {
        "" | "." => {
            // List /proc directory
            let mut content = String::new();
            content.push_str("uptime\n");
            content.push_str("meminfo\n");
            content.push_str("version\n");
            content.push_str("cpuinfo\n");
            content.push_str("kmsg\n");
            content.push_str("loadavg\n");
            content.push_str("stat\n");
            content.push_str("slabinfo\n");
            content.push_str("mounts\n");

            // Add per-process directories
            let pm = crate::process::PROCESS_MANAGER.lock();
            for process in pm.all_processes() {
                if process.state != crate::process::ProcessState::Terminated {
                    content.push_str(&format!("{}\n", process.pid));
                }
            }

            Some(content.into_bytes())
        }

        "uptime" => {
            let ticks = crate::task::timer::current_ticks();
            let secs = ticks / 18;
            let frac = (ticks % 18) * 100 / 18;
            // Format: uptime_seconds idle_seconds
            Some(format!("{}.{:02} 0.00\n", secs, frac).into_bytes())
        }

        "meminfo" => {
            let stats = crate::memory::memory_stats();
            let total_kb = stats.0 * 4; // frames * 4KiB
            let used_kb = stats.1 * 4;
            let free_kb = total_kb - used_kb;

            let mut s = String::new();
            s.push_str(&format!("MemTotal:    {:8} kB\n", total_kb));
            s.push_str(&format!("MemFree:     {:8} kB\n", free_kb));
            s.push_str(&format!("MemUsed:     {:8} kB\n", used_kb));
            s.push_str(&format!("Buffers:     {:8} kB\n", 0));
            s.push_str(&format!("Cached:      {:8} kB\n", 0));
            s.push_str(&format!("SwapTotal:   {:8} kB\n", 0));
            s.push_str(&format!("SwapFree:    {:8} kB\n", 0));
            s.push_str(&format!("PageSize:    {:8} B\n", 4096));
            s.push_str(&format!("TotalFrames: {:8}\n", stats.0));
            s.push_str(&format!("UsedFrames:  {:8}\n", stats.1));
            Some(s.into_bytes())
        }

        "version" => {
            Some(format!("RustOS version {} (rustc {}) {}\n",
                env!("CARGO_PKG_VERSION"),
                "nightly-2024",
                "x86_64"
            ).into_bytes())
        }

        "cpuinfo" => {
            let mut s = String::new();
            s.push_str("processor\t: 0\n");
            s.push_str("vendor_id\t: GenuineIntel\n");
            s.push_str("model name\t: QEMU Virtual CPU\n");
            s.push_str("cpu MHz\t\t: 0.000\n");
            s.push_str("cache size\t: 0 KB\n");
            s.push_str("flags\t\t: fpu de pse tsc msr pae mce cx8 apic\n");
            s.push_str("bogomips\t: 0.00\n");
            Some(s.into_bytes())
        }

        "kmsg" => {
            // Kernel log messages
            let entries = crate::klog::read_all();
            let mut s = String::new();
            for entry in entries {
                s.push_str(&entry);
                s.push('\n');
            }
            Some(s.into_bytes())
        }

        "loadavg" => {
            let pm = crate::process::PROCESS_MANAGER.lock();
            let running = pm.all_processes().iter()
                .filter(|p| p.state == crate::process::ProcessState::Running
                    || p.state == crate::process::ProcessState::Ready)
                .count();
            let total = pm.all_processes().iter()
                .filter(|p| p.state != crate::process::ProcessState::Terminated)
                .count();
            drop(pm);

            // Format: 1min 5min 15min running/total last_pid
            Some(format!("0.00 0.00 0.00 {}/{} 0\n", running, total).into_bytes())
        }

        "stat" => {
            let ticks = crate::task::timer::current_ticks();
            let mut s = String::new();
            s.push_str(&format!("cpu  {} 0 0 {} 0 0 0 0 0 0\n", ticks, ticks));
            s.push_str(&format!("btime {}\n", {
                let dt = crate::drivers::rtc::read_datetime();
                let uptime = ticks / 18;
                dt.to_unix_timestamp().saturating_sub(uptime)
            }));

            let pm = crate::process::PROCESS_MANAGER.lock();
            s.push_str(&format!("processes {}\n", pm.all_processes().len()));
            let running = pm.all_processes().iter()
                .filter(|p| p.state == crate::process::ProcessState::Running)
                .count();
            s.push_str(&format!("procs_running {}\n", running));
            let blocked = pm.all_processes().iter()
                .filter(|p| p.state == crate::process::ProcessState::Blocked)
                .count();
            s.push_str(&format!("procs_blocked {}\n", blocked));

            Some(s.into_bytes())
        }

        "slabinfo" => {
            let mut s = String::new();
            s.push_str("# name             size  per_slab  total_objs  used_objs  free_objs  slabs  allocs  frees  grew\n");
            for (name, st) in crate::slab::SLAB_REGISTRY.snapshot() {
                s.push_str(&format!(
                    "{:<16} {:>6}  {:>8}  {:>10}  {:>9}  {:>9}  {:>5}  {:>6}  {:>5}  {:>4}\n",
                    name, st.obj_size, st.objs_per_slab, st.total_objs, st.used_objs,
                    st.free_objs, st.total_slabs, st.allocs, st.frees, st.grew,
                ));
            }
            Some(s.into_bytes())
        }

        "mounts" => {
            let mut s = String::new();
            for m in crate::fs::vfs::list_mounts() {
                // Linux /proc/mounts format: src target type opts 0 0
                s.push_str(&format!("{} {} {} {} 0 0\n",
                    m.source, m.mount_point, m.fs_type, m.options));
            }
            Some(s.into_bytes())
        }

        _ => {
            // Try to parse as PID for /proc/[pid]/...
            let parts: Vec<&str> = path.splitn(2, '/').collect();
            if let Ok(pid) = parts[0].parse::<usize>() {
                let subpath = if parts.len() > 1 { parts[1] } else { "" };
                read_proc_pid(pid, subpath)
            } else {
                None
            }
        }
    }
}

/// Read per-process /proc/[pid]/ files
fn read_proc_pid(pid: usize, subpath: &str) -> Option<Vec<u8>> {
    let pm = crate::process::PROCESS_MANAGER.lock();
    let process = pm.get_process(pid)?;

    match subpath {
        "" | "." => {
            // List /proc/[pid] directory
            Some("status\ncmdline\nstat\nmaps\nfdinfo\n".as_bytes().to_vec())
        }

        "status" => {
            let mut s = String::new();
            s.push_str(&format!("Name:\tprocess_{}\n", pid));
            s.push_str(&format!("State:\t{}\n", process.state.as_str()));
            s.push_str(&format!("Pid:\t{}\n", pid));
            s.push_str(&format!("PPid:\t{}\n", process.parent_pid.unwrap_or(0)));
            s.push_str(&format!("Uid:\t0\t0\t0\t0\n"));
            s.push_str(&format!("Gid:\t0\t0\t0\t0\n"));
            s.push_str(&format!("VmRSS:\t{} kB\n", process.stack.len() / 1024));
            s.push_str(&format!("Threads:\t1\n"));
            s.push_str(&format!("SigPnd:\t{:016x}\n", process.pending_signals));
            s.push_str(&format!("SigBlk:\t{:016x}\n", process.signal_blocked));
            s.push_str(&format!("Nice:\t{}\n", process.nice));
            Some(s.into_bytes())
        }

        "cmdline" => {
            // We don't track cmdline yet, return process name
            Some(format!("process_{}\0", pid).into_bytes())
        }

        "stat" => {
            // Linux /proc/[pid]/stat format (simplified)
            let state_char = match process.state {
                crate::process::ProcessState::Running => 'R',
                crate::process::ProcessState::Ready => 'R',
                crate::process::ProcessState::Blocked => 'S',
                crate::process::ProcessState::Waiting => 'S',
                crate::process::ProcessState::Zombie => 'Z',
                crate::process::ProcessState::Terminated => 'X',
            };
            let ppid = process.parent_pid.unwrap_or(0);
            Some(format!("{} (process_{}) {} {} 0 0 0 0 0 0 0 0 0 0 0 0 {} 0\n",
                pid, pid, state_char, ppid, process.nice).into_bytes())
        }

        "maps" => {
            let mut s = String::new();
            if let Some(stack_addr) = process.user_stack_addr {
                let stack_size = process.user_stack_size.unwrap_or(0);
                s.push_str(&format!("{:016x}-{:016x} rw-p 00000000 00:00 0 [stack]\n",
                    stack_addr, stack_addr + stack_size));
            }
            if let Some(entry) = process.entry_point {
                s.push_str(&format!("{:016x}-{:016x} r-xp 00000000 00:00 0 [text]\n",
                    entry, entry + 0x1000));
            }
            Some(s.into_bytes())
        }

        "fdinfo" => {
            // Show open file descriptors for this process
            drop(pm); // Release PROCESS_MANAGER before locking FD tables
            let mut s = String::new();
            crate::syscall::filedesc::with_process_fd_table(pid, |table| {
                for fd in 0..256 {
                    if let Some(file) = table.get(fd) {
                        if file.is_open {
                            s.push_str(&format!("{}\t{}\toffset={}\n", fd, file.path, file.offset));
                        }
                    }
                }
            });
            if s.is_empty() {
                s.push_str("(no FD table registered for this process)\n");
            }
            Some(s.into_bytes())
        }

        _ => None,
    }
}

/// Check if a path is a /proc path
pub fn is_proc_path(path: &str) -> bool {
    path == "/proc" || path.starts_with("/proc/")
}

/// Check if a /proc path exists
pub fn exists(path: &str) -> bool {
    read_proc(path).is_some()
}

/// List /proc entries as FileInfo
pub fn list_proc() -> Vec<crate::fs::vfs::FileInfo> {
    use crate::fs::vfs::FileInfo;
    use alloc::string::ToString;

    let mut entries = Vec::new();

    // Static entries
    entries.push(FileInfo::new("uptime".to_string(), 0));
    entries.push(FileInfo::new("meminfo".to_string(), 0));
    entries.push(FileInfo::new("version".to_string(), 0));
    entries.push(FileInfo::new("cpuinfo".to_string(), 0));
    entries.push(FileInfo::new("kmsg".to_string(), 0));
    entries.push(FileInfo::new("loadavg".to_string(), 0));
    entries.push(FileInfo::new("stat".to_string(), 0));
    entries.push(FileInfo::new("slabinfo".to_string(), 0));

    // Per-process directories
    let pm = crate::process::PROCESS_MANAGER.lock();
    for process in pm.all_processes() {
        if process.state != crate::process::ProcessState::Terminated {
            entries.push(FileInfo::directory(format!("{}", process.pid)));
        }
    }

    entries
}

/// Check if a /proc path is a directory
pub fn is_directory(path: &str) -> bool {
    let path = path.trim_start_matches("/proc");
    let path = path.trim_start_matches('/');

    match path {
        "" | "." => true,
        _ => {
            // Check if it's a PID directory
            let parts: Vec<&str> = path.splitn(2, '/').collect();
            if parts.len() == 1 {
                if let Ok(pid) = parts[0].parse::<usize>() {
                    let pm = crate::process::PROCESS_MANAGER.lock();
                    return pm.get_process(pid).is_some();
                }
            }
            false
        }
    }
}
