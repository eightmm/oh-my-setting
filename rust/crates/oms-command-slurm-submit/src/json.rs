use oms_json::{PyString, Value};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

// Python's filesystem codec retains each invalid Unix byte as a low surrogate.
pub(crate) fn os(value: &OsStr) -> Value {
    Value::String(points(value.as_bytes(), false))
}
pub(crate) fn points(mut bytes: &[u8], wtf8: bool) -> PyString {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                out.extend(s.chars().map(u32::from));
                break;
            }
            Err(e) => {
                let n = e.valid_up_to();
                out.extend(
                    std::str::from_utf8(&bytes[..n])
                        .unwrap()
                        .chars()
                        .map(u32::from),
                );
                bytes = &bytes[n..];
                if wtf8
                    && bytes.len() >= 3
                    && bytes[0] == 0xed
                    && (0xa0..=0xbf).contains(&bytes[1])
                    && (0x80..=0xbf).contains(&bytes[2])
                {
                    out.push(
                        ((bytes[0] & 15) as u32) << 12
                            | ((bytes[1] & 63) as u32) << 6
                            | (bytes[2] & 63) as u32,
                    );
                    bytes = &bytes[3..];
                } else {
                    out.push(0xdc00 + bytes[0] as u32);
                    bytes = &bytes[1..];
                }
            }
        }
    }
    PyString::from_codepoints(out).unwrap()
}
pub(crate) fn text(value: &str) -> Value {
    Value::String(value.into())
}
pub(crate) fn object<const N: usize>(items: [(&str, Value); N]) -> Value {
    Value::Object(items.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
pub(crate) fn put(value: &mut Value, key: &str, item: Value) {
    if let Value::Object(o) = value {
        o.insert(key.into(), item);
    }
}
pub(crate) fn integer(value: i64) -> Value {
    Value::integer(&value.to_string()).unwrap()
}
pub(crate) fn copy_value(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::String(s) => Value::String(s.clone()),
        Value::Float(n) => Value::Float(*n),
        Value::Integer(_) => {
            Value::integer(std::str::from_utf8(&oms_json::canonical_json(value).unwrap()).unwrap())
                .unwrap()
        }
        Value::Array(a) => Value::Array(a.iter().map(copy_value).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), copy_value(v))).collect())
        }
    }
}

// json.dumps(sort_keys=True, ensure_ascii=True), with the default spaces.
// The compact variant is used only for the default-key document.
pub(crate) fn encode(value: &Value, compact: bool) -> Vec<u8> {
    fn quote(out: &mut String, s: &PyString) {
        use std::fmt::Write;
        out.push('"');
        for &c in s.codepoints() {
            match c {
                0x22 => out.push_str("\\\""),
                0x5c => out.push_str("\\\\"),
                8 => out.push_str("\\b"),
                9 => out.push_str("\\t"),
                10 => out.push_str("\\n"),
                12 => out.push_str("\\f"),
                13 => out.push_str("\\r"),
                0..=31 | 127..=65535 => {
                    write!(out, "\\u{c:04x}").unwrap();
                }
                65536.. => {
                    let n = c - 65536;
                    write!(
                        out,
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (n >> 10),
                        0xdc00 + (n & 1023)
                    )
                    .unwrap();
                }
                _ => out.push(char::from_u32(c).unwrap()),
            }
        }
        out.push('"');
    }
    fn emit(out: &mut String, v: &Value, compact: bool) {
        let sep = if compact { "," } else { ", " };
        match v {
            Value::String(s) => quote(out, s),
            Value::Array(a) => {
                out.push('[');
                for (i, item) in a.iter().enumerate() {
                    if i != 0 {
                        out.push_str(sep);
                    }
                    emit(out, item, compact);
                }
                out.push(']');
            }
            Value::Object(o) => {
                out.push('{');
                for (i, (k, item)) in o.iter().enumerate() {
                    if i != 0 {
                        out.push_str(sep);
                    }
                    quote(out, k);
                    out.push_str(if compact { ":" } else { ": " });
                    emit(out, item, compact);
                }
                out.push('}');
            }
            Value::Float(n) if !n.is_finite() => out.push_str(if n.is_nan() {
                "NaN"
            } else if n.is_sign_negative() {
                "-Infinity"
            } else {
                "Infinity"
            }),
            _ => out.push_str(std::str::from_utf8(&oms_json::canonical_json(v).unwrap()).unwrap()),
        }
    }
    let mut out = String::new();
    emit(&mut out, value, compact);
    out.into_bytes()
}
pub(crate) fn field<'a>(row: &'a oms_context::Json, key: &str) -> Option<&'a oms_context::Json> {
    if let oms_context::Json::Object(o) = row {
        o.iter()
            .rev()
            .find_map(|(k, v)| (k == key.as_bytes()).then_some(v))
    } else {
        None
    }
}
pub(crate) fn is_text(v: Option<&oms_context::Json>, s: &[u8]) -> bool {
    matches!(v, Some(oms_context::Json::String(t)) if t == s)
}
pub(crate) fn truth(v: Option<&oms_context::Json>) -> bool {
    use oms_context::Json;
    match v {
        None | Some(Json::Null | Json::Bool(false)) => false,
        Some(Json::String(s)) => !s.is_empty(),
        Some(Json::Array(a)) => !a.is_empty(),
        Some(Json::Object(o)) => !o.is_empty(),
        Some(Json::Number(n)) => n.parse::<f64>().map_or(true, |n| n != 0.0),
        _ => true,
    }
}
pub(crate) fn copy(value: Option<&oms_context::Json>) -> Value {
    use oms_context::Json;
    match value {
        None | Some(Json::Null) => Value::Null,
        Some(Json::Bool(b)) => Value::Bool(*b),
        Some(Json::String(s)) => Value::String(points(s, true)),
        Some(Json::Number(n)) => {
            if n == "-0" {
                integer(0)
            } else {
                Value::integer(n).unwrap_or_else(|_| Value::Float(n.parse().unwrap_or(0.0)))
            }
        }
        Some(Json::Array(a)) => Value::Array(a.iter().map(|v| copy(Some(v))).collect()),
        Some(Json::Object(o)) => Value::Object(
            o.iter()
                .map(|(k, v)| (points(k, true), copy(Some(v))))
                .collect(),
        ),
    }
}
pub(crate) fn display(v: Option<&oms_context::Json>) -> Vec<u8> {
    match v {
        None | Some(oms_context::Json::Null) => b"None".to_vec(),
        Some(oms_context::Json::String(s)) => {
            // stdout/stderr use surrogateescape for Unix argv round trips.
            let mut out = Vec::new();
            for &p in points(s, true).codepoints() {
                if (0xdc80..=0xdcff).contains(&p) {
                    out.push((p - 0xdc00) as u8);
                } else if let Some(c) = char::from_u32(p) {
                    let mut b = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                } else {
                    out.extend_from_slice(format!("\\u{p:04x}").as_bytes());
                }
            }
            out
        }
        Some(oms_context::Json::Bool(b)) => {
            if *b {
                b"True".to_vec()
            } else {
                b"False".to_vec()
            }
        }
        Some(oms_context::Json::Number(n)) => n.as_bytes().to_vec(),
        Some(v) => encode(&copy(Some(v)), false),
    }
}
