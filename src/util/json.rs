//! A tiny JSON reader/writer.
//!
//! CDP is a JSON protocol and the whole crate avoids dependencies, so we need
//! our own. Objects keep insertion order (`Vec<(String, Json)>`) which matters
//! for stable `--dump-json` output and for diffing CDP payloads in tests.

use crate::util::Result;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Object with ordered keys.
    Obj(Vec<(String, Json)>),
}

impl Default for Json {
    fn default() -> Json {
        Json::Null
    }
}

impl Json {
    pub fn object(fields: Vec<(&str, Json)>) -> Json {
        let mut out = Vec::with_capacity(fields.len());
        for (k, v) in fields {
            out.push((k.to_string(), v));
        }
        Json::Obj(out)
    }

    pub fn string_array(values: &[&str]) -> Json {
        Json::Arr(values.iter().map(|v| Json::Str(v.to_string())).collect())
    }

    pub fn s<T: Into<String>>(v: T) -> Json {
        Json::Str(v.into())
    }
    pub fn n(v: f64) -> Json {
        Json::Num(v)
    }
    pub fn i(v: i64) -> Json {
        Json::Num(v as f64)
    }
    pub fn u(v: usize) -> Json {
        Json::Num(v as f64)
    }
    pub fn b(v: bool) -> Json {
        Json::Bool(v)
    }
    pub fn arr(v: Vec<Json>) -> Json {
        Json::Arr(v)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Num(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }

    pub fn get<'a>(&'a self, key: &str) -> Option<&'a Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Num(n) => Some(*n != 0.0),
            Json::Str(s) => match s.as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            Json::Str(s) => s.trim().parse::<f64>().ok(),
            Json::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|v| v as i64)
    }

    pub fn as_usize(&self) -> Option<usize> {
        self.as_f64().and_then(|v| {
            if v >= 0.0 && v <= usize::MAX as f64 {
                Some(v as usize)
            } else {
                None
            }
        })
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// `key` as a string, defaulting to `""` for null/missing values.
    pub fn str_or_empty(&self, key: &str) -> String {
        match self.get(key) {
            Some(Json::Str(s)) => s.clone(),
            Some(Json::Num(n)) => format_number(*n),
            Some(Json::Bool(b)) => b.to_string(),
            _ => String::new(),
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Vec<(String, Json)>> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    /// Deep-ish helper: `{"a":{"b":1}}.dig(&["a","b"])`.
    pub fn dig(&self, path: &[&str]) -> Option<&Json> {
        let mut cur = self;
        for p in path {
            cur = cur.get(p)?;
        }
        Some(cur)
    }

    /// Insert or overwrite a key; turns non-objects into objects.
    pub fn set(&mut self, key: &str, value: Json) {
        if !matches!(self, Json::Obj(_)) {
            *self = Json::Obj(Vec::new());
        }
        if let Json::Obj(fields) = self {
            for slot in fields.iter_mut() {
                if slot.0 == key {
                    slot.1 = value;
                    return;
                }
            }
            fields.push((key.to_string(), value));
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Json> {
        if let Json::Obj(fields) = self {
            if let Some(i) = fields.iter().position(|(k, _)| k == key) {
                return Some(fields.remove(i).1);
            }
        }
        None
    }

    pub fn push(&mut self, value: Json) {
        if !matches!(self, Json::Arr(_)) {
            *self = Json::Arr(Vec::new());
        }
        if let Json::Arr(a) = self {
            a.push(value);
        }
    }

    pub fn to_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, None, 0);
        out
    }

    /// Pretty printed, used by `--dump-dom` style tooling and tests.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, Some(2), 0);
        out
    }

    fn write(&self, out: &mut String, indent: Option<usize>, depth: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => out.push_str(&format_number(*n)),
            Json::Str(s) => write_escaped(out, s),
            Json::Arr(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline_indent(out, indent, depth + 1);
                    it.write(out, indent, depth + 1);
                }
                newline_indent(out, indent, depth);
                out.push(']');
            }
            Json::Obj(fields) => {
                if fields.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline_indent(out, indent, depth + 1);
                    write_escaped(out, k);
                    out.push(':');
                    if indent.is_none() {
                        out.push(' ');
                    }
                    v.write(out, indent, depth + 1);
                }
                newline_indent(out, indent, depth);
                out.push('}');
            }
        }
    }

    /// Parse a JSON document. Trailing content other than whitespace is an error.
    pub fn parse(text: &str) -> Result<Json> {
        let bytes = text.as_bytes();
        let mut p = Parser { b: bytes, i: 0 };
        p.skip_ws();
        let v = p.value(0)?;
        p.skip_ws();
        if p.i != bytes.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }

    pub fn parse_or_null(text: &str) -> Json {
        Json::parse(text).unwrap_or(Json::Null)
    }
}

fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "null".to_string();
    }
    if n == n.trunc() && n.abs() < 9.007_199_254_740_992e15 {
        let mut s = format!("{}", n as i64);
        if s.ends_with(".0") {
            s.truncate(s.len() - 2);
        }
        s
    } else {
        format!("{}", n)
    }
}

fn newline_indent(out: &mut String, indent: Option<usize>, depth: usize) {
    if let Some(step) = indent {
        out.push('\n');
        for _ in 0..step * depth {
            out.push(' ');
        }
    }
}

