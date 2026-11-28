//! Bounded goal driving and restartable exact-tree commit publication.
use crate::cli::Args;
use crate::planning::{ProviderWall, invoke};
use crate::storage::{Context, MAX_FILE, PROGRESS};
use crate::{Error, Output, Result, adaptive, checkpoint, require, value, verify};
use oms_command_plan::{Action, Request};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Seek, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct Progress {
    pub head: String,
    pub tree: String,
    pub accept: String,
    pub closed: u64,
    pub receipts: u64,
    pub stalled: u64,
}
pub fn progress(current: &Progress, previous: Option<&Progress>) -> Value {
    let Some(before) = previous else {
        return json!({"head_changed": false, "tree_changed": false, "accept_changed": false, "tasks_closed": 0, "receipts_new": 0, "stalled_cycles": 0});
    };
    let closed = current.closed.saturating_sub(before.closed);
    json!({"head_changed": current.head != before.head, "tree_changed": current.tree != before.tree, "accept_changed": current.accept != before.accept, "tasks_closed": closed, "receipts_new": current.receipts.saturating_sub(before.receipts), "stalled_cycles": if current.tree != before.tree || closed > 0 { 0 } else { before.stalled.saturating_add(1) }})
}
pub fn lifecycle(packet: &[u8], verification: &str, overlay: Option<&Value>) -> Value {
    let status = oms_command_task::packet::metadata(packet, "status")
        .and_then(|s| std::str::from_utf8(s).ok())
        .unwrap_or("active");
    let mut evidence = json!({"record": "task-packet", "task_status": status, "verification": verification, "rule": "not-verified-fresh"});
    let state = if status == "verified" && verification == "fresh" {
        evidence["rule"] = "verified-fresh".into();
        "achieved"
    } else if !["active", "verified"].contains(&status) {
        evidence["rule"] = "unmapped-status".into();
        "unknown"
    } else if let Some(park) = overlay {
        evidence["record"] = "goal-drive".into();
        evidence["park"] = park.clone();
        match value::text(park, "reason") {
            "cycles-exhausted" | "tasks-exhausted" | "replan-budget-used" => "budget-limited",
            "stuck"
            | "acceptance-vacuous"
            | "acceptance-command-error"
            | "acceptance-command-changed" => "unmet",
            _ => "paused",
        }
    } else {
        "pursuing"
    };
    json!({"state": state, "evidence": evidence})
}
fn task(ctx: &Context<'_>, id: &str) -> Result<Value> {
    value::parse(
        &ctx.action(&Request::new(Action::Show).with("id", id))?
            .stdout,
    )
}
fn guard(ctx: &Context<'_>, expected_ref: &str) -> Result<()> {
    ctx.plan()?.git().guard()?;
    if !expected_ref.is_empty() {
        require(
            ctx.git(&["symbolic-ref", "--quiet", "HEAD"])? == expected_ref,
            "expected work ref changed",
        )?;
    }
    Ok(())
}
fn terminal(
    ctx: &Context<'_>,
    run: &str,
    cycle: u64,
    status: &str,
    reason: &str,
    mut out: Output,
) -> Result<Output> {
    let receipt = value::random("")?;
    let receipt = value::hash(receipt);
    let mut row = json!({"schema": 1, "kind": "terminal", "ts": value::now(), "run_id": run, "cycle": cycle, "status": status, "reason": reason, "base_sha": ctx.git(&["rev-parse", "HEAD"])?, "receipt": receipt});
    row["tree"] = ctx.git(&["rev-parse", "HEAD^{tree}"])?.into();
    row["worktree_clean"] = ctx.clean().is_ok().into();
    row["owner_id"] = ctx.owner_id.clone().into();
    if let Some(raw) = ctx.store.optional(crate::storage::RECEIPT, 64 * 1024)? {
        let contract = value::parse(&raw)?;
        if contract["owner_id"] == ctx.owner_id {
            row["spec_sha256"] = contract["spec_sha256"].clone();
        }
    }
    if let Some(cause) = ctx
        .rows(PROGRESS)?
        .iter()
        .rev()
        .find(|r| r["kind"] == "autopilot-failure" && r["run_id"] == run)
    {
        row["cause"] = cause["failure"].clone();
    }
    {
        let _lock = ctx.lock(PROGRESS)?;
        ctx.append(PROGRESS, &row)?;
    }
    let _ = ctx.services.observe_journal(
        crate::services::JournalEvent {
            repo: &ctx.repo,
            source_kind: "goal-drive",
            source_path: &ctx.state.join(PROGRESS),
            source_id: run,
            outcome: if status == "done" { "done" } else { "parked" },
            verification: if status == "done" {
                "passed"
            } else {
                "not_verified"
            },
        },
        ctx.interrupted,
    );
    if status == "park" {
        let _ = invoke(
            ctx,
            "fail-ledger",
            vec![
                "record".into(),
                "--kind".into(),
                "plan-run".into(),
                "--cmd".into(),
                format!("goal-drive run={run}").into(),
                "--exit".into(),
                "3".into(),
                "--summary".into(),
                format!("goal-drive parked: {reason}").into(),
            ],
            Duration::from_secs(30),
        );
        out.code = 3;
    }
    out.stdout.extend_from_slice(format!("goal-drive: {status} run={run} cycles={cycle} reason={reason}\ngoal-drive: terminal-v1 run={run} receipt={receipt} status={status} reason={reason}\n").as_bytes());
    Ok(out)
}
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(value::random("oms-autonomy-")?);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| Error::new(2, "cannot create private commit scratch"))?;
        Ok(Self(path))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn indexed(
    ctx: &Context<'_>,
    index: &Path,
    args: &[&OsStr],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let result = ctx.plan()?.git().output(
        args,
        input,
        &[("GIT_INDEX_FILE".into(), index.as_os_str().to_owned())],
        16 * MAX_FILE,
    )?;
    require(result.code == 0, "exact commit index operation failed")?;
    Ok(result.stdout)
}
fn index_text(
    ctx: &Context<'_>,
    index: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<String> {
    String::from_utf8(indexed(
        ctx,
        index,
        &args.iter().map(OsStr::new).collect::<Vec<_>>(),
        input,
    )?)
    .map(|s| s.trim().into())
    .map_err(|_| Error::new(2, "non-text Git object id"))
}
fn expected_index(ctx: &Context<'_>, base: &str, patch: &[u8], path: &Path) -> Result<String> {
    index_text(ctx, path, &["read-tree", base], None)?;
    index_text(
        ctx,
        path,
        &["apply", "--cached", "--binary", "-"],
        Some(patch),
    )?;
    let tree = index_text(ctx, path, &["write-tree"], None)?;
    require(value::commit(&tree), "invalid expected tree")?;
    Ok(tree)
}
fn actual_tree(ctx: &Context<'_>, base: &str, path: &Path) -> Result<String> {
    index_text(ctx, path, &["read-tree", base], None)?;
    index_text(ctx, path, &["add", "-A", "--", "."], None)?;
    index_text(ctx, path, &["write-tree"], None)
}
struct IndexLock {
    directory: std::fs::File,
    file: std::fs::File,
    leaf: OsString,
    identity: (u64, u64),
    renamed: bool,
    parent: PathBuf,
}
impl IndexLock {
    fn open(ctx: &Context<'_>) -> Result<(Self, PathBuf)> {
        let raw = ctx.git(&["rev-parse", "--git-path", "index"])?;
        let path = if Path::new(&raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            ctx.repo.join(raw)
        };
        let parent = std::fs::canonicalize(
            path.parent()
                .ok_or_else(|| Error::new(2, "index parent missing"))?,
        )
        .map_err(|_| Error::new(2, "index parent unavailable"))?;
        let directory =
            std::fs::File::open(&parent).map_err(|_| Error::new(2, "cannot open Git directory"))?;
        let leaf = path
            .file_name()
            .ok_or_else(|| Error::new(2, "index name missing"))?
            .to_owned();
        let lock_leaf = OsString::from(format!("{}.lock", leaf.to_string_lossy()));
        let fd = rustix::fs::openat(
            &directory,
            &lock_leaf,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|_| {
            Error::new(
                3,
                "intent-index-locked: preserve existing Git index transaction",
            )
        })?;
        let file: std::fs::File = fd.into();
        let info = file
            .metadata()
            .map_err(|_| Error::new(2, "cannot stat index lock"))?;
        let lock = Self {
            directory,
            file,
            leaf,
            identity: (info.dev(), info.ino()),
            renamed: false,
            parent: parent.clone(),
        };
        Ok((lock, parent.join(path.file_name().unwrap())))
    }
    #[allow(clippy::unnecessary_cast)]
    fn check(&self) -> Result<()> {
        let held = self
            .directory
            .metadata()
            .map_err(|_| Error::new(3, "Git index directory unavailable"))?;
        let named = std::fs::symlink_metadata(&self.parent)
            .map_err(|_| Error::new(3, "Git index directory changed"))?;
        require(
            named.is_dir()
                && !named.file_type().is_symlink()
                && (held.dev(), held.ino()) == (named.dev(), named.ino()),
            "Git index directory replaced during transaction",
        )?;
        let name = OsString::from(format!("{}.lock", self.leaf.to_string_lossy()));
        let info = rustix::fs::statat(&self.directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::new(3, "index lock changed"))?;
        require(
            (info.st_dev as u64, info.st_ino as u64) == self.identity && info.st_nlink == 1,
            "index lock replaced during transaction",
        )
    }
    fn publish(mut self, bytes: &[u8]) -> Result<()> {
        self.check()?;
        self.file
            .set_len(0)
            .and_then(|_| self.file.rewind())
            .and_then(|_| self.file.write_all(bytes))
            .and_then(|_| self.file.sync_all())
            .map_err(|_| {
                Error::new(3, "exact commit published; index synchronization uncertain")
            })?;
        self.check()?;
        let name = OsString::from(format!("{}.lock", self.leaf.to_string_lossy()));
        rustix::fs::renameat(&self.directory, name, &self.directory, &self.leaf)
            .map_err(|_| Error::new(3, "exact commit published; index rename failed"))?;
        self.renamed = true;
        self.directory
            .sync_all()
            .map_err(|_| Error::new(3, "index visible; durability uncertain"))
    }
}
impl Drop for IndexLock {
    fn drop(&mut self) {
        if !self.renamed && self.check().is_ok() {
            let name = OsString::from(format!("{}.lock", self.leaf.to_string_lossy()));
            let _ = rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty());
        }
    }
}
fn read_index(path: &Path) -> Result<Option<Vec<u8>>> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(2, "invalid index parent"))?;
    let root = oms_safe_fs::Root::open(parent).map_err(|_| Error::new(2, "unsafe index parent"))?;
    match root.read_path(
        Path::new(path.file_name().unwrap()),
        16 * MAX_FILE,
        oms_safe_fs::Policy::manifest(),
    ) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(oms_safe_fs::Error::Missing) => Ok(None),
        Err(_) => Err(Error::new(3, "unsafe Git index")),
    }
}
fn append_intent(ctx: &Context<'_>, row: &Value, phase: &str) -> Result<()> {
    let mut row = row.clone();
    row["phase"] = phase.into();
    row["ts"] = value::now().into();
    row["reason"] = phase.into();
    let _lock = ctx.lock(PROGRESS)?;
    ctx.append(PROGRESS, &row)
}
pub(crate) fn mark_repair(ctx: &Context<'_>, intent: &Value) -> Result<()> {
    append_intent(ctx, intent, "repairing")
}
fn evidence(ctx: &Context<'_>, path: &str) -> Result<Vec<u8>> {
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.state.join(path)
    };
    require(
        path.starts_with(&ctx.state),
        "task evidence escaped canonical state repository",
    )?;
    // Native landing publications retain a second link to their certified
    // anchor. Only the existing capture owner may validate those bytes;
    // ordinary evidence still requires the Store's single-link invariant.
    if path.parent() == Some(ctx.state.join(".oms/landing-patches").as_path())
        && let Some(bytes) = ctx
            .services
            .read_managed_patch(&ctx.state, &path, 16 * MAX_FILE)?
    {
        return Ok(bytes);
    }
    Ok(ctx.store.read(path, 16 * MAX_FILE)?)
}
/// Candidate terminals are indexed separately from all authoritative records
/// for the landing id so a contradictory row cannot hide under another task.
#[derive(Default)]
pub(crate) struct LandingLedger<'a> {
    by_task: BTreeMap<&'a str, Vec<&'a oms_command_plan::json::Value>>,
    by_id: BTreeMap<&'a str, Vec<&'a oms_command_plan::json::Value>>,
}
impl<'a> LandingLedger<'a> {
    pub(crate) fn new(rows: &'a [oms_command_plan::json::Value]) -> Self {
        let mut ledger = Self::default();
        for row in rows {
            let event = row.text("event");
            if matches!(event, "intent" | "complete" | "abandoned") {
                ledger
                    .by_id
                    .entry(row.text("landing_id"))
                    .or_default()
                    .push(row);
            }
            if event == "complete" {
                ledger
                    .by_task
                    .entry(row.text("task"))
                    .or_default()
                    .push(row);
            }
        }
        ledger
    }
}

