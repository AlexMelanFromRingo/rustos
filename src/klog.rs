/// Kernel logging subsystem (dmesg)
///
/// Provides a ring buffer for kernel log messages with log levels.
/// Messages can be read via the `dmesg` shell command or /proc/kmsg.

use alloc::string::String;
use alloc::format;
use core::fmt;
use spin::Mutex;

/// Log level (matches Linux syslog levels)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Emergency = 0,
    Alert = 1,
    Critical = 2,
    Error = 3,
    Warning = 4,
    Notice = 5,
    Info = 6,
    Debug = 7,
}

impl LogLevel {
    pub fn prefix(&self) -> &'static str {
        match self {
            LogLevel::Emergency => "EMERG",
            LogLevel::Alert => "ALERT",
            LogLevel::Critical => "CRIT",
            LogLevel::Error => "ERR",
            LogLevel::Warning => "WARN",
            LogLevel::Notice => "NOTICE",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.prefix())
    }
}

/// A single log entry
struct LogEntry {
    timestamp_ticks: u64,
    level: LogLevel,
    message: String,
}

/// Ring buffer size (number of entries)
const RING_BUFFER_SIZE: usize = 512;

/// Kernel log ring buffer
struct KernelLog {
    buffer: alloc::vec::Vec<LogEntry>,
    write_pos: usize,
    count: usize,
    /// Sequence number for tracking reads
    sequence: u64,
}

impl KernelLog {
    const fn new() -> Self {
        KernelLog {
            buffer: alloc::vec::Vec::new(),
            write_pos: 0,
            count: 0,
            sequence: 0,
        }
    }

    fn ensure_capacity(&mut self) {
        if self.buffer.capacity() == 0 {
            self.buffer.reserve(RING_BUFFER_SIZE);
        }
    }

    /// Add a log entry
    fn log(&mut self, level: LogLevel, message: String) {
        self.ensure_capacity();

        let ticks = crate::task::timer::current_ticks();
        let entry = LogEntry {
            timestamp_ticks: ticks,
            level,
            message,
        };

        if self.buffer.len() < RING_BUFFER_SIZE {
            self.buffer.push(entry);
        } else {
            self.buffer[self.write_pos] = entry;
        }
        self.write_pos = (self.write_pos + 1) % RING_BUFFER_SIZE;
        if self.count < RING_BUFFER_SIZE {
            self.count += 1;
        }
        self.sequence += 1;
    }

    /// Get all log entries in chronological order
    fn read_all(&self) -> alloc::vec::Vec<String> {
        let mut result = alloc::vec::Vec::with_capacity(self.count);

        if self.count < RING_BUFFER_SIZE {
            // Buffer hasn't wrapped yet
            for i in 0..self.count {
                let entry = &self.buffer[i];
                result.push(format_entry(entry));
            }
        } else {
            // Buffer has wrapped, start from write_pos (oldest)
            for i in 0..RING_BUFFER_SIZE {
                let idx = (self.write_pos + i) % RING_BUFFER_SIZE;
                let entry = &self.buffer[idx];
                result.push(format_entry(entry));
            }
        }

        result
    }

    /// Get number of entries
    fn len(&self) -> usize {
        self.count
    }
}

fn format_entry(entry: &LogEntry) -> String {
    let secs = entry.timestamp_ticks / 18;
    let frac = (entry.timestamp_ticks % 18) * 100 / 18;
    format!("[{:5}.{:02}] {}: {}", secs, frac, entry.level.prefix(), entry.message)
}

/// Global kernel log instance
static KLOG: Mutex<KernelLog> = Mutex::new(KernelLog::new());

/// Log a message at the given level
pub fn log(level: LogLevel, message: String) {
    KLOG.lock().log(level, message);
}

/// Read all log entries
pub fn read_all() -> alloc::vec::Vec<String> {
    KLOG.lock().read_all()
}

/// Get the number of log entries
pub fn entry_count() -> usize {
    KLOG.lock().len()
}

/// Logging macros

#[macro_export]
macro_rules! klog_info {
    ($($arg:tt)*) => {
        $crate::klog::log($crate::klog::LogLevel::Info, alloc::format!($($arg)*))
    };
}

#[macro_export]
macro_rules! klog_warn {
    ($($arg:tt)*) => {
        $crate::klog::log($crate::klog::LogLevel::Warning, alloc::format!($($arg)*))
    };
}

#[macro_export]
macro_rules! klog_err {
    ($($arg:tt)*) => {
        $crate::klog::log($crate::klog::LogLevel::Error, alloc::format!($($arg)*))
    };
}

#[macro_export]
macro_rules! klog_debug {
    ($($arg:tt)*) => {
        $crate::klog::log($crate::klog::LogLevel::Debug, alloc::format!($($arg)*))
    };
}
