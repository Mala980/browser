//! Command line entry point.
//!
//! Flags follow Chromium's spelling wherever one exists (`--headless`,
//! `--remote-debugging-port`, `--screenshot`, `--user-data-dir`, `--window-size`,
//! `--disable-gpu`, ...) so existing puppeteer/go-rod scripts and docs work
//! unmodified, with Kilat-only switches added under `--data-saver`, `--budget`,
//! `--blocklist`, `--block`, `--lazy-images`, `--stats`.

use crate::util::Json;

/// Exit codes.
pub const OK: i32 = 0;
pub const USAGE_ERROR: i32 = 2;
pub const RUNTIME_ERROR: i32 = 1;

pub fn run(args: &[String]) -> i32 {
    crate::util::log::init_from_env();
    let first = args.get(1).map(|s| s.as_str()).unwrap_or("");
    match first {
        "-V" | "--version" => {
            println!("{}", crate::version_string());
            OK
        }
        "-h" | "--help" | "help" => {
            print!("{}", usage());
            OK
        }
        "selftest" => selftest(),
        "dev" => dev(&args[2..]),
        _ => {
            // The browser front end (headless navigation, live-view window, CDP
            // listener) is wired up in `cli::browse`; keep parsing generic here.
            match crate::cli::browse::browse(args) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("kilat: {e}");
                    RUNTIME_ERROR
                }
            }
        }
    }
}

pub fn usage() -> String {
    let mut out = String::new();
    out.push_str(&format!("{} - a light web browser with a CDP server\n\n", crate::version_string()));
    out.push_str("USAGE\n  kilat [options] [url]\n\n");
    let rows: [(&str, &str); 26] = [
        ("--headless[=old|new]", "run without a window (default when no TTY)"),
        ("--window[=stream|off]", "full-browser mode: live view server, opened with termux-open-url"),
        ("--view-port=N", "port for the live view window (default 9223)"),
        ("--remote-debugging-port=N", "Chrome DevTools Protocol port (0 = random)"),
        ("--remote-debugging-address=IP", "bind address for CDP (default 127.0.0.1)"),
        ("--screenshot=FILE.png", "capture the rendered page and exit"),
        ("--dump-dom", "print the serialised DOM and exit"),
        ("--text", "print the visible text of the page and exit"),
        ("--json", "print a machine readable result (url, stats, timings, title)"),
        ("--pdf=FILE", "print to PDF (single page, text preserved)"),
        ("--viewport=WxH", "layout viewport in CSS pixels (default 1280x720)"),
        ("--device-scale=N", "device pixel ratio for painting (default 1)"),
        ("--timeout=SEC", "navigation timeout (default 30)"),
        ("--wait=load|dom|idle", "what to wait for before returning (default idle)"),
        ("--user-data-dir=DIR", "profile dir: HTTP cache, cookies, prefs"),
        ("--data-saver", "prefer smaller resources: skip webfonts, cap image size, lazy images"),
        ("--budget=BYTES", "abort the navigation after this many bytes on the wire"),
        ("--blocklist=on|off|FILE", "third-party tracker/ad blocking (default on)"),
        ("--block=HOST[,HOST]", "extra host patterns to block"),
        ("--allow=HOST[,HOST]", "remove hosts from the blocklist"),
        ("--images=on|off|lazy", "image loading policy (default lazy)"),
        ("--video=on|auto|off", "video fetching: off, metadata only (auto) or play"),
        ("--js=on|off", "enable JavaScript (QuickJS) - on by default in builds with the js feature"),
        ("--ua=STRING", "override the user agent"),
        ("--log=error|warn|info|debug|trace", "verbose logging (or KILAT_LOG)"),
        ("--stats", "print the byte/transfer accounting table"),
    ];
    for (flag, help) in rows.iter() {
        out.push_str(&format!("  {flag:<34} {help}\n"));
    }
    out.push_str(
        "\nSUBCOMMANDS\n  kilat selftest          run the internal test suite (used by CI)\n  \
         kilat dev <tool>        codec/util cross-check helpers (inflate, gzip, png, ws)\n",
    );
    out
}

