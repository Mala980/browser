//! Logging.
//!
//! `KILAT_LOG=debug` (or `--log=debug`) raises verbosity. Messages are also
//! kept in a small ring buffer so the CDP `Log` domain can replay them, which is
//! handy when a headless run in CI goes wrong.

use std::fmt::Arguments;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub const LEVEL_ERROR: usize = 0;
pub const LEVEL_WARN: usize = 1;
pub const LEVEL_INFO: usize = 2;
pub const LEVEL_DEBUG: usize = 3;
pub const LEVEL_TRACE: usize = 4;

static LEVEL: AtomicUsize = AtomicUsize::new(LEVEL_INFO);
static START: OnceInstant = OnceInstant::new();
/// Last messages, newest last; read by `Log.enable` to replay.
static RING: Mutex<Option<Vec<String>>> = Mutex::new(None);
const RING_CAP: usize = 256;

struct OnceInstant {
    inner: Mutex<Option<Instant>>,
}

impl OnceInstant {
    const fn new() -> OnceInstant {
        OnceInstant {
            inner: Mutex::new(None),
        }
    }
    fn start(&self) -> f64 {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if g.is_none() {
            *g = Some(Instant::now());
        }
        match g {
            Some(t) => t.elapsed().as_secs_f64() * 1000.0,
            None => 0.0,
        }
    }
}

pub fn set_level(name: &str) {
    let v = match name.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "quiet" | "slient" => usize::MAX,
        "error" => LEVEL_ERROR,
        "warn" | "warning" => LEVEL_WARN,
        "info" => LEVEL_INFO,
        "debug" => LEVEL_DEBUG,
        "trace" | "verbose" => LEVEL_TRACE,
        _ => LEVEL_INFO,
    };
    LEVEL.store(v, Ordering::Relaxed);
}

pub fn level() -> usize {
    LEVEL.load(Ordering::Relaxed)
}

pub fn enabled(l: usize) -> bool {
    l <= level()
}

pub fn init_from_env() {
    if let Ok(v) = std::env::var("KILAT_LOG") {
        set_level(&v);
    }
}

pub fn log_at(l: usize, target: &str, args: Arguments) {
    if !enabled(l) {
        return;
    }
    let name = match l {
        LEVEL_ERROR => "ERROR",
        LEVEL_WARN => "WARN ",
        LEVEL_INFO => "INFO ",
        LEVEL_DEBUG => "DEBUG",
        _ => "TRACE",
    };
    let line = format!(
        "[{:9.1}ms] {name} {target}: {}",
        START.start(),
        args
    );
    if l <= LEVEL_WARN {
        let _ = writeln!(std::io::stderr(), "{line}");
    } else {
        let _ = writeln!(std::io::stderr(), "{line}");
    }
    let mut g = match RING.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let buf = g.get_or_insert_with(Vec::new);
    buf.push(line);
    if buf.len() > RING_CAP {
        let excess = buf.len() - RING_CAP;
        buf.drain(0..excess);
    }
}

/// Snapshot of the ring buffer as `(level, message)` pairs.
pub fn recent() -> Vec<(usize, String)> {
    let g = match RING.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match &*g {
        Some(buf) => buf
            .iter()
            .map(|l| {
                let lvl = if l.contains("ERROR") {
                    LEVEL_ERROR
                } else if l.contains("WARN ") {
                    LEVEL_WARN
                } else {
                    LEVEL_INFO
                };
                (lvl, l.clone())
            })
            .collect(),
        None => Vec::new(),
    }
}

#[macro_export]
macro_rules! kerror {
    ($target:expr, $($arg:tt)*) => {
        if $crate::util::log::enabled($crate::util::log::LEVEL_ERROR) {
            $crate::util::log::log_at(
                $crate::util::log::LEVEL_ERROR,
                $target,
                format_args!($($arg)*),
            )
        }
    };
}

#[macro_export]
macro_rules! kwarn {
    ($target:expr, $($arg:tt)*) => {
        if $crate::util::log::enabled($crate::util::log::LEVEL_WARN) {
            $crate::util::log::log_at(
                $crate::util::log::LEVEL_WARN,
                $target,
                format_args!($($arg)*),
            )
        }
    };
}

#[macro_export]
macro_rules! kinfo {
    ($target:expr, $($arg:tt)*) => {
        if $crate::util::log::enabled($crate::util::log::LEVEL_INFO) {
            $crate::util::log::log_at(
                $crate::util::log::LEVEL_INFO,
                $target,
                format_args!($($arg)*),
            )
        }
    };
}

#[macro_export]
macro_rules! kdebug {
    ($target:expr, $($arg:tt)*) => {
        if $crate::util::log::enabled($crate::util::log::LEVEL_DEBUG) {
            $crate::util::log::log_at(
                $crate::util::log::LEVEL_DEBUG,
                $target,
                format_args!($($arg)*),
            )
        }
    };
}

#[macro_export]
macro_rules! ktrace {
    ($target:expr, $($arg:tt)*) => {
        if $crate::util::log::enabled($crate::util::log::LEVEL_TRACE) {
            $crate::util::log::log_at(
                $crate::util::log::LEVEL_TRACE,
                $target,
                format_args!($($arg)*),
            )
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_and_levels() {
        let saved = level();
        set_level("trace");
        kdebug!("test", "hello {}", 42);
        assert!(enabled(LEVEL_DEBUG));
        let r = recent();
        assert!(r.iter().any(|(_, l)| l.contains("hello 42")));
        set_level("off");
        assert!(!enabled(LEVEL_ERROR));
        if saved != usize::MAX {
            LEVEL.store(saved, Ordering::Relaxed);
        }
    }
}
