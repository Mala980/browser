//! Clocks plus the two header formats the HTTP cache needs (`Date`, `Expires`,
//! `Last-Modified`, `max-age`).

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static ZERO: Mutex<Option<Instant>> = Mutex::new(None);

/// Milliseconds since the process started; used for rAF pacing, frame budgets and
/// the `perf` numbers reported by CDP.
pub fn mono_ms() -> f64 {
    let mut g = match ZERO.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if g.is_none() {
        *g = Some(Instant::now());
    }
    match &*g {
        Some(t) => t.elapsed().as_secs_f64() * 1000.0,
        None => 0.0,
    }
}

pub fn unix_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    }
}

pub fn unix_secs() -> u64 {
    unix_ms() / 1000
}

pub fn sleep_ms(ms: u64) {
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// `Tue, 15 Nov 1994 08:12:31 GMT`
pub fn http_date(secs: u64) -> String {
    let (y, mo, d, hh, mm, ss, dow) = civil(secs as i64);
    let days = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        days[dow as usize % 7],
        d,
        months[mo as usize - 1],
        y,
        hh,
        mm,
        ss
    )
}

pub fn now_http_date() -> String {
    http_date(unix_secs())
}

fn leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days since epoch -> weekday index where 0 == Thursday (epoch was a Thursday).
fn dow_from_days(z: i64) -> i64 {
    let mut x = z + 4;
    x %= 7;
    if x < 0 {
        x += 7;
    }
    x
}

/// epoch seconds -> (year, month, day, hour, min, sec, weekday) in UTC.
/// Hinnant's civil_from_days; we only need dates at or after 1970.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = if secs < 0 { 0 } else { secs };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    // mp is the "months since March" index: 0..9 -> Mar..Dec, 10..11 -> Jan..Feb.
    let m = if mp < 10 { (mp + 3) as u32 } else { (mp - 9) as u32 };
    if m <= 2 {
        y += 1;
    }
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
        dow_from_days(days) as u32,
    )
}

/// Parse the three date formats HTTP allows: RFC 1123 (`Tue, 14 Nov 2023
/// 22:13:20 GMT`), obsolete RFC 850 (`Tue, 14-Nov-23 22:13:20 GMT`) and
/// asctime (`Tue Nov 14 22:13:20 2023`). Returns epoch seconds.
pub fn parse_http_date(text: &str) -> Option<u64> {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split_whitespace().map(|s| s.trim()).collect();
    let (day, month, year, time) = if parts.len() >= 4 && parts[0].contains('-') {
        let mut it = parts[0].split('-');
        (it.next()?, it.next()?, it.next()?, parts[1])
    } else if parts.len() >= 5 {
        // Drop the leading "Tue," weekday token.
        (parts[1], parts[2], parts[3], parts[4])
    } else if parts.len() == 5 && parts[1].len() == 3 {
        (parts[2], parts[1], parts[4], parts[3])
    } else {
        return None;
    };
    let dnum: i64 = day.parse().ok()?;
    let mnum = month_name_to_num(month)?;
    let mut ynum: i64 = year.parse().ok()?;
    if ynum < 100 {
        ynum += if ynum < 70 { 2000 } else { 1900 };
    }
    let tp: Vec<&str> = time.trim_end_matches("gmt").trim().split(':').collect();
    if tp.len() != 3 {
        return None;
    }
    let h: i64 = tp[0].parse().ok()?;
    let mi: i64 = tp[1].parse().ok()?;
    let sc: i64 = tp[2].parse().ok()?;
    if dnum < 1
        || dnum > 31
        || mnum < 1
        || mnum > 12
        || h > 23
        || mi > 59
        || sc > 60
        || ynum < 1970
    {
        return None;
    }
    let secs = days_from_civil(ynum, mnum, dnum) * 86_400 + h * 3600 + mi * 60 + sc;
    if secs < 0 {
        return None;
    }
    Some(secs as u64)
}

fn month_name_to_num(m: &str) -> Option<i64> {
    let names = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let m = m.trim().to_ascii_lowercase();
    for (i, n) in names.iter().enumerate() {
        if m.starts_with(n) {
            return Some(i as i64 + 1);
        }
    }
    None
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parsed `Cache-Control` value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CacheControl {
    pub max_age: Option<i64>,
    pub s_max_age: Option<i64>,
    pub no_cache: bool,
    pub no_store: bool,
    pub must_revalidate: bool,
    pub immutable: bool,
    pub public: bool,
    pub private: bool,
}

impl CacheControl {
    pub fn parse(value: &str) -> CacheControl {
        let mut cc = CacheControl::default();
        for part in value.split(',') {
            let p = part.trim();
            let (k, v) = match p.split_once('=') {
                Some((a, b)) => (a.trim().to_ascii_lowercase(), b.trim().trim_matches('"')),
                None => (p.to_ascii_lowercase(), ""),
            };
            let secs = v.parse::<i64>().ok();
            match k.as_str() {
                "max-age" => cc.max_age = secs,
                "s-maxage" => cc.s_max_age = secs,
                "no-cache" => cc.no_cache = true,
                "no-store" => cc.no_store = true,
                "must-revalidate" => cc.must_revalidate = true,
                "proxy-revalidate" => cc.must_revalidate = true,
                "immutable" => cc.immutable = true,
                "public" => cc.public = true,
                "private" => cc.private = true,
                _ => {}
            }
        }
        cc
    }

    /// Fresh lifetime in seconds, or None when the response is uncacheable.
    pub fn freshness(&self, date: u64, expires: Option<u64>) -> Option<i64> {
        if self.no_store {
            return None;
        }
        if let Some(a) = self.max_age {
            return Some(a);
        }
        if let Some(exp) = expires {
            if exp > date {
                return Some((exp - date) as i64);
            }
            return Some(0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_roundtrip() {
        // Fixed points from the HTTP spec / known dates.
        assert_eq!(http_date(784_465_537), "Thu, 10 Nov 1994 11:05:37 GMT");
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
        let back = parse_http_date("Tue, 14 Nov 2023 22:13:20 GMT").unwrap();
        assert_eq!(back, 1_700_000_000);
        assert_eq!(parse_http_date("Thu, 14-Nov-23 22:13:20 GMT"), Some(1_700_000_000));
        assert_eq!(parse_http_date("Tue Nov 14 22:13:20 2023"), None);
        assert_eq!(parse_http_date(""), None);
        assert_eq!(parse_http_date("garbage"), None);
    }

    #[test]
    fn cache_control_parsing() {
        let cc = CacheControl::parse("public, max-age=31536000, immutable");
        assert_eq!(cc.max_age, Some(31_536_000));
        assert!(cc.public && cc.immutable && !cc.no_store);
        let cc2 = CacheControl::parse("no-cache, no-store, must-revalidate");
        assert!(cc2.no_cache && cc2.no_store && cc2.must_revalidate);
        assert_eq!(cc2.freshness(100, None), None);
        assert_eq!(CacheControl::default().freshness(100, Some(160)), Some(60));
        assert_eq!(CacheControl::default().freshness(160, Some(100)), Some(0));
    }

    #[test]
    fn mono_clock_moves() {
        let a = mono_ms();
        sleep_ms(3);
        assert!(mono_ms() >= a);
        assert!(unix_ms() > 1_700_000_000_000);
    }
}
