#![forbid(unsafe_code)]

use oms_command_autonomy::{CommandResult, Invocation, ModelRoute, Services, run_with_services};
use oms_command_plan::services::{GroupMembership, GroupResolver};
use oms_command_plan::storage::ACTIVE;
use oms_command_plan::{Action, Context, Plan, Request};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
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
            .wait_timeout_while(state, Duration::from_secs(20), |s| !s.resume_reader)
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
            .wait_timeout_while(state, Duration::from_secs(20), |s| !s.reader_open)
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

thread_local! {
    static RACE_READER: RefCell<Option<(Arc<ReviewRace>, bool)>> = const { RefCell::new(None) };
}

fn group_with_review_race(gid: u32) -> oms_command_plan::Result<GroupMembership> {
    // Only the Plan file is group-writable. Its second group proof occurs
    // after the Plan reader has opened the file and sampled its inode.
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
    /// Task ids whose worker panics / reports failure.
    panics: BTreeSet<&'static str>,
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
    review_race: Option<Arc<ReviewRace>>,
}
impl oms_command_plan::Services for Mock {
    fn group_membership_resolver(&self) -> Option<GroupResolver> {
        Some(if self.review_race.is_some() {
            group_with_review_race
        } else {
            group
        })
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
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut started = self.started.lock().unwrap();
        while !predicate(&started) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "worker rendezvous timed out: {started:?}");
            started = self.wake.wait_timeout(started, left).unwrap().0;
        }
    }
    fn delegate(&self, call: &Invocation) {
        let args = call.args.iter().map(|a| a.to_string_lossy().into_owned());
        let args = args.collect::<Vec<_>>();
        let id = args
            .windows(2)
            .find(|w| w[0] == "--plan-task")
            .map(|w| w[1].clone())
            .unwrap();
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
        let lease = self.delegate_lease(&call.cwd, &id);
        self.plan(
            &call.cwd,
            &[
                "review",
                "--id",
                &id,
                "--lease-id",
                &lease,
                "--artifact",
                &format!("artifact-{id}"),
                "--patch",
                &format!("patch-{id}"),
            ],
        );
        if let Some(race) = free_lock {
            race.resume_reader();
        }
        if let Some(race) = &self.review_race
            && id == "t3-late"
        {
            RACE_READER.with(|reader| reader.replace(Some((race.clone(), false))));
        }
        self.live.fetch_sub(1, Ordering::SeqCst);
        self.running.lock().unwrap().remove(&id);
    }
}
impl Services for Mock {
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
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<CommandResult> {
        if command.tool == "peer-delegate" {
            self.delegate(command);
        }
        Ok((0, Vec::new(), Vec::new()))
    }
}

fn fixture(tasks: &[(&str, &str)]) -> Repo {
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
    mock.plan(&repo.0, &["init", "--goal", "parallel"]);
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
        out.contains("task=t2-boom panicked; lease released"),
        "{out}"
    );
    assert!(out.contains("started=3 failed=1"), "{out}");
    for ok in ["t1-slow", "t3-late"] {
        let task = state(&repo, ok);
        assert_eq!(task["state"], "review", "{ok}: {task}");
        assert_eq!(task["artifact"], format!("artifact-{ok}"));
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
    let mock = Mock {
        rendezvous: 2,
        ..Mock::default()
    };
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
    if scrubbed_child("one_controller_per_state_repository_and_flag_validation") {
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
    // Nothing left to claim is a refusal, not a silent success.
    let (code, _, err) = run(&repo, &mock, &["--parallel"]);
    assert_eq!(code, 2, "{err}");
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
