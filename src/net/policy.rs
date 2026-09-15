//! Request policy: what we ask for, what we refuse, and how much we allow.
//!
//! Bandwidth levers, in the order they pay off:
//!   1. never fetch ad/tracker hosts (blocklist, AdBlock-syntax subset),
//!   2. only request image types we can decode (a page stops sending us AVIF),
//!   3. `--data-saver`: cap sizes, prefer small images, skip fonts,
//!   4. HTTP cache + conditional requests (see [`crate::net::cache`]).

use crate::net::url::Url;

/// A single blocklist pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    /// Host must end with this (`||doubleclick.net^`). Empty means "path only".
    pub host: String,
    /// Substring that must appear in the path (lowercased).
    pub path: Option<String>,
    /// `@@` exception: matching this un-blocks.
    pub allow: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub blocklist: Vec<Pattern>,
    /// `--data-saver`: cap bodies, skip optional resources.
    pub data_saver: bool,
    /// Fetch subresources at all (off for `--dump-dom`).
    pub subresources: bool,
    pub images: bool,
    pub fonts: bool,
    pub video: bool,
    pub cookies: bool,
    pub javascript: bool,
    /// Referrer leakage: `never` sends none, `origin` sends only the origin.
    pub referrer: Referrer,
    pub max_body_bytes: usize,
    pub max_image_bytes: usize,
    pub accept_language: String,
    pub user_agent: String,
    /// Appended to every request (`--header Name: Value`).
    pub extra_headers: Vec<(String, String)>,
    /// Counters for `kilat stats`.
    pub blocked: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Referrer {
    #[default]
    Origin,
    Full,
    Never,
}

/// The embedded default list: the ad/tracking infrastructure large enough that
/// blocking it is a pure win for a text-and-image browser, and nothing that
/// breaks ordinary site function (CDNs, auth, payments are all absent).
const DEFAULT_BLOCK: &str = r#"
||doubleclick.net^
||googlesyndication.com^
||googleadservices.com^
||google-analytics.com^
||analytics.google.com^
||adservice.google.
||securepubads.g.doubleclick.net^
||pagead2.googlesyndication.com^
||admob.com^
||adsensecustomsearchads.com^
||amazon-adsystem.com^
||adnxs.com^
||adsafeprotected.com^
||smartadserver.com^
||criteo.com^
||criteo.net^
||taboola.com^
||outbrain.com^
||pubmatic.com^
||rubiconproject.com^
||openx.net^
||casalemedia.com^
||indexww.com^
||moatads.com^
||scorecardresearch.com^
||quantserve.com^
||chartbeat.com^
||chartbeat.net^
||hotjar.com^
||clarity.ms^
||mixpanel.com^
||segment.io^
||segment.com^
||amplitude.com^
||branch.io^
||appsflyer.com^
||adjust.com^
||facebook.com/tr?
||facebook.net/en_US/fbevents
||connect.facebook.net^
||twitter.com/i/ads
||ads-twitter.com^
||snapchat.com/ads
||mc.yandex.ru^
||yandex.ru/metrika
||top-fwz1.mail.ru^
||cnzz.com^
||hm.baidu.com^
||growingio.com^
||umeng.com^
||talkingdata.com^
||matomo.
||piwik.
||googletagmanager.com/gtag/
||googleoptimize.com^
||fullstory.com^
||logrocket.com^
||bugsnag.com^
||sentry-cdn.com^
||newrelic.com^
||nr-data.net^
||optmnstr.com^
||mailmunch.co^
||popads.net^
||propellerads.com^
||juicyads.com^
||exoclick.com^
||popcash.net^
||hilltopads.com^
"#;

impl Policy {
    /// `--data-saver` off, blocklist on: the default browsing profile.
    pub fn new() -> Policy {
        Policy {
            blocklist: parse_adblock_list(DEFAULT_BLOCK),
            data_saver: false,
            subresources: true,
            images: true,
            fonts: true,
            video: true,
            cookies: true,
            javascript: false,
            referrer: Referrer::Origin,
            max_body_bytes: 32 << 20,
            max_image_bytes: 8 << 20,
            accept_language: "en-US,en;q=0.9".to_string(),
            user_agent: crate::version_string(),
            extra_headers: Vec::new(),
            blocked: 0,
        }
    }

    /// The light profile: no fonts, smaller caps, everything blocked by default.
    pub fn data_saver() -> Policy {
        let mut p = Policy::new();
        p.data_saver = true;
        p.fonts = false;
        p.max_body_bytes = 8 << 20;
        p.max_image_bytes = 1 << 20;
        p
    }

    /// Nothing loaded except the document itself.
    pub fn bare() -> Policy {
        let mut p = Policy::new();
        p.subresources = false;
        p.images = false;
        p.fonts = false;
        p.video = false;
        p.javascript = false;
        p
    }

    pub fn add_blocklist(&mut self, text: &str) {
        self.blocklist.extend(parse_adblock_list(text));
    }

