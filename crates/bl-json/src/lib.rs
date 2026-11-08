//! Minimal JSON (RFC 8259) value type with a parser and serializer.
//!
//! Objects keep insertion order, which keeps files written by the
//! application stable and diff friendly.

#![forbid(unsafe_code)]

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};

/// Most keys one object of a parsed document may have.
pub const MAX_OBJECT_KEYS: usize = 1 << 20;
/// Most items one array of a parsed document may have.
pub const MAX_ARRAY_ITEMS: usize = 1 << 24;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Map),
}

/// An insertion ordered JSON object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Map {
    entries: Vec<(String, Value)>,
}

impl Map {
    pub fn new() -> Map {
        Map {
            entries: Vec::new(),
        }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
    /// Inserts or replaces a value, keeping the original position on replace.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        let value = value.into();
        if let Some(slot) = self.get_mut(&key) {
            *slot = value;
        } else {
            self.entries.push((key, value));
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(idx).1)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }
}

impl FromIterator<(String, Value)> for Map {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        let mut b = MapBuilder::new(RandomState::new());
        for (k, v) in iter {
            b.insert(k, v);
        }
        b.finish()
    }
}

/// Hashes a `u64` that already is a (keyed) hash: passes it through.
#[derive(Default)]
struct PassThrough(u64);

impl Hasher for PassThrough {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8) | b as u64;
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

/// Up to this many keys a [`MapBuilder`] looks for an equal key linearly.
const LINEAR_KEYS: usize = 16;

/// Builds a [`Map`] key by key in linear time: like [`Map::insert`] (a
/// repeated key keeps its first position and takes the last value), but
/// large objects find repeated keys through a hash index.
struct MapBuilder {
    entries: Vec<(String, Value)>,
    /// Keyed hash of a key -> the first entry with that hash; built once
    /// the object has more than [`LINEAR_KEYS`] keys.
    index: Option<HashMap<u64, usize, BuildHasherDefault<PassThrough>>>,
    hasher: RandomState,
}

impl MapBuilder {
    fn new(hasher: RandomState) -> MapBuilder {
        MapBuilder {
            entries: Vec::new(),
            index: None,
            hasher,
        }
    }

    /// Position of `key`, if present.
    fn find(&mut self, key: &str) -> Option<usize> {
        if self.index.is_none() {
            if self.entries.len() < LINEAR_KEYS {
                return self.entries.iter().position(|(k, _)| k == key);
            }
            let mut index = HashMap::default();
            for (i, (k, _)) in self.entries.iter().enumerate() {
                index.entry(self.hasher.hash_one(k.as_str())).or_insert(i);
            }
            self.index = Some(index);
        }
        let index = self.index.as_ref()?;
        let &i = index.get(&self.hasher.hash_one(key))?;
        if self.entries[i].0 == key {
            return Some(i);
        }
        // Two keys with the same 64-bit keyed hash: practically never, and
        // not something input can aim for. Correct anyway.
        self.entries.iter().position(|(k, _)| k == key)
    }

    /// Inserts or replaces; `false` (and nothing inserted) when a new key
    /// would make the object longer than `max` keys.
    fn insert_within(&mut self, key: String, value: Value, max: usize) -> bool {
        if let Some(i) = self.find(&key) {
            self.entries[i].1 = value;
            return true;
        }
        if self.entries.len() >= max {
            return false;
        }
        if let Some(index) = self.index.as_mut() {
            index
                .entry(self.hasher.hash_one(key.as_str()))
                .or_insert(self.entries.len());
        }
        self.entries.push((key, value));
        true
    }

    fn insert(&mut self, key: String, value: Value) {
        self.insert_within(key, value, usize::MAX);
    }

    fn finish(self) -> Map {
        Map {
            entries: self.entries,
        }
    }
}

static NULL: Value = Value::Null;

