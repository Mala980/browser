//! # Kilat
//!
//! A small, bandwidth-frugal web browser engine with a Chrome DevTools Protocol
//! front end. Design references: Servo (Rust engine structure), lightpanda
//! (headless-but-actually-a-browser, CDP driven, tiny footprint) and Obscura
//! (Rust + CDP automation).
//!
//! Hard constraints that shape the whole crate:
//!
//! * **no third-party Rust crates** - `cargo build --offline` must work, so the
//!   identical source builds inside Termux on a phone. HTML, CSS, layout,
//!   rasterising, inflate, PNG/JPEG/GIF, TrueType rasterising, HTTP, the CDP
//!   WebSocket server and an MP4/WebM demuxer are all implemented here.
//! * **software rasterising** - a framebuffer of RGBA8 and a display list, no GPU
//!   or window-system dependency, so the same binary runs headless in CI, on a
//!   Termux session, or pushed to a live-view window.
//! * **spend as few bytes as possible** - keep-alive, conditional revalidation,
//!   gzip, lazy images, blocked third parties and HTTP range video metadata all
//!   sit behind one [`net::Policy`](net/policy).
//!
//! The crate is organised as a pipeline; each stage owns its own arena of plain
//! indexable structs, which is what keeps the borrow checker out of the way:
//!
//! ```text
//! net::fetch -> html::parse -> dom::Dom -> css::cascade -> layout::Builder
//!              -> paint::DisplayList -> paint::Framebuffer -> PNG/window/CDP
//! ```

pub mod cli;
pub mod codec;
pub mod css;
pub mod dom;
pub mod font;
pub mod html;
pub mod layout;
pub mod net;
pub mod util;

/// Version string baked in by `build.rs` (cargo version + short git sha).
pub const VERSION: &str = env!("KILAT_VERSION");
pub const TARGET: &str = env!("KILAT_TARGET");

pub fn version_string() -> String {
    format!("Kilat/{} ({})", VERSION, TARGET)
}
