#![forbid(unsafe_code)]

use oms_command_autonomy::{CommandResult, Invocation, ModelRoute, Services, run_with_services};
use oms_command_plan::services::{GroupMembership, GroupResolver};
use oms_command_plan::storage::ACTIVE;
use oms_command_plan::{Action, Context, DelegateBinding, Plan, Request, json as plan_json};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

fn ambient(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("OMS_") || key.starts_with("OH_MY_SETTING_")
}
/// Plan and lock settings read OMS_* variables in-process, which cannot be
/// cleared under `forbid(unsafe_code)`: rerun in a scrubbed child.
fn scrubbed_child(name: &str) -> bool {
    if std::env::vars_os().all(|(k, _)| !ambient(&k)) {
        return false;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    if let Some(timeout) = std::env::var_os("OMS_TEST_CALL_TIMEOUT") {
        command.env("PARALLEL_TEST_CALL_TIMEOUT", timeout);
    }
    for (key, _) in std::env::vars_os().filter(|(k, _)| ambient(k)) {
        command.env_remove(key);
    }
    let out = command
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed;"), "child ran no test: {text}");
    true
}

struct Repo(PathBuf);
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// The invoking user is the sole member of every group (umask 002 hosts).
fn group(gid: u32) -> oms_command_plan::Result<GroupMembership> {
    let uid = rustix::process::getuid().as_raw();
    Ok(GroupMembership {
        gid,
        explicit_member_uids: vec![uid],
        primary_member_uids: vec![uid],
    })
}

fn call_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("PARALLEL_TEST_CALL_TIMEOUT")
            .or_else(|_| std::env::var("OMS_TEST_CALL_TIMEOUT"))
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120),
    )
}

#[derive(Default)]
struct ReviewRace {
    state: Mutex<ReviewRaceState>,
    wake: Condvar,
}

#[derive(Default)]
struct ReviewRaceState {
    reader_open: bool,
    resume_reader: bool,
}

impl ReviewRace {
    fn reader_opened(&self) {
        let mut state = self.state.lock().unwrap();
        state.reader_open = true;
        self.wake.notify_all();
        let (state, _) = self
            .wake
            .wait_timeout_while(state, call_timeout(), |s| !s.resume_reader)
            .unwrap();
        assert!(
            state.resume_reader,
            "sibling never released the Plan reader"
        );
    }

    fn reader_holds_lock(&self, repo: &Path) -> bool {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .wake
            .wait_timeout_while(state, call_timeout(), |s| !s.reader_open)
            .unwrap();
        assert!(state.reader_open, "reader never opened the Plan");
        drop(state);
        let lock =
            oms_lock::Lock::for_file(&repo.join(ACTIVE), oms_lock::Settings::from_env().unwrap())
                .unwrap()
                .try_acquire();
        match lock {
            Err(oms_lock::Error::Busy) => true,
            Ok(_) => false,
            Err(error) => panic!("could not check the canonical Plan lock: {error}"),
        }
    }

    fn resume_reader(&self) {
        self.state.lock().unwrap().resume_reader = true;
        self.wake.notify_all();
    }
}

#[derive(Default)]
struct SelectionRace {
    armed: AtomicBool,
    race: Arc<ReviewRace>,
}

thread_local! {
    static SELECTION_READER: RefCell<Option<(Arc<SelectionRace>, bool)>> = const { RefCell::new(None) };
    static RACE_READER: RefCell<Option<(Arc<ReviewRace>, bool)>> = const { RefCell::new(None) };
}

fn group_with_review_race(gid: u32) -> oms_command_plan::Result<GroupMembership> {
    // Only the Plan file is group-writable. Its second group proof occurs
    // after the Plan reader has opened the file and sampled its inode.
    SELECTION_READER.with(|reader| {
        let mut reader = reader.borrow_mut();
        if let Some((race, seen)) = reader.as_mut()
            && race.armed.load(Ordering::SeqCst)
        {
            if *seen {
                let race = reader.take().unwrap().0;
                race.race.reader_opened();
            } else {
                *seen = true;
            }
        }
    });
    let opened = RACE_READER.with(|reader| {
        let mut reader = reader.borrow_mut();
        match reader.as_mut() {
            Some((_, seen)) if !*seen => {
                *seen = true;
                None
            }
            _ => reader.take().map(|(race, _)| race),
        }
    });
    if let Some(race) = opened {
        race.reader_opened();
    }
    group(gid)
}

