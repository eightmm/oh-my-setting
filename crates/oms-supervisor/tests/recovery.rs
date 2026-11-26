//! Public native adapter recovery fixtures. Lifecycle suffix removal models a
//! crash after runtime publication. Hosts without Linux proc child enumeration
//! use saved completion snapshots for payload cases; native pre-launch paths
//! still execute, and capable Linux/macOS hosts exercise the complete runner.
use oms_command_operations::{
    self as ops,
    value::{V, obj},
};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn owned_tree_available() -> bool {
    !cfg!(target_os = "linux")
        || PathBuf::from(format!("/proc/self/task/{}/children", std::process::id())).exists()
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "oms-supervisor-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        Self(std::fs::canonicalize(root).unwrap())
    }
    fn call(&self, words: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_oms-supervisor"))
            .args(["--repo", self.0.to_str().unwrap()])
            .args(words)
            .current_dir(&self.0)
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .output()
            .unwrap()
    }
    fn ok(&self, words: &[&str]) -> String {
        let out = self.call(words);
        assert!(
            out.status.success(),
            "{words:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn job_path(&self, id: &str) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        let hash = format!("{:x}", Sha256::digest(self.0.as_os_str().as_bytes()));
        self.0
            .join("state/oh-my-setting/runtime")
            .join(hash)
            .join("jobs")
            .join(format!("{id}.json"))
    }
    fn job(&self, id: &str) -> V {
        V::parse(&std::fs::read(self.job_path(id)).unwrap()).unwrap()
    }
    fn save_job(&self, id: &str, job: &V) {
        ops::storage::write_json_atomic(&self.job_path(id), job, 0o600).unwrap();
    }
    fn status(&self, id: &str) -> V {
        V::parse(self.ok(&["status", "--attempt", id, "--json"]).as_bytes()).unwrap()
    }
    fn transition(&self, id: &str, state: &str, reason: &str, key: &str) {
        let row = ops::new_event(
            id,
            0usize.into(),
            "attempt.state_changed",
            obj([
                ("from_state", V::Null),
                ("to_state", state.into()),
                ("reason_code", reason.into()),
                (
                    "actor",
                    obj([
                        ("kind", "supervisor".into()),
                        ("name", "agent-supervisor".into()),
                    ]),
                ),
                ("idempotency_key", key.into()),
            ]),
        )
        .unwrap();
        ops::append_lifecycle(&self.0, row, &AtomicBool::new(false)).unwrap();
    }
    fn complete(&self, id: &str, target: &str, reason: &str, exit: i32) {
        if owned_tree_available() || reason == "hard_budget_unsupported" {
            let run = self.call(&["_run-one", "--attempt", id]);
            assert_eq!(
                run.status.code(),
                Some(exit),
                "{}",
                String::from_utf8_lossy(&run.stderr)
            );
            return;
        }
        eprintln!("proc child enumeration unavailable; replaying saved {target} runtime fixture");
        self.transition(id, "starting", "", "runner-starting");
        self.transition(id, "working", "", "runner-working");
        let key = match target {
            "review" => "runner-review",
            "done" => "runner-done",
            "timed_out" => "runner-timed-out",
            _ => "runner-command-failed",
        };
        let mut job = self.job(id);
        job.set("runtime_state", "finished");
        job.set("exit_code", exit as i64);
        let mut facts = obj([
            ("schema", 1usize.into()),
            ("state", target.into()),
            ("reason_code", reason.into()),
            ("idempotency_key", key.into()),
        ]);
        if matches!(target, "review" | "done") {
            facts.set("command_sha256", job.get("command_sha256").clone());
            facts.set("completion_state", job.get("completion_state").clone());
        }
        job.set("finish", facts);
        self.save_job(id, &job);
        if matches!(target, "review" | "done") {
            self.transition(id, "verifying", "", "runner-verifying");
            self.transition(id, "review", "", "runner-review");
            if target == "review" {
                return;
            }
        }
        self.transition(id, target, reason, key);
    }
    fn restore_lifecycle_before(&self, target: &str) {
        let path = ops::event_path(&self.0);
        let rows = ops::read_rows(&path).unwrap();
        let cut = rows
            .iter()
            .position(|r| r.get("to_state").text() == Some(target))
            .unwrap();
        let mut bytes = Vec::new();
        for row in &rows[..cut] {
            bytes.extend(row.json(false).unwrap());
            bytes.push(b'\n');
        }
        std::fs::write(path, bytes).unwrap();
    }
    fn recover(&self, id: &str, expected: &str, reason: &str) {
        let before = std::fs::read(ops::event_path(&self.0)).unwrap();
        assert_eq!(
            V::parse(self.ok(&["reconcile", "--json"]).as_bytes()).unwrap(),
            V::Arr(vec![id.into()])
        );
        assert_eq!(
            std::fs::read(ops::event_path(&self.0)).unwrap(),
            before,
            "preview is read-only"
        );
        self.ok(&["reconcile", "--apply", "--json"]);
        let status = self.status(id);
        assert_eq!(status.get("state").text(), Some(expected), "{status:?}");
        assert_eq!(status.get("reason_code").text().unwrap_or(""), reason);
        let settled = std::fs::read(ops::event_path(&self.0)).unwrap();
        assert_eq!(self.ok(&["reconcile", "--apply", "--json"]), "[]");
        assert_eq!(std::fs::read(ops::event_path(&self.0)).unwrap(), settled);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn final_runtime_records_replay_the_original_settlement_after_a_crash() {
    for (extra, payload, target, reason, exit) in [
        (vec![], "exit 0", "review", "", 0),
        (vec!["--completion-state", "done"], "exit 0", "done", "", 0),
        (vec![], "exit 7", "failed", "command_failed", 7),
        (
            vec!["--max-wall-seconds", "0"],
            "exit 0",
            "timed_out",
            "wall_budget_exceeded",
            124,
        ),
        (
            vec!["--max-tokens", "0"],
            "exit 0",
            "failed",
            "hard_budget_unsupported",
            78,
        ),
    ] {
        let f = Fixture::new();
        let mut submit = vec!["submit"];
        submit.extend(extra);
        submit.extend(["--", "/bin/sh", "-c", payload]);
        let id = f.ok(&submit);
        f.complete(&id, target, reason, exit);
        let original = f.job(&id);
        let phases: &[&str] = if target == "done" {
            &["verifying", "review", "done"]
        } else if target == "review" {
            &["verifying", "review"]
        } else {
            &[target]
        };
        let completed = std::fs::read(ops::event_path(&f.0)).unwrap();
        for phase in phases {
            std::fs::write(ops::event_path(&f.0), &completed).unwrap();
            f.restore_lifecycle_before(phase);
            f.recover(&id, target, reason);
            assert_eq!(f.job(&id).get("exit_code"), original.get("exit_code"));
        }
    }
    // An actual pre-spawn error needs no proc/subreaper capability, and its
    // persisted reason must survive the same lifecycle-publication gap.
    let f = Fixture::new();
    let id = f.ok(&["submit", "--", "/bin/true"]);
    let mut job = f.job(&id);
    job.set("cwd", f.0.join("missing").to_string_lossy().into_owned());
    f.save_job(&id, &job);
    assert_eq!(
        f.call(&["_run-one", "--attempt", &id]).status.code(),
        Some(2)
    );
    f.restore_lifecycle_before("blocked");
    f.recover(&id, "blocked", "supervisor_error");
}

#[test]
fn cancellation_and_legacy_terminal_records_reconcile_without_inventing_success() {
    let f = Fixture::new();
    assert!(f.ok(&["cancel", "--help"]).contains("[--json]"));
    let id = f.ok(&["submit", "--", "/bin/true"]);
    let plain = f.call(&["cancel", "--attempt", &id]);
    assert!(plain.status.success());
    assert_eq!(plain.stdout, format!("{id}\n").into_bytes());
    f.restore_lifecycle_before("cancelled");
    f.recover(&id, "cancelled", "cancelled_by_owner");
    let child = f.ok(&["resume", "--attempt", &id]);
    assert_eq!(f.job(&child).get("finish"), &V::Null);
    assert_eq!(f.job(&child).get("runtime_state").text(), Some("queued"));
    let first = f.call(&["cancel", "--attempt", &child, "--json"]);
    assert!(first.status.success());
    assert_eq!(first.stdout.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(
        V::parse(&first.stdout).unwrap(),
        obj([
            ("schema", 1usize.into()),
            ("attempt", child.as_str().into()),
            ("runtime_state", "cancelled".into()),
            ("delivery", "confirmed".into()),
            ("replayed", false.into()),
        ])
    );
    let saved_job = std::fs::read(f.job_path(&child)).unwrap();
    let saved_events = std::fs::read(ops::event_path(&f.0)).unwrap();
    assert_eq!(
        V::parse(f.ok(&["cancel", "--attempt", &child, "--json"]).as_bytes()).unwrap(),
        obj([
            ("schema", 1usize.into()),
            ("attempt", child.as_str().into()),
            ("runtime_state", "cancelled".into()),
            ("delivery", "confirmed".into()),
            ("replayed", true.into()),
        ])
    );
    assert_eq!(std::fs::read(f.job_path(&child)).unwrap(), saved_job);
    assert_eq!(std::fs::read(ops::event_path(&f.0)).unwrap(), saved_events);
    assert_eq!(f.ok(&["cancel", "--attempt", &child]), child);
    assert_eq!(std::fs::read(f.job_path(&child)).unwrap(), saved_job);
    assert_eq!(std::fs::read(ops::event_path(&f.0)).unwrap(), saved_events);
    let mut non_owner = f.job(&child);
    non_owner.set(
        "finish",
        obj([
            ("schema", 1usize.into()),
            ("state", "cancelled".into()),
            ("reason_code", "external_abort".into()),
            ("idempotency_key", "external-abort".into()),
        ]),
    );
    f.save_job(&child, &non_owner);
    let refused = f.call(&["cancel", "--attempt", &child, "--json"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("attempt is no longer cancellable: cancelled")
    );

    for (payload, expected, reason) in [
        ("exit 0", "blocked", "terminal_evidence_missing"),
        ("exit 7", "failed", "terminal_exit_nonzero"),
    ] {
        let f = Fixture::new();
        let id = f.ok(&["submit", "--", "/bin/sh", "-c", payload]);
        f.complete(
            &id,
            if payload == "exit 0" {
                "review"
            } else {
                "failed"
            },
            if payload == "exit 0" {
                ""
            } else {
                "command_failed"
            },
            if payload == "exit 0" { 0 } else { 7 },
        );
        let completed = std::fs::read(ops::event_path(&f.0)).unwrap();
        f.restore_lifecycle_before(if payload == "exit 0" {
            "verifying"
        } else {
            "failed"
        });
        let mut job = f.job(&id);
        if let V::Obj(ref mut map) = job {
            map.remove(b"finish".as_slice());
        }
        f.save_job(&id, &job);
        f.recover(&id, expected, reason);
        let refused = f.call(&["cancel", "--attempt", &id, "--json"]);
        assert_eq!(refused.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("attempt is no longer cancellable: finished")
        );
        if payload == "exit 0" {
            std::fs::write(ops::event_path(&f.0), completed).unwrap();
            assert_eq!(
                f.ok(&["reconcile", "--apply", "--json"]),
                "[]",
                "an already settled legacy review stays reviewed"
            );
            assert_eq!(f.status(&id).get("state").text(), Some("review"));
        }
    }
}

#[test]
fn active_cancellation_reports_dispatched_and_replays_request() {
    for active in ["launching", "starting", "running", "cleanup_failed"] {
        let f = Fixture::new();
        let id = f.ok(&["submit", "--", "/bin/true"]);
        let mut job = f.job(&id);
        job.set("runtime_state", active);
        f.save_job(&id, &job);
        assert_eq!(
            V::parse(f.ok(&["cancel", "--attempt", &id, "--json"]).as_bytes()).unwrap(),
            obj([
                ("schema", 1usize.into()),
                ("attempt", id.as_str().into()),
                ("runtime_state", "cancel_requested".into()),
                ("delivery", "dispatched".into()),
                ("replayed", false.into()),
            ])
        );
        let saved_job = std::fs::read(f.job_path(&id)).unwrap();
        let saved_events = std::fs::read(ops::event_path(&f.0)).unwrap();
        assert_eq!(
            V::parse(f.ok(&["cancel", "--attempt", &id, "--json"]).as_bytes()).unwrap(),
            obj([
                ("schema", 1usize.into()),
                ("attempt", id.as_str().into()),
                ("runtime_state", "cancel_requested".into()),
                ("delivery", "dispatched".into()),
                ("replayed", true.into()),
            ])
        );
        assert_eq!(std::fs::read(f.job_path(&id)).unwrap(), saved_job);
        assert_eq!(std::fs::read(ops::event_path(&f.0)).unwrap(), saved_events);
    }
}

#[test]
fn cancellation_identity_mismatch_never_signals_an_unrelated_process() {
    let f = Fixture::new();
    let id = f.ok(&["submit", "--", "/bin/true"]);
    let mut unrelated = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let mut job = f.job(&id);
    job.set("runtime_state", "running");
    job.set("pid", unrelated.id() as i64);
    job.set("pid_identity", "different-process-start");
    f.save_job(&id, &job);
    assert_eq!(
        V::parse(f.ok(&["cancel", "--attempt", &id, "--json"]).as_bytes()).unwrap(),
        obj([
            ("schema", 1usize.into()),
            ("attempt", id.as_str().into()),
            ("runtime_state", "interrupted".into()),
            ("delivery", "outcome_unknown".into()),
            ("replayed", false.into()),
        ])
    );
    f.restore_lifecycle_before("failed");
    f.recover(&id, "failed", "process_identity_mismatch");
    let refused = f.call(&["cancel", "--attempt", &id, "--json"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("attempt is no longer cancellable: interrupted")
    );
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}
