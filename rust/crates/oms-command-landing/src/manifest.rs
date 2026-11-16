//! Multi-repository manifest coordination for `oms land --check-manifest`.
//!
//! A manifest names candidate commits for several repositories and their
//! dependency order. Without `--execute-manifest` nothing is published: each
//! repository is only proven mergeable and verified. With it, a verified
//! fast-forward is applied to the local target branch. A failure holds every
//! transitive dependent and never reverts a repository already integrated; the
//! receipt names what is done, failed and blocked so a rerun of the same
//! manifest skips integrated repositories without repeating their verification.
use crate::{
    Context, Error, Output, Result, V, check, cli::Options, git::Git, hex, now, obj, parent,
    require, services::VerifyRequest, storage, worktree::Sandbox,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_REPOS: usize = 64;
const VERIFY_WALL: Duration = Duration::from_secs(3600);

struct Repo {
    id: String,
    path: PathBuf,
    target: String,
    sha: String,
    deps: Vec<String>,
    verify: Option<Vec<OsString>>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Integrated,
    Verified,
    Failed,
    Blocked,
}
impl State {
    fn name(self) -> &'static str {
        match self {
            State::Integrated => "integrated",
            State::Verified => "verified",
            State::Failed => "failed",
            State::Blocked => "blocked",
        }
    }
}
struct Outcome {
    state: State,
    reason: String,
    tip: String,
}

fn text<'a>(v: &'a V, name: &str) -> Result<&'a str> {
    v.get(name)
        .text()
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .ok_or_else(|| Error::new(2, format!("manifest field {name} must be nonempty text")))
}
fn only_keys(v: &V, allowed: &[&str], what: &str) -> Result<()> {
    let V::Obj(map) = v else {
        return Err(Error::new(2, format!("{what} must be an object")));
    };
    for key in map.keys() {
        require(
            allowed.iter().any(|a| a.as_bytes() == key.as_slice()),
            format!("unknown {what} field: {}", String::from_utf8_lossy(key)),
        )?;
    }
    Ok(())
}
fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn parse(manifest: &V, base: &Path) -> Result<Vec<Repo>> {
    only_keys(manifest, &["schema", "repos"], "manifest")?;
    require(
        matches!(manifest.get("schema"), V::Num(n) if n == "1"),
        "manifest schema must be 1",
    )?;
    let entries = manifest.get("repos").array();
    require(
        !entries.is_empty() && entries.len() <= MAX_REPOS,
        format!("manifest needs 1 to {MAX_REPOS} repos"),
    )?;
    let mut repos: Vec<Repo> = Vec::new();
    for entry in entries {
        only_keys(
            entry,
            &[
                "id",
                "path",
                "target_ref",
                "candidate_sha",
                "depends_on",
                "verify_cmd",
            ],
            "repo",
        )?;
        let id = text(entry, "id")?;
        require(safe_id(id), format!("invalid repo id: {id}"))?;
        require(
            repos.iter().all(|r| r.id != id),
            format!("duplicate repo id: {id}"),
        )?;
        let sha = text(entry, "candidate_sha")?;
        require(hex(sha, 40), format!("{id}: candidate_sha must be 40 hex"))?;
        let target = text(entry, "target_ref")?;
        require(
            !target.starts_with(['-', '/']) && !target.contains("..") && !target.contains(' '),
            format!("{id}: invalid target_ref"),
        )?;
        let raw = Path::new(text(entry, "path")?);
        let joined = if raw.is_absolute() {
            raw.to_owned()
        } else {
            base.join(raw)
        };
        let path = std::fs::canonicalize(&joined)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| Error::new(2, format!("{id}: path is not an existing directory")))?;
        let mut deps = Vec::new();
        match entry.get("depends_on") {
            V::Null => {}
            V::Arr(items) => {
                for dep in items {
                    deps.push(
                        dep.text()
                            .ok_or_else(|| Error::new(2, format!("{id}: depends_on holds ids")))?
                            .to_owned(),
                    );
                }
            }
            _ => return Err(Error::new(2, format!("{id}: depends_on must be an array"))),
        }
        let verify = match entry.get("verify_cmd") {
            V::Null => None,
            V::Arr(words) => {
                require(
                    !words.is_empty() && words.len() <= 64,
                    format!("{id}: verify_cmd needs 1 to 64 words"),
                )?;
                let mut argv = Vec::new();
                for word in words {
                    argv.push(OsString::from(
                        word.text().filter(|w| !w.is_empty()).ok_or_else(|| {
                            Error::new(2, format!("{id}: verify_cmd holds nonempty text"))
                        })?,
                    ));
                }
                crate::safe(
                    argv.iter()
                        .map(|a| a.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ")
                        .as_bytes(),
                )?;
                Some(argv)
            }
            _ => return Err(Error::new(2, format!("{id}: verify_cmd must be an array"))),
        };
        repos.push(Repo {
            id: id.to_owned(),
            path,
            target: target.to_owned(),
            sha: sha.to_owned(),
            deps,
            verify,
        });
    }
    let ids: BTreeSet<&str> = repos.iter().map(|r| r.id.as_str()).collect();
    let mut paths = BTreeSet::new();
    for repo in &repos {
        for dep in &repo.deps {
            require(
                ids.contains(dep.as_str()),
                format!("{}: unknown depends_on id {dep}", repo.id),
            )?;
        }
        require(
            paths.insert(&repo.path),
            format!("{}: repository path listed twice", repo.id),
        )?;
    }
    Ok(repos)
}