#[derive(Default)]
struct Mock {
    goal: bool,
    mutate_ready: bool,
    switch_ref: bool,
    mutation_done: AtomicBool,
    acceptance_after: usize,
    acceptances: AtomicUsize,
    landed: Mutex<Vec<(String, String)>>,
    /// Task ids whose worker panics / reports failure.
    panics: BTreeSet<&'static str>,
    /// Task ids whose first worker invocation returns a retryable failure with physical cleanup.
    retryable_once: BTreeSet<&'static str>,
    failed_once: Mutex<BTreeSet<String>>,
    /// `(waiter, target)` pairs where `waiter` waits until `target` has a returned worker call in `progress.jsonl`.
    wait_returned: Vec<(&'static str, &'static str)>,
    verified_criteria: bool,
    /// Task ids that wait here until the named task's worker has started.
    waits: Vec<(&'static str, &'static str)>,
    /// Every worker waits until this many workers have started.
    rendezvous: usize,
    started: Mutex<Vec<String>>,
    wake: Condvar,
    running: Mutex<BTreeSet<String>>,
    together: Mutex<Vec<(String, String)>>,
    peak: AtomicUsize,
    live: AtomicUsize,
    plan: Mutex<()>,
    invocations: Mutex<Vec<(String, Duration, Option<String>)>>,
    review_race: Option<Arc<ReviewRace>>,
    selection_race: Option<Arc<SelectionRace>>,
}
impl oms_command_plan::Services for Mock {
    fn supervise(
        &self,
        command: &oms_command_plan::services::SupervisedCommand,
    ) -> oms_command_plan::Result<oms_command_plan::services::Execution> {
        assert!(self.goal);
        self.acceptances.fetch_add(1, Ordering::SeqCst);
        let landed = self.landed.lock().unwrap().len();
        let exit = u8::from(landed < self.acceptance_after);
        let output = if self.verified_criteria {
            format!(
                "{} landed={landed}\n",
                command.argv.last().unwrap().to_string_lossy()
            )
            .into_bytes()
        } else {
            command
                .argv
                .last()
                .unwrap()
                .to_string_lossy()
                .as_bytes()
                .to_vec()
        };
        Ok(oms_command_plan::services::Execution {
            exit,
            runner_exit: exit,
            timed_out: false,
            output_limited: false,
            launch_error: false,
            supervision_error: false,
            output,
            duration_s: 0,
        })
    }
    fn group_membership_resolver(&self) -> Option<GroupResolver> {
        Some(
            if self.review_race.is_some() || self.selection_race.is_some() {
                group_with_review_race
            } else {
                group
            },
        )
    }
}
impl Mock {
    fn plan(&self, repo: &Path, args: &[&str]) -> (i32, String) {
        let _one = self.plan.lock().unwrap();
        let (code, out, err) = self.plan_command(repo, args);
        assert_eq!(code, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
        (code, String::from_utf8(out).unwrap())
    }
    fn plan_command(&self, repo: &Path, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        let mut full = vec!["--repo", repo.to_str().unwrap()];
        full.extend_from_slice(args);
        let words = full.iter().map(OsString::from).collect::<Vec<_>>();
        oms_command_plan::run_with_services(&words, repo, &AtomicBool::new(false), self)
    }
    fn delegate_lease(&self, repo: &Path, id: &str) -> String {
        let stop = AtomicBool::new(false);
        let req = Request::new(Action::Show).with("id", id);
        let context = Context::from_environment_at(&req, repo, repo, None).unwrap();
        let plan = Plan::open(context, self, &stop).unwrap();
        let shown = plan.show_task_with_locked_snapshot(id).unwrap();
        assert_eq!(shown.code, 0, "cannot read the mock delegate lease");
        let task: Value = serde_json::from_slice(&shown.stdout).unwrap();
        task["lease_id"].as_str().unwrap().to_owned()
    }
    fn wait_for(&self, predicate: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + call_timeout();
        let mut started = self.started.lock().unwrap();
        while !predicate(&started) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "worker rendezvous timed out: {started:?}");
            started = self.wake.wait_timeout(started, left).unwrap().0;
        }
    }
    fn review(
        &self,
        call: &Invocation,
        plan: &Plan<'_>,
        issued: &DelegateBinding,
        running: &DelegateBinding,
    ) {
        let _one = self.plan.lock().unwrap();
        let id = issued.task_id();
        let operation = call
            .environment
            .iter()
            .find_map(|(name, value)| (name == "OMS_OPERATION_ID").then_some(value))
            .and_then(|value| value.to_str())
            .expect("exact operation identity");
        let dir = call.cwd.join(".oms/artifacts");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let artifact = format!(".oms/artifacts/artifact-{id}.md");
        let patch = format!(".oms/artifacts/patch-{id}.patch");
        let report = b"fixture native review report\n";
        let changes = if self.goal {
            let task = plan.load().unwrap().task(id).unwrap().clone();
            let path = format!(
                "{}/{}.txt",
                task.get("allowed_paths").array().unwrap()[0]
                    .str()
                    .unwrap()
                    .trim_end_matches('/'),
                id
            );
            format!("diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{id}\n").into_bytes()
        } else {
            b"fixture native review patch\n".to_vec()
        };
        for (file, bytes) in [(&artifact, report.as_slice()), (&patch, changes.as_slice())] {
            let file = call.cwd.join(file);
            fs::write(&file, bytes).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let binding: Value = serde_json::from_slice(&issued.encode().unwrap()).unwrap();
        let index = json!({
            "schema": 1, "kind": "delegate", "event_id": format!("evt-{id}"),
            "attempt_id": format!("att-{id}"), "operation_id": operation,
            "task_id": id, "provider": "codex", "base_sha": issued.base_sha(),
            "exit": 0, "verify_exit": 0, "verification": "passed", "state": "review",
            "no_change": false, "plan_binding": binding, "artifact": artifact,
            "artifact_sha256": plan_json::sha(report), "patch": patch,
            "patch_sha256": plan_json::sha(&changes),
        });
        let raw = serde_json::to_vec(&index).unwrap();
        let index_path = dir.join("index.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_path)
            .unwrap();
        fs::set_permissions(&index_path, fs::Permissions::from_mode(0o600)).unwrap();
        file.write_all(&[raw.as_slice(), b"\n"].concat()).unwrap();
        plan.execute_bound_task(
            &Request::new(Action::Review)
                .with("id", id)
                .with("lease-id", running.lease_id())
                .with("artifact", artifact)
                .with("patch", patch),
            running,
            Some(&plan_json::parse(&raw).unwrap()),
        )
        .unwrap();
    }
    fn delegate(&self, call: &Invocation, stop: &AtomicBool) -> CommandResult {
        let args = call.args.iter().map(|a| a.to_string_lossy().into_owned());
        let args = args.collect::<Vec<_>>();
        let id = args
            .windows(2)
            .find(|w| w[0] == "--plan-task")
            .map(|w| w[1].clone())
            .unwrap();
        let issued = DelegateBinding::parse(
            args.windows(2).find(|w| w[0] == "--plan-binding").unwrap()[1].as_bytes(),
        )
        .unwrap();
        let retryable_fail = self.retryable_once.contains(id.as_str())
            && self.failed_once.lock().unwrap().insert(id.clone());
        let start = Request::new(Action::Start)
            .with("id", id.as_str())
            .with("lease-id", issued.lease_id());
        let context = Context::from_environment(&start, &call.cwd).unwrap();
        let plan = Plan::open(context, self, stop).unwrap();
        let running = (!self.panics.contains(id.as_str()) && !retryable_fail).then(|| {
            let _one = self.plan.lock().unwrap();
            plan.execute_bound_task(&start, &issued, None).unwrap()
        });

        {
            let mut running = self.running.lock().unwrap();
            for other in running.iter() {
                self.together
                    .lock()
                    .unwrap()
                    .push((other.clone(), id.clone()));
            }
            running.insert(id.clone());
        }
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        self.started.lock().unwrap().push(id.clone());
        self.wake.notify_all();
        if self.rendezvous > 0 {
            self.wait_for(|s| s.len() >= self.rendezvous);
        }
        if self.mutate_ready && id == "a" {
            let (_, raw) = self.plan(&call.cwd, &["evidence-snapshot", "--id", "c"]);
            let selected: Value = serde_json::from_str(&raw).unwrap();
            self.plan(
                &call.cwd,
                &[
                    "amend",
                    "--id",
                    "c",
                    "--expected-plan-sha256",
                    &plan_json::sha(fs::read(call.cwd.join(ACTIVE)).unwrap()),
                    "--expected-task-sha256",
                    selected["task_sha256"].as_str().unwrap(),
                    "--allowed",
                    "widened/",
                    "--reason",
                    "parent changed the pending task scope",
                ],
            );
            self.mutation_done.store(true, Ordering::SeqCst);
            self.wake.notify_all();
        }
        if self.switch_ref && id == "a" {
            git(&call.cwd, &["switch", "-q", "-c", "other-work"]);
            self.mutation_done.store(true, Ordering::SeqCst);
            self.wake.notify_all();
        }
        if (self.mutate_ready || self.switch_ref) && id == "b" {
            self.wait_for(|_| self.mutation_done.load(Ordering::SeqCst));
        }
        for (waiter, awaited) in &self.waits {
            if *waiter == id {
                self.wait_for(|s| s.iter().any(|t| t == awaited));
            }
        }
        if self.panics.contains(id.as_str()) {
            self.live.fetch_sub(1, Ordering::SeqCst);
            self.running.lock().unwrap().remove(&id);
            panic!("injected worker panic for {id}");
        }
        if retryable_fail {
            use oms_command_telemetry::store::{physical, usage};
            let operation = call
                .environment
                .iter()
                .rev()
                .find(|(k, _)| k == "OMS_OPERATION_ID")
                .unwrap()
                .1
                .to_str()
                .unwrap();
            let identity = usage::Identity {
                call_id: format!("call-{id}-{}", self.started.lock().unwrap().len()),
                operation_id: operation.into(),
                provider: "codex".into(),
                selected_model: Some("model-a".into()),
                task_id: Some(id.clone()),
                attempt_id: Some(format!("att-{id}-retryable")),
                ..Default::default()
            };
            let physical::Registration::New(mut receipt) = physical::begin(
                &call.cwd,
                identity,
                None,
                true,
                stop,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap() else {
                panic!("new physical telemetry");
            };
            receipt.revision += 1;
            receipt.phase = usage::Phase::Unknown;
            receipt.cleanup_complete = Some(true);
            receipt.source_sha256 = Some(plan_json::sha(b"rate limit\n"));
            usage::record(&call.cwd, &receipt, stop).unwrap();
            self.live.fetch_sub(1, Ordering::SeqCst);
            self.running.lock().unwrap().remove(&id);
            self.wake.notify_all();
            return (2, Vec::new(), b"rate limit\n".to_vec());
        }
        if self.review_race.is_none() {
            std::thread::sleep(Duration::from_millis(30));
        }
        let mut free_lock = None;
        if let Some(race) = &self.review_race
            && id == "t1-slow"
        {
            // Under the guard, let the reader finish before Review can
            // publish. Without the guard, publish first to force inode drift.
            if race.reader_holds_lock(&call.cwd) {
                race.resume_reader();
            } else {
                free_lock = Some(race);
            }
        }
        if let Some(race) = &self.selection_race
            && id == "r2"
        {
            if race.race.reader_holds_lock(&call.cwd) {
                race.race.resume_reader();
            } else {
                free_lock = Some(&race.race);
            }
        }
        self.review(call, &plan, &issued, &running.unwrap());
        if let Some(race) = &self.selection_race
            && id == "r1"
        {
            race.armed.store(true, Ordering::SeqCst);
        }
        if let Some(race) = free_lock {
            race.resume_reader();
        }
        if let Some(race) = &self.review_race
            && id == "t3-late"
        {
            RACE_READER.with(|reader| reader.replace(Some((race.clone(), false))));
        }
        for (waiter, target) in &self.wait_returned {
            if *waiter == id {
                let deadline = Instant::now() + call_timeout();
                let progress = call.cwd.join(".oms/plan/progress.jsonl");
                loop {
                    let has_returned = fs::read_to_string(&progress).is_ok_and(|raw| {
                        raw.lines().any(|line| {
                            serde_json::from_str::<Value>(line).is_ok_and(|row| {
                                row["kind"] == "autopilot-worker-call"
                                    && row["task_id"] == *target
                                    && row["phase"] == "returned"
                            })
                        })
                    });
                    if has_returned {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {target} returned call"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        self.live.fetch_sub(1, Ordering::SeqCst);
        self.running.lock().unwrap().remove(&id);
        (0, Vec::new(), Vec::new())
    }
    fn land(&self, call: &Invocation) -> CommandResult {
        let _one = self.plan.lock().unwrap();
        let encoded = call
            .args
            .windows(2)
            .find(|args| args[0] == "--expected-plan-review")
            .unwrap()[1]
            .to_str()
            .unwrap();
        let proof: Value = serde_json::from_str(encoded).unwrap();
        let binding = &proof["binding"];
        let id = binding["task_id"].as_str().unwrap();
        let lease = binding["lease_id"].as_str().unwrap();
        let patch_sha = proof["patch_sha256"].as_str().unwrap();
        let stop = AtomicBool::new(false);
        let req = Request::new(Action::Show).with("id", id);
        let plan = Plan::open(
            Context::from_environment(&req, &call.cwd).unwrap(),
            self,
            &stop,
        )
        .unwrap();
        let snapshot = plan.load().unwrap();
        let task = snapshot.task(id).unwrap();
        let patch = task.text("patch").to_owned();
        let land = Request::new(Action::Land)
            .with("id", id)
            .with("lease-id", lease)
            .with("expected-review-lease-id", lease)
            .with("expected-review-patch", patch.as_str())
            .with("expected-review-patch-sha256", patch_sha)
            .with("expected-review-verify", task.text("verify"))
            .with("expected-review-executor-id", "")
            .with("expected-review-executor-soul-sha256", "")
            .with("expected-review-binding", encoded);
        assert_eq!(plan.execute(&land).unwrap().code, 0);
        git(&call.cwd, &["apply", &patch]);
        let snapshot = plan.load().unwrap();
        let landing_task = snapshot.task(id).unwrap();
        let frozen = call
            .cwd
            .join(format!(".oms/landing-patches/land-{id}.patch"));
        fs::create_dir_all(frozen.parent().unwrap()).unwrap();
        fs::write(&frozen, fs::read(&patch).unwrap()).unwrap();
        fs::set_permissions(&frozen, fs::Permissions::from_mode(0o600)).unwrap();
        let mut done_task = landing_task.clone();
        done_task
            .set("patch", plan_json::string(frozen.to_str().unwrap()))
            .unwrap();
        let receipt_sha = plan_json::receipt_digest(landing_task).unwrap();
        let done_sha = plan_json::receipt_digest(&done_task).unwrap();
        let finish = Request::new(Action::Finish)
            .with("id", id)
            .with("lease-id", lease)
            .with("patch", frozen.as_os_str())
            .with("expected-landing-receipt-sha256", &receipt_sha)
            .with("expected-review-binding", encoded)
            .with("expected-landing-done-receipt-sha256", &done_sha);
        assert_eq!(plan.execute(&finish).unwrap().code, 0);
        let mut intent = json!({"schema":1,"ts":"2026-11-28T00:00:00Z","landing_id":format!("land-{id}"),
            "event":"intent","patch":frozen,"patch_sha":patch_sha,"base_sha":binding["base_sha"],
            "task":id,"lease":lease,"plan_id":snapshot.plan.get("plan_id").str().unwrap(),
            "plan_receipt_sha":receipt_sha,"plan_done_receipt_sha":done_sha,
            "approval":"","approval_version":"","receipt_sha":"","worktree":call.cwd,"plan_review_binding":proof});
        intent["receipt_sha"] = json!(
            oms_command_plan::patch_landing_receipt_digest(
                &plan_json::parse(&serde_json::to_vec(&intent).unwrap()).unwrap()
            )
            .unwrap()
        );
        let mut complete = intent.clone();
        complete["event"] = json!("complete");
        let path = call.cwd.join(".oms/landings.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        writeln!(file, "{intent}\n{complete}").unwrap();
        self.landed.lock().unwrap().push((
            id.to_owned(),
            binding["base_sha"].as_str().unwrap().to_owned(),
        ));
        (0, b"fixture patch admitted\n".to_vec(), Vec::new())
    }
}
impl Services for Mock {
    fn classify_autonomy_failure(&self, text: &str, _: i32) -> oms_command_autonomy::Result<Value> {
        Ok(if text.contains("rate limit") {
            json!({"code":"provider_capacity","recovery":"one_explicit_fallback","retryable":true})
        } else {
            json!({"code":"unknown_failure","recovery":"preserve_and_escalate","retryable":false})
        })
    }
    fn discovery_section(
        &self,
        repo: &Path,
        section: &str,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<Value> {
        if !self.verified_criteria || section != "acceptance" {
            return Err(oms_command_autonomy::Error::new(
                2,
                "native discovery projection service unavailable",
            ));
        }
        let landed = self.landed.lock().unwrap().len();
        let raw = fs::read(repo.join("PROJECT.md")).unwrap();
        let parsed = oms_command_autonomy::contract::parse(&raw, repo)?;
        let criteria = parsed
            .criteria
            .into_iter()
            .map(|(id, statement)| {
                let digest = plan_json::sha(format!("project\0{statement}").as_bytes());
                let status = if landed >= self.acceptance_after {
                    "verified"
                } else {
                    "unverified"
                };
                let evidence = if landed >= self.acceptance_after {
                    vec![json!({"status": "verified", "stale": false, "signal": "execution_feedback"})]
                } else {
                    Vec::new()
                };
                json!({
                    "id": id,
                    "source": "project",
                    "criterion_digest": digest,
                    "status": status,
                    "evidence": evidence
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"criteria": criteria}))
    }
    fn state_repository(
        &self,
        work: &Path,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<PathBuf> {
        Ok(work.to_owned())
    }
    fn resolve_route(
        &self,
        _: &Path,
        _: &Value,
        _: &str,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<ModelRoute> {
        Ok(ModelRoute {
            provider: "codex".into(),
            model_class: "routine".into(),
            primary: "model-a".into(),
            fallback: "model-b".into(),
            effort: "auto".into(),
            fallback_effort: String::new(),
        })
    }
    fn invoke(
        &self,
        command: &Invocation,
        stop: &AtomicBool,
    ) -> oms_command_autonomy::Result<CommandResult> {
        let peer_timeout = command
            .environment
            .iter()
            .find_map(|(k, v)| (k == "OMS_PEER_TIMEOUT").then(|| v.to_string_lossy().into_owned()));
        self.invocations.lock().unwrap().push((
            command.tool.clone(),
            command.wall,
            peer_timeout.clone(),
        ));
        if command.tool == "peer-delegate" {
            if std::env::var_os("OMS_PEER_TIMEOUT").is_none() {
                assert!(peer_timeout.is_none(), "{:?}", command.environment);
                assert_eq!(command.wall, Duration::from_secs(86_400 + 60));
            }
            return Ok(self.delegate(command, stop));
        } else if command.tool == "patch-land" {
            assert!(peer_timeout.is_none(), "{:?}", command.environment);
            assert_eq!(command.wall, Duration::from_secs(1200));
            if self.goal {
                return Ok(self.land(command));
            }
            return Ok((1, Vec::new(), b"fixture admission hold".to_vec()));
        }
        Ok((0, Vec::new(), Vec::new()))
    }
}

fn fixture(tasks: &[(&str, &str)]) -> Repo {
    fixture_with_acceptance(tasks, None)
}

fn fixture_with_acceptance(tasks: &[(&str, &str)], acceptance: Option<&str>) -> Repo {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "oms-parallel-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    let repo = Repo(fs::canonicalize(&path).unwrap());
    git(&repo.0, &["init", "-q"]);
    fs::write(repo.0.join("seed.txt"), "x").unwrap();
    git(&repo.0, &["add", "."]);
    git(&repo.0, &["commit", "-qm", "seed"]);
    let mock = Mock::default();
    let mut init = vec!["init", "--goal", "parallel"];
    if let Some(command) = acceptance {
        init.extend(["--accept", command]);
    }
    mock.plan(&repo.0, &init);
    fs::write(
        repo.0.join(".oms/fixture-brief"),
        "Implement the scoped task.",
    )
    .unwrap();
    // Ids are chosen so the plan's id-tiebroken creation order equals `tasks`.
    for (id, allowed) in tasks {
        mock.plan(
            &repo.0,
            &[
                "add",
                "--id",
                id,
                "--title",
                id,
                "--allowed",
                allowed,
                "--verify",
                "true",
                "--brief-file",
                ".oms/fixture-brief",
            ],
        );
    }
    repo
}
fn run(repo: &Repo, mock: &Mock, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec!["--to", "codex", "--repo", repo.0.to_str().unwrap()];
    args.extend_from_slice(extra);
    let (code, out, err) = run_with_services(
        "plan-run",
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &repo.0,
        &AtomicBool::new(false),
        mock,
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}
fn state(repo: &Repo, id: &str) -> Value {
    let mock = Mock::default();
    let (_, shown) = mock.plan(&repo.0, &["show", "--id", id]);
    serde_json::from_str(&shown).unwrap()
}

fn goal_fixture(tasks: &[(&str, &str)]) -> Repo {
    let repo = fixture_with_acceptance(tasks, Some("fixture acceptance"));
    git(&repo.0, &["branch", "-M", "main"]);
    git(&repo.0, &["config", "--local", "user.name", "fixture"]);
    git(
        &repo.0,
        &["config", "--local", "user.email", "fixture@example.invalid"],
    );
    fs::write(repo.0.join(".git/info/exclude"), ".oms/\n").unwrap();
    repo
}

fn drive(repo: &Repo, mock: &Mock, run_id: &str, max: &str, writers: &str) -> (i32, String) {
    let (code, out, err) = run_with_services(
        "goal-drive",
        &[
            "--run-id",
            run_id,
            "--max-cycles",
            max,
            "--max-writers",
            writers,
        ]
        .map(OsString::from),
        &repo.0,
        &AtomicBool::new(false),
        mock,
    );
    (
        code,
        format!(
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        ),
    )
}

fn reservations(repo: &Repo) -> Vec<Value> {
    fs::read_to_string(repo.0.join(".oms/plan/progress.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["kind"] == "goal-cycle")
        .collect()
}

#[test]
fn goal_drive_closes_reviewed_work_before_success_and_preserves_lifetime_budget() {
    if scrubbed_child(
        "goal_drive_closes_reviewed_work_before_success_and_preserves_lifetime_budget",
    ) {
        return;
    }
    let repo = goal_fixture(&[("a", "a/"), ("b", "b/"), ("c", "c/")]);
    let mock = Mock {
        goal: true,
        acceptance_after: 1,
        rendezvous: 2,
        ..Mock::default()
    };
    let (code, out) = drive(&repo, &mock, "bounded", "2", "2");
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("cycles-exhausted"), "{out}");
    assert_eq!(mock.started.lock().unwrap().len(), 2);
    assert_eq!(mock.landed.lock().unwrap().len(), 2);
    assert_eq!(state(&repo, "a")["state"], "done");
    assert_eq!(state(&repo, "b")["state"], "done");
    assert_eq!(state(&repo, "c")["state"], "ready");
    assert_eq!(reservations(&repo).len(), 2);
    let (code, out) = drive(&repo, &mock, "bounded", "2", "2");
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("cycles-exhausted"), "{out}");
    for (max, writers) in [("3", "2"), ("2", "3")] {
        let (code, out) = drive(&repo, &mock, "bounded", max, writers);
        assert_ne!(code, 0, "{out}");
        assert!(out.contains("frozen goal run"), "{out}");
    }
    assert_eq!(mock.started.lock().unwrap().len(), 2);
    assert_eq!(reservations(&repo).len(), 2);
}

#[test]
fn goal_drive_parallel_windows_hold_review_scopes_until_serial_integration() {
    if scrubbed_child("goal_drive_parallel_windows_hold_review_scopes_until_serial_integration") {
        return;
    }
    let repo = goal_fixture(&[("a", "src/"), ("b", "src/"), ("c", "docs/")]);
    let mock = Mock {
        goal: true,
        acceptance_after: 1,
        rendezvous: 2,
        ..Mock::default()
    };
    let (code, out) = drive(&repo, &mock, "complete", "3", "2");
    assert_eq!(code, 0, "{out}");
    for id in ["a", "b", "c"] {
        assert_eq!(state(&repo, id)["state"], "done", "{out}");
    }
    assert_eq!(mock.peak.load(Ordering::SeqCst), 2);
    let landed = mock.landed.lock().unwrap();
    assert_eq!(
        landed.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        ["a", "c", "b"]
    );
    assert!(landed.windows(2).all(|pair| pair[0].1 != pair[1].1));
    assert!(
        !mock
            .together
            .lock()
            .unwrap()
            .iter()
            .any(|(a, b)| a == "a" && b == "b" || a == "b" && b == "a")
    );
    assert_eq!(reservations(&repo).len(), 3);
    assert!(mock.acceptances.load(Ordering::SeqCst) >= 4);
}

#[test]
fn goal_drive_drains_external_reviews_without_spending_new_worker_attempts() {
    if scrubbed_child("goal_drive_drains_external_reviews_without_spending_new_worker_attempts") {
        return;
    }
    let repo = goal_fixture(&[("a", "a/"), ("b", "b/")]);
    let mock = Mock {
        goal: true,
        acceptance_after: 1,
        rendezvous: 2,
        ..Mock::default()
    };
    let (code, out, err) = run(&repo, &mock, &["--parallel", "--max-writers", "2"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out) = drive(&repo, &mock, "drain", "1", "1");
    assert_eq!(code, 0, "{out}");
    assert_eq!(mock.started.lock().unwrap().len(), 2);
    assert_eq!(mock.landed.lock().unwrap().len(), 2);
    assert!(reservations(&repo).is_empty());
}

fn autopilot_parallel_fixture(tasks: &[(&str, &str, Option<&str>)]) -> Repo {
    let bare = tasks
        .iter()
        .map(|(id, allowed, _)| (*id, *allowed))
        .collect::<Vec<_>>();
    let repo = goal_fixture(&bare);
    for (id, _, resources) in tasks {
        if let Some(res) = resources {
            let mock = Mock::default();
            let (_, raw) = mock.plan(&repo.0, &["evidence-snapshot", "--id", id]);
            let snap: Value = serde_json::from_str(&raw).unwrap();
            mock.plan(
                &repo.0,
                &[
                    "amend",
                    "--id",
                    id,
                    "--expected-plan-sha256",
                    &plan_json::sha(fs::read(repo.0.join(ACTIVE)).unwrap()),
                    "--expected-task-sha256",
                    snap["task_sha256"].as_str().unwrap(),
                    "--resources",
                    res,
                    "--reason",
                    "attach task resource",
                ],
            );
        }
    }
    fs::create_dir_all(repo.0.join("tests")).unwrap();
    fs::write(repo.0.join("tests/check.sh"), "exit 1\n").unwrap();
    fs::write(
        repo.0.join("PROJECT.md"),
        "# Project\n\n## Status\n- State: confirmed\n\n## Goal\n- Goal: Parallel retry\n\n## Scope\n- Allowed: a, b, c\n\n## Acceptance\n- Parallel tasks complete cleanly\n\n## Verification\n- Required checks: bash tests/check.sh\n- Required check files: tests/check.sh\n",
    )
    .unwrap();
    git(&repo.0, &["add", "PROJECT.md", "tests/check.sh"]);
    git(&repo.0, &["commit", "-qm", "project contract"]);
    let spec = plan_json::sha(fs::read(repo.0.join("PROJECT.md")).unwrap());
    let base_out = Command::new("git")
        .args(["-C", repo.0.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    let base_sha = String::from_utf8(base_out.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let branch = format!("oms/autopilot-{}", &spec[..12]);
    git(&repo.0, &["switch", "-q", "-c", &branch]);
    let proposal = serde_json::to_vec_pretty(&json!({
        "tasks": tasks.iter().map(|(id, ..)| json!({"id": id})).collect::<Vec<_>>()
    }))
    .unwrap();
    let proposal_sha = plan_json::sha(&proposal);
    let proposal_path = repo
        .0
        .join(".oms/plan")
        .join(format!("approved-proposal-{proposal_sha}.json"));
    fs::write(&proposal_path, &proposal).unwrap();
    fs::set_permissions(&proposal_path, fs::Permissions::from_mode(0o600)).unwrap();
    let route = |model: &str, timeout: &str| json!({"model": model, "fallback_model": "", "reasoning_effort": "auto", "timeout": timeout});
    let receipt = json!({
        "schema": 1,
        "kind": "autopilot-run",
        "stage": "approved",
        "repo": repo.0,
        "spec_sha256": spec,
        "providers": {"planner": "claude", "worker": "codex", "reviewer": "claude"},
        "routing": {
            "provider_timeout": "",
            "planner": route("", ""),
            "worker": route("model-a", ""),
            "reviewer": route("", "")
        },
        "contract": {
            "allowed": "a,b,c",
            "base": "main",
            "base_sha": base_sha,
            "remote": "origin",
            "max_cycles": 5,
            "max_writers": 2,
            "initial_tasks": 6,
            "replan_tasks": 2,
            "auto_repair": false,
            "retry_known": true,
            "review_mode": "off",
            "draft_pr": false,
            "allow_verifier_change": false,
            "allow_floor_change": "",
            "collaboration": "off"
        },
        "proposal": {"path": proposal_path, "sha256": proposal_sha},
        "branch": branch,
        "owner_id": format!("owner_{}", "1".repeat(32)),
        "updated": "2026-11-29T00:00:00Z"
    });
    let receipt_path = repo.0.join(".oms/plan/autopilot-run.json");
    fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o600)).unwrap();
    repo
}

fn autopilot_cmd(repo: &Repo, mock: &Mock, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec![];
    args.extend_from_slice(extra);
    args.extend(["--repo", repo.0.to_str().unwrap()]);
    let (code, out, err) = run_with_services(
        "autopilot",
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &repo.0,
        &AtomicBool::new(false),
        mock,
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[test]
fn goal_drive_failed_parallel_call_keeps_spent_reservations_and_healthy_results() {
    if scrubbed_child(
        "goal_drive_failed_parallel_call_keeps_spent_reservations_and_healthy_results",
    ) {
        return;
    }
    let repo = goal_fixture(&[("a", "a/"), ("b", "b/"), ("c", "c/")]);
    let mock = Mock {
        goal: true,
        acceptance_after: 1,
        rendezvous: 2,
        panics: BTreeSet::from(["a"]),
        ..Mock::default()
    };
    let (code, out) = drive(&repo, &mock, "failed", "2", "2");
    assert_eq!(code, 3, "{out}");
    assert_eq!(state(&repo, "b")["state"], "done", "{out}");
    assert_eq!(state(&repo, "a")["state"], "ready");
    assert_eq!(state(&repo, "c")["state"], "ready");
    assert_eq!(reservations(&repo).len(), 2);
    let (code, out) = drive(&repo, &mock, "failed", "2", "2");
    assert_eq!(code, 3, "{out}");
    assert_eq!(mock.started.lock().unwrap().len(), 2);

    // Positive end-to-end autopilot case (`max_cycles=5`, `retry_known=true`):
    // sibling `a` fails retryably, sibling `b` finishes later and commits
    // disjointly (`HEAD` advances), and `autopilot` resumes `a` then finishes `c`.
    let auto_repo =
        autopilot_parallel_fixture(&[("a", "a/", None), ("b", "b/", None), ("c", "c/", None)]);
    let auto_mock = Mock {
        goal: true,
        acceptance_after: 3,
        rendezvous: 2,
        retryable_once: BTreeSet::from(["a"]),
        wait_returned: vec![("b", "a")],
        verified_criteria: true,
        ..Mock::default()
    };
    let (code, out, err) = autopilot_cmd(&auto_repo, &auto_mock, &["run"]);
    assert_eq!(code, 3, "{out}{err}");
    assert_eq!(state(&auto_repo, "b")["state"], "done", "{out}{err}");
    assert_eq!(state(&auto_repo, "a")["state"], "ready");
    assert_eq!(state(&auto_repo, "c")["state"], "ready");

    let stop = AtomicBool::new(false);
    let obs =
        oms_command_autonomy::adaptive::observe(&auto_repo.0, &auto_repo.0, &auto_mock, &stop)
            .unwrap();
    assert_eq!(obs["action"], "resume", "{obs}");
    assert_eq!(obs["retry"]["task_id"], "a", "{obs}");
    assert_ne!(obs["retry"]["call"]["head"], obs["head"], "{obs}");

    let sha = obs["decision_sha256"].as_str().unwrap();
    let (code, out, err) = autopilot_cmd(
        &auto_repo,
        &auto_mock,
        &[
            "decide",
            "--expected-decision-sha256",
            sha,
            "--action",
            "resume",
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = autopilot_cmd(&auto_repo, &auto_mock, &["run"]);
    assert_eq!(code, 0, "{out}{err}");
    for id in ["a", "b", "c"] {
        assert_eq!(state(&auto_repo, id)["state"], "done", "{id}: {out}{err}");
    }

    // Negative fail-closed case: foreign commit after disjoint sibling commit
    // strands `autopilot` in `inspect` and refuses `resume`.
    let foreign_repo =
        autopilot_parallel_fixture(&[("a", "a/", None), ("b", "b/", None), ("c", "c/", None)]);
    let foreign_mock = Mock {
        goal: true,
        acceptance_after: 3,
        rendezvous: 2,
        retryable_once: BTreeSet::from(["a"]),
        wait_returned: vec![("b", "a")],
        verified_criteria: true,
        ..Mock::default()
    };
    let (code, out, err) = autopilot_cmd(&foreign_repo, &foreign_mock, &["run"]);
    assert_eq!(code, 3, "{out}{err}");
    fs::create_dir_all(foreign_repo.0.join("c")).unwrap();
    fs::write(
        foreign_repo.0.join("c/foreign.txt"),
        b"untracked provenance\n",
    )
    .unwrap();
    git(&foreign_repo.0, &["add", "c/foreign.txt"]);
    git(&foreign_repo.0, &["commit", "-qm", "foreign"]);
    let foreign_obs = oms_command_autonomy::adaptive::observe(
        &foreign_repo.0,
        &foreign_repo.0,
        &foreign_mock,
        &stop,
    )
    .unwrap();
    assert_eq!(foreign_obs["action"], "inspect", "{foreign_obs}");
    assert!(
        !foreign_obs["allowed_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "resume"),
        "{foreign_obs}"
    );
}

#[test]
fn goal_drive_refill_refuses_scope_changed_during_the_frozen_run() {
    if scrubbed_child("goal_drive_refill_refuses_scope_changed_during_the_frozen_run") {
        return;
    }
    let repo = goal_fixture(&[("a", "a/"), ("b", "b/"), ("c", "c/")]);
    let mock = Mock {
        goal: true,
        mutate_ready: true,
        acceptance_after: 3,
        rendezvous: 2,
        ..Mock::default()
    };
    let (code, out) = drive(&repo, &mock, "frozen", "3", "2");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("frozen goal run task"), "{out}");
    assert_eq!(mock.started.lock().unwrap().len(), 2);
    assert_eq!(state(&repo, "c")["state"], "ready");
    assert_eq!(reservations(&repo).len(), 2);
    for id in ["a", "b"] {
        assert_eq!(state(&repo, id)["state"], "review");
    }
}

#[test]
fn goal_drive_refuses_same_head_branch_switch_without_an_explicit_ref_flag() {
    if scrubbed_child("goal_drive_refuses_same_head_branch_switch_without_an_explicit_ref_flag") {
        return;
    }
    for tasks in [
        vec![("a", "a/"), ("b", "b/")],
        vec![("a", "a/"), ("b", "b/"), ("c", "c/")],
    ] {
        let repo = goal_fixture(&tasks);
        let mock = Mock {
            goal: true,
            switch_ref: true,
            acceptance_after: tasks.len(),
            rendezvous: 2,
            ..Mock::default()
        };
        let (code, out) = drive(&repo, &mock, "branch-bound", "3", "2");
        assert_ne!(code, 0, "{out}");
        assert!(
            out.contains("expected work ref changed") || out.contains("frozen goal run task"),
            "{out}"
        );
        assert_eq!(mock.started.lock().unwrap().len(), 2);
        assert!(mock.landed.lock().unwrap().is_empty());
        assert_eq!(reservations(&repo).len(), 2);
        for id in ["a", "b"] {
            assert_eq!(state(&repo, id)["state"], "review");
        }
        if tasks.len() == 3 {
            assert_eq!(state(&repo, "c")["state"], "ready");
        }
    }
}

#[test]
fn parallel_skips_tasks_without_briefs() {
    if scrubbed_child("parallel_skips_tasks_without_briefs") {
        return;
    }
    let repo = fixture(&[("t2-briefed", "b/")]);
    let mock = Mock::default();
    mock.plan(
        &repo.0,
        &[
            "add",
            "--id",
            "t1-unbriefed",
            "--title",
            "missing",
            "--allowed",
            "a/",
            "--verify",
            "true",
        ],
    );
    let (_, out, err) = run(&repo, &mock, &["--parallel"]);
    assert!(out.contains("started=1"), "{out}{err}");
    assert_eq!(state(&repo, "t1-unbriefed")["state"], "ready");
    assert_ne!(state(&repo, "t2-briefed")["state"], "ready");
}

#[test]
fn parallel_refills_slots_without_barrier_and_isolates_a_panicking_sibling() {
    if scrubbed_child("parallel_refills_slots_without_barrier_and_isolates_a_panicking_sibling") {
        return;
    }
    let repo = fixture(&[("t1-slow", "a/"), ("t2-boom", "b/"), ("t3-late", "c/")]);
    for relative in ["", ".oms", ".oms/plan"] {
        fs::set_permissions(repo.0.join(relative), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(repo.0.join(ACTIVE), fs::Permissions::from_mode(0o660)).unwrap();
    let mock = Mock {
        panics: ["t2-boom"].into(),
        // The slow task cannot finish until the third task has started, so the
        // freed slot must be refilled while the slow sibling is still running.
        waits: vec![("t1-slow", "t3-late")],
        review_race: Some(Arc::new(ReviewRace::default())),
        ..Mock::default()
    };
    let (code, out, err) = run(&repo, &mock, &["--parallel", "--max-writers", "2"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        out.contains("task=t2-boom panicked; unstarted claim released"),
        "{out}"
    );
    assert!(out.contains("started=3 failed=1"), "{out}");
    for ok in ["t1-slow", "t3-late"] {
        let task = state(&repo, ok);
        assert_eq!(task["state"], "review", "{ok}: {task}");
        assert_eq!(task["delegate_result"]["kind"], "native-delegate-review");
    }
    let boom = state(&repo, "t2-boom");
    assert_eq!(boom["state"], "ready", "{boom}");
    assert_eq!(boom["lease_id"], "");
    assert_eq!(mock.peak.load(Ordering::SeqCst), 2);
    // A ready task is never started twice in one run.
    let started = mock.started.lock().unwrap().clone();
    assert_eq!(started.len(), 3, "{started:?}");
}

#[test]
fn overlapping_scopes_never_run_together() {
    if scrubbed_child("overlapping_scopes_never_run_together") {
        return;
    }
    let repo = fixture(&[("o1", "src/"), ("o2", "src/lib.rs"), ("o3", "docs/")]);
    let mock = Mock::default();
    let (code, out, err) = run(&repo, &mock, &["--parallel", "--max-writers", "3"]);
    assert_eq!(code, 0, "{out}{err}");
    let together = mock.together.lock().unwrap().clone();
    let pair = |a: &str, b: &str| {
        together
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
    };
    assert!(!pair("o1", "o2"), "{together:?}");
    for id in ["o1", "o2", "o3"] {
        assert_eq!(state(&repo, id)["state"], "review");
    }
}

#[test]
fn delegate_lease_read_cannot_race_controller_publication() {
    if scrubbed_child("delegate_lease_read_cannot_race_controller_publication") {
        return;
    }
    let repo = fixture(&[("o2", "src/lib.rs"), ("o3", "docs/")]);
    let race = Arc::new(ReviewRace::default());
    let mock = Mock {
        review_race: Some(race.clone()),
        ..Mock::default()
    };
    mock.plan(&repo.0, &["claim", "--id", "o3", "--provider", "codex"]);
    let expected = state(&repo, "o3")["lease_id"].as_str().unwrap().to_owned();
    assert!(!expected.is_empty());
    for relative in ["", ".oms", ".oms/plan"] {
        fs::set_permissions(repo.0.join(relative), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(repo.0.join(ACTIVE), fs::Permissions::from_mode(0o660)).unwrap();

    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            RACE_READER.with(|reader| reader.replace(Some((race.clone(), false))));
            mock.delegate_lease(&repo.0, "o3")
        });
        let guarded = race.reader_holds_lock(&repo.0);
        if guarded {
            race.resume_reader();
        }
        // The controller does not share the mock delegate's command mutex.
        let (code, _, err) =
            mock.plan_command(&repo.0, &["claim", "--id", "o2", "--provider", "codex"]);
        if !guarded {
            race.resume_reader();
        }
        assert_eq!(
            code,
            0,
            "controller claim: {}",
            String::from_utf8_lossy(&err)
        );
        assert_eq!(reader.join().unwrap(), expected);
        assert!(
            guarded,
            "mock delegate read must hold the canonical Plan lock"
        );
    });
    assert_eq!(state(&repo, "o2")["state"], "claimed");
}

#[test]
fn review_backlog_stops_claims_drains_in_flight_and_exits_three() {
    if scrubbed_child("review_backlog_stops_claims_drains_in_flight_and_exits_three") {
        return;
    }
    let repo = fixture(&[("r1", "a/"), ("r2", "b/"), ("r3", "c/")]);
    // Both writers start before either can reach review, so the backlog limit
    // is first observed only after the first review lands.
    for relative in ["", ".oms", ".oms/plan"] {
        fs::set_permissions(repo.0.join(relative), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(repo.0.join(ACTIVE), fs::Permissions::from_mode(0o660)).unwrap();
    let race = Arc::new(SelectionRace::default());
    let mock = Mock {
        rendezvous: 2,
        selection_race: Some(race.clone()),
        ..Mock::default()
    };
    SELECTION_READER.with(|reader| reader.replace(Some((race, false))));
    let (code, out, err) = run(
        &repo,
        &mock,
        &[
            "--parallel",
            "--max-writers",
            "2",
            "--max-review-backlog",
            "1",
        ],
    );
    assert_eq!(code, 3, "{out}{err}");
    assert!(out.contains("backpressure=review-backlog"), "{out}");
    assert!(out.contains("started=2 failed=0"), "{out}");
    assert_eq!(state(&repo, "r1")["state"], "review");
    assert_eq!(state(&repo, "r2")["state"], "review");
    assert_eq!(state(&repo, "r3")["state"], "ready");
    assert_eq!(mock.started.lock().unwrap().len(), 2);
}

#[test]
fn one_controller_per_state_repository_and_flag_validation() {
    const NAME: &str = "one_controller_per_state_repository_and_flag_validation";
    if std::env::var("PARALLEL_PINNED_TIMEOUT_SUBTEST").as_deref() == Ok("75s") {
        assert_eq!(std::env::var("OMS_PEER_TIMEOUT").as_deref(), Ok("75s"));
        let repo = fixture(&[("p1", "a/"), ("p2", "b/")]);
        let mock = Mock::default();
        let (code, out, err) = run(
            &repo,
            &mock,
            &["--parallel", "--land", "--max-writers", "2"],
        );
        assert_eq!(code, 1, "{out}{err}");
        let recorded = mock.invocations.lock().unwrap().clone();
        assert_eq!(
            recorded
                .iter()
                .filter(|(tool, _, _)| tool == "peer-delegate")
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                (
                    "peer-delegate".into(),
                    Duration::from_secs(75),
                    Some("75s".into())
                ),
                (
                    "peer-delegate".into(),
                    Duration::from_secs(75),
                    Some("75s".into())
                ),
            ]
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|(tool, _, _)| tool == "patch-land")
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                ("patch-land".into(), Duration::from_secs(1200), None),
                ("patch-land".into(), Duration::from_secs(1200), None),
            ]
        );
        return;
    }
    if scrubbed_child(NAME) {
        return;
    }
    let repo = fixture(&[("s1", "a/")]);
    let mock = Mock::default();
    let held = oms_lock::Lock::for_file(
        &repo.0.join(".oms/plan/controller.lock"),
        oms_lock::Settings::from_env().unwrap(),
    )
    .unwrap()
    .try_acquire()
    .unwrap();
    let (code, out, err) = run(&repo, &mock, &["--parallel"]);
    assert_eq!(code, 75, "{out}{err}");
    assert!(mock.started.lock().unwrap().is_empty());
    assert_eq!(state(&repo, "s1")["state"], "ready");
    drop(held);
    for bad in [
        &["--parallel", "--id", "s1"][..],
        &["--parallel", "--dry-run"],
        &["--parallel", "--max-writers", "17"],
        &["--parallel", "--max-review-backlog", "65"],
        &["--max-writers", "2", "--next"],
        &["--max-review-backlog", "2", "--next"],
        &["--verify-combined", "--parallel"],
        &["--base-ref", "main", "--next"],
    ] {
        let (code, out, _) = run(&repo, &mock, bad);
        assert_eq!(code, 2, "{bad:?}: {out}");
    }
    assert!(mock.started.lock().unwrap().is_empty());
    let (code, out, err) = run(&repo, &mock, &["--parallel"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(state(&repo, "s1")["state"], "review");
    assert_eq!(
        mock.invocations
            .lock()
            .unwrap()
            .iter()
            .filter(|(tool, _, _)| tool == "peer-delegate")
            .cloned()
            .collect::<Vec<_>>(),
        vec![(
            "peer-delegate".into(),
            Duration::from_secs(86_400 + 60),
            None
        )]
    );
    // Nothing left to claim is a refusal, not a silent success.
    let (code, _, err) = run(&repo, &mock, &["--parallel"]);
    assert_eq!(code, 2, "{err}");

    let mut child = Command::new(std::env::current_exe().unwrap());
    if let Some(timeout) = std::env::var_os("OMS_TEST_CALL_TIMEOUT") {
        child.env("PARALLEL_TEST_CALL_TIMEOUT", timeout);
    }
    for (key, _) in std::env::vars_os().filter(|(k, _)| ambient(k)) {
        child.env_remove(key);
    }
    let result = child
        .args(["--exact", NAME, "--test-threads=1", "--nocapture"])
        .env("PARALLEL_PINNED_TIMEOUT_SUBTEST", "75s")
        .env("OMS_PEER_TIMEOUT", "75s")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.status.success(), "{text}");
    assert!(text.contains("1 passed;"), "{text}");
}

#[test]
fn overlapping_resources_never_run_together() {
    if scrubbed_child("overlapping_resources_never_run_together") {
        return;
    }
    let repo = fixture(&[]);
    let mock = Mock::default();
    for (id, allowed, resources) in [
        ("res1", "a/", Some("gpu-0")),
        ("res2", "b/", Some("gpu-0")),
        ("res3", "c/", None),
    ] {
        let mut args = vec![
            "add",
            "--id",
            id,
            "--title",
            id,
            "--allowed",
            allowed,
            "--verify",
            "true",
            "--brief-file",
            ".oms/fixture-brief",
        ];
        if let Some(res) = resources {
            args.extend(["--resources", res]);
        }
        mock.plan(&repo.0, &args);
    }
    let (code, out, err) = run(&repo, &mock, &["--parallel", "--max-writers", "3"]);
    assert_eq!(code, 0, "{out}{err}");
    let together = mock.together.lock().unwrap().clone();
    let pair = |a: &str, b: &str| {
        together
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
    };
    assert!(
        !pair("res1", "res2"),
        "shared resource must serialize res1 and res2: {together:?}"
    );
    for id in ["res1", "res2", "res3"] {
        assert_eq!(state(&repo, id)["state"], "review");
    }
    // Widened --max-writers (up to 16) is accepted as valid range (failing
    // cleanly on "no ready tasks", never on argument range validation).
    let (code, _, err) = run(&repo, &mock, &["--parallel", "--max-writers", "12"]);
    assert_eq!(code, 2, "{err}");
    assert!(!err.contains("max-writers"), "{err}");
}
