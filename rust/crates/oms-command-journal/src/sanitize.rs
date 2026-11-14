#![forbid(unsafe_code)]

use regex::{Captures, Regex};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::sync::OnceLock;

pub const SCHEMA_VERSION: u64 = 1;
pub const INDEX_SCHEMA_VERSION: &str = "2";
pub const RENDERER_VERSION: &str = "3";
pub const COLD_SCHEMA_VERSION: u64 = 1;

pub const LIVE_FILE_BYTE_LIMIT: u64 = 5 * 1024 * 1024;
pub const LIVE_EVENT_LINE_LIMIT: usize = 1000;
pub const JOURNAL_FILE_COUNT_LIMIT: usize = 100;

fn pattern(expression: &str) -> Regex {
    Regex::new(expression).expect("canonical sanitizer expression")
}

fn pem_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        pattern(
            r"-----BE[G]IN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
        )
    })
}

fn raw_field_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| pattern(r"(?i)^(?:raw|stdout|stderr|transcript|conversation|environment|env|diff|output_log|log_text|full_log|workspace_dump)$"))
}

fn sensitive_field(key: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        pattern(concat!(
            r"(?i)(?:^|_)(?:authorization|api_?key|to",
            "ken|se",
            "cret|pass",
            "word|passwd|credential)s?(?:_|$)"
        ))
    })
    .is_match(key)
}

fn redact_text(text: &str) -> String {
    let mut s = pem_re().replace_all(text, "[REDACTED]").into_owned();
    for (expression, replacement) in [
        (
            concat!(r"(?i)(authoriz", r"ation\s*[:=]\s*).*$"),
            "${1}[REDACTED]",
        ),
        (
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}",
            "Bearer [REDACTED]",
        ),
        (
            r"(?i)\b(machin[e]\s+\S+\s+logi[n]\s+\S+\s+passwor[d]\s+)\S+",
            "${1}[REDACTED]",
        ),
        (
            r"(?i)(?:hook[s]\.slac[k]\.com/service[s]/|disc[o]rd(?:app)?\.com/api/webhook[s]/)\S*",
            "[REDACTED]",
        ),
    ] {
        s = pattern(expression)
            .replace_all(&s, replacement)
            .into_owned();
    }
    let assignment = pattern(concat!(
        r#"(?i)(^|[^A-Za-z0-9_])([A-Za-z0-9_]*(?:t[o]ken|s[e]cret|passw(?:or)?[d]|credentia[l]s?|(?:ap[i]|s[e]cret|privat[e])[-_ ]?(?:ke[y]|t[o]ken)|aws_s[e]cret_access_[k]ey))(["']?)\s*[:"#,
        "=",
        r#"]\s*(?:"[^"]*"|'[^']*'|[^\s,;]+)"#
    ));
    s = assignment
        .replace_all(&s, |c: &Captures| {
            format!("{}{}{}{}[REDACTED]", &c[1], &c[2], &c[3], char::from(61))
        })
        .into_owned();
    let shapes = pattern(concat!(
        r"(?:gh[pousr]_[A-Za-z0-9_.-]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-(?:live|test)?[_-]?[A-Za-z0-9_-]{10,}|",
        r"xox[bap]-[A-Za-z0-9-]{10,}|hf_[A-Za-z0-9]{20,}|n[t]n_[0-9]{11}[A-Za-z0-9]{35}|",
        r"[sr]k_(?:liv[e]|tes[t])_[A-Za-z0-9]{16,}|AK[I]A[0-9A-Z]{16}|AI[z]a[0-9A-Za-z_-]{35}|",
        r"glpa[t]-[A-Za-z0-9_-]{20,}|np[m]_[A-Za-z0-9]{36,}|\bey[J][A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,})"
    ));
    s = shapes.replace_all(&s, "[REDACTED]").into_owned();
    let urls =
        pattern(r"(^|[^A-Za-z0-9+.-])([0-9+.-]*)([A-Za-z][A-Za-z0-9+.-]*://)[^/\s@]+@([^/\s]+)");
    s = urls
        .replace_all(&s, "${1}${2}${3}[REDACTED]@${4}")
        .into_owned();
    let paths = pattern(concat!(r"(?:/ho", r"me|/Us", r"ers)/[^/\s]+(?:/[^\s]*)?"));
    s = paths.replace_all(&s, "<private-path>").into_owned();
    pattern(r"(?i)[A-Z]:\\Us[e]rs\\[^\\\s]+(?:\\[^\s]*)?")
        .replace_all(&s, "<private-path>")
        .into_owned()
}

