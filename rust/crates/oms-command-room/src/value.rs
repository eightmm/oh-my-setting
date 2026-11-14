//! Python-compatible JSON strings and raw Unix paths, without lossy decoding.
use crate::{Error, Result};
use std::ffi::OsStr;
use std::ops::{Index, IndexMut};
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(Vec<u8>),
    Array(Vec<Value>),
    Object(Vec<(Vec<u8>, Value)>),
}
static NULL: Value = Value::Null;
impl Value {
    pub fn get(&self, name: &str) -> Option<&Value> {
        match self {
            Self::Object(v) => v.iter().find(|(k, _)| k == name.as_bytes()).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn set(&mut self, name: &str, value: impl Into<Value>) {
        self[name] = value.into();
    }
    pub fn remove(&mut self, name: &str) {
        if let Self::Object(v) = self {
            v.retain(|(k, _)| k != name.as_bytes());
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(v) => std::str::from_utf8(v).ok(),
            _ => None,
        }
    }
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }
    pub(crate) fn utf8_strings(&self) -> bool {
        match self {
            Self::String(bytes) => std::str::from_utf8(bytes).is_ok(),
            Self::Array(values) => values.iter().all(Self::utf8_strings),
            Self::Object(fields) => fields
                .iter()
                .all(|(name, value)| std::str::from_utf8(name).is_ok() && value.utf8_strings()),
            _ => true,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(v) = self {
            Some(*v)
        } else {
            None
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        if let Self::Number(v) = self {
            v.parse().ok()
        } else {
            None
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        if let Self::Number(v) = self {
            v.parse().ok()
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        if let Self::Array(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&Vec<(Vec<u8>, Value)>> {
        if let Self::Object(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn text(&self, name: &str) -> &str {
        self[name].as_str().unwrap_or("")
    }
    pub fn list(&self, name: &str) -> &[Value] {
        self[name].as_array().map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn flag(&self, name: &str) -> bool {
        self[name].as_bool() == Some(true)
    }
    pub fn number(&self, name: &str) -> u64 {
        self[name].as_u64().unwrap_or(0)
    }
}
impl Index<&str> for Value {
    type Output = Value;
    fn index(&self, name: &str) -> &Value {
        self.get(name).unwrap_or(&NULL)
    }
}
impl IndexMut<&str> for Value {
    fn index_mut(&mut self, name: &str) -> &mut Value {
        if !matches!(self, Self::Object(_)) {
            *self = obj([]);
        }
        let Self::Object(v) = self else {
            unreachable!()
        };
        let at = match v.iter().position(|(k, _)| k == name.as_bytes()) {
            Some(at) => at,
            None => {
                v.push((name.as_bytes().to_vec(), Self::Null));
                v.len() - 1
            }
        };
        &mut v[at].1
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::String(v.as_bytes().to_vec())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::String(v.into_bytes())
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Self::Number(v.to_string())
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Self::Number(v.to_string())
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Number(v.to_string())
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Self::Array(v)
    }
}
impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Self {
        Self::Array(v.into_iter().map(Value::from).collect())
    }
}
impl From<Option<String>> for Value {
    fn from(v: Option<String>) -> Self {
        v.map(Value::from).unwrap_or(Self::Null)
    }
}
pub fn obj<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
pub fn array<'a>(values: impl IntoIterator<Item = &'a str>) -> Value {
    Value::Array(values.into_iter().map(Value::from).collect())
}
pub fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| Error::new("invalid room event list"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("invalid room event list"))
        })
        .collect()
}
fn convert(v: &oms_context::Json) -> Value {
    use oms_context::Json as J;
    match v {
        J::Null => Value::Null,
        J::Bool(v) => Value::Bool(*v),
        J::Number(v) => Value::Number(v.clone()),
        J::String(v) => Value::String(v.clone()),
        J::Array(v) => Value::Array(v.iter().map(convert).collect()),
        J::Object(v) => Value::Object(v.iter().map(|(k, v)| (k.clone(), convert(v))).collect()),
    }
}
pub fn parse(raw: &[u8], limit: usize) -> Result<Value> {
    let v = oms_context::parse_experiment_json_with_depth(raw, limit, 64)
        .map_err(|_| Error::new("invalid room JSON"))?;
    Ok(convert(&v))
}
fn points(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let n = match b {
            0..=127 => 1,
            194..=223 => 2,
            224..=239 => 3,
            240..=244 => 4,
            _ => return Err(Error::new("invalid JSON string")),
        };
        let chunk = bytes
            .get(i..i + n)
            .ok_or_else(|| Error::new("invalid JSON string"))?;
        let mut p = if n == 1 {
            b as u32
        } else {
            (b & (0x7f >> n)) as u32
        };
        for b in &chunk[1..] {
            if b & 0xc0 != 0x80 {
                return Err(Error::new("invalid JSON string"));
            }
            p = (p << 6) | u32::from(b & 63);
        }
        if p > 0x10ffff || (n > 1 && p < [0, 0, 128, 2048, 65536][n]) {
            return Err(Error::new("invalid JSON string"));
        }
        out.push(p);
        i += n;
    }
    Ok(out)
}
fn quote(bytes: &[u8], out: &mut String) -> Result<()> {
    use std::fmt::Write;
    out.push('"');
    for p in points(bytes)? {
        match p {
            34 => out.push_str("\\\""),
            92 => out.push_str("\\\\"),
            8 => out.push_str("\\b"),
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            12 => out.push_str("\\f"),
            13 => out.push_str("\\r"),
            0..=31 | 0xd800..=0xdfff => {
                write!(out, "\\u{p:04x}").map_err(|_| Error::new("JSON encoding failed"))?;
            }
            _ => out.push(char::from_u32(p).ok_or_else(|| Error::new("invalid JSON codepoint"))?),
        }
    }
    out.push('"');
    Ok(())
}
fn emit(v: &Value, out: &mut String, depth: usize, pretty: bool, compact: bool) -> Result<()> {
    if depth > 64 {
        return Err(Error::new("room JSON exceeds depth bound"));
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Value::Number(n) => out.push_str(n),
        Value::String(s) => quote(s, out)?,
        Value::Array(v) => {
            out.push('[');
            for (i, v) in v.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !compact && !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth + 1));
                }
                emit(v, out, depth + 1, pretty, compact)?;
            }
            if pretty && !v.is_empty() {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            out.push(']');
        }
        Value::Object(v) => {
            out.push('{');
            for (i, (k, v)) in v.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !compact && !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth + 1));
                }
                quote(k, out)?;
                out.push(':');
                if !compact {
                    out.push(' ');
                }
                emit(v, out, depth + 1, pretty, compact)?;
            }
            if pretty && !v.is_empty() {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            out.push('}');
        }
    }
    Ok(())
}
pub fn encode(v: &Value, pretty: bool, compact: bool) -> Result<Vec<u8>> {
    let mut out = String::new();
    emit(v, &mut out, 0, pretty, compact)?;
    Ok(out.into_bytes())
}
pub fn os_value(raw: &OsStr) -> Value {
    let mut bytes = raw.as_bytes();
    let mut result = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(_) => {
                result.extend_from_slice(bytes);
                break;
            }
            Err(e) => {
                let at = e.valid_up_to();
                result.extend_from_slice(&bytes[..at]);
                let n = e.error_len().unwrap_or(bytes.len() - at);
                for b in &bytes[at..at + n] {
                    let p = 0xdc00 + u32::from(*b);
                    result.extend_from_slice(&[
                        0xe0 | (p >> 12) as u8,
                        0x80 | ((p >> 6) & 63) as u8,
                        0x80 | (p & 63) as u8,
                    ]);
                }
                bytes = &bytes[at + n..];
            }
        }
    }
    Value::String(result)
}
