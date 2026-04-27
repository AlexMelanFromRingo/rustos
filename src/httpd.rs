//! Tiny HTTP/1.0 server.
//!
//! Demonstrates the loopback TCP stack and VFS working together.  Serves
//! static files from /var/www; default document is index.html.  No
//! keep-alive, no chunked encoding, no MIME database — just enough to show
//! GET → 200 OK / 404 Not Found round-trips.

use crate::net::tcp;
use crate::net::{Ipv4Addr, SocketAddrV4};
use alloc::format;
use alloc::string::String;

/// Default document root.
pub const DOCUMENT_ROOT: &str = "/var/www";

/// Serve one request synchronously: accept the next pending connection,
/// parse a single GET request, write the response, close.
///
/// Returns Ok(handle) of the accepted socket, or Err(reason).  Caller is
/// expected to invoke this in a loop from a service tick.
pub fn serve_once(listener: usize) -> Result<usize, &'static str> {
    let accepted = tcp::accept(listener).map_err(|_| "no pending connection")?;

    // Read request line + headers (best-effort, single recv).
    let mut buf = [0u8; 1024];
    let n = match tcp::recv(accepted, &mut buf) {
        Ok(n) => n,
        Err(_) => 0,
    };
    let request = core::str::from_utf8(&buf[..n]).unwrap_or("");
    let path = parse_get_path(request).unwrap_or("/");

    let response = build_response(path);
    let _ = tcp::send(accepted, response.as_bytes());
    let _ = tcp::close(accepted);
    Ok(accepted)
}

fn parse_get_path(request: &str) -> Option<&str> {
    let first_line = request.lines().next()?;
    let mut it = first_line.split_whitespace();
    let method = it.next()?;
    if !method.eq_ignore_ascii_case("GET") { return None; }
    it.next() // path
}

fn build_response(path: &str) -> String {
    // Map "/" → "/index.html"; reject ".." path traversal.
    if path.contains("..") {
        return error_response(403, "Forbidden", "Path traversal not allowed");
    }
    let target = if path == "/" { "/index.html" } else { path };
    let full_path = format!("{}{}", DOCUMENT_ROOT, target);

    match crate::fs::vfs::VfsContext::read(&full_path) {
        Ok(body) => {
            let content_type = guess_mime(target);
            let mut resp = String::new();
            resp.push_str("HTTP/1.0 200 OK\r\n");
            resp.push_str(&format!("Content-Type: {}\r\n", content_type));
            resp.push_str(&format!("Content-Length: {}\r\n", body.len()));
            resp.push_str("Server: rustos-httpd/0.1\r\n");
            resp.push_str("Connection: close\r\n");
            resp.push_str("\r\n");
            // Body may not be valid UTF-8 but for our tiny use case we treat
            // everything as text.
            if let Ok(s) = core::str::from_utf8(&body) {
                resp.push_str(s);
            } else {
                resp.push_str("[binary content]");
            }
            resp
        }
        Err(_) => error_response(404, "Not Found",
            &format!("File '{}' not found in {}", target, DOCUMENT_ROOT)),
    }
}

fn error_response(code: u16, status: &str, message: &str) -> String {
    let body = format!(
        "<html><head><title>{} {}</title></head><body><h1>{} {}</h1><p>{}</p></body></html>\r\n",
        code, status, code, status, message
    );
    let mut resp = String::new();
    resp.push_str(&format!("HTTP/1.0 {} {}\r\n", code, status));
    resp.push_str("Content-Type: text/html\r\n");
    resp.push_str(&format!("Content-Length: {}\r\n", body.len()));
    resp.push_str("Server: rustos-httpd/0.1\r\n");
    resp.push_str("Connection: close\r\n");
    resp.push_str("\r\n");
    resp.push_str(&body);
    resp
}

fn guess_mime(path: &str) -> &'static str {
    if path.ends_with(".html") || path.ends_with(".htm") { "text/html" }
    else if path.ends_with(".txt") { "text/plain" }
    else if path.ends_with(".css")  { "text/css" }
    else if path.ends_with(".js")   { "application/javascript" }
    else if path.ends_with(".json") { "application/json" }
    else if path.ends_with(".png")  { "image/png" }
    else if path.ends_with(".jpg") || path.ends_with(".jpeg") { "image/jpeg" }
    else { "application/octet-stream" }
}

/// Provision /var/www with a default index page so out-of-the-box the
/// server has something to serve.
pub fn install_default_docroot() {
    use crate::fs::vfs::VfsContext;
    let _ = VfsContext::mkdir("/var/www");
    let html = b"<!doctype html>\n\
<html><head><title>RustOS</title></head>\n\
<body>\n\
  <h1>It works!</h1>\n\
  <p>This is the default page served by <code>rustos-httpd</code>.</p>\n\
  <p>Source: <a href=\"https://github.com/AlexMelanFromRingo/rustos\">rustos on GitHub</a></p>\n\
</body></html>\n";
    let _ = VfsContext::write("/var/www/index.html", html.to_vec());
    let _ = VfsContext::write("/var/www/hello.txt", b"Hello from RustOS\n".to_vec());
}

/// Spin up the server on `port` and synchronously serve a single request,
/// for use as a self-test.  Returns the listener handle (caller closes).
pub fn start(port: u16) -> Result<usize, &'static str> {
    let addr = SocketAddrV4 { ip: Ipv4Addr::LOCALHOST, port };
    tcp::listen(addr, 8).map_err(|_| "listen failed")
}
