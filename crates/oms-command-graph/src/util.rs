use crate::{Error, Result};
use oms_command_plan::services::GroupResolver;
use oms_command_plan::storage::Store;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

thread_local! {
    static GROUP_RESOLVER: Cell<Option<GroupResolver>> = const { Cell::new(None) };
}

pub struct GroupResolverGuard(Option<GroupResolver>);

impl Drop for GroupResolverGuard {
    fn drop(&mut self) {
        GROUP_RESOLVER.with(|slot| slot.set(self.0));
    }
}

pub fn with_group_resolver(resolver: Option<GroupResolver>) -> GroupResolverGuard {
    let previous = GROUP_RESOLVER.with(|slot| slot.replace(resolver));
    GroupResolverGuard(previous)
}

pub(crate) fn current_group_resolver() -> Option<GroupResolver> {
    GROUP_RESOLVER.with(Cell::get)
}

pub(crate) fn open_store(path: &Path) -> Result<Store> {
    Ok(Store::open(path)?.with_group_resolver(current_group_resolver()))
}

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
    process_detail(command, timeout, cancel).map(|(exit, out, err, _)| (exit, out, err))
}
pub fn process_detail(
    command: &mut Command,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<(i32, Vec<u8>, Vec<u8>, bool)> {
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
    let mut poll_delay = Duration::from_micros(250);
    let result = (|| -> Result<i32> {
        loop {
            check(cancel)?;
            if start.elapsed() > timeout {
                return Err(Error {
                    code: 124,
                    message: "command timed out".into(),
                });
            }
            drain(&mut out, &mut stdout)?;
            drain(&mut err, &mut stderr)?;
            // NOWAIT pins this owned process generation until its group is stopped.
            if waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|e| Error::new(e.to_string()))?
            .is_some_and(|s| s.exited() || s.killed() || s.dumped())
            {
                break;
            }
            std::thread::sleep(poll_delay);
            poll_delay = match poll_delay.as_micros() {
                0..=250 => Duration::from_millis(1),
                251..=1000 => Duration::from_millis(4),
                _ => Duration::from_millis(10),
            };
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
        use std::os::unix::process::ExitStatusExt;
        Ok(status
            .code()
            .unwrap_or_else(|| -status.signal().unwrap_or(1)))
    })();
    match result {
        Ok(status) => Ok((status, stdout, stderr, false)),
        Err(error) => {
            if !reaped {
                let _ = kill_process_group(pid, Signal::KILL);
                let _ = child.wait();
            }
            if error.code == 124 {
                Ok((-1, stdout, stderr, true))
            } else {
                Err(error)
            }
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
pub fn physical_temp_dir(temp_dir: &Path, cwd: &Path) -> PathBuf {
    let absolute = if temp_dir.is_absolute() {
        temp_dir.to_path_buf()
    } else {
        cwd.join(temp_dir)
    };
    if let Ok(physical) = std::fs::canonicalize(&absolute) {
        return physical;
    }
    for ancestor in absolute.ancestors() {
        if let Ok(physical) = std::fs::canonicalize(ancestor) {
            if let Ok(rest) = absolute.strip_prefix(ancestor)
                && rest
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
            {
                return physical.join(rest);
            }
            break;
        }
    }
    absolute
}
pub fn repo_root(selected: &Path, cancel: &AtomicBool) -> Result<PathBuf> {
    let physical = std::fs::canonicalize(selected)?;
    let root = git(&physical, &["rev-parse", "--show-toplevel"], cancel);
    if root.is_empty() {
        return Ok(physical);
    }
    let found = std::fs::canonicalize(root)?;
    if let Ok(rel) = physical.strip_prefix(&found)
        && !rel.as_os_str().is_empty()
    {
        let inside_hidden_state = rel
            .components()
            .any(|c| matches!(c.as_os_str().to_str(), Some(".oms" | ".git")));
        let inside_nested_tmp = std::env::current_dir().ok().is_some_and(|cwd| {
            let tmp = physical_temp_dir(&std::env::temp_dir(), &cwd);
            tmp.starts_with(&found) && tmp != found && physical.starts_with(&tmp)
        });
        if inside_hidden_state || inside_nested_tmp {
            return Ok(physical);
        }
    }
    Ok(found)
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
    enum Frame<'a> {
        Value(&'a Value, usize),
        Array(std::slice::Iter<'a, Value>, usize, bool),
        Object(std::vec::IntoIter<(&'a String, &'a Value)>, usize, bool),
    }
    fn separator(out: &mut Vec<u8>, pretty: bool, depth: usize, first: bool) {
        if !first {
            out.push(b',');
        }
        if pretty {
            out.push(b'\n');
            out.resize(out.len() + depth * 2, b' ');
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![Frame::Value(value, 0)];
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Value(value, depth) => match value {
                Value::Array(items) => {
                    if depth >= 1024 {
                        return Err(Error::new("JSON depth limit"));
                    }
                    out.push(b'[');
                    stack.push(Frame::Array(items.iter(), depth, true));
                }
                Value::Object(items) => {
                    if depth >= 1024 {
                        return Err(Error::new("JSON depth limit"));
                    }
                    let mut fields: Vec<_> = items.iter().collect();
                    fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
                    out.push(b'{');
                    stack.push(Frame::Object(fields.into_iter(), depth, true));
                }
                Value::Number(number) if number.is_f64() => out.extend(
                    oms_json::agent_event_json_bytes(&oms_json::Value::Float(
                        number.as_f64().unwrap(),
                    ))
                    .map_err(|e| Error::new(e.to_string()))?,
                ),
                _ => serde_json::to_writer(&mut out, value)?,
            },
            Frame::Array(mut items, depth, first) => {
                if let Some(item) = items.next() {
                    separator(&mut out, pretty, depth + 1, first);
                    stack.push(Frame::Array(items, depth, false));
                    stack.push(Frame::Value(item, depth + 1));
                } else {
                    if pretty && !first {
                        separator(&mut out, true, depth, true);
                    }
                    out.push(b']');
                }
            }
            Frame::Object(mut items, depth, first) => {
                if let Some((key, item)) = items.next() {
                    separator(&mut out, pretty, depth + 1, first);
                    serde_json::to_writer(&mut out, key)?;
                    out.push(b':');
                    if pretty {
                        out.push(b' ');
                    }
                    stack.push(Frame::Object(items, depth, false));
                    stack.push(Frame::Value(item, depth + 1));
                } else {
                    if pretty && !first {
                        separator(&mut out, true, depth, true);
                    }
                    out.push(b'}');
                }
            }
        }
    }
    out.push(b'\n');
    Ok(out)
}
pub fn read_json(path: &Path, limit: usize) -> Result<Option<Value>> {
    no_symlinks(path)?;
    let Some(parent) = path.parent() else {
        return Err(Error::new("file has no parent"));
    };
    if !parent.exists() {
        return Ok(None);
    }
    let store = open_store(parent)?;
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
    let store = open_store(root)?;
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
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Option<regex::Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(compiled) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(pattern) {
        return compiled.as_ref().is_some_and(|r| r.is_match(value));
    }
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
    let compiled = regex::Regex::new(&p).ok();
    let matched = compiled.as_ref().is_some_and(|r| r.is_match(value));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.insert(pattern.to_owned(), compiled);
    matched
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
