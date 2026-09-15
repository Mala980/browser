//! HTTP cache: freshness, revalidation, and an LRU byte budget.
//!
//! This is the single biggest bandwidth lever in the browser: a second visit
//! costs 304s instead of full bodies. Policy follows RFC 9111 with two
//! pragmatic additions - heuristic freshness for `Last-Modified` without
//! `Cache-Control` (what every browser does), and `stale-while-revalidate`
//! support so a page paints immediately and refreshes in the background.

use crate::util::time::{parse_http_date, unix_secs, CacheControl};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug)]
pub struct Entry {
    pub url: String,
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub date: u64,
    pub expires: Option<u64>,
    pub cc: CacheControl,
    /// Bytes the body occupied on the wire (compressed), for stats.
    pub wire_bytes: usize,
    /// Insertion clock in milliseconds, used for LRU eviction.
    pub touched_ms: u64,
    /// Only cacheable when `Vary` is empty or `Accept-Encoding`.
    pub vary_simple: bool,
}

impl Entry {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Seconds until expiry; negative means stale.
    pub fn lifetime_left(&self, now: u64) -> i64 {
        match self.cc.freshness(self.date, self.expires) {
            Some(l) => l as i64 - (now.saturating_sub(self.date)) as i64,
            None => 0,
        }
    }

    pub fn is_fresh(&self, now: u64) -> bool {
        if self.cc.no_cache || self.cc.must_revalidate {
            return false;
        }
        self.lifetime_left(now) > 0
    }

    /// Heuristic: 10% of the time since Last-Modified, capped at a day, when the
    /// response has no explicit freshness information.
    pub fn heuristic_fresh(&self, now: u64) -> bool {
        if self.cc.max_age.is_some() || self.expires.is_some() {
            return false;
        }
        let Some(lm) = self.last_modified.as_deref().and_then(parse_http_date) else {
            return false;
        };
        if lm >= now {
            return false;
        }
        let age = now - lm;
        let allowed = (age / 10).min(86_400).max(30);
        now.saturating_sub(self.date) <= allowed
    }

    pub fn stale_while_revalidate_left(&self, now: u64) -> i64 {
        let swr: u64 = header_int(&self.headers, "cache-control", "stale-while-revalidate")
            .unwrap_or(0);
        let deficit = self.lifetime_left(now).abs();
        if (deficit as u64) <= swr {
            deficit
        } else {
            0
        }
    }

