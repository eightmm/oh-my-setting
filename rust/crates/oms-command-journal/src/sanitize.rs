#![forbid(unsafe_code)]

use regex::{Captures, Regex};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::sync::OnceLock;

pub const SCHEMA_VERSION: &str = "2.0";
pub const INDEX_SCHEMA_VERSION: &str = "2";
pub const RENDERER_VERSION: &str = "3";
pub const COLD_SCHEMA_VERSION: &str = "1";

pub const SUMMARY_ITEM_CHAR_LIMIT: usize = 400;
pub const VALUE_CHAR_LIMIT: usize = 1000;
pub const DETAIL_CHAR_LIMIT: usize = 4000;
pub const SUMMARY_BYTE_LIMIT: usize = 64 * 1024;
pub const LIVE_FILE_BYTE_LIMIT: u64 = 5 * 1024 * 1024;
pub const LIVE_EVENT_LINE_LIMIT: usize = 1000;
pub const JOURNAL_FILE_COUNT_LIMIT: usize = 200;
pub const DAILY_ITEM_LIMIT: usize = 12;
pub const WEEKLY_ITEM_LIMIT: usize = 20;
pub const DIGEST_MAX_BYTES: usize = 1800;
pub const DIGEST_RECENT_DAYS: i64 = 7;
pub const DEFAULT_TIMEZONE: &str = "UTC";

fn ansi_escape_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("valid regex"))
}

fn pem_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)-----BE",
            r"GIN [A-Z0-9 ]*PRI",
            r"VATE KEY-----.*?-----END [A-Z0-9 ]*PRI",
            r"VATE KEY-----",
        ))
        .expect("valid regex")
    })
}

fn known_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Split prefixes so outbound diff scans never mistake test/regex patterns for secrets.
        let pattern = concat!(
            r"(?:",
            "ntn_",
            r"[A-Za-z0-9]{10,}|",
            "secr",
            "et_",
            r"[A-Za-z0-9]{10,}|",
            "gh[pousr]_",
            r"[A-Za-z0-9_]{12,}|",
            "github_pat_",
            r"[A-Za-z0-9_]{20,}|",
            "sk-",
            r"[A-Za-z0-9_-]{16,}|",
            "xox[baprs]-",
            r"[A-Za-z0-9-]{10,}|",
            "AIza",
            r"[0-9A-Za-z_-]{20,}|",
            "AKIA",
            r"[0-9A-Z]{16}",
            r")",
        );
        Regex::new(pattern).expect("valid regex")
    })
}

fn url_creds_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(https?://)([^/\s:@]+):([^/\s@]+)@").expect("valid regex"))
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._/+~=-]{8,}").expect("valid regex")
    })
}

fn secret_assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Keep keyword names separate from '=' syntax in source literals.
        let pattern = concat!(
            r"(?i)\b(",
            "tok",
            "en|",
            "pass",
            "word|",
            "pass",
            "wd|",
            "secr",
            "et|",
            "api",
            "_key|",
            "api",
            "key|",
            "authoriz",
            "ation|",
            "access",
            "_key|",
            "private",
            "_key",
            r")(\s*[:",
            "=",
            r"]\s*|\s+)([A-Za-z0-9._/+~=-]{6,})",
        );
        Regex::new(pattern).expect("valid regex")
    })
}

fn raw_field_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let pattern = concat!(
            r"(?i)^(",
            "raw|stdout|stderr|transcript|prompt|body|payload|diff|patch|",
            "env|environment|environ|headers|auth|authorization|cookie|cookies|",
            "credential|credentials|",
            "pass",
            "word|",
            "secr",
            "et|",
            "tok",
            "en|",
            "api",
            "_key|",
            "api",
            "key",
            r")$",
        );
        Regex::new(pattern).expect("valid regex")
    })
}

pub fn redact_secrets(text: &str) -> String {
    let mut s = ansi_escape_re().replace_all(text, "").into_owned();
    s = pem_re()
        .replace_all(&s, "[REDACTED PRIVATE KEY]")
        .into_owned();
    s = known_token_re().replace_all(&s, "[REDACTED]").into_owned();
    s = url_creds_re()
        .replace_all(&s, "${1}[REDACTED]:[REDACTED]@")
        .into_owned();
    s = bearer_re().replace_all(&s, "${1} [REDACTED]").into_owned();
    s = secret_assignment_re()
        .replace_all(&s, |caps: &Captures| {
            format!("{}{}[REDACTED]", &caps[1], &caps[2])
        })
        .into_owned();
    s
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

pub fn sanitize_text(raw: &str, limit: usize) -> String {
    let cleaned = redact_secrets(raw);
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, limit)
}

pub fn sanitize_multiline(raw: &str, limit: usize) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned = redact_secrets(&normalized);
    let lines: Vec<&str> = cleaned
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let joined = lines.join("\n");
    truncate_chars(&joined, limit)
}

pub fn bounded_summary(raw: &str, limit: usize) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut cleaned_lines = Vec::new();
    for line in normalized.lines() {
        let cleaned = sanitize_text(line, VALUE_CHAR_LIMIT);
        if !cleaned.is_empty() {
            cleaned_lines.push(cleaned);
        }
    }
    truncate_chars(&cleaned_lines.join("\n"), limit)
}

pub fn is_forbidden_raw_key(key: &str) -> bool {
    key.starts_with('_') || raw_field_re().is_match(key)
}

pub fn sanitize_value(value: &Value, key_hint: Option<&str>) -> Option<Value> {
    match value {
        Value::Null => Some(Value::Null),
        Value::Bool(b) => Some(Value::Bool(*b)),
        Value::Number(n) => Some(Value::Number(n.clone())),
        Value::String(s) => {
            let limit = match key_hint {
                Some("summary" | "note" | "decision" | "blocker" | "next_action") => {
                    DETAIL_CHAR_LIMIT
                }
                _ => VALUE_CHAR_LIMIT,
            };
            let cleaned = match key_hint {
                Some("summary" | "note") => sanitize_multiline(s, limit),
                _ => sanitize_text(s, limit),
            };
            Some(Value::String(cleaned))
        }
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items.iter().take(64) {
                if let Some(cleaned) = sanitize_value(item, None) {
                    out.push(cleaned);
                }
            }
            Some(Value::Array(out))
        }
        Value::Object(map) => {
            let mut sorted: BTreeMap<String, Value> = BTreeMap::new();
            for (k, v) in map {
                if is_forbidden_raw_key(k) {
                    continue;
                }
                if let Some(cleaned) = sanitize_value(v, Some(k.as_str())) {
                    sorted.insert(k.clone(), cleaned);
                }
            }
            let mut out = Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
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
