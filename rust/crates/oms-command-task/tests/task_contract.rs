#![forbid(unsafe_code)]

use oms_command_task::observe::fingerprint;
use oms_command_task::services::{
    FailureObservation, JournalObservation, MemoryOutcome, ReferenceOperation, ReferenceWrite,
    Services, VerifyObservation,
};
use oms_command_task::{Result, owns, run, run_with_services, usage};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "oms-task-contract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Op;
impl ReferenceOperation for Op {
    fn validate(&mut self, _: &ReferenceWrite<'_>) -> Result<()> {
        Ok(())
    }
    fn finish(&mut self, _: i32) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Fake {
    journal: Vec<&'static str>,
    memory: usize,
}
impl Services for Fake {
    fn state_repository(&mut self, work: &Path, _: &AtomicBool) -> Result<PathBuf> {
        Ok(work.to_path_buf())
    }
    fn begin_reference(&mut self, _: &Path, _: &Path) -> Result<Box<dyn ReferenceOperation>> {
        Ok(Box::new(Op))
    }
    fn observe_journal(&mut self, request: JournalObservation<'_>) -> Result<()> {
        self.journal.push(request.operation.as_str());
        Ok(())
    }
    fn promote_memory(&mut self, _: MemoryOutcome<'_>) -> Result<()> {
        self.memory += 1;
        Ok(())
    }
    fn observe_failure(&mut self, _: FailureObservation<'_>) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn observe_verify(&mut self, _: &Path, _: &[u8]) -> Result<VerifyObservation> {
        Ok(VerifyObservation::default())
    }
    fn skill_contracts(&mut self, _: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(Vec::new())
    }
}

/// The library reads harness env (parent-only guards); tests cannot mutate their own env
/// without unsafe, so each env-sensitive test re-runs itself in a scrubbed child process.
/// Returns true only inside that child, where the body should run.
fn isolated(name: &str, extra: &[(&str, &str)]) -> bool {
    if std::env::var_os("TASK_CONTRACT_ISOLATED").is_some() {
        return true;
    }
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"]);
    for (key, _) in std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v))) {
        if key.starts_with("OMS_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("TASK_CONTRACT_ISOLATED", "1")
        .envs(extra.iter().copied());
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

fn args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn call(repo: &Repo, svc: &mut Fake, items: &[&str]) -> (i32, String, String) {
    let flag = AtomicBool::new(false);
    let (code, out, err) = run_with_services(&args(items), repo.path(), &flag, svc);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[test]
fn owns_and_help_need_no_repository() {
    assert!(owns(&args(&["status"])));
    assert!(owns(&[]));
    let text = String::from_utf8(usage().to_vec()).unwrap();
    assert!(text.contains("Usage: agent-task.sh"));
    let repo = Repo::new();
    let (code, out, _) = call(&repo, &mut Fake::default(), &["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Commands:"));
    assert!(!repo.path().join(".oms").exists());
}

#[test]
fn unknown_command_and_option_fail_with_usage_exit() {
    let repo = Repo::new();
    let mut svc = Fake::default();
    let (code, _, err) = call(&repo, &mut svc, &["bogus"]);
    assert_ne!(code, 0);
    assert!(err.starts_with("error: "), "{err}");
    let (code, _, _) = call(&repo, &mut svc, &["status", "--no-such-flag"]);
    assert_ne!(code, 0);
}

#[test]
fn status_without_packet_reads_without_services() {
    if !isolated("status_without_packet_reads_without_services", &[]) {
        return;
    }
    let repo = Repo::new();
    let flag = AtomicBool::new(false);
    let (code, out, _) = run(&args(&["status", "--json"]), repo.path(), &flag);
    assert_eq!(code, 0);
    assert!(out.starts_with(b"{"), "{}", String::from_utf8_lossy(&out));
    assert!(!repo.path().join(".oms/task/current.md").exists());
}

#[test]
fn lifecycle_init_update_append_verify_close() {
    if !isolated("lifecycle_init_update_append_verify_close", &[]) {
        return;
    }
    let repo = Repo::new();
    let mut svc = Fake::default();
    let packet = repo.path().join(".oms/task/current.md");

    let (code, _, err) = call(
        &repo,
        &mut svc,
        &["init", "--goal", "ship contract", "--verify", "true"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(packet.is_file());

    let (code, _, err) = call(
        &repo,
        &mut svc,
        &["update", "--done", "tests pass", "--next", "append a note"],
    );
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = call(&repo, &mut svc, &["append", "--text", "noted-marker"]);
    assert_eq!(code, 0, "{err}");
    let body = std::fs::read_to_string(&packet).unwrap();
    assert!(body.contains("ship contract") && body.contains("tests pass"));
    assert!(body.contains("noted-marker"));

    let (code, out, _) = call(&repo, &mut svc, &["status", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("lifecycle"), "{out}");

    let (code, _, err) = call(&repo, &mut svc, &["verify"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = call(&repo, &mut svc, &["close"]);
    assert_eq!(code, 0, "{err}");
    assert!(!packet.exists());
    assert!(repo.path().join(".oms/task/archive").is_dir());
    assert_eq!(svc.journal, ["update", "verify", "close"]);
    assert_eq!(svc.memory, 1);
}

#[test]
fn append_needs_text_and_close_without_task_is_noop() {
    if !isolated("append_needs_text_and_close_without_task_is_noop", &[]) {
        return;
    }
    let repo = Repo::new();
    let mut svc = Fake::default();
    let (code, out, err) = call(&repo, &mut svc, &["close"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("no active task"), "{out}");
    assert!(!repo.path().join(".oms/task/archive").exists());
    let (code, _, _) = call(
        &repo,
        &mut svc,
        &["init", "--goal", "g", "--verify", "true"],
    );
    assert_eq!(code, 0);
    let (code, out, err) = call(&repo, &mut svc, &["append"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("append requires --text or --stdin"), "{err}");
}

#[test]
fn unavailable_services_fail_closed_on_mutation() {
    if !isolated("unavailable_services_fail_closed_on_mutation", &[]) {
        return;
    }
    let repo = Repo::new();
    let flag = AtomicBool::new(false);
    let (code, _, err) = run(
        &args(&["init", "--goal", "g", "--verify", "true"]),
        repo.path(),
        &flag,
    );
    assert_ne!(code, 0);
    let err = String::from_utf8_lossy(&err);
    assert!(err.contains("unavailable"), "{err}");
    assert!(!repo.path().join(".oms/task/current.md").exists());
}

#[test]
fn fingerprint_tolerates_missing_python3() {
    let repo = Repo::new();
    let mut asked = Vec::new();
    let fp = fingerprint(repo.path(), &[], |name| {
        asked.push(name.to_string());
        (name != "python3").then(|| format!("{name} 1.0"))
    })
    .unwrap();
    assert!(asked.iter().any(|n| n == "python3"));
    assert!(fp.tools.iter().all(|(n, _)| n != "python3"));
    assert!(fp.tools.iter().any(|(n, _)| n == "bash"));
    let none = fingerprint(repo.path(), &[], |_| None).unwrap();
    assert!(none.tools.is_empty());
}

#[test]
fn fingerprint_adds_runner_tools_once() {
    let repo = Repo::new();
    let runners = ["cargo".to_string(), "cargo".to_string(), "go".to_string()];
    let fp = fingerprint(repo.path(), &runners, |n| Some(n.to_string())).unwrap();
    let names: Vec<_> = fp.tools.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["python3", "bash", "git", "cargo", "rustc", "go"]);
}

#[test]
fn harness_child_cannot_init() {
    if !isolated("harness_child_cannot_init", &[("OMS_HARNESS_CHILD", "1")]) {
        return;
    }
    let repo = Repo::new();
    let (code, _, err) = call(
        &repo,
        &mut Fake::default(),
        &["init", "--goal", "g", "--verify", "true"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("parent-only"), "{err}");
    assert!(!repo.path().join(".oms/task/current.md").exists());
}

#[test]
fn close_with_expected_digest_rejects_missing_packet() {
    if !isolated("close_with_expected_digest_rejects_missing_packet", &[]) {
        return;
    }
    let repo = Repo::new();
    let mut svc = Fake::default();
    let digest = "0".repeat(64);
    let (code, _, err) = call(
        &repo,
        &mut svc,
        &["close", "--reason", "r", "--if-unchanged-sha256", &digest],
    );
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("changed before close"), "{err}");
}
