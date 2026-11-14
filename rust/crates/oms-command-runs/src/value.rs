use oms_context::Json;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

pub fn get<'a>(v: &'a Json, key: &str) -> Option<&'a Json> {
    if let Json::Object(items) = v {
        items
            .iter()
            .rev()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v)
    } else {
        None
    }
}
pub fn text(v: &Json) -> Option<&str> {
    if let Json::String(s) = v {
        std::str::from_utf8(s).ok()
    } else {
        None
    }
}
pub fn s(v: &Json, key: &str) -> String {
    get(v, key).map(display).unwrap_or_default()
}
pub fn string(v: impl AsRef<str>) -> Json {
    Json::String(v.as_ref().as_bytes().to_vec())
}
pub fn num(v: impl ToString) -> Json {
    Json::Number(v.to_string())
}
pub fn object(items: Vec<(&str, Json)>) -> Json {
    Json::Object(
        items
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
pub fn put(v: &mut Json, key: &str, value: Json) {
    if let Json::Object(items) = v {
        if let Some((_, old)) = items.iter_mut().find(|(k, _)| k == key.as_bytes()) {
            *old = value;
        } else {
            items.push((key.as_bytes().to_vec(), value));
        }
    }
}
pub fn remove(v: &mut Json, key: &str) {
    if let Json::Object(items) = v {
        items.retain(|(k, _)| k != key.as_bytes());
    }
}
pub fn copy(v: &Json) -> Json {
    match v {
        Json::Null => Json::Null,
        Json::Bool(b) => Json::Bool(*b),
        Json::Number(n) => Json::Number(n.clone()),
        Json::String(s) => Json::String(s.clone()),
        Json::Array(a) => Json::Array(a.iter().map(copy).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, v)| (k.clone(), copy(v))).collect()),
    }
}
pub fn number(v: &Json) -> Option<f64> {
    if let Json::Number(n) = v {
        n.parse::<f64>().ok().filter(|n| n.is_finite())
    } else {
        None
    }
}
pub fn integer(v: &Json) -> Option<&str> {
    if let Json::Number(n) = v
        && !n.contains(['.', 'e', 'E'])
        && n.trim_start_matches('-')
            .bytes()
            .all(|c| c.is_ascii_digit())
    {
        return Some(n);
    }
    None
}
pub fn truth(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.parse::<f64>().unwrap_or(1.0) != 0.0,
        Json::String(s) => !s.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::Object(o) => !o.is_empty(),
    }
}
pub fn display(v: &Json) -> String {
    match v {
        Json::String(b) => String::from_utf8_lossy(b).into_owned(),
        Json::Null => "None".into(),
        Json::Bool(b) => if *b { "True" } else { "False" }.into(),
        Json::Number(n) => {
            if let Ok(number) = n.parse::<f64>()
                && !number.is_finite()
                && (n.contains(['.', 'e', 'E']) || n.contains("Infinity") || n == "NaN")
            {
                return if number.is_nan() {
                    "nan"
                } else if number.is_sign_negative() {
                    "-inf"
                } else {
                    "inf"
                }
                .into();
            }
            oms_experiment::encode_json(v, false)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_else(|| n.clone())
        }
        _ => String::from_utf8_lossy(&encode(v, false, false).unwrap_or_default()).into_owned(),
    }
}
fn points(bytes: &[u8]) -> Vec<u32> {
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 128 {
            out.push(b as u32);
            i += 1;
            continue;
        }
        let n = if b & 0xe0 == 0xc0 {
            2
        } else if b & 0xf0 == 0xe0 {
            3
        } else if b & 0xf8 == 0xf0 {
            4
        } else {
            1
        };
        if n > 1 && i + n <= bytes.len() && bytes[i + 1..i + n].iter().all(|b| b & 0xc0 == 0x80) {
            let mut p = (b & (0x7f >> n)) as u32;
            for c in &bytes[i + 1..i + n] {
                p = (p << 6) | (c & 63) as u32;
            }
            if p <= 0x10ffff {
                out.push(p);
                i += n;
                continue;
            }
        }
        out.push(0xdc00 + b as u32);
        i += 1;
    }
    out
}
pub fn os(v: &OsStr) -> Json {
    let mut remaining = v.as_bytes();
    let mut p = vec![];
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(s) => {
                p.extend(s.chars().map(u32::from));
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                p.extend(
                    std::str::from_utf8(&remaining[..good])
                        .unwrap()
                        .chars()
                        .map(u32::from),
                );
                let n = e.error_len().unwrap_or(remaining.len() - good);
                p.extend(
                    remaining[good..good + n]
                        .iter()
                        .map(|b| 0xdc00 + u32::from(*b)),
                );
                remaining = &remaining[good + n..];
            }
        }
    }
    Json::String(from_points(p))
}
fn from_points(p: impl IntoIterator<Item = u32>) -> Vec<u8> {
    let mut bytes = vec![];
    for p in p {
        if let Some(c) = char::from_u32(p) {
            let mut b = [0; 4];
            bytes.extend(c.encode_utf8(&mut b).as_bytes());
        } else {
            bytes.extend([
                (0xe0 | (p >> 12)) as u8,
                (0x80 | ((p >> 6) & 63)) as u8,
                (0x80 | (p & 63)) as u8,
            ]);
        }
    }
    bytes
}
pub fn clip(bytes: &[u8], max: usize, controls: bool) -> Vec<u8> {
    from_points(
        points(bytes)
            .into_iter()
            .filter(|p| !controls || (*p >= 32 && *p != 127))
            .take(max),
    )
}
/// Python-compatible sorted JSON used by the established digest contracts.
pub fn encode(v: &Json, ascii: bool, spaced: bool) -> Result<Vec<u8>, String> {
    encoding(v, ascii, spaced, true)
}
pub fn ordered(v: &Json) -> Result<Vec<u8>, String> {
    encoding(v, false, true, false)
}
fn encoding(v: &Json, ascii: bool, spaced: bool, sorted: bool) -> Result<Vec<u8>, String> {
    fn quoted(out: &mut Vec<u8>, bytes: &[u8], ascii: bool) {
        out.push(b'"');
        for p in points(bytes) {
            match p {
                34 => out.extend(b"\\\""),
                92 => out.extend(b"\\\\"),
                8 => out.extend(b"\\b"),
                9 => out.extend(b"\\t"),
                10 => out.extend(b"\\n"),
                12 => out.extend(b"\\f"),
                13 => out.extend(b"\\r"),
                0..=31 | 0xd800..=0xdfff => out.extend(format!("\\u{p:04x}").bytes()),
                127..=0x10ffff if ascii => {
                    if p <= 65535 {
                        out.extend(format!("\\u{p:04x}").bytes());
                    } else {
                        let n = p - 65536;
                        out.extend(
                            format!(
                                "\\u{:04x}\\u{:04x}",
                                0xd800 + (n >> 10),
                                0xdc00 + (n & 1023)
                            )
                            .bytes(),
                        );
                    }
                }
                _ => {
                    let mut b = [0; 4];
                    out.extend(char::from_u32(p).unwrap().encode_utf8(&mut b).as_bytes());
                }
            }
        }
        out.push(b'"');
    }
    fn emit(
        out: &mut Vec<u8>,
        v: &Json,
        ascii: bool,
        spaced: bool,
        depth: usize,
        sorted: bool,
    ) -> Result<(), String> {
        if depth > 128 {
            return Err("JSON exceeds container-depth limit".into());
        }
        match v {
            Json::String(b) => quoted(out, b, ascii),
            Json::Array(a) => {
                out.push(b'[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.extend(if spaced { b", ".as_slice() } else { b"," });
                    }
                    emit(out, v, ascii, spaced, depth + 1, sorted)?;
                }
                out.push(b']');
            }
            Json::Object(items) => {
                let mut items = items.iter().collect::<Vec<_>>();
                if sorted {
                    items.sort_by_key(|a| points(&a.0));
                }
                out.push(b'{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.extend(if spaced { b", ".as_slice() } else { b"," });
                    }
                    quoted(out, k, ascii);
                    out.extend(if spaced { b": ".as_slice() } else { b":" });
                    emit(out, v, ascii, spaced, depth + 1, sorted)?;
                }
                out.push(b'}');
            }
            Json::Number(n)
                if n.parse::<f64>().is_ok_and(|n| !n.is_finite())
                    && (n.contains(['.', 'e', 'E']) || n.contains("Infinity") || n == "NaN") =>
            {
                let value = n.parse::<f64>().unwrap();
                out.extend(if value.is_nan() {
                    b"NaN".as_slice()
                } else if value.is_sign_negative() {
                    b"-Infinity".as_slice()
                } else {
                    b"Infinity".as_slice()
                });
            }
            _ => out.extend(oms_experiment::encode_json(v, false).map_err(|e| e.to_string())?),
        }
        Ok(())
    }
    let mut out = vec![];
    emit(&mut out, v, ascii, spaced, 0, sorted)?;
    Ok(out)
}
pub fn parse(b: &[u8]) -> Result<Json, String> {
    oms_context::parse_experiment_json_with_depth(b, b.len(), 128).map_err(str::to_owned)
}
pub fn strict(b: &[u8]) -> Result<Json, String> {
    oms_context::parse_json(b).map_err(str::to_owned)
}
pub fn hash(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(b))
}
pub fn digest(v: &Json) -> Result<String, String> {
    Ok(hash(&encode(v, false, false)?))
}
pub fn clean(v: &str, n: usize) -> String {
    v.chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .take(n)
        .collect()
}
