//! URL parsing and resolution (a WHATWG-shaped subset).
//!
//! Enough of the URL standard to be correct for real links: scheme-relative and
//! dot-segment resolution against a base, percent-encoding of the characters
//! browsers escape, `file:`/`data:` handling, origins for the same-origin check,
//! and query accessors. IDNA is intentionally absent: non-ASCII hosts are passed
//! through as UTF-8 and rely on the resolver, which every Android/Termux system
//! handles.

use crate::util::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub username: String,
    pub password: String,
    /// Lowercase, no brackets. Empty for `data:`/`about:` URLs.
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
    /// `true` when the URL had no authority component (`data:text/plain,x`).
    pub opaque: bool,
}

impl Url {
    pub fn parse(raw: &str) -> Result<Url> {
        Self::parse_inner(raw.trim(), None)
    }

    pub fn parse_with_base(base: Option<&Url>, raw: &str) -> Result<Url> {
        Self::parse_inner(raw.trim(), base)
    }

    fn parse_inner(raw: &str, base: Option<&Url>) -> Result<Url> {
        let text = strip_control(raw);
        if text.is_empty() {
            return match base {
                Some(b) => Ok(b.clone()),
                None => Err("url: empty".to_string()),
            };
        }
        // Fragment first: it never affects resolution.
        let (rest, fragment) = match text.find('#') {
            Some(k) => (&text[..k], Some(text[k + 1..].to_string())),
            None => (text.as_str(), None),
        };
        // Explicit scheme?
        let scheme_opt = find_scheme(rest);
        let (scheme, after_scheme) = match &scheme_opt {
            Some((s, idx)) => (s.clone(), &rest[idx + 1..]),
            None => {
                let base = base.ok_or_else(|| format!("url: no base for {raw:?}"))?;
                let (path, query) = split_query(rest);
                if rest.starts_with("//") {
                    // Scheme-relative: inherit the base scheme.
                    return Self::parse_inner(&format!("{}:{rest}", base.scheme), None).map(
                        |mut u| {
                            u.fragment = fragment;
                            u
                        },
                    );
                }
                let mut u = base.clone();
                u.fragment = fragment;
                if !path.is_empty() {
                    u.path = if path.starts_with('/') {
                        normalize_path(path)
                    } else {
                        let mut p = u.rdir();
                        p.push_str(path);
                        normalize_path(&p)
                    };
                    u.query = query.map(|q| q.to_string());
                } else if let Some(q) = query {
                    u.query = Some(q.to_string());
                }
                return Ok(u);
            }
        };
        let scheme_l = scheme.to_ascii_lowercase();
        let body = after_scheme;
        if scheme_l == "data" {
            return Ok(Url {
                scheme: scheme_l,
                username: String::new(),
                password: String::new(),
                host: String::new(),
                port: None,
                path: body.to_string(),
                query: None,
                fragment,
                opaque: true,
            });
        }
        if scheme_l == "about" || scheme_l == "javascript" || scheme_l == "blob" {
            return Ok(Url {
                scheme: scheme_l,
                username: String::new(),
                password: String::new(),
                host: String::new(),
                port: None,
                path: body.to_string(),
                query: None,
                fragment,
                opaque: true,
            });
        }
        if scheme_l == "file" {
            let (path, query) = split_query(body);
            let path = if path.starts_with("//") {
                // file://host/path - we only support the local form.
                let after = &path[2..];
                let slash = after.find('/').map(|k| &after[k..]).unwrap_or("/");
                slash.to_string()
            } else if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{path}")
            };
            return Ok(Url {
                scheme: scheme_l,
                username: String::new(),
                password: String::new(),
                host: String::new(),
                port: None,
                path: normalize_path(&path),
                query: query.map(|q| q.to_string()),
                fragment,
                opaque: false,
            });
        }
        // Hierarchical: optional `//authority`.
        let (authority, rest2) = if let Some(r) = body.strip_prefix("//") {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        } else {
            // `mailto:x@y` style: keep as opaque path.
            return Ok(Url {
                scheme: scheme_l,
                username: String::new(),
                password: String::new(),
                host: String::new(),
                port: None,
                path: body.to_string(),
                query: None,
                fragment,
                opaque: true,
            });
        };
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(k) => {
                let (u, h) = authority.split_at(k);
                let mut parts = u.splitn(2, ':');
                let name = parts.next().unwrap_or("").to_string();
                let pass = parts.next().unwrap_or("").to_string();
                (Some((name, pass)), &h[1..])
            }
            None => (None, authority),
        };
        let (host, port) = split_host_port(hostport)?;
        if host.is_empty() {
            return Err(format!("url: missing host in {raw:?}"));
        }
        let (path, query) = split_query(rest2);
        let path = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('/') {
            normalize_path(path)
        } else {
            format!("/{}", normalize_path(path))
        };
        let port = match port {
            // Drop the default port: it changes the URL string browsers send.
            Some(p) if Some(p) == default_port(&scheme_l) => None,
            other => other,
        };
        let (username, password) = match userinfo {
            Some((u, p)) => (u, p),
            None => (String::new(), String::new()),
        };
        Ok(Url {
            scheme: scheme_l,
            username,
            password,
            host,
            port,
            path,
            query: query.map(|q| q.to_string()),
            fragment,
            opaque: false,
        })
    }

    fn rdir(&self) -> String {
        match self.path.rfind('/') {
            Some(k) => self.path[..=k].to_string(),
            None => "/".to_string(),
        }
    }

    pub fn join(&self, rel: &str) -> Result<Url> {
        Url::parse_with_base(Some(self), rel)
    }

    pub fn has_scheme(&self, s: &str) -> bool {
        self.scheme.eq_ignore_ascii_case(s)
    }

    pub fn is_http(&self) -> bool {
        self.has_scheme("http") || self.has_scheme("https")
    }

    pub fn secure(&self) -> bool {
        self.has_scheme("https") || self.port == Some(443)
    }

    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or_else(|| default_port(&self.scheme).unwrap_or(80))
    }

    /// `scheme://host:port` - the cache and cookie keys.
    pub fn origin(&self) -> String {
        if self.opaque || self.host.is_empty() {
            return format!("{}:", self.scheme);
        }
        let p = match self.port {
            Some(p) => format!(":{p}"),
            None => String::new(),
        };
        format!("{}://{}{}", self.scheme, self.host, p)
    }

    pub fn is_same_origin(&self, other: &Url) -> bool {
        self.origin() == other.origin()
    }

    /// Everything a request line needs: `host[:port]`.
    pub fn authority(&self) -> String {
        match self.port {
            Some(p) if Some(p) != default_port(&self.scheme) => {
                format!("{}:{}", self.host, p)
            }
            _ => self.host.clone(),
        }
    }

    /// `/path?query`, the request target.
    pub fn request_target(&self) -> String {
        let p = if self.path.is_empty() { "/" } else { &self.path };
        match &self.query {
            Some(q) => format!("{p}?{q}"),
            None => p.to_string(),
        }
    }

    /// Re-serialise without the fragment (what goes on the wire).
    pub fn to_request_string(&self) -> String {
        let mut s = format!("{}://", self.scheme);
        if !self.opaque {
            if !self.username.is_empty() {
                s.push_str(&self.username);
                if !self.password.is_empty() {
                    s.push(':');
                    s.push_str(&self.password);
                }
                s.push('@');
            }
            s.push_str(&self.authority());
        }
        s.push_str(&self.path);
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }

    pub fn basename(&self) -> String {
        self.path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.host)
            .to_string()
    }

    pub fn extension(&self) -> String {
        let b = self.basename();
        match b.rfind('.') {
            Some(k) if k + 1 < b.len() => b[k + 1..].to_ascii_lowercase(),
            _ => String::new(),
        }
    }

    pub fn query_pairs(&self) -> Vec<(String, String)> {
        match &self.query {
            None => Vec::new(),
            Some(q) => q
                .split('&')
                .filter(|s| !s.is_empty())
                .map(|kv| match kv.split_once('=') {
                    Some((k, v)) => (percent_decode(k), percent_decode(v)),
                    None => (percent_decode(kv), String::new()),
                })
                .collect(),
        }
    }

    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query_pairs()
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }

    pub fn with_query(mut self, query: Option<String>) -> Url {
        self.query = query;
        self
    }

    /// `data:` URLs decode their payload (base64 or percent-encoded).
    pub fn data_bytes(&self) -> Option<(String, Vec<u8>)> {
        if !self.has_scheme("data") {
            return None;
        }
        let (meta, payload) = match self.path.split_once(',') {
            Some(x) => x,
            None => return Some(("text/plain".to_string(), Vec::new())),
        };
        let is_b64 = meta.rsplit(';').any(|p| p.eq_ignore_ascii_case("base64"));
        let mime = meta
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("text/plain")
            .to_ascii_lowercase();
        let bytes = if is_b64 {
            crate::codec::base64::decode(payload).ok()?
        } else {
            percent_decode(payload).into_bytes()
        };
        Some((mime, bytes))
    }

    /// Local file path for a `file:` URL.
    pub fn to_file_path(&self) -> Option<String> {
        if !self.has_scheme("file") {
            return None;
        }
        Some(percent_decode(&self.path))
    }

    pub fn from_file_path(path: &str) -> Url {
        Url {
            scheme: "file".to_string(),
            username: String::new(),
            password: String::new(),
            host: String::new(),
            port: None,
            path: if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{path}")
            },
            query: None,
            fragment: None,
            opaque: false,
        }
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.opaque {
            write!(f, "{}:{}", self.scheme, self.path)?;
        } else {
            write!(f, "{}://{}", self.scheme, self.authority())?;
            write!(f, "{}", if self.path.is_empty() { "/" } else { &self.path })?;
            if let Some(q) = &self.query {
                write!(f, "?{q}")?;
            }
        }
        if let Some(fr) = &self.fragment {
            write!(f, "#{fr}")?;
        }
        Ok(())
    }
}

