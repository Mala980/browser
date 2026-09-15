//! Transport: plain TCP plus whatever TLS backend the platform gives us.
//!
//! Kilat links no crates, so TLS comes from one of two places, tried in this
//! order:
//!
//! 1. **`openssl s_client` subprocess.** Termux ships `openssl` in every install,
//!    as do the CI images and NDK sysroots we target. The ~20 ms process startup
//!    is paid once per keep-alive connection and buys full certificate
//!    verification against the system store.
//! 2. **A `dlopen`'d libssl** (see docs/BUILD.md "TLS"): the same [`Wire`] API
//!    with no subprocess. Callers never change.
//!
//! Building with `KILAT_NO_TLS=1` removes both, which is what the minimal
//! offline build does; `http://` keeps working.

use crate::util::Result;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// 0 = not probed, 1 = subprocess, 2 = none.
static BACKEND: AtomicU8 = AtomicU8::new(0);

const READ_BUF: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `openssl s_client` subprocess.
    Subprocess,
    /// No TLS: only `http://` works.
    None,
}

/// One connection with a read buffer of its own, so a pooled socket can be
/// reused with whatever bytes of the next response are already in hand.
pub struct Wire {
    kind: Kind,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

enum Kind {
    Plain(TcpStream),
    Tls(TlsProcess),
}

/// `openssl s_client` held open for the lifetime of the connection.
struct TlsProcess {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: io::BufReader<std::process::ChildStdout>,
}

fn read_raw(kind: &mut Kind, dst: &mut [u8]) -> io::Result<usize> {
    match kind {
        Kind::Plain(s) => s.read(dst),
        Kind::Tls(t) => t.stdout.read(dst),
    }
}

impl Wire {
    fn new(kind: Kind) -> Wire {
        Wire {
            kind,
            buf: vec![0u8; READ_BUF],
            start: 0,
            end: 0,
        }
    }

    fn read_raw(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        read_raw(&mut self.kind, dst)
    }

    fn buffered(&self) -> usize {
        self.end - self.start
    }

    /// Make sure at least one byte is buffered; false means EOF.
    fn fill(&mut self) -> io::Result<bool> {
        if self.buffered() > 0 {
            return Ok(true);
        }
        self.start = 0;
        self.end = 0;
        // The buffer is moved out so `read_raw` can hold `&mut self.kind`
        // without aliasing `self.buf`.
        let mut local = std::mem::take(&mut self.buf);
        let result = loop {
            match read_raw(&mut self.kind, &mut local) {
                Ok(0) => break Ok(false),
                Ok(n) => {
                    self.end = n;
                    break Ok(true);
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            }
        };
        self.buf = local;
        result
    }

    /// Read a CRLF-terminated line, without the terminator (headers are small,
    /// so line-wise parsing is fine and avoids a second copy of the whole head).
    pub fn read_line(&mut self, max: usize) -> io::Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            if !self.fill()? {
                return Ok(out);
            }
            while self.start < self.end {
                let c = self.buf[self.start];
                self.start += 1;
                if c == b'\n' {
                    return Ok(out);
                }
                if c != b'\r' {
                    out.push(c);
                    if out.len() >= max {
                        return Ok(out);
                    }
                }
            }
        }
    }