pub fn redact_secrets(text: &str) -> String {
    redact_text(text)
}

pub fn truncate_chars(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    if limit <= 3 {
        return text.chars().take(limit).collect();
    }
    let prefix: String = text.chars().take(limit - 3).collect();
    format!("{}...", prefix.trim_end())
}

pub fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

pub(crate) fn canonical_truncate(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.into();
    }
    if maximum < "…".len() {
        return truncate_utf8_bytes(text, maximum);
    }
    format!("{}…", truncate_utf8_bytes(text, maximum - "…".len()))
}

pub fn sanitize_text(raw: &str, limit: usize) -> String {
    let controlled: String = raw
        .chars()
        .map(|c| if c >= ' ' || c == '\t' { c } else { ' ' })
        .collect();
    let clean = redact_text(&controlled);
    let folded = pattern(r"[ \t]+").replace_all(&clean, " ");
    canonical_truncate(folded.trim(), limit)
}

pub fn sanitize_multiline(raw: &str, limit: usize) -> String {
    let clean = pem_re().replace_all(raw, "[REDACTED]");
    let lines: Vec<String> = crate::unicode::text_lines(&clean)
        .map(|line| {
            let leading: String = line
                .chars()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .collect();
            let content = sanitize_text(&line[leading.len()..], 2000);
            if content.is_empty() {
                String::new()
            } else {
                format!("{}{}", leading.chars().take(8).collect::<String>(), content)
            }
        })
        .collect();
    canonical_truncate(&lines.join("\n"), limit)
}

pub fn bounded_summary(raw: &str, limit: usize) -> String {
    sanitize_multiline(raw, limit)
}

pub fn is_forbidden_raw_key(key: &str) -> bool {
    raw_field_re().is_match(key)
}

pub fn sanitize_value(value: &Value, key_hint: Option<&str>) -> Option<Value> {
    if key_hint.is_some_and(is_forbidden_raw_key) {
        return None;
    }
    if key_hint.is_some_and(sensitive_field) {
        return Some(Value::String("[REDACTED]".into()));
    }
    match value {
        Value::Null => None,
        Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(s) => Some(Value::String(sanitize_text(s, 2000))),
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .take(64)
                .filter_map(|v| sanitize_value(v, None))
                .collect(),
        )),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map.iter().take(64) {
                let key = sanitize_text(key, 120);
                if key.is_empty() || is_forbidden_raw_key(&key) {
                    continue;
                }
                if let Some(value) = sanitize_value(value, Some(&key)) {
                    out.insert(key, value);
                }
            }
            Some(Value::Object(out))
        }
    }
}

pub fn to_oms_json(value: &Value) -> Result<oms_json::Value, String> {
    match value {
        Value::Null => Ok(oms_json::Value::Null),
        Value::Bool(b) => Ok(oms_json::Value::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                oms_json::Value::integer(&i.to_string())
                    .map_err(|e| format!("invalid JSON integer: {e}"))
            } else if let Some(u) = n.as_u64() {
                oms_json::Value::integer(&u.to_string())
                    .map_err(|e| format!("invalid JSON integer: {e}"))
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    Ok(oms_json::Value::Float(f))
                } else {
                    Err("non-finite float in canonical JSON".to_string())
                }
            } else {
                Err("invalid JSON number".to_string())
            }
        }
        Value::String(s) => Ok(oms_json::Value::String(oms_json::PyString::from(
            s.as_str(),
        ))),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(to_oms_json(item)?);
            }
            Ok(oms_json::Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (k, v) in map {
                out.insert(oms_json::PyString::from(k.as_str()), to_oms_json(v)?);
            }
            Ok(oms_json::Value::Object(out))
        }
    }
}

pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, String> {
    let oms_val = to_oms_json(value)?;
    oms_json::canonical_json(&oms_val).map_err(|e| format!("canonical JSON error: {e}"))
}

pub fn canonical_json_string(value: &Value) -> Result<String, String> {
    let bytes = canonical_json_bytes(value)?;
    String::from_utf8(bytes).map_err(|e| format!("UTF-8 error: {e}"))
}