    pub fn load_blocklist_file(&mut self, path: &str) -> crate::util::Result<usize> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("blocklist {path}: {e}"))?;
        let n = self.blocklist.len();
        self.add_blocklist(&text);
        Ok(self.blocklist.len() - n)
    }

    /// Should this URL be fetched at all?
    pub fn blocks(&self, url: &Url) -> bool {
        if !url.is_http() {
            return false;
        }
        let host = url.host.as_str();
        let path = url.path.as_str();
        let query = url.query.clone().unwrap_or_default();
        let mut blocked = false;
        for p in self.blocklist.iter() {
            if !p.host.is_empty() && !host_ends_with(host, &p.host) {
                continue;
            }
            if let Some(sub) = &p.path {
                let hay = if query.is_empty() {
                    url.to_string().to_ascii_lowercase()
                } else {
                    format!("{url}").to_ascii_lowercase()
                };
                if !hay.contains(sub.as_str()) {
                    continue;
                }
            }
            blocked = !p.allow;
            if p.allow {
                return false;
            }
        }
        blocked
    }

    /// Subresources we refuse outright under the current profile.
    pub fn blocks_kind(&self, kind: ResourceKind) -> bool {
        if !self.subresources {
            return !matches!(kind, ResourceKind::Document);
        }
        match kind {
            ResourceKind::Image => !self.images,
            ResourceKind::Font => !self.fonts,
            ResourceKind::Video | ResourceKind::Audio => !self.video,
            ResourceKind::Script => !self.javascript,
            _ => false,
        }
    }

    /// The `Accept` header for images: we advertise only what we can decode, so
    /// servers hand us PNG/JPEG instead of a 400 KB AVIF we would discard.
    pub fn accept_for(&self, kind: ResourceKind) -> String {
        match kind {
            ResourceKind::Image if self.data_saver => {
                "image/png, image/jpeg, image/gif;q=0.9, image/webp;q=0.8, */*;q=0.1".to_string()
            }
            ResourceKind::Image => {
                "image/png, image/jpeg, image/gif, image/apng, image/webp;q=0.9, */*;q=0.5"
                    .to_string()
            }
            ResourceKind::Document => "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
                .to_string(),
            ResourceKind::Stylesheet => "text/css,*/*;q=0.1".to_string(),
            ResourceKind::Script => "*/*".to_string(),
            ResourceKind::Font => "font/woff2, font/woff, font/ttf, */*;q=0.5".to_string(),
            ResourceKind::Xhr => "application/json, text/plain, */*".to_string(),
            _ => "*/*".to_string(),
        }
    }

    pub fn referrer_value(&self, document: Option<&Url>) -> Option<String> {
        if self.referrer == Referrer::Never {
            return None;
        }
        let d = document?;
        if !d.is_http() {
            return None;
        }
        match self.referrer {
            Referrer::Full => Some(d.to_string()),
            _ => Some(format!("{}/", d.origin())),
        }
    }

    /// Byte cap for a resource class.
    pub fn cap_for(&self, kind: ResourceKind) -> usize {
        match kind {
            ResourceKind::Image => self.max_image_bytes,
            ResourceKind::Video if self.data_saver => 24 << 20,
            _ => self.max_body_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Document,
    Stylesheet,
    Script,
    Image,
    Font,
    Xhr,
    Video,
    Audio,
    Other,
}

impl ResourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResourceKind::Document => "document",
            ResourceKind::Stylesheet => "stylesheet",
            ResourceKind::Script => "script",
            ResourceKind::Image => "image",
            ResourceKind::Font => "font",
            ResourceKind::Xhr => "xhr",
            ResourceKind::Video => "video",
            ResourceKind::Audio => "audio",
            ResourceKind::Other => "other",
        }
    }

    pub fn from_extension_or_mime(hint: &str) -> ResourceKind {
        let h = hint.to_ascii_lowercase();
        if h.contains("html") || h.is_empty() {
            ResourceKind::Document
        } else if h.contains("css") {
            ResourceKind::Stylesheet
        } else if h.contains("javascript") || h.ends_with(".js") || h.ends_with(".mjs") {
            ResourceKind::Script
        } else if h.contains("image") || matches!(h.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "ico") {
            ResourceKind::Image
        } else if h.contains("font") || matches!(h.as_str(), "woff" | "woff2" | "ttf" | "otf") {
            ResourceKind::Font
        } else if h.contains("video") || matches!(h.as_str(), "mp4" | "webm" | "mov" | "mkv") {
            ResourceKind::Video
        } else if h.contains("audio") || matches!(h.as_str(), "mp3" | "ogg" | "wav" | "m4a") {
            ResourceKind::Audio
        } else if h.contains("json") || h.contains("xml") {
            ResourceKind::Xhr
        } else {
            ResourceKind::Other
        }
    }
}

