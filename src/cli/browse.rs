//! Headless / windowed browsing entry point.
//!
//! Placeholder while the engine modules land: navigation, the CDP server and the
//! live-view window are wired up here (see docs/ARCHITECTURE.md).
#![allow(dead_code)]

use crate::util::Result;

pub fn browse(args: &[String]) -> Result<i32> {
    let url = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_default();
    Err(format!(
        "navigation is not available in this build{}",
        if url.is_empty() {
            String::new()
        } else {
            format!(" (requested {url})")
        }
    ))
}
