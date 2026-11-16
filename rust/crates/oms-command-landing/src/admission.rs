//! Isolated patch admission. Candidate verification and the base-owned floor
//! are independent observations; restoring tests cannot replace the latter.
use crate::{
    Context, Error, Output, Result, V,
    cli::Options,
    digest,
    git::{Git, Snapshot},
    guard::in_scope,
    now, obj, require, safe,
    services::VerifyRequest,
    storage, string,
    worktree::Sandbox,
};
use oms_reference::{capture::CaptureIndex, scope::Protocol};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub verify: String,
    pub task: String,
    pub ml: bool,
    pub allow_verifier_change: bool,
    pub allow_test_reduction: bool,
    pub allow_floor_change: String,
    pub allow_restructure: bool,
    pub accept_encoding_loss: bool,
    pub accept_salvaged: bool,
    pub regression: String,
    pub red_check: String,
    pub covers: Vec<String>,
}
pub struct Admission {
    pub verdict: bool,
    pub report: PathBuf,
    pub record: V,
    pub patch: PathBuf,
    pub bytes: Vec<u8>,
    pub snapshot: Snapshot,
}
impl Policy {
    pub(crate) fn options(o: &Options) -> Self {
        Self {
            verify: o.get("verify").into(),
            task: o.get("plan-task").into(),
            ml: o.has("ml"),
            allow_verifier_change: o.has("allow-verifier-change"),
            allow_test_reduction: o.has("allow-test-reduction"),
            allow_floor_change: o.get("allow-floor-change").into(),
            allow_restructure: o.has("allow-restructure"),
            accept_encoding_loss: o.has("accept-encoding-loss"),
            accept_salvaged: o.has("accept-salvaged"),
            regression: o.get("regression-test").into(),
            red_check: o.get("red-check").into(),
            covers: o.all("covers"),
        }
    }
    pub fn parameters(&self) -> oms_reference::binding::Parameters {
        oms_reference::binding::Parameters {
            values: [
                self.verify.clone(),
                u8::from(self.ml).to_string(),
                self.task.clone(),
                u8::from(self.allow_verifier_change).to_string(),
                u8::from(self.allow_test_reduction).to_string(),
                self.allow_floor_change.clone(),
                u8::from(self.allow_restructure).to_string(),
                u8::from(self.accept_encoding_loss).to_string(),
            ],
        }
    }
}
/// Indexed private captures live outside the repository, where `read_patch`
/// refuses them; read those through the index and keep the same size bound.
pub(crate) fn capture_bytes(index: &CaptureIndex, path: &Path) -> Result<Vec<u8>> {
    match index.indexed_bytes(path)? {
        Some(raw) => {
            require(raw.len() <= storage::MAX, "patch exceeds the size bound")?;
            Ok(raw)
        }
        None => Ok(index.read_patch(path, storage::MAX)?),
    }
}
pub(crate) fn task(c: &Context<'_>, repo: &Path, id: &str) -> Result<V> {
    require(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "--plan-task must match [A-Za-z0-9._-]+",
    )?;
    let services = c
        .services
        .plan()
        .ok_or_else(|| Error::unavailable("plan"))?;
    let mut request =
        oms_command_plan::Request::new(oms_command_plan::Action::EvidenceSnapshot).with("id", id);
    request.repo = Some(repo.to_owned());
    let mut context = oms_command_plan::Context::from_environment(&request, repo)?;
    context.state_repo = c.state(repo)?;
    context.plan_file = context.state_repo.join(".oms/plan/tasks.json");
    let plan = oms_command_plan::Plan::open(context, services, c.interrupted)?;
    let row = V::parse(&plan.execute(&request)?.stdout)?;
    require(
        !row.get("executor_id").truth() && !row.get("executor_soul_sha256").truth(),
        "Soul executor receipt is retired; create a fresh plan task",
    )?;
    Ok(row)
}
fn test_path(path: &str) -> bool {
    regex::Regex::new(
        r"(^|/)(tests?|spec)/|(^|/)test_[^/]+$|[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$",
    )
    .unwrap()
    .is_match(path)
}
pub(crate) fn verify(
    c: &Context<'_>,
    repo: &Path,
    command: &str,
    wall: Duration,
) -> Result<crate::git::Execution> {
    safe(command.as_bytes())?;
    c.services.supervise(
        &VerifyRequest {
            argv: vec!["bash".into(), "-c".into(), command.into()],
            cwd: repo.to_owned(),
            wall,
            environment: vec![],
        },
        c.interrupted,
    )
}
fn code(result: &Result<crate::git::Execution>) -> i32 {
    result.as_ref().map(|r| r.code).unwrap_or(125)
}
fn output(result: &Result<crate::git::Execution>) -> Vec<u8> {
    match result {
        Ok(v) => [v.stdout.clone(), v.stderr.clone()].concat(),
        Err(e) => e.message.as_bytes().to_vec(),
    }
}
struct Scope {
    protocol: Protocol,
    repo: PathBuf,
    channel: PathBuf,
    expected: String,
    attempt: String,
    stop: Arc<AtomicBool>,
    monitor: Option<std::thread::JoinHandle<oms_reference::Result<()>>>,
    temporary: Option<storage::Temp>,
    finished: bool,
}
impl Scope {
    fn healthy(&self) -> Result<()> {
        require(
            self.monitor.as_ref().is_some_and(|m| !m.is_finished()),
            "live admission reference monitor is unavailable",
        )?;
        self.protocol
            .read(&self.repo, &self.channel, &self.expected)?;
        Ok(())
    }
    fn start(c: &Context<'_>, repo: &Path, patch: &Path) -> Result<Self> {
        let protocol = Protocol::production_with_controller(c.services.controller()?)?;
        let temporary = storage::Temp::new("oms-admission-reference")?;
        for name in ["scope", "result", "proof"] {
            storage::replace(&temporary.path, Path::new(name), None, b"")?;
        }
        let channel = temporary.path.join("scope");
        let mut result = vec![];
        let (attempt, expected) = protocol.begin_owned(
            repo,
            "patch-admit",
            patch,
            &channel,
            &mut result,
            c.interrupted,
        )?;
        std::fs::write(temporary.path.join("result"), &result)?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut scope = Self {
            protocol,
            repo: repo.to_owned(),
            channel,
            expected,
            attempt,
            stop,
            monitor: None,
            temporary: Some(temporary),
            finished: false,
        };
        let p = scope.protocol.clone();
        let r = scope.repo.clone();
        let ch = scope.channel.clone();
        let expected = scope.expected.clone();
        let stop = scope.stop.clone();
        scope.monitor = Some(std::thread::spawn(move || {
            p.monitor(&r, &ch, &expected, &stop)
        }));
        let proof = scope.protocol.await_monitor(
            repo,
            &scope.channel,
            &scope.expected,
            &std::process::id().to_string(),
            c.interrupted,
        )?;
        std::fs::write(
            scope.channel.parent().unwrap().join("proof"),
            format!("{proof}\n"),
        )?;
        Ok(scope)
    }
    fn finish(&mut self, interrupted: &AtomicBool) -> Result<()> {
        oms_reference::publication::finish(&self.repo, &self.attempt, "0", interrupted)?;
        self.finished = true;
        self.close(interrupted)
    }
    fn close(&mut self, interrupted: &AtomicBool) -> Result<()> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(m) = self.monitor.take() {
            let _ = m
                .join()
                .map_err(|_| Error::new(2, "reference monitor panicked"))?;
        }
        self.protocol
            .close_scope(&self.repo, &self.channel, &self.expected, interrupted)?;
        self.temporary.take();
        Ok(())
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let clear = AtomicBool::new(false);
        if !self.finished {
            let _ = oms_reference::publication::finish(&self.repo, &self.attempt, "1", &clear);
        }
        if self.monitor.is_some() {
            let _ = self.close(&clear);
        }
    }
}

