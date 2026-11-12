#![forbid(unsafe_code)]

//! Read-only session acquisition. Files can change concurrently; metadata checks detect
//! observed changes but do not create an atomic filesystem snapshot. Unlike legacy
//! `_lines`, ledger parent symlinks are rejected and observed growth beyond the cap
//! or metadata changes fail closed. Transcript parent symlinks remain permitted.

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Missing,
    Unreadable,
    Unsafe,
    Oversize,
    Changed,
    Cancelled,
    Projection(oms_context_session::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("input is missing"),
            Self::Unreadable => f.write_str("input is unreadable"),
            Self::Unsafe => f.write_str("input is not a safe regular file"),
            Self::Oversize => f.write_str("input is too large"),
            Self::Changed => f.write_str("input changed during acquisition"),
            Self::Cancelled => f.write_str("acquisition cancelled"),
            Self::Projection(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<oms_context_session::Error> for Error {
    fn from(e: oms_context_session::Error) -> Self {
        Self::Projection(e)
    }
}
fn map(e: rustix::io::Errno) -> Error {
    match e {
        rustix::io::Errno::NOENT => Error::Missing,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Error::Unsafe,
        _ => Error::Unreadable,
    }
}
fn regular(s: &Stat) -> bool {
    FileType::from_raw_mode(s.st_mode) == FileType::RegularFile
}
fn stable(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev
        && a.st_ino == b.st_ino
        && a.st_mode == b.st_mode
        && a.st_size == b.st_size
        && a.st_uid == b.st_uid
        && a.st_nlink == b.st_nlink
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn read_at(
    parent: impl AsFd,
    name: &OsStr,
    cap: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Vec<u8>, Error> {
    if cancel() {
        return Err(Error::Cancelled);
    }
    let named = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map)?;
    if !regular(&named) {
        return Err(Error::Unsafe);
    }
    let fd = fs::openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        if e == rustix::io::Errno::NOENT {
            Error::Changed
        } else {
            map(e)
        }
    })?;
    let before = fs::fstat(&fd).map_err(map)?;
    if !stable(&named, &before) {
        return Err(Error::Changed);
    }
    if !regular(&before) || before.st_size < 0 {
        return Err(Error::Unsafe);
    }
    if before.st_size as u64 > cap as u64 {
        return Err(Error::Oversize);
    }
    let mut file = std::fs::File::from(fd);
    let mut data = Vec::new();
    let mut buf = [0; 65536];
    loop {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let nmax = buf
            .len()
            .min(cap.saturating_sub(data.len()).saturating_add(1));
        let n = match file.read(&mut buf[..nmax]) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Error::Unreadable),
        };
        if n == 0 {
            break;
        }
        if n > cap - data.len() {
            return Err(Error::Oversize);
        }
        data.extend_from_slice(&buf[..n]);
    }
    let after = fs::fstat(&file).map_err(|_| Error::Changed)?;
    let final_name =
        fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| Error::Changed)?;
    if !stable(&before, &after)
        || !stable(&after, &final_name)
        || after.st_size as u64 != data.len() as u64
    {
        return Err(Error::Changed);
    }
    Ok(data)
}
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn ledger(root: &Path, cancel: &impl Fn() -> bool) -> Result<Vec<u8>, Error> {
    if cancel() {
        return Err(Error::Cancelled);
    }
    let physical = std::fs::canonicalize(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Missing
        } else {
            Error::Unreadable
        }
    })?;
    let root_fd = fs::open(&physical, dir_flags(), Mode::empty()).map_err(map)?;
    let root_stat = fs::fstat(&root_fd).map_err(map)?;
    let mut dirs = vec![root_fd];
    for name in [".oms", "hooks"] {
        if cancel() {
            return Err(Error::Cancelled);
        }
        match fs::openat(dirs.last().unwrap(), name, dir_flags(), Mode::empty()) {
            Ok(fd) => dirs.push(fd),
            Err(e) if e == rustix::io::Errno::NOENT => break,
            Err(e) => return Err(map(e)),
        }
    }
    let data = if dirs.len() < 3 {
        Vec::new()
    } else {
        match read_at(
            dirs.last().unwrap(),
            OsStr::new("events.jsonl"),
            oms_context_session::MAX_LEDGER_BYTES,
            cancel,
        ) {
            Err(Error::Missing) => Vec::new(),
            other => other?,
        }
    };
    let selected = std::fs::canonicalize(root).map_err(|_| Error::Changed)?;
    let now = fs::open(&selected, dir_flags(), Mode::empty()).map_err(|_| Error::Changed)?;
    let now = fs::fstat(now).map_err(|_| Error::Changed)?;
    if root_stat.st_dev != now.st_dev || root_stat.st_ino != now.st_ino {
        return Err(Error::Changed);
    }
    for (i, name) in [".oms", "hooks"].iter().take(dirs.len() - 1).enumerate() {
        let named =
            fs::statat(&dirs[i], *name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| Error::Changed)?;
        let opened = fs::fstat(&dirs[i + 1]).map_err(|_| Error::Changed)?;
        if named.st_dev != opened.st_dev
            || named.st_ino != opened.st_ino
            || FileType::from_raw_mode(named.st_mode) != FileType::Directory
        {
            return Err(Error::Changed);
        }
    }
    Ok(data)
}
fn transcript(path: &Path, cancel: &impl Fn() -> bool) -> Result<Vec<u8>, Error> {
    if path.as_os_str().is_empty() {
        return Err(Error::Projection(oms_context_session::Error::Invalid(
            "--session needs a transcript path",
        )));
    }
    let tail = path
        .as_os_str()
        .as_bytes()
        .rsplit(|b| *b == b'/')
        .next()
        .ok_or(Error::Unsafe)?;
    if tail.is_empty() || tail == b"." || tail == b".." {
        return Err(Error::Unsafe);
    }
    read_at(
        fs::CWD,
        path.as_os_str(),
        oms_context_session::MAX_TRANSCRIPT_BYTES,
        cancel,
    )
}
/// Resolve a UTF-8 transcript label, returning only UTF-8 labels. Session inspection
/// separately retains surrogate-escaped filesystem names until display.
/// This is a display label, not an access check; filesystem changes can race resolution.
/// Unlike legacy `_repo_label`, non-directory roots and symlink loops are rejected.
/// Expansion also fails closed after 16 MiB of cumulative candidate-path and link-target
/// bytes, cached resolved-path bytes (plus one byte per item), bounding work and pending components.
pub fn repo_label(root: &Path, raw: &str) -> Option<String> {
    if raw.is_empty() || raw.chars().count() > 4096 || raw.chars().any(|c| c < ' ') {
        return None;
    }
    let root = std::fs::canonicalize(root).ok()?;
    if !std::fs::metadata(&root).ok()?.is_dir() {
        return None;
    }
    String::from_utf8(resolve_label(&root, raw.as_bytes(), &|| false)?).ok()
}
fn path_bytes(raw: &[u8]) -> Option<OsString> {
    let mut output = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == 0xed && i + 2 < raw.len() && (0xa0..=0xbf).contains(&raw[i + 1]) {
            let second = raw[i + 1];
            let third = raw[i + 2];
            if !(0x80..=0xbf).contains(&third) || !(0xb2..=0xb3).contains(&second) {
                return None;
            }
            output.push(if second == 0xb2 { third } else { third + 0x40 });
            i += 3;
        } else {
            output.push(raw[i]);
            i += 1;
        }
    }
    Some(OsString::from_vec(output))
}
fn label_bytes(raw: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut remaining = raw;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(_) => {
                output.extend_from_slice(remaining);
                break;
            }
            Err(e) => {
                let n = e.valid_up_to();
                output.extend_from_slice(&remaining[..n]);
                let count = e.error_len().unwrap_or(remaining.len() - n);
                for b in &remaining[n..n + count] {
                    output.extend_from_slice(&[
                        0xed,
                        if *b < 0xc0 { 0xb2 } else { 0xb3 },
                        0x80 | (*b & 0x3f),
                    ]);
                }
                remaining = &remaining[n + count..];
            }
        }
    }
    output
}
fn missing_label_component(error: &std::io::Error, path: &Path, macos: bool) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    ) || (macos
        && path.to_str().is_none()
        && matches!(error.raw_os_error(), Some(code) if code == rustix::io::Errno::ILSEQ.raw_os_error() || code == rustix::io::Errno::INVAL.raw_os_error()))
}
fn resolve_label(root: &Path, raw: &[u8], cancel: &impl Fn() -> bool) -> Option<Vec<u8>> {
    if raw.is_empty()
        || raw.iter().filter(|b| **b & 0xc0 != 0x80).count() > 4096
        || raw.iter().any(|c| *c < b' ')
    {
        return None;
    }
    let raw = path_bytes(raw)?;
    let mut resolved = if Path::new(&raw).is_absolute() {
        PathBuf::from("/")
    } else {
        root.to_path_buf()
    };
    enum Step {
        Part(OsString),
        End(PathBuf),
    }
    let mut parts: VecDeque<Step> = Path::new(&raw)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(Step::Part(s.to_owned())),
            Component::ParentDir => Some(Step::Part(OsString::from(".."))),
            _ => None,
        })
        .collect();
    let mut seen: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut work = 0_usize;
    while let Some(step) = parts.pop_front() {
        if cancel() {
            return None;
        }
        let part = match step {
            Step::End(path) => {
                seen.insert(path, Some(resolved.clone()));
                continue;
            }
            Step::Part(part) => part,
        };
        if part == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        work = work.checked_add(candidate.as_os_str().as_bytes().len().checked_add(1)?)?;
        if work > oms_context_session::MAX_LINE_BYTES {
            return None;
        }
        if let Some(entry) = seen.get(&candidate) {
            let cached = entry.as_ref()?;
            work = work.checked_add(cached.as_os_str().as_bytes().len().checked_add(1)?)?;
            if work > oms_context_session::MAX_LINE_BYTES {
                return None;
            }
            resolved = cached.clone();
            continue;
        }
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                seen.insert(candidate.clone(), None);
                let target = std::fs::read_link(&candidate).ok()?;
                work = work.checked_add(target.as_os_str().as_bytes().len().checked_add(1)?)?;
                if work > oms_context_session::MAX_LINE_BYTES {
                    return None;
                }
                if target.is_absolute() {
                    resolved = PathBuf::from("/");
                }
                let mut add: VecDeque<Step> = target
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => Some(Step::Part(s.to_owned())),
                        Component::ParentDir => Some(Step::Part(OsString::from(".."))),
                        _ => None,
                    })
                    .collect();
                add.push_back(Step::End(candidate));
                add.append(&mut parts);
                parts = add;
            }
            Ok(_) => resolved.push(part),
            Err(e) if missing_label_component(&e, &candidate, cfg!(target_os = "macos")) => {
                resolved.push(part)
            }
            Err(_) => return None,
        }
    }
    let rel = resolved.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() {
        return Some(b".".to_vec());
    }
    let label = rel.as_os_str().as_bytes();
    if label.iter().any(|c| *c < b' ') {
        return None;
    }
    Some(label_bytes(label))
}
/// The callback returns true to cancel; it is checked before and during acquisition.
pub fn inspect_injections(
    root: &Path,
    session: Option<&str>,
    limit: usize,
    cancel: impl Fn() -> bool,
) -> Result<serde_json::Value, Error> {
    // Validate options even if the ledger is absent or unreadable.
    oms_context_session::inspect_injections(&[], session, limit)?;
    let data = ledger(root, &cancel)?;
    Ok(oms_context_session::inspect_injections(
        &data, session, limit,
    )?)
}
pub fn inspect_session(
    root: &Path,
    path: &Path,
    limit: usize,
    cancel: impl Fn() -> bool,
) -> Result<serde_json::Value, Error> {
    oms_context_session::inspect_session(&[], limit, |_| None)?;
    if path.as_os_str().is_empty() {
        return Err(Error::Projection(oms_context_session::Error::Invalid(
            "--session needs a transcript path",
        )));
    }
    if cancel() {
        return Err(Error::Cancelled);
    }
    let physical = std::fs::canonicalize(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Missing
        } else {
            Error::Unreadable
        }
    })?;
    let fd = fs::open(&physical, dir_flags(), Mode::empty()).map_err(map)?;
    let initial = fs::fstat(&fd).map_err(map)?;
    let data = transcript(path, &cancel)?;
    let cancelled = Cell::new(false);
    let value = oms_context_session::inspect_session_lossless(&data, limit, |raw| {
        resolve_label(&physical, raw, &|| {
            let stopped = cancel();
            if stopped {
                cancelled.set(true);
            }
            stopped
        })
    });
    if cancelled.get() || cancel() {
        return Err(Error::Cancelled);
    }
    let value = value?;
    let current = std::fs::canonicalize(root).map_err(|_| Error::Changed)?;
    let now = fs::open(&current, dir_flags(), Mode::empty()).map_err(|_| Error::Changed)?;
    let now = fs::fstat(now).map_err(|_| Error::Changed)?;
    if initial.st_dev != now.st_dev || initial.st_ino != now.st_ino {
        return Err(Error::Changed);
    }
    Ok(value)
}
pub fn inspect_timeline(
    path: &Path,
    rows: usize,
    cancel: impl Fn() -> bool,
) -> Result<serde_json::Value, Error> {
    oms_context_session::inspect_timeline(&[], rows)?;
    let data = transcript(path, &cancel)?;
    Ok(oms_context_session::inspect_timeline(&data, rows)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn macos_invalid_byte_metadata_errors_are_lexical() {
        let raw = Path::new(OsStr::from_bytes(b"bad\x80"));
        let valid = Path::new("valid");
        for code in [rustix::io::Errno::ILSEQ, rustix::io::Errno::INVAL] {
            let error = std::io::Error::from_raw_os_error(code.raw_os_error());
            assert!(missing_label_component(&error, raw, true));
            assert!(!missing_label_component(&error, valid, true));
            assert!(!missing_label_component(&error, raw, false));
        }
        let denied = std::io::Error::from_raw_os_error(rustix::io::Errno::ACCESS.raw_os_error());
        assert!(!missing_label_component(&denied, raw, true));
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(missing_label_component(&missing, valid, false));
    }
}