fn split_query(s: &str) -> (&str, Option<&str>) {
    match s.find('?') {
        Some(k) => (&s[..k], Some(&s[k + 1..])),
        None => (s, None),
    }
}

fn find_scheme(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic()) {
        return None;
    }
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.' => i += 1,
            b':' if i > 0 => {
                let scheme = s[..i].to_ascii_lowercase();
                // A single letter followed by ':' looks like a Windows drive
                // letter in a base-relative path; browsers require 2+ chars.
                if scheme.len() < 2 {
                    return None;
                }
                return Some((scheme, i));
            }
            _ => return None,
        }
    }
    None
}

fn split_host_port(h: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = h.strip_prefix('[') {
        // IPv6 literal.
        let end = rest.find(']').ok_or_else(|| "url: bad IPv6 literal".to_string())?;
        let host = rest[..end].to_ascii_lowercase();
        let tail = &rest[end + 1..];
        let port = tail
            .strip_prefix(':')
            .and_then(|p| if p.is_empty() { None } else { Some(p) })
            .map(|p| p.parse::<u16>().map_err(|_| format!("url: bad port {p:?}")))
            .transpose()?;
        return Ok((format!("[{host}]"), port));
    }
    match h.rfind(':') {
        Some(k) if !h[..k].contains(':') => {
            let port_str = &h[k + 1..];
            if port_str.is_empty() {
                Ok((h[..k].to_ascii_lowercase(), None))
            } else {
                let p: u16 = port_str
                    .parse()
                    .map_err(|_| format!("url: bad port {port_str:?}"))?;
                Ok((h[..k].to_ascii_lowercase(), Some(p)))
            }
        }
        _ => Ok((h.to_ascii_lowercase(), None)),
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn normalize_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let trailing_slash = p.ends_with('/') || p.ends_with("/.") || p.ends_with("/..");
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut s = String::with_capacity(p.len());
    s.push('/');
    s.push_str(&out.join("/"));
    if trailing_slash && !s.ends_with('/') {
        s.push('/');
    }
    s
}

fn strip_control(s: &str) -> String {
    if s.chars().all(|c| c >= ' ') {
        return s.to_string();
    }
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Percent-decode, treating `+` literally (paths) - query parsers opt in to the
/// `+`-as-space rule explicitly.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn percent_decode_form(s: &str) -> String {
    percent_decode(&s.replace('+', "%20"))
}

/// Escape the characters that would break a URL, keeping sub-delims as-is.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':'
            | b'@' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';'
            | b'=' | b'%' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Encode a query-component value (space becomes `%20`, `+` is literal).
pub fn encode_query_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn absolute_urls() {
        let a = u("HTTPS://Example.COM:443/a/b?x=1#f");
        assert_eq!(a.scheme, "https");
        assert_eq!(a.host, "example.com");
        assert_eq!(a.port, None, "default port must be dropped");
        assert_eq!(a.path, "/a/b");
        assert_eq!(a.query.as_deref(), Some("x=1"));
        assert_eq!(a.fragment.as_deref(), Some("f"));
        assert_eq!(a.authority(), "example.com");
        assert_eq!(a.request_target(), "/a/b?x=1");
        assert_eq!(a.to_string(), "https://example.com/a/b?x=1#f");

        let b = u("http://example.com:8080/");
        assert_eq!(b.effective_port(), 8080);
        assert_eq!(b.authority(), "example.com:8080");
        assert_eq!(b.origin(), "http://example.com:8080");
    }

    #[test]
    fn empty_path_becomes_slash() {
        assert_eq!(u("http://a.dev").path, "/");
        assert_eq!(u("http://a.dev?x").request_target(), "/?x");
    }

    #[test]
    fn relative_resolution() {
        let base = u("http://example.com/dir/page.html?q=1#x");
        let cases = [
            ("other.css", "http://example.com/dir/other.css"),
            ("/root.png", "http://example.com/root.png"),
            ("../up.html", "http://example.com/up.html"),
            ("./sib/../../x", "http://example.com/x"),
            ("?only=query", "http://example.com/dir/page.html?only=query"),
            ("#frag", "http://example.com/dir/page.html?q=1#frag"),
            ("//cdn.example.com/x.js", "http://cdn.example.com/x.js"),
            ("http://other.org/z", "http://other.org/z"),
            ("sub/", "http://example.com/dir/sub/"),
        ];
        for (rel, want) in cases {
            let got = base.join(rel).unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert_eq!(got.to_string(), want, "resolving {rel}");
        }
    }

    #[test]
    fn query_and_extensions() {
        let a = u("http://x.dev/p.htm?a=1&b=two%20words&a=2");
        assert_eq!(a.query_param("b").as_deref(), Some("two words"));
        assert_eq!(a.query_pairs().len(), 3);
        assert_eq!(a.extension(), "htm");
        assert_eq!(a.basename(), "p.htm");
        assert_eq!(u("http://x.dev/dir/").extension(), "");
    }

    #[test]
    fn data_urls() {
        let d = u("data:text/html,<b>hi</b>");
        assert_eq!(d.scheme, "data");
        let (mime, bytes) = d.data_bytes().unwrap();
        assert_eq!(mime, "text/html");
        assert_eq!(String::from_utf8_lossy(&bytes), "<b>hi</b>");
        let b64 = u("data:image/png;base64,iVBORw0KGgo=");
        let (mime2, bytes2) = b64.data_bytes().unwrap();
        assert_eq!(mime2, "image/png");
        assert_eq!(&bytes2[..4], b"\x89PNG");
    }

    #[test]
    fn file_urls() {
        let f = u("file:///home/user/index.html");
        assert_eq!(f.to_file_path().as_deref(), Some("/home/user/index.html"));
        let enc = u("file:///tmp/a%20b/c.html");
        assert_eq!(enc.to_file_path().as_deref(), Some("/tmp/a b/c.html"));
        assert_eq!(Url::from_file_path("/x/y").to_string(), "file:///x/y");
    }

    #[test]
    fn percent_and_control_chars() {
        assert_eq!(percent_decode("a%20b%e9"), "a b\u{e9}");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(encode_query_component("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(u("http://a.dev/\u{1}x"), u("http://a.dev/x"));
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn ipv6_and_userinfo() {
        let a = u("http://[::1]:8080/x");
        assert_eq!(a.host, "[::1]");
        assert_eq!(a.port, Some(8080));
        assert_eq!(a.authority(), "[::1]:8080");
        let b = u("http://user:pass@host.dev/p");
        assert_eq!(b.username, "user");
        assert_eq!(b.password, "pass");
        assert_eq!(b.host, "host.dev");
    }

    #[test]
    fn same_origin_and_flags() {
        assert!(u("http://a.dev/x").is_same_origin(&u("http://a.dev/y")));
        assert!(!u("http://a.dev/x").is_same_origin(&u("https://a.dev/x")));
        assert!(!u("http://a.dev/x").is_same_origin(&u("http://b.dev/x")));
        assert!(u("http://a.dev").is_http());
        assert!(!u("data:text/plain,x").is_http());
        assert!(u("https://a.dev").secure());
    }

    #[test]
    fn bad_input_is_rejected_not_panicking() {
        assert!(Url::parse("://x").is_err());
        assert!(Url::parse("http://:80/").is_err());
        assert!(Url::parse("http://a.dev:99999/").is_err());
        assert!(Url::parse_with_base(None, "relative/path").is_err());
        assert!(Url::parse("").is_err());
    }
}
