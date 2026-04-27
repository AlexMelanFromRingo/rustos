//! Minimal TCP/IPv4 implementation.
//!
//! Scope:
//!   * 3-way handshake (SYN, SYN+ACK, ACK)
//!   * Bidirectional data transfer with seq/ack tracking
//!   * Active and passive close (FIN/ACK) — TIME_WAIT is collapsed to CLOSED
//!     to keep state simple; RFC 793 compliance is not the goal here
//!   * Loopback-only — there is no retransmit queue, no congestion control,
//!     no out-of-order reassembly, no SACK, no TCP options
//!
//! What works on loopback in this build:
//!   listen → connect → established
//!   stream::send → peer::recv
//!   either side calls close → FIN handshake → both endpoints CLOSED
//!
//! Layered exactly like UDP: socket::ipv4_input dispatches PROTO_TCP to
//! [`tcp_input`].  Outbound writes go through [`emit`] which builds the
//! datagram, hands it to the routing layer, then calls drain_pending so the
//! peer's RX path can run with NET_STACK unheld.

use super::ip::{Ipv4Header, IPV4_HEADER_LEN, PROTO_TCP};
use super::{Ipv4Addr, SocketAddrV4};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

pub const TCP_HEADER_MIN: usize = 20;

mod flags {
    pub const FIN: u8 = 0x01;
    pub const SYN: u8 = 0x02;
    pub const RST: u8 = 0x04;
    pub const PSH: u8 = 0x08;
    pub const ACK: u8 = 0x10;
}

#[derive(Debug, Clone)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub data_offset_words: u8, // header length / 4
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
}

impl TcpHeader {
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < TCP_HEADER_MIN { return None; }
        let data_off_flags = u16::from_be_bytes([buf[12], buf[13]]);
        Some(TcpHeader {
            src_port: u16::from_be_bytes([buf[0], buf[1]]),
            dst_port: u16::from_be_bytes([buf[2], buf[3]]),
            seq: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ack: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
            data_offset_words: ((data_off_flags >> 12) & 0xF) as u8,
            flags: (data_off_flags & 0x3F) as u8,
            window: u16::from_be_bytes([buf[14], buf[15]]),
            checksum: u16::from_be_bytes([buf[16], buf[17]]),
            urgent: u16::from_be_bytes([buf[18], buf[19]]),
        })
    }

    fn write_to(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= TCP_HEADER_MIN);
        buf[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        buf[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        buf[4..8].copy_from_slice(&self.seq.to_be_bytes());
        buf[8..12].copy_from_slice(&self.ack.to_be_bytes());
        let do_flags: u16 = ((self.data_offset_words as u16) << 12) | (self.flags as u16);
        buf[12..14].copy_from_slice(&do_flags.to_be_bytes());
        buf[14..16].copy_from_slice(&self.window.to_be_bytes());
        buf[16..18].copy_from_slice(&[0, 0]);  // checksum cleared first
        buf[18..20].copy_from_slice(&self.urgent.to_be_bytes());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynRcvd,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    Closing,
    TimeWait,
}

impl TcpState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TcpState::Closed      => "CLOSED",
            TcpState::Listen      => "LISTEN",
            TcpState::SynSent     => "SYN_SENT",
            TcpState::SynRcvd     => "SYN_RCVD",
            TcpState::Established => "ESTABLISHED",
            TcpState::FinWait1    => "FIN_WAIT1",
            TcpState::FinWait2    => "FIN_WAIT2",
            TcpState::CloseWait   => "CLOSE_WAIT",
            TcpState::LastAck     => "LAST_ACK",
            TcpState::Closing     => "CLOSING",
            TcpState::TimeWait    => "TIME_WAIT",
        }
    }
}

/// One unacknowledged segment held on the retransmit queue.
#[derive(Debug, Clone)]
pub struct RetxSegment {
    pub seq:        u32,
    pub flags:      u8,
    pub data:       alloc::vec::Vec<u8>,
    pub send_tick:  u64,
    pub retries:    u8,
}

/// Maximum segment size for the loopback link.  Conservative.
pub const MSS: u32 = 1460;

/// One TCP connection (or a listening passive socket).
pub struct TcpEndpoint {
    pub state: TcpState,
    pub local: SocketAddrV4,
    pub remote: SocketAddrV4,
    pub snd_nxt: u32,           // next sequence number we'll send
    pub snd_una: u32,           // oldest unacknowledged sequence
    pub rcv_nxt: u32,           // next byte we expect from peer
    pub rcv_buf: VecDeque<u8>,  // bytes received, awaiting recv()

