//! HTTP/1.1 client: keep-alive, chunked, gzip/deflate, conditional requests,
//! redirects, and a cookie jar - on `std` only.
//!
//! Deliberate omissions: HTTP/2 (needs HPACK + framing; the bandwidth story is
//! already carried by the cache and blocklist), and brotli/zstd (we do not offer
//! them, so servers reply with gzip). Both are documented in docs/STATUS.md.

use crate::codec::inflate;
use crate::net::cache::{entry_from_response, Cache};
use crate::net::policy::{Policy, ResourceKind};
use crate::net::stats::Stats;
use crate::net::tls::{self, Wire};
use crate::net::url::Url;
use crate::util::time::{mono_ms, parse_http_date, unix_secs, CacheControl};
use crate::util::Result;
use std::collections::HashMap;
use std::io;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub max_redirects: usize,
    pub extra_headers: Vec<(String, String)>,
    /// `http://user:pass@proxy:port` - forwarded with absolute-form requests.
    pub proxy: Option<Proxy>,
    /// Reuse at most this many sockets.
    pub pool_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            connect_timeout: Duration::from_millis(10_000),
            read_timeout: Duration::from_millis(25_000),
            max_redirects: 8,
            extra_headers: Vec::new(),
            proxy: None,
            pool_size: 12,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl Proxy {
    /// `http://[user:pass@]host:port`.
    pub fn parse(spec: &str) -> Result<Proxy> {
        let u = Url::parse(spec)?;
        if !u.has_scheme("http") && !u.has_scheme("https") {
            return Err(format!("proxy: expected http://, got {:?}", u.scheme));
        }
        Ok(Proxy {
            host: u.host.clone(),
            port: u.port.unwrap_or(8080),
            user: u.username.clone(),
            password: u.password.clone(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Conditional {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Bytes actually transferred (compressed size for encoded bodies).
    pub wire_bytes: usize,
    pub url: Url,
    pub from_cache: bool,
    /// `304` answered a conditional request; the body came from the cache.
    pub not_modified: bool,
    pub elapsed_ms: u64,
    pub compressed: bool,
    pub redirects: usize,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn content_type(&self) -> Option<String> {
        self.header("content-type").map(|s| s.to_string())
    }
    /// Decode the body as text using the declared charset.
    pub fn text(&self) -> String {
        let cs = self
            .content_type()
            .and_then(|c| crate::net::mime::Mime::parse(&c).charset)
            .unwrap_or_default();
        crate::net::decode_bytes(&self.body, if cs.is_empty() { None } else { Some(&cs) })
    }
    pub fn mime(&self) -> crate::net::mime::Mime {
        crate::net::mime::resolve(self.content_type().as_deref(), &self.url, &self.body)
    }
}

/// Cookie jar: name/value plus the two attributes that actually matter for
/// sending - domain and path scope - and expiry.
#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Absolute unix seconds; `None` = session cookie.
    pub expires: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Jar {
    pub cookies: Vec<Cookie>,
}

impl Jar {
    pub fn len(&self) -> usize {
        self.cookies.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }
    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// Apply one `Set-Cookie` header.
    pub fn store(&mut self, url: &Url, header: &str) {
        let mut parts = header.split(';');
        let nv = match parts.next() {
            Some(s) => s.trim(),
            None => return,
        };
        let (name, value) = match nv.split_once('=') {
            Some((n, v)) => (n.trim().to_string(), v.trim().to_string()),
            None => (nv.to_string(), String::new()),
        };
        if name.is_empty() {
            return;
        }
        let mut c = Cookie {
            name: name.clone(),
            value,
            domain: url.host.clone(),
            path: url
                .path
                .rsplit_once('/')
                .map(|(p, _)| if p.is_empty() { "/".to_string() } else { p.to_string() })
                .unwrap_or_else(|| "/".to_string()),
            secure: false,
            http_only: false,
            expires: None,
        };
        for p in parts {
            let p = p.trim();
            let (k, v) = match p.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
                None => (p.to_ascii_lowercase(), ""),
            };
            match k.as_str() {
                "domain" => {
                    let mut d = k.len();
                    d += 1;
                    let _ = d;
                    c.domain = v.trim_start_matches('.').to_ascii_lowercase();
                }
                "path" => {
                    if !v.is_empty() {
                        c.path = v.to_string();
                    }
                }
                "max-age" => {
                    c.expires = match v.parse::<i64>() {
                        Ok(s) if s <= 0 => Some(0),
                        Ok(s) => Some(unix_secs() as i64 + s),
                        Err(_) => None,
                    }
                    .map(|x| x as u64);
                }
                "expires" => {
                    c.expires = parse_http_date(v);
                }
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                _ => {}
            }
        }
        let now = unix_secs();
        if let Some(e) = c.expires {
            if e <= now {
                self.remove(&c.domain, &c.path, &c.name);
                return;
            }
        }
        // Replace an existing cookie with the same scope.
        let dup = self
            .cookies
            .iter()
            .position(|x| x.name == c.name && x.domain == c.domain && x.path == c.path);
        match dup {
            Some(i) => self.cookies[i] = c,
            None => self.cookies.push(c),
        }
    }

    fn remove(&mut self, domain: &str, path: &str, name: &str) {
        self.cookies
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
    }

    /// The `Cookie:` header value for a request, or None.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let now = unix_secs();
        let mut out: Vec<String> = Vec::new();
        for c in self.cookies.iter() {
            if let Some(e) = c.expires {
                if e <= now {
                    continue;
                }
            }
            if c.secure && !url.secure() {
                continue;
            }
            if !domain_matches(&url.host, &c.domain) {
                continue;
            }
            if !url.path.starts_with(&c.path) {
                continue;
            }
            out.push(format!("{}={}", c.name, c.value));
        }
        if out.is_empty() {
            None
        } else {
            Some(out.join("; "))
        }
    }

    pub fn dump(&self) -> Vec<String> {
        self.cookies
            .iter()
            .map(|c| format!("{}={} ({}{})", c.name, c.value, c.domain, c.path))
            .collect()
    }
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

pub struct Client {
    pub config: Config,
    pool: HashMap<String, Wire>,
    pub jar: Jar,
    /// Last transport error, surfaced in `kilat net` output.
    pub last_error: Option<String>,
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

impl Client {
    pub fn new() -> Client {
        Client {
            config: Config::default(),
            pool: HashMap::new(),
            jar: Jar::default(),
            last_error: None,
        }
    }

    pub fn with_config(config: Config) -> Client {
        Client {
            config,
            pool: HashMap::new(),
            jar: Jar::default(),
            last_error: None,
        }
    }

    /// Fetch with cache + policy applied. Errors are `String` describing the
    /// transport failure; HTTP status is not an error.
    pub fn fetch(
        &mut self,
        url: &Url,
        kind: ResourceKind,
        policy: &mut Policy,
        cache: &mut Cache,
        stats: &Stats,
    ) -> Result<Response> {
        self.fetch_method(url, "GET", Vec::new(), kind, policy, cache, stats)
    }

    pub fn fetch_method(
        &mut self,
        url: &Url,
        method: &str,
        body: Vec<u8>,
        kind: ResourceKind,
        policy: &mut Policy,
        cache: &mut Cache,
        stats: &Stats,
    ) -> Result<Response> {
        let started = mono_ms();
        if policy.blocks(url) {
            policy.blocked += 1;
            stats.add_blocked(estimated_size(kind, policy));
            return Err(format!("blocked by policy: {}", url));
        }
        if policy.blocks_kind(kind) {
            return Err(format!("skipped ({}) by policy", kind.as_str()));
        }
        if url.has_scheme("data") {
            let (mime, bytes) = url
                .data_bytes()
                .ok_or_else(|| "data: URL is malformed".to_string())?;
            return Ok(Response {
                status: 200,
                reason: "OK".to_string(),
                headers: vec![("content-type".to_string(), mime)],
                body: bytes,
                wire_bytes: url.path.len(),
                url: url.clone(),
                from_cache: false,
                not_modified: false,
                elapsed_ms: (mono_ms() - started) as u64,
                compressed: false,
                redirects: 0,
            });
        }
        if url.has_scheme("file") {
            let path = url
                .to_file_path()
                .ok_or_else(|| "file: URL has no path".to_string())?;
            let bytes = read_file_capped(&path, policy.cap_for(kind))?;
            let ct = crate::net::mime::from_extension(&url.extension())
                .unwrap_or("application/octet-stream")
                .to_string();
            return Ok(Response {
                status: 200,
                reason: "OK".to_string(),
                headers: vec![("content-type".to_string(), ct)],
                body: bytes,
                wire_bytes: 0,
                url: url.clone(),
                from_cache: false,
                not_modified: false,
                elapsed_ms: (mono_ms() - started) as u64,
                compressed: false,
                redirects: 0,
            });
        }
        if !url.is_http() {
            return Err(format!("unsupported scheme {:?}", url.scheme));
        }
        let key = url.origin();
        // 1. A fresh cache entry ends the request here.
        if method == "GET" {
            if let Some(e) = cache.fresh(&key_of(url)) {
                let body = e.body.clone();
                let headers = e.headers.clone();
                let status = e.status;
                let reason = e.reason.clone();
                stats.add_cached(body.len() as u64);
                return Ok(Response {
                    status,
                    reason,
                    headers,
                    body,
                    wire_bytes: 0,
                    url: url.clone(),
                    from_cache: true,
                    not_modified: false,
                    elapsed_ms: (mono_ms() - started) as u64,
                    compressed: false,
                    redirects: 0,
                });
            }
        }
        // 2. Otherwise revalidate what we still hold.
        let stored = cache.take_stale(&key_of(url));
        let cond = stored
            .as_ref()
            .filter(|e| e.can_revalidate())
            .map(|e| Conditional {
                etag: e.etag.clone(),
                last_modified: e.last_modified.clone(),
            })
            .unwrap_or_default();
        let cap = policy.cap_for(kind);
        let mut redirects = 0usize;
        let mut current = url.clone();
        let mut method = method.to_string();
        let mut body = body;
        loop {
            let req_headers = self.build_headers(&current, kind, policy, &cond, &method, &body);
            let raw = match self.exchange(&current, &method, &req_headers, &body, stats, cap) {
                Ok(v) => v,
                Err(e) => {
                    self.last_error = Some(e.clone());
                    // One retry when a pooled socket went stale mid-flight.
                    if e.starts_with("stale:") {
                        self.pool.remove(&key);
                        let again = self.exchange(&current, &method, &req_headers, &body, stats, cap);
                        match again {
                            Ok(v) => v,
                            Err(e2) => {
                                stats.add_error();
                                return Err(e2);
                            }
                        }
                    } else {
                        stats.add_error();
                        return Err(e);
                    }
                }
            };
            let (status, reason, headers, body_bytes, wire) = raw;
            // 304: keep the stored body.
            if status == 304 {
                stats.add_not_modified();
                if stored.is_some() {
                    cache.insert(stored.clone().unwrap());
                    cache.revalidate_with(&key_of(&current), status, &headers);
                    let merged = cache
                        .get(&key_of(&current))
                        .map(|m| (m.body.clone(), m.headers.clone(), m.status, m.reason.clone()));
                    if let Some((b, h, s, r)) = merged {
                        return Ok(Response {
                            status: s,
                            reason: r,
                            headers: h,
                            body: b,
                            wire_bytes: wire,
                            url: current,
                            from_cache: true,
                            not_modified: true,
                            elapsed_ms: (mono_ms() - started) as u64,
                            compressed: false,
                            redirects,
                        });
                    }
                }
                return Ok(Response {
                    status,
                    reason,
                    headers,
                    body: Vec::new(),
                    wire_bytes: wire,
                    url: current,
                    from_cache: false,
                    not_modified: true,
                    elapsed_ms: (mono_ms() - started) as u64,
                    compressed: false,
                    redirects,
                });
            }
            // Redirects.
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if let Some(loc) = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                    .map(|(_, v)| v.clone())
                {
                    if redirects >= self.config.max_redirects {
                        stats.add_error();
                        return Err(format!("too many redirects (>{})", self.config.max_redirects));
                    }
                    let next = current.join(&loc)?;
                    if status == 303 || (status == 302 && !body.is_empty()) {
                        method = "GET".to_string();
                        body = Vec::new();
                    }
                    redirects += 1;
                    current = next;
                    continue;
                }
            }
            let compressed = {
                headers
                    .iter()
                    .any(|(k, v)| k.eq_ignore_ascii_case("content-encoding") && !v.eq_ignore_ascii_case("identity"))
            };
            let decoded = decode_body(&headers, body_bytes, cap)?;
            let out_len = decoded.len();
            // Cookies first: a redirect response may already have set some.
            if policy.cookies {
                for (k, v) in headers.iter() {
                    if k.eq_ignore_ascii_case("set-cookie") {
                        self.jar.store(&current, v);
                    }
                }
            }
            let entry = entry_from_response(&key_of(&current), status, &reason, &headers, decoded.clone(), wire);
            let cc = CacheControl::parse(
                headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("cache-control"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or(""),
            );
            let cache_it = Cache::cacheable(status, &cc, headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("vary")).map(|(_, v)| v.as_str()))
                && !decoded.is_empty()
                && method == "GET";
            if cache_it {
                cache.insert(entry);
            }
            stats.add_net(wire as u64);
            if compressed {
                stats.add_decoded(out_len.saturating_sub(wire) as u64);
            }
            return Ok(Response {
                status,
                reason,
                headers,
                body: decoded,
                wire_bytes: wire,
                url: current,
                from_cache: false,
                not_modified: false,
                elapsed_ms: (mono_ms() - started) as u64,
                compressed,
                redirects,
            });
        }
    }

    fn build_headers(
        &self,
        url: &Url,
        kind: ResourceKind,
        policy: &Policy,
        cond: &Conditional,
        method: &str,
        body: &[u8],
    ) -> Vec<(String, String)> {
        let mut h: Vec<(String, String)> = Vec::new();
        let mut push = |k: &str, v: String| h.push((k.to_string(), v));
        push("Host", url.authority());
        push("User-Agent", policy.user_agent.clone());
        push("Accept", policy.accept_for(kind));
        push("Accept-Encoding", "gzip, deflate".to_string());
        push("Accept-Language", policy.accept_language.clone());
        push("Connection", "keep-alive".to_string());
        if matches!(kind, ResourceKind::Image | ResourceKind::Document | ResourceKind::Stylesheet)
            && policy.data_saver
        {
            push("Save-Data", "Yes".to_string());
        }
        if let Some(r) = policy.referrer_value(Some(url)) {
            push("Referer", r);
        }
        if let Some(e) = cond.etag.as_deref() {
            if !e.is_empty() {
                push("If-None-Match", e.to_string());
            }
        }
        if let Some(lm) = &cond.last_modified {
            push("If-Modified-Since", lm.clone());
        }
        if policy.cookies {
            if let Some(c) = self.jar.header_for(url) {
                push("Cookie", c);
            }
        }
        if !body.is_empty() {
            push("Content-Length", body.len().to_string());
            push("Content-Type", "application/x-www-form-urlencoded".to_string());
        } else if method == "POST" || method == "PUT" {
            push("Content-Length", "0".to_string());
        }
        for (k, v) in &self.config.extra_headers {
            push(k, v.clone());
        }
        for (k, v) in &policy.extra_headers {
            push(k, v.clone());
        }        h
    }

    /// One request/response exchange over a pooled or fresh connection.
    fn exchange(
        &mut self,
        url: &Url,
        method: &str,
        headers: &[(String, String)],
        body: &[u8],
        stats: &Stats,
        cap: usize,
    ) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>, usize)> {
        let key = url.origin();
        let pooled = self.pool.remove(&key);
        let mut wire = match pooled {
            Some(mut w) => {
                if w.healthy() {
                    w
                } else {
                    return Err("stale: pooled connection closed".to_string());
                }
            }
            None => tls::connect(
                &url.host,
                url.effective_port(),
                url.secure(),
                self.config.connect_timeout,
            )?,
        };
        let t0 = mono_ms();
        let target = if self.config.proxy.is_some() && !url.secure() {
            url.to_request_string()
        } else {
            url.request_target()
        };
        let mut req = format!("{method} {target} HTTP/1.1\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(p) = &self.config.proxy {
            if !p.user.is_empty() {
                let token =
                    crate::codec::base64::encode(format!("{}:{}", p.user, p.password).as_bytes());
                req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
            }
        }
        req.push_str("\r\n");
        wire.write_all(req.as_bytes()).map_err(|e| io_err("write", &e))?;
        if !body.is_empty() {
            wire.write_all(body).map_err(|e| io_err("write-body", &e))?;
        }
        wire.set_read_timeout(Some(self.config.read_timeout));
        // Status line + headers.
        let head = wire
            .read_line(8 * 1024)
            .map_err(|e| io_err("read-status", &e))?;
        if head.len() < 12 || !head[..5].eq_ignore_ascii_case(b"HTTP/") {
            return Err(format!(
                "bad status line {:?}",
                String::from_utf8_lossy(&head)
            ));
        }
        let text = String::from_utf8_lossy(&head).into_owned();
        let mut it = text.splitn(3, ' ');
        let _version = it.next().unwrap_or("");
        let status: u16 = it
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| "http: unparsable status".to_string())?;
        let reason = it.next().unwrap_or("").trim().to_string();
        let mut hdrs: Vec<(String, String)> = Vec::new();
        loop {
            let line = wire.read_line(16 * 1024).map_err(|e| io_err("read-header", &e))?;
            if line.is_empty() {
                break;
            }
            let s = String::from_utf8_lossy(&line).into_owned();
            if let Some((k, v)) = s.split_once(':') {
                let name = k.trim().to_ascii_lowercase();
                let value = v.trim().to_string();
                if hdrs.iter().any(|(ek, _)| *ek == name) {
                    // Multiple values: keep them separate (Set-Cookie needs it).
                    hdrs.push((name, value));
                } else {
                    hdrs.push((name, value));
                }
            }
        }
        let raw = self.read_body(&mut wire, &hdrs, status, method, cap)?;
        let wire_bytes = raw.len();
        stats.add_net_ms((mono_ms() - t0) as u64);
        if method != "HEAD" && reuse_connection(status, &hdrs) {
            if self.pool.len() >= self.config.pool_size {
                if let Some(k) = self.pool.keys().next().cloned() {
                    if let Some(mut w) = self.pool.remove(&k) {
                        w.close();
                    }
                }
            }
            self.pool.insert(key, wire);
        }
        Ok((status, reason, hdrs, raw, wire_bytes))
    }

    fn read_body(
        &self,
        wire: &mut Wire,
        headers: &[(String, String)],
        status: u16,
        method: &str,
        cap: usize,
    ) -> Result<Vec<u8>> {
        if method == "HEAD" || matches!(status, 101 | 204 | 304) {
            return Ok(Vec::new());
        }
        let get = |n: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, v)| v.clone())
        };
        if get("transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false)
        {
            let mut out: Vec<u8> = Vec::new();
            loop {
                let line = wire
                    .read_line(256)
                    .map_err(|e| io_err("chunk-size", &e))?;
                let size_txt = String::from_utf8_lossy(&line).into_owned();
                let size_txt = size_txt.split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(size_txt, 16)
                    .map_err(|_| format!("http: bad chunk size {size_txt:?}"))?;
                if size == 0 {
                    // Trailers, then a blank line.
                    loop {
                        let t = wire.read_line(4096).map_err(|e| io_err("trailer", &e))?;
                        if t.is_empty() {
                            break;
                        }
                    }
                    break;
                }
                if out.len() + size > cap {
                    return Err(format!("http: body exceeds {cap} byte cap"));
                }
                let chunk = wire
                    .read_exact_vec(size)
                    .map_err(|e| io_err("chunk", &e))?;
                out.extend_from_slice(&chunk);
                let _crlf = wire.read_line(4).map_err(|e| io_err("chunk-crlf", &e))?;
            }
            return Ok(out);
        }
        match get("content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
            Some(n) => {
                if n > cap {
                    return Err(format!("http: content-length {n} exceeds cap {cap}"));
                }
                let v = wire
                    .read_exact_vec(n)
                    .map_err(|e| io_err("body", &e))?;
                Ok(v)
            }
            // No length: read until the server closes (Connection: close).
            None => Ok(wire
                .read_to_end_capped(cap)
                .map_err(|e| io_err("body-eof", &e))?),
        }
    }
}