/// Deterministic order: dependencies first, ties broken by id.
fn order(repos: &[Repo]) -> Result<Vec<usize>> {
    let mut placed: Vec<usize> = Vec::new();
    let mut done = BTreeSet::new();
    while placed.len() < repos.len() {
        let next = (0..repos.len())
            .filter(|i| !placed.contains(i))
            .filter(|&i| repos[i].deps.iter().all(|d| done.contains(d.as_str())))
            .min_by(|&a, &b| repos[a].id.cmp(&repos[b].id));
        let Some(next) = next else {
            return Err(Error::new(2, "manifest dependencies contain a cycle"));
        };
        done.insert(repos[next].id.as_str());
        placed.push(next);
    }
    Ok(placed)
}

fn merge_and_verify(c: &Context<'_>, repo: &Repo, tip: &str, ff: bool) -> Result<()> {
    if repo.verify.is_none() && ff {
        return Ok(());
    }
    let scratch = Sandbox::new(&repo.path, tip, c.interrupted)?;
    let sandbox = Git::new(&scratch.path, c.interrupted);
    sandbox.raw(&[
        OsStr::new("fetch"),
        OsStr::new("--quiet"),
        OsStr::new("--no-tags"),
        OsStr::new("--"),
        repo.path.as_os_str(),
        OsStr::new(&repo.sha),
    ])?;
    let merge = sandbox.output(
        &[
            OsStr::new("-c"),
            OsStr::new("user.name=oms-land"),
            OsStr::new("-c"),
            OsStr::new("user.email=oms-land@oms.invalid"),
            OsStr::new("merge"),
            OsStr::new("--no-edit"),
            OsStr::new(&repo.sha),
        ],
        None,
        &[],
    )?;
    require(
        merge.code == 0,
        "candidate does not merge cleanly into the target",
    )?;
    if let Some(argv) = &repo.verify {
        let result = c.services.supervise(
            &VerifyRequest {
                argv: argv.clone(),
                cwd: scratch.path.clone(),
                wall: VERIFY_WALL,
                environment: vec![],
            },
            c.interrupted,
        )?;
        require(
            result.code == 0,
            format!("verify_cmd exited {}", result.code),
        )?;
    }
    Ok(())
}

fn publish(c: &Context<'_>, repo: &Repo, git: &Git<'_>, tip: &str) -> Result<()> {
    let branch = format!("refs/heads/{}", repo.target);
    let checked_out = git.text(&["worktree", "list", "--porcelain"])?;
    let mut here = false;
    let mut current: Option<&str> = None;
    for line in checked_out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(p);
        } else if line.strip_prefix("branch ") == Some(branch.as_str()) {
            let same = current
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p == repo.path);
            require(
                same,
                "target branch is checked out in another worktree; refusing to move it",
            )?;
            here = true;
        }
    }
    check(c.interrupted)?;
    if here {
        git.tracked_clean()?;
        let current_tip = git
            .commit(&branch)
            .map_err(|_| Error::new(2, "target branch does not exist"))?;
        require(
            current_tip == tip,
            "target branch moved during verification; re-verify before publishing",
        )?;
        git.run(&["merge", "--ff-only", &repo.sha])?;
        let after = git
            .commit("HEAD")
            .map_err(|_| Error::new(2, "failed to resolve HEAD after merge"))?;
        require(
            after == repo.sha,
            "HEAD did not advance to candidate commit",
        )?;
    } else {
        git.run(&["update-ref", &branch, &repo.sha, tip])?;
    }
    Ok(())
}

