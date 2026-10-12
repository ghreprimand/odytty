// SPDX-License-Identifier: GPL-3.0-only
//! A tiny, self-contained JSON reader/writer for named profiles.
//!
//! No `serde_json` dependency: profiles use a bounded set of value kinds, so a
//! small reader and writer suffice. The lenient [`parse`] serves tool output
//! and palette files; profile documents use [`parse_strict`], which also
//! rejects raw control characters in strings, numbers outside the JSON grammar
//! or beyond `f64`, duplicate object keys, and objects over the member limit.
//! Unknown object keys are kept
//! (forward compatibility); the writer emits pretty-printed, hand-editable
//! output with a stable key order and two-space indentation. Numbers are held
//! as `f64`; a profile-document number with more than 15 significant digits,
//! which may not survive the trip through `f64` back to decimal, also keeps
//! its validated literal so a save writes it back unchanged.

use std::collections::HashSet;
use std::fmt::Write as _;

/// A parsed JSON value. Object entries keep insertion order so serialize is
/// deterministic and diff-friendly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(f64),
    /// A number whose literal has more than [`EXACT_DIGITS`] significant
    /// digits, so `f64` may not hold it exactly: the `f64` for arithmetic and
    /// the literal, already checked against the JSON number grammar, for
    /// writing. Only the strict reader produces it.
    Exact(f64, String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

#[allow(dead_code)]
impl Json {
    /// The value for `key` if this is an object holding it.
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries
                .iter()
                .find(|(entry_key, _)| entry_key == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) | Json::Exact(n, _) => Some(*n),
            _ => None,
        }
    }

    /// A non-negative, finite number as a `usize`, else `None`.
    pub(crate) fn as_usize(&self) -> Option<usize> {
        match self {
            Json::Num(n) | Json::Exact(n, _) if n.is_finite() && *n >= 0.0 => Some(*n as usize),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// An owned copy of a string value; `null` and non-strings yield `None`
    /// (so an explicit `null` optional field round-trips to `None`).
    pub(crate) fn as_owned_str(&self) -> Option<String> {
        match self {
            Json::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }
}

/// Build an object from a dynamic list of `(key, value)` pairs.
pub(crate) fn obj_pairs(entries: Vec<(String, Json)>) -> Json {
    Json::Obj(entries)
}

/// Pretty-print `value` with two-space indentation and a trailing newline.
pub(crate) fn to_pretty(value: &Json) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out.push('\n');
    out
}

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Json, depth: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => write_number(out, *n),
        Json::Exact(_, literal) => out.push_str(literal),
        Json::Str(s) => write_string(out, s),
        Json::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                write_indent(out, depth + 1);
                write_value(out, item, depth + 1);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            write_indent(out, depth);
            out.push(']');
        }
        Json::Obj(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, value)) in entries.iter().enumerate() {
                write_indent(out, depth + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, value, depth + 1);
                if index + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            write_indent(out, depth);
            out.push('}');
        }
    }
}

/// The most significant decimal digits that always survive a trip through
/// `f64` and back to the shortest decimal form.
const EXACT_DIGITS: usize = 15;

/// Significant digits in the mantissa of a JSON number literal: the digits
/// between the first and last non-zero one, ignoring the sign, the decimal
/// point and the exponent.
fn significant_digits(literal: &str) -> usize {
    let mantissa = literal.split(['e', 'E']).next().unwrap_or("");
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let Some(first) = digits.iter().position(|digit| *digit != b'0') else {
        return 0;
    };
    let last = digits
        .iter()
        .rposition(|digit| *digit != b'0')
        .unwrap_or(first);
    last - first + 1
}

