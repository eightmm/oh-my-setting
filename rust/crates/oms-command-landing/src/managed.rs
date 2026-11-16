//! Read-only ownership proofs for install copy fallbacks. A marker alone never
//! proves ownership: its current bytes and the expected source must agree.
use crate::{Error, Result, V, digest, require, storage, string};
use sha2::{Digest, Sha256};
use std::{
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
pub fn marker_path(target: &Path) -> Result<PathBuf> {
    let name = target
        .file_name()
        .ok_or_else(|| Error::new(2, "managed target has no filename"))?;
    let mut marker = std::ffi::OsString::from(".");
    marker.push(name);
    marker.push(".oh-my-setting-managed.json");
    Ok(target.with_file_name(marker))
}
pub fn path_hash(path: &Path) -> Result<(&'static str, String)> {
    let before = std::fs::symlink_metadata(path)?;
    require(
        !before.file_type().is_symlink(),
        "managed copies cannot contain a symlink root",
    )?;
    if before.is_file() {
        let parent = path
            .parent()
            .ok_or_else(|| Error::new(2, "managed file has no parent"))?;
        let root = storage::root(parent)?;
        let body = root
            .read_no_follow(
                Path::new(path.file_name().unwrap()),
                oms_safe_fs::ByteReadOptions::default(),
            )
            .map_err(|_| Error::new(2, "managed file cannot be safely read"))?;
        require(
            oms_reference::storage::generation(&before, &std::fs::symlink_metadata(path)?),
            "managed file changed while hashing",
        )?;
        let mut bytes = b"file\0".to_vec();
        bytes.extend(body);
        return Ok(("file", digest(&bytes)));
    }
    require(before.is_dir(), "managed target is not a file or directory")?;
    let root = storage::root(path)?;
    let mut hash = Sha256::new();
    hash.update(b"directory\0");
    hash_directory(path, Path::new(""), &root, &mut hash, 0)?;
    require(
        oms_reference::storage::same(&before, &std::fs::symlink_metadata(path)?),
        "managed root changed while hashing",
    )?;
    Ok(("directory", format!("{:x}", hash.finalize())))
}
fn hash_directory(
    base: &Path,
    relative: &Path,
    root: &oms_safe_fs::Root,
    hash: &mut Sha256,
    depth: usize,
) -> Result<()> {
    require(depth < 256, "managed copy exceeds directory depth bound")?;
    let path = base.join(relative);
    let initial = std::fs::symlink_metadata(&path)?;
    require(
        initial.is_dir() && !initial.file_type().is_symlink(),
        "managed copies cannot contain symlinks",
    )?;
    let mut directories = vec![];
    let mut files = vec![];
    for entry in std::fs::read_dir(&path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        require(!kind.is_symlink(), "managed copies cannot contain symlinks")?;
        if kind.is_dir() {
            directories.push(entry.file_name());
        } else {
            require(kind.is_file(), "unsupported managed copy file type")?;
            files.push(entry.file_name());
        }
    }
    directories.sort();
    files.sort();
    for name in &directories {
        let rel = relative.join(name);
        hash.update(b"D\0");
        hash.update(rel.as_os_str().as_bytes());
        hash.update(b"\0");
    }
    for name in files {
        let rel = relative.join(name);
        hash.update(b"F\0");
        hash.update(rel.as_os_str().as_bytes());
        hash.update(b"\0");
        let bytes = root
            .read_no_follow(&rel, oms_safe_fs::ByteReadOptions::default())
            .map_err(|_| Error::new(2, "managed copy file cannot be safely read"))?;
        hash.update(bytes);
        hash.update(b"\0");
    }
    require(
        oms_reference::storage::generation(&initial, &std::fs::symlink_metadata(&path)?),
        "managed directory changed while hashing",
    )?;
    for name in directories {
        hash_directory(base, &relative.join(name), root, hash, depth + 1)?;
    }
    Ok(())
}
pub fn read_marker(target: &Path) -> Result<Option<V>> {
    let raw = storage::read_external(&marker_path(target)?, true)?;
    if raw.is_empty() {
        return Ok(None);
    }
    let row = V::parse(&raw)?;
    require(
        row.get("schema") == &1usize.into()
            && ["file", "directory"].contains(&string(&row, "kind").as_str())
            && row.get("source").text().is_some()
            && crate::hex(&string(&row, "sha256"), 64),
        "invalid managed copy marker",
    )?;
    if row.has("source_sha256") {
        require(
            string(&row, "kind") == "file" && crate::hex(&string(&row, "source_sha256"), 64),
            "invalid managed source digest",
        )?;
    }
    Ok(Some(row))
}
pub fn intact_copy(target: &Path, row: &V) -> Result<bool> {
    let (kind, hash) = path_hash(target)?;
    Ok(kind == string(row, "kind") && hash == string(row, "sha256"))
}
pub fn inspect_copy(source: &Path, target: &Path) -> Result<&'static str> {
    let Some(row) = read_marker(target)? else {
        return Ok("foreign");
    };
    let stored = PathBuf::from(string(&row, "source"));
    let expected = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_owned());
    if stored != expected {
        return Ok("foreign");
    }
    if !intact_copy(target, &row)? {
        return Ok("modified");
    }
    match path_hash(source) {
        Ok((kind, hash))
            if kind == string(&row, "kind")
                && hash
                    == row
                        .get("source_sha256")
                        .text()
                        .unwrap_or_else(|| row.get("sha256").text().unwrap_or("")) =>
        {
            Ok("current")
        }
        _ => Ok("owned"),
    }
}
pub fn owned_source_under(root: &Path, target: &Path) -> Result<Option<PathBuf>> {
    let Some(row) = read_marker(target)? else {
        return Ok(None);
    };
    if !intact_copy(target, &row)? {
        return Ok(None);
    }
    let root = std::fs::canonicalize(root)?;
    let source = PathBuf::from(string(&row, "source"));
    Ok(source.starts_with(root).then_some(source))
}
