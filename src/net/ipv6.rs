//! IPv6 minimum stub: header serialise/parse, ICMPv6 echo, Neighbor
//! Solicit / Advertise.  Sufficient to:
//!
//!   * Receive an ICMPv6 echo request on loopback (::1) and reply.
//!   * Build the wire bytes of a Neighbor Solicitation so a future
//!     link-layer driver can answer "who-has fe80::x" queries.
//!
//! References:
//!   * RFC 8200 — IPv6 base header (§3)
//!   * RFC 4443 — ICMPv6, including echo request/reply (§4.1)
//!   * RFC 4861 — Neighbor Discovery: NS (§4.3) / NA (§4.4)
//!
//! Wire format (all big-endian):
//!
//!     IPv6 header — 40 bytes
//!     +0   u32  version(4) | tc(8) | flow(20)
//!     +4   u16  payload_len
//!     +6   u8   next_header
//!     +7   u8   hop_limit
//!     +8   u8[16] src
//!     +24  u8[16] dst

use alloc::vec::Vec;

pub const IPV6_HEADER_LEN: usize = 40;

pub const NH_ICMPV6: u8 = 58;
pub const NH_TCP:    u8 = 6;
pub const NH_UDP:    u8 = 17;

/// 128-bit IPv6 address.  `Eq` so /etc/hosts and the cache work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv6Addr(pub [u8; 16]);

impl Ipv6Addr {
    pub const LOOPBACK: Ipv6Addr = Ipv6Addr([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1]);
    pub const UNSPECIFIED: Ipv6Addr = Ipv6Addr([0u8; 16]);

    pub fn is_loopback(&self) -> bool { *self == Self::LOOPBACK }

    /// Format as compact zero-eliding hex (RFC 5952).  Not exhaustive
    /// — leaves all-zero runs as "::" and zero-pads each group.
    pub fn to_compact(&self) -> alloc::string::String {
        // Find the longest run of zero u16 groups (>= 2) for `::`.
        let groups: [u16; 8] = [
            u16::from_be_bytes([self.0[0],  self.0[1]]),
            u16::from_be_bytes([self.0[2],  self.0[3]]),
            u16::from_be_bytes([self.0[4],  self.0[5]]),
            u16::from_be_bytes([self.0[6],  self.0[7]]),
            u16::from_be_bytes([self.0[8],  self.0[9]]),
            u16::from_be_bytes([self.0[10], self.0[11]]),
            u16::from_be_bytes([self.0[12], self.0[13]]),
            u16::from_be_bytes([self.0[14], self.0[15]]),
        ];
        let (mut best_start, mut best_len) = (0usize, 0usize);
        let (mut cur_start, mut cur_len) = (0usize, 0usize);
        for (i, &g) in groups.iter().enumerate() {
            if g == 0 {
                if cur_len == 0 { cur_start = i; }
                cur_len += 1;
                if cur_len > best_len { best_len = cur_len; best_start = cur_start; }
            } else {
                cur_len = 0;
            }
        }
        if best_len < 2 { best_len = 0; }
        let mut s = alloc::string::String::new();
        let mut i = 0;
        while i < 8 {
            if i == best_start && best_len >= 2 {
                // Push "::" — both colons land here so the next
                // non-zero group's `:` prefix is suppressed by the
                // ends_with(':') check below.
                s.push_str("::");
                i += best_len;
                continue;
            }
            if !s.is_empty() && !s.ends_with(':') { s.push(':'); }
            let _ = core::fmt::Write::write_fmt(&mut s,
                format_args!("{:x}", groups[i]));
            i += 1;
        }
        if s.is_empty() { s.push_str("::"); }
        s
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ipv6Header {
    pub traffic_class: u8,
    pub flow_label:    u32, // 20-bit
    pub payload_len:   u16,
    pub next_header:   u8,
    pub hop_limit:     u8,
    pub src: Ipv6Addr,
    pub dst: Ipv6Addr,
}

impl Ipv6Header {
    pub fn new(src: Ipv6Addr, dst: Ipv6Addr, next_header: u8, payload_len: u16) -> Self {
        Ipv6Header {
            traffic_class: 0, flow_label: 0,
            payload_len, next_header,
            hop_limit: 64,
            src, dst,
        }
    }
    pub fn write_to(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= IPV6_HEADER_LEN);
        let v_tc_fl: u32 = (6u32 << 28)
            | ((self.traffic_class as u32) << 20)
            | (self.flow_label & 0x000F_FFFF);
        buf[0..4].copy_from_slice(&v_tc_fl.to_be_bytes());
        buf[4..6].copy_from_slice(&self.payload_len.to_be_bytes());
        buf[6] = self.next_header;
        buf[7] = self.hop_limit;
        buf[8..24].copy_from_slice(&self.src.0);
        buf[24..40].copy_from_slice(&self.dst.0);
    }
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < IPV6_HEADER_LEN { return None; }
        let v = (buf[0] >> 4) & 0xF;
        if v != 6 { return None; }
        let tc = ((buf[0] & 0x0F) << 4) | ((buf[1] & 0xF0) >> 4);
        let fl = ((buf[1] as u32 & 0x0F) << 16)
            | ((buf[2] as u32) << 8)
            | (buf[3] as u32);
        let payload_len = u16::from_be_bytes([buf[4], buf[5]]);
        let next_header = buf[6];
        let hop_limit   = buf[7];
        let mut src = [0u8; 16]; src.copy_from_slice(&buf[8..24]);
        let mut dst = [0u8; 16]; dst.copy_from_slice(&buf[24..40]);
        Some(Ipv6Header {
            traffic_class: tc, flow_label: fl,
            payload_len, next_header, hop_limit,
            src: Ipv6Addr(src), dst: Ipv6Addr(dst),
        })
    }
}