fn write_number(out: &mut String, n: f64) {
    if !n.is_finite() {
        // JSON has no NaN/Infinity; a shape never produces these, but never
        // emit invalid JSON if one somehow appears.
        out.push('0');
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        // Integer-valued (versions, indices): print without a fractional part.
        let _ = write!(out, "{}", n as i64);
    } else {
        // Fractional (split ratios): the shortest round-tripping repr.
        let _ = write!(out, "{n}");
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Maximum object/array nesting depth accepted by [`parse`]. A hand-editable
/// profile is only a few levels deep; this bound is far above any real profile
/// yet well below the ~thousands of frames that would exhaust the thread stack.
/// A file nested past this returns an `Err` (classified as a malformed profile)
/// instead of aborting the process via a stack overflow.
const MAX_PARSE_DEPTH: usize = 128;

/// Parse JSON text into a [`Json`] value, or an error message.
pub(crate) fn parse(input: &str) -> Result<Json, String> {
    parse_with(input, false)
}

/// Parse a profile document: [`parse`] plus the strict checks listed in the
/// module header, so a damaged hand-edited file is refused with a precise
/// message instead of being silently normalized on the next save.
pub(crate) fn parse_strict(input: &str) -> Result<Json, String> {
    parse_with(input, true)
}

fn parse_with(input: &str, strict: bool) -> Result<Json, String> {
    let mut parser = Parser {
        chars: input.chars().collect(),
        pos: 0,
        depth: 0,
        strict,
    };
    parser.skip_ws();
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(format!("trailing characters at position {}", parser.pos));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    /// Current object/array nesting depth, bounded by [`MAX_PARSE_DEPTH`] to
    /// keep a pathological deeply-nested input from overflowing the stack.
    depth: usize,
    /// Apply the profile-document checks (see [`parse_strict`]).
    strict: bool,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, ch: char) -> Result<(), String> {
        if self.peek() == Some(ch) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{ch}' at position {}", self.pos))
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        // Only object/array descent deepens the parse; guard here so every
        // recursive re-entry is counted regardless of the value kind.
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(format!(
                "maximum nesting depth {MAX_PARSE_DEPTH} exceeded at position {}",
                self.pos
            ));
        }
        let value = self.parse_value_inner();
        self.depth -= 1;
        value
    }

    fn parse_value_inner(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => Ok(Json::Str(self.parse_string()?)),
            Some('t' | 'f') => self.parse_bool(),
            Some('n') => self.parse_null(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!(
                "unexpected character '{c}' at position {}",
                self.pos
            )),
            None => Err("unexpected end of input".to_owned()),
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect('{')?;
        let mut entries = Vec::new();
        let mut keys = HashSet::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Json::Obj(entries));
        }
        loop {
            self.skip_ws();
            if self.strict && entries.len() >= super::limits::MAX_PROFILE_JSON_OBJECT_MEMBERS {
                return Err("object member limit exceeded".to_owned());
            }
            let key = self.parse_string()?;
            if self.strict && !keys.insert(key.clone()) {
                return Err(format!("duplicate object key {key:?}"));
            }
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                other => {
                    return Err(format!("expected ',' or '}}' in object, found {other:?}"));
                }
            }
        }
        Ok(Json::Obj(entries))
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some(']') => break,
                other => {
                    return Err(format!("expected ',' or ']' in array, found {other:?}"));
                }
            }
        }
        Ok(Json::Arr(items))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".to_owned()),
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('b') => out.push('\u{08}'),
                    Some('f') => out.push('\u{0c}'),
                    Some('u') => out.push(self.parse_unicode_escape()?),
                    other => {
                        return Err(format!("invalid escape '\\{}'", other.unwrap_or(' ')));
                    }
                },
                Some(c) if self.strict && (c as u32) < 0x20 => {
                    return Err(format!(
                        "unescaped control character U+{:04X} in string at position {}",
                        c as u32,
                        self.pos - 1
                    ));
                }
                Some(c) => out.push(c),
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let ch = self.bump().ok_or("unterminated \\u escape")?;
            let digit = ch
                .to_digit(16)
                .ok_or_else(|| format!("invalid hex digit '{ch}' in \\u escape"))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_unicode_escape(&mut self) -> Result<char, String> {
        let high = self.parse_hex4()?;
        if (0xD800..=0xDBFF).contains(&high) {
            // A high surrogate must be followed by a \uXXXX low surrogate.
            if self.bump() != Some('\\') || self.bump() != Some('u') {
                return Err("expected a low surrogate after a high surrogate".to_owned());
            }
            let low = self.parse_hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err("invalid low surrogate in \\u escape".to_owned());
            }
            let combined = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
            char::from_u32(combined).ok_or_else(|| "invalid surrogate pair".to_owned())
        } else {
            char::from_u32(high).ok_or_else(|| "invalid \\u escape".to_owned())
        }
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(
            self.peek(),
            Some(c) if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-'
        ) {
            self.pos += 1;
        }
        let slice: String = self.chars[start..self.pos].iter().collect();
        if self.strict && !is_json_number(&slice) {
            return Err(format!("invalid number '{slice}'"));
        }
        match slice.parse::<f64>() {
            Ok(value) if self.strict && !value.is_finite() => {
                Err(format!("number '{slice}' is out of range"))
            }
            Ok(value) if self.strict && significant_digits(&slice) > EXACT_DIGITS => {
                Ok(Json::Exact(value, slice))
            }
            Ok(value) => Ok(Json::Num(value)),
            Err(_) => Err(format!("invalid number '{slice}'")),
        }
    }

    fn parse_bool(&mut self) -> Result<Json, String> {
        if self.match_literal("true") {
            Ok(Json::Bool(true))
        } else if self.match_literal("false") {
            Ok(Json::Bool(false))
        } else {
            Err(format!("invalid literal at position {}", self.pos))
        }
    }

    fn parse_null(&mut self) -> Result<Json, String> {
        if self.match_literal("null") {
            Ok(Json::Null)
        } else {
            Err(format!("invalid literal at position {}", self.pos))
        }
    }

    fn match_literal(&mut self, literal: &str) -> bool {
        let end = self.pos + literal.chars().count();
        if end <= self.chars.len()
            && self.chars[self.pos..end].iter().collect::<String>() == literal
        {
            self.pos = end;
            true
        } else {
            false
        }
    }
}

/// Whether `text` matches the JSON number grammar: an optional minus, an
/// integer part without a leading zero, then optional fraction and exponent
/// parts that each need at least one digit.
fn is_json_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let digits = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == digits {
            return false;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let digits = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == digits {
            return false;
        }
    }
    index == bytes.len()
}
