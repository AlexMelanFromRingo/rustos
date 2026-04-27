//! Minimal IPv4 packet parsing and serialisation.
//!
//! Only enough to send and receive UDP datagrams over the loopback interface.
//! No fragmentation, no IP options, no IPsec.

use super::Ipv4Addr;

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP:  u8 = 6;
pub const PROTO_UDP:  u8 = 17;

/// Length of an IPv4 header without options.
pub const IPV4_HEADER_LEN: usize = 20;

#[derive(Debug, Clone)]
pub struct Ipv4Header {
    pub version_ihl: u8,    // 0x45 for v4 + 5 32-bit words = 20 bytes
    pub tos: u8,
    pub total_length: u16,
    pub identification: u16,
    pub flags_fragoff: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
}

impl Ipv4Header {
    pub fn new(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload_len: u16) -> Self {
        Ipv4Header {
            version_ihl: 0x45,
            tos: 0,
            total_length: IPV4_HEADER_LEN as u16 + payload_len,
            identification: 0,
            flags_fragoff: 0x4000, // DF bit
            ttl: 64,
            protocol,
            checksum: 0,
            src,
            dst,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= IPV4_HEADER_LEN);
        buf[0] = self.version_ihl;
        buf[1] = self.tos;
        buf[2..4].copy_from_slice(&self.total_length.to_be_bytes());
        buf[4..6].copy_from_slice(&self.identification.to_be_bytes());
        buf[6..8].copy_from_slice(&self.flags_fragoff.to_be_bytes());
        buf[8] = self.ttl;
        buf[9] = self.protocol;
        buf[10..12].copy_from_slice(&[0, 0]); // checksum cleared first
        buf[12..16].copy_from_slice(&self.src.0);
        buf[16..20].copy_from_slice(&self.dst.0);
        let cksum = compute_checksum(&buf[..IPV4_HEADER_LEN]);
        buf[10..12].copy_from_slice(&cksum.to_be_bytes());
    }

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < IPV4_HEADER_LEN { return None; }
        let total = u16::from_be_bytes([buf[2], buf[3]]);
        Some(Ipv4Header {
            version_ihl: buf[0],
            tos: buf[1],
            total_length: total,
            identification: u16::from_be_bytes([buf[4], buf[5]]),
            flags_fragoff: u16::from_be_bytes([buf[6], buf[7]]),
            ttl: buf[8],
            protocol: buf[9],
            checksum: u16::from_be_bytes([buf[10], buf[11]]),
            src: Ipv4Addr([buf[12], buf[13], buf[14], buf[15]]),
            dst: Ipv4Addr([buf[16], buf[17], buf[18], buf[19]]),
        })
    }
}

/// Internet checksum (RFC 1071).
pub fn compute_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}