fn io_err(what: &str, e: &io::Error) -> String {
    format!("http {what}: {e}")
}

fn reuse_connection(status: u16, headers: &[(String, String)]) -> bool {
    if matches!(status, 400..=599) {
        return false;
    }
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("connection") {
            let v = v.to_ascii_lowercase();
            if v.contains("close") || v.contains("upgrade") {
                return false;
            }
            return true;
        }
    }
    true
}

fn estimated_size(kind: ResourceKind, policy: &Policy) -> u64 {
    // A blocked request's saving is the average object of that class; used only
    // for the "saved bytes" figure.
    let base = match kind {
        ResourceKind::Image => (policy.max_image_bytes / 8).clamp(2_000, 120_000),
        ResourceKind::Script => 45_000,
        ResourceKind::Stylesheet => 9_000,
        ResourceKind::Font => 18_000,
        _ => 4_000,
    };
    base as u64
}

fn key_of(url: &Url) -> String {
    // Cache key per RFC: absolute URL without the fragment.
    url.to_request_string()
}

fn read_file_capped(path: &str, cap: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("file {path}: {e}"))?;
    if meta.len() as usize > cap {
        return Err(format!("file {path}: {} exceeds cap", meta.len()));
    }
    let mut f = std::fs::File::open(path).map_err(|e| format!("file {path}: {e}"))?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    f.read_to_end(&mut buf)
        .map_err(|e| format!("file {path}: {e}"))?;
    Ok(buf)
}