pub(crate) fn admit(
    c: &Context<'_>,
    repo: &Path,
    input: &Path,
    report: Option<&Path>,
    policy: &Policy,
    keep: bool,
) -> Result<Admission> {
    let git = Git::new(repo, c.interrupted);
    git.safe()?;
    git.tracked_clean()?;
    let snapshot = git.snapshot()?;
    let state = c.state(repo)?;
    storage::ensure_ignore(&state, c.interrupted)?;
    let index = CaptureIndex::load(&state)?;
    let mut path = input.to_owned();
    let mut bytes = capture_bytes(&index, &path)?;
    require(!bytes.is_empty(), "patch is empty")?;
    let known = index.admit(
        &path,
        report.unwrap_or(&state.join(".oms/artifacts/admit/pending.md")),
    )?;
    if !known && report.is_none() && !crate::child() {
        let landing = format!(
            "land-admit-{}-{}",
            now().replace(['-', ':'], ""),
            oms_command_plan::storage::random_hex(8)?
        );
        path = oms_reference::producer::Producer::open(&state, c.interrupted)?
            .allocate(&path, &landing, &digest(&bytes), repo, c.interrupted)
            .map_err(|e| Error::new(e.exit_code(), format!("{e:?}")))?;
        bytes = capture_bytes(&CaptureIndex::load(&state)?, &path)?;
    }
    let mut policy = policy.clone();
    let unavailable_context = oms_reference::binding::Unavailable;
    let domain = oms_reference::binding::Domain {
        harness_root: c.source,
        context: c
            .services
            .admission_context()
            .unwrap_or(&unavailable_context),
    };
    let mut allowed = vec![];
    let mut forbidden = vec![];
    if !policy.task.is_empty() {
        let row = task(c, repo, &policy.task)?;
        let stored = string(&row, "verify");
        require(
            !stored.is_empty(),
            "plan task has no stored verify contract",
        )?;
        require(
            policy.verify.is_empty() || policy.verify == stored,
            "--verify does not match plan task review contract",
        )?;
        policy.verify = stored;
        let context = c
            .services
            .admission_context()
            .ok_or_else(|| Error::unavailable("authenticated admission scope"))?;
        allowed = context.effective_allowed_paths(&state, &row, c.interrupted)?;
        forbidden = row
            .get("forbidden_paths")
            .array()
            .iter()
            .map(|s| {
                s.text()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new(2, "invalid forbidden path"))
            })
            .collect::<Result<_>>()?;
    }
    let base_check = storage::read(repo, Path::new("scripts/check.sh"), true)?;
    if policy.verify.is_empty() && !base_check.is_empty() {
        let text = String::from_utf8_lossy(&base_check);
        require(
            text.contains("fast)") || text.contains("fast |") || text.contains("--fast"),
            "existing check.sh requires an explicit --verify command",
        )?;
        policy.verify = if policy.ml {
            "bash scripts/check.sh ml-smoke".into()
        } else {
            "bash scripts/check.sh fast".into()
        };
    }
    if !policy.allow_floor_change.is_empty() {
        crate::parent()?;
        require(
            !policy.allow_floor_change.trim().is_empty()
                && !policy.allow_floor_change.chars().any(char::is_control),
            "--allow-floor-change requires a plain single-line reason",
        )?;
    }
    if !policy.covers.is_empty() {
        c.services.coverage(repo, &policy.covers, None)?;
    }
    let batch_prefix = domain.batch_prefix(
        &CaptureIndex::load(&state)?,
        repo,
        &policy.task,
        c.interrupted,
    )?;
    let mut scope = Scope::start(c, &state, &path)?;
    let report = report
        .map(Path::to_owned)
        .unwrap_or_else(|| state.join(format!(".oms/artifacts/admit/admit-{}.md", scope.attempt)));
    let scratch = Sandbox::new(repo, &snapshot.head, c.interrupted)?;
    let candidate = Git::new(&scratch.path, c.interrupted);
    let mut gates = vec![];
    let mut logs = Vec::new();
    let applied = candidate
        .apply(&bytes, true, false, true)
        .and_then(|()| candidate.apply(&bytes, false, false, true));
    gates.push((
        "apply",
        applied.is_ok(),
        applied
            .err()
            .map_or_else(|| "patch applies cleanly".into(), |e| e.message),
    ));
    let mut changed = vec![];
    let mut surface = vec![];
    let mut verification = String::from("skipped");
    let mut verify_exit = V::Null;
    if gates[0].1 {
        let raw = candidate.run(&[
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "-z",
            "HEAD",
            "--",
        ])?;
        changed = raw
            .split(|b| *b == 0)
            .filter(|v| !v.is_empty())
            .map(|b| {
                String::from_utf8(b.to_vec())
                    .map_err(|_| Error::new(2, "patch path cannot be scope-checked"))
            })
            .collect::<Result<Vec<_>>>()?;
        require(!changed.is_empty(), "patch has no changed files")?;
        let mut scope_ok = true;
        for file in &changed {
            scope_ok &= in_scope(file, &allowed, &forbidden)?;
            require(
                !file.split('/').any(|p| p.eq_ignore_ascii_case(".git")),
                "patch changes Git metadata",
            )?;
            if allowed.is_empty()
                && !policy.allow_restructure
                && !file.contains('/')
                && !git.succeeds(&["cat-file", "-e", &format!("HEAD:{file}")])?
            {
                scope_ok = false;
            }
        }
        gates.push((
            "scope",
            scope_ok,
            "changed paths obey the declared scope and structural floor".into(),
        ));
        let mut syntax = true;
        for file in &changed {
            if !scratch.path.join(file).is_file() {
                continue;
            }
            if file.ends_with(".sh") {
                let result = crate::git::process(
                    &scratch.path,
                    vec!["bash".into(), "-n".into(), "--".into(), file.into()],
                    None,
                    Duration::from_secs(30),
                    c.interrupted,
                    &[],
                )?;
                syntax &= result.code == 0;
                logs.extend(result.stderr);
                if executable("shellcheck") {
                    let checked = crate::git::process(
                        &scratch.path,
                        vec!["shellcheck".into(), "--".into(), file.into()],
                        None,
                        Duration::from_secs(60),
                        c.interrupted,
                        &[],
                    )?;
                    syntax &= checked.code == 0;
                    logs.extend(checked.stdout);
                    logs.extend(checked.stderr);
                }
            } else if file.ends_with(".py") {
                let bytes = storage::read(&scratch.path, Path::new(file), false)?;
                if let Some(valid) = c.services.check_python_syntax(Path::new(file), &bytes) {
                    syntax &= valid;
                    if !valid {
                        logs.extend(format!("{file}: python syntax error\n").into_bytes());
                    }
                }
            } else if file.ends_with(".json") {
                syntax &= V::parse(&storage::read(&scratch.path, Path::new(file), false)?).is_ok();
            }
        }
        gates.push(("syntax", syntax, "changed shell/JSON files parse".into()));
        let patch_text = String::from_utf8_lossy(&bytes);
        let assertion=regex::Regex::new(r"\bassert|\bexpect\s*\(|\bshould\b|\bfail\b|t\.(Error|Fatal)|\bok\s*\(|\brequire\s*\.|\bXCTAssert").unwrap();
        let (mut added, mut removed, mut tests, mut deleted) = (0usize, 0usize, false, false);
        let (mut lost, mut gained) = (false, false);
        for line in patch_text.lines() {
            if let Some((_, p)) = line
                .strip_prefix("diff --git ")
                .and_then(|v| v.split_once(" b/"))
            {
                tests = test_path(p);
            }
            if let Some(p) = line.strip_prefix("+++ b/") {
                tests = test_path(p);
            }
            if line == "+++ /dev/null" && tests {
                deleted = true;
            }
            if line.starts_with('+') && !line.starts_with("+++") {
                if tests && assertion.is_match(line) {
                    added += 1;
                }
                gained |= !line.is_ascii();
            } else if line.starts_with('-') && !line.starts_with("---") {
                if tests && assertion.is_match(line) {
                    removed += 1;
                }
                lost |= !line.is_ascii();
            }
        }
        gates.push((
            "tests",
            policy.allow_test_reduction || (!deleted && removed <= added),
            format!("{removed} assertion line(s) removed, {added} added"),
        ));
        gates.push((
            "encoding-loss",
            policy.accept_encoding_loss || !lost || gained,
            "patch hunk encoding preservation".into(),
        ));
        for file in &changed {
            if test_path(file)
                || policy.verify.contains(file)
                || [
                    "pytest.ini",
                    "tox.ini",
                    "pyproject.toml",
                    "package.json",
                    "Makefile",
                ]
                .contains(&file.as_str())
            {
                surface.push(file.clone());
            }
        }
        let verifier_hit = surface.iter().any(|f| {
            git.succeeds(&["cat-file", "-e", &format!("HEAD:{f}")])
                .unwrap_or(false)
        });
        gates.push((
            "verifier",
            policy.allow_verifier_change || !verifier_hit,
            "verification surfaces require explicit owner consent".into(),
        ));
        if gates.iter().all(|(_, ok, _)| *ok) {
            for prefix in batch_prefix.get("checks").array() {
                let command = prefix
                    .get("verify")
                    .text()
                    .ok_or_else(|| Error::new(2, "invalid batch prefix verifier"))?;
                let result = verify(c, &scratch.path, command, Duration::from_secs(1200));
                let rc = code(&result);
                logs.extend(output(&result));
                gates.push((
                    "batch-prefix",
                    rc == 0,
                    format!(
                        "earlier task {} verification exit {rc}",
                        string(prefix, "id")
                    ),
                ));
                if rc != 0 {
                    break;
                }
            }
        }
        if gates.iter().all(|(_, ok, _)| *ok) {
            if !policy.verify.is_empty() {
                let result = verify(c, &scratch.path, &policy.verify, Duration::from_secs(1200));
                let rc = code(&result);
                verification = "command".into();
                verify_exit = V::Num(rc.to_string());
                logs.extend(output(&result));
                gates.push((
                    "verify",
                    rc == 0,
                    format!("candidate verification exit {rc}"),
                ));
            }
            if !surface.is_empty() && !policy.verify.is_empty() {
                let floor = Sandbox::new(repo, &snapshot.head, c.interrupted)?;
                let floor_git = Git::new(&floor.path, c.interrupted);
                floor_git.apply(&bytes, false, false, true)?;
                let mut candidate_only = false;
                for file in &surface {
                    if git.succeeds(&["cat-file", "-e", &format!("HEAD:{file}")])? {
                        floor_git.run(&[
                            "restore",
                            "--source=HEAD",
                            "--staged",
                            "--worktree",
                            "--",
                            file,
                        ])?;
                    } else {
                        candidate_only = true;
                        if floor.path.join(file).exists() {
                            std::fs::remove_file(floor.path.join(file))?;
                        }
                    }
                }
                let floor_command = if candidate_only && !base_check.is_empty() {
                    "bash scripts/check.sh fast"
                } else {
                    &policy.verify
                };
                let result = verify(c, &floor.path, floor_command, Duration::from_secs(1200));
                let rc = code(&result);
                logs.extend(output(&result));
                gates.push((
                    "base-floor",
                    rc == 0 || !policy.allow_floor_change.is_empty(),
                    format!("unchanged baseline verification exit {rc}"),
                ));
            }
            if !policy.regression.is_empty() {
                let base = Sandbox::new(repo, &snapshot.head, c.interrupted)?;
                let tests: Vec<_> = changed.iter().filter(|p| test_path(p)).collect();
                if !tests.is_empty() {
                    let mut args: Vec<OsString> =
                        ["diff", "--cached", "--binary", "--no-renames", "HEAD", "--"]
                            .iter()
                            .map(OsString::from)
                            .collect();
                    args.extend(
                        tests
                            .iter()
                            .map(|s| OsString::from(format!(":(literal){s}"))),
                    );
                    let patch =
                        candidate.raw(&args.iter().map(OsString::as_os_str).collect::<Vec<_>>())?;
                    Git::new(&base.path, c.interrupted).apply(&patch, false, false, false)?;
                    let before =
                        verify(c, &base.path, &policy.regression, Duration::from_secs(1200));
                    let after = verify(
                        c,
                        &scratch.path,
                        &policy.regression,
                        Duration::from_secs(1200),
                    );
                    let (a, b) = (code(&before), code(&after));
                    gates.push((
                        "regression",
                        a > 0 && a < 124 && b == 0,
                        format!("base exit {a}, candidate exit {b}"),
                    ));
                } else {
                    gates.push((
                        "regression",
                        false,
                        "no patch tests can prove a regression".into(),
                    ));
                }
            }
        }
    }
    gates.push((
        "batch-prefix",
        domain.batch_prefix(
            &CaptureIndex::load(&state)?,
            repo,
            &policy.task,
            c.interrupted,
        )? == batch_prefix,
        "batch prefix stayed unchanged during admission".into(),
    ));
    gates.push((
        "source-integrity",
        git.snapshot()? == snapshot,
        "HEAD, tracked tree and index SHA256 stayed unchanged".into(),
    ));
    let verdict = gates.iter().all(|(_, ok, _)| *ok);
    let word = if verdict { "ADMIT" } else { "REJECT" };
    let mut body = format!(
        "# Patch admission: {word}\n\n- patch: {}\n- patch_sha: {}\n- base: {}\n- verification: {verification}\n- verify_sha256: {}\n- checked: {}\n\n## Ladder\n\n",
        path.display(),
        &digest(&bytes)[..16],
        &snapshot.head[..7],
        digest(policy.verify.as_bytes()),
        now()
    );
    for (name, ok, detail) in gates {
        body += &format!(
            "- {name}: {} — {detail}\n",
            if ok { "PASS" } else { "FAIL" }
        );
    }
    if !changed.is_empty() {
        body += &format!(
            "\n## Changed files\n\n{}\n",
            changed
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    if !policy.red_check.is_empty() {
        let request = crate::red::Request {
            repo,
            patch: &bytes,
            verify: &policy.red_check,
            base: &snapshot.head,
            timeout: Duration::from_secs(300),
            max_files: 8,
        };
        body += "\n## Red check (advisory, not admission evidence)\n\n";
        match crate::red::check(&request, c.services, c.interrupted) {
            Ok(result) => body += &crate::red::render(&result),
            Err(_) => body += "red check could not run; advisory, not admission evidence\n",
        }
    }
    if !logs.is_empty() {
        body += "\n## Verify output (tail)\n\n```\n";
        if safe(&logs).is_ok() {
            body += &String::from_utf8_lossy(&logs)
                .lines()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
        } else {
            body += "[output withheld by sensitive-content scanner]";
        }
        body += "\n```\n";
    }
    scope.healthy()?;
    require(
        git.snapshot()? == snapshot,
        "source changed after admission checks; no outcome witness published",
    )?;
    storage::write_external(&report, None, body.as_bytes(), c.interrupted)?;
    let report_rel = report
        .strip_prefix(&state)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| report.to_string_lossy().into_owned());
    let row = obj([
        ("schema", 1usize.into()),
        (
            "event_id",
            format!("art_{}", oms_command_plan::storage::random_hex(16)?).into(),
        ),
        ("ts", now().into()),
        ("kind", "patch-admit".into()),
        ("operation_id", scope.attempt.clone().into()),
        ("exit", usize::from(!verdict).into()),
        ("artifact", report_rel.into()),
        ("artifact_sha256", digest(body.as_bytes()).into()),
        ("patch_sha256", digest(&bytes).into()),
        (
            "patch_external",
            obj([
                (
                    "name",
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                ),
                ("owned", false.into()),
                ("sha256", digest(&bytes).into()),
            ]),
        ),
        ("base", snapshot.head.clone().into()),
        ("verification", verification.into()),
        ("verify_exit", verify_exit),
        (
            "coverage",
            obj([
                (
                    "covers",
                    V::Arr(policy.covers.iter().map(|s| s.as_str().into()).collect()),
                ),
                (
                    "status",
                    if verdict && !policy.verify.is_empty() {
                        "verified".into()
                    } else if verdict {
                        "inconclusive".into()
                    } else {
                        "failed".into()
                    },
                ),
                ("scope_digest", digest(&bytes).into()),
            ]),
        ),
    ]);
    storage::append(
        &state,
        Path::new(".oms/artifacts/index.jsonl"),
        &row,
        c.interrupted,
    )?;
    {
        let _guard = storage::lock(&state.join(".oms/artifacts/index.jsonl"), c.interrupted)?;
        oms_reference::capture::record_admission(
            &CaptureIndex::load(&state)?,
            &path,
            &scope.attempt,
            &report,
            if verdict { "0" } else { "1" },
        )?;
    }
    if !policy.covers.is_empty() {
        c.services.coverage(repo, &policy.covers, Some(&row))?;
    }
    scope.finish(c.interrupted)?;
    if keep {
        std::mem::forget(scratch);
    }
    Ok(Admission {
        verdict,
        report,
        record: row,
        patch: path,
        bytes,
        snapshot,
    })
}
fn executable(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(o.words.is_empty(), "unexpected patch-admit argument")?;
    let repo = c.repo(o)?;
    let path = o
        .path("patch", c.cwd)
        .ok_or_else(|| Error::new(2, "--patch is required"))?;
    let report = o.path("report", c.cwd);
    let result = admit(
        c,
        &repo,
        &path,
        report.as_deref(),
        &Policy::options(o),
        o.has("keep-worktree"),
    )?;
    Ok(Output {
        code: i32::from(!result.verdict),
        stdout: if result.verdict {
            b"ADMIT\n".to_vec()
        } else {
            b"REJECT\n".to_vec()
        },
        stderr: format!(
            "patch-admit: {} ({})\n",
            if result.verdict { "ADMIT" } else { "REJECT" },
            result.report.display()
        )
        .into_bytes(),
    })
}
