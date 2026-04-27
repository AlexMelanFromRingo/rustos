//! Minimal UDP/IPv4.

use super::ip::{compute_checksum, Ipv4Header, IPV4_HEADER_LEN, PROTO_UDP};
use super::{Ipv4Addr, SocketAddrV4};

pub const UDP_HEADER_LEN: usize = 8;

#[derive(Debug, Clone)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
}

impl UdpHeader {
    pub fn new(src_port: u16, dst_port: u16, payload_len: u16) -> Self {
        UdpHeader {
            src_port,
            dst_port,
            length: UDP_HEADER_LEN as u16 + payload_len,
            checksum: 0,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= UDP_HEADER_LEN);
        buf[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        buf[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        buf[4..6].copy_from_slice(&self.length.to_be_bytes());
        buf[6..8].copy_from_slice(&self.checksum.to_be_bytes());
    }

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < UDP_HEADER_LEN { return None; }
        Some(UdpHeader {
            src_port: u16::from_be_bytes([buf[0], buf[1]]),
            dst_port: u16::from_be_bytes([buf[2], buf[3]]),
            length: u16::from_be_bytes([buf[4], buf[5]]),
            checksum: u16::from_be_bytes([buf[6], buf[7]]),
        })
    }
}

/// Build a complete IPv4+UDP datagram.  Returns the on-wire byte sequence.
pub fn build_datagram(
    src: SocketAddrV4,
    dst: SocketAddrV4,
    payload: &[u8],
) -> alloc::vec::Vec<u8> {
    let total_len = IPV4_HEADER_LEN + UDP_HEADER_LEN + payload.len();
    let mut buf = alloc::vec![0u8; total_len];

    let ip_hdr = Ipv4Header::new(src.ip, dst.ip, PROTO_UDP,
        (UDP_HEADER_LEN + payload.len()) as u16);
    ip_hdr.write_to(&mut buf[..IPV4_HEADER_LEN]);

    let udp_hdr = UdpHeader::new(src.port, dst.port, payload.len() as u16);
    udp_hdr.write_to(&mut buf[IPV4_HEADER_LEN..IPV4_HEADER_LEN + UDP_HEADER_LEN]);

    buf[IPV4_HEADER_LEN + UDP_HEADER_LEN..].copy_from_slice(payload);

    // Compute UDP checksum with pseudo-header (IPv4).  Optional in IPv4, but
    // we set it for correctness over loopback.
    let pseudo_sum = pseudo_header_sum(src.ip, dst.ip, PROTO_UDP,
        (UDP_HEADER_LEN + payload.len()) as u16);
    let udp_segment = &buf[IPV4_HEADER_LEN..];
    let cksum = !add_words(pseudo_sum, partial_sum(udp_segment)) as u16;
    let cksum = if cksum == 0 { 0xFFFF } else { cksum };
    buf[IPV4_HEADER_LEN + 6..IPV4_HEADER_LEN + 8].copy_from_slice(&cksum.to_be_bytes());

    buf
}

fn pseudo_header_sum(src: Ipv4Addr, dst: Ipv4Addr, proto: u8, udp_len: u16) -> u32 {
    let mut s: u32 = 0;
    s += u16::from_be_bytes([src.0[0], src.0[1]]) as u32;
    s += u16::from_be_bytes([src.0[2], src.0[3]]) as u32;
    s += u16::from_be_bytes([dst.0[0], dst.0[1]]) as u32;
    s += u16::from_be_bytes([dst.0[2], dst.0[3]]) as u32;
    s += proto as u32;
    s += udp_len as u32;
    s
}

fn partial_sum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    sum
}

fn add_words(a: u32, b: u32) -> u32 {
    let mut s = a + b;
    while (s >> 16) != 0 {
        s = (s & 0xFFFF) + (s >> 16);
    }
    s
}

/// Verify that the pre-built datagram's checksums look sane.
pub fn verify_ip_checksum(packet: &[u8]) -> bool {
    if packet.len() < IPV4_HEADER_LEN {
        return false;
    }
    compute_checksum(&packet[..IPV4_HEADER_LEN]) == 0
}
