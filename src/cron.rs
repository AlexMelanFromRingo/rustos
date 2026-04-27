//! Cron-style periodic task scheduler.
//!
//! Simpler than POSIX cron: each entry has an integer-second interval and a
//! command (which we currently log via syslog rather than execute, since we
//! don't yet have an in-kernel sh evaluator).  The crond task fires at every
//! tick of the executor and re-checks due jobs.
//!
//! The crontab is persisted at /etc/crontab in a one-line-per-entry format:
//!
//!   # interval_secs  command
//!   60               echo cron-fire-1m
//!   3600             echo cron-fire-1h

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Debug, Clone)]
pub struct CronJob {
    pub interval_secs: u64,
    pub command: String,
    pub last_run_tick: u64,
    pub run_count: u64,
}

impl CronJob {
    pub fn new(interval_secs: u64, command: &str) -> Self {
        CronJob {
            interval_secs,
            command: alloc::string::ToString::to_string(command),
            last_run_tick: crate::task::timer::current_ticks(),
            run_count: 0,
        }
    }

    fn is_due(&self, now: u64) -> bool {
        let elapsed_ticks = now.saturating_sub(self.last_run_tick);
        // PIT runs at ~18.2 Hz, so 18.2 ticks ≈ 1 second.
        let elapsed_secs = elapsed_ticks * 11 / 200; // ≈ ticks/18.18
        elapsed_secs >= self.interval_secs
    }
}

pub struct CronTab {
    pub jobs: Vec<CronJob>,
}

impl CronTab {
    pub const fn new() -> Self { CronTab { jobs: Vec::new() } }

    pub fn add(&mut self, interval_secs: u64, command: &str) {
        self.jobs.push(CronJob::new(interval_secs, command));
    }

    /// Remove jobs by 1-based index from the listing order.
    pub fn remove(&mut self, idx: usize) -> bool {
        if idx == 0 || idx > self.jobs.len() { return false; }
        self.jobs.remove(idx - 1);
        true
    }

    pub fn list(&self) -> &[CronJob] { &self.jobs }
}

pub static CRONTAB: Mutex<CronTab> = Mutex::new(CronTab::new());

/// Serialize the crontab to /etc/crontab.
pub fn save() {
    use crate::fs::vfs::VfsContext;
    let table = CRONTAB.lock();
    let mut out = String::from("# interval_secs  command\n");
    for job in &table.jobs {
        out.push_str(&alloc::format!("{} {}\n", job.interval_secs, job.command));
    }
    let _ = VfsContext::write("/etc/crontab", out.into_bytes());
}

/// Re-read /etc/crontab into the in-memory tab.  Existing entries are
/// replaced.
pub fn reload() {
    use crate::fs::vfs::VfsContext;
    let data = match VfsContext::read("/etc/crontab") {
        Ok(d) => d,
        Err(_) => return,
    };
    let text = match core::str::from_utf8(&data) {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut table = CRONTAB.lock();
    table.jobs.clear();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let interval: u64 = match parts.next().and_then(|s| s.parse().ok()) {
            Some(n) => n,
            None => continue,
        };
        let cmd = parts.next().unwrap_or("").trim();
        if cmd.is_empty() { continue; }
        table.add(interval, cmd);
    }
}

/// Install one default sample job so /etc/crontab is non-empty after first
/// boot.  Real Linux ships a minimal default crontab too.
pub fn install_defaults() {
    let mut table = CRONTAB.lock();
    if !table.jobs.is_empty() { return; }
    table.add(300, "echo cron: 5-minute heartbeat");
}

/// Tick-driven dispatcher.  Walks every job, fires the due ones, queues
/// the command for the shell to run, and logs the dispatch through syslog
/// (facility=cron, severity=info).  Returns the number of jobs fired.
pub fn tick() -> usize {
    let now = crate::task::timer::current_ticks();
    let mut fired = 0usize;
    let mut to_dispatch: alloc::vec::Vec<(u64, alloc::string::String)> = alloc::vec::Vec::new();
    {
        let mut table = CRONTAB.lock();
        for job in table.jobs.iter_mut() {
            if job.is_due(now) {
                job.last_run_tick = now;
                job.run_count += 1;
                fired += 1;
                to_dispatch.push((job.run_count, job.command.clone()));
            }
        }
    }
    // Dispatch outside the lock so command execution can take CRONTAB later
    // without deadlocking against itself.
    for (run, cmd) in to_dispatch {
        crate::syslog::log(
            crate::syslog::Facility::Cron,
            crate::syslog::Severity::Info,
            "CRON",
            alloc::format!("({}) CMD ({})", run, cmd),
        );
        // Actually execute: build a transient Shell, capture output, log it.
        let output = run_in_transient_shell(&cmd);
        if !output.trim().is_empty() {
            crate::syslog::log(
                crate::syslog::Facility::Cron,
                crate::syslog::Severity::Info,
                "CRON-OUT",
                output.trim_end_matches('\n').to_string(),
            );
        }
    }
    fired
}

/// Run `cmd` on a one-shot Shell, returning its captured output.  Used by
/// cron and other background dispatchers that don't have the user's shell.
fn run_in_transient_shell(cmd: &str) -> alloc::string::String {
    crate::vga_buffer::start_capture();
    let mut sh = crate::shell::Shell::new();
    sh.set_buffer_for(cmd);
    sh.execute();
    crate::vga_buffer::stop_capture()
}
