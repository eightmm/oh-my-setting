//! Native `oms input`: immutable attachments, the editable queue and explicit delivery.
#![forbid(unsafe_code)]

mod cli;
mod feedback;
mod help;
mod json;
mod queue;
mod repository;
mod room;
mod storage;
mod terminal;

use chrono::{DateTime, Utc};
use cli::{Level, Options};
use json::{J, field, number, object, os_string, string, text};
use oms_safe_fs::{Policy, Root};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_SOURCE: usize = 4 * 1024 * 1024;
const MAX_ATTACHMENT: usize = 64 * 1024;
const MAX_FEEDBACK: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, &'static str>;
const UNSAFE: &str = "invalid, unavailable, or unsafe local input state";

/// Host effects the input queue cannot perform itself.
pub trait HostServices: Send + Sync {
    /// Append one `note` message with a stable ID to a room, as `oms room send` would.
    fn send_room_note(
        &self,
        repo: &Path,
        room: &str,
        from: &str,
        to: &str,
        text: &str,
        message_id: &str,
    ) -> std::result::Result<(), String>;
    /// Run `action` while holding the exclusive queue lock for `queue_path`.
    ///
    /// A wait past the lock timeout must return [`LOCK_TIMEOUT`] without running `action`;
    /// the command then exits 75. The host may take the canonical runtime lock (see
    /// [`runtime_lock_path`]) so native and legacy writers exclude each other.
    fn with_queue_lock(
        &self,
        queue_path: &Path,
        interrupted: &AtomicBool,
        action: &mut dyn FnMut() -> std::result::Result<(), &'static str>,
    ) -> std::result::Result<(), &'static str>;
}
/// Exit-75 message for a queue lock wait that timed out.
pub const LOCK_TIMEOUT: &str = "timed out waiting for state lock";

/// Canonical runtime lock file for a queue: `<lock_dir>/runtime-<sha256(path)[:32]>.lock`,
/// as `oms_runtime.common.file_lock` derives it. `lock_dir_override` stands for `OMS_LOCK_DIR`;
/// without it the directory is `$HOME/.cache/oh-my-setting/locks`.
pub fn runtime_lock_path(queue_path: &Path, lock_dir_override: Option<&Path>) -> Option<PathBuf> {
    let dir = match lock_dir_override {
        Some(dir) => dir.to_path_buf(),
        None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
            .join(".cache/oh-my-setting/locks"),
    };
    let resolved = resolve_lenient(queue_path)?;
    let digest = hash(resolved.as_os_str().as_bytes());
    Some(dir.join(format!("runtime-{}.lock", &digest[..32])))
}

/// `Path.resolve(strict=False)`: components are applied left to right, symbolic links are
/// resolved as they are met, and `..` pops the already-resolved prefix.
fn resolve_lenient(path: &Path) -> Option<PathBuf> {
    let mut hops = 0;
    let mut resolved = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        std::env::current_dir().ok()?.canonicalize().ok()?
    };
    let mut pending: Vec<OsString> = path
        .components()
        .rev()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_owned()),
            Component::ParentDir => Some(OsString::from("..")),
            _ => None,
        })
        .collect();
    while let Some(name) = pending.pop() {
        if name == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&name);
        match std::fs::symlink_metadata(&candidate) {
            Ok(info) if info.file_type().is_symlink() => {
                hops += 1;
                if hops > 40 {
                    return None;
                }
                let target = std::fs::read_link(&candidate).ok()?;
                if target.is_absolute() {
                    resolved = PathBuf::from("/");
                }
                for c in target.components().rev() {
                    match c {
                        Component::Normal(n) => pending.push(n.to_owned()),
                        Component::ParentDir => pending.push(OsString::from("..")),
                        _ => {}
                    }
                }
            }
            _ => resolved = candidate,
        }
    }
    Some(resolved)
}

