//! Native memory command and domain adapters. The entrypoint supplies a validated
//! installation root; it never derives executable authority from memory records.
#![forbid(unsafe_code)]
mod adapter;
mod carry;
mod cli;
mod os;
pub use adapter::{
    append_project_note, provider_context, run_database, run_review, run_review_with_stdin,
};
mod conflicts;
mod db;
mod diagnostics;
mod hygiene;
mod records;
mod redact;
mod review;
mod sources;
mod store;
mod unicode;
mod workflows;

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub type Output = (i32, Vec<u8>, Vec<u8>);
#[derive(Debug)]
pub(crate) struct Error(pub i32, pub String);
pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Error(2, message.into()))
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self(2, "memory: filesystem operation failed".into())
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self(
            2,
            format!(
                "memory database: {e}\nhint: run `oms agent-memory --repo . rebuild` from the project root"
            ),
        )
    }
}
/// Classification performs no IO and includes malformed invocations.
pub fn owns(_: &[OsString]) -> bool {
    true
}
/// Executes without changing process-global cwd or environment. Stdin is read
/// only for --stdin and the internal capture adapter.
pub fn run_with_source(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
) -> Output {
    match cli::execute(args, cwd, source_root, None, interrupted) {
        Ok(v) => v,
        Err(Error(code, message)) => (code, Vec::new(), format!("error: {message}\n").into_bytes()),
    }
}
/// Same as [`run_with_source`], reading `--stdin` inputs from `stdin` instead of process stdin.
pub fn run_with_source_and_stdin(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    stdin: &[u8],
    interrupted: &AtomicBool,
) -> Output {
    match cli::execute(args, cwd, source_root, Some(stdin), interrupted) {
        Ok(v) => v,
        Err(Error(code, message)) => (code, Vec::new(), format!("error: {message}\n").into_bytes()),
    }
}
pub(crate) fn check(flag: &AtomicBool) -> Result<()> {
    if flag.load(Ordering::Acquire) {
        Err(Error(130, "memory: interrupted".into()))
    } else {
        Ok(())
    }
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    stamp(secs as i64)
}
pub(crate) fn stamp(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|v| v.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub(crate) fn timestamp(s: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .ok()
        .map(|d| d.and_utc().timestamp())
}
pub(crate) fn fold(s: &str) -> String {
    unicode::casefold(s)
}
pub(crate) fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub(crate) fn clip(s: &str, chars: usize) -> String {
    s.chars().take(chars).collect()
}
pub(crate) fn byte_clip(s: &str, bytes: usize) -> &str {
    let mut end = bytes.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
pub(crate) fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").into()
}
pub(crate) fn parse_json(bytes: &[u8]) -> Option<Value> {
    fn convert(v: &oms_context::Json) -> Value {
        use oms_context::Json as J;
        match v {
            J::Null => Value::Null,
            J::Bool(b) => (*b).into(),
            J::Number(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            J::String(s) => String::from_utf8_lossy(s).into_owned().into(),
            J::Array(a) => Value::Array(a.iter().map(convert).collect()),
            J::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), convert(v)))
                    .collect(),
            ),
        }
    }
    oms_context::parse_json(bytes).ok().as_ref().map(convert)
}
pub(crate) fn json_bytes(v: &Value) -> Result<Vec<u8>> {
    fn convert(v: &Value) -> oms_json::Value {
        use oms_json::Value as J;
        match v {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::String(s) => J::String(s.as_str().into()),
            Value::Number(n) => oms_json::Value::integer(&n.to_string())
                .unwrap_or_else(|_| J::Float(n.as_f64().unwrap_or(0.0))),
            Value::Array(a) => J::Array(a.iter().map(convert).collect()),
            Value::Object(o) => J::Object(
                o.iter()
                    .map(|(k, v)| (k.as_str().into(), convert(v)))
                    .collect(),
            ),
        }
    }
    oms_json::canonical_json(&convert(v))
        .map_err(|_| Error(2, "memory: JSON encoding failed".into()))
}
pub(crate) fn json_line(v: &Value) -> Result<Vec<u8>> {
    let raw = json_bytes(v)?;
    let mut out = Vec::with_capacity(raw.len() + 32);
    let (mut quoted, mut escaped) = (false, false);
    for b in raw {
        out.push(b);
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
        } else if b == b'"' {
            quoted = true;
        } else if b == b':' || b == b',' {
            out.push(b' ');
        }
    }
    out.push(b'\n');
    Ok(out)
}
