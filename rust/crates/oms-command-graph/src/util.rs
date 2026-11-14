use crate::{Error, Result};
use oms_command_plan::storage::Store;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn sha(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data.as_ref()))
}
pub fn now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::<chrono::Utc>::from_timestamp(elapsed.as_secs() as i64, 0)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
pub fn at(cwd: &Path, path: impl AsRef<Path>) -> PathBuf {
    if path.as_ref().is_absolute() {
        path.as_ref().to_owned()
    } else {
        cwd.join(path)
    }
}
pub fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(Error {
            code: 130,
            message: "interrupted".into(),
        })
    } else {
        Ok(())
    }
}

pub fn command(
    repo: &Path,
    program: &str,
    args: &[&str],
    cancel: &AtomicBool,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let mut command = Command::new(program);
    command.args(args).current_dir(repo);
    process(&mut command, Duration::from_secs(60), cancel)
}
pub fn process(
    command: &mut Command,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    use rustix::{
        fs::{OFlags, fcntl_getfl, fcntl_setfl},
        process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid},
    };
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    check(cancel)?;
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid =
        Pid::from_raw(child.id() as i32).ok_or_else(|| Error::new("invalid child process"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let setup = || -> std::io::Result<()> {
        fcntl_setfl(&out, fcntl_getfl(&out)? | OFlags::NONBLOCK)?;
        fcntl_setfl(&err, fcntl_getfl(&err)? | OFlags::NONBLOCK)?;
        Ok(())
    };
    if let Err(error) = setup() {
        let _ = kill_process_group(pid, Signal::KILL);
        let _ = child.wait();
        return Err(error.into());
    }
    fn drain(input: &mut impl Read, data: &mut Vec<u8>) -> Result<()> {
        let mut buffer = [0u8; 16384];
        for _ in 0..64 {
            match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if data.len() + n > 256 * 1024 * 1024 {
                        return Err(Error::new("command output exceeds limit"));
                    }
                    data.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    let start = Instant::now();
    let mut stdout = vec![];
    let mut stderr = vec![];
    let mut reaped = false;
    let result = (|| -> Result<i32> {
        loop {
            check(cancel)?;
            if start.elapsed() > timeout {
                return Err(Error::new("command timed out"));
            }
            drain(&mut out, &mut stdout)?;
            drain(&mut err, &mut stderr)?;
            // NOWAIT pins this owned process generation until its group is stopped.
            if waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|e| Error::new(e.to_string()))?
            .is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = kill_process_group(pid, Signal::KILL);
        let status = child.wait()?;
        reaped = true;
        for _ in 0..512 {
            let old = stdout.len() + stderr.len();
            drain(&mut out, &mut stdout)?;
            drain(&mut err, &mut stderr)?;
            if old == stdout.len() + stderr.len() {
                break;
            }
        }
        Ok(status.code().unwrap_or(1))
    })();
    match result {
        Ok(status) => Ok((status, stdout, stderr)),
        Err(error) => {
            if !reaped {
                let _ = kill_process_group(pid, Signal::KILL);
                let _ = child.wait();
            }
            Err(error)
        }
    }
}
pub fn git(repo: &Path, args: &[&str], cancel: &AtomicBool) -> String {
    command(repo, "git", args, cancel)
        .ok()
        .filter(|v| v.0 == 0)
        .map(|v| String::from_utf8_lossy(&v.1).trim().to_owned())
        .unwrap_or_default()
}
pub fn repo_root(selected: &Path, cancel: &AtomicBool) -> Result<PathBuf> {
    let physical = std::fs::canonicalize(selected)?;
    let root = git(&physical, &["rev-parse", "--show-toplevel"], cancel);
    if root.is_empty() {
        Ok(physical)
    } else {
        Ok(std::fs::canonicalize(root)?)
    }
}
pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn n(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}
pub fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
pub fn bytes(value: &Value, pretty: bool) -> Result<Vec<u8>> {
    let value =
        oms_command_operations::value::V::parse(&serde_json::to_vec(value)?).map_err(Error::new)?;
    let mut data = value.json(pretty).map_err(Error::new)?;
    if pretty && data.last() == Some(&b'\n') {
        data.pop();
    }
    data.push(b'\n');
    Ok(data)
}
pub fn read_json(path: &Path, limit: usize) -> Result<Option<Value>> {
    no_symlinks(path)?;
    let Some(parent) = path.parent() else {
        return Err(Error::new("file has no parent"));
    };
    if !parent.exists() {
        return Ok(None);
    }
    let store = Store::open(parent)?;
    store
        .optional(
            Path::new(
                path.file_name()
                    .ok_or_else(|| Error::new("file has no name"))?,
            ),
            limit,
        )?
        .map(|b| serde_json::from_slice(&b).map_err(Into::into))
        .transpose()
}
pub fn write(path: &Path, raw: &[u8]) -> Result<()> {
    no_symlinks(path)?;
    let mut root = path
        .parent()
        .ok_or_else(|| Error::new("file has no parent"))?;
    while !root.exists() {
        root = root
            .parent()
            .ok_or_else(|| Error::new("file has no existing ancestor"))?;
    }
    let store = Store::open(root)?;
    store.mutate(path, None, raw, 256 * 1024 * 1024, true)?;
    Ok(())
}
pub fn no_symlinks(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        if part.is_symlink() {
            return Err(Error::new(
                "graph state path must not traverse a symbolic link",
            ));
        }
    }
    Ok(())
}
pub fn create_marker(repo: &Path) -> Result<()> {
    let path = Path::new(".oms/.gitignore");
    let root = oms_safe_fs::Root::open(repo)
        .map_err(|e| Error::new(format!("graph state root: {e:?}")))?;
    root.atomic_mutate(
        path,
        oms_safe_fs::MutationOptions {
            read: oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: 1024 * 1024,
            },
            create_parent: true,
            max_output: 1024 * 1024,
            preserve_existing: true,
            expected_exists: None,
        },
        |_| Ok::<Vec<u8>, Error>(b"*\n".to_vec()),
    )
    .map_err(|e| Error::new(format!("graph state marker: {e:?}")))?;
    Ok(())
}
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    write(path, &bytes(value, false)?)
}
pub fn glob(pattern: &str, value: &str) -> bool {
    let mut p = String::from("(?s)^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => p.push_str(".*"),
            '?' => p.push('.'),
            '[' => {
                let mut class = String::new();
                if chars.peek() == Some(&'!') {
                    chars.next();
                    class.push('^');
                }
                while chars.peek().is_some_and(|c| *c != ']') {
                    class.push(chars.next().unwrap());
                }
                if chars.next().is_some() {
                    p.push('[');
                    p.push_str(&class);
                    p.push(']');
                } else {
                    p.push_str("\\[");
                    p.push_str(&regex::escape(&class));
                }
            }
            _ => p.push_str(&regex::escape(&c.to_string())),
        }
    }
    p.push('$');
    regex::Regex::new(&p).is_ok_and(|r| r.is_match(value))
}
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '-'])
        && !path.contains('\0')
        && path.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
pub fn scrub(text: &str) -> String {
    use std::os::unix::ffi::OsStrExt;
    let env: Vec<_> = std::env::vars_os()
        .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
        .collect();
    let refs = env
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect::<Vec<_>>();
    if oms_graph_python::sensitive_text(text)
        || oms_scrub::scan_secret(text.as_bytes(), &refs).map_or(true, |hits| !hits.is_empty())
    {
        "[sensitive-content-omitted]".into()
    } else {
        text.into()
    }
}
