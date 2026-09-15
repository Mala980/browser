//! HTML character references (`&amp;`, `&#x2019;`, `&nbsp;`, ...).
//!
//! A curated named table (the entities that actually appear in documents) plus
//! full numeric support, including the historical Windows-1252 remapping of
//! `&#128;`..`&#159;` that the HTML spec requires every engine to implement.

use std::sync::OnceLock;

/// Named reference lookup. A sorted static table + binary search keeps the data
/// compact; `named()` wraps it so callers don't care.
pub fn named(name: &str) -> Option<char> {
    let table = ENTITIES.get_or_init(build);
    match table.binary_search_by(|(k, _)| k.as_str().cmp(name)) {
        Ok(i) => Some(table[i].1),
        Err(_) => None,
    }
}

/// Replace character references. `in_attr` follows the tokenizer's
/// attribute-value rule, where `&amp` without a semicolon stays literal.
pub fn decode(text: &str, in_attr: bool) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'&' {
            let start = i;
            while i < b.len() && b[i] != b'&' {
                i += 1;
            }
            out.push_str(&text[start..i]);
            continue;
        }
        let mut j = i + 1;
        if j < b.len() && b[j] == b'#' {
            j += 1;
            let hex = j < b.len() && (b[j] | 0x20) == b'x';
            if hex {
                j += 1;
            }
            let digits_start = j;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let has_semi = j < b.len() && b[j] == b';';
            // Text mode tolerates a missing semicolon; attribute mode does not.
            if j > digits_start && (has_semi || !in_attr) {
                let digits = &text[digits_start..j];
                if let Some(cp) = parse_int(digits, hex) {
                    out.push(decode_point(cp));
                    i = if has_semi { j + 1 } else { j };
                    continue;
                }
            }
        }
        // Named reference: with semicolon, or one of the legacy no-semicolon forms.
        let mut end = i + 1;
        while end < b.len() && b[end].is_ascii_alphanumeric() {
            end += 1;
        }
        if end < b.len() && b[end] == b';' {
            if let Some(c) = named(&text[i + 1..end]) {
                out.push(c);
                i = end + 1;
                continue;
            }
        } else if !in_attr {
            if let Some(c) = named_no_semi(&text[i + 1..end.min(b.len())]) {
                out.push(c);
                i = end;
                continue;
            }
        }
        out.push('&');
        i += 1;
    }
    out
}

fn parse_int(s: &str, hex: bool) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    if hex {
        u32::from_str_radix(s, 16).ok()
    } else {
        s.parse::<u32>().ok()
    }
}

/// C0/surrogate/out-of-range code points become U+FFFD; C1 maps through
/// Windows-1252 as the spec's "numeric character reference" table requires.
fn decode_point(cp: u32) -> char {
    const WIN1252: [char; 32] = [
        '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{fffd}',
        '\u{17d}', '\u{fffd}', '\u{fffd}', '\u{17e}', '\u{178}', '\u{161}', '\u{203a}', '\u{153}',
        '\u{fffd}', '\u{fffd}', '\u{2dc}', '\u{732}', '\u{2dd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
        '\u{fffd}', '\u{fffd}',
    ];
    if cp == 0 || cp > 0x10ffff || (0xd800..0xe000).contains(&cp) {
        return '\u{fffd}';
    }
    if (0x80..=0x9f).contains(&cp) {
        return WIN1252[(cp - 0x80) as usize];
    }
    char::from_u32(cp).unwrap_or('\u{fffd}')
}

