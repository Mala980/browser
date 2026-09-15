//! A tiny HTTP/1.1 server used by the network tests and `kilat dev serve`.
//!
//! CI sandboxes have no outbound internet, so the whole HTTP path - status
//! line, header folding, chunked bodies, gzip, conditional 304s, redirects and
//! keep-alive - is tested against this instead of a public site. It is also what
//! `scripts/ci-e2e.mjs` drives through CDP.

use crate::codec::deflate;
use std::io::{BufRead, Read, Write};

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Text,
    Chunked,
    Gzip,
    Redirect,
    Revalidate,
    Close,
    Big,
    Cookie,
    NotFound,
    Head,
    Unknown,
}

impl Route {
    /// Map a request target to a behaviour. Public so tests can document intent.
    pub fn of(path: &str) -> Route {
        let p = path.split('?').next().unwrap_or(path);
        match p {
            "/text" => Route::Text,
            "/chunked" => Route::Chunked,
            "/gzip" => Route::Gzip,
            "/redirect" => Route::Redirect,
            "/revalidate" => Route::Revalidate,
            "/close" => Route::Close,
            "/big" => Route::Big,
            "/cookie" => Route::Cookie,
            "/nope" => Route::NotFound,
            _ if p.ends_with(".txt") => Route::Head,
            _ => Route::Unknown,
        }
    }
}