    // ---- congestion control (slow start / AIMD) ----
    /// Congestion window in bytes.  Starts at MSS, doubles each ACK during
    /// slow start, then grows by MSS/cwnd per ACK after ssthresh, halved on
    /// loss (AIMD).
    pub cwnd:       u32,
    /// Slow-start threshold.  Initially "infinity" (set high).
    pub ssthresh:   u32,
    /// Smoothed RTT estimator (in ticks) — RFC 6298 with α=1/8.  Starts 0.
    pub srtt:       u32,
    pub rttvar:     u32,
    /// Retransmit timeout (ticks).  Default 2 ticks (~110 ms) before any
    /// RTT samples; recomputed as srtt + 4*rttvar after the first ACK.
    pub rto:        u32,
    /// Pending segments awaiting ACK.
    pub retx:       VecDeque<RetxSegment>,
    /// Number of consecutive RTO firings (for exponential backoff).
    pub rto_backoff: u8,

    /// For listeners: queue of (handle, remote-addr) pairs ready for accept.
    pub accept_queue: VecDeque<usize>,
    /// For listeners: outstanding child endpoints created by SYNs.
    pub backlog: u32,
}

impl TcpEndpoint {
    pub fn new_listener(local: SocketAddrV4, backlog: u32) -> Self {
        TcpEndpoint {
            state: TcpState::Listen,
            local,
            remote: SocketAddrV4 { ip: Ipv4Addr::ANY, port: 0 },
            snd_nxt: 0,
            snd_una: 0,
            rcv_nxt: 0,
            rcv_buf: VecDeque::new(),
            cwnd: MSS,
            ssthresh: u32::MAX,
            srtt: 0,
            rttvar: 0,
            rto: 2,
            retx: VecDeque::new(),
            rto_backoff: 0,
            accept_queue: VecDeque::new(),
            backlog,
        }
    }

    fn new_child_for_syn(local: SocketAddrV4, remote: SocketAddrV4, peer_seq: u32) -> Self {
        let iss: u32 = (crate::task::timer::current_ticks() as u32) ^ 0x12345678;
        TcpEndpoint {
            state: TcpState::SynRcvd,
            local,
            remote,
            snd_nxt: iss,
            snd_una: iss,
            rcv_nxt: peer_seq.wrapping_add(1),
            rcv_buf: VecDeque::new(),
            cwnd: MSS, ssthresh: u32::MAX,
            srtt: 0, rttvar: 0, rto: 2,
            retx: VecDeque::new(), rto_backoff: 0,
            accept_queue: VecDeque::new(),
            backlog: 0,
        }
    }

    fn new_active(local: SocketAddrV4, remote: SocketAddrV4) -> Self {
        let iss: u32 = (crate::task::timer::current_ticks() as u32) ^ 0xCAFEBABE;
        TcpEndpoint {
            state: TcpState::SynSent,
            local,
            remote,
            snd_nxt: iss.wrapping_add(1),
            snd_una: iss,
            rcv_nxt: 0,
            rcv_buf: VecDeque::new(),
            cwnd: MSS, ssthresh: u32::MAX,
            srtt: 0, rttvar: 0, rto: 2,
            retx: VecDeque::new(), rto_backoff: 0,
            accept_queue: VecDeque::new(),
            backlog: 0,
        }
    }

