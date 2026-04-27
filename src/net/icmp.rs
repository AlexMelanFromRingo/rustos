//! Minimal ICMPv4 layer.
//!
//! Only handles echo-request / echo-reply (ping).  All other ICMP types are
//! silently dropped.  Echo replies are queued onto a small ring buffer so
//! the shell `ping` command can match request→reply by (id, seq).

use super::ip::{compute_checksum, Ipv4Header, IPV4_HEADER_LEN, PROTO_ICMP};
use super::Ipv4Addr;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

pub const ICMP_TYPE_ECHO_REPLY:   u8 = 0;
pub const ICMP_TYPE_DEST_UNREACH: u8 = 3;
pub const ICMP_TYPE_ECHO_REQUEST: u8 = 8;

const ICMP_HEADER_LEN: usize = 8;

#[derive(Debug, Clone)]
pub struct IcmpHeader {
    pub kind: u8,
    pub code: u8,
    pub checksum: u16,
    pub identifier: u16,
    pub sequence: u16,
}

impl IcmpHeader {
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < ICMP_HEADER_LEN { return None; }
        Some(IcmpHeader {
            kind: buf[0],
            code: buf[1],
            checksum: u16::from_be_bytes([buf[2], buf[3]]),
            identifier: u16::from_be_bytes([buf[4], buf[5]]),
            sequence: u16::from_be_bytes([buf[6], buf[7]]),
        })
    }
}

/// One queued echo reply waiting to be matched by a ping client.
#[derive(Debug, Clone)]
pub struct EchoReply {
    pub from: Ipv4Addr,
    pub identifier: u16,
    pub sequence: u16,
    pub payload: Vec<u8>,
    pub recv_tick: u64,
}

static ECHO_REPLIES: Mutex<VecDeque<EchoReply>> = Mutex::new(VecDeque::new());

/// Send an ICMP echo-request to `dst`.  Returns the wire bytes also so the
/// caller can record TX counters / timestamps if useful.
pub fn send_echo_request(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    identifier: u16,
    sequence: u16,
    payload: &[u8],
) -> Result<(), &'static str> {
    let total_len = IPV4_HEADER_LEN + ICMP_HEADER_LEN + payload.len();
    let mut buf = alloc::vec![0u8; total_len];

    let ip_hdr = Ipv4Header::new(src, dst, PROTO_ICMP,
        (ICMP_HEADER_LEN + payload.len()) as u16);
    ip_hdr.write_to(&mut buf[..IPV4_HEADER_LEN]);

    // ICMP header — checksum cleared first
    let icmp_off = IPV4_HEADER_LEN;
    buf[icmp_off]     = ICMP_TYPE_ECHO_REQUEST;
    buf[icmp_off + 1] = 0;
    buf[icmp_off + 2] = 0;
    buf[icmp_off + 3] = 0;
    buf[icmp_off + 4..icmp_off + 6].copy_from_slice(&identifier.to_be_bytes());
    buf[icmp_off + 6..icmp_off + 8].copy_from_slice(&sequence.to_be_bytes());
    buf[icmp_off + ICMP_HEADER_LEN..].copy_from_slice(payload);

    // ICMP checksum covers ICMP header + payload (no pseudo-header for ICMPv4).
    let cksum = compute_checksum(&buf[icmp_off..]);
    buf[icmp_off + 2..icmp_off + 4].copy_from_slice(&cksum.to_be_bytes());

    // Route + transmit
    {
        let mut stack = super::NET_STACK.lock();
        let iface_entry = stack.route(dst).ok_or("no route")?;
        iface_entry.iface.transmit_ipv4(&buf)?;
    }
    super::drain_pending();
    Ok(())
}

/// Called from `socket::ipv4_input` for protocol = 1.
pub fn icmp_input(ip_hdr: &Ipv4Header, segment: &[u8]) {
    let hdr = match IcmpHeader::parse(segment) {
        Some(h) => h,
        None => return,
    };

    match hdr.kind {
        ICMP_TYPE_ECHO_REQUEST => {
            // Reply: swap src/dst, change type to ECHO_REPLY, recompute checksums.
            let payload = &segment[ICMP_HEADER_LEN..];
            let _ = send_echo_reply(ip_hdr.dst, ip_hdr.src, hdr.identifier, hdr.sequence, payload);
        }
        ICMP_TYPE_ECHO_REPLY => {
            // Queue it for any waiting ping client.
            let payload = segment[ICMP_HEADER_LEN..].to_vec();
            ECHO_REPLIES.lock().push_back(EchoReply {
                from: ip_hdr.src,
                identifier: hdr.identifier,
                sequence: hdr.sequence,
                payload,
                recv_tick: crate::task::timer::current_ticks(),
            });
        }
        _ => {} // ignore other ICMP types
    }
}

fn send_echo_reply(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    identifier: u16,
    sequence: u16,
    payload: &[u8],
) -> Result<(), &'static str> {
    let total_len = IPV4_HEADER_LEN + ICMP_HEADER_LEN + payload.len();
    let mut buf = alloc::vec![0u8; total_len];

    let ip_hdr = Ipv4Header::new(src, dst, PROTO_ICMP,
        (ICMP_HEADER_LEN + payload.len()) as u16);
    ip_hdr.write_to(&mut buf[..IPV4_HEADER_LEN]);

    let icmp_off = IPV4_HEADER_LEN;
    buf[icmp_off]     = ICMP_TYPE_ECHO_REPLY;
    buf[icmp_off + 1] = 0;
    buf[icmp_off + 2] = 0;
    buf[icmp_off + 3] = 0;
    buf[icmp_off + 4..icmp_off + 6].copy_from_slice(&identifier.to_be_bytes());
    buf[icmp_off + 6..icmp_off + 8].copy_from_slice(&sequence.to_be_bytes());
    buf[icmp_off + ICMP_HEADER_LEN..].copy_from_slice(payload);

    let cksum = compute_checksum(&buf[icmp_off..]);
    buf[icmp_off + 2..icmp_off + 4].copy_from_slice(&cksum.to_be_bytes());

    {
        let mut stack = super::NET_STACK.lock();
        let iface_entry = stack.route(dst).ok_or("no route")?;
        iface_entry.iface.transmit_ipv4(&buf)?;
    }
    super::drain_pending();
    Ok(())
}

/// Pull the first matching echo reply (by id+seq), if any.
pub fn drain_echo_reply(identifier: u16, sequence: u16) -> Option<EchoReply> {
    let mut q = ECHO_REPLIES.lock();
    if let Some(pos) = q.iter().position(|r| r.identifier == identifier && r.sequence == sequence) {
        q.remove(pos)
    } else {
        None
    }
}
