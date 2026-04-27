//! Loopback (lo) interface.
//!
//! Datagrams sent to a loopback address are queued back onto our own RX
//! queue and immediately dispatched to the socket layer.

use super::{Ipv4Addr, NetInterface};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Pending loopback datagrams that have been "transmitted" but not yet
/// dispatched to the protocol layer.  We queue here instead of calling
/// `ipv4_input` directly to avoid recursive locking on `NET_STACK`.
static LO_RX_QUEUE: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());

pub struct LoopbackIface {
    up: bool,
    rx_packets: AtomicU64,
    tx_packets: AtomicU64,
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
}

impl LoopbackIface {
    pub fn new() -> Self {
        LoopbackIface {
            up: false,
            rx_packets: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
        }
    }

    fn record_rx(&self, bytes: u64) {
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_tx(&self, bytes: u64) {
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

impl NetInterface for LoopbackIface {
    fn name(&self) -> &str { "lo" }
    fn ipv4(&self) -> Ipv4Addr { Ipv4Addr::LOCALHOST }
    fn netmask(&self) -> Ipv4Addr { Ipv4Addr([255, 0, 0, 0]) }
    fn mtu(&self) -> u16 { 65535 }
    fn is_up(&self) -> bool { self.up }
    fn set_up(&mut self, up: bool) { self.up = up; }
    fn rx_packets(&self) -> u64 { self.rx_packets.load(Ordering::Relaxed) }
    fn tx_packets(&self) -> u64 { self.tx_packets.load(Ordering::Relaxed) }
    fn rx_bytes(&self)   -> u64 { self.rx_bytes.load(Ordering::Relaxed) }
    fn tx_bytes(&self)   -> u64 { self.tx_bytes.load(Ordering::Relaxed) }

    fn transmit_ipv4(&mut self, packet: &[u8]) -> Result<(), &'static str> {
        if !self.up {
            return Err("interface down");
        }
        self.record_tx(packet.len() as u64);
        self.record_rx(packet.len() as u64);

        // Queue the packet on the RX queue.  The caller will drain it after
        // releasing NET_STACK so the protocol layer can re-enter sendto/etc.
        LO_RX_QUEUE.lock().push_back(packet.to_vec());
        Ok(())
    }
}

/// Pop one queued loopback packet, if any.  Used by `drain_pending` so the
/// protocol layer can dispatch with `NET_STACK` *not* held.
pub fn pop_pending() -> Option<Vec<u8>> {
    LO_RX_QUEUE.lock().pop_front()
}

/// Convenience: copy a packet without holding the iface mutex (to avoid
/// re-entrancy if `transmit_ipv4` calls back into the socket layer that
/// later wants to enumerate interfaces).
pub fn loopback_inject(packet: Vec<u8>) -> Result<(), &'static str> {
    let mut stack = super::NET_STACK.lock();
    let lo = stack.find_by_name("lo").ok_or("no lo interface")?;
    lo.iface.transmit_ipv4(&packet)
}
