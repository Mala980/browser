// Kilat build script.
//
// Two jobs, both small:
//  1. If `third_party/quickjs` is present and the `js` feature is on, compile the
//     QuickJS C sources plus our flat C shim (src/js/glue.c) into a static lib and
//     link it. No `cc` crate: we call the system compiler directly, exactly like the
//     Termux package build does (see termux/packages/*/build.sh).
//  2. Emit the git commit + version into the binary (`kilat --version`).

use std::path::Path;
use std::process::Command;

fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
        || std::env::var(name)
            .ok()
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".to_string());
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo::rustc-check-cfg=cfg(kilat_no_tls)");
    println!("cargo:rerun-if-changed=third_party/quickjs");
    println!("cargo:rerun-if-changed=src/js/glue.c");
    println!("cargo:rustc-env=KILAT_TARGET={target}");
    println!("cargo:rustc-env=KILAT_VERSION={}", version_string());

    if env_flag("KILAT_NO_TLS") {
        println!("cargo:rustc-cfg=kilat_no_tls");
    } else {
        // libssl/libcrypto are dlopen()'d at runtime, so nothing to link here.
        // The env var lets packagers point at a non-default location.
        if let Ok(p) = std::env::var("KILAT_OPENSSL_DIR") {
            println!("cargo:rustc-env=KILAT_OPENSSL_DIR={p}");
        }
    }

    if std::env::var("CARGO_FEATURE_JS").is_ok() {
        build_quickjs(&out_dir, &target);
    }
}

fn version_string() -> String {
    let cargo_v = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if sha.is_empty() {
        cargo_v
    } else {
        format!("{cargo_v}+{sha}")
    }
}

/// QuickJS is a single-C-file interpreter. We build it with the same compiler
/// cargo is using for the target so cross builds (Termux aarch64) just work.
fn build_quickjs(out_dir: &str, target: &str) {
    let qjs = Path::new("third_party/quickjs");
    if !qjs.join("quickjs.c").exists() {
        println!("cargo:warning=js feature enabled but third_party/quickjs is missing; run scripts/fetch-quickjs.sh - building without JavaScript");
        return;
    }
    let cc = std::env::var("CC").unwrap_or_else(|_| {
        if target.contains("darwin") {
            "cc".to_string()
        } else if target.contains("android") {
            std::env::var("KILAT_CC").unwrap_or_else(|_| "cc".to_string())
        } else {
            "cc".to_string()
        }
    });
    let ar = std::env::var("AR").unwrap_or_else(|_| {
        if target.contains("android") {
            std::env::var("KILAT_AR").unwrap_or_else(|_| "ar".to_string())
        } else {
            "ar".to_string()
        }
    });
    let files = [
        "quickjs.c",
        "libregexp.c",
        "libunicode.c",
        "cutils.c",
        "libbf.c",
    ];
    let obj_dir = Path::new(out_dir).join("qjsobjs");
    let _ = std::fs::create_dir_all(&obj_dir);
    let mut objs: Vec<String> = Vec::new();
    // -D_GNU_SOURCE and the config defines are what quickjs' own Makefile uses.
    let common: Vec<&str> = vec![
        "-O2",
        "-std=gnu11",
        "-funsigned-char",
        "-DNDEBUG",
        "-DCONFIG_VERSION=\"2025-01-01\"",
        "-DCONFIG_BIGNUM",
        "-D_GNU_SOURCE",
        "-Wno-unused-parameter",
        "-Wno-implicit-fallthrough",
        "-I",
        "third_party/quickjs",
    ];
    for name in files.iter() {
        let src = qjs.join(name);
        if !src.exists() {
            continue; // e.g. libbf.c absent in some checkouts
        }
        let obj = obj_dir.join(format!("{}.o", name.replace('.', "_")));
        let obj_s = obj.to_string_lossy().to_string();
        let st = Command::new(&cc)
            .args(&common)
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj_s)
            .status();
        match st {
            Ok(s) if s.success() => objs.push(obj_s),
            other => {
                println!("cargo:warning=compiling {name} failed ({other:?}); building without JavaScript");
                return;
            }
        }
    }
    let glue = obj_dir.join("glue.o");
    let glue_s = glue.to_string_lossy().to_string();
    let st = Command::new(&cc)
        .args(&common)
        .arg("-I")
        .arg("src/js")
        .arg("-c")
        .arg("src/js/glue.c")
        .arg("-o")
        .arg(&glue_s)
        .status();
    match st {
        Ok(s) if s.success() => objs.push(glue_s),
        other => {
            println!("cargo:warning=compiling src/js/glue.c failed ({other:?}); building without JavaScript");
            return;
        }
    }
    let lib = Path::new(out_dir).join("libkilatjs.a");
    let lib_s = lib.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&lib);
    // `ar rcs` is portable enough (GNU binutils, llvm-ar, Termux both provide it).
    let mut cmd = Command::new(&ar);
    cmd.arg("rcs").arg(&lib_s);
    for o in &objs {
        cmd.arg(o);
    }
    match cmd.status() {
        Ok(s) if s.success() => {}
        other => {
            println!("cargo:warning=archiving quickjs failed ({other:?}); building without JavaScript");
            return;
        }
    }
    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-lib=static=kilatjs");
    println!("cargo:rustc-link-lib=static=m");
    println!("cargo:rustc-link-lib=static=dl");
    println!("cargo:rustc-link-lib=static=pthread");
    println!("cargo:warning=JavaScript engine: QuickJS ({target})");
}