    pub fn can_revalidate(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

fn header_int(headers: &[(String, String)], header: &str, directive: &str) -> Option<u64> {
    let v = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(header))?
        .1
        .clone();
    for p in v.split(',') {
        let p = p.trim();
        if let Some(rest) = p.strip_prefix(directive) {
            if let Some(n) = rest.trim_start_matches('=').trim().parse::<u64>().ok() {
                return Some(n);
            }
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    pub hits: usize,
    pub stale_hits: usize,
    pub misses: usize,
    pub not_modified: usize,
    pub stored: usize,
    pub evicted: usize,
    pub body_bytes: usize,
    pub wire_bytes: usize,
}

pub struct Cache {
    map: HashMap<String, Entry>,
    order: VecDeque<String>,
    capacity: usize,
    used: usize,
    pub stats: CacheStats,
    /// `false` when `--cache=off`, which still allows conditional requests.
    enabled: bool,
}

impl Cache {
    pub fn new(capacity_bytes: usize) -> Cache {
        Cache {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity_bytes,
            used: 0,
            stats: CacheStats::default(),
            enabled: true,
        }
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.clear_bodies();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    pub fn get(&self, url: &str) -> Option<&Entry> {
        if !self.enabled {
            return None;
        }
        self.map.get(url)
    }

    pub fn get_mut(&mut self, url: &str) -> Option<&mut Entry> {
        if !self.enabled {
            return None;
        }
        self.map.get_mut(url)
    }

    /// Fresh entries are served straight out of the cache.
    pub fn fresh(&mut self, url: &str) -> Option<&Entry> {
        if !self.enabled {
            return None;
        }
        let now = unix_secs();
        if let Some(e) = self.map.get_mut(url) {
            if e.is_fresh(now) || e.heuristic_fresh(now) {
                self.stats.hits += 1;
                touch(&mut self.order, url);
                return Some(e);
            }
        }
        None
    }

    /// Keep the body but mark it needing validation (background revalidation).
    pub fn take_stale(&mut self, url: &str) -> Option<Entry> {
        if !self.enabled {
            return None;
        }
        let e = self.map.remove(url)?;
        self.used -= e.body.len().min(self.used);
        self.stats.stale_hits += 1;
        Some(e)
    }

    /// Storable? 200/203/206/300/301/302/307/308/404/410 are cacheable per RFC;
    /// `no-store` and unknown `Vary` are not.
    pub fn cacheable(status: u16, cc: &CacheControl, vary: Option<&str>) -> bool {
        if cc.no_store {
            return false;
        }
        let ok_status = matches!(
            status,
            200 | 203 | 204 | 206 | 300 | 301 | 302 | 307 | 308 | 404 | 410
        );
        if !ok_status {
            return false;
        }
        match vary {
            None | Some("") => true,
            Some(v) => {
                v.split(',')
                    .all(|p| p.trim().eq_ignore_ascii_case("accept-encoding"))
            }
        }
    }

    pub fn insert(&mut self, mut e: Entry) {
        if !self.enabled || !Self::cacheable(e.status, &e.cc, e.header("vary")) {
            return;
        }
        e.vary_simple = true;
        let key = e.url.clone();
        let size = e.body.len();
        if let Some(old) = self.map.insert(key.clone(), e) {
            self.used = self.used.saturating_sub(old.body.len());
            touch(&mut self.order, &key);
            return;
        }
        self.used += size;
        self.stats.stored += 1;
        self.order.push_back(key);
        while self.used > self.capacity {
            match self.order.pop_front() {
                Some(k) => {
                    if let Some(gone) = self.map.remove(&k) {
                        self.used = self.used.saturating_sub(gone.body.len());
                        self.stats.evicted += 1;
                    }
                }
                None => break,
            }
        }
    }

    /// Merge a 304 into the stored body, refreshing freshness metadata.
    pub fn revalidate_with(&mut self, url: &str, status: u16, headers: &[(String, String)]) {
        let now = unix_secs();
        let Some(e) = self.map.get_mut(url) else {
            return;
        };
        self.stats.not_modified += 1;
        let mut new_etag = None;
        let mut new_lm = None;
        let mut cc = CacheControl::default();
        let mut date = now;
        let mut expires = None;
        for (k, v) in headers {
            let kl = k.to_ascii_lowercase();
            match kl.as_str() {
                "etag" => new_etag = Some(v.clone()),
                "last-modified" => new_lm = Some(v.clone()),
                "cache-control" => cc = CacheControl::parse(v),
                "date" => date = parse_http_date(v).unwrap_or(now),
                "expires" => {
                    expires = parse_http_date(v);
                }
                _ => {}
            }
        }
        if status == 304 {
            if let Some(x) = new_etag {
                e.etag = Some(x);
            }
            if let Some(x) = new_lm {
                e.last_modified = Some(x);
            }
            e.date = date;
            e.expires = expires;
            e.cc = cc;
            e.touched_ms = crate::util::time::mono_ms() as u64;
            // Only these response headers may change on a 304.
            for (k, v) in headers {
                let kl = k.to_ascii_lowercase();
                if matches!(
                    kl.as_str(),
                    "cache-control"
                        | "content-type"
                        | "expires"
                        | "date"
                        | "server"
                        | "vary"
                        | "origin-agent-cluster"
                ) {
                    if let Some(slot) = e
                        .headers
                        .iter_mut()
                        .find(|(ek, _)| ek.eq_ignore_ascii_case(&kl))
                {
                    slot.1 = v.clone();
                } else {
                    e.headers.push((kl, v.clone()));
                }
            }
        }
        }
    }

    pub fn remove(&mut self, url: &str) {
        if let Some(e) = self.map.remove(url) {
            self.used = self.used.saturating_sub(e.body.len());
            if let Some(pos) = self.order.iter().position(|k| k == url) {
                self.order.remove(pos);
            }
        }
    }

    /// Drop every body but keep validators (ETag/Last-Modified), which is what
    /// `--data-saver` wants: still conditional, no disk pressure.
    pub fn clear_bodies(&mut self) {
        for e in self.map.values_mut() {
            self.used = self.used.saturating_sub(e.body.len());
            e.body = Vec::new();
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.used = 0;
    }

    /// Everything currently known for a URL, for `kilat net --cache`.
    pub fn dump(&self) -> Vec<(String, String, usize, bool)> {
        let now = unix_secs();
        self.map
            .values()
            .map(|e| {
                let state = if e.is_fresh(now) {
                    "fresh"
                } else if e.can_revalidate() {
                    "stale(validatable)"
                } else {
                    "stale"
                }
                .to_string();
                (
                    e.url.clone(),
                    state,
                    e.body.len(),
                    e.etag.is_some(),
                )
            })
            .collect()
    }
}

fn touch(order: &mut VecDeque<String>, url: &str) {
    if let Some(pos) = order.iter().position(|k| k == url) {
        order.remove(pos);
    }
    order.push_back(url.to_string());
}

/// Build a cache entry from a response. Kept here so `page` and `cdp` share one
/// notion of what is stored.
pub fn entry_from_response(
    url: &str,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
    wire_bytes: usize,
) -> Entry {
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    let cc = CacheControl::parse(&get("cache-control").unwrap_or_default());
    Entry {
        url: url.to_string(),
        status,
        reason: reason.to_string(),
        headers: headers.to_vec(),
        body,
        etag: get("etag"),
        last_modified: get("last-modified"),
        date: get("date")
            .as_deref()
            .and_then(parse_http_date)
            .unwrap_or_else(unix_secs),
        expires: get("expires").as_deref().and_then(parse_http_date),
        cc,
        wire_bytes,
        touched_ms: crate::util::time::mono_ms() as u64,
        vary_simple: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn freshness_from_cache_control() {
        let now = unix_secs();
        let mut e = Entry {
            url: "http://x/a.css".into(),
            status: 200,
            reason: "OK".into(),
            headers: hdrs(&[("cache-control", "max-age=100")]),
            body: vec![1u8; 10],
            etag: None,
            last_modified: None,
            date: now,
            expires: None,
            cc: CacheControl::parse("max-age=100"),
            wire_bytes: 10,
            touched_ms: 0,
            vary_simple: false,
        };
        assert!(e.is_fresh(now));
        assert!(e.lifetime_left(now) > 90);
        e.date = now - 500;
        assert!(!e.is_fresh(now));
        assert!(!e.heuristic_fresh(now));
        assert!(!e.can_revalidate());
    }

    #[test]
    fn no_store_is_not_cached() {
        assert!(!Cache::cacheable(200, &CacheControl::parse("no-store"), None));
        assert!(Cache::cacheable(200, &CacheControl::parse("public"), None));
        assert!(!Cache::cacheable(500, &CacheControl::parse(""), None));
        assert!(!Cache::cacheable(
            200,
            &CacheControl::parse(""),
            Some("Accept-Language")
        ));
        assert!(Cache::cacheable(
            200,
            &CacheControl::parse(""),
            Some("accept-encoding")
        ));
    }

    #[test]
    fn lru_evicts_by_bytes() {
        let mut c = Cache::new(100);
        for i in 0..5 {
            let mut e = entry_from_response(
                &format!("http://x/{i}"),
                200,
                "OK",
                &hdrs(&[("cache-control", "max-age=600")]),
                vec![i as u8; 30],
                30,
            );
            e.expires = None;
            c.insert(e);
        }
        assert_eq!(c.len(), 3, "3x30=90 fits, 4th evicts the oldest");
        assert!(c.get("http://x/0").is_none());
        assert!(c.get("http://x/4").is_some());
        assert!(c.stats.evicted >= 1);
    }

    #[test]
    fn fresh_lookup_and_revalidation() {
        let mut c = Cache::new(1 << 16);
        c.insert(entry_from_response(
            "http://x/a",
            200,
            "OK",
            &hdrs(&[
                ("cache-control", "max-age=3600"),
                ("etag", "W/\"1\""),
                ("content-type", "text/html"),
            ]),
            b"<p>hi</p>".to_vec(),
            9,
        ));
        assert!(c.fresh("http://x/a").is_some());
        assert_eq!(c.stats.hits, 1);
        let before = c.get("http://x/a").unwrap().body.clone();
        c.revalidate_with("http://x/a", 304, &hdrs(&[("cache-control", "max-age=60"), ("etag", "W/\"2\"")]));
        let after = c.get("http://x/a").unwrap();
        assert_eq!(after.body, before, "304 keeps the body");
        assert_eq!(after.etag.as_deref(), Some("W/\"2\""));
        assert_eq!(c.stats.not_modified, 1);
    }

    #[test]
    fn disabled_cache_stores_nothing() {
        let mut c = Cache::new(1000);
        c.set_enabled(false);
        c.insert(entry_from_response(
            "http://x/a",
            200,
            "OK",
            &hdrs(&[("cache-control", "max-age=3600")]),
            b"abc".to_vec(),
            3,
        ));
        assert!(c.is_empty());
        assert!(c.fresh("http://x/a").is_none());
    }

    #[test]
    fn heuristic_freshness_uses_last_modified() {
        let now = unix_secs();
        let lm = crate::util::time::http_date(now - 3600 * 24 * 10);
        let mut e = entry_from_response(
            "http://x/b",
            200,
            "OK",
            &hdrs(&[("last-modified", &lm)]),
            b"x".to_vec(),
            1,
        );
        e.date = now;
        assert!(e.heuristic_fresh(now), "1 day of the 10 day age");
        e.date = now - 86_400 * 5;
        assert!(!e.heuristic_fresh(now));
    }
}