impl Value {
    pub fn object() -> Value {
        Value::Object(Map::new())
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|v| v as f32)
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 => {
                Some(*n as i64)
            }
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64().and_then(|v| u64::try_from(v).ok())
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }
    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }
    /// Object member lookup; returns `Null` for missing keys or non-objects.
    pub fn get(&self, key: &str) -> &Value {
        match self {
            Value::Object(o) => o.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    /// Array element lookup; returns `Null` when out of range.
    pub fn at(&self, idx: usize) -> &Value {
        match self {
            Value::Array(a) => a.get(idx).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    /// Sets an object member (turning `Null` into an object first).
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        if self.is_null() {
            *self = Value::object();
        }
        if let Value::Object(o) = self {
            o.insert(key, value);
        }
    }
    /// Builder style variant of [`Value::set`].
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Value {
        self.set(key, value);
        self
    }
    pub fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).as_str().unwrap_or(default)
    }
    pub fn f64_or(&self, key: &str, default: f64) -> f64 {
        self.get(key).as_f64().unwrap_or(default)
    }
    pub fn f32_or(&self, key: &str, default: f32) -> f32 {
        self.get(key).as_f32().unwrap_or(default)
    }
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).as_bool().unwrap_or(default)
    }

    /// Compact serialization.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, None, 0);
        s
    }

    /// Pretty serialization with two-space indentation.
    pub fn to_json_pretty(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, Some(2), 0);
        s.push('\n');
        s
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_json())
    }
}

macro_rules! from_num {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Value { Value::Number(v as f64) }
        }
    )*};
}
from_num!(f64, f32, i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::String(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Value {
        Value::String(v)
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Value {
        Value::String(v.clone())
    }
}
impl From<Map> for Value {
    fn from(v: Map) -> Value {
        Value::Object(v)
    }
}
impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Value {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Value {
        match v {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, *n),
        Value::String(s) => write_string(out, s),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, level + 1);
                write_value(out, item, indent, level + 1);
            }
            newline(out, indent, level);
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, level + 1);
                write_string(out, k);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(out, item, indent, level + 1);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

fn write_number(out: &mut String, n: f64) {
    if !n.is_finite() {
        out.push_str("null");
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        let _ = write!(out, "{}", n as i64);
    } else {
        // Rust's Display for f64 is the shortest round-trip representation.
        let _ = write!(out, "{n}");
    }
}

/// Writes a JSON string literal with escaping.
pub fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message, self.line, self.column
        )
    }
}

impl std::error::Error for Error {}

/// Parses a JSON document.
pub fn parse(text: &str) -> Result<Value, Error> {
    parse_within(text, MAX_OBJECT_KEYS, MAX_ARRAY_ITEMS)
}

fn parse_within(text: &str, max_keys: usize, max_items: usize) -> Result<Value, Error> {
    let mut p = Parser {
        s: text.as_bytes(),
        pos: 0,
        depth: 0,
        hasher: RandomState::new(),
        max_keys,
        max_items,
    };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.pos != p.s.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

/// Parses JSON from bytes (must be UTF-8; a BOM is skipped).
pub fn parse_bytes(bytes: &[u8]) -> Result<Value, Error> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => parse(s),
        Err(e) => Err(Error {
            message: format!("invalid utf-8: {e}"),
            line: 0,
            column: 0,
        }),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    depth: usize,
    /// Hashes keys of large objects (keyed per document, so input cannot
    /// pick colliding keys).
    hasher: RandomState,
    /// [`MAX_OBJECT_KEYS`] and [`MAX_ARRAY_ITEMS`] (smaller in tests).
    max_keys: usize,
    max_items: usize,
}

const MAX_DEPTH: usize = 256;

