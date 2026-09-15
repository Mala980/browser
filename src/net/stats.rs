//! Per-page transfer accounting - the numbers behind "feels light".
//!
//! Every fetch reports into a `Stats`, so `kilat browse https://site` can print
//! "1.4 MB over the wire, 2.9 MB saved by cache+blocklist" and the CDP layer can
//! expose the same thing to automation.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Stats {
    /// Responses actually fetched from a server (compressed bytes on the wire).
    pub net_bytes: AtomicU64,
    /// Body bytes reconstructed after decompression.
    pub decoded_bytes: AtomicU64,
    /// Body bytes served from the HTTP cache without touching the network.
    pub cached_bytes: AtomicU64,
    pub requests: AtomicU64,
    pub cache_hits: AtomicU64,
    pub not_modified: AtomicU64,
    /// Requests refused by the blocklist, and the bytes they would have cost.
    pub blocked: AtomicU64,
    pub blocked_bytes: AtomicU64,
    /// Failed requests (DNS, TLS, 4xx/5xx, size caps).
    pub errors: AtomicU64,
    pub images_ok: AtomicU64,
    pub images_skipped: AtomicU64,
    /// Wall time spent in `recv`, for the "smoothness" story.
    pub net_ms: AtomicU64,
}

/// A read-only copy, handy for JSON/CDP and printing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub net_bytes: u64,
    pub decoded_bytes: u64,
    pub cached_bytes: u64,
    pub requests: u64,
    pub cache_hits: u64,
    pub not_modified: u64,
    pub blocked: u64,
    pub blocked_bytes: u64,
    pub errors: u64,
    pub images_ok: u64,
    pub images_skipped: u64,
    pub net_ms: u64,
}

impl Snapshot {
    /// Bytes we did *not* download: cache reuse, blocked requests, and the
    /// difference between the wire and the decoded body (compression).
    pub fn saved_bytes(&self) -> u64 {
        self.cached_bytes
            + self.blocked_bytes
            + self.decoded_bytes.saturating_sub(self.net_bytes)
    }

    pub fn human(&self) -> String {
        format!(
            "{} over the wire, {} requests, {} saved ({} cache hits, {} blocked, {}x 304)",
            kb(self.net_bytes),
            self.requests,
            kb(self.saved_bytes()),
            self.cache_hits,
            self.blocked,
            self.not_modified
        )
    }

    pub fn to_json(&self) -> crate::util::json::Json {
        use crate::util::json::Json;
        let mut m: Vec<(&str, Json)> = Vec::new();
        for (k, v) in [
            ("netBytes", self.net_bytes),
            ("decodedBytes", self.decoded_bytes),
            ("cachedBytes", self.cached_bytes),
            ("requests", self.requests),
            ("cacheHits", self.cache_hits),
            ("notModified", self.not_modified),
            ("blocked", self.blocked),
            ("blockedBytes", self.blocked_bytes),
            ("errors", self.errors),
            ("imagesOk", self.images_ok),
            ("imagesSkipped", self.images_skipped),
            ("netMs", self.net_ms),
            ("savedBytes", self.saved_bytes()),
        ] {
            m.push((k, Json::u(v as usize)));
        }
        Json::object(m)
    }
}

fn kb(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.2} MiB", n as f64 / (1024.0 * 1024.0))
    }
}

impl Stats {
    pub fn add_net(&self, bytes: u64) {
        self.net_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add_decoded(&self, bytes: u64) {
        self.decoded_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
    pub fn add_cached(&self, bytes: u64) {
        self.cached_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add_blocked(&self, bytes: u64) {
        self.blocked.fetch_add(1, Ordering::Relaxed);
        self.blocked_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
    pub fn add_not_modified(&self) {
        self.not_modified.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add_image(&self, ok: bool) {
        if ok {
            self.images_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.images_skipped.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn add_net_ms(&self, ms: u64) {
        self.net_ms.fetch_add(ms, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            net_bytes: self.net_bytes.load(Ordering::Relaxed),
            decoded_bytes: self.decoded_bytes.load(Ordering::Relaxed),
            cached_bytes: self.cached_bytes.load(Ordering::Relaxed),
            requests: self.requests.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            not_modified: self.not_modified.load(Ordering::Relaxed),
            blocked: self.blocked.load(Ordering::Relaxed),
            blocked_bytes: self.blocked_bytes.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            images_ok: self.images_ok.load(Ordering::Relaxed),
            images_skipped: self.images_skipped.load(Ordering::Relaxed),
            net_ms: self.net_ms.load(Ordering::Relaxed),
        }
    }
    pub fn reset(&self) {
        for a in [
            &self.net_bytes,
            &self.decoded_bytes,
            &self.cached_bytes,
            &self.requests,
            &self.cache_hits,
            &self.not_modified,
            &self.blocked,
            &self.blocked_bytes,
            &self.errors,
            &self.images_ok,
            &self.images_skipped,
            &self.net_ms,
        ] {
            a.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_and_savings() {
        let s = Stats::default();
        s.add_net(1000);
        s.add_decoded(4000);
        s.add_cached(2500);
        s.add_blocked(900);
        let snap = s.snapshot();
        assert_eq!(snap.net_bytes, 1000);
        assert_eq!(snap.requests, 1);
        // 1500 saved by compression + 2500 cached + 900 blocked.
        assert_eq!(snap.saved_bytes(), 1500 + 2500 + 900);
        assert!(snap.human().contains("KiB") || snap.human().contains("B"));
        s.reset();
        assert_eq!(s.snapshot(), Snapshot::default());
    }

    #[test]
    fn json_shape() {
        let s = Stats::default();
        s.add_net(10);
        let j = s.snapshot().to_json();
        let text = j.to_string();
        assert!(text.contains("\"netBytes\":10"), "{text}");
        assert!(text.contains("\"savedBytes\""), "{text}");
    }
}