pub fn pretty_json_bytes(value: &Value) -> Result<Vec<u8>, String> {
    let oms_val = to_oms_json(value)?;
    oms_json::pretty_json(&oms_val).map_err(|e| format!("pretty JSON error: {e}"))
}

fn py_string_has_surrogate(s: &oms_json::PyString) -> bool {
    s.codepoints()
        .iter()
        .any(|&p| (0xd800..=0xdfff).contains(&p))
}

fn value_has_surrogate(root: &oms_json::Value) -> bool {
    let mut stack = vec![root];
    while let Some(v) = stack.pop() {
        match v {
            oms_json::Value::String(s) => {
                if py_string_has_surrogate(s) {
                    return true;
                }
            }
            oms_json::Value::Array(items) => stack.extend(items.iter()),
            oms_json::Value::Object(map) => {
                for (k, val) in map {
                    if py_string_has_surrogate(k) {
                        return true;
                    }
                    stack.push(val);
                }
            }
            _ => {}
        }
    }
    false
}

fn emit_py_string_surrogateescape(out: &mut Vec<u8>, s: &oms_json::PyString) -> Result<(), String> {
    out.push(b'"');
    for &p in s.codepoints() {
        match p {
            34 => out.extend_from_slice(b"\\\""),
            92 => out.extend_from_slice(b"\\\\"),
            8 => out.extend_from_slice(b"\\b"),
            9 => out.extend_from_slice(b"\\t"),
            10 => out.extend_from_slice(b"\\n"),
            12 => out.extend_from_slice(b"\\f"),
            13 => out.extend_from_slice(b"\\r"),
            0..=31 => out.extend_from_slice(format!("\\u{p:04x}").as_bytes()),
            0xdc80..=0xdcff => out.push((p - 0xdc00) as u8),
            0xd800..=0xdfff => {
                return Err(format!(
                    "pretty JSON error: lone surrogate codepoint U+{p:04X} cannot be encoded"
                ));
            }
            _ => {
                let ch = char::from_u32(p)
                    .ok_or_else(|| format!("pretty JSON error: invalid codepoint U+{p:04X}"))?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
    Ok(())
}

/// Serialize an `oms_json::Value` with `pretty_json` structure while encoding
/// Python `surrogateescape` codepoints (`U+DC80..=U+DCFF`) back to raw bytes
/// matching Python `json.dumps(..., ensure_ascii=False)` + UTF-8 `surrogateescape` stdout.
/// Lone surrogates outside the `surrogateescape` range still return an error.
pub fn pretty_json_with_surrogates(value: &oms_json::Value) -> Result<Vec<u8>, String> {
    if !value_has_surrogate(value) {
        return oms_json::pretty_json(value).map_err(|e| format!("pretty JSON error: {e}"));
    }

    enum Frame<'a> {
        Node(&'a oms_json::Value, usize),
        Array(std::slice::Iter<'a, oms_json::Value>, usize, bool),
        Object(
            std::collections::btree_map::Iter<'a, oms_json::PyString, oms_json::Value>,
            usize,
            bool,
        ),
    }

    fn push_separator(out: &mut Vec<u8>, first: bool, depth: usize) {
        if !first {
            out.push(b',');
        }
        out.push(b'\n');
        for _ in 0..depth {
            out.extend_from_slice(b"  ");
        }
    }

    let mut out = Vec::new();
    let mut stack = vec![Frame::Node(value, 0)];
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Node(node, depth) => match node {
                oms_json::Value::Null
                | oms_json::Value::Bool(_)
                | oms_json::Value::Integer(_)
                | oms_json::Value::Float(_) => {
                    let scalar = oms_json::canonical_json(node)
                        .map_err(|e| format!("pretty JSON error: {e}"))?;
                    out.extend_from_slice(&scalar);
                }
                oms_json::Value::String(s) => emit_py_string_surrogateescape(&mut out, s)?,
                oms_json::Value::Array(items) => {
                    if depth >= 1024 {
                        return Err("pretty JSON error: maximum JSON nesting depth exceeded".into());
                    }
                    out.push(b'[');
                    stack.push(Frame::Array(items.iter(), depth, true));
                }
                oms_json::Value::Object(items) => {
                    if depth >= 1024 {
                        return Err("pretty JSON error: maximum JSON nesting depth exceeded".into());
                    }
                    out.push(b'{');
                    stack.push(Frame::Object(items.iter(), depth, true));
                }
            },
            Frame::Array(mut items, depth, first) => {
                if let Some(item) = items.next() {
                    push_separator(&mut out, first, depth + 1);
                    stack.push(Frame::Array(items, depth, false));
                    stack.push(Frame::Node(item, depth + 1));
                } else {
                    if !first {
                        out.push(b'\n');
                        for _ in 0..depth {
                            out.extend_from_slice(b"  ");
                        }
                    }
                    out.push(b']');
                }
            }
            Frame::Object(mut items, depth, first) => {
                if let Some((key, item)) = items.next() {
                    push_separator(&mut out, first, depth + 1);
                    emit_py_string_surrogateescape(&mut out, key)?;
                    out.extend_from_slice(b": ");
                    stack.push(Frame::Object(items, depth, false));
                    stack.push(Frame::Node(item, depth + 1));
                } else {
                    if !first {
                        out.push(b'\n');
                        for _ in 0..depth {
                            out.extend_from_slice(b"  ");
                        }
                    }
                    out.push(b'}');
                }
            }
        }
    }
    out.push(b'\n');
    Ok(out)
}

#[cfg(unix)]
pub fn os_str_to_py_string(raw: &OsStr) -> oms_json::PyString {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = raw.as_bytes();
    let mut points = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                points.extend(s.chars().map(u32::from));
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                points.extend(
                    std::str::from_utf8(&bytes[..valid])
                        .expect("valid utf8 prefix")
                        .chars()
                        .map(u32::from),
                );
                let len = e.error_len().unwrap_or(bytes.len() - valid);
                points.extend(
                    bytes[valid..valid + len]
                        .iter()
                        .map(|&b| 0xdc00 + u32::from(b)),
                );
                bytes = &bytes[valid + len..];
            }
        }
    }
    oms_json::PyString::from_codepoints(points).expect("surrogateescape codepoints are valid")
}

