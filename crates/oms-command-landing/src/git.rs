use crate::{Error, Result, check, require};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Execution {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed: Duration,
}
pub(crate) fn process(
    cwd: &Path,
    argv: Vec<OsString>,
    input: Option<&[u8]>,
    timeout: Duration,
    interrupted: &AtomicBool,
    environment: &[(OsString, OsString)],
) -> Result<Execution> {
    check(interrupted)?;
    let mut reactor = Reactor::new();
    let mut request = Request::new(argv);
    request.cwd = Some(cwd.to_owned());
    request.stdin = Some(input.unwrap_or_default().to_vec());
    request.streams = Streams::Separate;
    request.scope = Scope::ProcessGroup;
    request.capture.max_bytes = 64 * 1024 * 1024;
    request.capture.on_limit = oms_supervisor::capture::LimitAction::Terminate;
    request.stderr_capture = Some(request.capture.clone());
    request.deadline = Some(Instant::now() + timeout);
    request.environment = vec![
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_COUNT".into(), "0".into()),
        ("GIT_NO_LAZY_FETCH".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_PAGER".into(), "cat".into()),
        ("LC_ALL".into(), "C".into()),
    ];
    request.environment.extend_from_slice(environment);
    request.remove_environment = std::env::vars_os()
        .filter_map(|(key, _)| {
            (key.as_bytes().to_ascii_uppercase().starts_with(b"GIT_")
                && !request.environment.iter().any(|(fixed, _)| fixed == &key))
            .then_some(key)
        })
        .collect();
    let id = reactor
        .start(request)
        .map_err(|e| Error::new(2, e.to_string()))?;
    let mut stopped = false;
    loop {
        if interrupted.load(std::sync::atomic::Ordering::Relaxed) && !stopped {
            reactor.stop(id, StopReason::Cancelled)?;
            stopped = true;
        }
        match reactor.poll(id, Duration::from_millis(25))? {
            Progress::Pending(_) => {}
            Progress::Complete(done) => {
                require(
                    done.cleanup_complete() && done.output_complete(),
                    "command cleanup or output is incomplete",
                )?;
                let code = match done.stop {
                    Some(StopReason::Cancelled) => 130,
                    Some(StopReason::Deadline) => 124,
                    Some(StopReason::OutputLimit) => 125,
                    Some(_) => 2,
                    None => done.exit.map_or(2, |v| v.shell_code()),
                };
                return Ok(Execution {
                    code,
                    stdout: done.stdout.bytes,
                    stderr: done.stderr.map_or_else(Vec::new, |v| v.bytes),
                    elapsed: done.duration,
                });
            }
        }
    }
}
pub(crate) struct Git<'a> {
    pub repo: &'a Path,
    pub interrupted: &'a AtomicBool,
}
impl<'a> Git<'a> {
    pub fn new(repo: &'a Path, interrupted: &'a AtomicBool) -> Self {
        Self { repo, interrupted }
    }
    pub fn root(path: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
        let physical = std::fs::canonicalize(path)?;
        let git = Git {
            repo: &physical,
            interrupted,
        };
        std::fs::canonicalize(git.text(&["rev-parse", "--show-toplevel"])?)
            .map_err(|_| Error::new(2, "not a Git repository"))
    }
    pub fn output(
        &self,
        args: &[&OsStr],
        input: Option<&[u8]>,
        environment: &[(OsString, OsString)],
    ) -> Result<Execution> {
        let mut argv: Vec<OsString> = [
            "git",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "color.ui=false",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        argv.extend(args.iter().map(|s| s.to_os_string()));
        process(
            self.repo,
            argv,
            input,
            Duration::from_secs(120),
            self.interrupted,
            environment,
        )
    }
    pub fn raw(&self, args: &[&OsStr]) -> Result<Vec<u8>> {
        let out = self.output(args, None, &[])?;
        if out.code != 0 {
            return Err(Error::new(
                out.code,
                format!("Git command failed: {}", diagnostic(&out.stderr)),
            ));
        }
        Ok(out.stdout)
    }
    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.raw(&args.iter().map(OsStr::new).collect::<Vec<_>>())
    }
    pub fn text(&self, args: &[&str]) -> Result<String> {
        String::from_utf8(self.run(args)?)
            .map(|s| s.trim_end_matches(['\n', '\r']).into())
            .map_err(|_| Error::new(2, "Git returned non-UTF-8 metadata"))
    }
    pub fn succeeds(&self, args: &[&str]) -> Result<bool> {
        Ok(self
            .output(&args.iter().map(OsStr::new).collect::<Vec<_>>(), None, &[])?
            .code
            == 0)
    }
    pub fn commit(&self, name: &str) -> Result<String> {
        require(
            !name.is_empty() && !name.starts_with('-'),
            "revision must name a commit",
        )?;
        self.text(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{name}^{{commit}}"),
        ])
    }
    pub fn clean(&self) -> Result<()> {
        require(
            self.run(&["status", "--porcelain=v1", "--untracked-files=all"])?
                .is_empty(),
            "dirty worktree; operation refused",
        )
    }
    pub fn tracked_clean(&self) -> Result<()> {
        require(
            self.run(&[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "HEAD",
                "--",
            ])?
            .is_empty(),
            "tracked worktree or index changed; operation refused",
        )
    }
    pub fn safe(&self) -> Result<()> {
        oms_command_plan::repository::Git {
            group_snapshot: None,
            deadline: None,
            repo: self.repo,
            interrupted: self.interrupted,
            group_resolver: crate::storage::current_group_resolver(),
        }
        .guard()?;
        let flags = self.text(&["ls-files", "-v"])?;
        require(
            !flags.lines().any(|l| {
                l.as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_lowercase() || *b == b'S')
            }),
            "Git index has hidden entries",
        )?;
        require(
            self.run(&["ls-files", "--unmerged", "-z"])?.is_empty(),
            "unmerged index entries are not supported",
        )?;
        let debug = self.run(&["ls-files", "--debug", "-z"])?;
        for line in debug.split(|b| *b == b'\n') {
            if let Some(i) = line.windows(7).position(|s| s == b"flags: ") {
                let raw = std::str::from_utf8(&line[i + 7..])
                    .map_err(|_| Error::new(2, "unreadable index flags"))?;
                let flags = u64::from_str_radix(raw.trim(), 16)
                    .map_err(|_| Error::new(2, "unreadable index flags"))?;
                require(
                    flags & 0x20000000 == 0,
                    "intent-to-add entries are not supported",
                )?;
            }
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        let head = self.commit("HEAD")?;
        let tree = self.text(&["write-tree"])?;
        let index_path = PathBuf::from(self.text(&["rev-parse", "--git-path", "index"])?);
        let index_path = if index_path.is_absolute() {
            index_path
        } else {
            self.repo.join(index_path)
        };
        let mut tracked = self.run(&["ls-files", "--stage", "-z"])?;
        let paths = self.run(&["ls-files", "-z"])?;
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        for raw in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            check(self.interrupted)?;
            let path = Path::new(OsStr::from_bytes(raw));
            let full = self.repo.join(path);
            tracked.extend_from_slice(raw);
            tracked.push(0);
            match std::fs::symlink_metadata(&full) {
                Ok(meta) if meta.is_file() => {
                    tracked.extend_from_slice(b"file\0");
                    tracked.extend_from_slice(&(meta.mode() & 0o777).to_be_bytes());
                    let bytes = crate::storage::read(self.repo, path, false)?;
                    tracked.extend_from_slice(crate::digest(&bytes).as_bytes());
                    let after = std::fs::symlink_metadata(&full)?;
                    require(
                        oms_reference::storage::generation(&meta, &after),
                        "tracked file changed while fingerprinting",
                    )?;
                }
                Ok(meta) if meta.file_type().is_symlink() => {
                    tracked.extend_from_slice(b"link\0");
                    tracked.extend_from_slice(std::fs::read_link(&full)?.as_os_str().as_bytes());
                }
                Ok(meta) if meta.is_dir() => {
                    return Err(Error::new(
                        2,
                        "submodule tracked-state proof requires the native product inspector",
                    ));
                }
                Ok(_) => return Err(Error::new(2, "unsupported tracked file type")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracked.extend_from_slice(b"missing")
                }
                Err(e) => return Err(e.into()),
            }
            tracked.push(0);
        }
        let work = self.run(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            "HEAD",
            "--",
        ])?;
        // Git diff can refresh index stat entries, so capture its bytes last.
        let index = crate::storage::read_external(&index_path, false)?;
        Ok(Snapshot {
            head,
            tree,
            index_sha256: crate::digest(&index),
            tracked_sha256: crate::digest(&tracked),
            worktree_sha256: crate::digest(&work),
        })
    }
    pub fn transport_safe(&self) -> Result<()> {
        self.safe()?;
        for kind in ["--local", "--worktree"] {
            let output = self.output(
                &[
                    "config".as_ref(),
                    kind.as_ref(),
                    "--no-includes".as_ref(),
                    "--null".as_ref(),
                    "--name-only".as_ref(),
                    "--list".as_ref(),
                ],
                None,
                &[],
            )?;
            if output.code != 0 && kind == "--worktree" {
                continue;
            }
            require(
                output.code == 0,
                "Git transport configuration cannot be read",
            )?;
            for key in output.stdout.split(|b| *b == 0).filter(|k| !k.is_empty()) {
                let key = String::from_utf8_lossy(key).to_ascii_lowercase();
                let unsafe_key = [
                    "core.sshcommand",
                    "core.gitproxy",
                    "core.alternaterefscommand",
                ]
                .contains(&key.as_str())
                    || key.starts_with("url.")
                    || key.starts_with(concat!("creden", "tial."))
                    || key.starts_with("remote.")
                        && [".proxy", ".uploadpack", ".receivepack", ".vcs"]
                            .iter()
                            .any(|suffix| key.ends_with(suffix))
                    || key.starts_with("http.")
                        && [".extraheader", ".proxy", ".cookiefile"]
                            .iter()
                            .any(|suffix| key.ends_with(suffix));
                require(
                    !unsafe_key,
                    "unsafe executable or redirecting Git transport configuration",
                )?;
            }
        }
        Ok(())
    }
    pub fn apply(
        &self,
        patch: &[u8],
        check_only: bool,
        reverse: bool,
        indexed: bool,
    ) -> Result<()> {
        let mut args = vec![
            OsStr::new("apply"),
            OsStr::new("--binary"),
            OsStr::new("--whitespace=nowarn"),
        ];
        if check_only {
            args.push(OsStr::new("--check"));
        }
        if reverse {
            args.push(OsStr::new("--reverse"));
        }
        if indexed {
            args.push(OsStr::new("--index"));
        }
        args.push(OsStr::new("-"));
        let out = self.output(&args, Some(patch), &[])?;
        if out.code != 0 {
            Err(Error::new(
                out.code,
                format!("patch does not apply cleanly: {}", diagnostic(&out.stderr)),
            ))
        } else {
            Ok(())
        }
    }
}
fn diagnostic(raw: &[u8]) -> String {
    if crate::safe(raw).is_ok() {
        String::from_utf8_lossy(raw).trim().to_owned()
    } else {
        String::from("diagnostics withheld by sensitive-content scanner")
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub head: String,
    pub tree: String,
    pub index_sha256: String,
    pub tracked_sha256: String,
    pub worktree_sha256: String,
}
impl Snapshot {
    pub fn value(&self) -> crate::V {
        crate::obj([
            ("head", self.head.clone().into()),
            ("tree", self.tree.clone().into()),
            ("index_sha256", self.index_sha256.clone().into()),
            ("tracked_sha256", self.tracked_sha256.clone().into()),
            ("worktree_sha256", self.worktree_sha256.clone().into()),
        ])
    }
}
