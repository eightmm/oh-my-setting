//! Read-only pairwise preview. Merge objects live only in private temporary
//! storage; pairwise cleanliness never proves a combined landing is safe.
use crate::{Error, Result, require, value};
use oms_command_plan::repository::Git;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(value::random("oms-conflicts-")?);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| Error::new(2, "cannot create private merge object directory"))?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn merge(git: &Git<'_>, left: &str, right: &str) -> Result<Value> {
    let scratch = Scratch::new()?;
    let objects = git.namespace_path()?.join("objects");
    let quoted =
        oms_command_plan::json::canonical(&oms_command_plan::json::os(objects.as_os_str()))?;
    use std::os::unix::ffi::OsStringExt;
    let env = [
        (
            "GIT_OBJECT_DIRECTORY".into(),
            scratch.0.as_os_str().to_owned(),
        ),
        (
            "GIT_ALTERNATE_OBJECT_DIRECTORIES".into(),
            OsString::from_vec(quoted),
        ),
        ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
    ];
    let output = git.output(
        &[
            "merge-tree".as_ref(),
            "--write-tree".as_ref(),
            "--name-only".as_ref(),
            "--no-messages".as_ref(),
            "-z".as_ref(),
            left.as_ref(),
            right.as_ref(),
        ],
        None,
        &env,
        1024 * 1024,
    )?;
    let fields = output.stdout.split(|b| *b == 0).collect::<Vec<_>>();
    let valid = fields
        .first()
        .and_then(|s| std::str::from_utf8(s).ok())
        .is_some_and(value::commit);
    if !valid || ![0, 1].contains(&output.code) {
        return Ok(
            json!({"state": "error", "files": [], "reason": format!("git merge-tree failed (exit {})", output.code)}),
        );
    }
    let files = fields
        .iter()
        .skip(1)
        .filter(|s| !s.is_empty())
        .map(|s| oms_contract_markdown::bounded_line(&String::from_utf8_lossy(s), 200))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(200)
        .collect::<Vec<_>>();
    Ok(
        json!({"state": if output.code == 0 { "clean" } else { "conflicts" }, "files": files, "reason": ""}),
    )
}
pub fn pending(
    repo: &Path,
    base: &str,
    branches: &[String],
    limit: usize,
    interrupted: &AtomicBool,
) -> Result<Value> {
    require(
        (2..=24).contains(&limit),
        "pending conflict limit must be 2..24",
    )?;
    let repo =
        std::fs::canonicalize(repo).map_err(|_| Error::new(2, "cannot resolve repository"))?;
    let git = Git {
        repo: &repo,
        interrupted,
        group_resolver: None,
    };
    git.text(&["rev-parse", "--show-toplevel"])?;
    let mut doc = json!({"schema": 1, "mode": "pending-conflicts", "status": "ok", "readonly": true, "limit": {"max": limit, "hard_max": 24}});
    let version = git.text(&["--version"])?;
    let old = regex::Regex::new(r"([0-9]+)\.([0-9]+)")
        .unwrap()
        .captures(&version)
        .and_then(|c| Some((c[1].parse::<u64>().ok()?, c[2].parse::<u64>().ok()?)))
        .is_none_or(|v| v < (2, 38));
    if old {
        doc["status"] = "unavailable".into();
        doc["reason"] = "git-too-old".into();
        return Ok(doc);
    }
    let drivers = git.output(
        &[
            "config".as_ref(),
            "--no-includes".as_ref(),
            "--name-only".as_ref(),
            "--get-regexp".as_ref(),
            r"^merge\..*\.driver$".as_ref(),
        ],
        None,
        &[],
        65536,
    )?;
    if git.guard().is_err() || drivers.code != 1 {
        doc["status"] = "unavailable".into();
        doc["reason"] = "unsafe-config".into();
        return Ok(doc);
    }
    require(
        base.is_empty() || !base.starts_with('-'),
        "base cannot start with '-' ",
    )?;
    let defaults = ["origin/main", "origin/master", "main", "master"];
    let choices = if base.is_empty() {
        defaults.to_vec()
    } else {
        vec![base]
    };
    let (base_name, base_sha) = choices
        .iter()
        .find_map(|name| {
            git.text(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ])
            .ok()
            .filter(|sha| value::commit(sha))
            .map(|sha| ((*name).to_owned(), sha))
        })
        .ok_or_else(|| Error::new(2, "no base found; provide a commit ref"))?;
    doc["base"] = json!({"ref": base_name, "sha": base_sha});
    let mut found = vec![];
    let refs = if branches.is_empty() {
        vec!["refs/heads/oms/".to_owned()]
    } else {
        branches
            .iter()
            .map(|name| {
                require(
                    !name.starts_with('-') && !name.contains('\0'),
                    "invalid local branch name",
                )?;
                Ok(format!("refs/heads/{name}"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    for reference in refs {
        let output = git.text(&[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)%00%(committerdate:unix)",
            &reference,
        ])?;
        for line in output.lines() {
            let fields = line.split('\0').collect::<Vec<_>>();
            if fields.len() != 3 || !value::commit(fields[1]) {
                continue;
            }
            if !branches.is_empty() && !branches.iter().any(|name| name == fields[0]) {
                continue;
            }
            let when = fields[2].parse::<i64>().unwrap_or(0);
            found.push((fields[0].to_owned(), fields[1].to_owned(), when));
        }
    }
    if !branches.is_empty() {
        for branch in branches {
            require(
                found.iter().any(|(name, _, _)| name == branch),
                "a requested local branch does not exist",
            )?;
        }
    }
    found.sort();
    found.dedup();
    let mut pending = vec![];
    let mut skipped = vec![];
    for (name, sha, committed) in found {
        match git
            .text(&["rev-list", "--count", &format!("{base_sha}..{sha}")])
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            Some(ahead) if ahead > 0 => pending
                .push(json!({"name": name, "sha": sha, "ahead": ahead, "committed": committed})),
            Some(_) => skipped.push(json!({"name": name, "reason": "already in the base"})),
            None => skipped.push(json!({"name": name, "reason": "cannot compare with the base"})),
        }
    }
    pending.sort_by(|a, b| {
        b["committed"]
            .as_i64()
            .cmp(&a["committed"].as_i64())
            .then(value::text(a, "name").cmp(value::text(b, "name")))
    });
    let considered = pending.len();
    for row in pending.iter().skip(limit) {
        skipped
            .push(json!({"name": row["name"], "reason": "over the limit (oldest first dropped)"}));
    }
    pending.truncate(limit);
    pending.sort_by(|a, b| {
        a["committed"]
            .as_i64()
            .cmp(&b["committed"].as_i64())
            .then(value::text(a, "name").cmp(value::text(b, "name")))
    });
    let mut base_states = BTreeMap::new();
    for row in &mut pending {
        row["vs_base"] = merge(&git, &base_sha, value::text(row, "sha"))?;
        base_states.insert(
            value::text(row, "name").to_owned(),
            value::text(&row["vs_base"], "state").to_owned(),
        );
    }
    let mut pairs = vec![];
    let mut states = BTreeMap::new();
    for (index, left) in pending.iter().enumerate() {
        for right in &pending[index + 1..] {
            let result = merge(&git, value::text(left, "sha"), value::text(right, "sha"))?;
            let a = value::text(left, "name");
            let b = value::text(right, "name");
            let state = value::text(&result, "state").to_owned();
            states.insert((a.to_owned(), b.to_owned()), state.clone());
            states.insert((b.to_owned(), a.to_owned()), state);
            let mut result = result;
            result["a"] = a.into();
            result["b"] = b.into();
            pairs.push(result);
        }
    }
    let mut remaining = pending
        .iter()
        .map(|r| value::text(r, "name").to_owned())
        .collect::<Vec<_>>();
    let mut order = vec![];
    while !remaining.is_empty() {
        let best = remaining
            .iter()
            .min_by_key(|name| {
                let conflicts = remaining
                    .iter()
                    .filter(|other| {
                        *other != *name
                            && states
                                .get(&((*name).clone(), (*other).clone()))
                                .is_none_or(|s| s != "clean")
                    })
                    .count();
                let committed = pending
                    .iter()
                    .find(|r| value::text(r, "name") == name.as_str())
                    .and_then(|r| r["committed"].as_i64())
                    .unwrap_or(0);
                (
                    base_states.get(*name).is_none_or(|s| s != "clean"),
                    conflicts,
                    committed,
                    (*name).clone(),
                )
            })
            .unwrap()
            .clone();
        remaining.retain(|name| name != &best);
        order.push(best);
    }
    doc["limit"] = json!({"max": limit, "hard_max": 24, "considered": considered, "checked": pending.len(), "truncated": considered > pending.len()});
    doc["branches"] = pending.into();
    doc["skipped"] = skipped.into();
    doc["pairs"] = pairs.into();
    doc["suggested_order"] =
        json!({"label": "suggestion", "basis": "pairwise results only", "branches": order});
    Ok(doc)
}

/// Result of re-verifying candidate commits merged together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CombinedOutcome {
    Verified {
        combined_sha: String,
        tree_sha: String,
    },
    MergeConflict {
        commit: String,
        stderr: String,
    },
    VerificationFailed {
        commit: String,
        code: i32,
        stdout: String,
        stderr: String,
    },
    TargetDrifted {
        expected_base_sha: String,
        actual_base_sha: String,
    },
}

const COMBINED_WALL: Duration = Duration::from_secs(600);
const COMBINED_OUTPUT: usize = 1024 * 1024;
const COMBINED_TEXT: usize = 16 * 1024;

struct Raw {
    code: i32,
    stdout: String,
    stderr: String,
}
fn tail(raw: &[u8]) -> String {
    let at = raw.len().saturating_sub(COMBINED_TEXT);
    String::from_utf8_lossy(&raw[at..]).into_owned()
}
/// Isolated Git: ambient `GIT_*` state and user/system configuration never
/// reach the merge, and hooks cannot run during checkout or merge.
fn git_raw(dir: &Path, args: &[&str]) -> Result<Raw> {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "--no-pager",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "user.name=oms",
        "-c",
        "user.email=oms@localhost",
        "-C",
    ])
    .arg(dir)
    .args(args);
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| Error::new(2, "local Git unavailable"))?;
    Ok(Raw {
        code: out.status.code().unwrap_or(128),
        stdout: tail(&out.stdout),
        stderr: tail(&out.stderr),
    })
}
fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_raw(dir, args)?;
    require(
        out.code == 0,
        format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            out.stderr.trim()
        ),
    )?;
    Ok(out.stdout.trim().to_owned())
}
fn resolve(repo: &Path, reference: &str) -> Result<String> {
    let sha = git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .map_err(|_| Error::new(2, "cannot resolve commit reference"))?;
    require(value::commit(&sha), "reference did not resolve to a commit")?;
    Ok(sha)
}
/// Private detached worktree: removed with its metadata on every exit path and
/// never the primary checkout.
struct Worktree {
    repo: PathBuf,
    root: PathBuf,
    path: PathBuf,
    added: bool,
}
impl Worktree {
    fn new(repo: &Path, base: &str) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let tmp = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = tmp.join(value::random("oms-combined-")?);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|_| Error::new(2, "cannot create private combined-verification directory"))?;
        let mut guard = Self {
            repo: repo.to_owned(),
            path: root.join("tree"),
            root,
            added: false,
        };
        let path = guard.path.to_string_lossy().into_owned();
        guard.added = true;
        git_ok(repo, &["worktree", "add", "--detach", &path, base])?;
        Ok(guard)
    }
}
impl Drop for Worktree {
    fn drop(&mut self) {
        if self.added {
            let path = self.path.to_string_lossy().into_owned();
            let _ = git_raw(&self.repo, &["worktree", "remove", "--force", &path]);
            let _ = git_raw(&self.repo, &["worktree", "prune"]);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn run_verify(argv: &[OsString], dir: &Path, interrupted: &AtomicBool) -> Result<Raw> {
    use oms_supervisor::capture::{CaptureOptions, LimitAction};
    use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
    let mut request = Request::new(argv.to_vec());
    request.cwd = Some(dir.to_owned());
    request.stdin = None;
    request.streams = Streams::Separate;
    request.scope = Scope::ProcessGroup;
    request.deadline = Instant::now().checked_add(COMBINED_WALL);
    request.remove_environment = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| {
            key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
        })
        .collect();
    request.capture = CaptureOptions {
        max_bytes: COMBINED_OUTPUT,
        on_limit: LimitAction::Terminate,
        lines: None,
    };
    request.stderr_capture = Some(request.capture.clone());
    // Reactor::new keeps process-global child ownership untouched, so this is
    // safe beside other test or worker threads; dedicated() would refuse.
    let mut reactor = Reactor::new();
    let id = reactor
        .start(request)
        .map_err(|e| Error::new(2, e.to_string()))?;
    let mut cancelled = false;
    loop {
        if interrupted.load(Ordering::Acquire) && !cancelled {
            reactor
                .stop(id, StopReason::Cancelled)
                .map_err(|e| Error::new(2, e))?;
            cancelled = true;
        }
        match reactor
            .poll(id, Duration::from_millis(50))
            .map_err(|e| Error::new(2, e))?
        {
            Progress::Pending(_) => {}
            Progress::Complete(done) => {
                require(
                    done.cleanup_complete() && done.output_complete(),
                    "combined verification supervision incomplete",
                )?;
                return match done.stop {
                    Some(StopReason::Cancelled) => {
                        Err(Error::new(130, "combined verification interrupted"))
                    }
                    Some(StopReason::Deadline) => Err(Error::new(
                        124,
                        "combined verification exceeded its wall deadline",
                    )),
                    Some(_) => Err(Error::new(
                        125,
                        "combined verification exceeded its output bound or was stopped",
                    )),
                    None => Ok(Raw {
                        code: done.exit.map_or(125, |e| e.shell_code()),
                        stdout: tail(&done.stdout.bytes),
                        stderr: tail(&done.stderr.map_or_else(Vec::new, |s| s.bytes)),
                    }),
                };
            }
        }
    }
}
/// Merge `candidate_commits` on a detached scratch worktree at
/// `expected_base_sha`, run the verifier there, then compare-and-swap check
/// that `base_ref` did not move. The primary checkout and every ref stay
/// untouched; a pass proves only this exact combination on this exact base.
pub fn verify_combined(
    repo: &Path,
    base_ref: &str,
    expected_base_sha: &str,
    candidate_commits: &[String],
    verify_argv: Option<&[OsString]>,
    interrupted: &AtomicBool,
) -> Result<CombinedOutcome> {
    crate::checkpoint(interrupted)?;
    require(
        !candidate_commits.is_empty(),
        "no candidate commits to combine",
    )?;
    require(
        !base_ref.is_empty()
            && !base_ref.starts_with('-')
            && !base_ref
                .chars()
                .any(|c| c.is_control() || c.is_whitespace()),
        "invalid base reference",
    )?;
    require(
        value::commit(expected_base_sha),
        "expected base must be a full commit SHA",
    )?;
    for commit in candidate_commits {
        require(value::commit(commit), "candidate must be a full commit SHA")?;
    }
    let repo =
        std::fs::canonicalize(repo).map_err(|_| Error::new(2, "cannot resolve repository"))?;
    git_ok(&repo, &["rev-parse", "--git-dir"])?;
    let drifted = |actual: String| CombinedOutcome::TargetDrifted {
        expected_base_sha: expected_base_sha.to_owned(),
        actual_base_sha: actual,
    };
    let actual = resolve(&repo, base_ref)?;
    if actual != expected_base_sha {
        return Ok(drifted(actual));
    }
    for commit in candidate_commits {
        resolve(&repo, commit)?;
    }
    let worktree = Worktree::new(&repo, expected_base_sha)?;
    for commit in candidate_commits {
        crate::checkpoint(interrupted)?;
        let merged = git_raw(
            &worktree.path,
            &["merge", "--no-ff", "--no-edit", "--no-verify", commit],
        )?;
        if merged.code != 0 {
            let mut stderr = merged.stderr;
            stderr.push_str(&merged.stdout);
            return Ok(CombinedOutcome::MergeConflict {
                commit: commit.clone(),
                stderr,
            });
        }
    }
    let combined = git_ok(&worktree.path, &["rev-parse", "HEAD"])?;
    if let Some(argv) = verify_argv.filter(|argv| !argv.is_empty()) {
        let out = run_verify(argv, &worktree.path, interrupted)?;
        if out.code != 0 {
            return Ok(CombinedOutcome::VerificationFailed {
                commit: combined,
                code: out.code,
                stdout: out.stdout,
                stderr: out.stderr,
            });
        }
    }
    crate::checkpoint(interrupted)?;
    let after = resolve(&repo, base_ref)?;
    if after != expected_base_sha {
        return Ok(drifted(after));
    }
    let tree = git_ok(&worktree.path, &["rev-parse", "HEAD^{tree}"])?;
    require(
        value::commit(&combined) && value::commit(&tree),
        "combined merge produced no commit identity",
    )?;
    Ok(CombinedOutcome::Verified {
        combined_sha: combined,
        tree_sha: tree,
    })
}
