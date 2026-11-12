//! JSON encodings for OMS records and session transcript values.
//!
//! Objects have string keys, as in JSON records and decoded transcripts. This
//! API does not encode arbitrary Python dictionaries with non-string keys.
#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;

/// A Python string, including code points in the surrogate range.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PyString(Vec<u32>);

impl PyString {
    pub fn from_codepoints(points: Vec<u32>) -> Result<Self, Error> {
        if points.iter().any(|&p| p > 0x10ffff) {
            return Err(Error::InvalidCodepoint);
        }
        Ok(Self(points))
    }

    /// Encode like Python's UTF-8 codec with `errors="replace"`.
    pub fn utf8_with_replacement(&self) -> Vec<u8> {
        self.0
            .iter()
            .map(|&p| char::from_u32(p).unwrap_or('?'))
            .collect::<String>()
            .into_bytes()
    }

    /// Count UTF-8 bytes of the plain string, replacing each surrogate with `?`.
    pub fn utf8_with_replacement_len(&self) -> usize {
        self.0
            .iter()
            .map(|&p| char::from_u32(p).map_or(1, char::len_utf8))
            .sum()
    }
}
impl From<&str> for PyString {
    fn from(value: &str) -> Self {
        Self(value.chars().map(u32::from).collect())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(Integer),
    Float(f64),
    String(PyString),
    Array(Vec<Value>),
    Object(BTreeMap<PyString, Value>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Integer(String);

impl Value {
    /// Construct a Python integer from its canonical decimal representation.
    pub fn integer(decimal: &str) -> Result<Self, Error> {
        let digits = decimal.strip_prefix('-').unwrap_or(decimal);
        if digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
            || decimal == "-0"
        {
            return Err(Error::InvalidInteger);
        }
        Ok(Self::Integer(Integer(decimal.to_owned())))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidCodepoint,
    InvalidInteger,
    NonFiniteFloat,
    LoneSurrogate,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

fn string(out: &mut String, value: &PyString, ascii: bool, replace: bool) -> Result<(), Error> {
    out.push('"');
    for &p in &value.0 {
        match p {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            8 => out.push_str("\\b"),
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            12 => out.push_str("\\f"),
            13 => out.push_str("\\r"),
            0..=0x1f => {
                write!(out, "\\u{p:04x}").unwrap();
            }
            0x7f..=0x10ffff if ascii => {
                if p <= 0xffff {
                    write!(out, "\\u{p:04x}").unwrap();
                } else {
                    let n = p - 0x10000;
                    write!(
                        out,
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (n >> 10),
                        0xdc00 + (n & 1023)
                    )
                    .unwrap();
                }
            }
            _ => out.push(match char::from_u32(p) {
                Some(c) => c,
                None if replace => '?',
                None => return Err(Error::LoneSurrogate),
            }),
        }
    }
    out.push('"');
    Ok(())
}

fn float(value: f64) -> Result<String, Error> {
    if !value.is_finite() {
        return Err(Error::NonFiniteFloat);
    }
    let mut buffer = ryu::Buffer::new();
    let raw = buffer.format_finite(value);
    let (mantissa, exponent) = raw
        .split_once('e')
        .map_or((raw, 0), |(m, e)| (m, e.parse::<i32>().unwrap()));
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa
        .trim_start_matches('-')
        .chars()
        .filter(|&c| c != '.')
        .collect();
    let before = mantissa
        .trim_start_matches('-')
        .find('.')
        .unwrap_or(mantissa.trim_start_matches('-').len()) as i32;
    let first = digits.find(|c| c != '0');
    let Some(first) = first else {
        return Ok(if negative { "-0.0" } else { "0.0" }.to_owned());
    };
    let exp = exponent + before - first as i32 - 1;
    let significant = digits[first..].trim_end_matches('0');
    let sign = if negative { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let mut result = format!("{sign}{}", &significant[..1]);
        if significant.len() > 1 {
            result.push('.');
            result.push_str(&significant[1..]);
        }
        write!(
            result,
            "e{}{:#02}",
            if exp < 0 { '-' } else { '+' },
            exp.unsigned_abs()
        )
        .unwrap();
        Ok(result)
    } else {
        let pos = exp + 1;
        let mut result = sign.to_owned();
        if pos <= 0 {
            result.push_str("0.");
            result.push_str(&"0".repeat((-pos) as usize));
            result.push_str(significant);
        } else if pos as usize >= significant.len() {
            result.push_str(significant);
            result.push_str(&"0".repeat(pos as usize - significant.len()));
            result.push_str(".0");
        } else {
            result.push_str(&significant[..pos as usize]);
            result.push('.');
            result.push_str(&significant[pos as usize..]);
        }
        Ok(result)
    }
}

fn emit(
    out: &mut String,
    value: &Value,
    ascii: bool,
    pretty: bool,
    depth: usize,
    session: bool,
) -> Result<(), Error> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Integer(n) => out.push_str(&n.0),
        Value::Float(n) if session && !n.is_finite() => out.push_str(if n.is_nan() {
            "NaN"
        } else if n.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }),
        Value::Float(n) => out.push_str(&float(*n)?),
        Value::String(s) => string(out, s, ascii, session)?,
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth + 1));
                }
                emit(out, item, ascii, pretty, depth + 1, session)?;
            }
            if pretty && !items.is_empty() {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            out.push(']');
        }
        Value::Object(items) => {
            out.push('{');
            for (i, (key, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth + 1));
                }
                string(out, key, ascii, session)?;
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                emit(out, item, ascii, pretty, depth + 1, session)?;
            }
            if pretty && !items.is_empty() {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            out.push('}');
        }
    }
    Ok(())
}

fn encode(value: &Value, ascii: bool, pretty: bool, session: bool) -> Result<Vec<u8>, Error> {
    let mut out = String::new();
    emit(&mut out, value, ascii, pretty, 0, session)?;
    if pretty {
        out.push('\n');
    }
    Ok(out.into_bytes())
}
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, Error> {
    encode(value, false, false, false)
}
pub fn pretty_json(value: &Value) -> Result<Vec<u8>, Error> {
    encode(value, false, true, false)
}
pub fn agent_event_json_bytes(value: &Value) -> Result<Vec<u8>, Error> {
    encode(value, false, false, false)
}
pub fn projection_cache_digest(value: &Value) -> Result<String, Error> {
    Ok(format!(
        "{:x}",
        Sha256::digest(encode(value, true, false, false)?)
    ))
}

/// Encode tool-call arguments as `context_session._call_key` hashes them.
pub fn session_call_key_json_bytes(value: &Value) -> Result<Vec<u8>, Error> {
    encode(value, false, false, true)
}