fn host_ends_with(host: &str, suffix: &str) -> bool {
    let s = suffix.trim_end_matches('.');
    host == s || host.ends_with(&format!(".{s}"))
}

/// A useful subset of the AdBlock syntax: `||host^`, `||host/path`, `path`,
/// `@@exception`, wildcards as `*`, and everything else as a substring.
pub fn parse_adblock_list(text: &str) -> Vec<Pattern> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('!') || l.starts_with('[') || (l.contains("||") && l.len() < 5)
        {
            continue;
        }
        let (allow, rest) = match l.strip_prefix("@@") {
            Some(r) => (true, r),
            None => (false, l),
        };
        let rest = rest
            .trim_start_matches('|')
            .trim_start_matches('/')
            .trim_start_matches('*');
        if rest.is_empty() {
            continue;
        }
        // `||domain^` and `||domain/path`.
        let (host, path) = if let Some(stripped) = l.strip_prefix("@@||").or(l.strip_prefix("||")) {
            let cut = stripped.find(['^', '/']).unwrap_or(stripped.len());
            let h = stripped[..cut].trim_end_matches('^').to_ascii_lowercase();
            let p = stripped[cut..]
                .trim_start_matches('^')
                .trim_start_matches('/')
                .trim_end_matches('^');
            let p = p.split('*').next().unwrap_or(p);
            (
                h,
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_ascii_lowercase())
                },
            )
        } else {
            (String::new(), Some(rest.to_ascii_lowercase()))
        };
        if host.is_empty() && path.is_none() {
            continue;
        }
        out.push(Pattern {
            host,
            path,
            allow,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn default_list_blocks_ads_only() {
        let p = Policy::new();
        assert!(p.blocks(&url("https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js")));
        assert!(p.blocks(&url("https://www.google-analytics.com/analytics.js")));
        assert!(p.blocks(&url("https://stats.g.doubleclick.net/g/collect?v=2")));
        assert!(!p.blocks(&url("https://example.com/ads.css")));
        assert!(!p.blocks(&url("https://cdn.jsdelivr.net/npm/vue")));
        assert!(!p.blocks(&url("https://accounts.google.com/o/oauth2")));
        assert!(p.blocklist.len() > 40);
    }

    #[test]
    fn path_patterns_and_exceptions() {
        let mut p = Policy::default();
        p.add_blocklist("||fb.com/tr?\n@@||cdn.fb.com/tr?\n||evil.com/ads/*");
        assert!(p.blocks(&url("https://fb.com/tr?id=1")));
        assert!(!p.blocks(&url("https://cdn.fb.com/tr?id=1")));
        assert!(p.blocks(&url("https://www.evil.com/ads/x.js")));
        assert!(!p.blocks(&url("https://www.evil.com/app.js")));
    }

    #[test]
    fn kind_filters() {
        let mut p = Policy::new();
        p.fonts = false;
        assert!(p.blocks_kind(ResourceKind::Font));
        assert!(!p.blocks_kind(ResourceKind::Stylesheet));
        p.subresources = false;
        assert!(p.blocks_kind(ResourceKind::Stylesheet));
        assert!(!p.blocks_kind(ResourceKind::Document));
    }

    #[test]
    fn accept_headers_advertise_decodable_images() {
        let p = Policy::new();
        let a = p.accept_for(ResourceKind::Image);
        assert!(a.contains("image/png") && a.contains("image/jpeg"));
        assert!(p.accept_for(ResourceKind::Document).contains("text/html"));
        let ds = Policy::data_saver();
        assert!(ds.accept_for(ResourceKind::Image).contains("image/webp;q=0.8"));
        assert!(ds.max_image_bytes < p.max_image_bytes);
    }

    #[test]
    fn referrer_policy() {
        let mut p = Policy::new();
        let doc = url("https://a.dev/x/y?z=1");
        assert_eq!(p.referrer_value(Some(&doc)).as_deref(), Some("https://a.dev/"));
        p.referrer = Referrer::Never;
        assert_eq!(p.referrer_value(Some(&doc)), None);
        p.referrer = Referrer::Full;
        assert_eq!(
            p.referrer_value(Some(&doc)).as_deref(),
            Some("https://a.dev/x/y?z=1")
        );
        assert_eq!(p.referrer_value(Some(&url("file:///a/b"))), None);
    }

    #[test]
    fn kind_from_hints() {
        assert_eq!(
            ResourceKind::from_extension_or_mime("text/css"),
            ResourceKind::Stylesheet
        );
        assert_eq!(
            ResourceKind::from_extension_or_mime("image/webp"),
            ResourceKind::Image
        );
        assert_eq!(
            ResourceKind::from_extension_or_mime("woff2"),
            ResourceKind::Font
        );
        assert_eq!(
            ResourceKind::from_extension_or_mime("application/json"),
            ResourceKind::Xhr
        );
        assert_eq!(
            ResourceKind::from_extension_or_mime(""),
            ResourceKind::Document
        );
    }
}