fn process(c: &Context<'_>, repo: &Repo, execute: bool, resumed: bool) -> Result<Outcome> {
    let git = Git::new(&repo.path, c.interrupted);
    require(
        Git::root(&repo.path, c.interrupted)? == repo.path,
        "path is not the top level of a Git repository",
    )?;
    let found = git.commit(&repo.sha);
    require(
        found.as_ref().is_ok_and(|s| *s == repo.sha),
        "candidate commit is not present",
    )?;
    let branch = format!("refs/heads/{}", repo.target);
    git.run(&["check-ref-format", &branch])?;
    let tip = git
        .commit(&branch)
        .map_err(|_| Error::new(2, "target branch does not exist"))?;
    if git.succeeds(&["merge-base", "--is-ancestor", &repo.sha, &tip])? {
        return Ok(Outcome {
            state: State::Integrated,
            reason: if resumed {
                "already integrated (earlier receipt for this manifest)".into()
            } else {
                "candidate is already an ancestor of the target".into()
            },
            tip,
        });
    }
    let ff = git.succeeds(&["merge-base", "--is-ancestor", &tip, &repo.sha])?;
    merge_and_verify(c, repo, &tip, ff)?;
    if !execute {
        return Ok(Outcome {
            state: State::Verified,
            reason: "mergeable and verified; nothing applied".into(),
            tip,
        });
    }
    require(
        ff,
        "candidate is not a fast-forward of the target; rebase it and re-verify",
    )?;
    publish(c, repo, &git, &tip)?;
    Ok(Outcome {
        state: State::Integrated,
        reason: "fast-forwarded the target to the verified candidate".into(),
        tip: repo.sha.clone(),
    })
}