#[cfg(not(unix))]
pub fn os_str_to_py_string(raw: &OsStr) -> oms_json::PyString {
    oms_json::PyString::from(raw.to_string_lossy().as_ref())
}

pub fn os_str_to_utf8<'a>(raw: &'a OsStr, label: &str) -> Result<&'a str, String> {
    raw.to_str()
        .ok_or_else(|| format!("{label} contains non-UTF-8 bytes"))
}

pub fn diagnostic_os_str(raw: &OsStr) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = raw.as_bytes();
        let mut out = String::new();
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.push_str(std::str::from_utf8(&bytes[..valid]).expect("valid utf8 prefix"));
                    let len = e.error_len().unwrap_or(bytes.len() - valid);
                    for &b in &bytes[valid..valid + len] {
                        use std::fmt::Write as _;
                        let _ = write!(&mut out, "\\udc{b:02x}");
                    }
                    bytes = &bytes[valid + len..];
                }
            }
        }
        out
    }
    #[cfg(not(unix))]
    {
        raw.to_string_lossy().into_owned()
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

/// Refuse a number the current public JSON value type would silently round.
/// The canonical Python contract has unbounded integers; this refusal is an
/// explicit interface limitation, never permission to quarantine or migrate it.
pub(crate) fn require_exact_integers(bytes: &[u8]) -> Result<(), String> {
    let mut index = 0;
    let mut quoted = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\x5c' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            quoted = true;
            index += 1;
            continue;
        }
        if byte == b'-' || byte.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_digit()
                    || matches!(bytes[index], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                index += 1;
            }
            let number = &bytes[start..index];
            let digits = number.strip_prefix(b"-").unwrap_or(number);
            if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                let text = std::str::from_utf8(number).map_err(|_| "invalid JSON number")?;
                let representable = if number.starts_with(b"-") {
                    text.parse::<i64>().is_ok()
                } else {
                    text.parse::<u64>().is_ok()
                };
                if !representable {
                    return Err("canonical integer exceeds the current native JSON interface; original data was not converted".into());
                }
            }
        } else {
            index += 1;
        }
    }
    Ok(())
}

pub(crate) fn default_json_bytes(value: &oms_json::Value) -> Result<Vec<u8>, String> {
    let input = pretty_json_with_surrogates(value)?;
    let mut output = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for byte in input {
        if quoted {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\x5c' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
            output.push(byte);
        } else if !byte.is_ascii_whitespace() {
            output.push(byte);
            if matches!(byte, b',' | b':') {
                output.push(b' ');
            }
        }
    }
    output.push(b'\n');
    Ok(output)
}
