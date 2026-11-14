use crate::Output;
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const INPUT_MAX: usize = 8 * 1024 * 1024;
pub const STATE_MAX: usize = 1024 * 1024;
pub struct Context<'a> {
    pub provider: Option<&'static str>,
    pub cwd: &'a Path,
    pub source: &'a Path,
    pub interrupted: &'a AtomicBool,
}
pub fn empty() -> Output {
    (0, vec![], vec![])
}
pub fn out(text: impl AsRef<[u8]>) -> Output {
    (0, text.as_ref().to_vec(), vec![])
}
pub fn fail(text: impl AsRef<str>) -> Output {
    (
        2,
        vec![],
        format!("error: {}\n", text.as_ref()).into_bytes(),
    )
}
pub fn json_line(value: &Value) -> Vec<u8> {
    fn native(value: &Value) -> oms_json::Value {
        use oms_json::Value as N;
        match value {
            Value::Null => N::Null,
            Value::Bool(b) => N::Bool(*b),
            Value::String(s) => N::String(s.as_str().into()),
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    N::integer(&n.to_string()).expect("serde integer")
                } else {
                    N::Float(n.as_f64().expect("finite serde number"))
                }
            }
            Value::Array(a) => N::Array(a.iter().map(native).collect()),
            Value::Object(m) => N::Object(
                m.iter()
                    .map(|(k, v)| (k.as_str().into(), native(v)))
                    .collect(),
            ),
        }
    }
    let mut raw = oms_json::agent_event_json_bytes(&native(value)).unwrap_or_default();
    raw.push(b'\n');
    raw
}
pub fn document(raw: &[u8]) -> Option<Value> {
    if raw.len() > INPUT_MAX {
        return None;
    }
    serde_json::from_slice::<Value>(raw)
        .ok()
        .filter(Value::is_object)
}
pub fn strict_document(raw: &[u8]) -> Result<Value, String> {
    use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
    struct Strict;
    impl<'de> DeserializeSeed<'de> for Strict {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
            struct Checked;
            impl<'de> Visitor<'de> for Checked {
                type Value = Value;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("strict JSON")
                }
                fn visit_bool<E: Error>(self, v: bool) -> Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_i64<E: Error>(self, v: i64) -> Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_u64<E: Error>(self, v: u64) -> Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_f64<E: Error>(self, v: f64) -> Result<Value, E> {
                    serde_json::Number::from_f64(v)
                        .map(Value::Number)
                        .ok_or_else(|| E::custom("non-finite number"))
                }
                fn visit_str<E: Error>(self, v: &str) -> Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_string<E: Error>(self, v: String) -> Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_none<E: Error>(self) -> Result<Value, E> {
                    Ok(Value::Null)
                }
                fn visit_unit<E: Error>(self) -> Result<Value, E> {
                    Ok(Value::Null)
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
                    let mut values = Vec::new();
                    while let Some(v) = seq.next_element_seed(Strict)? {
                        values.push(v);
                    }
                    Ok(Value::Array(values))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some(k) = map.next_key::<String>()? {
                        if values.contains_key(&k) {
                            return Err(A::Error::custom("duplicate JSON key"));
                        }
                        values.insert(k, map.next_value_seed(Strict)?);
                    }
                    Ok(Value::Object(values))
                }
            }
            d.deserialize_any(Checked)
        }
    }
    if raw.len() > INPUT_MAX {
        return Err("JSON exceeds byte limit".into());
    }
    let mut d = serde_json::Deserializer::from_slice(raw);
    let v = Strict
        .deserialize(&mut d)
        .map_err(|_| "invalid or duplicate-key JSON")?;
    d.end().map_err(|_| "trailing JSON input")?;
    if !v.is_object() {
        return Err("JSON must contain an object".into());
    }
    Ok(v)
}
pub fn input_error(name: &str) -> Output {
    if ["command-guard-hook", "git-move-guard-hook"].contains(&name) {
        let label = if name == "command-guard-hook" {
            "command guard"
        } else {
            "git move guard"
        };
        (0, vec![], format!("{label}: cannot evaluate this command (hook input unavailable or oversized); allowing it unchecked.\n").into_bytes())
    } else if name == "turn-guard" {
        out(json_line(
            &json!({"systemMessage": "oh-my-setting turn guard: unavailable (unreadable input); this turn was not checked."}),
        ))
    } else if name.ends_with("-config") || name == "codex-hook-bridge" {
        fail("hook input unavailable or oversized")
    } else {
        empty()
    }
}
pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}
pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
pub fn off(name: &str) -> bool {
    matches!(
        env(name).as_str(),
        "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF"
    )
}
pub fn enabled(name: &str) -> bool {
    env(name) != "0"
}
pub fn on(name: &str) -> bool {
    let value = env(name);
    value.is_empty() || value == "1"
}
pub fn child() -> bool {
    env("OMS_HARNESS_CHILD") == "1"
}
pub fn panel() -> bool {
    !child()
        && !env("OMS_PANEL_MAIN_ATTEMPT").is_empty()
        && env("OMS_PANEL_SESSION").starts_with("oms-")
}
pub fn cap(name: &str, default: u64, min: u64, max: u64) -> u64 {
    env(name).parse::<u64>().unwrap_or(default).clamp(min, max)
}
pub fn seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn now() -> String {
    oms_command_room::now()
}
pub fn epoch(raw: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|d| d.timestamp_millis() as f64 / 1000.0)
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%SZ")
                .ok()
                .map(|d| d.and_utc().timestamp() as f64)
        })
}
pub fn hash(raw: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(raw.as_ref()))
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn first<'a>(v: &'a Value, names: &[&str]) -> &'a str {
    names
        .iter()
        .map(|k| text(v, k))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}
