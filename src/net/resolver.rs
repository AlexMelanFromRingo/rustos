//! Name → address resolver backed by /etc/hosts.
//!
//! No DNS yet — this is only the static-table resolver, equivalent to nsswitch
//! configured with `hosts: files`.  Each line of /etc/hosts is
//!
//!   IPv4-addr  canonical-name  alias1 alias2 ...
//!
//! and lines beginning with `#` are comments.

use super::Ipv4Addr;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// One resolved record: a primary name and a list of additional aliases that
/// all map to the same IPv4 address.
#[derive(Debug, Clone)]
pub struct HostEntry {
    pub addr: Ipv4Addr,
    pub canonical: String,
    pub aliases: Vec<String>,
}

impl HostEntry {
    pub fn matches(&self, name: &str) -> bool {
        self.canonical.eq_ignore_ascii_case(name)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    }
}

/// Parse the /etc/hosts text and return all entries.  Lines that don't start
/// with a parseable IPv4 address are silently skipped.
pub fn parse_hosts(text: &str) -> Vec<HostEntry> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = match raw.find('#') { Some(i) => &raw[..i], None => raw };
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.split_whitespace();
        let addr_str = match parts.next() { Some(s) => s, None => continue };
        let addr = match Ipv4Addr::parse(addr_str) { Some(a) => a, None => continue };
        let canonical = match parts.next() { Some(s) => s.to_string(), None => continue };
        let aliases: Vec<String> = parts.map(|s| s.to_string()).collect();
        out.push(HostEntry { addr, canonical, aliases });
    }
    out
}

/// Resolve `name` to an IPv4 address using /etc/hosts.  Returns `None` if
/// neither a literal address nor a hosts-file match was found.
pub fn resolve(name: &str) -> Option<Ipv4Addr> {
    if let Some(addr) = Ipv4Addr::parse(name) {
        return Some(addr);
    }
    let data = match crate::fs::vfs::VfsContext::read("/etc/hosts") {
        Ok(d) => d,
        Err(_) => return None,
    };
    let text = match core::str::from_utf8(&data) {
        Ok(t) => t,
        Err(_) => return None,
    };
    parse_hosts(text).into_iter().find(|e| e.matches(name)).map(|e| e.addr)
}

/// Resolve `name` to all matching entries (returning `(addr, canonical)` pairs).
pub fn resolve_all(name: &str) -> Vec<(Ipv4Addr, String)> {
    if let Some(addr) = Ipv4Addr::parse(name) {
        return alloc::vec![(addr, name.to_string())];
    }
    let data = match crate::fs::vfs::VfsContext::read("/etc/hosts") {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let text = match core::str::from_utf8(&data) { Ok(t) => t, Err(_) => return Vec::new() };
    parse_hosts(text).into_iter()
        .filter(|e| e.matches(name))
        .map(|e| (e.addr, e.canonical))
        .collect()
}
