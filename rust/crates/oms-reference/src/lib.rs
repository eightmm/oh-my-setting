//! Native reference publication and managed capture evidence.
//! The caller retains command execution, publication and admission authority.
#![forbid(unsafe_code)]

pub mod actions;
pub mod binding;
pub mod capture;
pub mod process;
pub mod producer;
pub mod publication;
pub mod repository;
pub mod scope;
pub mod storage;

pub use oms_command_operations::value::{V, obj};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};
pub type Result<T> = std::result::Result<T, String>;

pub fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
pub(crate) fn checkpoint(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err("interrupted".into())
    } else {
        Ok(())
    }
}
pub(crate) fn now() -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
pub(crate) fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn text<'a>(value: &'a V, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .text()
        .ok_or_else(|| format!("invalid {key}"))
}
pub(crate) fn path_value(path: &Path) -> V {
    use std::os::unix::ffi::OsStrExt;
    V::Str(path.as_os_str().as_bytes().to_vec())
}

/// Python's capture encoding uses sorted keys, compact separators, ASCII
/// escapes and a final newline. Lifecycle JSON has a different Unicode policy.
pub fn encoded(value: &V) -> Result<Vec<u8>> {
    let raw = value.json(false)?;
    let input = std::str::from_utf8(&raw).map_err(|_| "invalid capture JSON text")?;
    let mut out = String::new();
    for ch in input.chars() {
        if ch <= '~' {
            out.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                use std::fmt::Write;
                write!(&mut out, "\\u{unit:04x}").map_err(|e| e.to_string())?;
            }
        }
    }
    out.push('\n');
    Ok(out.into_bytes())
}
