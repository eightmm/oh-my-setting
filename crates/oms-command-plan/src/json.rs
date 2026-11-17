//! Plan JSON keeps object order and unknown fields; digests use Python encodings.
use crate::{Result, error, require};
use oms_context::Json;
use oms_json::{PyString, Value as Encoded};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(Vec<u8>),
    Array(Vec<Value>),
    Object(Vec<(Vec<u8>, Value)>),
}
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        fn numeric(a: &Value, b: &Value) -> Option<bool> {
            fn number(v: &Value) -> Option<(String, bool)> {
                match v {
                    Value::Bool(b) => Some((u8::from(*b).to_string(), false)),
                    Value::Number(n) => Some((n.clone(), n.contains(['.', 'e', 'E']))),
                    _ => None,
                }
            }
            let (a, af) = number(a)?;
            let (b, bf) = number(b)?;
            let integer = |s: &str| {
                if s == "-0" {
                    "0".to_owned()
                } else {
                    s.to_owned()
                }
            };
            Some(match (af, bf) {
                (false, false) => integer(&a) == integer(&b),
                (true, true) => a.parse::<f64>().ok() == b.parse::<f64>().ok(),
                (true, false) => a.parse::<f64>().is_ok_and(|n| {
                    n.is_finite()
                        && n.fract() == 0.0
                        && (if n == 0.0 {
                            "0".to_owned()
                        } else {
                            format!("{n:.0}")
                        }) == integer(&b)
                }),
                (false, true) => b.parse::<f64>().is_ok_and(|n| {
                    n.is_finite()
                        && n.fract() == 0.0
                        && (if n == 0.0 {
                            "0".to_owned()
                        } else {
                            format!("{n:.0}")
                        }) == integer(&a)
                }),
            })
        }
        if let Some(equal) = numeric(self, other) {
            return equal;
        }
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| {
                        b.iter()
                            .find(|(name, _)| name == k)
                            .is_some_and(|(_, w)| v == w)
                    })
            }
            _ => false,
        }
    }
}
impl Eq for Value {}
pub static NULL: Value = Value::Null;
impl Value {
    pub fn get(&self, name: &str) -> &Self {
        self.opt(name).unwrap_or(&NULL)
    }
    pub fn opt(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Object(items) => items
                .iter()
                .find(|(k, _)| k == name.as_bytes())
                .map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn set(&mut self, name: &str, value: Self) -> Result<()> {
        let Self::Object(items) = self else {
            return Err(error("expected an object"));
        };
        if let Some((_, old)) = items.iter_mut().find(|(k, _)| k == name.as_bytes()) {
            *old = value;
        } else {
            items.push((name.as_bytes().to_vec(), value));
        }
        Ok(())
    }
    pub fn remove(&mut self, name: &str) -> Option<Self> {
        let Self::Object(items) = self else {
            return None;
        };
        let index = items.iter().position(|(k, _)| k == name.as_bytes())?;
        Some(items.remove(index).1)
    }
    pub fn default(&mut self, name: &str, value: Self) -> Result<()> {
        if self.opt(name).is_none() {
            self.set(name, value)?;
        }
        Ok(())
    }
    pub fn str(&self) -> Result<&str> {
        match self {
            Self::String(v) => {
                std::str::from_utf8(v).map_err(|_| error("text contains a lone surrogate"))
            }
            _ => Err(error("expected text")),
        }
    }
    pub fn text(&self, name: &str) -> &str {
        self.get(name).str().unwrap_or("")
    }
    pub fn array(&self) -> Result<&[Self]> {
        match self {
            Self::Array(v) => Ok(v),
            _ => Err(error("expected a list")),
        }
    }
    pub fn array_mut(&mut self) -> Result<&mut Vec<Self>> {
        match self {
            Self::Array(v) => Ok(v),
            _ => Err(error("expected a list")),
        }
    }
    pub fn object(&self) -> Result<&[(Vec<u8>, Self)]> {
        match self {
            Self::Object(v) => Ok(v),
            _ => Err(error("expected an object")),
        }
    }
    pub fn int(&self) -> Result<i128> {
        match self {
            Self::Number(v) if !v.contains(['.', 'e', 'E']) => {
                v.parse().map_err(|_| error("integer is out of range"))
            }
            _ => Err(error("expected an integer")),
        }
    }
    pub fn is_int(&self, n: i128) -> bool {
        self.int() == Ok(n)
    }
    pub fn truth(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(v) => *v,
            Self::Number(v) => v.parse::<f64>() != Ok(0.0),
            Self::String(v) => !v.is_empty(),
            Self::Array(v) => !v.is_empty(),
            Self::Object(v) => !v.is_empty(),
        }
    }
    pub fn strings(&self) -> Result<Vec<&str>> {
        self.array()?.iter().map(Self::str).collect()
    }
}
pub fn object<const N: usize>(items: [(&str, Value); N]) -> Value {
    Value::Object(
        items
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
pub fn string(value: impl AsRef<str>) -> Value {
    Value::String(value.as_ref().as_bytes().to_vec())
}
pub fn number(value: impl ToString) -> Value {
    Value::Number(value.to_string())
}
pub fn increment(value: &Value) -> Result<Value> {
    let Value::Number(raw) = value else {
        return Err(error("invalid lease epoch"));
    };
    require(
        !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) && raw.len() <= 4300,
        "invalid lease epoch",
    )?;
    let mut digits = raw.as_bytes().to_vec();
    let mut carry = true;
    for digit in digits.iter_mut().rev() {
        if !carry {
            break;
        }
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            carry = false;
        }
    }
    if carry {
        digits.insert(0, b'1');
    }
    require(
        digits.len() <= 4300,
        "lease epoch exceeds integer digit limit",
    )?;
    Ok(Value::Number(
        String::from_utf8(digits).map_err(|_| error("invalid lease epoch"))?,
    ))
}
pub fn list<'a>(items: impl IntoIterator<Item = &'a str>) -> Value {
    Value::Array(items.into_iter().map(string).collect())
}
pub fn empty() -> Value {
    Value::Object(vec![])
}
fn from_context(value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(v) => Value::Bool(*v),
        Json::Number(v) => Value::Number(v.clone()),
        Json::String(v) => Value::String(v.clone()),
        Json::Array(v) => Value::Array(v.iter().map(from_context).collect()),
        Json::Object(v) => Value::Object(
            v.iter()
                .map(|(k, v)| (k.clone(), from_context(v)))
                .collect(),
        ),
    }
}
pub fn parse(raw: &[u8]) -> Result<Value> {
    oms_context::parse_json(raw)
        .map(|v| from_context(&v))
        .map_err(error)
}
pub fn parse_bounded(raw: &[u8], limit: usize) -> Result<Value> {
    oms_context::parse_json_bounded(raw, limit)
        .map(|v| from_context(&v))
        .map_err(error)
}
pub fn parse_legacy(raw: &[u8], limit: usize) -> Result<Value> {
    oms_context::parse_experiment_json(raw, limit)
        .map(|v| from_context(&v))
        .map_err(error)
}
pub fn keys(value: &Value, required: &[&str], optional: &[&str]) -> Result<()> {
    let items = value.object()?;
    require(
        required.iter().all(|k| value.opt(k).is_some())
            && items
                .iter()
                .all(|(k, _)| required.iter().chain(optional).any(|n| n.as_bytes() == k)),
        "malformed or unsupported object fields",
    )
}
pub fn sha(raw: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(raw.as_ref()))
}
pub fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn digest(value: &Value) -> Result<&str> {
    let s = value.str()?;
    require(hex(s, 64), "invalid SHA-256")?;
    Ok(s)
}
pub fn points(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        let width = match b {
            0..=127 => 1,
            194..=223 => 2,
            224..=239 => 3,
            240..=244 => 4,
            _ => return Err(error("invalid string encoding")),
        };
        let chunk = bytes
            .get(at..at + width)
            .ok_or_else(|| error("invalid string encoding"))?;
        let mut p = u32::from(if width == 1 { b } else { b & (0x7f >> width) });
        for b in &chunk[1..] {
            require(b & 0xc0 == 0x80, "invalid string encoding")?;
            p = (p << 6) | u32::from(b & 63);
        }
        require(
            p <= 0x10ffff && (width == 1 || p >= [0, 0, 128, 2048, 65536][width]),
            "invalid string encoding",
        )?;
        result.push(p);
        at += width;
    }
    Ok(result)
}
pub fn os(value: &OsStr) -> Value {
    let mut raw = value.as_bytes();
    let mut out = Vec::new();
    while !raw.is_empty() {
        match std::str::from_utf8(raw) {
            Ok(_) => {
                out.extend_from_slice(raw);
                break;
            }
            Err(e) => {
                let n = e.valid_up_to();
                out.extend_from_slice(&raw[..n]);
                let len = e.error_len().unwrap_or(raw.len() - n);
                for b in &raw[n..n + len] {
                    let p = 0xdc00 + u32::from(*b);
                    out.extend_from_slice(&[
                        (0xe0 | (p >> 12)) as u8,
                        (0x80 | ((p >> 6) & 63)) as u8,
                        (0x80 | (p & 63)) as u8,
                    ]);
                }
                raw = &raw[n + len..];
            }
        }
    }
    Value::String(out)
}
pub fn path(value: &Value) -> Result<OsString> {
    let Value::String(raw) = value else {
        return Err(error("expected path text"));
    };
    let mut out = Vec::new();
    for p in points(raw)? {
        if (0xdc80..=0xdcff).contains(&p) {
            out.push((p - 0xdc00) as u8);
        } else {
            let c = char::from_u32(p).ok_or_else(|| error("invalid path surrogate"))?;
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }
    require(!out.contains(&0), "NUL in path")?;
    Ok(OsString::from_vec(out))
}
fn domain(value: &Value) -> Result<Encoded> {
    Ok(match value {
        Value::Null => Encoded::Null,
        Value::Bool(v) => Encoded::Bool(*v),
        Value::Number(n) if n.contains(['.', 'e', 'E']) => {
            Encoded::Float(n.parse().map_err(|_| error("invalid JSON number"))?)
        }
        Value::Number(n) => Encoded::integer(if n == "-0" { "0" } else { n }).map_err(error)?,
        Value::String(v) => Encoded::String(PyString::from_codepoints(points(v)?).map_err(error)?),
        Value::Array(v) => Encoded::Array(v.iter().map(domain).collect::<Result<_>>()?),
        Value::Object(v) => Encoded::Object(
            v.iter()
                .map(|(k, v)| {
                    Ok((
                        PyString::from_codepoints(points(k)?).map_err(error)?,
                        domain(v)?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?,
        ),
    })
}
pub fn canonical(value: &Value) -> Result<Vec<u8>> {
    oms_json::canonical_json(&domain(value)?).map_err(error)
}
pub fn encoded(value: &Value) -> Result<Vec<u8>> {
    let mut out = canonical(value)?;
    out.push(b'\n');
    Ok(out)
}
pub fn hash(value: &Value) -> Result<String> {
    Ok(sha(canonical(value)?))
}
pub fn encoded_hash(value: &Value) -> Result<String> {
    Ok(sha(encoded(value)?))
}
/// Python's default separators, for legacy request and reuse receipt stamps.
pub fn default_bytes(value: &Value, ascii: bool) -> Result<Vec<u8>> {
    let raw =
        String::from_utf8(canonical(value)?).map_err(|_| error("JSON encoding unavailable"))?;
    let mut out = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in raw.chars() {
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' && quoted {
            out.push(c);
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            out.push(c);
            continue;
        }
        if ascii && quoted && c as u32 >= 127 {
            let point = c as u32;
            if point <= 65535 {
                out.push_str(&format!("\\u{point:04x}"));
            } else {
                let p = point - 65536;
                out.push_str(&format!(
                    "\\u{:04x}\\u{:04x}",
                    0xd800 + (p >> 10),
                    0xdc00 + (p & 1023)
                ));
            }
        } else {
            out.push(c);
            if !quoted && matches!(c, ',' | ':') {
                out.push(' ');
            }
        }
    }
    Ok(out.into_bytes())
}
pub fn ascii_canonical(value: &Value) -> Result<Vec<u8>> {
    let raw = default_bytes(value, true)?;
    let mut out = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for byte in raw {
        if escaped {
            out.push(byte);
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            out.push(byte);
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quoted = !quoted;
        }
        if quoted || byte != b' ' {
            out.push(byte);
        }
    }
    Ok(out)
}
fn emit(value: &Value, depth: usize, out: &mut Vec<u8>) -> Result<()> {
    require(depth <= 1024, "JSON nesting exceeds 1024")?;
    match value {
        Value::Object(items) => {
            out.push(b'{');
            for (i, (k, v)) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.push(b'\n');
                out.extend(std::iter::repeat_n(b' ', (depth + 1) * 2));
                out.extend(canonical(&Value::String(k.clone()))?);
                out.extend_from_slice(b": ");
                emit(v, depth + 1, out)?;
            }
            if !items.is_empty() {
                out.push(b'\n');
                out.extend(std::iter::repeat_n(b' ', depth * 2));
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.push(b'\n');
                out.extend(std::iter::repeat_n(b' ', (depth + 1) * 2));
                emit(v, depth + 1, out)?;
            }
            if !items.is_empty() {
                out.push(b'\n');
                out.extend(std::iter::repeat_n(b' ', depth * 2));
            }
            out.push(b']');
        }
        _ => out.extend(canonical(value)?),
    }
    Ok(())
}
pub fn pretty(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    emit(value, 0, &mut out)?;
    Ok(out)
}
pub fn projection(task: &Value) -> Result<Value> {
    Ok(Value::Object(
        task.object()?
            .iter()
            .filter(|(k, _)| {
                ![
                    "state",
                    "updated",
                    "claim_expired",
                    "claim_age_s",
                    "project_contract",
                    "task_sha256",
                ]
                .iter()
                .any(|v| k == v.as_bytes())
            })
            .cloned()
            .collect(),
    ))
}
pub fn receipt_digest(task: &Value) -> Result<String> {
    hash(&projection(task)?)
}