fn named_no_semi(name: &str) -> Option<char> {
    match name {
        "amp" => Some('\u{26}'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        "copy" => Some('\u{a9}'),
        "reg" => Some('\u{ae}'),
        _ => None,
    }
}

/// Build the (name, char) table once and keep it sorted for binary search.
fn build() -> Vec<(&'static str, char)> {
    let mut v: Vec<(&'static str, char)> = RAW.to_vec();
    v.sort_unstable_by(|a, b| a.0.cmp(b.0));
    v.dedup_by_key(|e| e.0);
    v
}

/// The table itself: HTML5 named references that matter for text rendering.
const RAW: &[(&str, char)] = [
    ("AElig", '\u{c6}'),
    ("AMP", '\u{26}'),
    ("Aacute", '\u{c1}'),
    ("Acirc", '\u{c2}'),
    ("Agrave", '\u{c0}'),
    ("Aring", '\u{c5}'),
    ("Atilde", '\u{c3}'),
    ("Auml", '\u{c4}'),
    ("COPY", '\u{a9}'),
    ("Ccedil", '\u{c7}'),
    ("ETH", '\u{d0}'),
    ("Eacute", '\u{c9}'),
    ("Ecirc", '\u{ca}'),
    ("Egrave", '\u{c8}'),
    ("Euml", '\u{cb}'),
    ("GT", '>'),
    ("Iacute", '\u{cd}'),
    ("Icirc", '\u{ce}'),
    ("Igrave", '\u{cc}'),
    ("Iuml", '\u{ef}'),
    ("LT", '<'),
    ("Ntilde", '\u{d1}'),
    ("Oacute", '\u{d3}'),
    ("Ocirc", '\u{d4}'),
    ("Ograve", '\u{d2}'),
    ("Oslash", '\u{d8}'),
    ("Otilde", '\u{d5}'),
    ("Ouml", '\u{d6}'),
    ("QUOT", '"'),
    ("REG", '\u{ae}'),
    ("THORN", '\u{de}'),
    ("Uacute", '\u{da}'),
    ("Ucirc", '\u{db}'),
    ("Ugrave", '\u{d9}'),
    ("Uuml", '\u{dc}'),
    ("Yacute", '\u{dd}'),
    ("aacute", '\u{e1}'),
    ("acirc", '\u{e2}'),
    ("acute", '\u{b4}'),
    ("aelig", '\u{e6}'),
    ("agrave", '\u{e0}'),
    ("alefsym", '\u{2135}'),
    ("alpha", '\u{3b1}'),
    ("amp", '\u{26}'),
    ("apos", '\''),
    ("aring", '\u{e5}'),
    ("asymp", '\u{2248}'),
    ("atilde", '\u{e3}'),
    ("auml", '\u{e4}'),
    ("bdquo", '\u{201e}'),
    ("brvbar", '\u{a6}'),
    ("bull", '\u{2022}'),
    ("cap", '\u{2229}'),
    ("ccedil", '\u{e7}'),
    ("cedil", '\u{b8}'),
    ("cent", '\u{a2}'),
    ("chi", '\u{3c7}'),
    ("circ", '\u{2c6}'),
    ("clubs", '\u{2663}'),
    ("cong", '\u{2245}'),
    ("copy", '\u{a9}'),
    ("curren", '\u{a4}'),
    ("dArr", '\u{21d3}'),
    ("dagger", '\u{2020}'),
    ("darr", '\u{2193}'),
    ("deg", '\u{b0}'),
    ("delta", '\u{3b4}'),
    ("diams", '\u{2666}'),
    ("divide", '\u{f7}'),
    ("eacute", '\u{e9}'),
    ("ecirc", '\u{ea}'),
    ("egrave", '\u{e8}'),
    ("empty", '\u{2205}'),
    ("emsp", '\u{2003}'),
    ("ensp", '\u{2002}'),
    ("epsilon", '\u{3b5}'),
    ("equiv", '\u{2261}'),
    ("eta", '\u{3b7}'),
    ("eth", '\u{f0}'),
    ("euml", '\u{eb}'),
    ("euro", '\u{20ac}'),
    ("fnof", '\u{192}'),
    ("forall", '\u{2200}'),
    ("frac12", '\u{bd}'),
    ("frac14", '\u{bc}'),
    ("frac34", '\u{be}'),
    ("gt", '>'),
    ("hArr", '\u{21d4}'),
    ("harr", '\u{2194}'),
    ("hearts", '\u{2665}'),
    ("hellip", '\u{2026}'),
    ("iacute", '\u{ed}'),
    ("icirc", '\u{ee}'),
    ("iexcl", '\u{a1}'),
    ("igrave", '\u{ec}'),
    ("infin", '\u{221e}'),
    ("int", '\u{222b}'),
    ("iota", '\u{3b9}'),
    ("iquest", '\u{bf}'),
    ("isin", '\u{2208}'),
    ("iuml", '\u{ef}'),
    ("kappa", '\u{3ba}'),
    ("lArr", '\u{21d0}'),
    ("lambda", '\u{3bb}'),
    ("lang", '\u{27e8}'),
    ("laquo", '\u{ab}'),
    ("larr", '\u{2190}'),
    ("lceil", '\u{2308}'),
    ("ldquo", '\u{201c}'),
    ("le", '\u{2264}'),
    ("lfloor", '\u{230a}'),
    ("lowast", '\u{2217}'),
    ("loz", '\u{25ca}'),
    ("lrm", '\u{200e}'),
    ("lsaquo", '\u{2039}'),
    ("lsquo", '\u{2018}'),
    ("lt", '<'),
    ("macr", '\u{af}'),
    ("mdash", '\u{2014}'),
    ("micro", '\u{b5}'),
    ("middot", '\u{b7}'),
    ("minus", '\u{2212}'),
    ("mu", '\u{3bc}'),
    ("nabla", '\u{2207}'),
    ("nbsp", '\u{a0}'),
    ("ndash", '\u{2013}'),
    ("ne", '\u{2260}'),
    ("ni", '\u{220b}'),
    ("not", '\u{ac}'),
    ("notin", '\u{2209}'),
    ("nsub", '\u{2284}'),
    ("ntilde", '\u{f1}'),
    ("nu", '\u{3bd}'),
    ("oacute", '\u{f3}'),
    ("ocirc", '\u{f4}'),
    ("oe", '\u{153}'),
    ("ograve", '\u{f2}'),
    ("oline", '\u{203e}'),
    ("omega", '\u{3c9}'),
    ("omicron", '\u{3bf}'),
    ("oplus", '\u{2295}'),
    ("or", '\u{2228}'),
    ("ordf", '\u{aa}'),
    ("ordm", '\u{ba}'),
    ("oslash", '\u{f8}'),
    ("otilde", '\u{f5}'),
    ("otimes", '\u{2297}'),
    ("ouml", '\u{f6}'),
    ("para", '\u{b6}'),
    ("permil", '\u{2030}'),
    ("perp", '\u{22a5}'),
    ("phi", '\u{3c6}'),
    ("pi", '\u{3c0}'),
    ("piv", '\u{3d6}'),
    ("plusmn", '\u{b1}'),
    ("pound", '\u{a3}'),
    ("prime", '\u{2032}'),
    ("Prime", '\u{2033}'),
    ("prod", '\u{220f}'),
    ("prop", '\u{221d}'),
    ("psi", '\u{3c8}'),
    ("quot", '"'),
    ("rArr", '\u{21d2}'),
    ("radic", '\u{221a}'),
    ("rang", '\u{27e9}'),
    ("raquo", '\u{bb}'),
    ("rarr", '\u{2192}'),
    ("rcaron", '\u{161}'),
    ("rceil", '\u{2309}'),
    ("rdquo", '\u{201d}'),
    ("real", '\u{211c}'),
    ("rfloor", '\u{230b}'),
    ("rho", '\u{3c1}'),
    ("rlm", '\u{200f}'),
    ("rsaquo", '\u{203a}'),
    ("rsquo", '\u{2019}'),
    ("sbquo", '\u{201a}'),
    ("scaron", '\u{161}'),
    ("sdot", '\u{22c5}'),
    ("sect", '\u{a7}'),
    ("shy", '\u{ad}'),
    ("sigma", '\u{3c3}'),
    ("sigmaf", '\u{3c2}'),
    ("sim", '\u{223c}'),
    ("spades", '\u{2660}'),
    ("sub", '\u{2282}'),
    ("sube", '\u{2286}'),
    ("sum", '\u{2211}'),
    ("sup", '\u{2283}'),
    ("sup1", '\u{b9}'),
    ("sup2", '\u{b2}'),
    ("sup3", '\u{b3}'),
    ("supe", '\u{2287}'),
    ("szlig", '\u{df}'),
    ("tau", '\u{3c4}'),
    ("there4", '\u{2234}'),
    ("theta", '\u{3b8}'),
    ("thinsp", '\u{2009}'),
    ("thorn", '\u{fe}'),
    ("tilde", '\u{2dc}'),
    ("times", '\u{d7}'),
    ("trade", '\u{2122}'),
    ("uArr", '\u{21d1}'),
    ("uacute", '\u{fa}'),
    ("uarr", '\u{2191}'),
    ("ucirc", '\u{fb}'),
    ("ugrave", '\u{f9}'),
    ("uml", '\u{a8}'),
    ("upsilon", '\u{3c5}'),
    ("uuml", '\u{fc}'),
    ("weierp", '\u{2118}'),
    ("xi", '\u{3be}'),
    ("yacute", '\u{fd}'),
    ("yen", '\u{a5}'),
    ("yuml", '\u{ff}'),
    ("zeta", '\u{3b6}'),
    ("zwj", '\u{200d}'),
    ("zwnj", '\u{200c}'),
];

static ENTITIES: OnceLock<Vec<(&'static str, char)>> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_and_numeric() {
        assert_eq!(decode("a &amp; b", false), "a & b");
        assert_eq!(decode("&lt;div&gt;", false), "<div>");
        assert_eq!(decode("&#65;&#x42;", false), "AB");
        assert_eq!(
            decode("&nbsp;&copy;&hellip;", false),
            "\u{a0}\u{a9}\u{2026}"
        );
        assert_eq!(decode("&#133;", false), "\u{2026}");
        assert_eq!(decode("&#0;", false), "\u{fffd}");
        assert_eq!(decode("&#xD800;", false), "\u{fffd}");
        assert_eq!(decode("AT&T", false), "AT&T");
        assert_eq!(decode("a&ampb", false), "a&b");
        assert_eq!(decode("a&ampb", true), "a&ampb");
        assert_eq!(decode("&unknownthing;", false), "&unknownthing;");
        assert_eq!(decode("100&#37;", false), "100%");
        assert_eq!(decode("no ampersands", false), "no ampersands");
        assert_eq!(named("Igrave"), Some('\u{cc}'));
        assert_eq!(named("Iuml"), Some('\u{ef}'));
        assert_eq!(named("amp"), Some('\u{26}'));
        assert_eq!(named("QUOT"), Some('"'));
        assert_eq!(named("nope"), None);
    }

    #[test]
    fn table_is_sorted_after_build() {
        let t = build();
        assert_eq!(t.len(), RAW.len(), "duplicate entity names in RAW");
        for w in t.windows(2) {
            assert!(w[0].0 < w[1].0, "unsorted: {} before {}", w[0].0, w[1].0);
        }
    }
}
