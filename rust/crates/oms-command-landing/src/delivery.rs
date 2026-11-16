//! Exact-HEAD delivery receipts and read-only lock status. Receipt state never
//! substitutes for a fresh lock observation, successful exact-SHA CI or proof
//! that the full selection executed.
use crate::{
    Context, Error, Output, Result, V, admission, ci,
    cli::Options,
    digest,
    git::{Git, Snapshot},
    now, obj, parent, require, storage, string,
    worktree::{self, Sandbox},
};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
fn cksum(raw: &[u8]) -> (u32, usize) {
    let mut crc = 0u32;
    let add = |crc: &mut u32, b: u8| {
        *crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            *crc = if *crc & 0x80000000 != 0 {
                (*crc << 1) ^ 0x04c11db7
            } else {
                *crc << 1
            };
        }
    };
    for b in raw {
        add(&mut crc, *b);
    }
    let mut size = raw.len();
    while size != 0 {
        add(&mut crc, (size & 255) as u8);
        size >>= 8;
    }
    (!crc, raw.len())
}
pub(crate) fn state_directory(git: &Git<'_>) -> Result<PathBuf> {
    let common = worktree::common(git)?;
    let root = std::fs::canonicalize(
        common
            .parent()
            .ok_or_else(|| Error::new(2, "common directory has no parent"))?,
    )?;
    let name = root.file_name().unwrap_or_default().to_string_lossy();
    let name = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .replace("..", "_");
    let name = if matches!(name.as_str(), "" | "." | "..") {
        String::from("repo")
    } else {
        name
    };
    let (crc, size) = cksum(root.as_os_str().as_encoded_bytes());
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        .ok_or_else(|| Error::new(2, "land state home unavailable"))?;
    Ok(state
        .join("oh-my-setting/land")
        .join(format!("{name}-{crc}-{size}")))
}
fn save(path: &Path, row: &V, interrupted: &std::sync::atomic::AtomicBool) -> Result<()> {
    let raw = storage::read_external(path, true)?;
    storage::write_external(
        path,
        if path.exists() { Some(&raw) } else { None },
        &row.json(false)?,
        interrupted,
    )
}
fn status(c: &Context<'_>, o: &Options, git: &Git<'_>, directory: &Path) -> Result<Output> {
    let settings =
        oms_lock::Settings::from_env().map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let lock = oms_lock::Lock::for_file(&directory.join("active"), settings)
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let observed = lock.probe();
    let active = match observed {
        oms_lock::Status::Held => true.into(),
        oms_lock::Status::Free => false.into(),
        oms_lock::Status::Unknown => V::Null,
    };
    let code = if o.has("active-exit") {
        match observed {
            oms_lock::Status::Held => 3,
            oms_lock::Status::Free => 0,
            oms_lock::Status::Unknown => 4,
        }
    } else {
        0
    };
    let head = git.commit("HEAD").unwrap_or_default();
    let mut receipts = vec![];
    if directory.exists() {
        for entry in std::fs::read_dir(directory)?.flatten() {
            if entry.path().extension() != Some(OsStr::new("json")) {
                continue;
            }
            if let Ok(raw) = storage::read_external(&entry.path(), false)
                && let Ok(row) = V::parse(&raw)
            {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                receipts.push((string(&row, "sha") == head, modified, row, entry.path()));
            }
        }
    }
    receipts.sort_by_key(|(matches, time, _, _)| (*matches, *time));
    let Some((_, _, mut row, path)) = receipts.pop() else {
        let mut out = if o.has("json") {
            Output::json(&obj([("active", active)]))?
        } else {
            Output::text(format!(
                "land lock: {} (no landing recorded)\n",
                observed.as_str()
            ))
        };
        out.code = if o.has("active-exit") { code } else { 1 };
        return Ok(out);
    };
    let mut matches = false;
    let holder = storage::read_external(&directory.join("active-holder"), true)
        .ok()
        .and_then(|raw| V::parse(&raw).ok());
    if let Some(holder) = holder {
        let pid = holder.get("pid").py().parse::<u32>().unwrap_or(0);
        let alive = oms_command_operations::lock::pid_alive(i64::from(pid))
            && oms_command_operations::lock::process_start(pid).as_deref()
                == holder.get("start").text();
        matches = observed == oms_lock::Status::Held
            && alive
            && holder.get("receipt").text() == path.file_name().and_then(OsStr::to_str)
            && holder.get("sha") == row.get("sha");
        if alive && observed == oms_lock::Status::Held {
            row.set("active_holder", holder.get("sha").clone());
        }
    }
    row.set("active", active);
    if observed == oms_lock::Status::Held {
        row.set("active_matches_receipt", matches);
    }
    let mut out = if o.has("json") {
        Output::json(&row)?
    } else {
        let state = string(&row, "state");
        Output::text(format!(
            "land {}: {}\n  active: {}\n",
            string(&row, "sha").chars().take(7).collect::<String>(),
            if state == "running" && !matches {
                "receipt says running; lock does not prove this receipt is active"
            } else {
                &state
            },
            observed.as_str()
        ))
    };
    out.code = code;
    let _ = c;
    Ok(out)
}
fn finish(path: &Path, row: &mut V, state: &str, message: &str, c: &Context<'_>) -> Result<Output> {
    row.set("state", state);
    row.set("finished_at", now());
    row.set("message", message);
    save(path, row, c.interrupted)?;
    Ok(Output {
        code: i32::from(!matches!(state, "passed" | "pr-opened")),
        stdout: format!("land: {message}\nreceipt: {}\n", path.display()).into_bytes(),
        stderr: vec![],
    })
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Direct,
    Pr,
    LocalMerge,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Direct => "direct",
            Mode::Pr => "pr",
            Mode::LocalMerge => "local-merge",
        }
    }
}
struct Run<'a> {
    c: &'a Context<'a>,
    o: &'a Options,
    repo: &'a Path,
    git: &'a Git<'a>,
    directory: &'a Path,
    receipt: &'a Path,
    stamp: &'a str,
    snapshot: &'a Snapshot,
    gate: &'a str,
    destination: &'a str,
    remote: &'a str,
    target: &'a str,
    ci_wait: u64,
    head_branch: &'a str,
    from_sha: &'a str,
}
/// Runs the gate in a detached sandbox at the exact HEAD (local-merge: at the
/// merge result). `Some` is a finished failure; nothing has been published.
fn gate_phase(r: &Run<'_>, row: &mut V) -> Result<Option<Output>> {
    let (c, repo, snapshot, gate) = (r.c, r.repo, r.snapshot, r.gate);
    if row.get("gate").get("rc") == &0usize.into() {
        return Ok(None);
    }
    let scratch = Sandbox::new(repo, &snapshot.head, c.interrupted)?;
    let mut merged_tree = None;
    if !r.from_sha.is_empty() {
        let sandbox = Git::new(&scratch.path, c.interrupted);
        sandbox.raw(&[
            OsStr::new("fetch"),
            OsStr::new("--quiet"),
            OsStr::new("--no-tags"),
            OsStr::new("--"),
            repo.as_os_str(),
            OsStr::new(r.from_sha),
        ])?;
        let merge = sandbox.output(
            &[
                OsStr::new("-c"),
                OsStr::new("user.name=oms-land"),
                OsStr::new("-c"),
                OsStr::new("user.email=oms-land@oms.invalid"),
                OsStr::new("merge"),
                OsStr::new("--no-edit"),
                OsStr::new(r.from_sha),
            ],
            None,
            &[],
        )?;
        if merge.code != 0 {
            return finish(
                r.receipt,
                row,
                "failed",
                "merge does not apply cleanly; nothing merged",
                c,
            )
            .map(Some);
        }
        merged_tree = Some(sandbox.text(&["rev-parse", "HEAD^{tree}"])?);
    }
    let result = admission::verify(c, &scratch.path, gate, Duration::from_secs(3600))?;
    let log = [result.stdout, result.stderr].concat();
    let log = if crate::safe(&log).is_ok() {
        log
    } else {
        b"[gate output withheld]\n".to_vec()
    };
    let log_path = r.directory.join(format!("{}.log", r.stamp));
    let old = storage::read_external(&log_path, true)?;
    storage::write_external(
        &log_path,
        if log_path.exists() { Some(&old) } else { None },
        &log,
        c.interrupted,
    )?;
    let mut gate_row = row.get("gate").clone();
    gate_row.set("rc", V::Num(result.code.to_string()));
    let words = oms_command_plan::floor::split(gate, false)?;
    let full = words == ["bash", "scripts/check.sh", "--parallel"];
    gate_row.set("scope", if full { "full" } else { "unknown" });
    if let Some(tree) = merged_tree {
        gate_row.set("tree", tree);
    }
    row.set("gate", gate_row);
    save(r.receipt, row, c.interrupted)?;
    if result.code != 0 {
        return finish(r.receipt, row, "failed", "gate failed; nothing pushed", c).map(Some);
    }
    Ok(None)
}
fn gh_exec(r: &Run<'_>, cwd: &Path, args: &[&str]) -> Result<crate::git::Execution> {
    let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| "gh".into());
    crate::git::process(
        cwd,
        std::iter::once(bin)
            .chain(args.iter().map(std::ffi::OsString::from))
            .collect(),
        None,
        Duration::from_secs(60),
        r.c.interrupted,
        &[("GH_HOST".into(), "github.com".into())],
    )
}
fn open_pr(r: &Run<'_>, repo_arg: &str) -> Result<Option<V>> {
    let rows = ci::gh(
        r.c,
        &[
            "pr",
            "list",
            "--repo",
            repo_arg,
            "--head",
            r.head_branch,
            "--base",
            r.target,
            "--state",
            "open",
            "--json",
            "number,url,headRefOid",
            "--limit",
            "2",
        ],
        Duration::from_secs(30),
    )?;
    require(
        matches!(rows, V::Arr(_)) && rows.array().len() <= 1,
        "ambiguous or invalid pull request list",
    )?;
    Ok(rows.array().first().cloned())
}
fn pr_text(base: &str, r: &Run<'_>) -> Result<(String, String)> {
    let range = format!("{base}..{}", r.snapshot.head);
    let log = r
        .git
        .text(&["log", "--format=%h %s", "--max-count=50", &range])?;
    let count: usize = r
        .git
        .text(&["rev-list", "--count", &range])?
        .parse()
        .map_err(|_| Error::new(2, "unreadable commit count"))?;
    let newest = log
        .lines()
        .next()
        .and_then(|l| l.split_once(' '))
        .map_or("land", |(_, s)| s);
    let mut title: String = newest.chars().take(100).collect();
    if count > 1 {
        title += &format!(" (+{} more)", count - 1);
    }
    let body = format!(
        "Landed by `oms land --pr`.\n\nCommits ({count}):\n{}\n\nLocal gate passed on exact head {}.\n",
        log.lines()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        r.snapshot.head
    );
    crate::safe(title.as_bytes())?;
    crate::safe(body.as_bytes())?;
    Ok((title, body))
}
fn land_pr(r: &Run<'_>, row: &mut V, base: &str) -> Result<Output> {
    let (c, o, git, snapshot, target, head_branch) =
        (r.c, r.o, r.git, r.snapshot, r.target, r.head_branch);
    if base == snapshot.head {
        row.set("ci", obj([("conclusion", "skipped".into())]));
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "nothing-to-land".into()),
            ]),
        );
        return finish(
            r.receipt,
            row,
            "passed",
            "remote target already contains HEAD; nothing to land",
            c,
        );
    }
    let repo_arg = ci::query_repo(r.destination)?;
    let method = o.value("merge-method", "squash");
    let (title, body) = pr_text(base, r)?;
    row.set("head_branch", head_branch);
    save(r.receipt, row, c.interrupted)?;
    {
        let _guard = c
            .services
            .publication_guard(&c.state(r.repo)?, c.interrupted)?;
        require(git.snapshot()? == *snapshot, "tree moved before push")?;
        require(
            crate::review::remote_tip(git, r.destination, target)?.as_deref() == Some(base),
            "remote target changed before push",
        )?;
        git.transport_safe()?;
        let existing = crate::review::remote_tip(git, r.destination, head_branch)?;
        if let Some(tip) = existing.as_deref().filter(|t| *t != snapshot.head) {
            let ancestor = git.output(
                &[
                    OsStr::new("merge-base"),
                    OsStr::new("--is-ancestor"),
                    OsStr::new(tip),
                    OsStr::new(&snapshot.head),
                ],
                None,
                &[],
            )?;
            if ancestor.code != 0 {
                return finish(
                    r.receipt,
                    row,
                    "failed",
                    "remote head branch has diverged from local HEAD; refusing to overwrite existing commits",
                    c,
                );
            }
        }
        if existing.as_deref() != Some(&snapshot.head) {
            let refspec = format!("{}:refs/heads/{head_branch}", snapshot.head);
            let lease = existing
                .as_ref()
                .map(|tip| format!("--force-with-lease=refs/heads/{head_branch}:{tip}"));
            let mut args = vec![OsStr::new("push"), OsStr::new("--no-verify")];
            args.extend(lease.as_deref().map(OsStr::new));
            args.extend([OsStr::new(r.remote), OsStr::new(&refspec)]);
            let result = git.output(&args, None, &[])?;
            row.set(
                "push",
                obj([
                    ("rc", V::Num(result.code.to_string())),
                    ("base", base.into()),
                    ("head_branch", head_branch.into()),
                    ("moved", 1usize.into()),
                ]),
            );
            save(r.receipt, row, c.interrupted)?;
            if result.code != 0 {
                return finish(
                    r.receipt,
                    row,
                    "failed",
                    "branch push failed or outcome unknown; inspect remote before retrying",
                    c,
                );
            }
        }
    }
    let scratch = storage::Temp::new("oh-my-setting-land-pr")?;
    let mut pr = open_pr(r, &repo_arg)?;
    if pr.is_none() {
        let body_path = scratch.path.join("body.md");
        std::fs::write(&body_path, &body)?;
        let created = gh_exec(
            r,
            &scratch.path,
            &[
                "pr",
                "create",
                "--repo",
                &repo_arg,
                "--base",
                target,
                "--head",
                head_branch,
                "--title",
                &title,
                "--body-file",
                &body_path.to_string_lossy(),
            ],
        )?;
        if created.code != 0 {
            return finish(
                r.receipt,
                row,
                "failed",
                "branch pushed; pull request creation failed",
                c,
            );
        }
        pr = open_pr(r, &repo_arg)?;
    }
    for _ in 0..5 {
        if pr
            .as_ref()
            .is_some_and(|p| p.get("headRefOid").text() == Some(&snapshot.head))
        {
            break;
        }
        std::thread::park_timeout(Duration::from_secs(2));
        pr = open_pr(r, &repo_arg)?;
    }
    let Some(pr) = pr.filter(|p| {
        p.get("headRefOid").text() == Some(&snapshot.head) && p.get("number").integer(true)
    }) else {
        return finish(
            r.receipt,
            row,
            "failed",
            "no open pull request at the exact HEAD; nothing merged",
            c,
        );
    };
    let number = pr.get("number").py();
    row.set("pr_number", pr.get("number").clone());
    row.set("pr_url", pr.get("url").clone());
    save(r.receipt, row, c.interrupted)?;
    let mut ci_scope = "unknown";
    if r.ci_wait == 0 {
        row.set(
            "ci",
            obj([
                ("conclusion", "skipped".into()),
                ("scope", "unknown".into()),
            ]),
        );
        row.set(
            "merge",
            obj([
                ("state", "SKIPPED".into()),
                ("reason", "ci-wait-zero".into()),
            ]),
        );
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "ci-wait-zero".into())]),
        );
        return finish(
            r.receipt,
            row,
            "pr-opened",
            "pull request opened/updated; merge skipped because --ci-wait=0",
            c,
        );
    } else {
        let deadline = Instant::now() + Duration::from_secs(r.ci_wait);
        let mut successful = None;
        let conclusion = loop {
            crate::check(c.interrupted)?;
            let mut found = None;
            for event in ["pull_request", "push"] {
                match ci::latest(c, r.destination, &snapshot.head, head_branch, event) {
                    Ok(Some(run)) => {
                        found = Some(run);
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return finish(
                            r.receipt,
                            row,
                            "failed",
                            &format!("CI unknown: {e}; pull request left open"),
                            c,
                        );
                    }
                }
            }
            let state = match &found {
                Some(run) => {
                    let state = ci::conclusion(run, &snapshot.head)?;
                    row.set(
                        "ci",
                        obj([
                            ("conclusion", state.clone().into()),
                            ("run_id", run.get("databaseId").clone()),
                        ]),
                    );
                    save(r.receipt, row, c.interrupted)?;
                    if state == "success" {
                        successful = Some(run.clone());
                    }
                    state
                }
                None => "pending".into(),
            };
            if state != "pending" || Instant::now() >= deadline {
                break state;
            }
            std::thread::park_timeout(
                Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now())),
            );
        };
        if conclusion != "success" {
            return finish(
                r.receipt,
                row,
                "failed",
                &format!("CI {conclusion}; pull request left open and unmerged; retry --wait"),
                c,
            );
        }
        if let Some(run) = successful
            && let Ok(detailed) =
                ci::run_details(c, r.destination, run.get("databaseId"), &snapshot.head)
            && ci::full_scope(&detailed, &snapshot.head).is_ok_and(|s| s == "full")
        {
            ci_scope = "full";
        }
        let mut ci_row = row.get("ci").clone();
        ci_row.set("scope", ci_scope);
        row.set("ci", ci_row);
    }
    let flag = format!("--{method}");
    let merge_state = obj([("method", method.into()), ("state", "requested".into())]);
    row.set("merge", merge_state);
    save(r.receipt, row, c.interrupted)?;
    // A neutral cwd keeps `--delete-branch` from touching this checkout.
    let merged = gh_exec(
        r,
        &scratch.path,
        &[
            "pr",
            "merge",
            &number,
            "--repo",
            &repo_arg,
            &flag,
            "--delete-branch",
            "--match-head-commit",
            &snapshot.head,
        ],
    )?;
    if merged.code != 0 {
        return finish(
            r.receipt,
            row,
            "failed",
            "merge refused or outcome unknown; pull request left open",
            c,
        );
    }
    let view = ci::gh(
        c,
        &[
            "pr",
            "view",
            &number,
            "--repo",
            &repo_arg,
            "--json",
            "state,mergeCommit",
        ],
        Duration::from_secs(30),
    );
    let (state, commit) = match &view {
        Ok(v) => (
            string(v, "state"),
            v.get("mergeCommit").get("oid").clone().or(""),
        ),
        Err(_) => (String::from("UNKNOWN"), "".into()),
    };
    row.set(
        "merge",
        obj([
            ("method", method.into()),
            ("state", state.clone().into()),
            ("commit", commit),
        ]),
    );
    save(r.receipt, row, c.interrupted)?;
    if state != "MERGED" {
        return finish(
            r.receipt,
            row,
            "failed",
            "merge requested; merged state not confirmed",
            c,
        );
    }
    let fetched = git.run(&[
        "-c",
        "fetch.writeCommitGraph=false",
        "fetch",
        "--no-tags",
        "--",
        r.remote,
        &format!("+refs/heads/{target}:refs/remotes/{}/{target}", r.remote),
    ]);
    if fetched.is_err() {
        return finish(
            r.receipt,
            row,
            "failed",
            "merged; fetching the target failed",
            c,
        );
    }
    let local_full = row.get("gate").get("scope").text() == Some("full");
    let merge_commit = row
        .get("merge")
        .get("commit")
        .text()
        .unwrap_or("")
        .to_owned();
    if o.has("no-update") {
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "disabled".into())]),
        );
    } else if !merge_commit.is_empty() && merge_commit != snapshot.head {
        // GitHub built a new commit (squash/merge); CI above covered only the PR head.
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "merge-commit-pending-ci".into()),
            ]),
        );
    } else if ci_scope == "full" || local_full {
        let update = c.services.after_land(r.repo, row, c.interrupted)?;
        row.set("update", update);
        if row.get("update").get("rc") != &0usize.into() {
            return finish(
                r.receipt,
                row,
                "failed",
                "merged; installation failed or was deferred",
                c,
            );
        }
    } else {
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "full-ci-required".into()),
            ]),
        );
    }
    save(r.receipt, row, c.interrupted)?;
    let mut output = finish(
        r.receipt,
        row,
        "passed",
        &format!("merged PR #{number} ({method}) after exact-SHA CI"),
        c,
    )?;
    tail(r, &mut output)?;
    Ok(output)
}
fn global_git_config() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let candidates = [
        std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from),
        home.as_ref().map(|h| h.join(".gitconfig")),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(|x| PathBuf::from(x).join("git/config"))
            .or_else(|| home.as_ref().map(|h| h.join(".config/git/config"))),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .map(|p| vec![("GIT_CONFIG_GLOBAL".into(), p.into_os_string())])
        .unwrap_or_default()
}
fn land_local(r: &Run<'_>, row: &mut V) -> Result<Output> {
    let (c, git, snapshot) = (r.c, r.git, r.snapshot);
    row.set("merged_from", r.from_sha);
    row.set("push", obj([("rc", "skipped".into())]));
    row.set("ci", obj([("conclusion", "skipped".into())]));
    row.set(
        "update",
        obj([("rc", "skipped".into()), ("reason", "local-merge".into())]),
    );
    save(r.receipt, row, c.interrupted)?;
    require(
        git.snapshot()? == *snapshot,
        "tree moved before the local merge",
    )?;
    let mut environment = global_git_config();
    environment.push(("OMS_LAND_ACTIVE".into(), "1".into()));
    let before_head = git.commit("HEAD")?;
    // Hooks stay enabled here: a pre-merge-commit guard accepts only this gated merge.
    let merge = crate::git::process(
        r.repo,
        ["git", "merge", "--no-edit", r.from_sha]
            .iter()
            .map(std::ffi::OsString::from)
            .collect(),
        None,
        Duration::from_secs(3600),
        c.interrupted,
        &environment,
    )?;
    if merge.code != 0 {
        let _ = git.output(&[OsStr::new("merge"), OsStr::new("--abort")], None, &[]);
        return finish(
            r.receipt,
            row,
            "failed",
            "merge refused, conflicted or lacked a Git identity; aborted",
            c,
        );
    }
    let head = git.commit("HEAD")?;
    row.set("merged_head", head);
    save(r.receipt, row, c.interrupted)?;
    let tree = match git.text(&["rev-parse", "HEAD^{tree}"]) {
        Ok(t) => t,
        Err(e) => {
            let _ = git.output(
                &[
                    OsStr::new("reset"),
                    OsStr::new("--hard"),
                    OsStr::new(&before_head),
                ],
                None,
                &[],
            );
            return Err(e);
        }
    };
    if row
        .get("gate")
        .get("tree")
        .text()
        .is_some_and(|t| t != tree)
    {
        let _ = git.output(
            &[
                OsStr::new("reset"),
                OsStr::new("--hard"),
                OsStr::new(&before_head),
            ],
            None,
            &[],
        );
        return finish(
            r.receipt,
            row,
            "failed",
            "merged tree differs from the gated tree; rolled back the local merge",
            c,
        );
    }
    finish(
        r.receipt,
        row,
        "passed",
        "gated merge completed locally; nothing pushed",
        c,
    )
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    if o.has("check-manifest") {
        return crate::manifest::execute(c, o);
    }
    match o.action("") {
        "notes" => return crate::notes::execute(c, o),
        "blame-session" => return crate::attribution::blame(c, o),
        "conflicts" => return conflicts(c, o),
        "" | "status" => {}
        _ => return Err(Error::new(2, "unknown land action")),
    }
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    let directory = state_directory(&git)?;
    if o.action("") == "status" {
        require(o.words.len() == 1, "unexpected land status argument")?;
        return status(c, o, &git, &directory);
    }
    parent()?;
    let env_mode = std::env::var("OMS_LAND_MODE").unwrap_or_default();
    require(
        ["", "direct", "pr", "auto"].contains(&env_mode.as_str()),
        "OMS_LAND_MODE must be pr, auto or direct",
    )?;
    let ci_wait = o
        .value("ci-wait", "2700")
        .parse::<u64>()
        .map_err(|_| Error::new(2, "--ci-wait must be seconds"))?;
    let sibling_wait = o
        .value("sibling-wait", "1800")
        .parse::<u64>()
        .map_err(|_| Error::new(2, "--sibling-wait must be seconds"))?;
    require(
        !o.has("ci-full") || (ci_wait > 0 && !o.has("no-update")),
        "--ci-full needs --ci-wait above zero and an install",
    )?;
    require(
        !o.has("handoff-receipt-to") || ci_wait > 0,
        "receipt handoff requires successful CI; --ci-wait must be above zero",
    )?;
    let remote = o.value("remote", "origin");
    require(
        !remote.is_empty()
            && !remote.starts_with('-')
            && remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid remote name",
    )?;
    let target = o.value("target", "main");
    git.run(&["check-ref-format", &format!("refs/heads/{target}")])?;
    git.transport_safe()?;
    git.tracked_clean()?;
    let snapshot = git.snapshot()?;
    let local_merge = o.has("local-merge");
    let destination = if local_merge {
        String::new()
    } else {
        git.text(&["remote", "get-url", "--push", remote])?
    };
    require(
        local_merge || destination.lines().count() == 1,
        "land requires one push destination",
    )?;
    let branch = git.text(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let auto_pr = env_mode == "auto"
        && branch != "HEAD"
        && branch != target
        && ci::query_repo(&destination).is_ok_and(|s| s.starts_with("github.com/"));
    let mode = if local_merge {
        Mode::LocalMerge
    } else if o.has("pr") || env_mode == "pr" || auto_pr {
        Mode::Pr
    } else {
        Mode::Direct
    };
    require(
        mode == Mode::Pr || !(o.has("head-branch") || o.has("merge-method")),
        "--head-branch and --merge-method apply to --pr only",
    )?;
    require(
        mode != Mode::Pr || !o.has("ci-full"),
        "--ci-full is not supported with --pr",
    )?;
    let mut head_branch = String::new();
    let mut from_sha = String::new();
    if mode == Mode::Pr {
        head_branch = if o.has("head-branch") {
            o.get("head-branch").to_owned()
        } else if branch != "HEAD" && branch != target {
            branch.clone()
        } else {
            format!("oms/land-{}", &snapshot.head[..12])
        };
        require(
            head_branch != target && !head_branch.starts_with('-'),
            "PR head branch must differ from the target branch",
        )?;
        git.run(&["check-ref-format", &format!("refs/heads/{head_branch}")])?;
    } else if mode == Mode::LocalMerge {
        require(
            !o.has("ci-full") && !o.has("handoff-receipt-to") && !o.has("sync-shared"),
            "--local-merge does not use CI, handoff or shared sync",
        )?;
        require(
            branch == target,
            format!("--local-merge requires the checked-out branch to be {target}"),
        )?;
        from_sha = git
            .commit(o.get("from"))
            .map_err(|_| Error::new(2, "--from does not resolve to a commit"))?;
        require(
            !git.succeeds(&["merge-base", "--is-ancestor", &from_sha, &snapshot.head])?,
            "--from is already contained in HEAD; nothing to merge",
        )?;
    }
    let gate = if o.has("gate") {
        o.get("gate").into()
    } else if repo.join("scripts/check.sh").is_file() {
        if repo.join("rust/crates/oms").is_dir() {
            let from = if local_merge {
                snapshot.head.clone()
            } else {
                format!("{remote}/{target}")
            };
            format!(
                "bash scripts/check.sh --affected --ci-parity --changed-from {from} --changed-to HEAD"
            )
        } else {
            String::from("bash scripts/check.sh")
        }
    } else {
        return Err(Error::new(2, "no default gate; pass --gate CMD"));
    };
    crate::safe(gate.as_bytes())?;
    let mut request_parts = vec![destination.trim(), remote, target, &gate];
    if mode != Mode::Direct {
        request_parts.extend([
            mode.name(),
            head_branch.as_str(),
            from_sha.as_str(),
            o.value("merge-method", "squash"),
        ]);
    }
    let request = V::Arr(request_parts.iter().map(|s| V::from(*s)).collect());
    let stamp_value = oms_command_plan::json::parse(&request.json(false)?)?;
    let request_hash = digest(&oms_command_plan::json::default_bytes(&stamp_value, true)?);
    let stamp = format!("{}-{}", snapshot.head, &request_hash[..24]);
    let receipt = directory.join(format!("{stamp}.json"));
    let _active = storage::lock(&directory.join("active"), c.interrupted)?;
    let holder = obj([
        ("receipt", format!("{stamp}.json").into()),
        ("sha", snapshot.head.clone().into()),
        ("pid", (std::process::id() as usize).into()),
        (
            "start",
            oms_command_operations::lock::process_start(std::process::id())
                .unwrap_or_default()
                .into(),
        ),
    ]);
    save(&directory.join("active-holder"), &holder, c.interrupted)?;
    let previous = storage::read_external(&receipt, true)?;
    let mut row = if previous.is_empty() {
        obj([
            ("schema", 1usize.into()),
            ("sha", snapshot.head.clone().into()),
            ("request", stamp.clone().into()),
            ("repo", digest(repo.as_os_str().as_encoded_bytes()).into()),
            ("remote", remote.into()),
            ("target", target.into()),
            ("mode", mode.name().into()),
            ("started_at", now().into()),
            ("state", "running".into()),
            (
                "gate",
                obj([
                    ("command", gate.clone().into()),
                    ("rc", "pending".into()),
                    ("scope", "unknown".into()),
                ]),
            ),
            ("push", obj([("rc", "pending".into())])),
            ("ci", obj([("conclusion", "pending".into())])),
            (
                "update",
                obj([("rc", "skipped".into()), ("reason", "not-reached".into())]),
            ),
        ])
    } else {
        let row = V::parse(&previous)?;
        require(
            string(&row, "sha") == snapshot.head
                && row.get("gate").get("command").text() == Some(&gate),
            "land receipt identity mismatch",
        )?;
        row
    };
    save(&receipt, &row, c.interrupted)?;
    let base = if mode == Mode::LocalMerge {
        String::new()
    } else {
        let base = crate::review::remote_tip(&git, &destination, target)?
            .ok_or_else(|| Error::new(2, "remote target is absent"))?;
        require(
            git.succeeds(&["merge-base", "--is-ancestor", &base, &snapshot.head])?,
            "remote target is not an ancestor; rebase before land",
        )?;
        base
    };
    let ctx = Run {
        c,
        o,
        repo: &repo,
        git: &git,
        directory: &directory,
        receipt: &receipt,
        stamp: &stamp,
        snapshot: &snapshot,
        gate: &gate,
        destination: &destination,
        remote,
        target,
        ci_wait,
        head_branch: &head_branch,
        from_sha: &from_sha,
    };
    if let Some(failed) = gate_phase(&ctx, &mut row)? {
        return Ok(failed);
    }
    require(
        git.snapshot()? == snapshot,
        "HEAD, tracked tree or index changed while gate ran; nothing pushed",
    )?;
    git.tracked_clean()?;
    if !o.has("ignore-siblings") {
        c.services
            .wait_siblings(&repo, Duration::from_secs(sibling_wait), c.interrupted)?;
    }
    match mode {
        Mode::Pr => return land_pr(&ctx, &mut row, &base),
        Mode::LocalMerge => return land_local(&ctx, &mut row),
        Mode::Direct => {}
    }
    if base != snapshot.head {
        let _guard = c
            .services
            .publication_guard(&c.state(&repo)?, c.interrupted)?;
        require(git.snapshot()? == snapshot, "tree moved before push")?;
        require(
            crate::review::remote_tip(&git, &destination, target)?.as_deref() == Some(&base),
            "remote target changed before push",
        )?;
        git.transport_safe()?;
        let result = git.output(
            &[
                OsStr::new("push"),
                OsStr::new("--no-verify"),
                OsStr::new(&format!("--force-with-lease=refs/heads/{target}:{base}")),
                OsStr::new(remote),
                OsStr::new(&format!("{}:refs/heads/{target}", snapshot.head)),
            ],
            None,
            &[],
        )?;
        row.set(
            "push",
            obj([
                ("rc", V::Num(result.code.to_string())),
                ("base", base.into()),
                ("moved", 1usize.into()),
            ]),
        );
        save(&receipt, &row, c.interrupted)?;
        if result.code != 0 {
            return finish(
                &receipt,
                &mut row,
                "failed",
                "push failed or outcome unknown; inspect remote before retrying",
                c,
            );
        }
    } else if row.get("push").get("rc") != &0usize.into() {
        row.set(
            "push",
            obj([
                ("rc", 0usize.into()),
                ("base", base.into()),
                ("moved", 0usize.into()),
            ]),
        );
        save(&receipt, &row, c.interrupted)?;
    }
    if row.get("push").get("moved") == &1usize.into()
        && let Some(base) = crate::helpers::recorded_base(&row)?
    {
        let _ = crate::attribution::record_commits(
            &c.state(&repo)?,
            &repo,
            &format!("{base}..{}", snapshot.head),
            &stamp,
            c.interrupted,
        );
    }
    if ci_wait == 0 {
        row.set("ci", obj([("conclusion", "skipped".into())]));
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "ci-skipped".into())]),
        );
        return finish(
            &receipt,
            &mut row,
            "passed",
            "pushed; CI skipped; nothing installed",
            c,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(ci_wait);
    let mut successful = None;
    loop {
        crate::check(c.interrupted)?;
        let query = ci::latest(c, &destination, &snapshot.head, target, "push");
        match query {
            Ok(Some(run)) => {
                let conclusion = ci::conclusion(&run, &snapshot.head)?;
                row.set(
                    "ci",
                    obj([
                        ("conclusion", conclusion.clone().into()),
                        ("run_id", run.get("databaseId").clone()),
                    ]),
                );
                save(&receipt, &row, c.interrupted)?;
                if conclusion == "success" {
                    successful = Some(run);
                    break;
                }
                if conclusion != "pending" {
                    return finish(
                        &receipt,
                        &mut row,
                        "failed",
                        &format!("pushed; CI {conclusion}"),
                        c,
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                return finish(
                    &receipt,
                    &mut row,
                    "failed",
                    &format!("pushed; CI unknown: {e}"),
                    c,
                );
            }
        }
        if Instant::now() >= deadline
            || !(o.has("wait") || o.has("ci-full") || o.has("handoff-receipt-to"))
        {
            break;
        }
        std::thread::park_timeout(
            Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    let Some(run) = successful else {
        return finish(
            &receipt,
            &mut row,
            "failed",
            "pushed; CI pending or missing; retry --wait",
            c,
        );
    };
    let detailed = ci::run_details(c, &destination, run.get("databaseId"), &snapshot.head)?;
    let mut scope = ci::full_scope(&detailed, &snapshot.head).unwrap_or("unknown");
    let local_full = row.get("gate").get("scope").text() == Some("full");
    if scope != "full" && !local_full && !o.has("no-update") {
        let full = ci::latest(c, &destination, &snapshot.head, target, "workflow_dispatch")?;
        if let Some(full) = full
            && ci::conclusion(&full, &snapshot.head)? == "success"
        {
            let proof = ci::run_details(c, &destination, full.get("databaseId"), &snapshot.head)?;
            if ci::full_scope(&proof, &snapshot.head).is_ok_and(|s| s == "full") {
                scope = "full";
                let mut ci = row.get("ci").clone();
                ci.set("full_run_id", full.get("databaseId").clone());
                row.set("ci", ci);
            }
        }
        if scope != "full" && o.has("ci-full") && !row.get("ci").get("dispatch").truth() {
            require(
                crate::review::remote_tip(&git, &destination, target)?.as_deref()
                    == Some(&snapshot.head),
                "remote target moved before full CI dispatch",
            )?;
            let mut ci = row.get("ci").clone();
            ci.set("dispatch", "requested");
            row.set("ci", ci);
            save(&receipt, &row, c.interrupted)?;
            let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| "gh".into());
            let result = crate::git::process(
                &repo,
                vec![
                    bin,
                    "workflow".into(),
                    "run".into(),
                    "test.yml".into(),
                    "--repo".into(),
                    ci::query_repo(&destination)?.into(),
                    "--ref".into(),
                    target.into(),
                ],
                None,
                Duration::from_secs(30),
                c.interrupted,
                &[],
            )?;
            if result.code != 0 {
                return finish(
                    &receipt,
                    &mut row,
                    "failed",
                    "full CI dispatch outcome unknown; request will not be repeated",
                    c,
                );
            }
        }
        if scope != "full"
            && (o.has("wait") || o.has("ci-full") || o.has("handoff-receipt-to"))
            && row.get("ci").get("dispatch").truth()
        {
            while Instant::now() < deadline {
                crate::check(c.interrupted)?;
                if let Some(full) =
                    ci::latest(c, &destination, &snapshot.head, target, "workflow_dispatch")?
                {
                    let conclusion = ci::conclusion(&full, &snapshot.head)?;
                    if conclusion == "success" {
                        let details = ci::run_details(
                            c,
                            &destination,
                            full.get("databaseId"),
                            &snapshot.head,
                        )?;
                        if ci::full_scope(&details, &snapshot.head).is_ok_and(|s| s == "full") {
                            scope = "full";
                            let mut ci = row.get("ci").clone();
                            ci.set("full_run_id", full.get("databaseId").clone());
                            row.set("ci", ci);
                            break;
                        }
                    } else if conclusion != "pending" {
                        return finish(
                            &receipt,
                            &mut row,
                            "failed",
                            "full CI did not succeed for the exact SHA",
                            c,
                        );
                    }
                }
                std::thread::park_timeout(
                    Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            if scope != "full" {
                return finish(
                    &receipt,
                    &mut row,
                    "failed",
                    "full CI remains pending or unproved; retry --wait",
                    c,
                );
            }
        }
    }
    let mut ci_row = row.get("ci").clone();
    ci_row.set("scope", scope);
    row.set("ci", ci_row);
    if o.has("no-update") {
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "disabled".into())]),
        );
    } else if scope == "full" || local_full {
        require(
            git.snapshot()? == snapshot
                && crate::review::remote_tip(&git, &destination, target)?.as_deref()
                    == Some(&snapshot.head),
            "install target moved from verified SHA",
        )?;
        let update = c.services.after_land(&repo, &row, c.interrupted)?;
        row.set("update", update);
        if row.get("update").get("rc") != &0usize.into() {
            return finish(
                &receipt,
                &mut row,
                "failed",
                "pushed; CI passed; installation failed or was deferred",
                c,
            );
        }
    } else {
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "full-ci-required".into()),
            ]),
        );
    }
    save(&receipt, &row, c.interrupted)?;
    let mut output = finish(
        &receipt,
        &mut row,
        "passed",
        if scope == "full" || local_full {
            "pushed; exact-SHA CI and full proof passed"
        } else {
            "pushed; CI passed; full proof missing; nothing installed"
        },
        c,
    )?;
    tail(&ctx, &mut output)?;
    Ok(output)
}
fn tail(r: &Run<'_>, output: &mut Output) -> Result<()> {
    let (c, o, repo, receipt, remote, target) = (r.c, r.o, r.repo, r.receipt, r.remote, r.target);
    if let Some(destination) = o.path("handoff-receipt-to", c.cwd) {
        crate::notes::handoff(repo, &destination, receipt, c.interrupted)?;
        output
            .stdout
            .extend_from_slice(b"land: exact receipt and range boundary handed off\n");
    }
    if let Some(destination) = o.path("sync-shared", c.cwd) {
        let mut sync_options = Options::default();
        sync_options
            .values
            .insert("repo".into(), vec![destination.into_os_string()]);
        sync_options
            .values
            .insert("remote".into(), vec![remote.into()]);
        sync_options
            .values
            .insert("target".into(), vec![target.into()]);
        let synced = sync(c, &sync_options)?;
        output.stdout.extend(synced.stdout);
        output.stderr.extend(synced.stderr);
    }
    Ok(())
}
pub(crate) fn sync(c: &Context<'_>, o: &Options) -> Result<Output> {
    parent()?;
    require(o.words.is_empty(), "unexpected sync-shared argument")?;
    let repo = c.state(&c.repo(o)?)?;
    let git = Git::new(&repo, c.interrupted);
    let remote = o.value("remote", "origin");
    let target = o.value("target", "main");
    require(
        !remote.is_empty()
            && !remote.starts_with('-')
            && remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid remote name",
    )?;
    git.run(&["check-ref-format", &format!("refs/heads/{target}")])?;
    let _guard = c.services.publication_guard(&repo, c.interrupted)?;
    let _sync = storage::lock(&repo.join(".oms/locks/canonical-sync"), c.interrupted)?;
    git.safe()?;
    git.run(&["symbolic-ref", "--quiet", "HEAD"])?;
    git.clean()?;
    let before = git.snapshot()?;
    let reference = format!("refs/remotes/{remote}/{target}");
    git.run(&[
        "-c",
        "fetch.writeCommitGraph=false",
        "fetch",
        "--no-tags",
        "--",
        remote,
        &format!("+refs/heads/{target}:{reference}"),
    ])?;
    require(
        git.succeeds(&["merge-base", "--is-ancestor", "HEAD", &reference])?,
        "shared checkout is ahead or diverged; sync refused",
    )?;
    git.clean()?;
    require(
        git.snapshot()? == before,
        "shared checkout changed during fetch",
    )?;
    let raw = git.run(&["merge", "--ff-only", &reference])?;
    Ok(Output {
        stdout: raw,
        ..Output::default()
    })
}
fn conflicts(c: &Context<'_>, o: &Options) -> Result<Output> {
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    if git.safe().is_err() {
        return Ok(Output::text(
            "land conflicts: not checked (unsafe Git config)\n",
        ));
    }
    let max = o
        .value("max", "12")
        .parse::<usize>()
        .map_err(|_| Error::new(2, "--max must be 1..24"))?;
    require((1..=24).contains(&max), "--max must be 1..24")?;
    let base = git.commit(o.value("base", "main"))?;
    let branches = if o.words.len() > 1 {
        o.words[1..].to_vec()
    } else {
        git.text(&[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/oms/",
        ])?
        .lines()
        .map(str::to_owned)
        .collect()
    };
    let branches: Vec<_> = branches.into_iter().take(max).collect();
    let mut rows = vec![];
    for (i, left) in std::iter::once(&base).chain(branches.iter()).enumerate() {
        for right in branches.iter().skip(i) {
            let out = git.output(
                &[
                    "merge-tree".as_ref(),
                    "--write-tree".as_ref(),
                    left.as_ref(),
                    right.as_ref(),
                ],
                None,
                &[],
            )?;
            if out.code == 129 {
                return Ok(Output::text(
                    "land conflicts: Git too old (requires 2.38+)\n",
                ));
            }
            rows.push(obj([
                ("left", left.clone().into()),
                ("right", right.clone().into()),
                ("clean", (out.code == 0).into()),
                (
                    "detail",
                    String::from_utf8_lossy(&out.stdout).into_owned().into(),
                ),
            ]));
        }
    }
    if o.has("json") {
        Output::json(&obj([("limit", max.into()), ("pairs", V::Arr(rows))]))
    } else {
        let mut out = format!("land conflicts: at most {max} branches\n");
        for row in rows {
            out += &format!(
                "{} + {}: {}\n",
                string(&row, "left"),
                string(&row, "right"),
                if row.get("clean").truth() {
                    "clean"
                } else {
                    "conflicts"
                }
            );
        }
        out += "landing order suggestion: ";
        out += &branches.join(" -> ");
        out.push('\n');
        Ok(Output::text(out))
    }
}