pub fn session(v: &Value) -> &str {
    first(v, &["session_id", "sessionId"])
}
pub fn session_hash(v: &Value) -> String {
    let s = session(v);
    hash(if s.is_empty() { "nosession" } else { s })[..32].into()
}
pub fn event(v: &Value) -> &str {
    first(v, &["hook_event_name", "hookEventName"])
}
pub fn payload_cwd<'a>(v: &'a Value, cwd: &'a Path) -> &'a Path {
    let raw = first(v, &["cwd", "currentWorkingDirectory"]);
    if raw.is_empty() { cwd } else { Path::new(raw) }
}
pub fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("/"))
}
pub fn expand(raw: &Path) -> PathBuf {
    if raw == Path::new("~") {
        home()
    } else if let Ok(rest) = raw.strip_prefix("~/") {
        home().join(rest)
    } else {
        raw.into()
    }
}
pub fn absolute(cwd: &Path, raw: &Path) -> PathBuf {
    let raw = expand(raw);
    if raw.is_absolute() {
        normalize(&raw)
    } else {
        normalize(&cwd.join(raw))
    }
}
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(c.as_os_str()),
        }
    }
    out
}
pub fn root(raw: &Path, cwd: &Path) -> Option<PathBuf> {
    let p = fs::canonicalize(absolute(cwd, raw)).ok()?;
    if !p.is_dir() {
        return None;
    }
    for a in p.ancestors() {
        if let Ok(m) = fs::symlink_metadata(a.join(".git"))
            && (m.is_dir() || m.is_file())
        {
            return Some(a.into());
        }
    }
    Some(p)
}
pub fn repo(v: &Value, ctx: &Context<'_>) -> Option<PathBuf> {
    if let Some(p) = env_path("OMS_STATE_REPO") {
        return root(&p, ctx.cwd);
    }
    for k in ["cwd", "currentWorkingDirectory"] {
        if let Some(value) = v.get(k)
            && !value.is_null()
            && value != ""
        {
            return value.as_str().and_then(|p| root(Path::new(p), ctx.cwd));
        }
    }
    root(ctx.cwd, ctx.cwd)
}
pub fn adopted(repo: &Path) -> bool {
    fs::symlink_metadata(repo.join(".oms")).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}
