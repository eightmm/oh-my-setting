//! Fixed, local Git queries and exact source/input fences.
use crate::json::{self, Value, object, string};
use crate::storage::{MAX_PATCH, Store};
use crate::{Result, error, require};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Git<'a> {
    pub repo: &'a Path,
    pub interrupted: &'a AtomicBool,
    pub group_resolver: Option<crate::services::GroupResolver>,
    /// An outer transaction's absolute proof deadline. None keeps the 30s default.
    pub deadline: Option<Instant>,
    pub group_snapshot: Option<std::sync::Arc<crate::storage::GroupProofSnapshot>>,
}
pub struct GitOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
}
impl Git<'_> {
    pub fn output(
        &self,
        args: &[&OsStr],
        input: Option<&[u8]>,
        environment: &[(OsString, OsString)],
        limit: usize,
    ) -> Result<GitOutput> {
        let mut cmd = Command::new("git");
        cmd.args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .arg("-C")
        .arg(self.repo)
        .args(args);
        for (k, _) in std::env::vars_os() {
            if k.as_bytes().to_ascii_uppercase().starts_with(b"GIT_") {
                cmd.env_remove(k);
            }
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_COUNT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C");
        for (k, v) in environment {
            cmd.env(k, v);
        }
        let deadline = self
            .deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        self.output_before(cmd, input, limit, deadline)
    }
    fn proof_error(&self, message: &str) -> crate::Error {
        if self.interrupted.load(Ordering::Acquire) {
            if self.deadline.is_none() {
                return error("local Git proof interrupted or timed out");
            }
            return crate::Error {
                code: 130,
                message: "local Git proof interrupted".into(),
            };
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return crate::Error {
                code: 75,
                message: "local Git proof deadline expired".into(),
            };
        }
        error(message)
    }
    fn output_before(
        &self,
        command: Command,
        input: Option<&[u8]>,
        limit: usize,
        deadline: Instant,
    ) -> Result<GitOutput> {
        use oms_supervisor::process::{
            DuplexOptions, Progress, Reactor, Request, Scope, StopReason, Streams,
        };
        if self.interrupted.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(self.proof_error("local Git proof interrupted or timed out"));
        }
        let mut request = Request::new(
            std::iter::once(command.get_program().to_owned())
                .chain(command.get_args().map(OsStr::to_owned))
                .collect(),
        );
        // The default query contract rejects a short stdin write. Fixed stdin
        // permits prefix consumption; duplex retains the required write proof.
        let strict_input = input.filter(|_| self.deadline.is_none());
        if let Some(input) = strict_input {
            request.duplex = Some(DuplexOptions {
                max_frame_bytes: input.len().max(1),
                max_input_bytes: input.len().max(1),
                max_input_frames: 1,
                max_output_bytes: 65536,
                max_output_events: 8,
                max_read_bytes_per_turn: 65536,
                max_write_bytes_per_turn: 65536,
            });
        } else {
            request.stdin = input.map(<[u8]>::to_vec);
        }
        request.environment = command
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();
        request.remove_environment = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_owned())
            .collect();
        request.deadline = Some(deadline.min(Instant::now() + Duration::from_secs(30)));
        request.scope = Scope::ProcessGroup;
        request.streams = Streams::Separate;
        request.terminate_grace = Duration::ZERO;
        request.capture.max_bytes = limit;
        request.capture.on_limit = oms_supervisor::capture::LimitAction::Terminate;
        request.stderr_capture = Some(oms_supervisor::capture::CaptureOptions {
            max_bytes: 0,
            on_limit: oms_supervisor::capture::LimitAction::Drain,
            lines: None,
        });
        let mut reactor = Reactor::new();
        let job = reactor
            .start(request)
            .map_err(|_| self.proof_error("local Git proof unavailable"))?;
        if let Some(input) = strict_input {
            if !input.is_empty() {
                reactor
                    .send(job, input)
                    .map_err(|_| self.proof_error("Git input unavailable"))?;
            }
            reactor
                .close_input(job)
                .map_err(|_| self.proof_error("Git input unavailable"))?;
        }
        let mut cancelled = false;
        loop {
            if self.interrupted.load(Ordering::Acquire) && !cancelled {
                reactor
                    .stop(job, StopReason::Cancelled)
                    .map_err(|_| self.proof_error("local Git cancellation unavailable"))?;
                cancelled = true;
            }
            if strict_input.is_some() {
                // The bounded capture owns the result; discard incremental
                // events so its independent duplex queue cannot stall drainage.
                while reactor
                    .receive(job)
                    .map_err(|_| self.proof_error("Git capture unavailable"))?
                    .is_some()
                {}
            }
            match reactor
                .poll(job, Duration::from_millis(10))
                .map_err(|_| self.proof_error("local Git supervision unavailable"))?
            {
                Progress::Pending(_) => {}
                Progress::Complete(result) => {
                    if self.interrupted.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return Err(self.proof_error("local Git proof interrupted or timed out"));
                    }
                    require(!result.stdout.limited, "Git output exceeds bound")?;
                    if let Some(input) = strict_input {
                        require(
                            result.duplex.as_ref().is_some_and(|duplex| {
                                duplex.status.input_drained
                                    && !duplex.status.input_broken_pipe
                                    && duplex.status.input_written_bytes == input.len() as u64
                            }),
                            "Git input unavailable",
                        )?;
                    }
                    require(
                        result.stop.is_none()
                            && result.stdout.eof
                            && !result.stdout.cutoff
                            && result.cleanup_complete()
                            && result.supervision_error.is_none(),
                        "local Git proof incomplete",
                    )?;
                    return Ok(GitOutput {
                        code: result.exit.map_or(128, |exit| {
                            if self.deadline.is_some() {
                                exit.shell_code()
                            } else {
                                exit.code.unwrap_or(128)
                            }
                        }),
                        stdout: result.stdout.bytes,
                    });
                }
            }
        }
    }
    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.raw(&args.iter().map(OsStr::new).collect::<Vec<_>>())
    }
    fn bounded(&self, args: &[&str], limit: usize) -> Result<Vec<u8>> {
        let result = self.output(
            &args.iter().map(OsStr::new).collect::<Vec<_>>(),
            None,
            &[],
            limit,
        )?;
        require(result.code == 0, "local Git proof unavailable")?;
        Ok(result.stdout)
    }
    pub fn raw(&self, args: &[&OsStr]) -> Result<Vec<u8>> {
        let r = self.output(args, None, &[], 4 * 1024 * 1024)?;
        require(r.code == 0, "local Git proof unavailable")?;
        Ok(r.stdout)
    }
    pub fn text(&self, args: &[&str]) -> Result<String> {
        String::from_utf8(self.run(args)?)
            .map(|s| s.trim().replace('\r', ""))
            .map_err(|_| error("Git identity is not UTF-8"))
    }
    pub fn ancestor(&self, base: &str, head: &str) -> Result<()> {
        self.run(&["merge-base", "--is-ancestor", base, head])?;
        Ok(())
    }
    pub fn namespace_path(&self) -> Result<PathBuf> {
        let raw = self.run(&["rev-parse", "--git-common-dir"])?;
        let path = PathBuf::from(OsString::from_vec(trim(&raw).to_vec()));
        std::fs::canonicalize(self.repo.join(path))
            .map_err(|_| error("Git common directory unavailable"))
    }
    pub fn namespace(&self) -> Result<String> {
        Ok(json::sha(self.namespace_path()?.as_os_str().as_bytes()))
    }
    pub fn snapshot(&self) -> Result<Value> {
        let head = self.text(&["rev-parse", "HEAD"])?;
        require(commit(&head), "full local HEAD unavailable")?;
        let symbolic = self.output(
            &[
                OsStr::new("symbolic-ref"),
                OsStr::new("-q"),
                OsStr::new("HEAD"),
            ],
            None,
            &[],
            4096,
        )?;
        let reference = match symbolic.code {
            0 => String::from_utf8(trim(&symbolic.stdout).to_vec())
                .map_err(|_| error("Git ref is not UTF-8"))?,
            1 => format!("DETACHED@{head}"),
            _ => return Err(error("cannot resolve repository symbolic ref")),
        };
        let dirty = self.run(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])?;
        Ok(object([
            ("head", string(head)),
            ("git_ref", string(reference)),
            ("clean", Value::Bool(dirty.is_empty())),
        ]))
    }
    pub fn guard(&self) -> Result<()> {
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_CONFIG_PARAMETERS",
        ] {
            require(
                std::env::var_os(name).is_none_or(|v| v.is_empty()),
                "Git redirect or command configuration is active",
            )?;
        }
        let count = std::env::var("GIT_CONFIG_COUNT").unwrap_or_else(|_| "0".into());
        let n = count
            .parse::<usize>()
            .map_err(|_| error("invalid GIT_CONFIG_COUNT"))?;
        require(
            n <= 64 && count == n.to_string(),
            "invalid GIT_CONFIG_COUNT",
        )?;
        for i in 0..n {
            let k = std::env::var(format!("GIT_CONFIG_KEY_{i}"))
                .map_err(|_| error("incomplete command-scope Git config"))?;
            let v = std::env::var(format!("GIT_CONFIG_VALUE_{i}"))
                .map_err(|_| error("incomplete command-scope Git config"))?;
            require(
                k.eq_ignore_ascii_case("core.hooksPath") && v == "/dev/null",
                "unsafe command-scope Git config",
            )?;
        }
        for (flag, name, missing) in [
            ("--git-common-dir", "config", false),
            ("--git-dir", "config.worktree", true),
        ] {
            let directory = self.run(&["rev-parse", flag])?;
            let path = std::fs::canonicalize(
                self.repo
                    .join(OsString::from_vec(trim(&directory).to_vec())),
            )
            .map_err(|_| error("Git config directory unavailable"))?;
            let root = Store::open(&path)?
                .with_group_resolver(self.group_resolver)
                .with_group_snapshot(self.group_snapshot.clone());
            let raw = if missing {
                root.optional(name, 1024 * 1024)?
            } else {
                Some(root.read(name, 1024 * 1024)?)
            };
            let Some(raw) = raw else {
                continue;
            };
            require(
                !raw.contains(&0) && std::str::from_utf8(&raw).is_ok(),
                "invalid Git config encoding",
            )?;
            let result = self.output(
                &[
                    OsStr::new("config"),
                    OsStr::new("--no-includes"),
                    OsStr::new("--file"),
                    OsStr::new("-"),
                    OsStr::new("--null"),
                    OsStr::new("--name-only"),
                    OsStr::new("--list"),
                ],
                Some(&raw),
                &[(OsString::from("GIT_DIR"), OsString::from("/dev/null"))],
                1024 * 1024,
            )?;
            require(
                result.code == 0 && (result.stdout.is_empty() || result.stdout.ends_with(&[0])),
                "Git config inspection failed",
            )?;
            for key in result.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
                let key = key.to_ascii_lowercase();
                let fields = key.split(|b| *b == b'.').collect::<Vec<_>>();
                let section = fields[0];
                let option = *fields.last().unwrap();
                require(
                    !([b"include".as_slice(), b"includeif", b"hook"].contains(&section)
                        || key == b"core.fsmonitor"
                        || section == b"filter"
                            && [b"clean".as_slice(), b"smudge", b"process"].contains(&option)
                        || section == b"diff"
                            && [b"command".as_slice(), b"external", b"textconv"].contains(&option)),
                    "unsafe Git execution config",
                )?;
            }
        }
        let flags = self.run(&["ls-files", "-v", "-z"])?;
        require(
            flags
                .split(|b| *b == 0)
                .filter(|r| !r.is_empty())
                .all(|r| r[0] != b'S' && !r[0].is_ascii_lowercase()),
            "hidden Git index flags are active",
        )
    }
    pub fn acceptance_snapshot(&self) -> Result<String> {
        use std::os::unix::fs::MetadataExt;
        let mut hash = Sha256::new();
        let mut total = 0usize;
        let limit: usize = 64 * 1024 * 1024;
        let head = self.run(&["rev-parse", "HEAD"])?;
        let symbolic = self.output(
            &[
                OsStr::new("symbolic-ref"),
                OsStr::new("-q"),
                OsStr::new("HEAD"),
            ],
            None,
            &[],
            4096,
        )?;
        let reference = match symbolic.code {
            0 => trim(&symbolic.stdout).to_vec(),
            1 => [b"DETACHED@".as_slice(), trim(&head)].concat(),
            _ => return Err(error("Git symbolic ref unavailable")),
        };
        for bytes in [&reference, &head] {
            total += bytes.len();
            hash.update(bytes);
            hash.update([0]);
        }
        for args in [
            vec!["ls-files", "--stage", "-z"],
            vec![
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "HEAD",
                "--",
            ],
        ] {
            let raw = self.bounded(&args, limit.saturating_sub(total))?;
            total += raw.len();
            require(total <= limit, "acceptance snapshot exceeds 64 MiB")?;
            hash.update(raw);
            hash.update([0]);
        }
        let tracked = self.bounded(&["ls-files", "-z"], limit.saturating_sub(total))?;
        total += tracked.len();
        hash.update(&tracked);
        hash.update([0]);
        let mut names = tracked
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        names.sort();
        for name in names {
            let mode = match std::fs::symlink_metadata(self.repo.join(OsStr::from_bytes(name))) {
                Ok(m) => m.mode(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(_) => return Err(error("tracked mode unavailable")),
            };
            hash.update(name);
            hash.update(b"\0mode\0");
            hash.update(mode.to_string());
            hash.update([0]);
        }
        let untracked = self.bounded(
            &["ls-files", "--others", "--exclude-standard", "-z"],
            limit.saturating_sub(total),
        )?;
        total += untracked.len();
        hash.update(&untracked);
        hash.update([0]);
        let store = Store::open(self.repo)?
            .with_group_resolver(self.group_resolver)
            .with_group_snapshot(self.group_snapshot.clone());
        let mut names = untracked
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        names.sort();
        for name in names {
            let rel = Path::new(OsStr::from_bytes(name));
            let path = self.repo.join(rel);
            let m = std::fs::symlink_metadata(&path)
                .map_err(|_| error("untracked input unavailable"))?;
            hash.update(name);
            hash.update([0]);
            hash.update(m.mode().to_string());
            hash.update([0]);
            let raw = if m.file_type().is_symlink() {
                let real = std::fs::canonicalize(&path)
                    .map_err(|_| error("untracked symlink unavailable"))?;
                require(
                    real.starts_with(self.repo),
                    "untracked symlink escapes repository",
                )?;
                std::fs::read_link(&path)
                    .map_err(|_| error("untracked symlink unavailable"))?
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            } else {
                store.read(rel, limit.saturating_sub(total))?
            };
            total += raw.len();
            require(total <= limit, "acceptance snapshot exceeds 64 MiB")?;
            hash.update(raw);
        }
        require(total <= limit, "acceptance snapshot exceeds 64 MiB")?;
        Ok(format!("{:x}", hash.finalize()))
    }
}
pub fn trim(raw: &[u8]) -> &[u8] {
    let start = raw
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |n| n + 1);
    &raw[start..end]
}
pub fn commit(value: &str) -> bool {
    json::hex(value, 40) || json::hex(value, 64)
}
pub fn source_mode(mode: u32) -> &'static str {
    if mode & 0o100 != 0 {
        "100755"
    } else {
        "100644"
    }
}

