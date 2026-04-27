//! AF_UNIX (Unix domain) sockets.
//!
//! Each Unix domain socket is identified by a filesystem path.  Two flavours
//! are supported:
//!
//!   * SOCK_DGRAM — connectionless message-oriented; sendto(path, msg) puts
//!     the datagram on the queue of the socket bound to `path`.
//!   * SOCK_STREAM — connection-oriented; listen/connect/accept produce a
//!     pair of cross-linked endpoints, send/recv ferry bytes between them.
//!
//! No filesystem visibility yet (the path is just a key in the global
//! socket map; we don't create a real socket inode at the path).  This is
//! enough to test the API surface.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixKind {
    Datagram,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixState {
    Closed,
    Bound,
    Listening,
    Connected,
}

pub struct UnixEndpoint {
    pub kind: UnixKind,
    pub state: UnixState,
    pub bound: Option<String>,
    /// For SOCK_STREAM: handle of the connected peer.  None on listeners.
    pub peer: Option<usize>,
    /// Receive queue: datagrams (with src path) for SOCK_DGRAM, raw bytes
    /// for SOCK_STREAM.
    pub rx_dgrams: VecDeque<(String, Vec<u8>)>,
    pub rx_bytes: VecDeque<u8>,
    /// Listener: handles of accepted endpoints awaiting accept().
    pub accept_queue: VecDeque<usize>,
}

impl UnixEndpoint {
    fn new(kind: UnixKind) -> Self {
        UnixEndpoint {
            kind,
            state: UnixState::Closed,
            bound: None,
            peer: None,
            rx_dgrams: VecDeque::new(),
            rx_bytes: VecDeque::new(),
            accept_queue: VecDeque::new(),
        }
    }
}

pub struct UnixTable {
    pub endpoints: Vec<Option<UnixEndpoint>>,
}

impl UnixTable {
    pub const fn new() -> Self { UnixTable { endpoints: Vec::new() } }

    fn allocate(&mut self, ep: UnixEndpoint) -> usize {
        for (i, slot) in self.endpoints.iter_mut().enumerate() {
            if slot.is_none() { *slot = Some(ep); return i; }
        }
        self.endpoints.push(Some(ep));
        self.endpoints.len() - 1
    }

    fn find_bound(&mut self, path: &str) -> Option<usize> {
        for (i, slot) in self.endpoints.iter().enumerate() {
            if let Some(ep) = slot {
                if ep.bound.as_deref() == Some(path) { return Some(i); }
            }
        }
        None
    }
}

