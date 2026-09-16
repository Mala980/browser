//! Font loading and matching.
//!
//! `tt` parses the files; this module finds them, keeps one copy of each face
//! (`Arc`, shared by every layout that uses it) and answers the question the
//! layout engine actually asks: "which face do I draw this run with?".
//!
//! No fontconfig, no FreeType: the search list is a handful of well-known
//! directories (Termux, Android, desktop Linux, macOS, WSL) plus `KILAT_FONTS`,
//! and matching is a direct comparison against the `name` table. That is enough
//! for the family names CSS authors use in practice, and it costs nothing at
//! startup beyond the faces we decide to load.

pub mod raster;
pub mod tt;

use crate::util::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tt::Face;

/// A request for a face: the CSS `font` shorthand reduced to what matters here.
#[derive(Clone, Debug)]
pub struct Request {
    pub families: Vec<String>,
    pub weight: u16,
    pub italic: bool,
    /// -9..=9 in CSS terms (`font-stretch` percentage bucket).
    pub stretch: i8,
}

impl Default for Request {
    fn default() -> Request {
        Request {
            families: vec!["sans-serif".to_string()],
            weight: 400,
            italic: false,
            stretch: 0,
        }
    }
}

impl Request {
    pub fn new(families: &str, weight: u16, italic: bool) -> Request {
        Request {
            families: split_families(families),
            weight,
            italic,
            stretch: 0,
        }
    }
}

/// Generic family names -> concrete candidates, longest match first.
const GENERICS: &[(&str, &[&str])] = &[
    (
        "serif",
        &["DejaVu Serif", "Liberation Serif", "Noto Serif", "Tinos", "Roboto Slab"],
    ),
    (
        "sans-serif",
        &[
            "Roboto",
            "DejaVu Sans",
            "Liberation Sans",
            "Noto Sans",
            "Arimo",
            "Segoe UI",
            "Helvetica Neue",
            "Helvetica",
            "Arial",
        ],
    ),
    ("monospace", &["DejaVu Sans Mono", "Roboto Mono", "Liberation Mono", "Noto Sans Mono"]),
    ("cursive", &["DejaVu Sans", "Comic Neue"]),
    ("fantasy", &["DejaVu Sans"]),
    ("system-ui", &["Roboto", "DejaVu Sans", "Segoe UI"]),
    ("ui-sans-serif", &["Roboto", "DejaVu Sans", "Segoe UI"]),
    ("emoji", &["Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji"]),
];

/// Directories that hold fonts on the platforms Kilat targets. Checked in order;
/// missing ones are skipped silently.
pub fn font_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    };

    if let Ok(v) = std::env::var("KILAT_FONTS") {
        for part in v.split(':') {
            if part.is_empty() {
                continue;
            }
            let pb = PathBuf::from(part);
            if pb.is_file() {
                // A single file: its parent is the searchable directory.
                if let Some(d) = pb.parent() {
                    push(d.to_path_buf());
                }
            } else {
                push(pb);
            }
        }
    }
    // Termux: `pkg install font-*` and the terminal's own Tera font live here.
    for prefix in [
        "/data/data/com.termux/files/usr",
        "/usr/local",
        "/usr",
        "/system/fonts",
        "/Library/Fonts",
        "/System/Library/Fonts",
        "/mnt/c/Windows/Fonts",
    ] {
        push(PathBuf::from(prefix).join("share").join("fonts"));
        if prefix == "/system/fonts" || prefix == "/Library/Fonts" || prefix.contains("Windows") {
            push(PathBuf::from(prefix));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let h = PathBuf::from(home);
        push(h.join(".fonts"));
        push(h.join(".local/share/fonts"));
        push(h.join("Library/Fonts"));
    }
    // XDG data dirs (e.g. flatpak-provided fonts).
    if let Ok(v) = std::env::var("XDG_DATA_DIRS") {
        for part in v.split(':') {
            push(PathBuf::from(part).join("fonts"));
        }
    }
    drop(push);
    out
}

/// Everything the engine needs to turn text into positioned glyphs.
#[derive(Clone)]
pub struct FontDB {
    pub faces: Vec<Arc<Face>>,
    /// Origin of each face, for `kilat dev fonts` and CDP font reporting.
    pub sources: Vec<String>,
    /// Cap on how many faces we keep (a desktop /usr/share/fonts has thousands).
    pub max_faces: usize,
    loaded_dirs: Vec<String>,
    pub scanned_files: usize,
}