impl Parser<'_> {
    fn err(&self, msg: &str) -> Error {
        let upto = &self.s[..self.pos.min(self.s.len())];
        let line = upto.iter().filter(|&&b| b == b'\n').count() + 1;
        let column = upto.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
        Error {
            message: msg.to_string(),
            line,
            column,
        }
    }

    fn skip_ws(&mut self) {
        while let Some(&b) = self.s.get(self.pos) {
            if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.s[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Value, Error> {
        match self.s.get(self.pos) {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') if self.eat(b"true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat(b"false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat(b"null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Value, Error> {
        self.enter()?;
        self.pos += 1;
        let mut map = MapBuilder::new(self.hasher.clone());
        self.skip_ws();
        if self.eat(b"}") {
            self.depth -= 1;
            return Ok(Value::Object(map.finish()));
        }
        loop {
            self.skip_ws();
            if self.s.get(self.pos) != Some(&b'"') {
                return Err(self.err("expected object key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(b":") {
                return Err(self.err("expected ':'"));
            }
            self.skip_ws();
            let v = self.value()?;
            if !map.insert_within(key, v, self.max_keys) {
                return Err(self.err("too many keys in object"));
            }
            self.skip_ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"}") {
                break;
            }
            return Err(self.err("expected ',' or '}'"));
        }
        self.depth -= 1;
        Ok(Value::Object(map.finish()))
    }

    fn array(&mut self) -> Result<Value, Error> {
        self.enter()?;
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b"]") {
            self.depth -= 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            if items.len() >= self.max_items {
                return Err(self.err("too many items in array"));
            }
            items.push(self.value()?);
            self.skip_ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"]") {
                break;
            }
            return Err(self.err("expected ',' or ']'"));
        }
        self.depth -= 1;
        Ok(Value::Array(items))
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let h = self
            .s
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.err("truncated escape"))?;
        let s = std::str::from_utf8(h).map_err(|_| self.err("bad escape"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("bad unicode escape"))?;
        self.pos += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(&b) = self.s.get(self.pos) {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // Input is a &str (valid UTF-8) and we only stop at ASCII bytes,
            // so this slice is valid UTF-8.
            out.push_str(
                std::str::from_utf8(&self.s[start..self.pos])
                    .map_err(|_| self.err("invalid utf-8"))?,
            );
            match self.s.get(self.pos) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let e = *self
                        .s
                        .get(self.pos)
                        .ok_or_else(|| self.err("unterminated escape"))?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xd800..0xdc00).contains(&cp) {
                                if self.eat(b"\\u") {
                                    let lo = self.hex4()?;
                                    if (0xdc00..0xe000).contains(&lo) {
                                        cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                                    } else {
                                        cp = 0xfffd;
                                    }
                                } else {
                                    cp = 0xfffd;
                                }
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("invalid escape")),
                    }
                }
                Some(_) => return Err(self.err("control character in string")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        if self.s.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        let digits = |p: &mut Parser| {
            let s = p.pos;
            while matches!(p.s.get(p.pos), Some(b'0'..=b'9')) {
                p.pos += 1;
            }
            p.pos - s
        };
        if digits(self) == 0 {
            return Err(self.err("invalid number"));
        }
        if self.s.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if digits(self) == 0 {
                return Err(self.err("invalid number"));
            }
        }
        if matches!(self.s.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.s.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if digits(self) == 0 {
                return Err(self.err("invalid number"));
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap_or("0");
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.err("invalid number"))
    }
}

/// Convenience macro for building JSON objects: `json_obj!{"a" => 1, "b" => "x"}`.
#[macro_export]
macro_rules! json_obj {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = $crate::Map::new();
        $( m.insert($k, $v); )*
        $crate::Value::Object(m)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a":1,"b":[true,false,null],"c":"x\"y\\z\nä😀","d":-1.5e3,"e":{}}"#;
        let v = parse(src).unwrap();
        assert_eq!(v.get("a").as_i64(), Some(1));
        assert_eq!(v.get("c").as_str(), Some("x\"y\\z\nä😀"));
        assert_eq!(v.get("d").as_f64(), Some(-1500.0));
        let again = parse(&v.to_json()).unwrap();
        assert_eq!(v, again);
        let pretty = parse(&v.to_json_pretty()).unwrap();
        assert_eq!(v, pretty);
    }

    #[test]
    fn order_preserved() {
        let v = parse(r#"{"z":1,"a":2,"m":3}"#).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }

    #[test]
    fn errors() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "01x",
            "\"abc",
            "tru",
            "[1 2]",
            "{\"a\":1,}",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = "[".repeat(1000);
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn repeated_keys_keep_the_first_place_and_the_last_value() {
        let v = parse(r#"{"a":1,"b":2,"a":3}"#).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["a", "b"]);
        assert_eq!(v.get("a").as_i64(), Some(3));
        // The same in an object large enough for the hash index, compared
        // with inserting one by one.
        let mut text = String::from("{");
        let mut expected = Map::new();
        for i in 0..2000u32 {
            let k = format!("k{}", (i * 7919) % 613);
            text += &format!("\"{k}\":{i},");
            expected.insert(k, i);
        }
        text.pop();
        text.push('}');
        let v = parse(&text).unwrap();
        assert_eq!(v, Value::Object(expected.clone()));
        let pairs: Vec<(String, Value)> = (0..2000u32)
            .map(|i| (format!("k{}", (i * 7919) % 613), Value::from(i)))
            .collect();
        assert_eq!(pairs.into_iter().collect::<Map>(), expected);
    }

    #[test]
    fn objects_with_many_keys_parse_in_linear_time() {
        let n = 400_000;
        let mut text = String::with_capacity(n * 16);
        text.push('{');
        for i in 0..n {
            text += &format!("\"key{i}\":{i},");
        }
        // A repeated key at the end must still be found.
        text += "\"key17\":-1}";
        let t = std::time::Instant::now();
        let v = parse(&text).unwrap();
        let took = t.elapsed();
        let m = v.as_object().unwrap();
        assert_eq!(m.len(), n);
        assert_eq!(m.get("key17").and_then(Value::as_i64), Some(-1));
        assert_eq!(m.keys().nth(n - 1), Some("key399999"));
        println!("{n} keys parsed in {took:?}");
        // Quadratic insertion took minutes; linear is about a second even
        // unoptimised.
        assert!(took.as_secs() < 20, "{n} keys took {took:?}");
    }

    #[test]
    fn key_and_item_limits() {
        assert!(parse_within(r#"{"a":1,"b":2,"a":3}"#, 2, 10).is_ok());
        let e = parse_within(r#"{"a":1,"b":2,"c":3}"#, 2, 10).unwrap_err();
        assert_eq!(e.message, "too many keys in object");
        assert!(parse_within("[1,2,3]", 2, 3).is_ok());
        let e = parse_within("[1,2,3,4]", 2, 3).unwrap_err();
        assert_eq!(e.message, "too many items in array");
        // The real key limit.
        let mut text = String::from("{");
        for i in 0..=MAX_OBJECT_KEYS {
            text += &format!("\"{i:x}\":0,");
        }
        text.pop();
        text.push('}');
        assert_eq!(parse(&text).unwrap_err().message, "too many keys in object");
    }

    #[test]
    fn numbers() {
        assert_eq!(parse("0.1").unwrap().to_json(), "0.1");
        assert_eq!(parse("16777215").unwrap().to_json(), "16777215");
        assert_eq!(Value::from(f64::NAN).to_json(), "null");
        let m = json_obj! {"x" => 1u32, "y" => vec![1.5f64, 2.0]};
        assert_eq!(m.to_json(), r#"{"x":1,"y":[1.5,2]}"#);
    }

    #[test]
    fn hfx_presets_parse() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../presets");
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let data = std::fs::read(e.path()).unwrap();
                let v = parse_bytes(&data).unwrap();
                assert!(v.get("effects").as_array().is_some());
            }
        }
    }
}
