//! Networking: URLs, MIME, the HTTP cache, request policy, and the client.
//!
//! Everything here is `std` only - no async runtime, no crates. Blocking I/O on
//! worker threads is plenty for a browser that wants to be light, and it keeps
//! the Termux build trivial (`cargo build --offline` with zero downloads).

pub mod cache;
pub mod http;
pub mod mime;
pub mod policy;
pub mod stats;
pub mod testserver;
pub mod tls;
pub mod url;

pub use cache::{Cache, Entry as CacheEntry};
pub use http::{Client, Config as HttpConfig, Cookie, Jar as CookieJar, Response};
pub use mime::Mime;
pub use policy::{Policy, ResourceKind};
pub use stats::{Snapshot as StatsSnapshot, Stats};
pub use url::{percent_decode, percent_encode, Url};

use crate::util::Result;

/// Fetch a URL as text with the default policy - what `kilat fetch` does.
pub fn read_text(url_str: &str) -> Result<(Response, String)> {
    let url = Url::parse(url_str)?;
    let mut policy = Policy::new();
    let mut cache = Cache::new(32 << 20);
    let stats = Stats::default();
    let mut client = Client::default();
    let resp = client.fetch(&url, ResourceKind::Document, &mut policy, &mut cache, &stats)?;
    let text = resp.text();
    Ok((resp, text))
}

/// Byte-level charset decoding for document bodies (see `html` module docs).
pub fn decode_bytes(bytes: &[u8], declared: Option<&str>) -> String {
    crate::html::decode_bytes(bytes, declared)
}

/// Absolute-URI resolution against a document URL, for `<a href>`/`<img src>`.
pub fn resolve(base: Option<&Url>, href: &str) -> Result<Url> {
    let raw = href.trim();
    if raw.is_empty() {
        return match base {
            Some(b) => Ok(b.clone()),
            None => Err("empty url".to_string()),
        };
    }
    Url::parse_with_base(base, raw)
}

/// Strip `#frag`, drop `..` segments: normalising a URL before using it as a key.
pub fn canonicalise(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => u.to_request_string(),
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalise_strips_fragment() {
        assert_eq!(
            canonicalise("http://a.dev/p?x=1#frag"),
            "http://a.dev/p?x=1".to_string()
        );
        assert_eq!(canonicalise("not a url"), "not a url".to_string());
    }

    #[test]
    fn resolve_with_and_without_base() {
        let base = Url::parse("http://a.dev/dir/i.html").unwrap();
        assert_eq!(
            resolve(Some(&base), "x.css").unwrap().path,
            "/dir/x.css".to_string()
        );
        assert_eq!(resolve(Some(&base), "").unwrap().path, "/dir/i.html".to_string());
        assert!(resolve(None, "x.css").is_err());
        assert_eq!(
            resolve(Some(&base), "https://other.dev/z").unwrap().host,
            "other.dev".to_string()
        );
    }

    /// End-to-end against our own server: framing, gzip, chunked, redirects,
    /// conditional revalidation and keep-alive, all without touching the network.
    #[test]
    fn client_round_trip_against_testserver() {
        let srv = testserver::Server::start().expect("start");
        let base = format!("http://127.0.0.1:{}", srv.port);
        let mut client = Client::default();
        client.config.connect_timeout = std::time::Duration::from_secs(2);
        client.config.read_timeout = std::time::Duration::from_secs(5);
        let mut policy = Policy::new();
        policy.subresources = true;
        let mut cache = Cache::new(4 << 20);
        let stats = Stats::default();

        let plain = client
            .fetch(
                &Url::parse(&format!("{base}/text")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("fetch /text");
        assert_eq!(plain.status, 200);
        assert_eq!(plain.body, b"hello kilat".to_vec());

        let chunked = client
            .fetch(
                &Url::parse(&format!("{base}/chunked")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("fetch /chunked");
        assert!(String::from_utf8_lossy(&chunked.body).contains("chunked body works"));

        let gz = client
            .fetch(
                &Url::parse(&format!("{base}/gzip")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("fetch /gzip");
        assert!(gz.compressed, "gzip must be detected");
        assert_eq!(gz.body, b"gzip payload gzip payload gzip payload gzip payload".to_vec());

        let red = client
            .fetch(
                &Url::parse(&format!("{base}/redirect")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("redirect");
        assert_eq!(red.status, 200, "redirects are followed");
        assert_eq!(red.redirects, 1);
        assert_eq!(red.url.path, "/text".to_string());

        // Conditional request: first fills the cache, second revalidates to 304.
        macro_rules! rv {
            () => {
                client
                    .fetch(
                        &Url::parse(&format!("{base}/revalidate")).unwrap(),
                        ResourceKind::Document,
                        &mut policy,
                        &mut cache,
                        &stats,
                    )
                    .expect("revalidate")
            };
        }
        let a = rv!();
        assert_eq!(a.body, b"fresh body".to_vec());
        // Force it stale but validatable by rewinding its date.
        {
            let e = cache
                .get_mut(&format!("127.0.0.1:{}/revalidate", srv.port))
                .or_else(|| cache.get_mut(&format!("http://127.0.0.1:{}/revalidate", srv.port)))
                .expect("stored");
            e.date = crate::util::time::unix_secs() - 3600;
        }
        let b = rv!();
        assert_eq!(b.status, 200);
        assert_eq!(b.body, b"fresh body".to_vec());
        assert!(
            b.not_modified || b.from_cache,
            "must not refetch the body: {:?}",
            (b.not_modified, b.from_cache)
        );

        let cookie = client
            .fetch(
                &Url::parse(&format!("{base}/cookie")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("cookie set");
        assert!(!client.jar.is_empty(), "Set-Cookie stored");
        let again = client
            .fetch(
                &Url::parse(&format!("{base}/cookie")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("cookie sent");
        assert!(
            String::from_utf8_lossy(&again.body).contains("visit=1"),
            "{:?}",
            String::from_utf8_lossy(&again.body)
        );
        let _ = cookie;

        let missing = client
            .fetch(
                &Url::parse(&format!("{base}/nope")).unwrap(),
                ResourceKind::Document,
                &mut policy,
                &mut cache,
                &stats,
            )
            .expect("404 is not an error");
        assert_eq!(missing.status, 404);

        let snap = stats.snapshot();
        assert!(snap.requests >= 6, "{snap:?}");
        assert!(snap.net_bytes > 0);
        assert_eq!(snap.not_modified, 1, "one conditional hit");
        drop(srv);
    }
}
