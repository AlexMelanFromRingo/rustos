//! Kernel-level socket abstraction.
//!
//! A socket is identified by a small integer handle (no FD interop yet).
//! Each socket has a protocol, an optional bound local address, and a
//! receive queue.  Outbound datagrams go through the protocol layer
//! ([`udp`](super::udp)) and onto the routed interface.

use super::ip::{Ipv4Header, IPV4_HEADER_LEN, PROTO_ICMP, PROTO_UDP};
use super::udp::{UdpHeader, UDP_HEADER_LEN, build_datagram};
use super::{Ipv4Addr, SocketAddrV4};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum SocketDomain {
    AF_INET,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    Datagram, // SOCK_DGRAM
    Stream,   // SOCK_STREAM (placeholder)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketProto {
    Udp,
}

pub type SocketHandle = usize;

#[derive(Debug, Clone)]
pub struct ReceivedDatagram {
    pub from: SocketAddrV4,
    pub data: Vec<u8>,
}

pub struct Socket {
    pub handle: SocketHandle,
    pub domain: SocketDomain,
    pub kind: SocketType,
    pub proto: SocketProto,
    pub bound: Option<SocketAddrV4>,
    pub rx: VecDeque<ReceivedDatagram>,
}

pub struct SocketTable {
    sockets: Vec<Option<Socket>>,
    next_ephemeral_port: u16,
}

impl SocketTable {
    pub const fn new() -> Self {
        SocketTable { sockets: Vec::new(), next_ephemeral_port: 49152 }
    }

    pub fn create(&mut self, domain: SocketDomain, kind: SocketType, proto: SocketProto)
        -> SocketHandle
    {
        let handle = self.sockets.len();
        self.sockets.push(Some(Socket {
            handle,
            domain,
            kind,
            proto,
            bound: None,
            rx: VecDeque::new(),
        }));
        handle
    }

    pub fn get_mut(&mut self, h: SocketHandle) -> Option<&mut Socket> {
        self.sockets.get_mut(h).and_then(|s| s.as_mut())
    }

    pub fn get(&self, h: SocketHandle) -> Option<&Socket> {
        self.sockets.get(h).and_then(|s| s.as_ref())
    }

    pub fn close(&mut self, h: SocketHandle) {
        if h < self.sockets.len() { self.sockets[h] = None; }
    }

