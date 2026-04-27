//! DHCPv4 client (RFC 2131 / 2132).
//!
//! Builds DHCP DISCOVER/REQUEST messages and parses OFFER/ACK responses.
//! Without a real Ethernet driver this can only loop back over `lo`, so the
//! "lease" obtained on this build is symbolic — what matters is that the
//! packet build/parse round-trips through the real wire format and the
//! shell `dhclient` command exercises both halves.

use super::Ipv4Addr;
use alloc::vec::Vec;

pub const OP_BOOTREQUEST: u8 = 1;
pub const OP_BOOTREPLY:   u8 = 2;
pub const HTYPE_ETHERNET: u8 = 1;
pub const HLEN_ETHERNET:  u8 = 6;

pub const MAGIC_COOKIE: u32 = 0x6382_5363;

pub mod msg_type {
    pub const DISCOVER: u8 = 1;
    pub const OFFER:    u8 = 2;
    pub const REQUEST:  u8 = 3;
    pub const ACK:      u8 = 5;
    pub const NAK:      u8 = 6;
    pub const RELEASE:  u8 = 7;
}

#[derive(Debug, Clone)]
pub struct DhcpMessage {
    pub op: u8,
    pub xid: u32,
    pub ciaddr: Ipv4Addr,
    pub yiaddr: Ipv4Addr,
    pub siaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub chaddr: [u8; 16],
    pub options: Vec<(u8, Vec<u8>)>,
}

impl DhcpMessage {
    pub fn build(&self) -> Vec<u8> {
        let mut out = alloc::vec![0u8; 240];
        out[0] = self.op;
        out[1] = HTYPE_ETHERNET;
        out[2] = HLEN_ETHERNET;
        out[3] = 0; // hops
        out[4..8].copy_from_slice(&self.xid.to_be_bytes());
        // secs(2), flags(2) — leave 0
        out[16..20].copy_from_slice(&self.ciaddr.0);
        out[20..24].copy_from_slice(&self.yiaddr.0);
        out[24..28].copy_from_slice(&self.siaddr.0);
        out[28..32].copy_from_slice(&self.giaddr.0);
        out[44..60].copy_from_slice(&self.chaddr);
        // sname(64) + file(128) zero
        out[236..240].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());

        for (tag, val) in &self.options {
            out.push(*tag);
            out.push(val.len() as u8);
            out.extend_from_slice(val);
        }
        out.push(0xFF); // END
        out
    }

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 240 { return None; }
        if u32::from_be_bytes([buf[236], buf[237], buf[238], buf[239]]) != MAGIC_COOKIE {
            return None;
        }
        let mut chaddr = [0u8; 16];
        chaddr.copy_from_slice(&buf[44..60]);
        let mut msg = DhcpMessage {
            op: buf[0],
            xid: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ciaddr: Ipv4Addr([buf[16], buf[17], buf[18], buf[19]]),
            yiaddr: Ipv4Addr([buf[20], buf[21], buf[22], buf[23]]),
            siaddr: Ipv4Addr([buf[24], buf[25], buf[26], buf[27]]),
            giaddr: Ipv4Addr([buf[28], buf[29], buf[30], buf[31]]),
            chaddr,
            options: Vec::new(),
        };
        let mut i = 240;
        while i < buf.len() {
            let tag = buf[i];
            if tag == 0 { i += 1; continue; }      // PAD
            if tag == 0xFF { break; }               // END
            i += 1;
            if i >= buf.len() { break; }
            let len = buf[i] as usize;
            i += 1;
            if i + len > buf.len() { break; }
            msg.options.push((tag, buf[i..i + len].to_vec()));
            i += len;
        }
        Some(msg)
    }

    pub fn message_type(&self) -> Option<u8> {
        self.options.iter().find(|(t, _)| *t == 53).and_then(|(_, v)| v.first().copied())
    }

    pub fn server_id(&self) -> Option<Ipv4Addr> {
        self.options.iter().find(|(t, _)| *t == 54).map(|(_, v)| {
            if v.len() == 4 { Ipv4Addr([v[0], v[1], v[2], v[3]]) } else { Ipv4Addr::ANY }
        })
    }

    pub fn lease_seconds(&self) -> Option<u32> {
        self.options.iter().find(|(t, _)| *t == 51).map(|(_, v)| {
            if v.len() == 4 { u32::from_be_bytes([v[0], v[1], v[2], v[3]]) } else { 0 }
        })
    }
}

