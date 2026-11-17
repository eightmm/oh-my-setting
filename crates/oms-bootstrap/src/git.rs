//! HEAD resolution only; never runs hooks, Git, or receipt-selected programs.
use crate::filesystem::Anchor;
use crate::{Error, hex};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

fn line(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}
fn location(base: &Path, bytes: &[u8]) -> Result<PathBuf, Error> {
    if bytes.is_empty() || bytes.contains(&0) || bytes.contains(&b'\n') {
        return Err(Error::integrity("invalid native Git directory"));
    }
    let value = Path::new(std::ffi::OsStr::from_bytes(bytes));
    Ok(if value.is_absolute() {
        value.into()
    } else {
        base.join(value)
    })
}
pub(crate) fn revision(root: &Anchor) -> Result<String, Error> {
    let dot = root.path().join(".git");
    let metadata = std::fs::symlink_metadata(&dot)
        .map_err(|_| Error::integrity("cannot determine native payload revision"))?;
    let git = if metadata.is_dir() && !metadata.file_type().is_symlink() {
        root.descend(Path::new(".git"))?
    } else {
        let record = root.file(Path::new(".git"), false)?;
        let bytes = record.read(64 * 1024)?;
        let path = line(&bytes)
            .strip_prefix(b"gitdir: ")
            .ok_or_else(|| Error::integrity("invalid native Git directory"))?;
        Anchor::open(&location(root.path(), path)?)?
    };
    let common = if git.exists(Path::new("commondir"))? {
        let bytes = git.file(Path::new("commondir"), false)?.read(64 * 1024)?;
        Anchor::open(&location(git.path(), line(&bytes))?)?
    } else {
        Anchor::open(git.path())?
    };
    let mut value = git.file(Path::new("HEAD"), false)?.read(64 * 1024)?;
    for depth in 0..=32 {
        let text = line(&value);
        if hex(text, 40) {
            return Ok(String::from_utf8(text.to_vec()).expect("hex revision"));
        }
        let name = text
            .strip_prefix(b"ref: ")
            .ok_or_else(|| Error::integrity("invalid native source revision"))?;
        let path = Path::new(std::ffi::OsStr::from_bytes(name));
        if !name.starts_with(b"refs/")
            || name.contains(&0)
            || name.iter().any(u8::is_ascii_whitespace)
            || name.iter().any(|b| {
                *b < 0x20 || matches!(b, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\' | 0x7f)
            })
            || name
                .windows(2)
                .any(|pair| matches!(pair, b".." | b"@{" | b"//"))
            || name.split(|b| *b == b'/').any(|part| {
                part.starts_with(b".") || part.ends_with(b".") || part.ends_with(b".lock")
            })
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Error::integrity("invalid native source reference"));
        }
        if depth == 32 {
            break;
        }
        let authority = if name.starts_with(b"refs/worktree/")
            || name.starts_with(b"refs/bisect/")
            || name.starts_with(b"refs/rewritten/")
        {
            &git
        } else {
            &common
        };
        if authority.exists(path)? {
            value = authority.file(path, false)?.read(64 * 1024)?;
            continue;
        }
        if common.exists(Path::new("packed-refs"))? {
            let bytes = common
                .file(Path::new("packed-refs"), false)?
                .read(16 * 1024 * 1024)?;
            let mut selected = None;
            for row in bytes.split(|b| *b == b'\n') {
                if row.first().is_none_or(|b| matches!(b, b'#' | b'^')) {
                    continue;
                }
                let Some(index) = row.iter().position(|b| *b == b' ') else {
                    return Err(Error::integrity("invalid native packed references"));
                };
                let (hash, reference) = (&row[..index], &row[index + 1..]);
                if reference == name {
                    if !hex(hash, 40) || selected.is_some() {
                        return Err(Error::integrity("invalid native packed source reference"));
                    }
                    selected = Some(String::from_utf8(hash.to_vec()).expect("hex revision"));
                }
            }
            if let Some(revision) = selected {
                return Ok(revision);
            }
        }
        return Err(Error::integrity("cannot determine native payload revision"));
    }
    Err(Error::integrity(
        "native source reference chain exceeds limit",
    ))
}
