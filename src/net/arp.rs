//! ARP (RFC 826) for IPv4-over-Ethernet.

use super::eth;
use super::Ipv4Addr;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

pub const HTYPE_ETHERNET: u16 = 1;
pub const PTYPE_IPV4:     u16 = 0x0800;
pub const HLEN_ETHERNET:  u8 = 6;
pub const PLEN_IPV4:      u8 = 4;

pub mod op {
    pub const REQUEST: u16 = 1;
    pub const REPLY:   u16 = 2;
}

pub const ARP_LEN: usize = 28;

#[derive(Debug, Clone)]
pub struct ArpPacket {
    pub htype: u16,
    pub ptype: u16,
    pub hlen:  u8,
    pub plen:  u8,
    pub op:    u16,
    pub sender_mac: [u8; 6],
    pub sender_ip:  Ipv4Addr,
    pub target_mac: [u8; 6],
    pub target_ip:  Ipv4Addr,
}

impl ArpPacket {
    pub fn build(&self) -> [u8; ARP_LEN] {
        let mut out = [0u8; ARP_LEN];
        out[0..2].copy_from_slice(&self.htype.to_be_bytes());
        out[2..4].copy_from_slice(&self.ptype.to_be_bytes());
        out[4] = self.hlen;
        out[5] = self.plen;
        out[6..8].copy_from_slice(&self.op.to_be_bytes());
        out[8..14].copy_from_slice(&self.sender_mac);
        out[14..18].copy_from_slice(&self.sender_ip.0);
        out[18..24].copy_from_slice(&self.target_mac);
        out[24..28].copy_from_slice(&self.target_ip.0);
        out
    }

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < ARP_LEN { return None; }
        let mut sender_mac = [0u8; 6]; sender_mac.copy_from_slice(&buf[8..14]);
        let mut target_mac = [0u8; 6]; target_mac.copy_from_slice(&buf[18..24]);
        Some(ArpPacket {
            htype: u16::from_be_bytes([buf[0], buf[1]]),
            ptype: u16::from_be_bytes([buf[2], buf[3]]),
            hlen:  buf[4],
            plen:  buf[5],
            op:    u16::from_be_bytes([buf[6], buf[7]]),
            sender_mac,
            sender_ip: Ipv4Addr([buf[14], buf[15], buf[16], buf[17]]),
            target_mac,
            target_ip: Ipv4Addr([buf[24], buf[25], buf[26], buf[27]]),
        })
    }
}

/// IP → MAC table.  Populated by ARP replies and lookups.
static ARP_TABLE: Mutex<BTreeMap<u32, [u8; 6]>> = Mutex::new(BTreeMap::new());

pub fn cache_insert(ip: Ipv4Addr, mac: [u8; 6]) {
    ARP_TABLE.lock().insert(ip.to_u32(), mac);
}

pub fn cache_lookup(ip: Ipv4Addr) -> Option<[u8; 6]> {
    ARP_TABLE.lock().get(&ip.to_u32()).copied()
}

pub fn cache_snapshot() -> Vec<(Ipv4Addr, [u8; 6])> {
    ARP_TABLE.lock().iter().map(|(k, v)| (Ipv4Addr::from_u32(*k), *v)).collect()
}

pub fn cache_clear() {
    ARP_TABLE.lock().clear();
}

/// Build an ARP REQUEST broadcast frame (Ethernet + ARP) asking who has
/// `target_ip`.  Returns the wire bytes.
pub fn build_request_frame(sender_mac: [u8; 6], sender_ip: Ipv4Addr, target_ip: Ipv4Addr) -> Vec<u8> {
    let arp = ArpPacket {
        htype: HTYPE_ETHERNET,
        ptype: PTYPE_IPV4,
        hlen:  HLEN_ETHERNET,
        plen:  PLEN_IPV4,
        op:    op::REQUEST,
        sender_mac,
        sender_ip,
        target_mac: [0u8; 6],
        target_ip,
    };
    eth::build_frame(eth::BROADCAST_MAC, sender_mac, eth::ethertype::ARP, &arp.build())
}

/// Build a reply to a request: my MAC -> requester MAC.
pub fn build_reply_frame(my_mac: [u8; 6], my_ip: Ipv4Addr,
                          req: &ArpPacket) -> Vec<u8> {
    let arp = ArpPacket {
        htype: HTYPE_ETHERNET,
        ptype: PTYPE_IPV4,
        hlen:  HLEN_ETHERNET,
        plen:  PLEN_IPV4,
        op:    op::REPLY,
        sender_mac: my_mac,
        sender_ip:  my_ip,
        target_mac: req.sender_mac,
        target_ip:  req.sender_ip,
    };
    eth::build_frame(req.sender_mac, my_mac, eth::ethertype::ARP, &arp.build())
}

/// Process an inbound ARP frame.  Returns Some(reply_bytes) if we should
/// respond, None otherwise.  Always updates the cache from the sender side.
pub fn input(arp_payload: &[u8], my_mac: [u8; 6], my_ip: Ipv4Addr) -> Option<Vec<u8>> {
    let arp = ArpPacket::parse(arp_payload)?;
    if arp.htype != HTYPE_ETHERNET || arp.ptype != PTYPE_IPV4 { return None; }

    cache_insert(arp.sender_ip, arp.sender_mac);

    if arp.op == op::REQUEST && arp.target_ip == my_ip {
        return Some(build_reply_frame(my_mac, my_ip, &arp));
    }
    None
}