/// One's-complement sum used by ICMPv6's "pseudo-header" checksum
/// (RFC 4443 §2.3).  Caller passes the IPv6 src/dst, packet length,
/// next-header (always 58 for ICMPv6), and the message bytes.  This is
/// the same algorithm as IPv4 RFC 1071 over a synthesised pseudo-header.
pub fn icmpv6_checksum(src: &Ipv6Addr, dst: &Ipv6Addr,
                       length: u32, nh: u8, msg: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let add_word = |sum: &mut u32, w: u16| {
        *sum += w as u32;
    };
    // Pseudo-header.
    for chunk in src.0.chunks(2) {
        add_word(&mut sum, u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    for chunk in dst.0.chunks(2) {
        add_word(&mut sum, u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    add_word(&mut sum, (length >> 16) as u16);
    add_word(&mut sum, length as u16);
    add_word(&mut sum, 0);
    add_word(&mut sum, nh as u16);
    // Message body.
    let mut i = 0;
    while i + 1 < msg.len() {
        add_word(&mut sum, u16::from_be_bytes([msg[i], msg[i + 1]]));
        i += 2;
    }
    if msg.len() & 1 != 0 {
        add_word(&mut sum, (msg[msg.len() - 1] as u16) << 8);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// ---- ICMPv6 message types we care about ----

pub const ICMPV6_ECHO_REQUEST:  u8 = 128;
pub const ICMPV6_ECHO_REPLY:    u8 = 129;
pub const ICMPV6_NEIGHBOR_SOL:  u8 = 135;
pub const ICMPV6_NEIGHBOR_ADV:  u8 = 136;

/// Build an ICMPv6 echo-request packet (full IPv6 packet bytes).
pub fn build_echo_request(src: Ipv6Addr, dst: Ipv6Addr,
                          identifier: u16, sequence: u16,
                          payload: &[u8]) -> Vec<u8> {
    let icmp_len = 8 + payload.len();
    let mut buf = alloc::vec![0u8; IPV6_HEADER_LEN + icmp_len];
    Ipv6Header::new(src, dst, NH_ICMPV6, icmp_len as u16).write_to(&mut buf);
    let icmp = &mut buf[IPV6_HEADER_LEN..];
    icmp[0] = ICMPV6_ECHO_REQUEST;
    icmp[1] = 0; // code
    icmp[2] = 0; icmp[3] = 0; // checksum placeholder
    icmp[4..6].copy_from_slice(&identifier.to_be_bytes());
    icmp[6..8].copy_from_slice(&sequence.to_be_bytes());
    icmp[8..].copy_from_slice(payload);
    let csum = icmpv6_checksum(&src, &dst, icmp_len as u32, NH_ICMPV6, icmp);
    icmp[2..4].copy_from_slice(&csum.to_be_bytes());
    buf
}

/// Build an ICMPv6 echo-reply packet (mirrors echo-request).
pub fn build_echo_reply(src: Ipv6Addr, dst: Ipv6Addr,
                        identifier: u16, sequence: u16,
                        payload: &[u8]) -> Vec<u8> {
    let mut p = build_echo_request(src, dst, identifier, sequence, payload);
    p[IPV6_HEADER_LEN] = ICMPV6_ECHO_REPLY;
    // Recompute checksum after type change.
    p[IPV6_HEADER_LEN + 2] = 0;
    p[IPV6_HEADER_LEN + 3] = 0;
    let icmp_len = p.len() - IPV6_HEADER_LEN;
    let csum = icmpv6_checksum(&src, &dst, icmp_len as u32, NH_ICMPV6,
        &p[IPV6_HEADER_LEN..]);
    p[IPV6_HEADER_LEN + 2..IPV6_HEADER_LEN + 4].copy_from_slice(&csum.to_be_bytes());
    p
}

/// Build a Neighbor Solicitation message (RFC 4861 §4.3): asks
/// "who has `target`?" so a future link-layer (eth + IPv6 nbr cache)
/// can resolve link-layer addresses.  Includes the optional source
/// link-layer address option when `src_ll` is provided.
pub fn build_neighbor_solicitation(src: Ipv6Addr, target: Ipv6Addr,
                                   src_ll: Option<[u8; 6]>) -> Vec<u8> {
    // Multicast solicited-node address: ff02::1:ffXX:XXXX where the
    // last 24 bits are the low 24 of `target`.
    let mut dst = [0u8; 16];
    dst[0] = 0xFF; dst[1] = 0x02;
    dst[11] = 1;   dst[12] = 0xFF;
    dst[13] = target.0[13];
    dst[14] = target.0[14];
    dst[15] = target.0[15];
    let dst = Ipv6Addr(dst);

    // ICMPv6 message: type(1) code(1) csum(2) reserved(4) target(16)
    // + optional Source Link-Layer Address option (type=1, len=1,
    // 6-byte MAC).
    let opt_len = if src_ll.is_some() { 8 } else { 0 };
    let icmp_len = 24 + opt_len;
    let mut buf = alloc::vec![0u8; IPV6_HEADER_LEN + icmp_len];
    Ipv6Header::new(src, dst, NH_ICMPV6, icmp_len as u16).write_to(&mut buf);
    let icmp = &mut buf[IPV6_HEADER_LEN..];
    icmp[0] = ICMPV6_NEIGHBOR_SOL;
    icmp[1] = 0;
    // bytes 2..4 = checksum (filled below)
    // bytes 4..8 = reserved (zero)
    icmp[8..24].copy_from_slice(&target.0);
    if let Some(mac) = src_ll {
        icmp[24] = 1;       // type: Source Link-Layer Address
        icmp[25] = 1;       // length in 8-byte units
        icmp[26..32].copy_from_slice(&mac);
    }
    let csum = icmpv6_checksum(&src, &dst, icmp_len as u32, NH_ICMPV6, icmp);
    icmp[2..4].copy_from_slice(&csum.to_be_bytes());
    buf
}

/// Receive-side dispatch: parse an IPv6 packet, and if it's an ICMPv6
/// echo-request to one of our addresses, return the bytes of the
/// echo-reply that should go on the wire.  Anything else returns None.
pub fn handle_inbound(buf: &[u8], our: &[Ipv6Addr]) -> Option<Vec<u8>> {
    let hdr = Ipv6Header::parse(buf)?;
    if hdr.next_header != NH_ICMPV6 { return None; }
    let payload = &buf[IPV6_HEADER_LEN..];
    if payload.len() < 8 { return None; }
    if payload[0] != ICMPV6_ECHO_REQUEST { return None; }
    if !our.contains(&hdr.dst) { return None; }
    let id  = u16::from_be_bytes([payload[4], payload[5]]);
    let seq = u16::from_be_bytes([payload[6], payload[7]]);
    Some(build_echo_reply(hdr.dst, hdr.src, id, seq, &payload[8..]))
}