    /// Process an ACK (covering snd_una..ack).  Removes acknowledged
    /// segments from the retransmit queue, samples RTT for one of them,
    /// and advances cwnd per congestion-control rules.
    pub fn process_ack(&mut self, ack: u32) {
        // Walk the retx queue from the front, dropping fully-ACKed segs.
        let now = crate::task::timer::current_ticks();
        let mut acked_bytes: u32 = 0;
        let mut newest_acked_send_tick: Option<u64> = None;
        while let Some(seg) = self.retx.front() {
            let end = seg.seq.wrapping_add(seg.data.len() as u32);
            // ack covers entirely past `end` (mod-32 comparison).
            if seq_le(end, ack) {
                acked_bytes = acked_bytes.saturating_add(seg.data.len() as u32);
                if seg.retries == 0 {
                    // Karn's algorithm: only sample RTT from non-retransmitted segs.
                    newest_acked_send_tick = Some(seg.send_tick);
                }
                self.retx.pop_front();
            } else {
                break;
            }
        }

        if acked_bytes > 0 {
            self.snd_una = ack;
            self.rto_backoff = 0;
            // RTT update — RFC 6298 with α=1/8, β=1/4.
            if let Some(st) = newest_acked_send_tick {
                let rtt = now.saturating_sub(st) as u32;
                if self.srtt == 0 {
                    self.srtt = rtt;
                    self.rttvar = rtt / 2;
                } else {
                    let abs_diff = if self.srtt > rtt { self.srtt - rtt } else { rtt - self.srtt };
                    self.rttvar = self.rttvar - (self.rttvar / 4) + (abs_diff / 4);
                    self.srtt = self.srtt - (self.srtt / 8) + (rtt / 8);
                }
                // RTO = SRTT + 4·RTTVAR, floor at 2 ticks (~110 ms).
                self.rto = (self.srtt + 4 * self.rttvar).max(2);
            }
            // Congestion control: slow start while cwnd < ssthresh, then
            // congestion avoidance (cwnd += MSS / cwnd) — implemented as
            // cwnd += MSS·MSS / cwnd to keep the math integer.
            if self.cwnd < self.ssthresh {
                self.cwnd = self.cwnd.saturating_add(acked_bytes.min(MSS));
            } else {
                let inc = MSS.saturating_mul(MSS) / self.cwnd.max(1);
                self.cwnd = self.cwnd.saturating_add(inc.max(1));
            }
        }
    }

    /// Mark a retransmit-timeout event: shrink ssthresh, reset cwnd to
    /// MSS, double RTO, increment backoff.  RFC 6298 §5.
    pub fn on_rto(&mut self) {
        self.ssthresh = (self.cwnd / 2).max(2 * MSS);
        self.cwnd = MSS;
        self.rto = self.rto.saturating_mul(2).min(60); // cap RTO at ~3.3s
        self.rto_backoff = self.rto_backoff.saturating_add(1);
    }
}

/// Modular sequence-number comparison: a <= b in 32-bit serial space.
#[inline]
fn seq_le(a: u32, b: u32) -> bool {
    // a is "less or equal" to b if (b - a) < 2^31.
    b.wrapping_sub(a) < 0x8000_0000
}

/// Endpoint table.  Handles are stable indices.
pub struct TcpTable {
    pub endpoints: Vec<Option<TcpEndpoint>>,
    next_ephemeral_port: u16,
}

impl TcpTable {
    pub const fn new() -> Self {
        TcpTable { endpoints: Vec::new(), next_ephemeral_port: 49152 }
    }

    fn allocate(&mut self, ep: TcpEndpoint) -> usize {
        for (i, slot) in self.endpoints.iter_mut().enumerate() {
            if slot.is_none() { *slot = Some(ep); return i; }
        }
        self.endpoints.push(Some(ep));
        self.endpoints.len() - 1
    }

    fn pick_ephemeral_port(&mut self) -> u16 {
        let p = self.next_ephemeral_port;
        self.next_ephemeral_port = if p >= 65000 { 49152 } else { p + 1 };
        p
    }

    fn find_listener(&mut self, local: SocketAddrV4) -> Option<&mut TcpEndpoint> {
        for slot in self.endpoints.iter_mut() {
            if let Some(ep) = slot {
                if ep.state == TcpState::Listen {
                    let ip_match = ep.local.ip.is_unspecified() || ep.local.ip == local.ip;
                    if ip_match && ep.local.port == local.port {
                        return Some(ep);
                    }
                }
            }
        }
        None
    }

    fn find_endpoint(&mut self, local: SocketAddrV4, remote: SocketAddrV4) -> Option<usize> {
        for (i, slot) in self.endpoints.iter().enumerate() {
            if let Some(ep) = slot {
                if ep.local.port == local.port && ep.remote == remote {
                    return Some(i);
                }
            }
        }
        None
    }
}

