#![forbid(unsafe_code)]

use oms_command_state::{HostServices, NoHostServices, json, run_with_host, run_with_runtime_host};
use std::ffi::OsString;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempRepo(PathBuf);
impl TempRepo {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "oms-host-services-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        fs::set_permissions(&dir, Permissions::from_mode(0o750)).unwrap();
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
impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn own_group(uid: u32) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
    Ok(oms_command_plan::services::GroupMembership {
        gid: rustix::process::getgid().as_raw(),
        explicit_member_uids: vec![uid],
        primary_member_uids: vec![uid],
    })
}
fn resolve(gid: u32) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
    let uid = rustix::process::getuid().as_raw();
    let mut membership = own_group(uid)?;
    membership.gid = gid;
    Ok(membership)
}

struct GroupPlan;
impl oms_command_plan::Services for GroupPlan {
    fn group_membership_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        Some(resolve)
    }
}

struct CustomHost {
    plan: GroupPlan,
}
impl HostServices for CustomHost {
    fn plan_services(&self) -> &(dyn oms_command_plan::Services + Sync) {
        &self.plan
    }
    fn task_status(
        &self,
        _repo: &Path,
        _interrupted: &AtomicBool,
    ) -> Option<oms_command_state::Output> {
        Some((
            0,
            br#"{"schema":1,"lifecycle":"pursuing","marker":"host-task"}"#.to_vec(),
            Vec::new(),
        ))
    }
}

fn args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn seed(repo: &Path) {
    let plan = repo.join(".oms/plan");
    fs::create_dir_all(&plan).unwrap();
    fs::set_permissions(repo.join(".oms"), Permissions::from_mode(0o770)).unwrap();
    fs::set_permissions(&plan, Permissions::from_mode(0o770)).unwrap();
    fs::write(plan.join("tasks.json"), br#"{"schema":1,"tasks":{}}"#).unwrap();
    fs::set_permissions(plan.join("tasks.json"), Permissions::from_mode(0o664)).unwrap();
}

fn state(repo: &Path, host: &dyn HostServices) -> json::Value {
    let flag = AtomicBool::new(false);
    let mut services = Dummy;
    let (code, out, err) = run_with_host(
        "state",
        &args(&["--json", "--repo", &repo.to_string_lossy()]),
        repo,
        &flag,
        &mut services,
        host,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    json::parse(&out).unwrap()
}
struct Dummy;
impl oms_command_runs::Services for Dummy {}

fn verify_findings(repo: &Path, host: &dyn HostServices) -> Vec<String> {
    let flag = AtomicBool::new(false);
    let (_, out, err) = run_with_host(
        "state-verify",
        &args(&["--json", "--repo", &repo.to_string_lossy()]),
        repo,
        &flag,
        &mut Dummy,
        host,
    );
    assert!(!out.is_empty(), "{}", String::from_utf8_lossy(&err));
    let report = json::parse(&out).unwrap();
    report
        .get("findings")
        .array()
        .unwrap()
        .iter()
        .map(|f| format!("{}: {}", f.text("family"), f.text("message")))
        .collect()
}

#[test]
fn state_uses_host_plan_services_and_task_status() {
    let repo = TempRepo::new();
    seed(repo.path());
    let default = state(repo.path(), &NoHostServices);
    assert!(!default.get("plan").get("healthy").truth());
    assert_eq!(default.get("task").text("marker"), "");

    let custom = state(repo.path(), &CustomHost { plan: GroupPlan });
    assert!(custom.get("plan").get("healthy").truth());
    assert_eq!(custom.get("plan").get("task_count").int().unwrap(), 0);
    assert_eq!(custom.get("task").text("marker"), "host-task");
    assert_eq!(custom.get("task").text("lifecycle"), "pursuing");
}

#[test]
fn state_verify_retirement_check_uses_host_plan_services() {
    let repo = TempRepo::new();
    seed(repo.path());
    fs::write(repo.path().join(".oms/plan/retirements.jsonl"), b"").unwrap();
    let default = verify_findings(repo.path(), &NoHostServices);
    assert!(
        default
            .iter()
            .any(|f| f.contains("retirement integrity error")),
        "{default:?}"
    );
    let custom = verify_findings(repo.path(), &CustomHost { plan: GroupPlan });
    assert!(
        !custom
            .iter()
            .any(|f| f.contains("retirement integrity error")),
        "{custom:?}"
    );
}

fn seed_gc(repo: &Path) {
    seed(repo);
    let plan = repo.join(".oms/plan");
    fs::write(
        plan.join("tasks.json"),
        br#"{"schema":1,"tasks":{"t1":{"state":"claimed","lease_id":"lease-1"}}}"#,
    )
    .unwrap();
    fs::set_permissions(plan.join("tasks.json"), Permissions::from_mode(0o664)).unwrap();
    fs::create_dir_all(plan.join("completions")).unwrap();
    fs::set_permissions(plan.join("completions"), Permissions::from_mode(0o770)).unwrap();
    let marks = repo.join(".oms/delegations");
    fs::create_dir_all(&marks).unwrap();
    fs::set_permissions(&marks, Permissions::from_mode(0o770)).unwrap();
    fs::write(
        marks.join("m1.json"),
        br#"{"schema":1,"id":"m1","pid":2147483646,"task_id":"t1","lease_id":"lease-1"}"#,
    )
    .unwrap();
    fs::write(repo.join(".oms/landings.jsonl"), b"").unwrap();
}

fn gc(repo: &Path, host: &dyn HostServices) -> (i32, String, String) {
    let flag = AtomicBool::new(false);
    let (code, out, err) = run_with_host(
        "gc",
        &args(&["--dry-run", "--json", "--repo", &repo.to_string_lossy()]),
        repo,
        &flag,
        &mut Dummy,
        host,
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[test]
fn gc_bridges_plan_services_group_resolver_to_completion_and_orphan_paths() {
    let repo = TempRepo::new();
    seed_gc(repo.path());
    let (code, out, err) = gc(repo.path(), &NoHostServices);
    assert_eq!(code, 2, "{out}");
    assert!(err.contains("private-group account proof"), "{err}");

    // CustomHost overrides plan_services only; group_resolver must follow it.
    let (code, out, err) = gc(repo.path(), &CustomHost { plan: GroupPlan });
    assert_eq!(code, 0, "{err}");
    let report = json::parse(out.as_bytes()).unwrap();
    let kinds: Vec<_> = report
        .get("items")
        .array()
        .unwrap()
        .iter()
        .map(|i| i.text("path").to_string())
        .collect();
    assert_eq!(kinds, [".oms/delegations/m1.json"]);
}

#[test]
fn runtime_host_entry_threads_host_services() {
    let repo = TempRepo::new();
    seed(repo.path());
    let runtime = json::parse(br#"{"schema":2}"#).unwrap();
    let flag = AtomicBool::new(false);
    let call = |host: &dyn HostServices| {
        let (code, out, err) = run_with_runtime_host(
            "state",
            &args(&["--json", "--repo", &repo.path().to_string_lossy()]),
            repo.path(),
            &flag,
            &runtime,
            host,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        json::parse(&out).unwrap()
    };
    let custom = call(&CustomHost { plan: GroupPlan });
    assert!(custom.get("plan").get("healthy").truth());
    assert_eq!(custom.get("task").text("marker"), "host-task");
    assert!(custom.get("runtime").get("healthy").truth());
    assert!(!call(&NoHostServices).get("plan").get("healthy").truth());
}
