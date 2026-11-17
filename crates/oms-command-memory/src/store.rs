use crate::{
    records::{self, Entry},
    *,
};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
const MAX: usize = isize::MAX as usize;
static SERIAL: AtomicU64 = AtomicU64::new(0);
pub(crate) fn environment() -> Vec<(Vec<u8>, Vec<u8>)> {
    std::env::vars_os()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect()
}
pub(crate) fn clean(s: &str, strict: bool) -> bool {
    clean_bytes(s.as_bytes(), strict)
}
pub(crate) fn clean_bytes(bytes: &[u8], strict: bool) -> bool {
    let vars = environment();
    let borrowed: Vec<_> = vars
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    if strict {
        oms_scrub::scan(bytes, &borrowed).is_ok_and(|hits| hits.is_empty())
    } else {
        oms_scrub::scan_secret(bytes, &borrowed).is_ok_and(|hits| hits.is_empty())
    }
}
pub(crate) fn normalize_paths(s: &str) -> String {
    let home = home();
    let s = if home.as_os_str().is_empty() {
        s.to_owned()
    } else {
        home.to_str()
            .map(|home| s.replace(home, "~"))
            .unwrap_or_else(|| s.to_owned())
    };
    regex::Regex::new(r"/(?:hom[e]|User[s])/[A-Za-z0-9._-]+")
        .unwrap()
        .replace_all(&s, "~")
        .into_owned()
}
pub(crate) fn home() -> PathBuf {
    let p = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    fs::canonicalize(&p).unwrap_or(p)
}
pub(crate) fn global() -> PathBuf {
    std::env::var_os("OH_MY_SETTING_GLOBAL_MEMORY")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".oh-my-setting/local/agent-memory.md"))
}
pub(crate) fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    }
}
pub(crate) fn git(repo: &Path, args: &[&OsStr], interrupted: &AtomicBool) -> Option<Vec<u8>> {
    if interrupted.load(Ordering::Acquire) {
        return None;
    }
    let mut child = Command::new("git")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut stream = stdout.take((MAX + 1) as u64);
        stream.read_to_end(&mut out).ok()?;
        Some(out)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None)
                if start.elapsed() < Duration::from_secs(30)
                    && !interrupted.load(Ordering::Acquire) =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()??;
    if status?.success() && out.len() <= MAX {
        Some(out)
    } else {
        None
    }
}
pub(crate) fn git_text(repo: &Path, args: &[&str], flag: &AtomicBool) -> String {
    let a: Vec<_> = args.iter().map(OsStr::new).collect();
    git(repo, &a, flag)
        .map(|v| String::from_utf8_lossy(&v).trim_end().to_owned())
        .unwrap_or_default()
}
pub(crate) fn repo_root(cwd: &Path, path: &Path, flag: &AtomicBool) -> Result<PathBuf> {
    let path = absolute(cwd, path);
    if !path.is_dir() {
        return err(format!(
            "--repo {} is not a directory",
            os::diagnostic(path.as_os_str())
        ));
    }
    let root = git(
        &path,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        flag,
    )
    .unwrap_or_default();
    let root = root.strip_suffix(b"\n").unwrap_or(&root);
    fs::canonicalize(if root.is_empty() {
        path
    } else {
        PathBuf::from(os::from_bytes(root.to_vec()))
    })
    .map_err(Into::into)
}
pub(crate) fn validate_owned(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m)
            if !m.file_type().is_file()
                || m.nlink() != 1
                || m.uid() != rustix::process::getuid().as_raw() =>
        {
            err("memory: unsafe file type, owner, or link count")
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn read(path: &Path) -> Result<Vec<u8>> {
    if path.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    validate_owned(path)?;
    let root =
        Root::open("/").map_err(|_| Error(2, "memory: cannot open filesystem root".into()))?;
    root.read_no_follow(
        path,
        ByteReadOptions {
            missing_ok: true,
            max_bytes: MAX,
        },
    )
    .map_err(|e| Error(2, format!("memory: unsafe or unreadable source ({e:?})")))
}
pub(crate) fn lock(path: &Path, flag: &AtomicBool) -> Result<oms_lock::Guard> {
    check(flag)?;
    let settings =
        oms_lock::Settings::from_env().map_err(|e| Error(e.exit_code(), e.to_string()))?;
    let lock = oms_lock::Lock::for_file(path, settings)
        .map_err(|e| Error(e.exit_code(), e.to_string()))?;
    let start = Instant::now();
    let timeout = oms_lock::Settings::from_env()
        .map_err(|e| Error(e.exit_code(), e.to_string()))?
        .timeout;
    loop {
        check(flag)?;
        match lock.try_acquire() {
            Ok(g) => return Ok(g),
            Err(oms_lock::Error::Busy) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(e) => return Err(Error(e.exit_code(), e.to_string())),
        }
    }
}
/// The caller holds the legacy file lock. Existing bytes are a preserved prefix
/// for appends. Copy-on-write publishes one complete record and fsyncs it; a
/// killed writer never exposes a half-written canonical record.
pub(crate) fn mutate(
    path: &Path,
    flag: &AtomicBool,
    transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    check(flag)?;
    validate_owned(path)?;
    let root =
        Root::open("/").map_err(|_| Error(2, "memory: cannot open filesystem root".into()))?;
    root.atomic_mutate(
        path,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX,
            },
            create_parent: true,
            max_output: MAX,
            ..Default::default()
        },
        transform,
    )
    .map(|o| o.bytes)
    .map_err(|e| match e {
        oms_safe_fs::MutationError::Transform(e) => e,
        oms_safe_fs::MutationError::Storage { phase, error } => Error(
            2,
            format!("memory: storage failed ({phase:?}, {error:?}); inspect state before retrying"),
        ),
    })
}
pub struct Store {
    pub repo: PathBuf,
    pub shared: PathBuf,
    pub pins: PathBuf,
    pub summary: PathBuf,
    pub database: PathBuf,
    pub failures: PathBuf,
    pub scope: String,
    warnings: RefCell<Vec<u8>>,
}
impl Store {
    pub(crate) fn new(repo: PathBuf, file: Option<PathBuf>, global_scope: bool) -> Self {
        let repo = file
            .as_ref()
            .and_then(|p| {
                let dir = p.parent()?;
                if dir.file_name()? == "memory" && dir.parent()?.file_name()? == ".oms" {
                    Some(dir.parent()?.parent()?.to_owned())
                } else {
                    None
                }
            })
            .unwrap_or(repo);
        let shared = file.unwrap_or_else(|| {
            if global_scope {
                global()
            } else {
                repo.join(".oms/memory/shared.md")
            }
        });
        let dir = shared.parent().unwrap_or(Path::new(".")).to_path_buf();
        let name = shared.file_name().unwrap_or_default();
        let stem = if name == "shared.md" {
            OsString::from("memory")
        } else {
            shared.file_stem().unwrap_or(name).to_owned()
        };
        let mut dbname = stem;
        dbname.push(".sqlite3");
        let failures = if dir.file_name() == Some(OsStr::new("memory"))
            && dir.parent().and_then(Path::file_name) == Some(OsStr::new(".oms"))
        {
            dir.parent().unwrap().join("failures.jsonl")
        } else {
            dir.join("failures.jsonl")
        };
        Self {
            repo,
            shared,
            pins: dir.join("pins.md"),
            summary: dir.join("summary.md"),
            database: dir.join(dbname),
            failures,
            scope: if global_scope { "global" } else { "project" }.into(),
            warnings: RefCell::new(Vec::new()),
        }
    }
    pub(crate) fn canonical_repo(&self) -> Option<&Path> {
        (self.shared.parent() == Some(self.repo.join(".oms/memory").as_path()))
            .then_some(self.repo.as_path())
    }
    pub(crate) fn logs(&self) -> Result<(Vec<Entry>, Vec<Entry>)> {
        Ok((
            records::parse(&read(&self.shared)?, "shared"),
            records::parse(&read(&self.pins)?, "pins"),
        ))
    }
    pub(crate) fn active(&self) -> Result<Vec<Entry>> {
        let (n, p) = self.logs()?;
        let states = records::states(&n, &p);
        Ok(n.into_iter()
            .filter(|e| e.kind != "retirement" && !states.contains_key(&e.event_id))
            .collect())
    }
    pub(crate) fn ignore(&self, flag: &AtomicBool) -> Result<()> {
        if self.shared.starts_with(self.repo.join(".oms")) {
            let p = self.repo.join(".oms/.gitignore");
            let _g = lock(&p, flag)?;
            validate_owned(&p)?;
            if p.exists() {
                return Ok(());
            }
            mutate(&p, flag, |old| {
                if old.is_empty() {
                    Ok(b"*\n".to_vec())
                } else {
                    Ok(old.to_vec())
                }
            })?;
        }
        Ok(())
    }
    fn initial(&self) -> String {
        format!(
            "# Shared Agent Memory\n\n- scope: {}\n- owner: oh-my-setting agent harness\n\nStable preferences, recurring workflow notes, and known pitfalls shared by Codex, Claude Code, and Antigravity.\n{}\nThis file is the human-readable source log. Provider prompts use pins.md and summary.md by default.\n\n",
            self.scope,
            "Do not store credentials, private keys, machine paths, project-private paths, or cluster details here."
        )
    }
    pub(crate) fn init(&self, flag: &AtomicBool) -> Result<()> {
        self.ignore(flag)?;
        let _g = lock(&self.shared, flag)?;
        let exists = self.shared.exists();
        mutate(&self.shared, flag, |old| {
            Ok(if !exists {
                self.initial().into_bytes()
            } else {
                old.to_vec()
            })
        })?;
        Ok(())
    }
    pub(crate) fn validate(&self, id: &str) -> Result<()> {
        let (notes, pins) = self.logs()?;
        let states = records::states(&notes, &pins);
        let count = notes
            .iter()
            .chain(&pins)
            .filter(|e| e.event_id == id && e.kind != "retirement")
            .count();
        if !records::valid_id(id) || count != 1 {
            return err("memory database: unknown or ambiguous memory ID");
        }
        if states.contains_key(id) {
            return err("memory database: memory note is already superseded, retired or redacted");
        }
        Ok(())
    }
    fn metadata(&self, kind: &str, flag: &AtomicBool) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        let stamp = now().replace(['-', ':'], "");
        m.insert("schema".into(), "1".into());
        m.insert(
            "event_id".into(),
            format!(
                "mem-{stamp}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ),
        );
        m.insert("kind".into(), kind.into());
        let task = if self.canonical_repo().is_some() {
            read(&self.repo.join(".oms/task/current.md")).unwrap_or_default()
        } else {
            Vec::new()
        };
        let task = String::from_utf8_lossy(&task);
        for (field, env, record) in [
            ("task_id", "OMS_TASK_ID", "task_id"),
            (
                "session_hash",
                "OMS_AGENT_TASK_SOURCE_SESSION",
                "source_session",
            ),
        ] {
            let value = std::env::var(env)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| {
                    task.lines()
                        .take_while(|l| !l.starts_with("## "))
                        .find_map(|l| l.strip_prefix(&format!("- {record}: ")))
                        .unwrap_or("")
                        .into()
                });
            m.insert(field.into(), value.replace(['\r', '\n'], ""));
        }
        let sha = if self.canonical_repo().is_some() {
            git_text(&self.repo, &["rev-parse", "HEAD"], flag)
        } else {
            String::new()
        };
        m.insert("git_sha".into(), sha.clone());
        let status = git_text(
            &self.repo,
            &["status", "--porcelain", "--untracked-files=normal"],
            flag,
        );
        m.insert(
            "git_dirty".into(),
            if sha.is_empty() {
                ""
            } else if status.is_empty() {
                "0"
            } else {
                "1"
            }
            .into(),
        );
        m.insert(
            "git_state".into(),
            if sha.is_empty() {
                String::new()
            } else {
                fingerprint(&self.repo, &sha, flag)
            },
        );
        m
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append(
        &self,
        agent: &str,
        note: &str,
        kind: &str,
        citation: Option<(&Path, usize)>,
        paths: &str,
        link: Option<(&str, &str)>,
        flag: &AtomicBool,
    ) -> Result<String> {
        self.append_impl(
            agent,
            note,
            kind,
            citation,
            paths,
            link,
            &[],
            None,
            false,
            flag,
        )
    }
    pub(crate) fn append_extra(
        &self,
        agent: &str,
        note: &str,
        kind: &str,
        extra: &[(&str, &str)],
        flag: &AtomicBool,
    ) -> Result<String> {
        self.append_impl(agent, note, kind, None, "", None, extra, None, false, flag)
    }
    pub(crate) fn append_held(
        &self,
        agent: &str,
        note: &str,
        kind: &str,
        flag: &AtomicBool,
    ) -> Result<String> {
        self.append_impl(agent, note, kind, None, "", None, &[], None, true, flag)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_bytes(
        &self,
        agent: &str,
        note: &[u8],
        kind: &str,
        citation: Option<(&Path, usize)>,
        paths: &str,
        link: Option<(&str, &str)>,
        flag: &AtomicBool,
    ) -> Result<String> {
        self.append_impl(
            agent,
            &String::from_utf8_lossy(note),
            kind,
            citation,
            paths,
            link,
            &[],
            Some(note),
            false,
            flag,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn append_impl(
        &self,
        agent: &str,
        note: &str,
        kind: &str,
        citation: Option<(&Path, usize)>,
        paths: &str,
        link: Option<(&str, &str)>,
        extra: &[(&str, &str)],
        raw: Option<&[u8]>,
        held: bool,
        flag: &AtomicBool,
    ) -> Result<String> {
        if !clean_bytes(raw.unwrap_or(note.as_bytes()), true) {
            return Err(Error(
                3,
                "memory note contains sensitive-looking content; not appended".into(),
            ));
        }
        if note.contains(START) {
            return Err(Error(
                3,
                "memory note contains the reserved memory metadata marker; not appended".into(),
            ));
        }
        if link.is_some() && note.trim().is_empty() {
            return err("memory text must not be empty");
        }
        let mut m = self.metadata(kind, flag);
        for (k, v) in extra {
            m.insert((*k).into(), (*v).into());
        }
        if !paths.is_empty() && kind != "retirement" {
            m.insert("paths".into(), paths.into());
        }
        let mut raw_citation = None;
        if let Some((path, line)) = citation {
            if self.canonical_repo().is_none() {
                return err("source citations require project memory under REPO/.oms/memory");
            }
            let (fields, path) = self.citation(path, line, flag)?;
            m.extend(fields);
            raw_citation = Some(path);
        }
        if let Some((target, op)) = link {
            if !records::valid_id(target) {
                return err("invalid memory ID");
            }
            m.insert(op.into(), target.into());
            m.insert("link_complete".into(), "required".into());
        }
        let id = m["event_id"].clone();
        let _g = if held {
            None
        } else {
            Some(lock(&self.shared, flag)?)
        };
        if let Some((target, _)) = link {
            self.validate(target)?;
        }
        self.ignore(flag)?;
        let mut record = Vec::new();
        if link.is_some() {
            record.push(b'\n');
        }
        record.extend(format!("{START}\n").as_bytes());
        for (key, value) in &m {
            record.extend(format!("{key}: ").as_bytes());
            if key == "source_path" {
                if let Some(path) = &raw_citation {
                    record.extend(path.as_encoded_bytes());
                } else {
                    record.extend(value.as_bytes());
                }
            } else {
                record.extend(value.as_bytes());
            }
            record.push(b'\n');
        }
        record.extend(b"-->\n");
        record.extend(format!("## {} {agent}\n\n", now()).as_bytes());
        record.extend(raw.unwrap_or(note.as_bytes()));
        if link.is_some() {
            record.extend(format!("\n{COMPLETE}\n").as_bytes());
        } else {
            record.extend(b"\n\n");
        }
        let exists = self.shared.exists();
        mutate(&self.shared, flag, |old| {
            let mut out = if !exists {
                self.initial().into_bytes()
            } else {
                old.to_vec()
            };
            out.extend_from_slice(&record);
            Ok(out)
        })?;
        drop(_g);
        if !held {
            self.after_write(flag);
        }
        Ok(id)
    }
    pub(crate) fn pin_bytes(&self, agent: &str, raw: &[u8], flag: &AtomicBool) -> Result<String> {
        if !clean_bytes(raw, true) {
            return Err(Error(
                3,
                "memory pin contains sensitive-looking content; not appended".into(),
            ));
        }
        let mut remaining = raw;
        let mut note = String::new();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(s) => {
                    note.push_str(s);
                    break;
                }
                Err(e) => {
                    note.push_str(std::str::from_utf8(&remaining[..e.valid_up_to()]).unwrap_or(""));
                    let skip = e.error_len().unwrap_or(remaining.len() - e.valid_up_to());
                    remaining = &remaining[e.valid_up_to() + skip..];
                }
            }
        }
        self.pin(agent, &note, flag)
    }
    pub(crate) fn pin(&self, agent: &str, note: &str, flag: &AtomicBool) -> Result<String> {
        if !clean(note, true) {
            return Err(Error(
                3,
                "memory pin contains sensitive-looking content; not appended".into(),
            ));
        }
        if note.contains(START) {
            return Err(Error(
                3,
                "memory pin contains the reserved memory metadata marker; not appended".into(),
            ));
        }
        let chars = std::env::var("OMS_AGENT_MEMORY_PIN_CHARS")
            .unwrap_or_else(|_| "240".into())
            .parse::<usize>()
            .map_err(|_| Error(2, "invalid pin byte limit".into()))?;
        let m = self.metadata("pin", flag);
        let mut line = flat(note);
        if note.starts_with(char::is_whitespace) {
            line.insert(0, ' ');
        }
        if !line.is_empty() && note.ends_with(char::is_whitespace) {
            line.push(' ');
        }
        let record = format!(
            "{}- {} [{agent}] {}\n",
            metadata_text(&m),
            now(),
            byte_clip(&line, chars)
        );
        self.ignore(flag)?;
        let _g = lock(&self.pins, flag)?;
        let exists = self.pins.exists();
        mutate(&self.pins, flag, |old| {
            let mut out = if !exists {
                format!("# Pinned Agent Memory\n\n- scope: {}\n\nPinned high-signal notes always eligible for provider context. Keep short.\n\n",self.scope).into_bytes()
            } else {
                old.to_vec()
            };
            out.extend_from_slice(record.as_bytes());
            Ok(out)
        })?;
        drop(_g);
        if db::sync(self, flag).is_err() {
            self.warning("warning: memory note is safe in the Markdown source, but its database index is stale");
        }
        Ok(m["event_id"].clone())
    }
    pub(crate) fn after_write(&self, flag: &AtomicBool) {
        if let Err(Error(_, message)) = self.compact(flag) {
            self.warning(&format!("warning: {message}"));
        }
        if db::sync(self, flag).is_err() {
            self.warning("warning: memory note is safe in the Markdown source, but its database index is stale");
        }
    }
    pub(crate) fn warning(&self, message: &str) {
        let mut warnings = self.warnings.borrow_mut();
        warnings.extend(message.as_bytes());
        warnings.push(b'\n');
    }
    pub(crate) fn take_warnings(&self) -> Vec<u8> {
        self.warnings.take()
    }
    pub(crate) fn with_warnings(&self, result: Result<Output>) -> Result<Output> {
        result.map(|mut out| {
            out.2.extend(self.take_warnings());
            out
        })
    }

    pub(crate) fn summary_body(&self) -> Result<String> {
        let active = self.active()?;
        let eligible = crate::review::context_filter(self)?;
        Ok(active
            .iter()
            .filter(|e| {
                eligible(e)
                    && e.field("source_path").is_empty()
                    && e.field("paths").is_empty()
                    && !e.body.trim().is_empty()
            })
            .map(|e| {
                let body = flat(e.body.lines().next().unwrap_or(""));
                let body = if body.chars().count() > 240 {
                    format!("{}...", clip(&body, 240))
                } else {
                    body
                };
                format!("- {} {}: {body}\n", e.occurred_at, e.agent)
            })
            .collect())
    }
    pub(crate) fn compact(&self, flag: &AtomicBool) -> Result<()> {
        let _g = lock(&self.summary, flag)?;
        let raw = read(&self.shared)?;
        if raw.is_empty() {
            return Ok(());
        }
        if !clean(&String::from_utf8_lossy(&raw), true) {
            return Err(Error(
                3,
                "compact memory not refreshed because source contains sensitive-looking content"
                    .into(),
            ));
        }
        let body = self.summary_body()?;
        let body = tail(&body, 40);
        if !clean(&body, true) {
            return Err(Error(
                3,
                "compact memory not refreshed because source contains sensitive-looking content"
                    .into(),
            ));
        }
        self.ignore(flag)?;
        mutate(&self.summary, flag, |_| {
            Ok(format!("# Compact Agent Memory\n\n- scope: {}\n- generated: {}\n\nRecent compact notes generated from shared.md. Keep this short for provider context.\n\n{body}",self.scope,now()).into_bytes())
        })?;
        Ok(())
    }
    fn citation(
        &self,
        path: &Path,
        line: usize,
        flag: &AtomicBool,
    ) -> Result<(BTreeMap<String, String>, OsString)> {
        if path
            .as_os_str()
            .as_bytes()
            .iter()
            .any(|b| b"\r\n".contains(b))
        {
            return err("citation paths cannot contain newlines");
        }
        let listed = git(
            &self.repo,
            &[
                OsStr::new("-c"),
                OsStr::new("core.quotePath=false"),
                OsStr::new("ls-files"),
                OsStr::new("--full-name"),
                OsStr::new("--error-unmatch"),
                OsStr::new("--"),
                path.as_os_str(),
            ],
            flag,
        )
        .unwrap_or_default();
        let listed = listed.strip_suffix(b"\n").unwrap_or(&listed);
        let listed: Vec<_> = listed.iter().copied().filter(|b| *b != b'\r').collect();
        if listed.is_empty() || listed.contains(&b'\n') {
            return err(format!(
                "citation source must be one tracked file in the current project: {}",
                os::diagnostic(path.as_os_str())
            ));
        }
        let listed = os::from_bytes(listed);
        let mut spec = OsString::from("HEAD:");
        spec.push(&listed);
        let oid = git(&self.repo, &[OsStr::new("rev-parse"), &spec], flag)
            .and_then(|s| String::from_utf8(s).ok())
            .unwrap_or_default()
            .trim_end()
            .to_owned();
        if oid.is_empty() || git_text(&self.repo, &["cat-file", "-t", &oid], flag) != "blob" {
            return err(format!(
                "citation source is not a committed file at HEAD: {}",
                os::diagnostic(&listed)
            ));
        }
        let bytes = git(&self.repo, &[OsStr::new("show"), &spec], flag)
            .ok_or_else(|| Error(2, "citation source unavailable".into()))?;
        let rows = blob_lines(&bytes);
        let row = rows.get(line.wrapping_sub(1)).ok_or_else(|| {
            Error(
                2,
                format!(
                    "citation line {line} does not exist in HEAD:{}",
                    os::diagnostic(&listed)
                ),
            )
        })?;
        Ok((
            BTreeMap::from([
                ("source_path".into(), os::diagnostic(&listed)),
                ("source_line".into(), line.to_string()),
                ("source_line_sha256".into(), hash(row)),
                ("source_blob_oid".into(), oid),
            ]),
            listed,
        ))
    }
    pub(crate) fn context(&self, full: bool, flag: &AtomicBool) -> Result<String> {
        self.context_view(full, false, flag)
    }
    pub(crate) fn context_view(
        &self,
        full: bool,
        pins_only: bool,
        flag: &AtomicBool,
    ) -> Result<String> {
        let (n, p) = self.logs()?;
        let state = records::states(&n, &p);
        let eligible = crate::review::context_filter(self)?;
        let active =
            |e: &&Entry| e.kind != "retirement" && !state.contains_key(&e.event_id) && eligible(e);
        let mut body = String::new();
        if full {
            if !clean(&String::from_utf8_lossy(&read(&self.shared)?), true) {
                self.warning(&format!("warning: shared memory omitted because it contains sensitive-looking content: {}",self.scope));
                return Ok(body);
            }
            let content: String = n
                .iter()
                .filter(active)
                .map(|e| format!("{}\n\n", e.rendered))
                .collect();
            let content = tail(&content, 120);
            if content.is_empty() {
                return Ok(body);
            }
            body.push_str(&format!("### {}\n", self.scope));
            let bytes = content.len();
            body.push_str(byte_clip(&content, 16384));
            if bytes > 16384 {
                body.push_str(&format!(
                    "\n[shared memory truncated: 16384 of {bytes} bytes shown]\n"
                ));
            }
            body.push('\n');
            return Ok(body);
        }
        if !pins_only
            && !(crate::cli::child() && self.scope == "global")
            && read(&self.summary)?.is_empty()
            && !read(&self.shared)?.is_empty()
        {
            let _ = self.compact(flag);
        }
        if clean(&String::from_utf8_lossy(&read(&self.pins)?), true) {
            let pins: String = p
                .iter()
                .filter(active)
                .map(|e| format!("{}\n", e.rendered))
                .collect();
            let pins = tail(&pins, 30);
            if !pins.is_empty() {
                body.push_str(&format!("Pinned:\n{}\n\n", pins.trim_end()));
            }
        }
        let summary = read(&self.summary)?;
        if !pins_only && !summary.is_empty() && clean(&String::from_utf8_lossy(&summary), true) {
            let recent = self.summary_body()?;
            if clean(&recent, true) && !recent.is_empty() {
                body.push_str(&format!(
                    "Compact recent:\n{}\n\n",
                    tail(&recent, 45).trim_end()
                ));
            }
        }
        Ok(if body.is_empty() {
            body
        } else {
            format!("### {}\n{body}", self.scope)
        })
    }
    pub(crate) fn rows(&self, name: &str) -> Result<Vec<Value>> {
        let p = self.shared.parent().unwrap().join(name);
        Ok(records::failure_rows(&read(&p)?)
            .into_iter()
            .filter(|r| r["schema"] == 1 && r["id"].is_string())
            .collect())
    }
}
use records::{COMPLETE, START};
pub(crate) fn metadata_text(m: &BTreeMap<String, String>) -> String {
    let mut s = format!("{START}\n");
    for (k, v) in m {
        s.push_str(&format!("{k}: {v}\n"));
    }
    s.push_str("-->\n");
    s
}
pub(crate) fn blob_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        if bytes[at] == b'\r' || bytes[at] == b'\n' {
            out.push(&bytes[start..at]);
            if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                at += 1;
            }
            start = at + 1;
        }
        at += 1;
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
}
pub(crate) fn tail(s: &str, n: usize) -> String {
    let lines: Vec<_> = s.split_inclusive('\n').collect();
    lines[lines.len().saturating_sub(n)..].concat()
}
fn fingerprint(repo: &Path, sha: &str, flag: &AtomicBool) -> String {
    let call = |args: &[&str]| {
        let a: Vec<_> = args.iter().map(OsStr::new).collect();
        git(repo, &a, flag).unwrap_or_default()
    };
    let mut h = Sha256::new();
    h.update(b"index\0");
    h.update(call(&["ls-files", "--stage", "-z"]));
    let tracked = call(&["ls-files", "-z"]);
    let mut paths: Vec<_> = tracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    for raw in paths {
        h.update(b"\0path\0");
        h.update(Sha256::digest(raw));
        let path = repo.join(OsStr::from_bytes(raw));
        let info = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                h.update(b"missing\0");
                continue;
            }
        };
        h.update(format!("mode:{:o}\0", info.mode()));
        if info.file_type().is_symlink() {
            h.update(b"link\0");
            if let Ok(link) = fs::read_link(&path) {
                h.update(link.as_os_str().as_bytes());
            }
        } else if info.is_file() {
            h.update(b"file\0");
            match fs::File::open(&path) {
                Ok(mut file) => {
                    let mut buf = [0; 65536];
                    loop {
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
            h.update(format!(
                "other:{}:{}\0",
                info.len(),
                info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128
            ));
        }
    }
    let tracked = format!("{:x}", h.finalize());
    let mut h = Sha256::new();
    let others = call(&["ls-files", "-z", "--others", "--exclude-standard"]);
    let mut paths: Vec<_> = others
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    for raw in paths {
        h.update(Sha256::digest(raw));
        match fs::symlink_metadata(repo.join(OsStr::from_bytes(raw))) {
            Ok(m) => h.update(format!(
                "M:{:o}:{}:{}",
                m.mode(),
                m.len(),
                m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128
            )),
            Err(_) => h.update(b"MISSING"),
        }
    }
    format!("{sha}:{tracked}:{:x}", h.finalize())
}
/// Revalidate citation metadata against the current committed blob, sharing
/// each blob read across this command's result pool.
pub(crate) fn validate_citation(
    e: &Entry,
    repo: &Path,
    cache: &mut BTreeMap<OsString, (Option<Vec<u8>>, String)>,
    flag: &AtomicBool,
) -> Option<Value> {
    let path = e.source_path();
    let line = e.field("source_line").parse::<usize>().ok()?;
    let expected = e.field("source_line_sha256");
    if path.is_empty() || line == 0 || expected.is_empty() {
        return None;
    }
    let mut out = json!({"status":"stale","path":os::diagnostic(path),"line":line,"source_blob_oid":e.field("source_blob_oid")});
    if repo.as_os_str().is_empty()
        || Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|p| p == std::path::Component::ParentDir)
    {
        out["reason"] = "source-unavailable".into();
        return Some(out);
    }
    let (raw, oid) = cache.entry(path.to_owned()).or_insert_with(|| {
        let mut spec = OsString::from("HEAD:");
        spec.push(path);
        let id = git(repo, &[OsStr::new("rev-parse"), &spec], flag)
            .and_then(|v| String::from_utf8(v).ok())
            .unwrap_or_default()
            .trim_end()
            .to_owned();
        (git(repo, &[OsStr::new("show"), &spec], flag), id)
    });
    let Some(raw) = raw else {
        out["reason"] = "source-missing".into();
        return Some(out);
    };
    out["current_blob_oid"] = oid.clone().into();
    let rows = blob_lines(raw);
    if rows.get(line - 1).is_some_and(|r| hash(r) == expected) {
        out["status"] = "valid".into();
        out["current_line"] = line.into();
        return Some(out);
    }
    let matches: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            if hash(r) == expected {
                Some(i + 1)
            } else {
                None
            }
        })
        .collect();
    if matches.len() == 1 {
        out["status"] = "moved".into();
        out["current_line"] = matches[0].into();
    } else {
        out["reason"] = if matches.is_empty() {
            "line-changed"
        } else {
            "line-ambiguous"
        }
        .into();
    }
    Some(out)
}