/// `kilat selftest`: cheap sanity checks that also work in a stripped CI run.
fn selftest() -> i32 {
    let mut failures = 0usize;
    let mut check = |name: &str, ok: bool| {
        println!("{:<28} {}", name, if ok { "ok" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };
    check(
        "inflate/deflate roundtrip",
        crate::codec::deflate::roundtrip_check(b"the repetition repetition repetition repetition").unwrap_or(false),
    );
    let px = vec![3u8; 8 * 8 * 4];
    let enc = crate::codec::png::encode(8, 8, &px);
    let dec = crate::codec::png::decode(&enc).unwrap_or_else(|_| crate::codec::Decoded::still(0, 0, Vec::new()));
    check("png roundtrip", dec.width == 8 && dec.rgba == px);
    check(
        "gzip roundtrip",
        crate::codec::inflate::gunzip(&crate::codec::deflate::compress_gzip(b"hello world", 6))
            .map(|v| v == b"hello world")
            .unwrap_or(false),
    );
    check(
        "websocket accept key",
        crate::codec::sha1::websocket_accept("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
    );
    check("json parse", Json::parse(r#"{"a":[1,2,{"b":null}]}"#).is_ok());
    if failures == 0 {
        println!("all selftests passed");
        OK
    } else {
        println!("{failures} selftest(s) failed");
        RUNTIME_ERROR
    }
}

/// `kilat dev ...` helpers used by CI to cross-check the codecs against
/// independent implementations (python zlib, ImageMagick, libjpeg).
fn dev(args: &[String]) -> i32 {
    let tool = args.first().map(|s| s.as_str()).unwrap_or("");
    let read_stdin = || -> Vec<u8> {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut buf);
        buf
    };
    let write_stdout = |data: &[u8]| -> i32 {
        use std::io::Write;
        let so = std::io::stdout();
        let mut lock = so.lock();
        if lock.write_all(data).is_err() {
            return RUNTIME_ERROR;
        }
        let _ = lock.flush();
        OK
    };
    match tool {
        "inflate" | "gunzip" => {
            let src = read_stdin();
            match crate::codec::inflate::gunzip(&src) {
                Ok(v) => write_stdout(&v),
                Err(e) => {
                    eprintln!("kilat dev {tool}: {e}");
                    RUNTIME_ERROR
                }
            }
        }
        "inflate-zlib" => {
            let src = read_stdin();
            match crate::codec::inflate::inflate_zlib(&src) {
                Ok(v) => write_stdout(&v),
                Err(e) => {
                    eprintln!("kilat dev inflate-zlib: {e}");
                    RUNTIME_ERROR
                }
            }
        }
        "gzip" => {
            let src = read_stdin();
            let lvl: u32 = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(6);
            write_stdout(&crate::codec::deflate::compress_gzip(&src, lvl))
        }
        "deflate" => {
            let src = read_stdin();
            write_stdout(&crate::codec::deflate::compress(&src))
        }
        "png-decode" => {
            let src = read_stdin();
            match crate::codec::png::decode(&src) {
                Ok(d) => {
                    let mut hdr = format!("{}x{}\n", d.width, d.height).into_bytes();
                    hdr.extend_from_slice(&d.rgba);
                    write_stdout(&hdr)
                }
                Err(e) => {
                    eprintln!("kilat dev png-decode: {e}");
                    RUNTIME_ERROR
                }
            }
        }
        "png-encode" => {
            // stdin: "WxH\n" followed by W*H*4 RGBA bytes
            let src = read_stdin();
            let nl = match src.iter().position(|&b| b == b'\n') {
                Some(i) => i,
                None => {
                    eprintln!("kilat dev png-encode: missing header line");
                    return USAGE_ERROR;
                }
            };
            let header = String::from_utf8_lossy(&src[..nl]).replace('x', " ");
            let mut it = header.split_whitespace();
            let w: u32 = match it.next().and_then(|v| v.parse().ok()) {
                Some(v) => v,
                None => return USAGE_ERROR,
            };
            let h: u32 = match it.next().and_then(|v| v.parse().ok()) {
                Some(v) => v,
                None => return USAGE_ERROR,
            };
            write_stdout(&crate::codec::png::encode(w, h, &src[nl + 1..]))
        }
        "jpeg-decode" => {
            let src = read_stdin();
            match crate::codec::jpeg::decode(&src) {
                Ok(d) => {
                    let mut hdr = format!("{}x{}\n", d.width, d.height).into_bytes();
                    hdr.extend_from_slice(&d.rgba);
                    write_stdout(&hdr)
                }
                Err(e) => {
                    eprintln!("kilat dev jpeg-decode: {e}");
                    RUNTIME_ERROR
                }
            }
        }
        "ws-accept" => {
            let key = args.get(2).cloned().unwrap_or_default();
            println!("{}", crate::codec::sha1::websocket_accept(&key));
            OK
        }
        other => {
            eprintln!("unknown dev tool {other:?}");
            USAGE_ERROR
        }
    }
}

pub(crate) mod browse;
pub(crate) mod devtools;