/// Undo `Content-Encoding`. Brotli/zstd are never requested, so an error here
/// means the server ignored us: return the body as-is rather than failing.
pub fn decode_body(headers: &[(String, String)], mut data: Vec<u8>, cap: usize) -> Result<Vec<u8>> {
    let enc = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
        .map(|(_, v)| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if enc.is_empty() || enc == "identity" {
        return Ok(data);
    }
    for layer in enc.split(',').rev() {
        match layer.trim() {
            "gzip" | "x-gzip" => {
                data = inflate::gunzip(&data).map_err(|e| format!("gzip: {e}"))?
            }
            "deflate" => {
                data = match inflate::inflate_zlib(&data) {
                    Ok(v) => v,
                    // Many servers send raw deflate without a zlib wrapper.
                    Err(_) => inflate::inflate_raw(&data, cap.max(64 << 20))?,
                }
            }
            other => {
                crate::kdebug!("net", "unsupported content-encoding {other:?}; using body as-is");
                return Ok(data);
            }
        }
    }
    Ok(data)
}

/// Percent-decode a form body into pairs (used by the JS `fetch` bridge and
/// `kilat net --post`).
pub fn parse_form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (
                crate::net::url::percent_decode_form(k),
                crate::net::url::percent_decode_form(v),
            ),
            None => (kv.to_string(), String::new()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::deflate;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn proxy_parsing() {
        let p = Proxy::parse("http://bob:pw@127.0.0.1:3128").unwrap();
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 3128);
        assert_eq!(p.user, "bob");
        assert!(Proxy::parse("socks5://x:1").is_err());
    }

    #[test]
    fn cookie_scoping() {
        let mut j = Jar::default();
        let site = url("http://a.example.com/app/page");
        j.store(&site, "sid=1; Path=/app");
        j.store(&site, "wide=2; Domain=example.com; Path=/");
        j.store(&site, "sec=3; Secure; Path=/");
        j.store(&site, "gone=4; Max-Age=0; Path=/");
        let h = j.header_for(&url("http://a.example.com/app/x")).unwrap();
        assert!(h.contains("sid=1"), "{h}");
        assert!(h.contains("wide=2"), "{h}");
        assert!(!h.contains("sec=3"), "secure cookie on http");
        assert!(!h.contains("gone=4"), "expired cookie");
        // Path scoping.
        let elsewhere = j.header_for(&url("http://a.example.com/other")).unwrap();
        assert!(!elsewhere.contains("sid=1"), "{elsewhere}");
        // Subdomain inherits a domain-scoped cookie, not a host-only one.
        let sub = j.header_for(&url("http://b.example.com/")).unwrap();
        assert!(sub.contains("wide=2"));
        assert!(j.header_for(&url("http://other.com/")).is_none());
        assert_eq!(j.len(), 3);
    }

    #[test]
    fn chunked_and_gzip_decoding() {
        // Chunked framing is exercised end-to-end against our own test server in
        // src/net/server.rs; here we verify the decoders we call.
        let text = b"<html>hello hello hello</html>";
        let gz = deflate::compress_gzip(text, 6);
        let out = decode_body(
            &[("content-encoding".to_string(), "gzip".to_string())],
            gz.clone(),
            1 << 20,
        )
        .unwrap();
        assert_eq!(out, text.to_vec());
        assert!(gz.len() < text.len() || text.len() < 32);
        let raw = deflate::compress(text);
        let out2 = decode_body(
            &[("content-encoding".to_string(), "deflate".to_string())],
            raw,
            1 << 20,
        )
        .unwrap();
        assert_eq!(out2, text.to_vec());
        // Unknown encodings must not lose the body.
        let out3 = decode_body(
            &[("content-encoding".to_string(), "br".to_string())],
            b"xyz".to_vec(),
            1 << 20,
        )
        .unwrap();
        assert_eq!(out3, b"xyz".to_vec());
    }

    #[test]
    fn cache_key_drops_fragment() {
        assert_eq!(
            key_of(&url("http://x/y?a=1#frag")),
            "http://x/y?a=1".to_string()
        );
    }

    #[test]
    fn header_building_reflects_policy() {
        let mut policy = Policy::new();
        policy.data_saver = true;
        let c = Client::new();
        let h = c.build_headers(
            &url("http://a.dev/img.png"),
            ResourceKind::Image,
            &policy,
            &Conditional {
                etag: Some("\"v1\"".into()),
                last_modified: None,
            },
            "GET",
            b"",
        );
        let get = |n: &str| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("Accept-Encoding").as_deref(), Some("gzip, deflate"));
        assert_eq!(get("If-None-Match").as_deref(), Some("\"v1\""));
        assert_eq!(get("Save-Data").as_deref(), Some("Yes"));
        assert!(get("Accept").unwrap().contains("image/png"));
    }

    #[test]
    fn reuse_connection_respects_close() {
        assert!(!reuse_connection(200, &[("connection".into(), "close".into())]));
        assert!(reuse_connection(200, &[("content-length".into(), "3".into())]));
        assert!(!reuse_connection(503, &[]));
    }

    #[test]
    fn file_and_data_urls_shortcut_the_network() {
        let mut c = Client::new();
        let mut policy = Policy::new();
        let mut cache = Cache::new(1 << 16);
        let stats = Stats::default();
        let r = c
            .fetch(&url("data:text/plain,hi%20there"), ResourceKind::Document, &mut policy, &mut cache, &stats)
            .unwrap();
        assert_eq!(r.body, b"hi there");
        assert_eq!(r.status, 200);
        let r2 = c
            .fetch(&url("file:///nonexistent/z"), ResourceKind::Document, &mut policy, &mut cache, &stats)
            .unwrap_err();
        assert!(r2.contains("file"), "{r2}");
    }

    #[test]
    fn blocked_urls_error_with_policy_counter() {
        let mut c = Client::new();
        let mut policy = Policy::new();
        let mut cache = Cache::new(1 << 16);
        let stats = Stats::default();
        let e = c
            .fetch(
                &url("https://pagead2.googlesyndication.com/x.js"),
                ResourceKind::Script,
                &mut policy,
                &mut cache,
                &stats,
            )
            .unwrap_err();
        assert!(e.contains("blocked"), "{e}");
        assert_eq!(policy.blocked, 1);
        assert_eq!(stats.snapshot().blocked, 1);
    }

    #[test]
    fn form_pairs() {
        assert_eq!(
            parse_form("a=1&b=two+words&c="),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "two words".to_string()),
                ("c".to_string(), String::new())
            ]
        );
    }
}