/// Unlink through a pinned no-follow parent and persist the directory entry.
/// Removing a link never follows it; non-regular derived files are refused.
pub(crate) fn remove(path: &Path) -> Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, fsync, open, openat, unlinkat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = open("/", flags, Mode::empty())
        .map_err(|_| Error(2, "memory: cannot open directory".into()))?;
    let components: Vec<_> = path.components().collect();
    for part in &components[..components.len().saturating_sub(1)] {
        match part {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                dir = match openat(&dir, *name, flags, Mode::empty()) {
                    Ok(fd) => fd,
                    Err(rustix::io::Errno::NOENT) => return Ok(()),
                    Err(_) => return err("memory: unsafe derived directory"),
                };
            }
            _ => return err("memory: unsafe derived path"),
        }
    }
    validate_owned(path)?;
    let Some(name) = path.file_name() else {
        return err("memory: invalid derived path");
    };
    match unlinkat(&dir, name, AtFlags::empty()) {
        Ok(()) => fsync(&dir).map_err(|_| Error(2, "memory: directory sync failed".into())),
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => err("memory: derived copy could not be removed"),
    }
}

/// A bounded head read for handoff pointers; content beyond the head is neither
/// allocated nor read, and non-regular or multiply-linked leaves are refused.
pub(crate) fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, open, openat};
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = open("/", directory, Mode::empty())
        .map_err(|_| Error(2, "memory: directory open failed".into()))?;
    let parts: Vec<_> = path.components().collect();
    for component in &parts[..parts.len().saturating_sub(1)] {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                dir = openat(&dir, *name, directory, Mode::empty())
                    .map_err(|_| Error(2, "memory: unsafe source directory".into()))?;
            }
            _ => return err("memory: invalid source path"),
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| Error(2, "memory: invalid source path".into()))?;
    let fd = openat(
        &dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Error(2, "memory: source open failed".into()))?;
    let info = fstat(&fd).map_err(|_| Error(2, "memory: source stat failed".into()))?;
    if FileType::from_raw_mode(info.st_mode) != FileType::RegularFile
        || info.st_nlink != 1
        || info.st_uid != rustix::process::getuid().as_raw()
    {
        return err("memory: unsafe source file");
    }
    let file = fs::File::from(fd);
    let mut bytes = Vec::new();
    file.take(limit as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}