fn receipt_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .filter(|p| p.is_dir())
        .ok_or_else(|| Error::new(2, "manifest receipt directory must already exist"))?;
    let name = path
        .file_name()
        .filter(|n| *n != OsStr::new(".") && *n != OsStr::new(".."))
        .ok_or_else(|| Error::new(2, "invalid manifest receipt path"))?;
    Ok(parent.join(name))
}
fn write_receipt(path: &Path, value: &V, c: &Context<'_>) -> Result<()> {
    let old = storage::read_external(path, true)?;
    let mut bytes = value.json(true)?;
    bytes.push(b'\n');
    storage::write_external(
        path,
        if path.exists() { Some(&old) } else { None },
        &bytes,
        c.interrupted,
    )?;
    let meta = std::fs::symlink_metadata(path)?;
    require(
        meta.file_type().is_file(),
        "manifest receipt is not a regular file",
    )?;
    if meta.permissions().mode() & 0o777 != 0o600 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(
        o.words.is_empty(),
        "--check-manifest takes no land action word",
    )?;
    let manifest_path = o
        .path("check-manifest", c.cwd)
        .ok_or_else(|| Error::new(2, "--check-manifest requires a path"))?;
    let receipt = receipt_path(
        &o.path("manifest-receipt", c.cwd)
            .ok_or_else(|| Error::new(2, "--check-manifest requires --manifest-receipt PATH"))?,
    )?;
    let execute = o.has("execute-manifest");
    let manifest_path = std::fs::canonicalize(&manifest_path)
        .map_err(|_| Error::new(2, "manifest file cannot be read"))?;
    let raw = storage::read_external(&manifest_path, false)?;
    require(raw.len() <= MAX_MANIFEST, "manifest is too large")?;
    let manifest = V::parse(&raw).map_err(|_| Error::new(2, "manifest is not valid JSON"))?;
    let base = manifest_path
        .parent()
        .ok_or_else(|| Error::new(2, "invalid manifest path"))?;
    let repos = parse(&manifest, base)?;
    let sequence = order(&repos)?;
    let digest = crate::digest(&manifest.json(false)?);
    // Resume evidence: an earlier receipt for this exact manifest.
    let earlier = V::parse(&storage::read_external(&receipt, true)?).unwrap_or(V::Null);
    let resumed: BTreeSet<String> = if earlier.get("manifest_sha256").text() == Some(&digest) {
        earlier
            .get("integrated")
            .array()
            .iter()
            .filter_map(|v| v.text().map(str::to_owned))
            .collect()
    } else {
        BTreeSet::new()
    };
    // Publication authority is needed only to move a target branch.
    let mut guards = Vec::new();
    if execute {
        parent()?;
        oms_reference::repository::Resolver::from_env()?;
        let mut lock_roots = BTreeSet::new();
        for repo in &repos {
            if Git::root(&repo.path, c.interrupted).is_ok() {
                let state = c.state(&repo.path)?;
                storage::ensure_ignore(&state, c.interrupted)?;
                lock_roots.insert(state);
            } else {
                lock_roots.insert(repo.path.clone());
            }
        }
        for path in &lock_roots {
            guards.push(c.services.publication_guard(path, c.interrupted)?);
        }
    }
    let mut results: BTreeMap<&str, (State, String, String)> = BTreeMap::new();
    for index in sequence.iter().copied() {
        check(c.interrupted)?;
        let repo = &repos[index];
        let held: Vec<&str> = repo
            .deps
            .iter()
            .map(String::as_str)
            .filter(|d| matches!(results[d].0, State::Failed | State::Blocked))
            .collect();
        let outcome = if held.is_empty() {
            match process(c, repo, execute, resumed.contains(&repo.id)) {
                Ok(done) => done,
                Err(e) if e.code == 130 => return Err(e),
                Err(e) => Outcome {
                    state: State::Failed,
                    reason: e.message,
                    tip: String::new(),
                },
            }
        } else {
            Outcome {
                state: State::Blocked,
                reason: format!("held by {}", held.join(", ")),
                tip: String::new(),
            }
        };
        results.insert(&repo.id, (outcome.state, outcome.reason, outcome.tip));
    }
    drop(guards);
    let ids = |wanted: State| -> V {
        V::Arr(
            sequence
                .iter()
                .map(|&i| repos[i].id.as_str())
                .filter(|id| results[id].0 == wanted)
                .map(V::from)
                .collect(),
        )
    };
    let failed = V::Arr(
        sequence
            .iter()
            .map(|&i| &repos[i])
            .filter(|r| results[r.id.as_str()].0 == State::Failed)
            .map(|r| {
                obj([
                    ("id", r.id.as_str().into()),
                    ("reason", results[r.id.as_str()].1.as_str().into()),
                ])
            })
            .collect(),
    );
    let blocked = V::Arr(
        sequence
            .iter()
            .map(|&i| &repos[i])
            .filter(|r| results[r.id.as_str()].0 == State::Blocked)
            .map(|r| {
                obj([
                    ("id", r.id.as_str().into()),
                    ("reason", results[r.id.as_str()].1.as_str().into()),
                ])
            })
            .collect(),
    );
    let held = failed.array().len() + blocked.array().len() > 0;
    let all_integrated = results.values().all(|r| r.0 == State::Integrated);
    let status = if held {
        "partial-hold"
    } else if all_integrated {
        "integrated"
    } else {
        "verified"
    };
    let detail = V::Arr(
        sequence
            .iter()
            .map(|&i| {
                let r = &repos[i];
                let (state, reason, tip) = &results[r.id.as_str()];
                obj([
                    ("id", r.id.as_str().into()),
                    ("path", r.path.to_string_lossy().into_owned().into()),
                    ("target_ref", r.target.as_str().into()),
                    ("candidate_sha", r.sha.as_str().into()),
                    ("state", state.name().into()),
                    ("reason", reason.as_str().into()),
                    ("target_tip", tip.as_str().into()),
                ])
            })
            .collect(),
    );
    let summary = obj([
        ("schema", 1usize.into()),
        ("status", status.into()),
        ("mode", if execute { "execute" } else { "check" }.into()),
        ("manifest_sha256", digest.into()),
        ("integrated", ids(State::Integrated)),
        ("verified", ids(State::Verified)),
        ("failed", failed),
        ("blocked_dependents", blocked),
        ("repos", detail),
        ("updated", now().into()),
    ]);
    write_receipt(&receipt, &summary, c)?;
    let mut output = Output::json(&summary)?;
    output.code = if held { 3 } else { 0 };
    Ok(output)
}
