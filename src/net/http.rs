//! Minimal HTTP/1.0 client.  Used by `wget` and `pkg fetch`.
//!
//! No streaming, no chunked encoding, no TLS — just a single
//! GET-and-recv on top of the loopback TCP stack we already have.
//! That's enough to talk to the kernel-resident httpd or any plain
//! HTTP/1.0 endpoint reachable via DNS+TCP.
//!
//! `get(url)` returns `(status_line, body)` so the caller can decide
//! how to react (write to file, parse, etc.) and decouples the
//! shell's wget from the package fetcher.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug)]
pub enum HttpError {
    BadUrl,
    DnsFailed,
    ConnectFailed,
    SendFailed,
    RecvFailed,
    BadResponse,
}

/// Parse a URL into (host, port, path).  Accepts http://, https:// (we
/// ignore the TLS — http only at the wire level), and bare host/path.
fn split_url(url: &str) -> Result<(&str, u16, &str), HttpError> {
    let s = url.strip_prefix("http://").unwrap_or(url);
    let s = s.strip_prefix("https://").unwrap_or(s);
    let (authority, path) = match s.find('/') {
        Some(p) => (&s[..p], &s[p..]),
        None    => (s, "/"),
    };
    if authority.is_empty() { return Err(HttpError::BadUrl); }
    let (host, port) = match authority.rfind(':') {
        Some(p) => {
            let port = authority[p + 1..].parse::<u16>().map_err(|_| HttpError::BadUrl)?;
            (&authority[..p], port)
        }
        None => (authority, 80u16),
    };
    Ok((host, port, path))
}

/// HTTP/1.0 GET.  Returns the status line and the body Vec.
pub fn get(url: &str) -> Result<(String, Vec<u8>), HttpError> {
    let (host, port, path) = split_url(url)?;

    let addr = crate::net::dns::resolve(host).ok_or(HttpError::DnsFailed)?;

    use crate::net::tcp::{close, connect, recv, send};
    use crate::net::SocketAddrV4;
    let server_addr = SocketAddrV4 { ip: addr, port };
    let conn = connect(server_addr).map_err(|_| HttpError::ConnectFailed)?;

    let req = alloc::format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: rustos-http/0.1\r\nConnection: close\r\n\r\n",
        path, host);
    if send(conn, req.as_bytes()).is_err() {
        let _ = close(conn);
        return Err(HttpError::SendFailed);
    }

    // If we're talking to the local httpd over loopback, drive one
    // accept→serve→close round so the response is queued before recv.
    if addr.is_loopback() && port == 80 {
        if let Some(server_listener) = crate::httpd::global_listener() {
            let _ = crate::httpd::serve_once(server_listener);
        }
    }

    let mut buf = [0u8; 16384];
    let n = recv(conn, &mut buf).map_err(|_| HttpError::RecvFailed)?;
    let _ = close(conn);

    let resp = &buf[..n];
    // Find header/body split.
    let mut hdr_end = 0usize;
    let mut found = false;
    while hdr_end + 3 < resp.len() {
        if &resp[hdr_end..hdr_end + 4] == b"\r\n\r\n" {
            found = true;
            break;
        }
        hdr_end += 1;
    }
    let body_start = if found { hdr_end + 4 } else { return Err(HttpError::BadResponse); };
    let header_text = core::str::from_utf8(&resp[..hdr_end]).map_err(|_| HttpError::BadResponse)?;
    let status = header_text.lines().next().unwrap_or("").to_string();
    let body = resp[body_start..].to_vec();
    Ok((status, body))
}
