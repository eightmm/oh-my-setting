//! Private terminal records and positive process-generation proofs.
use crate::services::ExecutionRequest;
use crate::{Error, Result, Services, Store, Value, require};
use oms_reference::process::{IdentitySource, NativeIdentity, Owner, OwnerError};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const MAX_RECORD: usize = 65536;
pub const MAX_READ: usize = 32768;
pub const MAX_ITEMS: usize = 128;
pub const MAX_ARCHIVED: usize = 512;
pub fn process(pid: u32) -> Result<Owner> {
    NativeIdentity
        .owner_identity(pid)
        .map_err(|_| Error::new("native process generation unavailable"))
}
pub fn process_gone(owner: &Owner) -> bool {
    match NativeIdentity.owner_identity(owner.pid) {
        Ok(current) => current != *owner,
        Err(OwnerError::Exited) => true,
        Err(_) => false,
    }
}
pub fn ordinary_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| {
            let name = k.to_string_lossy();
            ![
                "OMS_",
                "CLAUDE_CODE_",
                "CODEX_THREAD_",
                "CODEX_SESSION_",
                "AGY_SESSION_",
                "ANTIGRAVITY_SESSION_",
            ]
            .iter()
            .any(|p| name.starts_with(p))
                && ![
                    "CLAUDECODE",
                    "CLAUDE_SESSION_ID",
                    "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
                ]
                .contains(&name.as_ref())
        })
        .collect()
}
pub fn state_home() -> Result<PathBuf> {
    let path = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".local/state")))
        .ok_or_else(|| Error::new("private state home unavailable"))?;
    require(
        path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "state home must be absolute and normalized",
    )?;
    Ok(path)
}
fn directory(path: &Path, create: bool, private_start: usize) -> Result<()> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = fs::open("/", flags, Mode::empty()).map_err(Error::new)?;
    for (index, name) in path
        .components()
        .filter_map(|v| {
            if let Component::Normal(v) = v {
                Some(v)
            } else {
                None
            }
        })
        .enumerate()
    {
        if create {
            match fs::mkdirat(&parent, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(Error::new(e)),
            }
        }
        let child = fs::openat(&parent, name, flags, Mode::empty())
            .map_err(|_| Error::new("private directory unavailable or unsafe"))?;
        let named = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(Error::new)?;
        let info = fs::fstat(&child).map_err(Error::new)?;
        require(
            FileType::from_raw_mode(info.st_mode) == FileType::Directory
                && named.st_dev == info.st_dev
                && named.st_ino == info.st_ino,
            "private directory changed",
        )?;
        if index >= private_start {
            require(
                info.st_uid == rustix::process::getuid().as_raw() && info.st_mode & 0o077 == 0,
                "unsafe private directory",
            )?;
        } else if index + 1 == private_start {
            require(
                info.st_uid == rustix::process::getuid().as_raw() && info.st_mode & 0o022 == 0,
                "unsafe state directory",
            )?;
        }
        parent = child;
    }
    Ok(())
}
pub fn valid_id(ident: &str) -> bool {
    ident
        .strip_prefix("term-")
        .or_else(|| ident.strip_prefix("job-"))
        .is_some_and(|v| crate::hex(v, 32))
}
/// Matches the existing per-repository private namespace. Record files are never repaired.
pub struct PrivateStore {
    pub repo: PathBuf,
    pub root: PathBuf,
    pub kind: String,
    pub archived: bool,
    pub(crate) store: Store,
}
impl PrivateStore {
    pub fn open(repo: &Path, kind: &str, archived: bool, create: bool) -> Result<Self> {
        Self::at(repo, kind, archived, create, &state_home()?)
    }
    pub fn open_optional(repo: &Path, kind: &str, archived: bool) -> Result<Option<Self>> {
        let base = state_home()?;
        let canon = std::fs::canonicalize(repo).map_err(Error::new)?;
        let namespace = crate::digest(canon.as_os_str().as_encoded_bytes());
        let root = base
            .join("oh-my-setting/panel-control")
            .join(namespace)
            .join(format!("{kind}{}", if archived { "-archive" } else { "" }));
        match std::fs::symlink_metadata(&root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::new(e)),
            Ok(_) => Self::at(&canon, kind, archived, false, &base).map(Some),
        }
    }
    pub fn at(repo: &Path, kind: &str, archived: bool, create: bool, base: &Path) -> Result<Self> {
        require(
            ["terminals", "workspaces", "jobs"].contains(&kind),
            "invalid managed record kind",
        )?;
        let repo = std::fs::canonicalize(repo).map_err(Error::new)?;
        require(
            base.is_absolute()
                && !base.starts_with(&repo)
                && !base.components().any(|v| matches!(v, Component::ParentDir)),
            "private state must be normalized and outside repository",
        )?;
        let namespace = crate::digest(repo.as_os_str().as_encoded_bytes());
        let root = base
            .join("oh-my-setting/panel-control")
            .join(namespace)
            .join(format!("{kind}{}", if archived { "-archive" } else { "" }));
        let base_count = base
            .components()
            .filter(|v| matches!(v, Component::Normal(_)))
            .count();
        directory(&root, create, base_count + 1)?;
        Ok(Self {
            repo,
            store: Store::open(&root)?,
            root,
            kind: kind.into(),
            archived,
        })
    }
    pub fn identifiers(&self) -> Result<Vec<String>> {
        let mut ids = vec![];
        for entry in std::fs::read_dir(&self.root).map_err(Error::new)? {
            let entry = entry.map_err(Error::new)?;
            if let Some(id) = entry.file_name().to_str()
                && valid_id(id)
            {
                ids.push(id.to_owned());
            }
        }
        require(
            ids.len()
                <= if self.archived {
                    MAX_ARCHIVED
                } else {
                    MAX_ITEMS
                },
            "managed record capacity exceeded",
        )?;
        ids.sort();
        Ok(ids)
    }
    pub fn record(&self, ident: &str) -> Result<Value> {
        require(valid_id(ident), "invalid managed record ID")?;
        let path = PathBuf::from(ident).join("record");
        let info = std::fs::symlink_metadata(self.root.join(&path)).map_err(Error::new)?;
        require(
            info.uid() == rustix::process::getuid().as_raw() && info.mode() & 0o077 == 0,
            "unsafe private record",
        )?;
        let row = self
            .store
            .json(&path, MAX_RECORD, false)?
            .ok_or_else(|| Error::new("managed record missing"))?;
        require(
            row["schema"] == 1
                && row["id"] == ident
                && row["repo"] == self.repo.to_string_lossy().as_ref()
                && row["uid"].as_u64() == Some(u64::from(rustix::process::getuid().as_raw()))
                && row["archived"].as_bool().unwrap_or(false) == self.archived,
            "invalid managed record binding",
        )?;
        Ok(row)
    }
    pub fn allocate(&self, prefix: &str, kind: &str, fields: &Value) -> Result<Value> {
        crate::parent_only()?;
        require(
            !self.archived && ["term", "job"].contains(&prefix) && fields.is_object(),
            "invalid record allocation",
        )?;
        let tx = self
            .store
            .lock_at(Path::new("lock"), Duration::from_secs(3))?;
        require(
            self.identifiers()?.len() < MAX_ITEMS,
            "active slots are full; archive a completed item",
        )?;
        let id = format!("{prefix}-{}", crate::value::unique()?);
        let mut row = fields.clone();
        row["schema"] = 1.into();
        row["id"] = id.clone().into();
        row["repo"] = self.repo.to_string_lossy().as_ref().into();
        row["uid"] = rustix::process::getuid().as_raw().into();
        row["kind"] = kind.into();
        tx.write_json(&PathBuf::from(&id).join("record"), &row, true, MAX_RECORD)?;
        Ok(row)
    }
    pub fn update(&self, ident: &str, expected: &Value, changed: &Value) -> Result<()> {
        crate::parent_only()?;
        let tx = self
            .store
            .lock_at(Path::new("lock"), Duration::from_secs(3))?;
        require(self.record(ident)? == *expected, "managed record changed")?;
        tx.write_json(
            &PathBuf::from(ident).join("record"),
            changed,
            false,
            MAX_RECORD,
        )?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<Value>> {
        self.identifiers()?.iter().map(|v| self.record(v)).collect()
    }
}
pub fn preview(raw: &[u8], maximum: usize) -> Result<Value> {
    require(
        (1..=MAX_READ).contains(&maximum),
        "read limit must be between 1 and 32768 bytes",
    )?;
    let source_truncated = raw.len() > MAX_RECORD;
    let raw = &raw[..raw.len().min(MAX_RECORD)];
    let raw = String::from_utf8_lossy(raw);
    let ansi =
        regex::Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\|$)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]")
            .map_err(Error::new)?;
    let plain = ansi
        .replace_all(&raw, "")
        .chars()
        .filter(|c| matches!(c, '\n' | '\t') || (!c.is_control()))
        .collect::<String>();
    let long = plain.lines().any(|v| v.len() > 1024);
    let bounded = plain
        .lines()
        .map(|v| {
            if v.len() > 1024 {
                "[oversized line omitted]"
            } else {
                v
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let safe = crate::value::redact(bounded.as_bytes())?;
    let mut end = safe.len().min(maximum);
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    Ok(
        serde_json::json!({"text":&safe[..end],"truncated":source_truncated || long || safe.len()>maximum}),
    )
}
pub fn tmux(
    service: &dyn Services,
    repo: &Path,
    argv: &[OsString],
    stop: &AtomicBool,
) -> Result<String> {
    let mut full = vec!["tmux".into()];
    full.extend_from_slice(argv);
    let mut request = ExecutionRequest::new(full, repo);
    request.timeout = Duration::from_secs(3);
    request.output_limit = 1024 * 1024;
    let output = service.execute(&request, stop)?;
    output.success()?;
    String::from_utf8(output.stdout)
        .map(|v| v.replace('\r', "").trim().into())
        .map_err(Error::new)
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionProof {
    pub session: String,
    pub server_pid: u32,
    pub server_birth: String,
    pub socket_digest: String,
}
pub(crate) fn target_id(value: &Value, prefix: char) -> bool {
    value
        .as_str()
        .and_then(|v| v.strip_prefix(prefix))
        .is_some_and(|v| !v.is_empty() && v.bytes().all(|v| v.is_ascii_digit()))
}
/// Probe the recorded server independently of a session that may be gone.
pub(crate) fn server_present(
    repo: &Path,
    proof: &SessionProof,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    require(
        proof.server_pid > 0
            && !proof.server_birth.is_empty()
            && crate::hex(&proof.socket_digest, 64),
        "invalid server proof",
    )?;
    let owner = Owner {
        pid: proof.server_pid,
        birth: proof.server_birth.clone(),
        uid: rustix::process::getuid().as_raw(),
    };
    if process_gone(&owner) {
        return Ok(false);
    }
    let output = tmux(
        service,
        repo,
        &crate::services::args(&["display-message", "-p", "#{pid}\t#{socket_path}"]),
        stop,
    )?;
    let fields: Vec<_> = output.split('\t').collect();
    require(
        fields.len() == 2
            && fields[0] == proof.server_pid.to_string()
            && crate::digest(fields[1].as_bytes()) == proof.socket_digest
            && process(proof.server_pid)? == owner,
        "server identity changed",
    )?;
    Ok(true)
}
/// Positive global pane disappearance on the recorded, repository-owned host.
pub fn terminal_gone(
    repo: &Path,
    row: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    require(
        row["repo"]
            == std::fs::canonicalize(repo)
                .map_err(Error::new)?
                .to_string_lossy()
                .as_ref()
            && row["kind"] == "shell"
            && target_id(&row["pane"], '%'),
        "invalid terminal binding",
    )?;
    let proof: SessionProof =
        serde_json::from_value(row["session_proof"].clone()).map_err(Error::new)?;
    if !server_present(repo, &proof, service, stop)? {
        return Ok(true);
    }
    require(
        session_proof(service, repo, &proof.session, stop)? == proof,
        "terminal host ownership changed",
    )?;
    let panes = tmux(
        service,
        repo,
        &crate::services::args(&["list-panes", "-a", "-F", "#{pane_id}"]),
        stop,
    )?;
    require(
        panes.lines().all(|v| target_id(&v.into(), '%')),
        "invalid pane inventory",
    )?;
    Ok(!panes.lines().any(|v| row["pane"] == v))
}
pub fn forget_terminal(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::history::forget_record(
        repo,
        "terminals",
        ident,
        |row| terminal_gone(repo, row, service, stop),
        &["record"],
    )
}
pub fn session_proof(
    service: &dyn Services,
    repo: &Path,
    session: &str,
    stop: &AtomicBool,
) -> Result<SessionProof> {
    require(
        session
            .strip_prefix('$')
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|v| v.is_ascii_digit())),
        "explicit tmux session ID required",
    )?;
    let output = tmux(
        service,
        repo,
        &crate::services::args(&[
            "display-message",
            "-p",
            "-t",
            session,
            "#{session_id}\t#{pid}\t#{socket_path}",
        ]),
        stop,
    )?;
    let fields: Vec<_> = output.split('\t').collect();
    require(
        fields.len() == 3 && fields[0] == session,
        "session identity unavailable",
    )?;
    let owner = tmux(
        service,
        repo,
        &crate::services::args(&["show-options", "-v", "-t", session, "@oms_panel_repo"]),
        stop,
    )?;
    require(
        owner
            == std::fs::canonicalize(repo)
                .map_err(Error::new)?
                .to_string_lossy(),
        "session is not owned by selected repository",
    )?;
    let pid = fields[1].parse().map_err(Error::new)?;
    let process = process(pid)?;
    Ok(SessionProof {
        session: session.into(),
        server_pid: pid,
        server_birth: process.birth,
        socket_digest: crate::digest(fields[2].as_bytes()),
    })
}
pub fn open_terminal(
    repo: &Path,
    session: &str,
    label: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let proof = session_proof(service, repo, session, stop)?;
    let store = PrivateStore::open(repo, "terminals", false, true)?;
    let row = store.allocate("term","shell",&serde_json::json!({"session_proof":proof,"label":crate::eventlog::clean(label,120),"state":"starting"}))?;
    let ident = row["id"].as_str().unwrap();
    require(
        session_proof(service, repo, session, stop)? == proof,
        "session changed before terminal launch",
    )?;
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| Path::new(s).is_absolute())
        .unwrap_or_else(|| "/bin/sh".into());
    let command = format!("exec {} -l", shlex::try_quote(&shell).map_err(Error::new)?);
    let mut argv = crate::services::args(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}\t#{pane_id}",
        "-t",
        session,
        "-n",
        &format!("oms-{}", &ident[5..17]),
        "-c",
    ]);
    argv.push(repo.into());
    argv.push(command.into());
    let created = tmux(service, repo, &argv, stop)?;
    let parts: Vec<_> = created.split('\t').collect();
    require(
        parts.len() == 2 && parts[0].starts_with('@') && parts[1].starts_with('%'),
        "terminal launch binding unavailable; preserve starting record",
    )?;
    let mut changed = row.clone();
    changed["window"] = parts[0].into();
    changed["pane"] = parts[1].into();
    changed["state"] = "running".into();
    store.update(ident, &row, &changed)?;
    Ok(changed)
}
pub fn switch_commands(
    service: &dyn Services,
    repo: &Path,
    target_session: &str,
    target_window: &str,
    target_pane: &str,
    stop: &AtomicBool,
) -> Result<Vec<Vec<OsString>>> {
    require(
        target_id(&Value::String(target_session.to_owned()), '$')
            && target_id(&Value::String(target_window.to_owned()), '@')
            && target_id(&Value::String(target_pane.to_owned()), '%'),
        "invalid tmux target identifiers",
    )?;
    let caller = std::env::var("TMUX_PANE").unwrap_or_default();
    require(
        target_id(&Value::String(caller.clone()), '%'),
        "current tmux pane is unavailable; select the original terminal",
    )?;
    let current = tmux(
        service,
        repo,
        &crate::services::args(&["display-message", "-p", "-t", &caller, "#{session_id}"]),
        stop,
    )?;
    let current_session = current.trim();
    require(
        target_id(&Value::String(current_session.to_owned()), '$'),
        "current tmux session is unavailable; native session was preserved",
    )?;
    let mut commands = vec![
        crate::services::args(&["select-window", "-t", target_window]),
        crate::services::args(&["select-pane", "-t", target_pane]),
    ];
    if current_session != target_session {
        let out = tmux(
            service,
            repo,
            &crate::services::args(&["list-clients", "-F", "#{client_name}\t#{session_id}"]),
            stop,
        )?;
        let clients: Vec<String> = out
            .lines()
            .filter_map(|line| {
                let (client, sess) = line.split_once('\t')?;
                if sess == current_session
                    && !client.is_empty()
                    && client.len() <= 256
                    && !client.starts_with('-')
                    && crate::eventlog::clean(client, 256) == client
                {
                    Some(client.to_owned())
                } else {
                    None
                }
            })
            .collect();
        require(
            clients.len() == 1,
            "unique attached client required to switch tmux sessions",
        )?;
        commands.push(crate::services::args(&[
            "switch-client",
            "-c",
            &clients[0],
            "-t",
            target_session,
        ]));
    }
    Ok(commands)
}

