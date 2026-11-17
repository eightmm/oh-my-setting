//! Unix filesystem text keeps its original bytes until the output boundary.
use crate::*;
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
};
pub(crate) fn points(raw: &OsStr) -> oms_json::PyString {
    let mut bytes = raw.as_bytes();
    let mut points = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                points.extend(s.chars().map(u32::from));
                break;
            }
            Err(e) => {
                let end = e.valid_up_to();
                points.extend(
                    std::str::from_utf8(&bytes[..end])
                        .unwrap_or("")
                        .chars()
                        .map(u32::from),
                );
                let count = e.error_len().unwrap_or(bytes.len() - end);
                points.extend(
                    bytes[end..end + count]
                        .iter()
                        .map(|b| 0xdc00 + u32::from(*b)),
                );
                bytes = &bytes[end + count..];
            }
        }
    }
    oms_json::PyString::from_codepoints(points).expect("decoded OS codepoints")
}
pub(crate) fn diagnostic(raw: &OsStr) -> String {
    let mut out = String::new();
    for point in points(raw).codepoints() {
        if let Some(c) = char::from_u32(*point) {
            out.push(c);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "\\x{:02x}", point - 0xdc00);
        }
    }
    out
}
pub(crate) fn line(parts: &[&[u8]]) -> Output {
    let mut out = Vec::new();
    for part in parts {
        out.extend(*part);
    }
    out.push(b'\n');
    (0, out, Vec::new())
}
pub(crate) fn from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from_vec(bytes)
}
fn quote(raw: &OsStr, out: &mut Vec<u8>) {
    use std::fmt::Write;
    out.push(b'"');
    for point in points(raw).codepoints() {
        match *point {
            34 => out.extend(b"\\\""),
            92 => out.extend(b"\\\\"),
            8 => out.extend(b"\\b"),
            9 => out.extend(b"\\t"),
            10 => out.extend(b"\\n"),
            12 => out.extend(b"\\f"),
            13 => out.extend(b"\\r"),
            0..=31 | 0xd800..=0xdfff => {
                let mut s = String::new();
                let _ = write!(s, "\\u{point:04x}");
                out.extend(s.as_bytes());
            }
            _ => {
                if let Some(c) = char::from_u32(*point) {
                    let mut buf = [0; 4];
                    out.extend(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }
    out.push(b'"');
}
/// JSON pointer overrides carry OS text outside serde's scalar-only strings.
/// Normal values still use oms-json; surrogateescaped bytes are emitted as
/// JSON surrogate escapes, retaining identity for a lossless reader.
pub(crate) fn json_line(value: &Value, paths: &[(String, OsString)]) -> Result<Vec<u8>> {
    fn emit(v: &Value, at: &str, paths: &[(String, OsString)], out: &mut Vec<u8>) -> Result<()> {
        if let Some((_, path)) = paths.iter().find(|(pointer, _)| pointer == at) {
            quote(path, out);
            return Ok(());
        }
        match v {
            Value::Object(fields) => {
                out.push(b'{');
                let mut fields: Vec<_> = fields.iter().collect();
                fields.sort_by_key(|(key, _)| *key);
                for (i, (key, value)) in fields.into_iter().enumerate() {
                    if i > 0 {
                        out.extend(b", ");
                    }
                    out.extend(json_bytes(&Value::String(key.clone()))?);
                    out.extend(b": ");
                    emit(
                        value,
                        &format!("{}/{}", at, key.replace('~', "~0").replace('/', "~1")),
                        paths,
                        out,
                    )?;
                }
                out.push(b'}');
            }
            Value::Array(values) => {
                out.push(b'[');
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        out.extend(b", ");
                    }
                    emit(value, &format!("{at}/{i}"), paths, out)?;
                }
                out.push(b']');
            }
            _ => out.extend(json_bytes(v)?),
        }
        Ok(())
    }
    let mut out = Vec::new();
    emit(value, "", paths, &mut out)?;
    out.push(b'\n');
    Ok(out)
}