/// Appends through the native `oms room send`, with no Python in the path.
///
/// Standalone fallback: queue mutations lock a private `.queue.lock` (flock). That does not
/// interlock with the legacy Python runtime lock; until the `oms` entrypoint injects
/// `oms-lock` (`Mode::Mkdir`) on [`runtime_lock_path`] and `scripts/lib/oms_inputs` is retired,
/// do not run legacy and native writers on one queue concurrently.
pub struct DefaultHostServices;
impl HostServices for DefaultHostServices {
    fn with_queue_lock(
        &self,
        queue_path: &Path,
        interrupted: &AtomicBool,
        action: &mut dyn FnMut() -> std::result::Result<(), &'static str>,
    ) -> std::result::Result<(), &'static str> {
        let repo = queue_path.ancestors().nth(3).ok_or(UNSAFE)?;
        let _lock = storage::lock(repo, interrupted)?;
        action()
    }
    fn send_room_note(
        &self,
        repo: &Path,
        room: &str,
        from: &str,
        to: &str,
        text: &str,
        message_id: &str,
    ) -> std::result::Result<(), String> {
        let program = std::env::current_exe()
            .ok()
            .filter(|p| p.file_name().is_some_and(|n| n == "oms"))
            .unwrap_or_else(|| PathBuf::from("oms"));
        let mut command = std::process::Command::new(program);
        command
            .args(["room", "send", "--repo"])
            .arg(repo)
            .args(["--id", room, "--participant", from, "--to", to])
            .args(["--kind", "note", "--message-id", message_id, "--text", text]);
        // Python sent in-process: no inherited relay queue or carry marker may reroute it.
        for key in [
            "OMS_ROOM_RELAY_QUEUE",
            "OMS_ROOM_RELAY_ALLOWED",
            "OMS_ROOM_CARRY",
            "OMS_ROOM_CARRY_TS",
        ] {
            command.env_remove(key);
        }
        storage::run_bounded(
            command,
            1024 * 1024,
            &AtomicBool::new(false),
            60,
            "room append is unavailable",
            "room append output exceeded its bound",
            "room append failed",
        )
        .map(drop)
        .map_err(str::to_owned)
    }
}

/// Every `oms input` invocation is native, including help and argument errors.
pub fn owns(_args: &[OsString]) -> bool {
    true
}

/// Return exit status, stdout, stderr.
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(args, default_repo, interrupted, &DefaultHostServices)
}
pub fn run_with_services(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    services: &dyn HostServices,
) -> (i32, Vec<u8>, Vec<u8>) {
    if let Err(message) = clean_git_environment() {
        return (2, vec![], format!("error: {message}\n").into_bytes());
    }
    let options = match cli::parse(args) {
        Ok(options) => options,
        Err(exit) => return exit,
    };
    match execute(options, default_repo, interrupted, services) {
        Ok((code, output)) => (code, output, Vec::new()),
        Err(message) => (
            if message == LOCK_TIMEOUT { 75 } else { 2 },
            Vec::new(),
            format!("error: {message}\n").into_bytes(),
        ),
    }
}

