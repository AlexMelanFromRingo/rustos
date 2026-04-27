//! Network stack root.
//!
//! This is a minimal "datagram-only" network stack: a small interface table,
//! IPv4 + UDP, an in-kernel socket abstraction, and a single working
//! transport (loopback / 127.0.0.0/8).  Outbound datagrams to a loopback
//! address are queued back onto `lo`'s receive queue and matched against a
//! per-port socket map.
//!
//! The intent is to give an Ubuntu-Server-like surface:
//!   - `ifconfig`-style introspection of interfaces
//!   - `netstat` enumeration of bound sockets
//!   - syscalls (or kernel-side equivalents) that approximate
//!     `socket(2)` / `bind(2)` / `sendto(2)` / `recvfrom(2)`
//!
//! Once a hardware NIC driver lands (RTL8139 / virtio-net), `ip.rs` and
//! `udp.rs` can be reused unchanged: only the per-interface transmit hook
//! changes.

pub mod ip;
pub mod udp;
pub mod loopback;
pub mod socket;

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// IPv4 address stored as a single u32 in network byte order (big-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const LOCALHOST: Ipv4Addr = Ipv4Addr([127, 0, 0, 1]);
    pub const ANY:       Ipv4Addr = Ipv4Addr([0, 0, 0, 0]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255, 255, 255, 255]);

    pub fn new(a: u8, b: u8, c: u8, d: u8) -> Self { Ipv4Addr([a, b, c, d]) }

    pub fn is_loopback(&self) -> bool { self.0[0] == 127 }
    pub fn is_unspecified(&self) -> bool { self.0 == [0, 0, 0, 0] }
    pub fn is_broadcast(&self) -> bool { self.0 == [255, 255, 255, 255] }

    pub fn to_u32(&self) -> u32 {
        ((self.0[0] as u32) << 24) | ((self.0[1] as u32) << 16)
            | ((self.0[2] as u32) << 8) | (self.0[3] as u32)
    }
    pub fn from_u32(v: u32) -> Self {
        Ipv4Addr([
            (v >> 24) as u8,
            (v >> 16) as u8,
            (v >> 8) as u8,
            v as u8,
        ])
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.split('.');
        let a: u8 = parts.next()?.parse().ok()?;
        let b: u8 = parts.next()?.parse().ok()?;
        let c: u8 = parts.next()?.parse().ok()?;
        let d: u8 = parts.next()?.parse().ok()?;
        if parts.next().is_some() { return None; }
        Some(Ipv4Addr([a, b, c, d]))
    }
}

impl core::fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

/// Generic IPv4 socket address (IP + port).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketAddrV4 {
    pub ip: Ipv4Addr,
    pub port: u16,
}

impl core::fmt::Display for SocketAddrV4 {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

/// What a NetInterface actually does on transmit.
pub trait NetInterface: Send {
    /// Human-readable name (e.g. "lo").
    fn name(&self) -> &str;
    /// Local IPv4 address.
    fn ipv4(&self) -> Ipv4Addr;
    /// IPv4 netmask.
    fn netmask(&self) -> Ipv4Addr;
    /// MTU in bytes.
    fn mtu(&self) -> u16;
    /// True if administratively up.
    fn is_up(&self) -> bool;
    /// Bring administratively up/down.
    fn set_up(&mut self, up: bool);
    /// Counters.
    fn rx_bytes(&self) -> u64;
    fn tx_bytes(&self) -> u64;
    fn rx_packets(&self) -> u64;
    fn tx_packets(&self) -> u64;
    /// Transmit a fully-formed IPv4 datagram (header + payload).
    /// Returns Ok(()) on accept (queued) or Err on link/buffer issue.
    fn transmit_ipv4(&mut self, packet: &[u8]) -> Result<(), &'static str>;
}

/// Owned interface registration.
pub struct InterfaceEntry {
    pub iface: alloc::boxed::Box<dyn NetInterface>,
}

pub struct NetStack {
    pub interfaces: Vec<InterfaceEntry>,
}

impl NetStack {
    pub const fn new() -> Self { NetStack { interfaces: Vec::new() } }

    pub fn register(&mut self, iface: alloc::boxed::Box<dyn NetInterface>) {
        self.interfaces.push(InterfaceEntry { iface });
    }

    pub fn find_by_name(&mut self, name: &str) -> Option<&mut InterfaceEntry> {
        self.interfaces.iter_mut().find(|e| e.iface.name() == name)
    }

    /// Pick an interface to send a packet to `dst`.  We always prefer `lo`
    /// for loopback addresses; otherwise use the first `up` interface.
    pub fn route(&mut self, dst: Ipv4Addr) -> Option<&mut InterfaceEntry> {
        if dst.is_loopback() {
            return self.interfaces.iter_mut()
                .find(|e| e.iface.name() == "lo");
        }
        self.interfaces.iter_mut().find(|e| e.iface.is_up() && e.iface.name() != "lo")
    }
}

pub static NET_STACK: Mutex<NetStack> = Mutex::new(NetStack::new());

/// Initialise the network stack: register the loopback interface.
pub fn init() {
    let mut stack = NET_STACK.lock();
    stack.register(alloc::boxed::Box::new(loopback::LoopbackIface::new()));
    let lo = stack.interfaces.iter_mut().next().unwrap();
    lo.iface.set_up(true);
}

/// Return a single line summary of every registered interface (for ifconfig).
pub fn ifconfig_lines() -> Vec<String> {
    let stack = NET_STACK.lock();
    let mut out = Vec::new();
    for e in stack.interfaces.iter() {
        let i = &e.iface;
        out.push(alloc::format!(
            "{}: flags={} mtu {}\n        inet {}  netmask {}\n        RX packets {}  bytes {}\n        TX packets {}  bytes {}",
            i.name(),
            if i.is_up() { "<UP,RUNNING,LOOPBACK>" } else { "<DOWN>" },
            i.mtu(),
            i.ipv4(), i.netmask(),
            i.rx_packets(), i.rx_bytes(),
            i.tx_packets(), i.tx_bytes(),
        ));
    }
    out
}