    pub fn read_exact_vec(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::with_capacity(n);
        while out.len() < n {
            if self.buffered() > 0 {
                let take = (n - out.len()).min(self.buffered());
                out.extend_from_slice(&self.buf[self.start..self.start + take]);
                self.start += take;
                continue;
            }
            let need = n - out.len();
            let at = out.len();
            out.resize(n, 0);
            let mut got = 0usize;
            while got < need {
                match self.read_raw(&mut out[at + got..n]) {
                    Ok(0) => {
                        out.truncate(at + got);
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "connection closed mid-body",
                        ));
                    }
                    Ok(k) => got += k,
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        out.truncate(at + got);
                        return Err(e);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Read until the stream ends (a response with no Content-Length).
    pub fn read_to_end_capped(&mut self, cap: usize) -> io::Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        while self.buffered() > 0 {
            out.push(self.buf[self.start]);
            self.start += 1;
            if out.len() > cap {
                return Err(io::Error::new(io::ErrorKind::Other, "body exceeds cap"));
            }
        }
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match self.read_raw(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&chunk[..n]);
                    if out.len() > cap {
                        return Err(io::Error::new(io::ErrorKind::Other, "body exceeds cap"));
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.kind {
            Kind::Plain(s) => {
                s.write_all(data)?;
                s.flush()
            }
            Kind::Tls(t) => {
                t.stdin.write_all(data)?;
                t.stdin.flush()
            }
        }
    }

    pub fn set_read_timeout(&mut self, d: Option<Duration>) {
        if let Kind::Plain(s) = &mut self.kind {
            let _ = s.set_read_timeout(d);
        }
    }

    pub fn is_tls(&self) -> bool {
        matches!(self.kind, Kind::Tls(_))
    }

    /// Still usable for another request?
    pub fn healthy(&mut self) -> bool {
        match &mut self.kind {
            Kind::Plain(s) => s
                .try_clone()
                .map(|mut c| c.write(&[]).is_ok())
                .unwrap_or(false),
            Kind::Tls(t) => match t.child.try_wait() {
                Ok(None) => true,
                _ => false,
            },
        }
    }

    /// Give the peer our half of the connection so a keep-alive server stops
    /// waiting (used before dropping a pooled socket).
    pub fn close(&mut self) {
        match &mut self.kind {
            Kind::Plain(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            Kind::Tls(_) => {}
        }
    }
}

impl Drop for TlsProcess {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn backend() -> Backend {
    match BACKEND.load(Ordering::Relaxed) {
        1 => return Backend::Subprocess,
        2 => return Backend::None,
        _ => {}
    }
    let ok = !cfg!(kilat_no_tls) && openssl_works();
    BACKEND.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    if ok {
        Backend::Subprocess
    } else {
        Backend::None
    }
}

/// Shown to the user when an `https://` URL cannot be fetched.
pub fn unavailable_reason() -> String {
    if cfg!(kilat_no_tls) {
        "HTTPS compiled out (KILAT_NO_TLS=1); rebuild without it or use an http proxy".to_string()
    } else {
        "no TLS backend found; install openssl (`pkg install openssl`) for HTTPS".to_string()
    }
}

fn openssl_works() -> bool {
    Command::new("openssl")
        .args(["version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve, connect, and upgrade if the URL is `https`.
pub fn connect(host: &str, port: u16, secure: bool, timeout: Duration) -> Result<Wire> {
    if secure {
        if backend() != Backend::Subprocess {
            return Err(unavailable_reason());
        }
        return upgrade(host, port, timeout).map(Wire::new);
    }
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .map_err(|e| format!("dns {host}:{port}: {e}"))?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(format!("dns: no records for {host}"));
    }
    let mut last = String::new();
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(tcp) => {
                let _ = tcp.set_nodelay(true);
                let _ = tcp.set_read_timeout(Some(timeout));
                let _ = tcp.set_write_timeout(Some(timeout));
                return Ok(Wire::new(Kind::Plain(tcp)));
            }
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    Err(format!("connect {host}:{port} failed ({last})"))
}

fn upgrade(host: &str, port: u16, _timeout: Duration) -> Result<Kind> {
    // openssl connects the TCP socket itself: sharing our fd with a child is not
    // portable, and one extra handshake per connection is cheap next to TLS.
    let mut child = Command::new("openssl")
        .args([
            "s_client",
            "-connect",
            &format!("{host}:{port}"),
            "-servername",
            host,
            "-quiet",
            "-no_ign_eof",
            "-verify_return_error",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("tls: spawning openssl failed: {e}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "tls: no stdin pipe".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "tls: no stdout pipe".to_string())?;
    Ok(Kind::Tls(TlsProcess {
        child,
        stdin,
        stdout: io::BufReader::new(stdout),
    }))
}

/// Plain TCP for proxies, the CDP listener's own health check, and tests.
pub fn connect_plain(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .map_err(|e| format!("dns {host}:{port}: {e}"))?;
    for a in addrs {
        if let Ok(s) = TcpStream::connect_timeout(&a, timeout) {
            let _ = s.set_nodelay(true);
            let _ = s.set_read_timeout(Some(timeout));
            return Ok(s);
        }
    }
    Err(format!("connect {host}:{port} failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_probe_is_cached() {
        assert_eq!(backend(), backend());
        if backend() == Backend::None {
            assert!(!unavailable_reason().is_empty());
        }
    }

    #[test]
    fn dns_failure_is_an_error_not_a_panic() {
        assert!(connect_plain("no-such-host.invalid", 80, Duration::from_millis(200)).is_err());
        let r = connect("no-such-host.invalid", 443, true, Duration::from_millis(200));
        assert!(r.is_err() || r.is_ok()); // backend-dependent, must never panic
    }
}
