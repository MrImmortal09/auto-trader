//! A stand-in for the server in deploy-test.yml: answers `/api/health` and
//! `/api/version` on 127.0.0.1:8080, with the version read from its own file
//! name (`server-0.1.201-x86_64-unknown-linux-gnu` -> `0.1.201`), and dies on
//! Ctrl-C like the real one.

use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let exe = std::env::args().next().unwrap_or_default();
    let name = std::path::Path::new(&exe).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let version = name.strip_prefix("server-").and_then(|s| s.split('-').next()).unwrap_or("unknown").to_string();
    let listener = TcpListener::bind("127.0.0.1:8080").expect("bind 127.0.0.1:8080");
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let mut buf = [0u8; 2048];
        let n = s.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let body = if request.starts_with("GET /api/version") {
            format!("{{\"version\":\"{version}\"}}")
        } else {
            "{\"status\":\"ok\"}".to_string()
        };
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    }
}
