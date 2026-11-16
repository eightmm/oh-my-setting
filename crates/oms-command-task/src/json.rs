use oms_context::Json;

pub fn object(items: impl IntoIterator<Item = (impl AsRef<str>, Json)>) -> Json {
    Json::Object(
        items
            .into_iter()
            .map(|(k, v)| (k.as_ref().as_bytes().to_vec(), v))
            .collect(),
    )
}
pub fn string(s: impl AsRef<[u8]>) -> Json {
    Json::String(s.as_ref().to_vec())
}
pub fn field<'a>(v: &'a Json, name: &str) -> &'a Json {
    static NONE: Json = Json::Null;
    if let Json::Object(items) = v {
        items
            .iter()
            .rev()
            .find(|(k, _)| k == name.as_bytes())
            .map(|(_, v)| v)
            .unwrap_or(&NONE)
    } else {
        &NONE
    }
}
pub fn text(v: &Json) -> Option<&[u8]> {
    if let Json::String(s) = v {
        Some(s)
    } else {
        None
    }
}
pub fn copy(v: &Json) -> Json {
    match v {
        Json::Null => Json::Null,
        Json::Bool(v) => Json::Bool(*v),
        Json::Number(v) => Json::Number(v.clone()),
        Json::String(v) => Json::String(v.clone()),
        Json::Array(v) => Json::Array(v.iter().map(copy).collect()),
        Json::Object(v) => Json::Object(v.iter().map(|(k, v)| (k.clone(), copy(v))).collect()),
    }
}

// JSON strings arrive as WTF-8; OS strings may additionally contain undecodable
// bytes. Emit both losslessly as Python-compatible surrogate escapes.
fn quoted(raw: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0..=31 => out.extend_from_slice(format!("\\u{b:04x}").as_bytes()),
            32..=127 => out.push(b),
            _ => {
                if b == 0xed
                    && i + 2 < raw.len()
                    && (0xa0..=0xbf).contains(&raw[i + 1])
                    && (0x80..=0xbf).contains(&raw[i + 2])
                {
                    let cp = (u32::from(b & 15) << 12)
                        | (u32::from(raw[i + 1] & 63) << 6)
                        | u32::from(raw[i + 2] & 63);
                    out.extend_from_slice(format!("\\u{cp:04x}").as_bytes());
                    i += 3;
                    continue;
                }
                let n = if b < 0xe0 {
                    2
                } else if b < 0xf0 {
                    3
                } else {
                    4
                };
                if i + n <= raw.len() && std::str::from_utf8(&raw[i..i + n]).is_ok() {
                    out.extend_from_slice(&raw[i..i + n]);
                    i += n;
                    continue;
                }
                out.extend_from_slice(format!("\\udc{b:02x}").as_bytes());
            }
        }
        i += 1;
    }
    out.push(b'"');
}
pub fn encode(v: &Json) -> Vec<u8> {
    fn emit(v: &Json, out: &mut Vec<u8>) {
        match v {
            Json::Null => out.extend_from_slice(b"null"),
            Json::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
            Json::Number(v) => out.extend_from_slice(v.as_bytes()),
            Json::String(v) => quoted(v, out),
            Json::Array(v) => {
                out.push(b'[');
                for (i, v) in v.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    emit(v, out);
                }
                out.push(b']');
            }
            Json::Object(v) => {
                out.push(b'{');
                for (i, (k, v)) in v.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    quoted(k, out);
                    out.extend_from_slice(b": ");
                    emit(v, out);
                }
                out.push(b'}');
            }
        }
    }
    let mut result = Vec::new();
    emit(v, &mut result);
    result
}