pub(crate) fn child() -> bool {
    std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1")
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH").is_some_and(|v| v != "0")
}
fn checkpoint(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err("input capture interrupted")
    } else {
        Ok(())
    }
}
fn clean_git_environment() -> Result<()> {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            return Err(
                "input refuses Git repository/index redirect environment variables; select a clean --repo context",
            );
        }
    }
    Ok(())
}
fn utc_now() -> String {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn tracked_inputs(repo: &Path, interrupted: &AtomicBool) -> Result<Vec<u8>> {
    git(
        repo,
        &[
            OsStr::new("ls-files"),
            OsStr::new("--"),
            OsStr::new(".oms/inputs"),
        ],
        MAX_SOURCE,
        interrupted,
    )
}
fn text_option(o: &Options, limit: usize, base: &Path) -> Result<Option<String>> {
    if let Some(raw) = o.values.get("--text-file").filter(|v| !v.is_empty()) {
        let data = read_absolute(&expand_user(raw, base)?, limit)?;
        let body = String::from_utf8(data).map_err(|_| UNSAFE)?;
        safe_text(&body, limit)?;
        return Ok(Some(body));
    }
    o.values
        .get("--text")
        .map(|raw| {
            let body = raw.to_str().ok_or(UNSAFE)?;
            safe_text(body, limit)?;
            Ok(body.to_owned())
        })
        .transpose()
}
fn value_of<'a>(o: &'a Options, key: &str) -> Result<&'a str> {
    option_text(o, key)
}
fn texts(o: &Options, key: &str) -> Result<Vec<String>> {
    o.lists
        .get(key)
        .map(|values| {
            values
                .iter()
                .map(|v| v.to_str().map(str::to_owned).ok_or(UNSAFE))
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}
fn execute(
    o: Options,
    default_repo: &Path,
    interrupted: &AtomicBool,
    services: &dyn HostServices,
) -> Result<(i32, Vec<u8>)> {
    let level = o.level.ok_or(UNSAFE)?;
    let words: Vec<&OsStr> = o.positional.iter().map(OsString::as_os_str).collect();
    let target = words.last().copied().unwrap_or_default();
    let read_only = matches!(
        level,
        Level::One | Level::Many | Level::List | Level::Show | Level::Preview
    ) || level == Level::Deliver && !o.flags.contains("--apply");
    let child = child();
    if child && level != Level::One {
        return Err(
            "workers may retrieve only explicitly addressed attachment IDs; queue inspection is parent-only",
        );
    }
    checkpoint(interrupted)?;
    let inherited_default = std::env::var_os("OMS_ROOM_REPO").filter(|_| read_only && child);
    let selected = o
        .values
        .get("--repo")
        .map(Path::new)
        .or(inherited_default.as_deref().map(Path::new))
        .unwrap_or(default_repo);
    let selected = if selected.is_absolute() {
        selected.to_path_buf()
    } else {
        default_repo.join(selected)
    };
    let work = repository(&selected, interrupted)?;
    let repo = state_repository(&work, interrupted)?;
    if let Some(inherited) =
        std::env::var_os("OMS_ROOM_REPO").filter(|_| std::env::var_os("OMS_ROOM_ID").is_some())
        && state_repository(
            &repository(Path::new(&inherited), interrupted)?,
            interrupted,
        )? != repo
    {
        return Err("selected repository conflicts with the inherited room repository");
    }
    checkpoint(interrupted)?;
    let root = Root::open(&repo).map_err(|_| UNSAFE)?;
    let ctx = queue::Ctx {
        repo: &repo,
        root: &root,
        work: &work,
        interrupted,
        services,
    };
    let key = || Ok::<&str, &'static str>(target.to_str().unwrap_or("\u{fffd}"));
    let report = match level {
        Level::Many => object([
            ("schema", number(1)),
            ("attachments", J::Array(listing(&root)?)),
        ]),
        Level::One => {
            if child {
                queue::authorize_child_attachment(&ctx, key()?)?;
            }
            retrieve(
                &root,
                valid_id(target)?,
                o.flags.contains("--full"),
                interrupted,
            )?
        }
        Level::File => {
            let relative = relative_source(Path::new(words[words.len() - 1]))?;
            let source_root = Root::open(&work).map_err(|_| UNSAFE)?;
            let data = read(&source_root, &relative, MAX_SOURCE)?;
            let body = std::str::from_utf8(&data).map_err(|_| UNSAFE)?;
            let lines = split_lines(body);
            let start = line_option(&o, "--start-line")?;
            let end = line_option(&o, "--end-line")?;
            let (first, last, selected) = if start.is_some() || end.is_some() {
                let first = start.filter(|n| *n != 0).unwrap_or(1);
                let last = end.filter(|n| *n != 0).unwrap_or(lines.len() as i64);
                if first < 1 || first > last || last > lines.len() as i64 {
                    return Err("selected lines are outside the file");
                }
                (
                    first,
                    last,
                    lines[first as usize - 1..last as usize].concat(),
                )
            } else {
                (1, lines.len() as i64, body.to_owned())
            };
            let source = object([
                ("verified", J::Bool(true)),
                ("worktree", os_string(work.as_os_str())),
                ("path", os_string(relative.as_os_str())),
                ("full_file_sha256", string(&hash(&data))),
                ("full_file_bytes", number(data.len() as i64)),
                ("start_line", number(first)),
                ("end_line", number(last)),
            ]);
            create(&repo, "file", source, &selected, interrupted)?
        }
        Level::Selection => {
            let kind = option_text(&o, "--source")?;
            if !matches!(kind, "browser" | "chat" | "text") {
                return Err("invalid selection source");
            }
            let origin = option_text(&o, "--origin")?;
            validate_origin(origin)?;
            let content = text_option(&o, MAX_ATTACHMENT, default_repo)?.ok_or(EMPTY_TEXT)?;
            let source = object([
                ("verified", J::Bool(false)),
                ("kind", string(kind)),
                ("declared_origin", string(origin)),
                (
                    "note",
                    string(
                        "Origin was supplied by the caller; OMS did not inspect a browser or chat.",
                    ),
                ),
            ]);
            create(&repo, "selection", source, &content, interrupted)?
        }
        Level::Terminal => {
            let lines = line_option(&o, "--lines")?.unwrap_or(80);
            let (source, content) =
                terminal::capture(&work, option_text(&o, "--pane")?, lines, interrupted)?;
            create(&repo, "terminal", source, &content, interrupted)?
        }
        Level::List => queue::list(&ctx)?,
        Level::Add => queue::add(
            &ctx,
            value_of(&o, "--room")?,
            value_of(&o, "--from")?,
            value_of(&o, "--to")?,
            &text_option(&o, 3500, default_repo)?.ok_or(EMPTY_TEXT)?,
            o.values
                .get("--mode")
                .and_then(|m| m.to_str())
                .unwrap_or("next"),
            &texts(&o, "--attachment")?,
        )?,
        Level::Edit => {
            let replacements = texts(&o, "--attachment")?;
            let clear = o.flags.contains("--clear-attachments");
            if clear && !replacements.is_empty() {
                return Err("choose replacement attachments or --clear-attachments");
            }
            let keys = if clear {
                Some(Vec::new())
            } else {
                o.lists.get("--attachment").map(|_| replacements)
            };
            queue::edit(
                &ctx,
                key()?,
                text_option(&o, 3500, default_repo)?.as_deref(),
                o.values.get("--mode").and_then(|m| m.to_str()),
                keys.as_deref(),
            )?
        }
        Level::Move => queue::move_item(
            &ctx,
            key()?,
            o.values
                .get("--before")
                .filter(|v| !v.is_empty())
                .map(|v| v.to_str().ok_or(UNSAFE))
                .transpose()?,
        )?,
        Level::Cancel => queue::cancel(&ctx, key()?)?,
        Level::Show => queue::item(&queue::state(&ctx)?, key()?)?,
        Level::Preview => queue::preview(&ctx, &queue::item(&queue::state(&ctx)?, key()?)?)?,
        Level::Deliver => {
            let expected = o
                .values
                .get("--expected-sha256")
                .filter(|v| !v.is_empty())
                .map(|v| feedback::digest(v.to_str().ok_or(UNSAFE)?))
                .transpose()?;
            queue::deliver(
                &ctx,
                key()?,
                o.flags.contains("--apply"),
                o.flags.contains("--at-boundary"),
                expected,
            )?
        }
        Level::Reconcile => queue::reconcile(&ctx, key()?)?,
        Level::Feedback => feedback::enqueue(
            &ctx,
            value_of(&o, "--batch")?,
            value_of(&o, "--room")?,
            value_of(&o, "--from")?,
            value_of(&o, "--to")?,
            o.values
                .get("--mode")
                .and_then(|m| m.to_str())
                .unwrap_or("next"),
        )?,
        Level::Root | Level::Attach => return Err(UNSAFE),
    };
    let code = if json::str_of(&report, "status") == Some("uncertain") {
        3
    } else {
        0
    };
    Ok((code, json::encode(&report)?))
}
const EMPTY_TEXT: &str = "text is empty, oversized, or contains unsupported control characters";
fn read_opt(root: &Root, path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    match root.read_path(
        path,
        limit,
        Policy {
            max_components: None,
            ..Policy::manifest()
        },
    ) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(oms_safe_fs::Error::Missing) => Ok(None),
        Err(_) => Err("input file is missing, unreadable, or unsafe"),
    }
}

fn option_text<'a>(o: &'a Options, key: &str) -> Result<&'a str> {
    o.values
        .get(key)
        .and_then(|s| s.to_str())
        .ok_or("required input option is missing or invalid")
}
fn line_option(o: &Options, key: &str) -> Result<Option<i64>> {
    o.values
        .get(key)
        .map(|v| {
            v.to_str().ok_or(UNSAFE)?.parse().or_else(|_| {
                Ok(if v.as_bytes().starts_with(b"-") {
                    i64::MIN
                } else {
                    i64::MAX
                })
            })
        })
        .transpose()
}
fn relative_source(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err("file attachments require a repository-relative path");
    }
    let mut relative = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) if name != ".git" && name != ".oms" => relative.push(name),
            _ => return Err("file attachment escapes source scope or names private state"),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(UNSAFE);
    }
    Ok(relative)
}
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            let mut end = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, next)| *next == '\n') {
                end = iter.next().unwrap().0 + 1;
            }
            lines.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(root: &Root, path: &Path, limit: usize) -> Result<Vec<u8>> {
    root.read_path(
        path,
        limit,
        Policy {
            max_components: None,
            ..Policy::manifest()
        },
    )
    .map_err(|_| "input file is missing, unreadable, or unsafe")
}
fn read_absolute(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        return Err(UNSAFE);
    };
    // A root at / retains and validates every directory traversed, including the
    // caller's text-file parents, without canonicalizing away symbolic links.
    read(
        &Root::open("/").map_err(|_| UNSAFE)?,
        absolute.strip_prefix("/").map_err(|_| UNSAFE)?,
        limit,
    )
}
fn expand_user(raw: &OsStr, base: &Path) -> Result<PathBuf> {
    let bytes = raw.as_bytes();
    let path = if bytes == b"~" || bytes.starts_with(b"~/") {
        let home = std::env::var_os("HOME").ok_or(UNSAFE)?;
        PathBuf::from(home).join(OsStr::from_bytes(if bytes.len() > 1 {
            &bytes[2..]
        } else {
            b""
        }))
    } else {
        PathBuf::from(raw)
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    // abspath is lexical; it must never follow a source symlink.
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(part),
        }
    }
    Ok(normalized)
}
fn valid_id(raw: &OsStr) -> Result<&str> {
    let id = raw.to_str().ok_or("expected an exact local identifier")?;
    if id.is_empty()
        || id.len() > 160
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("expected an exact local identifier");
    }
    Ok(id)
}

