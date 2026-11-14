use crate::{Error, Result, hash, process};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

struct Limited {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for Limited {
    fn write(&mut self, raw: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(raw.len()) > self.cap {
            return Err(std::io::Error::other("Git output exceeds read bound"));
        }
        self.bytes.extend_from_slice(raw);
        Ok(raw.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn query(
    repo: &Path,
    args: &[&OsStr],
    interrupted: &AtomicBool,
) -> Result<(i32, Vec<u8>)> {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(repo)
        .args(args);
    let mut out = Limited {
        bytes: Vec::new(),
        cap: 32 * 1024 * 1024,
    };
    let result = process::execute(&mut command, false, false, &mut out, 0, interrupted, None)?;
    Ok((result.exit, out.bytes))
}
fn checked(repo: &Path, args: &[&str], interrupted: &AtomicBool) -> Result<Vec<u8>> {
    let args: Vec<_> = args.iter().map(OsStr::new).collect();
    let (exit, raw) = query(repo, &args, interrupted)?;
    if exit != 0 {
        return Err(Error::new(1, "Git metadata query failed; state is unknown"));
    }
    Ok(raw)
}
fn repository_hint(path: &Path) -> bool {
    path.ancestors().any(|parent| {
        let marker = parent.join(".git");
        match fs::symlink_metadata(&marker) {
            // An empty directory is also used as a non-repository discovery
            // barrier by disposable OMS fixtures. It contains no Git state.
            Ok(meta) if meta.is_dir() => {
                fs::read_dir(marker).map_or(true, |mut entries| entries.next().is_some())
            }
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    }) || std::env::var_os("GIT_DIR").is_some()
}
pub(crate) fn repository(path: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
    let physical = path
        .canonicalize()
        .map_err(|e| Error::io("resolve repository", e))?;
    if !physical.is_dir() {
        return Err(Error::new(1, "repository is not a directory"));
    }
    let (exit, raw) = query(
        &physical,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        interrupted,
    )?;
    if exit == 0 {
        let raw = raw.strip_suffix(b"\n").unwrap_or(&raw);
        return PathBuf::from(OsString::from_vec(raw.to_vec()))
            .canonicalize()
            .map_err(|e| Error::io("resolve Git root", e));
    }
    // A failed query inside an apparent repository is uncertainty, not non-Git.
    if repository_hint(&physical) {
        return Err(Error::new(1, "Git repository discovery failed"));
    }
    Ok(physical)
}
pub fn state_fingerprint(repo: &Path, interrupted: &AtomicBool) -> Result<String> {
    let (exit, _) = query(
        repo,
        &[OsStr::new("rev-parse"), OsStr::new("--git-dir")],
        interrupted,
    )?;
    if exit != 0 {
        if repository_hint(repo) {
            return Err(Error::new(1, "Git state is unavailable"));
        }
        return Ok("non-git".into());
    }
    let (rc, head) = query(
        repo,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("HEAD"),
        ],
        interrupted,
    )?;
    let head = if rc == 0 {
        std::str::from_utf8(head.strip_suffix(b"\n").unwrap_or(&head))
            .map_err(|_| Error::new(1, "invalid Git HEAD"))?
            .to_owned()
    } else {
        // Only a known symbolic HEAD whose named ref is absent is unborn.
        let branch = checked(repo, &["symbolic-ref", "-q", "HEAD"], interrupted)?;
        let branch = branch.strip_suffix(b"\n").unwrap_or(&branch);
        let (rc, _) = query(
            repo,
            &[
                OsStr::new("show-ref"),
                OsStr::new("--verify"),
                OsStr::new("--quiet"),
                OsStr::from_bytes(branch),
            ],
            interrupted,
        )?;
        if rc != 1 {
            return Err(Error::new(1, "Git HEAD is unreadable, not proven unborn"));
        }
        "unborn".into()
    };
    let index = checked(repo, &["ls-files", "--stage", "-z"], interrupted)?;
    let tracked = checked(repo, &["ls-files", "-z"], interrupted)?;
    let mut paths: Vec<_> = tracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    let mut h = Sha256::new();
    h.update(b"index\0");
    h.update(index);
    for rel in paths {
        crate::checkpoint(interrupted, None)?;
        h.update(b"\0path\0");
        h.update(Sha256::digest(rel));
        let path = repo.join(OsStr::from_bytes(rel));
        let info = match fs::symlink_metadata(&path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                h.update(b"missing\0");
                continue;
            }
            Err(e) => return Err(Error::io("read tracked metadata", e)),
        };
        h.update(format!("mode:{:o}\0", info.mode()).as_bytes());
        if info.file_type().is_symlink() {
            h.update(b"link\0");
            h.update(
                fs::read_link(&path)
                    .map_err(|e| Error::io("read tracked link", e))?
                    .as_os_str()
                    .as_bytes(),
            );
        } else if info.is_file() {
            h.update(b"file\0");
            match fs::File::open(&path) {
                Ok(mut file) => {
                    let mut buf = [0; 65536];
                    loop {
                        crate::checkpoint(interrupted, None)?;
                        match file.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => h.update(&buf[..n]),
                            Err(_) => {
                                h.update(b"unreadable\0");
                                break;
                            }
                        }
                    }
                }
                Err(_) => h.update(b"unreadable\0"),
            }
        } else {
            h.update(
                format!(
                    "other:{}:{}\0",
                    info.size(),
                    i128::from(info.mtime()) * 1_000_000_000 + i128::from(info.mtime_nsec())
                )
                .as_bytes(),
            );
        }
    }
    let tracked_hash = format!("{:x}", h.finalize());
    let untracked = checked(
        repo,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        interrupted,
    )?;
    let mut paths: Vec<_> = untracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    let mut h = Sha256::new();
    for rel in paths {
        h.update(Sha256::digest(rel));
        match fs::symlink_metadata(repo.join(OsStr::from_bytes(rel))) {
            Ok(info) => h.update(
                format!(
                    "M:{:o}:{}:{}",
                    info.mode(),
                    info.size(),
                    i128::from(info.mtime()) * 1_000_000_000 + i128::from(info.mtime_nsec())
                )
                .as_bytes(),
            ),
            Err(_) => h.update(b"MISSING"),
        }
    }
    Ok(format!("{head}:{tracked_hash}:{:x}", h.finalize()))
}
pub fn command_hash(command: &[u8]) -> String {
    hash(&[command, b"\n"].concat())
}
