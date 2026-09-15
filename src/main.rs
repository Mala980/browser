//! Kilat CLI.
//!
//! All real work happens in the library; `main` only forwards argv and maps the
//! exit code, so integration tests can call `kilat::cli::run` in-process.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(kilat::cli::run(&args));
}