    /// Look up the socket bound to (ip, port) for a given protocol.
    pub fn find_bound(&mut self, ip: Ipv4Addr, port: u16, proto: SocketProto)
        -> Option<&mut Socket>
    {
        for slot in self.sockets.iter_mut() {
            if let Some(s) = slot {
                if s.proto == proto {
                    if let Some(addr) = s.bound {
                        let ip_match = addr.ip.is_unspecified() || addr.ip == ip;
                        if ip_match && addr.port == port {
                            return Some(s);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn pick_ephemeral_port(&mut self) -> u16 {
        let p = self.next_ephemeral_port;
        self.next_ephemeral_port = if p >= 65000 { 49152 } else { p + 1 };
        p
    }

    pub fn enumerate(&self) -> alloc::vec::Vec<(SocketHandle, SocketProto, Option<SocketAddrV4>, usize)> {
        self.sockets.iter().filter_map(|slot| {
            slot.as_ref().map(|s| (s.handle, s.proto, s.bound, s.rx.len()))
        }).collect()
    }
}

pub static SOCKETS: Mutex<SocketTable> = Mutex::new(SocketTable::new());

/// Errors from the socket API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketError {
    InvalidHandle,
    AddressInUse,
    NotBound,
    Unreachable,
    NoRoute,
    BufferTooSmall,
}

/// `socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)` equivalent.
pub fn socket(domain: SocketDomain, kind: SocketType, proto: SocketProto) -> SocketHandle {
    SOCKETS.lock().create(domain, kind, proto)
}

/// `bind(sock, addr)`.  Returns Err if the local addr is already bound to
/// another socket of the same protocol.
pub fn bind(h: SocketHandle, addr: SocketAddrV4) -> Result<(), SocketError> {
    let mut t = SOCKETS.lock();
    // Reject duplicates
    let dup = t.sockets.iter().any(|s| {
        if let Some(s) = s {
            s.handle != h
                && s.proto == SocketProto::Udp
                && s.bound.map(|a| a == addr).unwrap_or(false)
        } else {
            false
        }
    });
    if dup { return Err(SocketError::AddressInUse); }
    let s = t.get_mut(h).ok_or(SocketError::InvalidHandle)?;
    s.bound = Some(addr);
    Ok(())
}

/// `sendto(sock, buf, addr)`.
pub fn sendto(
    h: SocketHandle,
    payload: &[u8],
    dst: SocketAddrV4,
) -> Result<usize, SocketError> {
    // Determine source addr.  If the socket is unbound, pick an ephemeral port.
    let src: SocketAddrV4 = {
        let mut t = SOCKETS.lock();
        let s = t.get_mut(h).ok_or(SocketError::InvalidHandle)?;
        if let Some(b) = s.bound {
            if b.ip.is_unspecified() {
                SocketAddrV4 { ip: source_ip_for(dst.ip, &t).unwrap_or(Ipv4Addr::LOCALHOST), port: b.port }
            } else { b }
        } else {
            let port = t.pick_ephemeral_port();
            let ip = source_ip_for(dst.ip, &t).unwrap_or(Ipv4Addr::LOCALHOST);
            let addr = SocketAddrV4 { ip, port };
            let s = t.get_mut(h).ok_or(SocketError::InvalidHandle)?;
            s.bound = Some(addr);
            addr
        }
    };

    let datagram = build_datagram(src, dst, payload);

    // Route + transmit.  Drop the stack lock before draining so callbacks
    // (e.g. ICMP reply path) can re-enter the stack.
    {
        let mut stack = super::NET_STACK.lock();
        let iface_entry = stack.route(dst.ip).ok_or(SocketError::NoRoute)?;
        iface_entry.iface.transmit_ipv4(&datagram).map_err(|_| SocketError::Unreachable)?;
    }
    super::drain_pending();
    Ok(payload.len())
}

/// Inspect available source IPs for the given destination.  Picks the IP of
/// the routing interface, or `127.0.0.1` for loopback addresses.
fn source_ip_for(dst: Ipv4Addr, _table: &SocketTable) -> Option<Ipv4Addr> {
    if dst.is_loopback() {
        return Some(Ipv4Addr::LOCALHOST);
    }
    None
}

/// `recvfrom(sock, buf)`.  Returns (bytes_read, source_addr).
pub fn recvfrom(h: SocketHandle, buf: &mut [u8]) -> Result<(usize, SocketAddrV4), SocketError> {
    let mut t = SOCKETS.lock();
    let s = t.get_mut(h).ok_or(SocketError::InvalidHandle)?;
    let dgram = s.rx.pop_front().ok_or(SocketError::NotBound)?;
    let n = dgram.data.len().min(buf.len());
    buf[..n].copy_from_slice(&dgram.data[..n]);
    if n < dgram.data.len() {
        // Datagram truncated — Linux returns the truncated length as Ok and
        // sets MSG_TRUNC.  We simplify to BufferTooSmall.
        return Err(SocketError::BufferTooSmall);
    }
    Ok((n, dgram.from))
}

/// True if there is at least one datagram queued for this socket.
pub fn has_pending(h: SocketHandle) -> bool {
    SOCKETS.lock().get(h).map(|s| !s.rx.is_empty()).unwrap_or(false)
}

pub fn close(h: SocketHandle) {
    SOCKETS.lock().close(h);
}

/// Network → socket.  Called by every interface's RX path.
///
/// Parses the IPv4 header, dispatches by protocol, and drops the packet on
/// any malformedness.
pub fn ipv4_input(packet: &[u8]) {
    if packet.len() < IPV4_HEADER_LEN { return; }
    let hdr = match Ipv4Header::parse(packet) {
        Some(h) => h,
        None => return,
    };
    if hdr.total_length as usize > packet.len() { return; }

    match hdr.protocol {
        PROTO_UDP => udp_input(&hdr, &packet[IPV4_HEADER_LEN..hdr.total_length as usize]),
        PROTO_ICMP => super::icmp::icmp_input(&hdr, &packet[IPV4_HEADER_LEN..hdr.total_length as usize]),
        _ => {} // unknown protocol — silently drop
    }
}

fn udp_input(ip_hdr: &Ipv4Header, segment: &[u8]) {
    if segment.len() < UDP_HEADER_LEN { return; }
    let udp = match UdpHeader::parse(segment) {
        Some(h) => h,
        None => return,
    };
    if udp.length as usize > segment.len() { return; }
    let payload_end = udp.length as usize;
    let payload = &segment[UDP_HEADER_LEN..payload_end];

    let mut t = SOCKETS.lock();
    if let Some(s) = t.find_bound(ip_hdr.dst, udp.dst_port, SocketProto::Udp) {
        s.rx.push_back(ReceivedDatagram {
            from: SocketAddrV4 { ip: ip_hdr.src, port: udp.src_port },
            data: payload.to_vec(),
        });
    }
    // No bound socket — drop silently (real Linux would generate ICMP port
    // unreachable for non-loopback packets).
}