pub static UNIX: Mutex<UnixTable> = Mutex::new(UnixTable::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixError {
    InvalidHandle,
    AddressInUse,
    NotBound,
    NotListening,
    NotConnected,
    AlreadyConnected,
    Refused,
    NoData,
}

pub fn socket(kind: UnixKind) -> usize {
    UNIX.lock().allocate(UnixEndpoint::new(kind))
}

pub fn bind(h: usize, path: &str) -> Result<(), UnixError> {
    let mut t = UNIX.lock();
    if t.endpoints.iter().any(|s| s.as_ref().map_or(false, |e| e.bound.as_deref() == Some(path))) {
        return Err(UnixError::AddressInUse);
    }
    let ep = t.endpoints.get_mut(h).and_then(|s| s.as_mut()).ok_or(UnixError::InvalidHandle)?;
    ep.bound = Some(path.to_string());
    ep.state = UnixState::Bound;
    Ok(())
}

pub fn listen(h: usize, _backlog: u32) -> Result<(), UnixError> {
    let mut t = UNIX.lock();
    let ep = t.endpoints.get_mut(h).and_then(|s| s.as_mut()).ok_or(UnixError::InvalidHandle)?;
    if ep.kind != UnixKind::Stream { return Err(UnixError::NotListening); }
    if ep.bound.is_none() { return Err(UnixError::NotBound); }
    ep.state = UnixState::Listening;
    Ok(())
}

pub fn accept(h: usize) -> Result<usize, UnixError> {
    let mut t = UNIX.lock();
    let ep = t.endpoints.get_mut(h).and_then(|s| s.as_mut()).ok_or(UnixError::InvalidHandle)?;
    if ep.state != UnixState::Listening { return Err(UnixError::NotListening); }
    ep.accept_queue.pop_front().ok_or(UnixError::NoData)
}

pub fn connect(client: usize, path: &str) -> Result<(), UnixError> {
    let server_handle = {
        let mut t = UNIX.lock();
        let server_idx = t.find_bound(path).ok_or(UnixError::Refused)?;
        let server_state = t.endpoints[server_idx].as_ref().unwrap().state;
        let server_kind = t.endpoints[server_idx].as_ref().unwrap().kind;
        if server_kind != UnixKind::Stream { return Err(UnixError::Refused); }
        if server_state != UnixState::Listening { return Err(UnixError::Refused); }
        server_idx
    };

    // Allocate a server-side accepted endpoint and link it to the client.
    let mut t = UNIX.lock();
    let mut accepted = UnixEndpoint::new(UnixKind::Stream);
    accepted.state = UnixState::Connected;
    accepted.peer = Some(client);
    let accepted_h = t.allocate(accepted);

    // Update the client to connected and pointed at the accepted endpoint.
    {
        let cli = t.endpoints.get_mut(client).and_then(|s| s.as_mut())
            .ok_or(UnixError::InvalidHandle)?;
        cli.state = UnixState::Connected;
        cli.peer = Some(accepted_h);
    }

    // Queue the accepted handle on the listener.
    if let Some(ep) = t.endpoints.get_mut(server_handle).and_then(|s| s.as_mut()) {
        ep.accept_queue.push_back(accepted_h);
    }

    Ok(())
}

pub fn send(h: usize, data: &[u8]) -> Result<usize, UnixError> {
    let mut t = UNIX.lock();
    let peer_h = {
        let ep = t.endpoints.get(h).and_then(|s| s.as_ref()).ok_or(UnixError::InvalidHandle)?;
        if ep.kind != UnixKind::Stream { return Err(UnixError::NotConnected); }
        if ep.state != UnixState::Connected { return Err(UnixError::NotConnected); }
        ep.peer.ok_or(UnixError::NotConnected)?
    };

    let peer = t.endpoints.get_mut(peer_h).and_then(|s| s.as_mut())
        .ok_or(UnixError::NotConnected)?;
    for &b in data { peer.rx_bytes.push_back(b); }
    Ok(data.len())
}

pub fn recv(h: usize, buf: &mut [u8]) -> Result<usize, UnixError> {
    let mut t = UNIX.lock();
    let ep = t.endpoints.get_mut(h).and_then(|s| s.as_mut()).ok_or(UnixError::InvalidHandle)?;
    if ep.kind != UnixKind::Stream { return Err(UnixError::NotConnected); }
    if ep.rx_bytes.is_empty() {
        return Err(UnixError::NoData);
    }
    let mut n = 0;
    while n < buf.len() {
        match ep.rx_bytes.pop_front() {
            Some(b) => { buf[n] = b; n += 1; }
            None => break,
        }
    }
    Ok(n)
}

pub fn sendto(client: usize, dst_path: &str, data: &[u8]) -> Result<usize, UnixError> {
    let mut t = UNIX.lock();
    let src_path = t.endpoints.get(client).and_then(|s| s.as_ref())
        .and_then(|e| e.bound.clone())
        .unwrap_or_else(|| String::from("(anonymous)"));
    let target = t.find_bound(dst_path).ok_or(UnixError::Refused)?;
    let ep = t.endpoints.get_mut(target).and_then(|s| s.as_mut()).ok_or(UnixError::Refused)?;
    if ep.kind != UnixKind::Datagram { return Err(UnixError::Refused); }
    ep.rx_dgrams.push_back((src_path, data.to_vec()));
    Ok(data.len())
}

pub fn recvfrom(h: usize, buf: &mut [u8]) -> Result<(usize, String), UnixError> {
    let mut t = UNIX.lock();
    let ep = t.endpoints.get_mut(h).and_then(|s| s.as_mut()).ok_or(UnixError::InvalidHandle)?;
    if ep.kind != UnixKind::Datagram { return Err(UnixError::NotConnected); }
    let (src, payload) = ep.rx_dgrams.pop_front().ok_or(UnixError::NoData)?;
    let n = payload.len().min(buf.len());
    buf[..n].copy_from_slice(&payload[..n]);
    Ok((n, src))
}

pub fn close(h: usize) {
    let mut t = UNIX.lock();
    if h < t.endpoints.len() { t.endpoints[h] = None; }
}

/// Snapshot for shell `ss -x`-style listing.
pub fn enumerate() -> Vec<(usize, UnixKind, UnixState, Option<String>, usize)> {
    let t = UNIX.lock();
    t.endpoints.iter().enumerate().filter_map(|(i, slot)| {
        slot.as_ref().map(|e| {
            let qlen = if e.kind == UnixKind::Datagram { e.rx_dgrams.len() } else { e.rx_bytes.len() };
            (i, e.kind, e.state, e.bound.clone(), qlen)
        })
    }).collect()
}