pub static TCP: Mutex<TcpTable> = Mutex::new(TcpTable::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpError {
    InvalidHandle,
    NotListening,
    AddressInUse,
    NoRoute,
    Refused,
    AlreadyConnected,
    NotConnected,
    Closed,
}

/// Build and emit one TCP segment.  Returns the wire bytes for diagnostics.
fn emit(local: SocketAddrV4, remote: SocketAddrV4, seq: u32, ack: u32,
        flags: u8, payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    let total_len = IPV4_HEADER_LEN + TCP_HEADER_MIN + payload.len();
    let mut buf = alloc::vec![0u8; total_len];

    let ip_hdr = Ipv4Header::new(local.ip, remote.ip, PROTO_TCP,
        (TCP_HEADER_MIN + payload.len()) as u16);
    ip_hdr.write_to(&mut buf[..IPV4_HEADER_LEN]);

    let mut th = TcpHeader {
        src_port: local.port,
        dst_port: remote.port,
        seq,
        ack,
        data_offset_words: 5,
        flags,
        window: 0xFFFF,
        checksum: 0,
        urgent: 0,
    };
    th.write_to(&mut buf[IPV4_HEADER_LEN..IPV4_HEADER_LEN + TCP_HEADER_MIN]);
    buf[IPV4_HEADER_LEN + TCP_HEADER_MIN..].copy_from_slice(payload);

    // Pseudo-header sum
    let mut sum: u32 = 0;
    sum += u16::from_be_bytes([local.ip.0[0], local.ip.0[1]]) as u32;
    sum += u16::from_be_bytes([local.ip.0[2], local.ip.0[3]]) as u32;
    sum += u16::from_be_bytes([remote.ip.0[0], remote.ip.0[1]]) as u32;
    sum += u16::from_be_bytes([remote.ip.0[2], remote.ip.0[3]]) as u32;
    sum += PROTO_TCP as u32;
    sum += (TCP_HEADER_MIN + payload.len()) as u32;
    let segment = &buf[IPV4_HEADER_LEN..];
    let mut i = 0;
    while i + 1 < segment.len() {
        sum += u16::from_be_bytes([segment[i], segment[i + 1]]) as u32;
        i += 2;
    }
    if i < segment.len() {
        sum += (segment[i] as u32) << 8;
    }
    while (sum >> 16) != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    let cksum = !(sum as u16);
    th.checksum = if cksum == 0 { 0xFFFF } else { cksum };
    buf[IPV4_HEADER_LEN + 16..IPV4_HEADER_LEN + 18].copy_from_slice(&th.checksum.to_be_bytes());

    {
        let mut stack = super::NET_STACK.lock();
        let iface_entry = stack.route(remote.ip).ok_or("no route")?;
        iface_entry.iface.transmit_ipv4(&buf)?;
    }
    super::drain_pending();
    Ok(buf)
}

/// Open a listening socket bound to `addr` with the given backlog.
pub fn listen(addr: SocketAddrV4, backlog: u32) -> Result<usize, TcpError> {
    let mut table = TCP.lock();
    // Address-in-use check
    for slot in table.endpoints.iter() {
        if let Some(ep) = slot {
            if ep.state == TcpState::Listen && ep.local == addr {
                return Err(TcpError::AddressInUse);
            }
        }
    }
    let h = table.allocate(TcpEndpoint::new_listener(addr, backlog));
    Ok(h)
}

/// Pop the next accepted connection from a listener's queue, if any.
/// Returns the accepted endpoint's handle.
pub fn accept(listener_handle: usize) -> Result<usize, TcpError> {
    let mut table = TCP.lock();
    let lp = table.endpoints.get_mut(listener_handle)
        .and_then(|s| s.as_mut())
        .ok_or(TcpError::InvalidHandle)?;
    if lp.state != TcpState::Listen { return Err(TcpError::NotListening); }
    lp.accept_queue.pop_front().ok_or(TcpError::NotListening)
}

/// Initiate a connection.  Caller must drive [`drain_pending`] indirectly
/// through [`emit`]; the SYN+ACK from the peer arrives synchronously on
/// loopback, so by the time this returns we are already ESTABLISHED.
pub fn connect(remote: SocketAddrV4) -> Result<usize, TcpError> {
    let mut table = TCP.lock();
    let local_port = table.pick_ephemeral_port();
    let local = SocketAddrV4 { ip: Ipv4Addr::LOCALHOST, port: local_port };
    let ep = TcpEndpoint::new_active(local, remote);
    let iss = ep.snd_una;
    let h = table.allocate(ep);
    drop(table);

    // Send SYN.  drain_pending fires the SYN-ACK back into us, which then
    // emits the final ACK -> ESTABLISHED.
    emit(local, remote, iss, 0, flags::SYN, &[])
        .map_err(|_| TcpError::NoRoute)?;

    // After drain_pending the local endpoint should now be ESTABLISHED.
    let table = TCP.lock();
    let ep = table.endpoints.get(h).and_then(|s| s.as_ref())
        .ok_or(TcpError::InvalidHandle)?;
    if ep.state == TcpState::Established { Ok(h) }
    else if ep.state == TcpState::Closed { Err(TcpError::Refused) }
    else { Ok(h) }
}

/// Send `data` on an established connection.  Records the segment on the
/// per-endpoint retransmit queue so it can be replayed if the peer doesn't
/// ACK within RTO.
pub fn send(h: usize, data: &[u8]) -> Result<usize, TcpError> {
    let (local, remote, seq, ack) = {
        let mut table = TCP.lock();
        let ep = table.endpoints.get_mut(h).and_then(|s| s.as_mut())
            .ok_or(TcpError::InvalidHandle)?;
        if ep.state != TcpState::Established && ep.state != TcpState::CloseWait {
            return Err(TcpError::NotConnected);
        }
        let seq = ep.snd_nxt;
        let ack = ep.rcv_nxt;
        ep.snd_nxt = ep.snd_nxt.wrapping_add(data.len() as u32);

        // Record on the retransmit queue.
        ep.retx.push_back(RetxSegment {
            seq,
            flags: flags::ACK | flags::PSH,
            data: data.to_vec(),
            send_tick: crate::task::timer::current_ticks(),
            retries: 0,
        });

        (ep.local, ep.remote, seq, ack)
    };
    emit(local, remote, seq, ack, flags::ACK | flags::PSH, data)
        .map_err(|_| TcpError::Closed)?;
    Ok(data.len())
}

/// Periodic tick called by a kernel task: walk every endpoint's retx
/// queue, retransmit segments past their RTO, halve cwnd on each RTO fire.
/// Returns the number of segments retransmitted this tick.
pub fn retransmit_tick() -> usize {
    let now = crate::task::timer::current_ticks();
    let mut to_retx: Vec<(SocketAddrV4, SocketAddrV4, u32, u32, u8, alloc::vec::Vec<u8>)> = Vec::new();
    {
        let mut table = TCP.lock();
        for slot in table.endpoints.iter_mut() {
            if let Some(ep) = slot {
                if ep.retx.is_empty() { continue; }
                // RTO fires on the oldest queued segment.
                let oldest = ep.retx.front().unwrap();
                let elapsed = now.saturating_sub(oldest.send_tick) as u32;
                if elapsed < ep.rto { continue; }

                ep.on_rto();
                let ack = ep.rcv_nxt;

                // Move the oldest seg to the back, bump retries, mark resend.
                if let Some(mut seg) = ep.retx.pop_front() {
                    seg.retries = seg.retries.saturating_add(1);
                    seg.send_tick = now;
                    let local = ep.local;
                    let remote = ep.remote;
                    to_retx.push((local, remote, seg.seq, ack, seg.flags, seg.data.clone()));
                    ep.retx.push_back(seg);
                }
            }
        }
    }
    let n = to_retx.len();
    for (local, remote, seq, ack, fl, data) in to_retx {
        let _ = emit(local, remote, seq, ack, fl, &data);
    }
    n
}

/// Receive up to `buf.len()` bytes from the connection.  Non-blocking.
pub fn recv(h: usize, buf: &mut [u8]) -> Result<usize, TcpError> {
    let mut table = TCP.lock();
    let ep = table.endpoints.get_mut(h).and_then(|s| s.as_mut())
        .ok_or(TcpError::InvalidHandle)?;
    if ep.rcv_buf.is_empty() {
        if matches!(ep.state, TcpState::Closed | TcpState::CloseWait | TcpState::LastAck) {
            return Ok(0); // EOF
        }
        return Err(TcpError::NotConnected);
    }
    let mut n = 0;
    while n < buf.len() {
        match ep.rcv_buf.pop_front() {
            Some(b) => { buf[n] = b; n += 1; }
            None => break,
        }
    }
    Ok(n)
}

/// Active close — sends FIN and walks toward CLOSED via FIN_WAIT_*.
pub fn close(h: usize) -> Result<(), TcpError> {
    let (local, remote, seq, ack, do_fin) = {
        let mut table = TCP.lock();
        let ep = table.endpoints.get_mut(h).and_then(|s| s.as_mut())
            .ok_or(TcpError::InvalidHandle)?;
        match ep.state {
            TcpState::Established => {
                ep.state = TcpState::FinWait1;
                let seq = ep.snd_nxt;
                ep.snd_nxt = ep.snd_nxt.wrapping_add(1); // FIN consumes one seq
                (ep.local, ep.remote, seq, ep.rcv_nxt, true)
            }
            TcpState::CloseWait => {
                ep.state = TcpState::LastAck;
                let seq = ep.snd_nxt;
                ep.snd_nxt = ep.snd_nxt.wrapping_add(1);
                (ep.local, ep.remote, seq, ep.rcv_nxt, true)
            }
            TcpState::Listen | TcpState::Closed => {
                ep.state = TcpState::Closed;
                return Ok(());
            }
            _ => return Ok(()),
        }
    };
    if do_fin {
        let _ = emit(local, remote, seq, ack, flags::FIN | flags::ACK, &[]);
    }
    Ok(())
}

/// Inbound dispatch from socket::ipv4_input.
pub fn tcp_input(ip_hdr: &Ipv4Header, segment: &[u8]) {
    let th = match TcpHeader::parse(segment) {
        Some(t) => t,
        None => return,
    };
    let header_len = th.data_offset_words as usize * 4;
    if segment.len() < header_len { return; }
    let payload = &segment[header_len..];

    let local = SocketAddrV4 { ip: ip_hdr.dst, port: th.dst_port };
    let remote = SocketAddrV4 { ip: ip_hdr.src, port: th.src_port };

    // Look for a matching established/active endpoint first.
    let mut table = TCP.lock();
    if let Some(idx) = table.find_endpoint(local, remote) {
        let pending = process_segment_for(&mut table, idx, &th, payload);
        // Drop the TCP lock before emitting any ACKs so the peer's
        // re-entrant ipv4_input path can lock TCP again.
        drop(table);
        if let Some((seq, ack, fl)) = pending {
            let _ = emit(local, remote, seq, ack, fl, &[]);
        }
        return;
    }

    // Otherwise, try to match a LISTEN.
    if th.flags & flags::SYN != 0 && th.flags & flags::ACK == 0 {
        if let Some(_listener) = table.find_listener(local) {
            // Spawn a child endpoint, queue it on the listener.
            let child = TcpEndpoint::new_child_for_syn(local, remote, th.seq);
            let child_iss = child.snd_una;
            let child_rcvnxt = child.rcv_nxt;
            let child_idx = table.allocate(child);

            // Re-find listener (the borrow checker requires this).
            if let Some(listener) = table.find_listener(local) {
                listener.accept_queue.push_back(child_idx);
            }

            // Reply with SYN+ACK, ack = peer.seq+1, seq = our_iss.
            drop(table);
            let _ = emit(local, remote, child_iss, child_rcvnxt,
                flags::SYN | flags::ACK, &[]);
            return;
        }

        // No listener — RST.
        drop(table);
        let _ = emit(local, remote, 0, th.seq.wrapping_add(1), flags::RST | flags::ACK, &[]);
        return;
    }

    // No matching endpoint and not a SYN — RST.
    drop(table);
    let _ = emit(local, remote, th.ack, th.seq.wrapping_add(payload.len() as u32),
                 flags::RST, &[]);
}

fn process_segment_for(
    table: &mut TcpTable,
    idx: usize,
    th: &TcpHeader,
    payload: &[u8],
) -> Option<(u32, u32, u8)> {
    let ep = match table.endpoints.get_mut(idx).and_then(|s| s.as_mut()) {
        Some(e) => e,
        None => return None,
    };
    let _local = ep.local;
    let _remote = ep.remote;

    let mut send_ack: Option<(u32, u32, u8)> = None; // (seq, ack, flags)

    match ep.state {
        TcpState::SynSent => {
            // Expect SYN+ACK in response to our SYN.
            if th.flags & (flags::SYN | flags::ACK) == (flags::SYN | flags::ACK) {
                ep.snd_una = th.ack;
                ep.rcv_nxt = th.seq.wrapping_add(1);
                ep.state = TcpState::Established;
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
            } else if th.flags & flags::RST != 0 {
                ep.state = TcpState::Closed;
            }
        }
        TcpState::SynRcvd => {
            if th.flags & flags::ACK != 0 && th.ack == ep.snd_nxt.wrapping_add(1) {
                ep.snd_una = th.ack;
                ep.snd_nxt = th.ack;
                ep.state = TcpState::Established;
            }
        }
        TcpState::Established => {
            if !payload.is_empty() && th.seq == ep.rcv_nxt {
                for &b in payload { ep.rcv_buf.push_back(b); }
                ep.rcv_nxt = ep.rcv_nxt.wrapping_add(payload.len() as u32);
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
            }
            if th.flags & flags::ACK != 0 {
                ep.process_ack(th.ack);
            }
            if th.flags & flags::FIN != 0 {
                ep.rcv_nxt = ep.rcv_nxt.wrapping_add(1);
                ep.state = TcpState::CloseWait;
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
            }
        }
        TcpState::FinWait1 => {
            if th.flags & flags::FIN != 0 && th.flags & flags::ACK != 0 {
                // Peer FIN+ACK: go straight to CLOSED (skipping TIME_WAIT
                // since loopback doesn't need it).
                ep.rcv_nxt = th.seq.wrapping_add(1);
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
                ep.state = TcpState::Closed;
            } else if th.flags & flags::ACK != 0 {
                ep.state = TcpState::FinWait2;
            } else if th.flags & flags::FIN != 0 {
                ep.rcv_nxt = th.seq.wrapping_add(1);
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
                ep.state = TcpState::Closing;
            }
        }
        TcpState::FinWait2 => {
            if th.flags & flags::FIN != 0 {
                ep.rcv_nxt = th.seq.wrapping_add(1);
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
                ep.state = TcpState::Closed;
            }
        }
        TcpState::CloseWait => {
            // Buffer late data, ack everything.
            if !payload.is_empty() && th.seq == ep.rcv_nxt {
                for &b in payload { ep.rcv_buf.push_back(b); }
                ep.rcv_nxt = ep.rcv_nxt.wrapping_add(payload.len() as u32);
                send_ack = Some((ep.snd_nxt, ep.rcv_nxt, flags::ACK));
            }
        }
        TcpState::LastAck => {
            if th.flags & flags::ACK != 0 {
                ep.state = TcpState::Closed;
            }
        }
        _ => {} // Closed / Listen / Closing / TimeWait — ignore here
    }

    // Return the pending ACK to be sent by the caller after dropping the
    // TCP lock (avoids deadlock if the peer's ipv4_input re-enters us).
    send_ack
}

/// Snapshot for netstat-style listing.
pub fn enumerate() -> Vec<(usize, TcpState, SocketAddrV4, SocketAddrV4, usize)> {
    let table = TCP.lock();
    table.endpoints.iter().enumerate().filter_map(|(i, slot)| {
        slot.as_ref().map(|ep| (i, ep.state, ep.local, ep.remote, ep.rcv_buf.len()))
    }).collect()
}

/// Detailed view used by `tcpinfo` for diagnostics.
#[derive(Debug, Clone)]
pub struct EndpointInfo {
    pub handle: usize,
    pub state: TcpState,
    pub local: SocketAddrV4,
    pub remote: SocketAddrV4,
    pub snd_nxt: u32,
    pub snd_una: u32,
    pub rcv_nxt: u32,
    pub cwnd: u32,
    pub ssthresh: u32,
    pub srtt: u32,
    pub rttvar: u32,
    pub rto: u32,
    pub retx_len: usize,
    pub retx_backoff: u8,
    pub rcv_buf_len: usize,
}

pub fn enumerate_detailed() -> Vec<EndpointInfo> {
    let table = TCP.lock();
    table.endpoints.iter().enumerate().filter_map(|(i, slot)| {
        slot.as_ref().map(|ep| EndpointInfo {
            handle: i,
            state: ep.state,
            local: ep.local,
            remote: ep.remote,
            snd_nxt: ep.snd_nxt,
            snd_una: ep.snd_una,
            rcv_nxt: ep.rcv_nxt,
            cwnd: ep.cwnd,
            ssthresh: ep.ssthresh,
            srtt: ep.srtt,
            rttvar: ep.rttvar,
            rto: ep.rto,
            retx_len: ep.retx.len(),
            retx_backoff: ep.rto_backoff,
            rcv_buf_len: ep.rcv_buf.len(),
        })
    }).collect()
}