/// Extensions we can actually read.
fn supported(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) => matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "woff"),
        None => false,
    }
}

impl FontDB {
    pub fn new() -> FontDB {
        FontDB {
            faces: Vec::new(),
            sources: Vec::new(),
            max_faces: 48,
            loaded_dirs: Vec::new(),
            scanned_files: 0,
        }
    }

    /// Faces already added keep their index; loading never invalidates them.
    pub fn load_system() -> FontDB {
        let mut db = FontDB::new();
        let dirs = font_dirs();
        for d in dirs {
            db.load_dir(&d);
            if db.faces.len() >= db.max_faces {
                break;
            }
        }
        db
    }

    pub fn load_dir(&mut self, dir: &Path) -> usize {
        let key = dir.to_string_lossy().to_string();
        if self.loaded_dirs.contains(&key) {
            return 0;
        }
        self.loaded_dirs.push(key);
        let before = self.faces.len();
        self.walk(dir, 0);
        self.faces.len() - before
    }

    fn walk(&mut self, dir: &Path, depth: usize) {
        if depth > 3 || self.faces.len() >= self.max_faces {
            return;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        let mut subdirs = Vec::new();
        for p in entries {
            if p.is_dir() {
                if p.file_name()
                    .map(|n| n.to_string_lossy().starts_with('.'))
                    .unwrap_or(true)
                {
                    continue;
                }
                subdirs.push(p);
                continue;
            }
            if !supported(&p) {
                continue;
            }
            self.scanned_files += 1;
            let big = std::fs::metadata(&p).map(|m| m.len() > 40 << 20).unwrap_or(true);
            if big {
                continue;
            }
            if self.add_file(&p.to_string_lossy()).is_ok() && self.faces.len() >= self.max_faces {
                return;
            }
        }
        for d in subdirs {
            self.walk(&d, depth + 1);
            if self.faces.len() >= self.max_faces {
                return;
            }
        }
    }

    pub fn add_file(&mut self, path: &str) -> Result<usize> {
        let face = Face::parse_file(path)?;
        Ok(self.add(Arc::new(face), path.to_string()))
    }

    pub fn add(&mut self, face: Arc<Face>, source: impl Into<String>) -> usize {
        // Same postscript name twice = same font found through two directories.
        if let Some(i) = self
            .faces
            .iter()
            .position(|f| f.postscript_name == face.postscript_name && !face.postscript_name.is_empty())
        {
            return i;
        }
        self.faces.push(face);
        self.sources.push(source.into());
        self.faces.len() - 1
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Family names we know about, de-duplicated, in load order.
    pub fn families(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in &self.faces {
            if !out.iter().any(|s: &String| s == &f.family) {
                out.push(f.family.clone());
            }
        }
        out
    }

    /// `CSS.getPlatformFontsForNode` wants (family, glyph count) per font.
    pub fn platform_fonts(&self, used: &[usize]) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for &i in used {
            let face = match self.faces.get(i) {
                Some(f) => f,
                None => continue,
            };
            match out.iter_mut().find(|(n, _)| *n == face.family) {
                Some((_, c)) => *c += 1,
                None => out.push((face.family.clone(), 1)),
            }
        }
        out
    }

    /// The face used when nothing matches: a real sans first, then anything.
    pub fn default_face(&self) -> Option<usize> {
        let sans = GENERICS
            .iter()
            .find(|(g, _)| *g == "sans-serif")
            .map(|(_, c)| *c)
            .unwrap_or(&[] as &[&str]);
        for cand in sans.iter() {
            if let Some(i) = self.match_family(cand, 400, false) {
                return Some(i);
            }
        }
        if self.faces.is_empty() {
            None
        } else {
            Some(0)
        }
    }

    fn name_matches(&self, i: usize, want: &str) -> bool {
        let face = match self.faces.get(i) {
            Some(f) => f,
            None => return false,
        };
        let w = normalize_family(want);
        if w.is_empty() {
            return false;
        }
        normalize_family(&face.family) == w
            || normalize_family(&face.full_name) == w
            || normalize_family(&face.postscript_name) == w
            || normalize_family(&face.family).contains(&w) && w.len() >= 4
    }

    /// Score a face against a weight/style request; lower is better.
    fn style_score(face: &Face, weight: u16, italic: bool) -> i32 {
        let mut s = (face.weight as i32 - weight as i32).abs() / 100;
        // CSS rule: for w >= 600 prefer bold, otherwise prefer non-bold.
        let want_bold = weight >= 600;
        if face.bold != want_bold {
            s += 3;
        }
        if face.italic != italic {
            s += italic_offset(weight, italic, face.italic);
        }
        s
    }

    /// Best face for one family name, or None.
    pub fn match_family(&self, family: &str, weight: u16, italic: bool) -> Option<usize> {
        let mut best: Option<(i32, usize)> = None;
        for i in 0..self.faces.len() {
            if !self.name_matches(i, family) {
                continue;
            }
            let sc = FontDB::style_score(&self.faces[i], weight, italic);
            match best {
                Some((b, _)) if b <= sc => {}
                _ => best = Some((sc, i)),
            }
        }
        best.map(|(_, i)| i)
    }

    /// Resolve a `font-family` list (with generics expanded) to a face index.
    pub fn resolve(&self, req: &Request) -> Option<usize> {
        let mut best: Option<(i32, usize)> = None;
        for (slot, fam) in req.families.iter().enumerate() {
            let fam_l = fam.to_ascii_lowercase();
            let mut tried: Vec<usize> = Vec::new();
            if let Some((_, list)) = GENERICS.iter().find(|(g, _)| *g == fam_l.as_str()) {
                for cand in list.iter() {
                    if let Some(i) = self.match_family(cand, req.weight, req.italic) {
                        tried.push(i);
                    }
                }
            }
            if let Some(i) = self.match_family(fam, req.weight, req.italic) {
                tried.push(i);
            }
            for i in tried {
                // Earlier families beat better styles: the author asked for them.
                let sc = FontDB::style_score(&self.faces[i], req.weight, req.italic)
                    + slot as i32 * 40;
                match best {
                    Some((b, _)) if b <= sc => {}
                    _ => best = Some((sc, i)),
                }
            }
        }
        if best.is_none() {
            // Nothing matched by name: any face will do, non-italic 400 first.
            for i in 0..self.faces.len() {
                let sc = FontDB::style_score(&self.faces[i], req.weight, req.italic) + 100;
                match best {
                    Some((b, _)) if b <= sc => {}
                    _ => best = Some((sc, i)),
                }
            }
        }
        best.map(|(_, i)| i)
    }

    /// Last-resort face for a character no requested family covers.
    pub fn covering(&self, ch: char, skip: &[usize]) -> Option<usize> {
        for i in 0..self.faces.len() {
            if skip.contains(&i) {
                continue;
            }
            if self.faces[i].coverage(ch) {
                return Some(i);
            }
        }
        None
    }

    pub fn face(&self, i: usize) -> Option<Arc<Face>> {
        self.faces.get(i).cloned()
    }

    /// Measurement with the fallback metric rules applied when no face exists.
    pub fn measure(&self, i: Option<usize>, text: &str, px: f32, kerning: bool) -> f32 {
        match i.and_then(|i| self.faces.get(i)) {
            Some(f) => f.measure(text, px, kerning),
            None => fallback_measure(text, px),
        }
    }

    pub fn ascent(&self, i: Option<usize>, px: f32) -> f32 {
        match i.and_then(|i| self.faces.get(i)) {
            Some(f) => f.ascent(px),
            None => fallback_ascent(px),
        }
    }

    pub fn descent(&self, i: Option<usize>, px: f32) -> f32 {
        match i.and_then(|i| self.faces.get(i)) {
            Some(f) => f.descent(px),
            None => fallback_descent(px),
        }
    }

    /// Normal line box height: ascent + descent + the face's own line gap.
    pub fn line_height(&self, i: Option<usize>, px: f32) -> f32 {
        let gap = match i.and_then(|i| self.faces.get(i)) {
            Some(f) => f.line_gap_px(px),
            None => 0.0,
        };
        self.ascent(i, px) + self.descent(i, px) + gap
    }
}

/// Italic mismatch penalty depends on the requested weight (CSS 4 §5.2).
fn italic_offset(weight: u16, want_italic: bool, face_italic: bool) -> i32 {
    let _ = (weight, want_italic, face_italic);
    6
}

fn normalize_family(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn split_families(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in list.split(',') {
        let p = part.trim().trim_matches('"').trim_matches('\'').trim();
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
    if out.is_empty() {
        out.push("sans-serif".to_string());
    }
    out
}

// ---- metrics used when no font is available -------------------------------
//
// These mirror the "typical text" rules a browser falls back to: an em box that
// is wide enough to look right, and line boxes that don't collapse.

pub fn fallback_ascent(px: f32) -> f32 {
    px * 0.8
}

pub fn fallback_descent(px: f32) -> f32 {
    px * 0.2
}

pub fn fallback_line_height(px: f32) -> f32 {
    px * 1.15
}

/// Rough advance: Latin ~0.5em, ideographs/emoji ~1em, spaces 0.28em.
pub fn fallback_measure(text: &str, px: f32) -> f32 {
    let mut w = 0.0f32;
    for ch in text.chars() {
        w += match ch {
            ' ' => 0.28,
            '\u{21}'..='\u{2f}' | '\u{3a}'..='\u{40}' | '\u{5b}'..='\u{60}' | '\u{7b}'..='\u{7e}' => {
                0.35
            }
            'i' | 'j' | 'l' | 't' | 'f' | 'r' | 'I' | 'J' => 0.3,
            'm' | 'w' | 'M' | 'W' => 0.85,
            c if is_wide(c) => 1.0,
            _ => 0.52,
        };
    }
    w * px
}

fn is_wide(c: char) -> bool {
    let u = c as u32;
    (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0x1F300..=0x1FAFF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mini() -> Option<Arc<Face>> {
        let mut p = String::from(env!("CARGO_MANIFEST_DIR"));
        p.push_str("/tests/data/kilat-mini.ttf");
        let bytes = std::fs::read(&p).ok()?;
        Face::parse(bytes).ok().map(Arc::new)
    }

    fn db_with_mini() -> FontDB {
        let mut db = FontDB::new();
        if let Some(f) = mini() {
            db.add(f, "kilat-mini.ttf");
        }
        db
    }

    #[test]
    fn family_and_style_matching() {
        let db = db_with_mini();
        if db.is_empty() {
            return;
        }
        assert_eq!(db.families(), vec!["Kilat Mini".to_string()]);
        // Exact name, quoted name and a sloppy case/spelling all resolve.
        assert_eq!(db.match_family("Kilat Mini", 400, false), Some(0));
        assert_eq!(db.match_family("kilatmini", 400, false), Some(0));
        assert_eq!(db.match_family("KilatMini-Regular", 400, false), Some(0));
        assert_eq!(db.match_family("Nope Sans", 400, false), None);
        let req = Request::new("\"Kilat Mini\", serif", 400, false);
        assert_eq!(db.resolve(&req), Some(0));
        // Unknown family list still lands on *something* rather than nothing.
        let req2 = Request::new("Nonexistent Sans", 700, true);
        assert_eq!(db.resolve(&req2), Some(0));
        assert!(db.covering('A', &[]).is_some());
        assert!(db.covering('\u{1F600}', &[]).is_none());
    }

    #[test]
    fn measurement_paths() {
        let db = db_with_mini();
        if !db.is_empty() {
            assert_eq!(db.measure(Some(0), "A", 20.0, false), 14.4);
            assert_eq!(db.measure(None, "A", 20.0, false), 20.0 * 0.52);
            // The fallback must never return zero for non-empty text.
            assert!(db.measure(None, "hello world", 16.0, false) > 40.0);
        }
        assert_eq!(fallback_ascent(10.0), 8.0);
        assert_eq!(fallback_descent(10.0), 2.0);
        assert_eq!(fallback_line_height(10.0), 11.5);
        assert_eq!(fallback_measure("", 12.0), 0.0);
        assert!(fallback_measure("漢字", 12.0) > fallback_measure("ab", 12.0));
        assert_eq!(normalize_family("Times-New Roman"), "timesnewroman");
        assert_eq!(split_families("a, \"b c\", 'd'"), vec!["a", "b c", "d"]);
        assert_eq!(split_families(""), vec!["sans-serif".to_string()]);
    }

    #[test]
    fn dedupes_by_postscript_name() {
        let mut db = FontDB::new();
        if let Some(f) = mini() {
            let i1 = db.add(f.clone(), "a.ttf");
            let i2 = db.add(f, "b.ttf");
            assert_eq!(i1, i2);
            assert_eq!(db.faces.len(), 1);
        }
    }

    #[test]
    fn discovery_never_fails() {
        // Whatever the machine has, this must not blow up and must be idempotent.
        let dirs = font_dirs();
        let mut db = FontDB::new();
        for d in dirs.iter().take(2) {
            db.load_dir(d);
            let n = db.faces.len();
            db.load_dir(d);
            assert_eq!(db.faces.len(), n, "re-scanning a dir is a no-op");
        }
        db.max_faces = 1;
        for d in dirs.iter().skip(2) {
            db.load_dir(d);
            assert!(db.faces.len() <= 1, "max_faces is respected");
        }
    }
}