/// Build a DISCOVER for the given MAC address and transaction id.
pub fn build_discover(mac: [u8; 6], xid: u32) -> Vec<u8> {
    let mut chaddr = [0u8; 16];
    chaddr[..6].copy_from_slice(&mac);
    let msg = DhcpMessage {
        op: OP_BOOTREQUEST,
        xid,
        ciaddr: Ipv4Addr::ANY,
        yiaddr: Ipv4Addr::ANY,
        siaddr: Ipv4Addr::ANY,
        giaddr: Ipv4Addr::ANY,
        chaddr,
        options: alloc::vec![
            (53, alloc::vec![msg_type::DISCOVER]),                     // DHCP Message Type
            (55, alloc::vec![1, 3, 6, 15, 51, 54]),                    // Parameter Request List
            (12, b"rustos".to_vec()),                                  // Hostname
        ],
    };
    msg.build()
}

/// Build a REQUEST in response to an OFFER.
pub fn build_request(mac: [u8; 6], xid: u32, requested: Ipv4Addr, server: Ipv4Addr) -> Vec<u8> {
    let mut chaddr = [0u8; 16];
    chaddr[..6].copy_from_slice(&mac);
    let msg = DhcpMessage {
        op: OP_BOOTREQUEST,
        xid,
        ciaddr: Ipv4Addr::ANY,
        yiaddr: Ipv4Addr::ANY,
        siaddr: Ipv4Addr::ANY,
        giaddr: Ipv4Addr::ANY,
        chaddr,
        options: alloc::vec![
            (53, alloc::vec![msg_type::REQUEST]),
            (50, requested.0.to_vec()),  // Requested IP
            (54, server.0.to_vec()),     // Server Identifier
        ],
    };
    msg.build()
}

/// Forge a synthetic OFFER for self-test (no real DHCP server on lo).
pub fn fake_offer_for(discover_bytes: &[u8], yiaddr: Ipv4Addr, server: Ipv4Addr) -> Vec<u8> {
    let req = DhcpMessage::parse(discover_bytes).expect("malformed");
    let resp = DhcpMessage {
        op: OP_BOOTREPLY,
        xid: req.xid,
        ciaddr: Ipv4Addr::ANY,
        yiaddr,
        siaddr: server,
        giaddr: Ipv4Addr::ANY,
        chaddr: req.chaddr,
        options: alloc::vec![
            (53, alloc::vec![msg_type::OFFER]),
            (1,  alloc::vec![255, 255, 255, 0]),   // subnet mask
            (3,  server.0.to_vec()),               // router
            (51, 86400u32.to_be_bytes().to_vec()), // lease 24h
            (54, server.0.to_vec()),               // server id
            (6,  alloc::vec![127, 0, 0, 1]),       // DNS
        ],
    };
    resp.build()
}

/// Forge a synthetic ACK for self-test.
pub fn fake_ack_for(request_bytes: &[u8], yiaddr: Ipv4Addr, server: Ipv4Addr) -> Vec<u8> {
    let req = DhcpMessage::parse(request_bytes).expect("malformed");
    let resp = DhcpMessage {
        op: OP_BOOTREPLY,
        xid: req.xid,
        ciaddr: Ipv4Addr::ANY,
        yiaddr,
        siaddr: server,
        giaddr: Ipv4Addr::ANY,
        chaddr: req.chaddr,
        options: alloc::vec![
            (53, alloc::vec![msg_type::ACK]),
            (1,  alloc::vec![255, 255, 255, 0]),
            (3,  server.0.to_vec()),
            (51, 86400u32.to_be_bytes().to_vec()),
            (54, server.0.to_vec()),
            (6,  alloc::vec![127, 0, 0, 1]),
        ],
    };
    resp.build()
}