/// What the server should answer, decided from the request. Kept separate from
/// the socket code so the framing rules are unit-testable.
pub fn respond(method: &str, path: &str, headers: &[(String, String)]) -> Vec<u8> {
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    let route = Route::of(path);
    let body_gzip = get("accept-encoding")
        .map(|v| v.contains("gzip"))
        .unwrap_or(false);
    let mut head = String::new();
    let mut body: Vec<u8> = Vec::new();
    let mut chunked = false;
    let mut close = false;
    let mut extra: Vec<String> = Vec::new();
    match route {
        Route::Text => {
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n");
            body = b"hello kilat".to_vec();
        }
        Route::Chunked => {
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\n");
            chunked = true;
            body = b"<html><body><p>chunked body works</p></body></html>".to_vec();
        }
        Route::Gzip => {
            body = b"gzip payload gzip payload gzip payload gzip payload".to_vec();
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n");
            if body_gzip {
                body = deflate::compress_gzip(&body, 6);
                head.push_str("Content-Encoding: gzip\r\n");
            }
        }
        Route::Redirect => {
            head.push_str("HTTP/1.1 302 Found\r\nLocation: /text\r\nContent-Type: text/plain\r\n");
            body = b"redirecting".to_vec();
        }
        Route::Revalidate => {
            let etag = "\"v1\"";
            if get("if-none-match").as_deref() == Some(etag) {
                head.push_str("HTTP/1.1 304 Not Modified\r\n");
                extra.push(format!("ETag: {etag}"));
                extra.push("Cache-Control: max-age=0".to_string());
                let out = format!("{head}\r\n").into_bytes();
                return out;
            }
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n");
            extra.push(format!("ETag: {etag}"));
            extra.push("Cache-Control: max-age=0".to_string());
            body = b"fresh body".to_vec();
        }
        Route::Close => {
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n");
            body = b"closing".to_vec();
            close = true;
        }
        Route::Big => {
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n");
            body = vec![7u8; 1 << 20];
        }
        Route::Cookie => {
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n");
            extra.push("Set-Cookie: visit=1; Path=/; Max-Age=3600".to_string());
            body = format!("cookies: {}", get("cookie").unwrap_or_else(|| "none".into()))
                .into_bytes();
        }
        Route::NotFound => {
            head.push_str("HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\n");
            body = b"no such thing".to_vec();
        }
        Route::Head => {
            head.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n");
            extra.push("X-Total: 3".to_string());
            body = if method == "HEAD" { Vec::new() } else { b"abc".to_vec() };
        }
        Route::Unknown => {
            head.push_str("HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n");
            body = b"unknown route".to_vec();
        }
    }
    if method == "HEAD" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    } else if !chunked && !matches!(route, Route::Revalidate) {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    if close {
        head.push_str("Connection: close\r\n");
    } else {
        head.push_str("Connection: keep-alive\r\n");
    }
    for e in extra {
        head.push_str(&e);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    if chunked {
        // Two chunks, to prove the parser handles more than one.
        let mid = body.len() / 2;
        for slice in [ &body[..mid], &body[mid..] ] {
            if slice.is_empty() {
                continue;
            }
            out.extend_from_slice(format!("{:x}\r\n", slice.len()).as_bytes());
            out.extend_from_slice(slice);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
    } else {
        out.extend_from_slice(&body);
    }
    out
}

/// Read one request: request line + headers. Returns (method, path, headers).
pub fn read_request<R: BufLine>(r: &mut R) -> Option<(String, String, Vec<(String, String)>)> {
    let line = r.line()?;
    let mut it = line.split_whitespace();
    let method = it.next()?.to_string();
    let path = it.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        match r.line() {
            None => break,
            Some(l) if l.is_empty() => break,
            Some(l) => {
                if let Some((k, v)) = l.split_once(':') {
                    headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                }
            }
        }
    }
    Some((method, path, headers))
}

pub trait BufLine {
    fn line(&mut self) -> Option<String>;
}

impl<R: Read> BufLine for std::io::BufReader<R> {
    fn line(&mut self) -> Option<String> {
        let mut s = String::new();
        match self.read_line(&mut s) {
            Ok(0) => None,
            Ok(_) => {
                while s.ends_with('\n') || s.ends_with('\r') {
                    s.pop();
                }
                Some(s)
            }
            Err(_) => None,
        }
    }
}

pub struct Server {
    pub port: u16,
    stop: Arc<AtomicBool>,
    hits: Arc<AtomicU16>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Bind an ephemeral loopback port and serve in the background.
    pub fn start() -> std::io::Result<Server> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let hits = Arc::new(AtomicU16::new(0));
        let (stop2, hits2) = (stop.clone(), hits.clone());
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                // Each accept is preceded by the shutdown check above; a wake-up
                // connection may land in flight, so ignore it.
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                match conn {
                    Ok(sock) => {
                        let hits = hits2.clone();
                        std::thread::spawn(move || {
                            let _ = handle_conn(sock, &hits);
                        });
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Server {
            port,
            stop,
            hits,
            thread: Some(thread),
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn hits(&self) -> u16 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Shut the accept loop down. A throwaway connection is what wakes
    /// `incoming()`, which otherwise blocks until the process exits.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn handle_conn(mut sock: TcpStream, hits: &AtomicU16) -> std::io::Result<()> {
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut reader = std::io::BufReader::new(sock.try_clone()?);
    loop {
        let req = match read_request(&mut reader) {
            Some(r) => r,
            None => return Ok(()),
        };
        hits.fetch_add(1, Ordering::Relaxed);
        let (method, path, headers) = req;
        let bytes = respond(&method, &path, &headers);
        sock.write_all(&bytes)?;
        sock.flush()?;
        let keep = headers
            .iter()
            .find(|(k, _)| k == "connection")
            .map(|(_, v)| !v.eq_ignore_ascii_case("close"))
            .unwrap_or(true);
        if !keep || path.ends_with("/close") {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_and_framing() {
        let r = respond("GET", "/text", &[]);
        let text = String::from_utf8_lossy(&r).into_owned();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Content-Length: 11"));
        let c = String::from_utf8_lossy(&respond("GET", "/chunked", &[])).into_owned();
        assert!(c.contains("Transfer-Encoding: chunked"));
        assert!(c.ends_with("0\r\n\r\n"), "{c}");
        assert!(!c.contains("Content-Length"));
        let g = respond("GET", "/gzip", &[("accept-encoding".into(), "gzip".into())]);
        assert!(String::from_utf8_lossy(&g).contains("Content-Encoding: gzip"));
        let plain = respond("GET", "/gzip", &[]);
        assert!(!String::from_utf8_lossy(&plain).contains("Content-Encoding"));
        let re = String::from_utf8_lossy(&respond(
            "GET",
            "/revalidate",
            &[("if-none-match".into(), "\"v1\"".into())],
        ))
        .into_owned();
        assert!(re.starts_with("HTTP/1.1 304"), "{re}");
        assert!(!re.contains("Content-Length"), "304 must have no body");
        let h = String::from_utf8_lossy(&respond("HEAD", "/x.txt", &[])).into_owned();
        assert!(h.contains("Content-Length: 3"));
        assert!(!h.ends_with("abc"));
    }

    #[test]
    fn request_line_parsing() {
        let raw = b"GET /a HTTP/1.1\r\nHost: x\r\nAccept: */*\r\n\r\n";
        let mut br = std::io::BufReader::new(&raw[..]);
        let (m, p, h) = read_request(&mut br).unwrap();
        assert_eq!(m, "GET");
        assert_eq!(p, "/a");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].0, "host");
    }

    #[test]
    fn serves_real_sockets() {
        let mut srv = Server::start().expect("bind");
        let mut sock = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
        sock.write_all(
            b"GET /text HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        let mut buf = Vec::new();
        std::io::BufReader::new(&mut sock).read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf).into_owned();
        assert!(text.contains("hello kilat"), "{text}");
        assert_eq!(srv.hits(), 1);
        srv.stop();
    }
}
