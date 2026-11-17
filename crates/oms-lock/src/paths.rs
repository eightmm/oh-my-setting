use crate::{Error, Settings};
use rustix::fs::{self, Access, AtFlags};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub(crate) fn validate_path(path: &Path) -> Result<(), Error> {
    if path.as_os_str().as_bytes().contains(&0)
        || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(Error::InvalidTarget);
    }
    Ok(())
}

pub(crate) fn flock_available(settings: &Settings) -> bool {
    std::env::split_paths(&settings.search_path).any(|dir| {
        let path = if dir.is_absolute() {
            dir
        } else {
            settings.cwd.join(dir)
        }
        .join("flock");
        std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            && fs::accessat(fs::CWD, &path, Access::EXEC_OK, AtFlags::EACCESS).is_ok()
    })
}

/// POSIX cksum (not reflected CRC32), over the exact pathname bytes.
pub(crate) fn cksum(bytes: &[u8]) -> String {
    fn step(mut crc: u32, byte: u8) -> u32 {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
        crc
    }
    let mut crc = 0;
    for byte in bytes {
        crc = step(crc, *byte);
    }
    let mut len = bytes.len();
    while len != 0 {
        crc = step(crc, len as u8);
        len >>= 8;
    }
    format!("{}-{}", !crc, bytes.len())
}

fn beneath(path: &[u8], root: &[u8]) -> bool {
    path.starts_with(root) && path.get(root.len()) == Some(&b'/')
}

pub(crate) fn derive(target: &Path, settings: &Settings) -> Result<PathBuf, Error> {
    if target.as_os_str().is_empty() {
        return Err(Error::InvalidTarget);
    }
    // PathBuf::join normalizes some separators: preserve Bash's exact byte spelling.
    let mut absolute = Vec::new();
    if !target.is_absolute() {
        absolute.extend_from_slice(settings.cwd.as_os_str().as_bytes());
        absolute.push(b'/');
    }
    absolute.extend_from_slice(target.as_os_str().as_bytes());
    if absolute.contains(&0) {
        return Err(Error::InvalidTarget);
    }
    let mut leaf = absolute.as_slice();
    while leaf.len() > 1 && leaf.last() == Some(&b'/') {
        leaf = &leaf[..leaf.len() - 1];
    }
    let name = if leaf == b"/" {
        b"/".as_slice()
    } else {
        leaf.rsplit(|b| *b == b'/').next().unwrap_or_default()
    };
    let mut filename: Vec<u8> = name
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(b) {
                *b
            } else {
                b'_'
            }
        })
        .collect();
    filename.extend_from_slice(format!(".{}.lock", cksum(&absolute)).as_bytes());
    let root = if let Some(root) = &settings.lock_dir {
        if root.is_absolute() {
            root.clone()
        } else {
            settings.cwd.join(root)
        }
    } else {
        let tmp = settings
            .temp_dir
            .as_os_str()
            .as_bytes()
            .strip_suffix(b"/")
            .unwrap_or(settings.temp_dir.as_os_str().as_bytes());
        let home = settings
            .home
            .as_os_str()
            .as_bytes()
            .strip_suffix(b"/")
            .unwrap_or(settings.home.as_os_str().as_bytes());
        let temp_root = if beneath(&absolute, tmp) {
            Some(Path::new(OsStr::from_bytes(tmp)))
        } else if beneath(&absolute, home) {
            None
        } else if beneath(&absolute, b"/tmp") {
            Some(Path::new("/tmp"))
        } else {
            None
        };
        let candidate = temp_root.map(|p| {
            p.join(format!(
                "oh-my-setting-locks-{}",
                rustix::process::getuid().as_raw()
            ))
        });
        match candidate {
            Some(path) if safe_candidate(&path) => path,
            _ => settings.home.join(".cache/oh-my-setting/locks"),
        }
    };
    Ok(root.join(OsString::from_vec(filename)))
}

fn safe_candidate(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            m.is_dir()
                && !m.file_type().is_symlink()
                && m.uid() == rustix::process::getuid().as_raw()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.parent().is_some_and(|parent| {
            fs::accessat(fs::CWD, parent, Access::WRITE_OK, AtFlags::EACCESS).is_ok()
        }),
        Err(_) => false,
    }
}