fn write_escaped(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, msg: &str) -> Result<T> {
        Err(format!("json: {msg} at byte {}", self.i))
    }

    fn skip_ws(&mut self) {
        while self.i < self.b.len() {
            match self.b[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json> {
        if depth > 128 {
            return self.err("nesting too deep");
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => self.err("expected a value"),
        }
    }

    fn literal(&mut self, word: &str) -> Result<()> {
        if self.b.len() - self.i >= word.len() && &self.b[self.i..self.i + word.len()] == word.as_bytes()
        {
            self.i += word.len();
            Ok(())
        } else {
            self.err("bad literal")
        }
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.i;
        self.eat(b'-');
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-' {
                // A sign is only valid right after e/E; being lenient here is fine
                // because we re-parse the slice with str::parse.
                self.i += 1;
            } else {
                break;
            }
        }
        let txt = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number".to_string())?;
        match txt.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(Json::Num(v)),
            _ => self.err("bad number"),
        }
    }

    fn string(&mut self) -> Result<String> {
        if !self.eat(b'"') {
            return self.err("expected string");
        }
        let mut out = String::new();
        loop {
            if self.i >= self.b.len() {
                return self.err("unterminated string");
            }
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let esc = self.peek().ok_or_else(|| "bad escape".to_string())?;
                    self.i += 1;
                    match esc {
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'u' => {
                            let cp = self.hex4()?;
                            if (0xd800..0xdc00).contains(&cp) {
                                // surrogate pair
                                if self.peek() == Some(b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xdc00..0xe000).contains(&lo) {
                                        let c =
                                            0x1_0000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                                        out.push(
                                            char::from_u32(c).unwrap_or('\u{fffd}'),
                                        );
                                        continue;
                                    }
                                }
                                out.push('\u{fffd}');
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                        }
                        _ => return self.err("bad escape"),
                    }
                }
                _ => {
                    // Copy the raw UTF-8 sequence so multi-byte chars survive.
                    let start = self.i;
                    let len = utf8_len(c);
                    self.i = (self.i + len).min(self.b.len());
                    match std::str::from_utf8(&self.b[start..self.i]) {
                        Ok(s) => out.push_str(s),
                        Err(_) => out.push('\u{fffd}'),
                    }
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        if self.i + 4 > self.b.len() {
            return self.err("bad \\u escape");
        }
        let mut v: u32 = 0;
        for k in 0..4 {
            let c = self.b[self.i + k];
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return self.err("bad hex digit"),
            };
            v = v * 16 + d;
        }
        self.i += 4;
        Ok(v)
    }

    fn array(&mut self, depth: usize) -> Result<Json> {
        self.eat(b'[');
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(b']') {
                return Ok(Json::Arr(items));
            }
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Ok(Json::Arr(items));
            }
            return self.err("expected , or ]");
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json> {
        self.eat(b'{');
        let mut fields: Vec<(String, Json)> = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(b'}') {
                return Ok(Json::Obj(fields));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(b':') {
                return self.err("expected :");
            }
            let value = self.value(depth + 1)?;
            match fields.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => fields.push((key, value)),
            }
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Ok(Json::Obj(fields));
            }
            return self.err("expected , or }");
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a":1,"b":[true,null,"xé"],"c":{"d":-2.5},"e":"line\nbreak\t\"q\"\\"}"#;
        let j = Json::parse(src).unwrap();
        assert_eq!(j.get("a").unwrap().as_i64(), Some(1));
        assert_eq!(j.dig(&["c", "d"]).unwrap().as_f64(), Some(-2.5));
        let arr = j.get("b").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[2].as_str(), Some("xé"));
        let again = Json::parse(&j.to_string()).unwrap();
        assert_eq!(again, j);
    }

    #[test]
    fn escapes_and_unicode() {
        let j = Json::parse(r#"{"k":"\u0041\u00e9\ud83d\ude00"}"#).unwrap();
        assert_eq!(j.get("k").unwrap().as_str().unwrap(), "Aé😀");
        let s = Json::s("a\"b\u{1}").to_string();
        assert_eq!(s, "\"a\\\"b\\u0001\"");
    }

    #[test]
    fn numbers() {
        assert_eq!(Json::parse("3.0").unwrap().to_string(), "3");
        assert_eq!(Json::parse("1e3").unwrap().to_string(), "1000");
        assert_eq!(Json::parse("-0.25").unwrap().to_string(), "-0.25");
    }

    #[test]
    fn errors() {
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("[1,]").is_err());
        assert!(Json::parse("\"unterminated").is_err());
        assert!(Json::parse("{} trailing").is_err());
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(Json::parse(&deep).is_err());
    }

    #[test]
    fn mutation() {
        let mut o = Json::object(vec![("id", Json::i(1))]);
        o.set("result", Json::object(vec![]));
        o.set("id", Json::i(7));
        assert_eq!(o.to_string(), r#"{"id": 7, "result": {}}"#);
        assert_eq!(o.remove("id").unwrap(), Json::i(7));
        let mut a = Json::arr(vec![]);
        a.push(Json::b(true));
        assert_eq!(a.to_string(), "[true]");
    }
}