fn secret_rule() -> &'static Regex {
    static RULE: OnceLock<Regex> = OnceLock::new();
    RULE.get_or_init(|| {
        let names = [
            "api[_-]?key",
            "access[_-]?token",
            "client[_-]?secret",
            "passwor[d]",
            "private[_-]?key",
        ]
        .join("|");
        Regex::new(&format!(r#"(?i)(?:{names})["']?\s*[:=]|-----BE[G]IN [A-Z ]+PRIVATE KEY-----|\b(?:ghp|github_pat|sk)-[A-Za-z0-9_-]{{12,}}\b|\bBearer\s+[A-Za-z0-9._~+/-]{{12,}}={{0,2}}\b|(?-i:\b(?:gh[pousr]_|github_[p]at_|sk-(?:proj-|ant-)?)[A-Za-z0-9_-]{{12,}})"#)).unwrap()
    })
}
/// Refuse text that reads as a credential assignment, token or private key.
fn reject_secrets(value: &str) -> Result<()> {
    if secret_rule().is_match(value) {
        return Err("input contains secret-like content; redact it before attachment or delivery");
    }
    Ok(())
}
fn safe_text(value: &str, limit: usize) -> Result<()> {
    if value.len() > limit
        || value.trim().is_empty()
        || value
            .chars()
            .any(|c| c < ' ' && !matches!(c, '\r' | '\n' | '\t'))
    {
        return Err("text is empty, oversized, or contains unsupported control characters");
    }
    reject_secrets(value)?;
    Ok(())
}
fn validate_origin(origin: &str) -> Result<()> {
    safe_text(origin, 1000)?;
    if origin.contains(['\r', '\n', '\t']) {
        return Err("selection origin must be one line");
    }
    let authority = origin
        .split_once("//")
        .map(|(_, s)| s.split(['/', '?', '#']).next().unwrap_or(""));
    if let Some(authority) = authority {
        let host = authority.rsplit('@').next().unwrap_or("");
        if host.contains(['[', ']']) {
            let host = host
                .strip_prefix('[')
                .and_then(|s| s.split_once(']'))
                .ok_or(UNSAFE)?;
            if !host.1.is_empty() && !host.1.starts_with(':') {
                return Err(UNSAFE);
            }
            let address = host.0.split('%').next().unwrap_or("");
            if address.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(UNSAFE);
            }
        }
    }
    if authority.is_some_and(|s| s.contains('@')) {
        return Err("selection origin must not contain URL credentials");
    }
    static QUERY_RULE: OnceLock<Regex> = OnceLock::new();
    let query_rule = QUERY_RULE
        .get_or_init(|| Regex::new("token|secret|password|credential|signature|api.?key").unwrap());
    if let Some((_, query)) = origin.split_once('?') {
        for parameter in query.split('#').next().unwrap_or("").split('&') {
            let (key, value) = parameter.split_once('=').unwrap_or((parameter, ""));
            if value.is_empty() {
                continue;
            }
            let key = percent_decode(key).to_lowercase();
            if query_rule.is_match(&key) {
                return Err("selection origin must not contain a credential-bearing query");
            }
        }
    }
    Ok(())
}
fn percent_decode(raw: &str) -> String {
    let mut result = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            result.push((a * 16 + b) as u8);
            i += 3;
        } else {
            result.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn git(repo: &Path, args: &[&OsStr], limit: usize, interrupted: &AtomicBool) -> Result<Vec<u8>> {
    storage::bounded_git(repo, args, limit, interrupted)
}
fn repository(selected: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
    let bytes = git(
        selected,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        64 * 1024,
        interrupted,
    )?;
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let root = PathBuf::from(OsString::from_vec(bytes.to_vec()))
        .canonicalize()
        .map_err(|_| "inputs require a Git repository root")?;
    if root.as_os_str().as_bytes().iter().any(|b| *b < 32) {
        return Err("repository path contains unsupported control characters");
    }
    Ok(root)
}
fn state_repository(work: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
    repository::state(work, interrupted)
}

fn create(
    repo: &Path,
    kind: &str,
    source: J,
    content: &str,
    interrupted: &AtomicBool,
) -> Result<J> {
    safe_text(
        content,
        if kind == "artifact-feedback" {
            MAX_FEEDBACK
        } else {
            MAX_ATTACHMENT
        },
    )?;
    checkpoint(interrupted)?;
    let tracked = git(
        repo,
        &[
            OsStr::new("ls-files"),
            OsStr::new("--"),
            OsStr::new(".oms/inputs"),
        ],
        MAX_SOURCE,
        interrupted,
    )?;
    if !tracked.is_empty() {
        return Err("private input state must not be tracked by Git");
    }
    storage::create(
        repo,
        interrupted,
        |id| {
            object([
                ("schema", number(1)),
                ("id", string(id)),
                ("repo", os_string(repo.as_os_str())),
                ("kind", string(kind)),
                ("source", source),
                ("content_sha256", string(&hash(content.as_bytes()))),
                ("content_bytes", number(content.len() as i64)),
                ("created_at", string(&utc_now())),
            ])
        },
        content.as_bytes(),
    )
}
fn load(root: &Root, id: &str) -> Result<(J, String)> {
    valid_id(OsStr::new(id))?;
    let path = PathBuf::from(".oms/inputs/attachments").join(id);
    let meta = read_opt(root, &path.join("meta.json"), MAX_ATTACHMENT)?
        .ok_or("attachment is missing or belongs to another repository")?;
    let record = json::parse(&meta)?;
    if field(&record, "schema")? != &number(1)
        || text(field(&record, "id")?)? != id
        || field(&record, "repo")? != &os_string(root.physical_root().as_os_str())
    {
        return Err("attachment is missing or belongs to another repository");
    }
    let kind = text(field(&record, "kind")?)?;
    let source = field(&record, "source")?;
    if !matches!(source, J::Object(_)) {
        return Err(UNSAFE);
    }
    text(field(&record, "created_at")?)?;
    match kind {
        "file" => {
            json::os_path(field(source, "worktree")?)?;
            json::os_path(field(source, "path")?)?;
            text(field(source, "full_file_sha256")?)?;
        }
        "artifact-feedback" => {
            for key in ["view_id", "data_sha256", "registry_sha256"] {
                text(field(source, key)?)?;
            }
        }
        "terminal" => {
            for key in ["pane", "pid", "cwd"] {
                text(field(source, key)?)?;
            }
        }
        _ => {}
    }
    let limit = if kind == "artifact-feedback" {
        MAX_FEEDBACK
    } else {
        MAX_ATTACHMENT
    };
    let data = read(root, &path.join("content.txt"), limit)?;
    if field(&record, "content_bytes")? != &number(data.len() as i64)
        || text(field(&record, "content_sha256")?)? != hash(&data)
    {
        return Err("immutable attachment content has changed");
    }
    let content = String::from_utf8(data).map_err(|_| UNSAFE)?;
    Ok((record, content))
}
fn listing(root: &Root) -> Result<Vec<J>> {
    let names = storage::attachment_names(root.physical_root())?;
    let mut result = Vec::new();
    for id in names {
        if storage::metadata_exists(root.physical_root(), &id)? {
            match load(root, valid_id(&id)?) {
                Ok((record, _)) => result.push(record),
                Err(error) => {
                    if storage::metadata_exists(root.physical_root(), &id)? {
                        return Err(error);
                    }
                }
            }
        }
    }
    for record in &result {
        text(field(record, "created_at")?)?;
    }
    result.sort_by(|a, b| {
        text(field(a, "created_at").unwrap_or(&J::Null))
            .unwrap_or("")
            .cmp(text(field(b, "created_at").unwrap_or(&J::Null)).unwrap_or(""))
    });
    Ok(result)
}
fn validate(root: &Root, record: &J, interrupted: &AtomicBool) -> Result<bool> {
    let source = field(record, "source")?;
    match text(field(record, "kind")?)? {
        "file" => {
            let work = json::os_path(field(source, "worktree")?)?;
            let path = json::os_path(field(source, "path")?)?;
            if path.is_absolute() {
                return Err(UNSAFE);
            }
            let path = relative_source(&path)
                .map_err(|_| "attachment source escapes its recorded worktree")?;
            if storage::plain(&work)? != work {
                return Err(UNSAFE);
            }
            let source_root = Root::open(&work).map_err(|_| UNSAFE)?;
            let bytes = read(&source_root, &path, MAX_SOURCE)?;
            if text(field(source, "full_file_sha256")?)? != hash(&bytes) {
                return Err(
                    "file attachment source drifted; capture a new attachment before delivery",
                );
            }
        }
        "terminal" => terminal::matches_record(source, interrupted)?,
        "selection" => {}
        "artifact-feedback" => {
            let view = valid_id(OsStr::new(text(field(source, "view_id")?)?))?;
            let drifted = "feedback view source changed";
            let registry = json::parse(&read(
                root,
                &PathBuf::from(".oms/artifact-views").join(format!("{view}.json")),
                MAX_FEEDBACK,
            )?)?;
            if field(&registry, "id")? != field(source, "view_id")?
                || field(&registry, "data_sha256")? != field(source, "data_sha256")?
                || hash(&json::canonical(&registry)?) != text(field(source, "registry_sha256")?)?
            {
                return Err(drifted);
            }
        }
        _ => return Err("unknown attachment source kind"),
    }
    Ok(field(source, "verified").is_ok_and(|v| v == &J::Bool(true)))
}
fn retrieve(root: &Root, id: &str, full: bool, interrupted: &AtomicBool) -> Result<J> {
    let (mut record, content) = load(root, id)?;
    let status = match validate(root, &record, interrupted) {
        Ok(true) => "current",
        Ok(false) => "declared-unverified",
        Err(_) => "drifted-or-unavailable",
    };
    let mut end = if full {
        content.len()
    } else {
        content.len().min(8192)
    };
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let J::Object(fields) = &mut record else {
        return Err(UNSAFE);
    };
    fields
        .retain(|(key, _)| !matches!(key.as_slice(), b"content" | b"truncated" | b"source_status"));
    fields.extend([
        (b"content".to_vec(), string(&content[..end])),
        (b"truncated".to_vec(), J::Bool(end < content.len())),
        (b"source_status".to_vec(), string(status)),
    ]);
    Ok(record)
}