pub fn ensure_oms(repo: &Path) -> Result<(), String> {
    backup(&repo.join(".oms/.gitignore"), b"*\n", STATE_MAX, false)
}
pub fn normalize_machine(raw: &str) -> String {
    let h = home();
    let raw = if h != Path::new("/") {
        raw.replace(h.to_string_lossy().as_ref(), "~")
    } else {
        raw.into()
    };
    regex::Regex::new(r"/(?:hom[e]|User[s])/[A-Za-z0-9._-]+")
        .unwrap()
        .replace_all(&raw, "~")
        .into_owned()
}
pub fn git_directories(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    let marker = repo.join(".git");
    let directory = if let Ok(raw) = read(&marker, 4096) {
        let raw = std::str::from_utf8(&raw).ok()?.trim();
        absolute(repo, Path::new(raw.strip_prefix("gitdir:")?.trim()))
    } else {
        marker
    };
    let directory = directory.canonicalize().ok()?;
    let common = read(&directory.join("commondir"), 4096)
        .ok()
        .and_then(|r| String::from_utf8(r).ok())
        .map(|s| absolute(&directory, Path::new(s.trim())))
        .unwrap_or_else(|| directory.clone());
    Some((directory, common.canonicalize().ok()?))
}
pub fn head(repo: &Path) -> Option<String> {
    let (directory, common) = git_directories(repo)?;
    let raw = read(&directory.join("HEAD"), 4096).ok()?;
    let raw = std::str::from_utf8(&raw).ok()?.trim();
    let valid =
        |s: &str| (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if valid(raw) {
        return Some(raw.into());
    }
    let name = raw.strip_prefix("ref: ")?;
    if !name.starts_with("refs/")
        || Path::new(name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    if let Ok(raw) = read(&common.join(name), 4096) {
        let s = std::str::from_utf8(&raw).ok()?.trim();
        if valid(s) {
            return Some(s.into());
        }
    }
    let packed = read(&common.join("packed-refs"), INPUT_MAX).ok()?;
    String::from_utf8_lossy(&packed).lines().find_map(|l| {
        let (oid, r) = l.split_once(' ')?;
        (r == name && valid(oid)).then(|| oid.into())
    })
}
pub fn state_path(repo: &Path, v: &Value, suffix: &str) -> PathBuf {
    repo.join(format!(
        ".oms/hooks/sessions/{}{suffix}.json",
        session_hash(v)
    ))
}
pub fn read(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| "cannot open regular file")?;
    let info = rustix::fs::fstat(&fd).map_err(|_| "cannot inspect regular file")?;
    if rustix::fs::FileType::from_raw_mode(info.st_mode) != rustix::fs::FileType::RegularFile
        || info.st_size < 0
        || info.st_size as u64 > max as u64
    {
        return Err("file is unsafe or oversized".into());
    }
    let mut raw = Vec::new();
    fs::File::from(fd)
        .take(max as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "cannot read file")?;
    if raw.len() > max {
        return Err("file is oversized".into());
    }
    Ok(raw)
}
pub fn load(path: &Path) -> Value {
    read(path, STATE_MAX)
        .ok()
        .and_then(|r| document(&r))
        .unwrap_or_else(|| json!({}))
}
pub fn lock(path: &Path, cwd: &Path) -> Result<oms_lock::Guard, String> {
    let mut settings = oms_lock::Settings::from_env().map_err(|_| "lock settings unavailable")?;
    settings.cwd = cwd.into();
    settings.timeout = Duration::from_secs(1);
    oms_lock::Lock::for_file(path, settings)
        .and_then(|l| l.try_acquire())
        .map_err(|_| "hook state is busy or unsafe".into())
}
pub fn mutate<T>(
    path: &Path,
    max: usize,
    preserve: bool,
    transform: impl FnOnce(&[u8]) -> Result<(Vec<u8>, T), String>,
) -> Result<T, String> {
    if !path.is_absolute() {
        return Err("state path must be absolute".into());
    }
    let anchor = Path::new("/");
    let base = Root::open(anchor).map_err(|_| "unsafe file root")?;
    let relative = path.strip_prefix(anchor).map_err(|_| "external file")?;
    let _guard = lock(path, base.physical_root())?;
    let mut result = None;
    base.atomic_mutate(
        relative,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: max,
            },
            create_parent: true,
            max_output: max,
            preserve_existing: false,
            expected_exists: None,
        },
        |raw| {
            let (bytes, value) = transform(raw)?;
            result = Some(value);
            if preserve && !raw.is_empty() {
                Ok::<_, String>(raw.to_vec())
            } else {
                Ok(bytes)
            }
        },
    )
    .map_err(|_| "atomic state publication failed; reconcile before retrying")?;
    result.ok_or_else(|| "state transformation did not run".into())
}
pub fn write(path: &Path, value: &Value) -> Result<(), String> {
    let raw = json_line(value);
    mutate(path, INPUT_MAX, false, |_| Ok((raw, ())))
}
pub fn backup(path: &Path, original: &[u8], max: usize, exact: bool) -> Result<(), String> {
    let base = Root::open("/").map_err(|_| "unsafe backup root")?;
    let relative = path
        .strip_prefix("/")
        .map_err(|_| "backup path must be absolute")?;
    let _guard = lock(path, base.physical_root())?;
    if exact {
        match base.read_no_follow(
            relative,
            ByteReadOptions {
                missing_ok: false,
                max_bytes: max,
            },
        ) {
            Ok(old) if old == original => return Ok(()),
            Ok(_) => return Err("existing backup differs".into()),
            Err(oms_safe_fs::ByteError::Missing) => {}
            Err(_) => return Err("backup is unsafe or unreadable".into()),
        }
    }
    base.atomic_mutate(
        relative,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: max,
            },
            create_parent: true,
            max_output: max,
            preserve_existing: true,
            expected_exists: None,
        },
        |_| Ok::<_, String>(original.to_vec()),
    )
    .map_err(|_| "backup publication failed; reconcile before retrying")?;
    Ok(())
}
fn open_parent(path: &Path) -> Result<std::os::fd::OwnedFd, String> {
    use rustix::fs::{self as rfs, Mode, OFlags};
    if !path.is_absolute() {
        return Err("state path must be absolute".into());
    }
    let parent_path = path.parent().ok_or("state parent missing")?;
    let mut parent = rfs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "append root unavailable")?;
    for component in parent_path.components() {
        if let Component::Normal(name) = component {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            parent = match rfs::openat(&parent, name, flags, Mode::empty()) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => {
                    rfs::mkdirat(&parent, name, Mode::from_raw_mode(0o700))
                        .or_else(|e| {
                            if e == rustix::io::Errno::EXIST {
                                Ok(())
                            } else {
                                Err(e)
                            }
                        })
                        .map_err(|_| "append parent creation failed")?;
                    rfs::openat(&parent, name, flags, Mode::empty())
                        .map_err(|_| "append parent unsafe")?
                }
                Err(_) => return Err("append parent unsafe".into()),
            };
        }
    }
    Ok(parent)
}
pub fn append(path: &Path, row: &Value, max: usize) -> Result<(), String> {
    use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
    if !path.is_absolute() {
        return Err("append path must be absolute".into());
    }
    let _guard = lock(path, Path::new("/"))?;
    let parent_path = path.parent().ok_or("append parent missing")?;
    let parent = open_parent(path)?;
    let name = path.file_name().ok_or("append name missing")?;
    let fd = rfs::openat(
        &parent,
        name,
        OFlags::RDWR
            | OFlags::APPEND
            | OFlags::CREATE
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| "append file unavailable")?;
    let before = rfs::fstat(&fd).map_err(|_| "append file unavailable")?;
    if rfs::FileType::from_raw_mode(before.st_mode) != rfs::FileType::RegularFile
        || before.st_nlink != 1
        || before.st_size < 0
    {
        return Err("append file unsafe".into());
    }
    let mut encoded = json_line(row);
    let mut file = fs::File::from(fd);
    if before.st_size > 0 {
        file.seek(SeekFrom::End(-1))
            .map_err(|_| "append tail unavailable")?;
        let mut last = [0];
        file.read_exact(&mut last)
            .map_err(|_| "append tail unavailable")?;
        if last[0] != b'\n' {
            encoded.insert(0, b'\n');
        }
    }
    if before.st_size as u64 + encoded.len() as u64 > max as u64 {
        return Err("append file exceeds byte limit".into());
    }
    let check = || -> Result<(), String> {
        let named = rfs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| "append name changed")?;
        let parent_now = rfs::stat(parent_path).map_err(|_| "append parent changed")?;
        let opened = rfs::fstat(&parent).map_err(|_| "append parent changed")?;
        if named.st_dev != before.st_dev
            || named.st_ino != before.st_ino
            || named.st_nlink != 1
            || parent_now.st_dev != opened.st_dev
            || parent_now.st_ino != opened.st_ino
        {
            return Err("append path changed".into());
        }
        Ok(())
    };
    check()?;
    file.write_all(&encoded)
        .and_then(|_| file.sync_all())
        .map_err(|_| "append incomplete; reconcile before retrying")?;
    check()?;
    rfs::fsync(&parent).map_err(|_| "append published; directory sync failed")?;
    Ok(())
}
pub fn safe_text(raw: &str, limit: usize) -> String {
    let environment: Vec<_> = std::env::vars_os().collect();
    let entries: Vec<_> = environment
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    let cleaned = oms_scrub::redact(raw.as_bytes(), &entries)
        .ok()
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
        .unwrap_or_else(|| "[withheld]".into());
    let mut cleaned = cleaned;
    let h = home();
    if h != Path::new("/") {
        cleaned = cleaned.replace(h.to_string_lossy().as_ref(), "~");
    }
    let cleaned = regex::Regex::new(r"/(?:hom[e]|User[s])/[A-Za-z0-9._-]+")
        .unwrap()
        .replace_all(&cleaned, "~");
    clip(&flat(&cleaned), limit)
}
pub fn clip(raw: &str, max: usize) -> String {
    let mut end = raw.len().min(max);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    raw[..end].into()
}
pub fn flat(raw: &str) -> String {
    raw.chars()
        .map(|c| if control(c) { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn control(c: char) -> bool {
    use unicode_general_category::{GeneralCategory as C, get_general_category as cat};
    matches!(
        cat(c),
        C::Control | C::Format | C::Surrogate | C::PrivateUse | C::Unassigned
    )
}
pub fn identifier(raw: &str, max: usize) -> bool {
    !raw.is_empty()
        && raw.len() <= max
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
pub fn bounded(raw: &Value, max: usize) -> String {
    let v = if raw.is_object() {
        raw.get("id")
            .or_else(|| raw.get("name"))
            .or_else(|| raw.get("display_name"))
            .unwrap_or(&Value::Null)
    } else {
        raw
    };
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let s = s.trim();
    if s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:+() /-".contains(&b))
    {
        s.into()
    } else {
        String::new()
    }
}
pub fn agent(v: &Value, ctx: &Context<'_>) -> &'static str {
    if let Some(provider) = ctx.provider {
        return provider;
    }
    match env("OMS_HOOK_AGENT").as_str() {
        "claude" => return "claude",
        "codex" => return "codex",
        _ => {}
    }
    let path = first(v, &["transcript_path", "transcriptPath"]);
    if Path::new(path)
        .file_name()
        .is_some_and(|n| n.as_bytes().starts_with(b"rollout-"))
        || !first(v, &["turn_id", "turnId"]).is_empty()
    {
        "codex"
    } else {
        "claude"
    }
}
pub fn hud_dir() -> PathBuf {
    env_path("OMS_HUD_CACHE_DIR").unwrap_or_else(|| {
        std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("oh-my-setting-hud")
    })
}
pub fn invocation(
    argv: Vec<OsString>,
    input: Option<Vec<u8>>,
    cwd: &Path,
    timeout: Duration,
    max: usize,
    ctx: &Context<'_>,
) -> Result<oms_command_peer::process::Completed, String> {
    use oms_command_peer::process::{Executor, Invocation, Native};
    Native
        .execute(
            Invocation {
                argv,
                cwd: cwd.into(),
                input,
                timeout,
                max_bytes: max,
                environment: vec![],
                remove_environment: vec![],
                merge: false,
            },
            ctx.interrupted,
        )
        .map_err(|e| e.message)
}
pub fn git(cwd: &Path, args: &[&str], ctx: &Context<'_>) -> Option<String> {
    let mut argv = vec![
        OsString::from("git"),
        "--no-optional-locks".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
    ];
    argv.extend(args.iter().map(OsString::from));
    let p = invocation(argv, None, cwd, Duration::from_secs(2), STATE_MAX, ctx).ok()?;
    (p.complete && p.code == 0).then(|| String::from_utf8_lossy(&p.stdout).trim_end().into())
}
pub fn executable(name: &OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let check = |p: &Path| {
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if name.as_bytes().contains(&b'/') {
        let p = PathBuf::from(name);
        return check(&p).then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| check(p))
}
pub struct EventLock {
    parent: std::os::fd::OwnedFd,
    identity: rustix::fs::Stat,
}
impl EventLock {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        use rustix::fs::{self as rfs, AtFlags, FileType, Mode};
        let parent = open_parent(path)?;
        let end = Instant::now() + Duration::from_secs(1);
        loop {
            match rfs::mkdirat(&parent, ".events-lockdir", Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let identity =
                        rfs::statat(&parent, ".events-lockdir", AtFlags::SYMLINK_NOFOLLOW)
                            .map_err(|_| "event lock changed")?;
                    if FileType::from_raw_mode(identity.st_mode) != FileType::Directory {
                        return Err("event lock unsafe".into());
                    }
                    return Ok(Self { parent, identity });
                }
                Err(rustix::io::Errno::EXIST) => {
                    if let Ok(info) =
                        rfs::statat(&parent, ".events-lockdir", AtFlags::SYMLINK_NOFOLLOW)
                        && FileType::from_raw_mode(info.st_mode) == FileType::Directory
                        && seconds() - info.st_mtime as f64 > 60.
                    {
                        let _ = rfs::unlinkat(&parent, ".events-lockdir", AtFlags::REMOVEDIR);
                    }
                    if Instant::now() >= end {
                        return Err("hook events busy".into());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err("hook events unsafe".into()),
            }
        }
    }
}
impl Drop for EventLock {
    fn drop(&mut self) {
        use rustix::fs::{self as rfs, AtFlags};
        if let Ok(info) = rfs::statat(&self.parent, ".events-lockdir", AtFlags::SYMLINK_NOFOLLOW)
            && info.st_dev == self.identity.st_dev
            && info.st_ino == self.identity.st_ino
        {
            let _ = rfs::unlinkat(&self.parent, ".events-lockdir", AtFlags::REMOVEDIR);
        }
    }
}
pub fn event_row(repo: &Path, v: &Value, fields: Value) {
    if !adopted(repo) {
        return;
    }
    let path = repo.join(".oms/hooks/events.jsonl");
    let raw_turn = first(v, &["turn_id", "turnId"]);
    let turn = bounded(&json!(raw_turn), 120);
    let mut row = json!({"schema":1, "ts":now(), "agent":bounded(&json!(env("OMS_AGENT")),40),
        "hook":bounded(&json!(event(v)),80), "session":session_hash(v), "turn_id":turn,
        "cwd_hash": if first(v,&["cwd","currentWorkingDirectory"]).is_empty() {String::new()} else {hash(first(v,&["cwd","currentWorkingDirectory"]))[..16].into()}});
    if text(&row, "agent").is_empty() {
        row["agent"] = json!("hook");
    }
    if text(&row, "hook").is_empty() {
        row["hook"] = json!("unknown");
    }
    if !raw_turn.is_empty() && turn.is_empty() {
        row["turn_id_hash"] = json!(&hash(raw_turn)[..16]);
    }
    if let Some(fields) = fields.as_object() {
        row.as_object_mut().unwrap().extend(fields.clone());
    }
    if let Ok(_held) = EventLock::acquire(&path) {
        let _ = append(&path, &row, 64 * INPUT_MAX);
    }
}
pub fn injection(repo: &Path, v: &Value, kind: &str, raw: &[u8]) {
    if raw.is_empty() || raw.len() > STATE_MAX || child() {
        return;
    }
    event_row(
        repo,
        v,
        json!({"action":"injection", "status":kind, "bytes":raw.len(),
        "lines":raw.iter().filter(|b| **b==b'\n').count(), "source": bounded(&v["source"],20)}),
    );
}
pub fn interrupted(ctx: &Context<'_>) -> bool {
    ctx.interrupted.load(Ordering::Acquire)
}
pub fn budgeted<T>(
    ctx: &Context<'_>,
    duration: Duration,
    run: impl FnOnce(&AtomicBool) -> T,
) -> (T, bool) {
    let cancel = AtomicBool::new(interrupted(ctx));
    let deadline = Instant::now() + duration;
    std::thread::scope(|scope| {
        let (done, receiver) = std::sync::mpsc::channel();
        let observed = &cancel;
        scope.spawn(move || {
            loop {
                match receiver.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= deadline || interrupted(ctx) {
                            observed.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            }
        });
        let result = run(&cancel);
        let timed_out = cancel.load(Ordering::Acquire);
        let _ = done.send(());
        (result, timed_out)
    })
}
