//! Minimal DNS A-record client over UDP (RFC 1035).
//!
//! Parses /etc/resolv.conf for `nameserver IP` lines and queries each in
//! turn.  In this build the only routable destination is loopback, so DNS
//! over the wire only works if the configured nameserver is also a local
//! UDP responder.  When no resolver is reachable, callers should fall back
//! to the static [`super::resolver`] which reads /etc/hosts.

use super::Ipv4Addr;
use super::socket::{bind, recvfrom, sendto, socket, SocketDomain, SocketProto, SocketType};
use super::SocketAddrV4;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

const DNS_PORT: u16 = 53;
const DNS_TIMEOUT_MS: i32 = 200;

/// Cached A-record entry: (name → addr, set_at_tick).
#[derive(Debug, Clone)]
struct CacheEntry {
    addr: Ipv4Addr,
    set_at_tick: u64,
}

static CACHE: Mutex<alloc::collections::BTreeMap<String, CacheEntry>> =
    Mutex::new(alloc::collections::BTreeMap::new());
const CACHE_TTL_TICKS: u64 = 30 * 18; // ~30 seconds

/// Read /etc/resolv.conf and return the configured nameserver IPs (in order).
pub fn read_resolvers() -> Vec<Ipv4Addr> {
    let data = match crate::fs::vfs::VfsContext::read("/etc/resolv.conf") {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let text = match core::str::from_utf8(&data) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = match raw.find('#') { Some(i) => &raw[..i], None => raw };
        let line = line.trim();
        if !line.starts_with("nameserver") { continue; }
        if let Some(rest) = line.split_whitespace().nth(1) {
            if let Some(addr) = Ipv4Addr::parse(rest) {
                out.push(addr);
            }
        }
    }
    out
}

/// Build a minimal DNS query for an A record.  Returns the wire bytes.
pub fn build_a_query(name: &str, txn_id: u16) -> Vec<u8> {
    let mut out = Vec::new();
    // Header (12 bytes):
    //   ID, FLAGS=0x0100 (standard query, RD=1), QDCOUNT=1, ANCOUNT=ARCOUNT=NSCOUNT=0
    out.extend_from_slice(&txn_id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    // QNAME: each label preceded by its length, terminated by 0 byte.
    for label in name.split('.') {
        let bytes = label.as_bytes();
        if bytes.is_empty() || bytes.len() > 63 { continue; }
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    out.push(0);
    out.extend_from_slice(&1u16.to_be_bytes()); // QTYPE = A
    out.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    out
}

/// Parse a DNS response, returning the first A-record IP if any.
pub fn parse_a_response(buf: &[u8], expected_txn: u16) -> Option<Ipv4Addr> {
    if buf.len() < 12 { return None; }
    let txn = u16::from_be_bytes([buf[0], buf[1]]);
    if txn != expected_txn { return None; }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x000F != 0 { return None; } // RCODE != 0 = error
    let ancount = u16::from_be_bytes([buf[6], buf[7]]);
    if ancount == 0 { return None; }

    // Skip QDCOUNT questions.  We just generated the query so we know its
    // structure; advance past the question section by walking labels.
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
    let mut pos = 12usize;
    for _ in 0..qdcount {
        pos = skip_name(buf, pos)?;
        // QTYPE + QCLASS
        if pos + 4 > buf.len() { return None; }
        pos += 4;
    }

    // Walk answers.  Return the first A record.
    for _ in 0..ancount {
        pos = skip_name(buf, pos)?;
        if pos + 10 > buf.len() { return None; }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let _class = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]);
        let _ttl = u32::from_be_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > buf.len() { return None; }
        if rtype == 1 && rdlen == 4 {
            return Some(Ipv4Addr([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]));
        }
        pos += rdlen;
    }
    None
}

/// Step past a DNS name (handles label compression pointers).
fn skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        if pos >= buf.len() { return None; }
        let b = buf[pos];
        if b == 0 { return Some(pos + 1); }
        if b & 0xC0 == 0xC0 {
            // Pointer — 2 bytes total
            return Some(pos + 2);
        }
        let len = b as usize;
        pos += 1 + len;
    }
}

/// Resolve `name` to an IPv4 address.  Tries:
///   1. cache (TTL-bounded)
///   2. /etc/hosts
///   3. each nameserver from /etc/resolv.conf in order
pub fn resolve(name: &str) -> Option<Ipv4Addr> {
    if let Some(addr) = Ipv4Addr::parse(name) { return Some(addr); }

    // Cache hit (TTL-checked)
    let now = crate::task::timer::current_ticks();
    {
        let cache = CACHE.lock();
        if let Some(e) = cache.get(name) {
            if now.saturating_sub(e.set_at_tick) < CACHE_TTL_TICKS {
                return Some(e.addr);
            }
        }
    }

    // /etc/hosts (instant)
    if let Some(addr) = super::resolver::resolve(name) {
        CACHE.lock().insert(name.to_string(), CacheEntry { addr, set_at_tick: now });
        return Some(addr);
    }

    // Wire query to each configured nameserver.
    let resolvers = read_resolvers();
    for ns in resolvers {
        if let Some(addr) = query_one(name, ns) {
            CACHE.lock().insert(name.to_string(), CacheEntry { addr, set_at_tick: now });
            return Some(addr);
        }
    }
    None
}

fn query_one(name: &str, server: Ipv4Addr) -> Option<Ipv4Addr> {
    let txn: u16 = (crate::task::timer::current_ticks() as u16) | 0x0001;
    let query = build_a_query(name, txn);

    let s = socket(SocketDomain::AF_INET, SocketType::Datagram, SocketProto::Udp);
    let _ = bind(s, SocketAddrV4 { ip: Ipv4Addr::ANY, port: 0 });

    let dst = SocketAddrV4 { ip: server, port: DNS_PORT };
    if sendto(s, &query, dst).is_err() {
        super::socket::close(s);
        return None;
    }

    let mut buf = [0u8; 1024];
    // do_poll-style wait via a timeout-bounded recvfrom retry loop.
    let start = crate::task::timer::current_ticks();
    loop {
        match recvfrom(s, &mut buf) {
            Ok((n, _)) => {
                super::socket::close(s);
                return parse_a_response(&buf[..n], txn);
            }
            Err(_) => {
                let now = crate::task::timer::current_ticks();
                let elapsed_ms = (now - start) * 55;
                if elapsed_ms as i32 >= DNS_TIMEOUT_MS {
                    super::socket::close(s);
                    return None;
                }
                x86_64::instructions::interrupts::enable_and_hlt();
            }
        }
    }
}

/// Drop all cached entries (used by `cache_flush`-style admin commands).
pub fn flush_cache() {
    CACHE.lock().clear();
}

pub fn cache_size() -> usize { CACHE.lock().len() }
