//! Application-level syslog facility (RFC 3164 BSD syslog inspired).
//!
//! Stores recent syslog entries in an in-memory ring buffer.  A flush task
//! periodically appends new entries to /var/log/messages.  The `logger`
//! shell command is the user-facing producer; kernel modules can use the
//! [`syslog_info!`] / [`syslog_warn!`] / [`syslog_err!`] macros.

use alloc::collections::VecDeque;
use alloc::string::String;
use spin::Mutex;

/// RFC 3164 facility codes.  Only the most common values are exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Facility {
    Kern    = 0,
    User    = 1,
    Mail    = 2,
    Daemon  = 3,
    Auth    = 4,
    Syslog  = 5,
    Lpr     = 6,
    News    = 7,
    Uucp    = 8,
    Cron    = 9,
    Authpriv = 10,
    Local0  = 16,
    Local1  = 17,
    Local2  = 18,
    Local3  = 19,
    Local4  = 20,
    Local5  = 21,
    Local6  = 22,
    Local7  = 23,
}

impl Facility {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "kern" => Some(Facility::Kern),
            "user" => Some(Facility::User),
            "mail" => Some(Facility::Mail),
            "daemon" => Some(Facility::Daemon),
            "auth" => Some(Facility::Auth),
            "syslog" => Some(Facility::Syslog),
            "lpr" => Some(Facility::Lpr),
            "news" => Some(Facility::News),
            "uucp" => Some(Facility::Uucp),
            "cron" => Some(Facility::Cron),
            "authpriv" => Some(Facility::Authpriv),
            "local0" => Some(Facility::Local0),
            "local1" => Some(Facility::Local1),
            "local2" => Some(Facility::Local2),
            "local3" => Some(Facility::Local3),
            "local4" => Some(Facility::Local4),
            "local5" => Some(Facility::Local5),
            "local6" => Some(Facility::Local6),
            "local7" => Some(Facility::Local7),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Facility::Kern => "kern",
            Facility::User => "user",
            Facility::Mail => "mail",
            Facility::Daemon => "daemon",
            Facility::Auth => "auth",
            Facility::Syslog => "syslog",
            Facility::Lpr => "lpr",
            Facility::News => "news",
            Facility::Uucp => "uucp",
            Facility::Cron => "cron",
            Facility::Authpriv => "authpriv",
            Facility::Local0 => "local0",
            Facility::Local1 => "local1",
            Facility::Local2 => "local2",
            Facility::Local3 => "local3",
            Facility::Local4 => "local4",
            Facility::Local5 => "local5",
            Facility::Local6 => "local6",
            Facility::Local7 => "local7",
        }
    }
}

/// RFC 3164 severity codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Severity {
    Emerg   = 0,
    Alert   = 1,
    Crit    = 2,
    Err     = 3,
    Warning = 4,
    Notice  = 5,
    Info    = 6,
    Debug   = 7,
}

impl Severity {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "emerg" | "panic" => Some(Severity::Emerg),
            "alert" => Some(Severity::Alert),
            "crit" => Some(Severity::Crit),
            "err" | "error" => Some(Severity::Err),
            "warning" | "warn" => Some(Severity::Warning),
            "notice" => Some(Severity::Notice),
            "info" => Some(Severity::Info),
            "debug" => Some(Severity::Debug),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Emerg => "emerg",
            Severity::Alert => "alert",
            Severity::Crit => "crit",
            Severity::Err => "err",
            Severity::Warning => "warning",
            Severity::Notice => "notice",
            Severity::Info => "info",
            Severity::Debug => "debug",
        }
    }
}

/// One syslog entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub tick: u64,
    pub facility: Facility,
    pub severity: Severity,
    pub tag: String,
    pub message: String,
}

impl Entry {
    /// PRI value as per RFC 3164: facility * 8 + severity.
    pub fn pri(&self) -> u8 {
        ((self.facility as u8) * 8) + (self.severity as u8)
    }

    /// Format as a single line approximating BSD syslog format:
    /// `<PRI>YYYY-MM-DD hh:mm:ss host tag: message`
    pub fn format_line(&self, hostname: &str) -> String {
        let dt = crate::drivers::rtc::read_datetime();
        let mut buf = [0u8; 32];
        let len = dt.format(&mut buf);
        let date = core::str::from_utf8(&buf[..len]).unwrap_or("?");
        alloc::format!(
            "<{}>{} {} {}: {}",
            self.pri(), date, hostname, self.tag, self.message
        )
    }
}

const RING_SIZE: usize = 512;

pub struct SyslogRing {
    entries: VecDeque<Entry>,
    /// Index of the next unflushed entry (relative to entries).  We compact
    /// after flush rather than maintain it precisely for simplicity: every
    /// entry past `flushed_through` (a sequence number) is unflushed.
    seq: u64,
    flushed_through: u64,
}

impl SyslogRing {
    pub const fn new() -> Self {
        SyslogRing {
            entries: VecDeque::new(),
            seq: 0,
            flushed_through: 0,
        }
    }

    pub fn log(&mut self, fac: Facility, sev: Severity, tag: &str, message: String) {
        let entry = Entry {
            tick: crate::task::timer::current_ticks(),
            facility: fac,
            severity: sev,
            tag: alloc::string::ToString::to_string(tag),
            message,
        };
        self.seq += 1;
        if self.entries.len() == RING_SIZE {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn snapshot(&self) -> alloc::vec::Vec<Entry> {
        self.entries.iter().cloned().collect()
    }

    /// Take all entries written since the last flush.  Bumps the watermark.
    pub fn drain_for_flush(&mut self) -> alloc::vec::Vec<Entry> {
        let total = self.seq;
        let new = total - self.flushed_through;
        self.flushed_through = total;
        let take = (new as usize).min(self.entries.len());
        let start = self.entries.len() - take;
        self.entries.range(start..).cloned().collect()
    }
}

pub static SYSLOG: Mutex<SyslogRing> = Mutex::new(SyslogRing::new());

/// Convenience top-level loggers.
pub fn log(facility: Facility, severity: Severity, tag: &str, message: alloc::string::String) {
    SYSLOG.lock().log(facility, severity, tag, message);
}

#[macro_export]
macro_rules! syslog_info {
    ($tag:expr, $($arg:tt)*) => {
        $crate::syslog::log(
            $crate::syslog::Facility::Daemon,
            $crate::syslog::Severity::Info,
            $tag,
            ::alloc::format!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! syslog_warn {
    ($tag:expr, $($arg:tt)*) => {
        $crate::syslog::log(
            $crate::syslog::Facility::Daemon,
            $crate::syslog::Severity::Warning,
            $tag,
            ::alloc::format!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! syslog_err {
    ($tag:expr, $($arg:tt)*) => {
        $crate::syslog::log(
            $crate::syslog::Facility::Daemon,
            $crate::syslog::Severity::Err,
            $tag,
            ::alloc::format!($($arg)*),
        )
    };
}

/// Append all unflushed entries to /var/log/messages.  Called periodically
/// by the syslogd async task.
pub fn flush_to_messages_log(hostname: &str) {
    use crate::fs::vfs::VfsContext;

    let entries = SYSLOG.lock().drain_for_flush();
    if entries.is_empty() { return; }

    // Append to existing content.
    let prior = VfsContext::read("/var/log/messages").unwrap_or_default();
    let mut combined = alloc::string::String::from_utf8(prior).unwrap_or_default();
    for e in entries {
        combined.push_str(&e.format_line(hostname));
        combined.push('\n');
    }
    let _ = VfsContext::write("/var/log/messages", combined.into_bytes());
}
