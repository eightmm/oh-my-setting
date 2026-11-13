//! Retain Python surrogate-escaped OS paths through ASCII JSON, without lossy conversion.
use crate::{Result, UNSAFE};
pub use oms_context::Json as J;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

pub fn field<'a>(value: &'a J, key: &str) -> Result<&'a J> {
    let J::Object(fields) = value else {
        return Err(UNSAFE);
    };
    fields
        .iter()
        .find(|(k, _)| k == key.as_bytes())
        .map(|(_, v)| v)
        .ok_or(UNSAFE)
}
pub fn text(value: &J) -> Result<&str> {
    let J::String(bytes) = value else {
        return Err(UNSAFE);
    };
    std::str::from_utf8(bytes).map_err(|_| UNSAFE)
}
pub fn string(value: &str) -> J {
    J::String(value.as_bytes().to_vec())
}
pub fn number(value: i64) -> J {
    J::Number(value.to_string())
}
pub fn object<const N: usize>(values: [(&str, J); N]) -> J {
    J::Object(
        values
            .into_iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value))
            .collect(),
    )
}
pub fn os_string(raw: &OsStr) -> J {
    let mut bytes = raw.as_bytes();
    let mut result = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(_) => {
                result.extend_from_slice(bytes);
                break;
            }
            Err(error) => {
                let end = error.valid_up_to();
                result.extend_from_slice(&bytes[..end]);
                let count = error.error_len().unwrap_or(bytes.len() - end);
                for byte in &bytes[end..end + count] {
                    let point = 0xdc00 + u32::from(*byte);
                    result.extend_from_slice(&[
                        0xe0 | (point >> 12) as u8,
                        0x80 | ((point >> 6) & 63) as u8,
                        0x80 | (point & 63) as u8,
                    ]);
                }
                bytes = &bytes[end + count..];
            }
        }
    }
    J::String(result)
}
pub(super) fn points(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let width = match b {
            0..=127 => 1,
            194..=223 => 2,
            224..=239 => 3,
            240..=244 => 4,
            _ => return Err(UNSAFE),
        };
        let chunk = bytes.get(i..i + width).ok_or(UNSAFE)?;
        let mut point = u32::from(b & (0x7f >> width.min(3)));
        if width == 1 {
            point = u32::from(b);
        }
        for b in &chunk[1..] {
            if b & 0xc0 != 0x80 {
                return Err(UNSAFE);
            }
            point = (point << 6) | u32::from(b & 63);
        }
        if point > 0x10ffff || (width > 1 && point < [0, 0, 128, 2048, 65536][width]) {
            return Err(UNSAFE);
        }
        result.push(point);
        i += width;
    }
    Ok(result)
}
pub fn os_path(value: &J) -> Result<PathBuf> {
    let J::String(bytes) = value else {
        return Err(UNSAFE);
    };
    let mut raw = Vec::new();
    for point in points(bytes)? {
        if (0xdc80..=0xdcff).contains(&point) {
            raw.push((point - 0xdc00) as u8);
        } else {
            let ch = char::from_u32(point).ok_or(UNSAFE)?;
            raw.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }
    Ok(PathBuf::from(OsString::from_vec(raw)))
}
pub fn parse(bytes: &[u8]) -> Result<J> {
    oms_context::parse_json(bytes).map_err(|_| UNSAFE)
}
fn quote(bytes: &[u8], ascii: bool) -> Result<String> {
    use std::fmt::Write;
    let mut output = String::from("\"");
    for point in points(bytes)? {
        match point {
            34 => output.push_str("\\\""),
            92 => output.push_str("\\\\"),
            8 => output.push_str("\\b"),
            9 => output.push_str("\\t"),
            10 => output.push_str("\\n"),
            12 => output.push_str("\\f"),
            13 => output.push_str("\\r"),
            0..=31 | 0xd800..=0xdfff => {
                write!(output, "\\u{point:04x}").map_err(|_| UNSAFE)?;
            }
            127..=0x10ffff if ascii => {
                if point <= 65535 {
                    write!(output, "\\u{point:04x}").map_err(|_| UNSAFE)?;
                } else {
                    let n = point - 65536;
                    write!(
                        output,
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (n >> 10),
                        0xdc00 + (n & 1023)
                    )
                    .map_err(|_| UNSAFE)?;
                }
            }
            _ => output.push(char::from_u32(point).ok_or(UNSAFE)?),
        }
    }
    output.push('"');
    Ok(output)
}
fn emit(value: &J, depth: usize, pretty: bool, ascii: bool, output: &mut String) -> Result<()> {
    if depth > 64 {
        return Err(UNSAFE);
    }
    let pad = |n: usize| " ".repeat(n * 2);
    match value {
        J::Null => output.push_str("null"),
        J::Bool(b) => output.push_str(if *b { "true" } else { "false" }),
        J::Number(n) => output.push_str(n),
        J::String(bytes) => output.push_str(&quote(bytes, ascii)?),
        J::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                if pretty {
                    output.push('\n');
                    output.push_str(&pad(depth + 1));
                }
                emit(value, depth + 1, pretty, ascii, output)?;
            }
            if pretty && !values.is_empty() {
                output.push('\n');
                output.push_str(&pad(depth));
            }
            output.push(']');
        }
        J::Object(fields) => {
            let mut sorted: Vec<_> = fields.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
            output.push('{');
            for (index, (key, value)) in sorted.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                if pretty {
                    output.push('\n');
                    output.push_str(&pad(depth + 1));
                }
                output.push_str(&quote(key, ascii)?);
                output.push(':');
                if pretty {
                    output.push(' ');
                }
                emit(value, depth + 1, pretty, ascii, output)?;
            }
            if pretty && !fields.is_empty() {
                output.push('\n');
                output.push_str(&pad(depth));
            }
            output.push('}');
        }
    }
    Ok(())
}
pub fn encode(value: &J) -> Result<Vec<u8>> {
    let mut output = String::new();
    emit(value, 0, true, true, &mut output)?;
    output.push('\n');
    Ok(output.into_bytes())
}
fn domain(value: &J, depth: usize) -> Result<oms_json::Value> {
    use oms_json::{PyString, Value};
    if depth > 64 {
        return Err(UNSAFE);
    }
    Ok(match value {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::String(bytes) => {
            Value::String(PyString::from_codepoints(points(bytes)?).map_err(|_| UNSAFE)?)
        }
        J::Number(raw) if raw.contains(['.', 'e', 'E']) => {
            Value::Float(raw.parse().map_err(|_| UNSAFE)?)
        }
        J::Number(raw) => {
            Value::integer(if raw == "-0" { "0" } else { raw }).map_err(|_| UNSAFE)?
        }
        J::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| domain(v, depth + 1))
                .collect::<Result<_>>()?,
        ),
        J::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    Ok((
                        PyString::from_codepoints(points(key)?).map_err(|_| UNSAFE)?,
                        domain(value, depth + 1)?,
                    ))
                })
                .collect::<Result<_>>()?,
        ),
    })
}
pub fn canonical(value: &J) -> Result<Vec<u8>> {
    oms_json::canonical_json(&domain(value, 0)?).map_err(|_| UNSAFE)
}