/// An ephemeral index and object directory; product proof never writes the
/// repository index or object namespace. All names are removed by Drop.
pub struct ProductIndex {
    directory: PathBuf,
    environment: Vec<(OsString, OsString)>,
}
impl ProductIndex {
    pub fn new(git: &Git<'_>) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let parent = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let directory = parent.join(format!(
            "oms-plan-product-{}",
            crate::storage::random_hex(16)?
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| error("cannot create private product index"))?;
        let result = (|| {
            let objects = directory.join("objects");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&objects)
                .map_err(|_| error("cannot create private product objects"))?;
            let common = git.namespace_path()?.join("objects");
            let mut alternates = Vec::new();
            alternates.extend(json::canonical(&json::os(common.as_os_str()))?);
            Ok(Self {
                directory: directory.clone(),
                environment: vec![
                    (
                        OsString::from("GIT_INDEX_FILE"),
                        directory.join("index").into_os_string(),
                    ),
                    (
                        OsString::from("GIT_OBJECT_DIRECTORY"),
                        objects.into_os_string(),
                    ),
                    (
                        OsString::from("GIT_ALTERNATE_OBJECT_DIRECTORIES"),
                        OsString::from_vec(alternates),
                    ),
                ],
            })
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(directory);
        }
        result
    }
    pub fn run(&self, git: &Git<'_>, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let r = git.output(
            &args.iter().map(OsStr::new).collect::<Vec<_>>(),
            input,
            &self.environment,
            MAX_PATCH,
        )?;
        require(r.code == 0, "cannot prove actual successor product")?;
        Ok(r.stdout)
    }
}
impl Drop for ProductIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