pub fn terminal_proof(
    repo: &Path,
    row: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<(String, String, String)> {
    let canon = std::fs::canonicalize(repo).map_err(Error::new)?;
    require(
        row["archived"].as_bool() != Some(true)
            && row["kind"] == "shell"
            && row["repo"] == canon.to_string_lossy().as_ref()
            && target_id(&row["pane"], '%'),
        "invalid or archived terminal binding",
    )?;
    let proof: SessionProof =
        serde_json::from_value(row["session_proof"].clone()).map_err(Error::new)?;
    require(
        session_proof(service, repo, &proof.session, stop)? == proof,
        "terminal host ownership changed",
    )?;
    let pane = row["pane"].as_str().unwrap();
    let info = tmux(
        service,
        repo,
        &crate::services::args(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_dead}",
        ]),
        stop,
    )?;
    let fields: Vec<&str> = info.trim_end_matches(['\r', '\n']).split('\t').collect();
    require(
        fields.len() == 4
            && fields[0] == proof.session
            && fields[2] == pane
            && fields[3] == "0"
            && target_id(&Value::String(fields[1].to_owned()), '@'),
        "terminal pane changed or exited",
    )?;
    Ok((proof.session, fields[1].to_owned(), pane.to_owned()))
}

pub fn public_terminal(
    repo: &Path,
    row: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Value {
    let state = if terminal_proof(repo, row, service, stop).is_ok() {
        "running"
    } else if terminal_gone(repo, row, service, stop).unwrap_or(false) {
        "gone"
    } else {
        "unknown"
    };
    let archived = row["archived"] == true;
    serde_json::json!({
        "id": row["id"],
        "label": crate::eventlog::clean(row["label"].as_str().unwrap_or("Terminal"), 120),
        "kind": "shell",
        "state": state,
        "archived": archived,
        "capabilities": {
            "focus": state == "running",
            "read": state == "running",
            "restart": false,
            "forget": state == "gone",
            "archive": state == "gone" && !archived,
            "restore": state == "gone" && archived,
        },
    })
}

pub fn list_terminals(
    repo: &Path,
    archived: bool,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Vec<Value>> {
    crate::parent_only()?;
    let Some(store) = PrivateStore::open_optional(repo, "terminals", archived)? else {
        return Ok(vec![]);
    };
    let mut rows = Vec::new();
    for id in store.identifiers()? {
        let row = store.record(&id)?;
        require(row["kind"] == "shell", "invalid terminal record kind")?;
        rows.push(public_terminal(repo, &row, service, stop));
    }
    Ok(rows)
}

pub fn focus_terminal(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let store = PrivateStore::open(repo, "terminals", false, false)?;
    let row = store.record(ident)?;
    let (session, window, pane) = terminal_proof(repo, &row, service, stop)?;
    let commands = switch_commands(service, repo, &session, &window, &pane, stop)?;
    for cmd in &commands {
        terminal_proof(repo, &row, service, stop)?;
        tmux(service, repo, cmd, stop)?;
    }
    Ok(public_terminal(repo, &row, service, stop))
}

pub fn archive_terminal(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::history::archive(repo, "terminals", ident, |row| {
        terminal_gone(repo, row, service, stop)
    })
}

pub fn restore_terminal(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::history::restore(repo, "terminals", ident, |row| {
        terminal_gone(repo, row, service, stop)
    })
}

pub fn read_terminal(
    repo: &Path,
    ident: &str,
    maximum: usize,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let store = PrivateStore::open(repo, "terminals", false, false)?;
    let row = store.record(ident)?;
    let (_, _, pane) = terminal_proof(repo, &row, service, stop)?;
    let raw = tmux(
        service,
        repo,
        &crate::services::args(&["capture-pane", "-p", "-t", &pane, "-S", "-200"]),
        stop,
    )?;
    preview(raw.as_bytes(), maximum)
}
/// Private dispatch namespace, selected from this controller's environment only.
pub fn controller_store(repo: &Path, create: bool) -> Result<Store> {
    let repo = std::fs::canonicalize(repo).map_err(Error::new)?;
    let base = state_home()?;
    let root = base
        .join("oh-my-setting/panel-control")
        .join(crate::digest(repo.as_os_str().as_encoded_bytes()))
        .join("dispatch");
    require(
        !root.starts_with(&repo),
        "dispatch state must be outside repository",
    )?;
    let count = base
        .components()
        .filter(|v| matches!(v, Component::Normal(_)))
        .count();
    directory(&root, create, count + 1)?;
    Store::open(&root)
}

/// Signal only the observed generation. Linux pidfds close the PID reuse race;
/// the POSIX fallback rechecks identity immediately before the kernel call.
pub fn signal_process(owner: &Owner, signal: rustix::process::Signal) -> Result<bool> {
    crate::parent_only()?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(owner.pid).map_err(Error::new)?)
        .ok_or_else(|| Error::new("invalid process ID"))?;
    require(
        owner.uid == rustix::process::getuid().as_raw() && owner.pid != std::process::id(),
        "refusing foreign or controller process",
    )?;
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{PidfdFlags, pidfd_open, pidfd_send_signal};
        match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(fd) => {
                if process_gone(owner) {
                    return Ok(false);
                }
                require(
                    process(owner.pid)? == *owner,
                    "process identity unconfirmed",
                )?;
                return match pidfd_send_signal(fd, signal) {
                    Ok(()) => Ok(true),
                    Err(rustix::io::Errno::SRCH) => Ok(false),
                    Err(e) => Err(Error::new(e)),
                };
            }
            Err(rustix::io::Errno::SRCH) => return Ok(false),
            Err(rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL | rustix::io::Errno::PERM) => {}
            Err(e) => return Err(Error::new(e)),
        }
    }
    if process_gone(owner) {
        return Ok(false);
    }
    require(
        process(owner.pid)? == *owner,
        "process identity unconfirmed",
    )?;
    match rustix::process::kill_process(pid, signal) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(e) => Err(Error::new(e)),
    }
}
/// Stop one explicitly bound terminal pane. Server/session and pane identity
/// are rechecked before the one-shot host mutation; an ambiguous result refuses.
pub fn stop_terminal(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let store = PrivateStore::open(repo, "terminals", false, false)?;
    let row = store.record(ident)?;
    let proof: SessionProof =
        serde_json::from_value(row["session_proof"].clone()).map_err(Error::new)?;
    require(
        session_proof(service, repo, &proof.session, stop)? == proof,
        "terminal live host identity changed",
    )?;
    let pane = row["pane"]
        .as_str()
        .ok_or_else(|| Error::new("terminal launch unconfirmed"))?;
    require(
        pane.strip_prefix('%')
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
        "invalid terminal pane",
    )?;
    let expected = format!(
        "{}\\t{}\\t{}",
        proof.session,
        row["window"].as_str().unwrap_or_default(),
        pane
    );
    let binding = tmux(
        service,
        repo,
        &crate::services::args(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_id}\t#{window_id}\t#{pane_id}",
        ]),
        stop,
    )?;
    require(binding == expected, "terminal pane binding changed")?;
    require(
        session_proof(service, repo, &proof.session, stop)? == proof && store.record(ident)? == row,
        "terminal generation changed before stop",
    )?;
    tmux(
        service,
        repo,
        &crate::services::args(&["kill-pane", "-t", pane]),
        stop,
    )
    .map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    let panes = tmux(
        service,
        repo,
        &crate::services::args(&["list-panes", "-a", "-F", "#{pane_id}"]),
        stop,
    )?;
    require(
        !panes.lines().any(|v| v == pane),
        "terminal pane exit unconfirmed",
    )?;
    let mut changed = row.clone();
    changed["state"] = "stopped".into();
    changed["host_pane_absent"] = true.into();
    // Pane disappearance does not prove every escaped shell descendant exited.
    changed["cleanup_complete"] = false.into();
    store.update(ident, &row, &changed).map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    Ok(changed)
}

/// POSIX group signaling requires its live group leader's exact generation.
/// Sending a signal proves neither descendant containment nor group completion.
pub fn signal_process_group(owner: &Owner, signal: rustix::process::Signal) -> Result<bool> {
    crate::parent_only()?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(owner.pid).map_err(Error::new)?)
        .ok_or_else(|| Error::new("invalid group leader"))?;
    require(
        owner.uid == rustix::process::getuid().as_raw()
            && pid != rustix::process::getpid()
            && rustix::process::getpgid(None).map_err(Error::new)? != pid,
        "refusing foreign or controller group",
    )?;
    if process_gone(owner) {
        return Ok(false);
    }
    require(
        process(owner.pid)? == *owner
            && rustix::process::getpgid(Some(pid)).map_err(Error::new)? == pid,
        "group leader generation changed",
    )?;
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(e) => Err(Error::new(e)),
    }
}