pub(crate) fn completed_landing(
    ctx: &Context<'_>,
    id: &str,
    expected: Option<&oms_command_plan::ReviewFence>,
) -> Result<Value> {
    let plan = ctx.plan()?;
    let snapshot = if ctx.gates.is_some() {
        plan.load_with_locked_snapshot()?
    } else {
        plan.load()?
    };
    let rows = plan.patch_landing_rows()?;
    let ledger = LandingLedger::new(&rows);
    let row = completed_landing_from_snapshot(ctx, &plan, &snapshot, &ledger, id, expected)?;
    value::from_plan(row)
}

/// Commit publication and milestone progress consume the same canonical
/// result proof. Batch readers reuse the Plan snapshot and ledger indexes.
pub(crate) fn completed_landing_from_snapshot<'a>(
    ctx: &Context<'_>,
    plan: &oms_command_plan::Plan<'_>,
    snapshot: &oms_command_plan::Snapshot,
    ledger: &LandingLedger<'a>,
    id: &str,
    expected: Option<&oms_command_plan::ReviewFence>,
) -> Result<&'a oms_command_plan::json::Value> {
    let task = snapshot.task(id)?;
    require(
        snapshot.verified_done(task),
        "completed landing requires done task",
    )?;
    let receipt = oms_command_plan::json::receipt_digest(task)?;
    let patch_hash = value::hash(evidence(ctx, task.text("patch"))?);
    let worktree = ctx.repo.to_string_lossy();
    let candidate = ledger
        .by_task
        .get(id)
        .into_iter()
        .flatten()
        .rev()
        .find(|row| {
            (row.text("worktree") == worktree.as_ref()
                || row.opt("worktree").is_none() && ctx.repo == ctx.state)
                && row.get("plan_id") == snapshot.plan.get("plan_id")
                && row.text("lease") == task.text("review_lease_id")
                && row.text("plan_done_receipt_sha") == receipt
                && row.text("patch") == task.text("patch")
                && row.text("patch_sha") == patch_hash
                && expected.is_none_or(|proof| row.text("base_sha") == proof.base_sha())
        })
        .ok_or_else(|| Error::new(3, "no exact completed landing receipt"))?;
    let landing_id = candidate.text("landing_id");
    let records = ledger.by_id.get(landing_id).into_iter().flatten().copied();
    let (intent, terminal) = oms_command_plan::completed_patch_landing(records, landing_id)?;
    require(
        value::commit(intent.text("base_sha")),
        "landing receipt has invalid base",
    )?;
    let recorded = intent.opt("plan_review_binding");
    if let Some(expected) = expected {
        let recorded = recorded
            .ok_or_else(|| Error::new(2, "landing receipt is missing the selected Plan review"))?;
        require(
            oms_command_plan::json::canonical(recorded)? == expected.encode()?,
            "landing receipt belongs to another selected Plan review",
        )?;
    }
    let proof = recorded
        .map(|value| {
            oms_command_plan::ReviewFence::parse(&oms_command_plan::json::canonical(value)?)
        })
        .transpose()?;
    plan.check_landing_done(snapshot, id, proof.as_ref(), &patch_hash)?;
    Ok(terminal)
}
fn changed_paths(ctx: &Context<'_>, base: &str, tree: &str) -> Result<Vec<String>> {
    let raw = ctx.plan()?.git().run(&[
        "diff-tree",
        "--no-commit-id",
        "--no-renames",
        "--name-only",
        "-r",
        "-z",
        base,
        tree,
    ])?;
    let paths = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| {
            std::str::from_utf8(s)
                .map(str::to_owned)
                .map_err(|_| Error::new(3, "frozen patch path is not UTF-8"))
        })
        .collect::<Result<std::collections::BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    require(!paths.is_empty(), "frozen patch has no changed paths")?;
    Ok(paths)
}
fn review_receipt(ctx: &Context<'_>, row: &Value, intent: &Value) -> Result<()> {
    require(
        value::text(row, "state") == "done"
            && value::text(row, "id") == value::text(intent, "task_id")
            && value::text(row, "lease_id") == value::text(intent, "lease_id")
            && value::text(row, "review_lease_id") == value::text(intent, "lease_id")
            && value::text(row, "provider") == value::text(intent, "provider")
            && value::hash(value::text(row, "verify")) == value::text(intent, "verify_sha256")
            && value::text(row, "executor_id").is_empty()
            && value::text(row, "executor_soul_sha256").is_empty(),
        "landed task no longer matches exact commit intent",
    )?;
    require(
        value::hash(evidence(ctx, value::text(row, "patch"))?)
            == value::text(intent, "patch_sha256"),
        "landed patch differs from frozen intent",
    )?;
    evidence(ctx, value::text(row, "artifact"))?;
    let landed = completed_landing(ctx, value::text(row, "id"), None)?;
    require(
        value::text(&landed, "base_sha") == value::text(intent, "base_sha"),
        "completed landing differs from frozen commit base",
    )?;
    Ok(())
}
fn commit_intent(ctx: &Context<'_>, intent: &Value) -> Result<String> {
    let reference = value::text(intent, "head_ref");
    guard(ctx, reference)?;
    let base = value::text(intent, "base_sha");
    require(value::commit(base), "invalid frozen commit base")?;
    let patch = ctx
        .store
        .read(value::text(intent, "patch"), 16 * MAX_FILE)?;
    let frozen_path = Path::new(value::text(intent, "patch"));
    require(
        frozen_path.parent() == Some(Path::new(".oms/plan/commit-patches"))
            && frozen_path.components().count() == 4,
        "frozen patch escaped commit-patches namespace",
    )?;
    require(
        !patch.is_empty() && value::hash(&patch) == value::text(intent, "patch_sha256"),
        "frozen intent patch changed",
    )?;
    let row = task(ctx, value::text(intent, "task_id"))?;
    review_receipt(ctx, &row, intent)?;
    let scratch = Temporary::new()?;
    let expected_path = scratch.0.join("expected.index");
    let actual_path = scratch.0.join("actual.index");
    let tree = expected_index(ctx, base, &patch, &expected_path)?;
    require(
        intent["paths"] == json!(changed_paths(ctx, base, &tree)?),
        "frozen patch changed its recorded paths",
    )?;
    require(
        actual_tree(ctx, base, &actual_path)? == tree,
        "commit-mismatch: working tree differs from exact frozen patch",
    )?;
    let head = ctx.git(&["rev-parse", "HEAD"])?;
    let already = head != base
        && ctx.git(&["rev-parse", &format!("{head}^")])? == base
        && ctx.git(&["rev-parse", &format!("{head}^{{tree}}")])? == tree;
    require(
        head == base || already,
        "intent-head-moved: current lineage differs from frozen parent/tree",
    )?;
    let (lock, index_path) = IndexLock::open(ctx)?;
    let original = read_index(&index_path)?;
    let current_index_path = scratch.0.join("current.index");
    if let Some(bytes) = &original {
        std::fs::write(&current_index_path, bytes)
            .map_err(|_| Error::new(2, "cannot snapshot Git index"))?;
    } else {
        index_text(ctx, &current_index_path, &["read-tree", "--empty"], None)?;
    }
    let current_index_tree = index_text(ctx, &current_index_path, &["write-tree"], None)?;
    let base_tree = ctx.git(&["rev-parse", &format!("{base}^{{tree}}")])?;
    require(
        current_index_tree == base_tree || already && current_index_tree == tree,
        "intent-index-moved: staged bytes preserved",
    )?;
    guard(ctx, reference)?;
    require(
        ctx.git(&["rev-parse", "HEAD"])? == head && read_index(&index_path)? == original,
        "index or HEAD changed before publication",
    )?;
    let current_actual = scratch.0.join("final.index");
    require(
        actual_tree(ctx, base, &current_actual)? == tree,
        "worktree changed before publication",
    )?;
    let commit = if already {
        head.clone()
    } else {
        let title = value::text(intent, "title");
        let title = title.trim_start_matches(['-', ' ']);
        require(
            !title.is_empty() && value::clean_text(title),
            "commit title must be nonempty text",
        )?;
        let commit = ctx.git(&["commit-tree", &tree, "-p", base, "-m", title])?;
        require(value::commit(&commit), "commit object could not be created")?;
        let mut prepared = intent.clone();
        prepared["commit"] = commit.clone().into();
        prepared["expected_tree"] = tree.clone().into();
        append_intent(ctx, &prepared, "landed")?;
        lock.check()?;
        require(
            read_index(&index_path)? == original,
            "index changed before ref CAS",
        )?;
        ctx.git(&[
            "update-ref",
            "-m",
            "goal-drive: exact task",
            reference,
            &commit,
            base,
        ])?;
        commit
    };
    require(
        ctx.git(&["symbolic-ref", "--quiet", "HEAD"])? == reference
            && ctx.git(&["rev-parse", reference])? == commit
            && ctx.git(&["rev-parse", &format!("{commit}^")])? == base
            && ctx.git(&["rev-parse", &format!("{commit}^{{tree}}")])? == tree
            && read_index(&index_path)? == original,
        "published commit/index mismatch; preserve frozen intent",
    )?;
    let index_bytes =
        std::fs::read(&expected_path).map_err(|_| Error::new(3, "cannot read expected index"))?;
    lock.publish(&index_bytes)?;
    ctx.clean()?;
    append_intent(ctx, intent, "committed")?;
    Ok(commit)
}
fn prepare_intent(
    ctx: &Context<'_>,
    id: &str,
    base: &str,
    reference: &str,
    run: &str,
    cycle: u64,
    title: Option<&str>,
) -> Result<Value> {
    let task = task(ctx, id)?;
    require(
        ["review", "done"].contains(&value::text(&task, "state"))
            && !value::text(&task, "lease_id").is_empty()
            && value::text(&task, "lease_id") == value::text(&task, "review_lease_id"),
        "review lacks exact lease authority",
    )?;
    let patch = evidence(ctx, value::text(&task, "patch"))?;
    require(!patch.is_empty(), "reviewed patch is empty")?;
    let frozen = format!(".oms/plan/commit-patches/{}.patch", value::random("")?);
    ctx.store.publish_once(&frozen, &patch, 16 * MAX_FILE)?;
    let scratch = Temporary::new()?;
    let tree = expected_index(ctx, base, &patch, &scratch.0.join("paths.index"))?;
    let paths = changed_paths(ctx, base, &tree)?;
    let intent = json!({"schema": 1, "kind": "commit-intent", "worktree": ctx.repo, "run_id": run, "cycle": cycle, "intent_id": value::random("intent-")?, "task_id": id, "base_sha": base, "head_ref": reference, "patch": frozen, "patch_sha256": value::hash(&patch), "paths": paths, "title": title.unwrap_or_else(|| value::text(&task, "title")), "provider": task["provider"], "lease_id": task["lease_id"], "verify_sha256": value::hash(value::text(&task, "verify"))});
    append_intent(ctx, &intent, "prepared")?;
    Ok(intent)
}
fn refreshed_intent(ctx: &Context<'_>, intent: &Value) -> Result<Value> {
    let current = task(ctx, value::text(intent, "task_id"))?;
    let patch = evidence(ctx, value::text(&current, "patch"))?;
    if value::hash(&patch) == value::text(intent, "patch_sha256") {
        return Ok(intent.clone());
    }
    require(
        current["repair_count"].as_u64() == Some(1)
            && value::text(&current, "lease_id") == value::text(intent, "lease_id")
            && value::text(&current, "review_lease_id") == value::text(intent, "lease_id")
            && value::text(&current, "provider") == value::text(intent, "provider")
            && value::hash(value::text(&current, "verify")) == value::text(intent, "verify_sha256"),
        "repair changed the frozen task, lease, provider or verifier",
    )?;
    let scratch = Temporary::new()?;
    let tree = expected_index(
        ctx,
        value::text(intent, "base_sha"),
        &patch,
        &scratch.0.join("repair.index"),
    )?;
    let frozen = format!(".oms/plan/commit-patches/{}.patch", value::random("")?);
    ctx.store.publish_once(&frozen, &patch, 16 * MAX_FILE)?;
    let mut updated = intent.clone();
    updated["patch"] = frozen.into();
    updated["patch_sha256"] = value::hash(&patch).into();
    updated["paths"] = json!(changed_paths(ctx, value::text(intent, "base_sha"), &tree)?);
    append_intent(ctx, &updated, "prepared")?;
    Ok(updated)
}
fn reconcile(ctx: &Context<'_>, args: &Args, wall: ProviderWall) -> Result<()> {
    let mut latest = BTreeMap::new();
    for row in ctx.rows(PROGRESS)? {
        if value::text(&row, "kind") == "commit-intent"
            && row["worktree"] == ctx.repo.to_string_lossy().as_ref()
        {
            latest.insert(value::text(&row, "intent_id").to_owned(), row);
        }
    }
    let open = latest
        .values()
        .filter(|r| !["committed", "abandoned"].contains(&value::text(r, "phase")))
        .collect::<Vec<_>>();
    require(
        open.len() <= 1,
        "multiple open commit intents; reconcile without widening authority",
    )?;
    if let Some(intent) = open.first() {
        require(
            !args.has("commit-task") || args.text("commit-task")? == value::text(intent, "task_id"),
            "another task has an open commit intent; requested task cannot advance it",
        )?;
        let current = task(ctx, value::text(intent, "task_id"))?;
        let state = value::text(&current, "state");
        let expired_claim = state == "stale"
            && current["claim_expired"] == true
            && current["stored_state"] == "claimed";
        if ["review", "claimed"].contains(&state) || expired_claim {
            let mut run_args = Args::default();
            run_args.set("id", value::text(intent, "task_id"));
            run_args.set("to", args.get("to", value::text(intent, "provider"))?);
            run_args.flags.insert("land".into());
            for key in [
                "model",
                "fallback-model",
                "reasoning-effort",
                "allow-floor-change",
            ] {
                if args.has(key) {
                    run_args.set(key, args.values[key].clone());
                }
            }
            for key in ["allow-verifier-change", "auto-repair", "retry-known"] {
                if args.flag(key) {
                    run_args.flags.insert(key.into());
                }
            }
            let mut reconcile_ctx = Context::new(
                ctx.repo.clone(),
                ctx.state.clone(),
                ctx.services,
                ctx.interrupted,
            )?;
            reconcile_ctx.owner_id = ctx.owner_id.clone();
            reconcile_ctx.run_id = ctx.run_id.clone();
            reconcile_ctx.cycle = ctx.cycle;
            reconcile_ctx.gates = ctx.gates;
            reconcile_ctx.peer_wall = Some(wall);
            let result = crate::runner::execute_bound(&reconcile_ctx, &run_args, Some(intent))?;
            require(
                result.code == 0,
                "open commit intent could not complete its exact reviewed landing",
            )?;
        } else if state != "done" {
            let out = invoke(
                ctx,
                "patch-land",
                vec![
                    "--repo".into(),
                    ctx.repo.as_os_str().to_owned(),
                    "--recover".into(),
                    "--plan-task".into(),
                    value::text(intent, "task_id").into(),
                ],
                Duration::from_secs(1800),
            )?;
            require(
                out.code == 0,
                "landing-recovery-failed; preserve open commit intent",
            )?;
        }
        commit_intent(ctx, &refreshed_intent(ctx, intent)?)?;
    }
    Ok(())
}
pub(crate) fn execute(ctx: &Context<'_>, args: &Args) -> Result<Output> {
    if args.has("campaign") {
        return crate::campaign::execute(ctx, args);
    }
    require(
        !args.flag("dry-run") && !args.flag("status"),
        "--dry-run and --status require --campaign",
    )?;
    ctx.parent()?;
    let max = args.number("max-cycles", 3, 1, 10)?;
    let run = if args.has("run-id") {
        args.required("run-id")?.to_owned()
    } else {
        value::random("goal-")?
    };
    require(value::id(&run, 64), "run id must be 1..64 safe characters")?;
    let owner = args.text("contract-id")?;
    if !owner.is_empty() {
        let row = value::parse(&ctx.store.read(crate::storage::RECEIPT, 64 * 1024)?)?;
        crate::receipt::live(ctx, &row)?;
        require(
            value::text(&row, "owner_id") == owner
                && !["parked", "done", "proposal-review", "proposing"]
                    .contains(&value::text(&row, "stage"))
                && row["contract"]["max_cycles"].as_u64() == Some(max),
            "contract id, stage or lifetime cycle cap differs from live receipt",
        )?;
        require(
            args.text("expected-ref")? == format!("refs/heads/{}", value::text(&row, "branch")),
            "autopilot goal-drive requires the frozen full work ref",
        )?;
    }
    let expected_ref = args.text("expected-ref")?;
    require(
        expected_ref.is_empty()
            || expected_ref.starts_with("refs/heads/") && !expected_ref.contains(".."),
        "expected ref must be a full work branch ref",
    )?;
    verify::model(args.text("model")?)?;
    verify::model(args.text("fallback-model")?)?;
    verify::effort(args.get("reasoning-effort", "auto")?)?;
    let wall = if args.has("provider-timeout") {
        ProviderWall::Pinned(verify::duration(args.required("provider-timeout")?)?)
    } else {
        ctx.peer_wall.map_or_else(ProviderWall::from_env, Ok)?
    };
    if args.has("allow-floor-change") {
        verify::reason(args.required("allow-floor-change")?)?;
    }
    let _drive_lock = ctx.lock(".oms/plan/goal-drive")?;
    guard(ctx, expected_ref)?;
    if let Err(error) = reconcile(ctx, args, wall) {
        return terminal(
            ctx,
            &run,
            0,
            "park",
            "commit-recovery-failed",
            Output {
                stderr: format!("{}\n", error.message).into_bytes(),
                ..Output::default()
            },
        );
    }
    if args.has("commit-task") {
        let id = args.required("commit-task")?;
        require(value::id(id, 160), "invalid commit task id")?;
        let current = task(ctx, id)?;
        require(
            value::text(&current, "state") == "done",
            "--commit-task requires an already-landed task",
        )?;
        let receipt = completed_landing(ctx, id, None)?;
        if ctx.clean().is_ok() {
            return Ok(Output::text(format!(
                "goal-drive: landed task={id} already committed"
            )));
        }
        let base = value::text(&receipt, "base_sha").to_owned();
        let reference = ctx.git(&["symbolic-ref", "--quiet", "HEAD"])?;
        let intent = prepare_intent(
            ctx,
            id,
            &base,
            &reference,
            &run,
            0,
            if args.has("commit-message") {
                Some(args.required("commit-message")?)
            } else {
                None
            },
        )?;
        return match commit_intent(ctx, &intent) {
            Ok(commit) => Ok(Output::text(format!(
                "goal-drive: committed task={id} commit={commit}"
            ))),
            Err(e) => terminal(
                ctx,
                &run,
                0,
                "park",
                "intent-commit-failed",
                Output {
                    stderr: format!("{}\n", e.message).into_bytes(),
                    ..Output::default()
                },
            ),
        };
    }
    ctx.clean()?;
    let plan = ctx.plan_value()?;
    let accept = value::text(&plan, "accept").to_owned();
    require(
        !accept.trim().is_empty(),
        "plan requires an acceptance command",
    )?;
    require(
        plan["tasks"].as_object().is_some_and(|t| !t.is_empty()),
        "goal-drive requires an existing approved plan with tasks",
    )?;
    let mut previous: Option<Progress> = None;
    let mut output = Output::default();
    let mut attempts = 0;
    for cycle in 1u64.. {
        let mut cycle_context = Context::new(
            ctx.repo.clone(),
            ctx.state.clone(),
            ctx.services,
            ctx.interrupted,
        )?;
        cycle_context.owner_id = owner.to_owned();
        cycle_context.run_id = run.clone();
        cycle_context.cycle = Some(cycle);
        cycle_context.peer_wall = Some(wall);
        let ctx = &cycle_context;
        checkpoint(ctx.interrupted)?;
        guard(ctx, expected_ref)?;
        let plan = ctx.plan_value()?;
        if value::text(&plan, "accept") != accept {
            return terminal(
                ctx,
                &run,
                cycle,
                "park",
                "acceptance-command-changed",
                output,
            );
        }
        let mut acceptance_plan = ctx.plan()?;
        acceptance_plan.context.run_id = run.clone();
        acceptance_plan.context.cycle = Some(cycle);
        let acceptance = acceptance_plan.execute(&Request::new(Action::Accept))?;
        let accept_code = acceptance.code;
        if accept_code != 0 {
            adaptive::record_failure(
                ctx,
                "acceptance",
                &String::from_utf8_lossy(&acceptance.stderr),
                accept_code,
            )?;
        }
        let adaptive_progress = if ctx.owner_id.is_empty() {
            None
        } else {
            Some(adaptive::cycle(ctx, &plan)?)
        };
        output.append(Output {
            code: accept_code,
            stdout: acceptance.stdout,
            stderr: acceptance.stderr,
        });
        guard(ctx, expected_ref)?;
        let tranche = adaptive::tranche(ctx)?;
        if accept_code == 0 && tranche["pending"] != true {
            let pending = plan["tasks"].as_object().is_some_and(|t| {
                t.values()
                    .any(|t| !["done", "cancelled"].contains(&value::text(t, "state")))
            });
            let failed_before = ctx.rows(PROGRESS)?.iter().any(|r| {
                value::text(r, "kind") == "acceptance"
                    && value::text(r, "status") == "fail"
                    && value::text(r, "accept_sha256_full") == value::hash(&accept)
                    && r.get("base_sha")
                        .and_then(Value::as_str)
                        .is_some_and(|base| {
                            ctx.plan()
                                .is_ok_and(|p| p.git().ancestor(base, "HEAD").is_ok())
                        })
            });
            if cycle == 1 && pending && !failed_before {
                return terminal(ctx, &run, cycle, "park", "acceptance-vacuous", output);
            }
            output.code = 0;
            return terminal(ctx, &run, cycle, "done", "acceptance-pass", output);
        }
        if accept_code != 3 && accept_code != 0 {
            return terminal(ctx, &run, cycle, "park", "acceptance-command-error", output);
        }
        if tranche["next_milestone"] == true && tranche["pending"] == false {
            return terminal(ctx, &run, cycle, "park", "milestone-verification", output);
        }
        let current = Progress {
            head: ctx.git(&["rev-parse", "HEAD"])?,
            tree: ctx.git(&["rev-parse", "HEAD^{tree}"])?,
            accept: ctx
                .rows(PROGRESS)?
                .iter()
                .rev()
                .find(|r| value::text(r, "kind") == "acceptance")
                .map(|r| value::text(r, "output_sha256").to_owned())
                .unwrap_or_default(),
            closed: plan["tasks"]
                .as_object()
                .unwrap()
                .values()
                .filter(|t| ["done", "cancelled"].contains(&value::text(t, "state")))
                .count() as u64,
            receipts: 0,
            stalled: 0,
        };
        if tranche["first_attempt"] != true
            && !plan["tasks"]
                .as_object()
                .is_some_and(|tasks| tasks.values().any(|t| t["state"] == "review"))
            && (adaptive_progress
                .as_ref()
                .is_some_and(|p| p["stalled_cycles"].as_u64().unwrap_or(0) > 0)
                || adaptive_progress.is_none()
                    && previous.as_ref().is_some_and(|before| {
                        before.tree == current.tree
                            && !current.accept.is_empty()
                            && before.accept == current.accept
                    }))
        {
            return terminal(ctx, &run, cycle, "park", "stuck", output);
        }
        let facts = progress(&current, previous.as_ref());
        let mut current = current;
        current.stalled = facts["stalled_cycles"].as_u64().unwrap_or(0);
        previous = Some(current.clone());
        let pending = plan["tasks"]
            .as_object()
            .unwrap()
            .values()
            .filter(|t| value::text(t, "state") == "review")
            .collect::<Vec<_>>();
        if pending.len() > 1 {
            return terminal(ctx, &run, cycle, "park", "multiple-reviewed-tasks", output);
        }
        let reference = ctx.git(&["symbolic-ref", "--quiet", "HEAD"])?;
        let id = if let Some(task) = pending.first() {
            value::text(task, "id").to_owned()
        } else {
            let rows = ctx.rows(PROGRESS)?;
            let spent = if owner.is_empty() {
                attempts
            } else {
                rows.iter()
                    .filter(|r| {
                        value::text(r, "kind") == "autopilot-cycle"
                            && value::text(r, "contract_id") == owner
                    })
                    .count() as u64
            };
            if spent >= max {
                return terminal(ctx, &run, cycle, "park", "cycles-exhausted", output);
            }
            let _ = ctx.action(&Request::new(Action::Reclaim));
            let selected = if let Some(id) = tranche["retry_task_id"].as_str() {
                let task = ctx.plan_value()?["tasks"][id].clone();
                require(task["state"] == "ready", "retry task is no longer ready")?;
                task
            } else {
                match ctx.action(&Request::new(Action::Next).flag("json")) {
                    Ok(out) if out.code == 0 => value::parse(&out.stdout)?,
                    Ok(out) => {
                        let reason = adaptive::selection_reason(ctx)?;
                        output.append(out);
                        return terminal(ctx, &run, cycle, "park", reason, output);
                    }
                    Err(e) => {
                        adaptive::record_failure(ctx, "selection", &e.message, e.code)?;
                        output
                            .stderr
                            .extend_from_slice(format!("{}\n", e.message).as_bytes());
                        return terminal(
                            ctx,
                            &run,
                            cycle,
                            "park",
                            "plan-observation-error",
                            output,
                        );
                    }
                }
            };
            let selected_id = value::text(&selected, "id");
            require(value::id(selected_id, 160), "next task has invalid id")?;
            let mut reservation = json!({"schema": 1, "kind": if owner.is_empty() { "goal-cycle" } else { "autopilot-cycle" }, "ts": value::now(), "contract_id": owner, "run_id": run, "cycle": cycle, "task_id": selected_id, "lease_id": ""});
            if !tranche.is_null() {
                reservation["proposal_sha256"] = tranche["proposal_sha256"].clone();
                reservation["retry_sha256"] = tranche["retry_sha256"].clone();
            }
            reservation
                .as_object_mut()
                .unwrap()
                .extend(facts.as_object().unwrap().clone());
            {
                let _lock = ctx.lock(PROGRESS)?;
                ctx.append(PROGRESS, &reservation)?;
            }
            attempts += 1;
            let mut run_args = Args::default();
            run_args.set("id", selected_id);
            run_args.set("to", args.get("to", "codex")?);
            for key in ["model", "fallback-model", "reasoning-effort"] {
                if args.has(key) {
                    run_args.set(key, args.values[key].clone());
                }
            }
            if !tranche["retry_route"].is_null() {
                for (flag, field) in [
                    ("model", "model"),
                    ("fallback-model", "fallback_model"),
                    ("reasoning-effort", "reasoning_effort"),
                ] {
                    let text = value::text(&tranche["retry_route"], field);
                    if !text.is_empty() {
                        run_args.set(flag, text);
                    }
                }
            }
            if args.flag("retry-known") {
                run_args.flags.insert("retry-known".into());
            }
            let result = crate::runner::execute(ctx, &run_args);
            match result {
                Ok(out) if out.code == 0 => {
                    output.append(out);
                }
                Ok(out) => {
                    adaptive::record_failure(
                        ctx,
                        "worker",
                        &format!(
                            "{}{}",
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        ),
                        out.code,
                    )?;
                    output.append(out);
                    return terminal(ctx, &run, cycle, "park", "task-failed", output);
                }
                Err(e) => {
                    adaptive::record_failure(ctx, "worker", &e.message, e.code)?;
                    output
                        .stderr
                        .extend_from_slice(format!("{}\n", e.message).as_bytes());
                    return terminal(
                        ctx,
                        &run,
                        cycle,
                        "park",
                        if e.message.contains("no actionable task") {
                            "tasks-exhausted"
                        } else {
                            "task-failed"
                        },
                        output,
                    );
                }
            }
            let after = ctx.plan_value()?;
            let reviews = after["tasks"]
                .as_object()
                .unwrap()
                .values()
                .filter(|t| value::text(t, "state") == "review")
                .collect::<Vec<_>>();
            if reviews.len() != 1 {
                return terminal(ctx, &run, cycle, "park", "invalid-review-receipt", output);
            }
            value::text(reviews[0], "id").to_owned()
        };
        if ctx.git(&["rev-parse", "HEAD"])? != current.head
            || ctx.git(&["symbolic-ref", "--quiet", "HEAD"])? != reference
        {
            return terminal(ctx, &run, cycle, "park", "head-moved", output);
        }
        let intent = prepare_intent(ctx, &id, &current.head, &reference, &run, cycle, None)?;
        let mut land_args = Args::default();
        land_args.set("id", &id);
        land_args.set("to", args.get("to", "codex")?);
        land_args.flags.insert("land".into());
        for key in [
            "model",
            "fallback-model",
            "reasoning-effort",
            "allow-floor-change",
        ] {
            if args.has(key) {
                land_args.set(key, args.values[key].clone());
            }
        }
        for key in ["allow-verifier-change", "auto-repair", "retry-known"] {
            if args.flag(key) {
                land_args.flags.insert(key.into());
            }
        }
        match crate::runner::execute_bound(ctx, &land_args, Some(&intent)) {
            Ok(out) if out.code == 0 => output.append(out),
            Ok(out) => {
                adaptive::record_failure(
                    ctx,
                    "landing",
                    &format!(
                        "{}{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    ),
                    out.code,
                )?;
                output.append(out);
                return terminal(ctx, &run, cycle, "park", "task-failed", output);
            }
            Err(e) => {
                adaptive::record_failure(ctx, "landing", &e.message, e.code)?;
                output
                    .stderr
                    .extend_from_slice(format!("{}\n", e.message).as_bytes());
                return terminal(ctx, &run, cycle, "park", "task-failed", output);
            }
        }
        let intent = refreshed_intent(ctx, &intent)?;
        append_intent(ctx, &intent, "landed")?;
        match commit_intent(ctx, &intent) {
            Ok(commit) => output.stdout.extend_from_slice(
                format!("goal-drive: committed task={id} commit={commit}\n").as_bytes(),
            ),
            Err(e) => {
                adaptive::record_failure(ctx, "commit", &e.message, e.code)?;
                output
                    .stderr
                    .extend_from_slice(format!("{}\n", e.message).as_bytes());
                return terminal(ctx, &run, cycle, "park", "intent-commit-failed", output);
            }
        }
        // Retain a typed task-cycle association for lifecycle and retro readers;
        // it is not another attempt reservation.
        let _lock = ctx.lock(PROGRESS)?;
        ctx.append(PROGRESS, &json!({"schema": 1, "kind": "goal-task", "run_id": run, "cycle": cycle, "task_id": id, "ts": value::now()}))?;
    }
    unreachable!()
}
