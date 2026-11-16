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

#[test]
fn read_only_views_and_zero_repair_snapshot() {
    struct CountingHost(AtomicU64);
    impl HostServices for CountingHost {
        fn plan_services(&self) -> &(dyn oms_command_plan::Services + Sync) {
            &GroupPlan
        }
        fn task_status(&self, _: &Path, _: &AtomicBool) -> Option<oms_command_state::Output> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Some((0, br#"{"schema":1,"present":false}"#.to_vec(), vec![]))
        }
    }
    let repo = TempRepo::new();
    let stop = AtomicBool::new(false);
    let host = CountingHost(AtomicU64::new(0));
    for tool in ["state", "inbox", "dashboard"] {
        let (code, _, err) = run_with_host(
            tool,
            &args(&["--json", "--repo", &repo.path().to_string_lossy()]),
            repo.path(),
            &stop,
            &mut Dummy,
            &host,
        );
        assert!(code <= 1, "{}", String::from_utf8_lossy(&err));
        assert!(!repo.path().join(".oms").exists(), "{tool} created state");
    }
    host.0.store(0, Ordering::Relaxed);
    let (code, _, err) = run_with_host(
        "inbox",
        &args(&[
            "--json",
            "--fix-safe",
            "--repo",
            &repo.path().to_string_lossy(),
        ]),
        repo.path(),
        &stop,
        &mut Dummy,
        &host,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    assert_eq!(host.0.load(Ordering::Relaxed), 1);
}

#[test]
fn lease_append_preserves_descriptor_bytes_and_concurrent_batches() {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let repo = TempRepo::new();
    let call = |words: &[&str]| {
        let (code, _, err) = run_with_host(
            "lease",
            &args(words),
            repo.path(),
            &AtomicBool::new(false),
            &mut Dummy,
            &NoHostServices,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    };
    call(&["add", "first"]);
    let path = repo.path().join(".oms/leases.jsonl");
    let mut first = fs::read(&path).unwrap();
    assert_eq!(first.pop(), Some(b'\n'));
    fs::write(&path, &first).unwrap();
    let mut descriptor = fs::File::open(&path).unwrap();
    let inode = descriptor.metadata().unwrap().ino();
    std::thread::scope(|scope| {
        for i in 0..4 {
            let call = &call;
            scope.spawn(move || call(&["add", &format!("path-{i}")]));
        }
    });
    call(&["release", "--mine"]);
    call(&["release", "--mine"]);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    let mut observed = Vec::new();
    descriptor.read_to_end(&mut observed).unwrap();
    assert_eq!(observed, fs::read(&path).unwrap());
    assert!(observed.starts_with(&first));
    let lines = observed
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 10);
    for line in lines {
        json::parse(line).unwrap();
    }
}

#[test]
fn state_git_projection_uses_one_status() {
    if let Some(path) = std::env::var_os("OMS_STATE_GIT_FIXTURE") {
        let repo = Path::new(&path);
        let _ = state(repo, &CustomHost { plan: GroupPlan });
        let trace = fs::read_to_string(repo.join("git-calls")).unwrap();
        assert_eq!(
            trace
                .lines()
                .filter(|l| l.contains("status --porcelain=v2 --branch"))
                .count(),
            1,
            "{trace}"
        );
        assert!(!trace.contains("rev-list --count"), "{trace}");
        assert!(!trace.contains("--abbrev-ref @{upstream}"), "{trace}");
        return;
    }
    let repo = TempRepo::new();
    let original = std::env::var_os("PATH").unwrap();
    let git = std::env::split_paths(&original)
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap();
    let bin = repo.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("git"), b"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$OMS_GIT_TRACE_FILE\"\nexec \"$OMS_GIT_REAL\" \"$@\"\n").unwrap();
    fs::set_permissions(bin.join("git"), Permissions::from_mode(0o700)).unwrap();
    let path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&original))).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "state_git_projection_uses_one_status",
            "--nocapture",
        ])
        .env("PATH", path)
        .env("OMS_STATE_GIT_FIXTURE", repo.path())
        .env("OMS_GIT_REAL", git)
        .env("OMS_GIT_TRACE_FILE", repo.path().join("git-calls"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn artifact_repairs_preserve_inode_and_first_later_success() {
    use std::os::unix::fs::MetadataExt;
    let repo = TempRepo::new();
    let path = repo.path().join(".oms/artifacts/index.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    let count = 256;
    for i in 0..count + 2 {
        let row = json::object([
            ("schema", json::number(1)),
            ("event_id", json::string(format!("event-{i}"))),
            ("operation_id", json::string(format!("op-{i}"))),
            ("artifact_id", json::string(format!("artifact-{i}"))),
            ("ts", json::string("2026-01-01T00:00:00Z")),
            (
                "kind",
                json::string(if i < count { "delegate" } else { "patch-admit" }),
            ),
            ("provider", json::string("fixture")),
            ("exit", json::number(if i < count { 1 } else { 0 })),
            ("patch_sha256", json::string("a".repeat(64))),
        ]);
        bytes.extend_from_slice(&json::default_bytes(&row, false).unwrap());
        bytes.push(b'\n');
    }
    fs::write(&path, &bytes).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let invoke = || {
        let result = run_with_host(
            "inbox",
            &args(&[
                "--repo",
                &repo.path().to_string_lossy(),
                "--json",
                "--fix-safe",
            ]),
            repo.path(),
            &AtomicBool::new(false),
            &mut Dummy,
            &CustomHost { plan: GroupPlan },
        );
        assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
    };
    invoke();
    let repaired = fs::read(&path).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert!(repaired.starts_with(&bytes));
    let rows = repaired
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| json::parse(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), count * 2 + 2);
    for (i, row) in rows.iter().skip(count + 2).enumerate() {
        assert_eq!(row.text("resolves_event_id"), format!("event-{i}"));
        assert!(
            row.text("reason")
                .starts_with(&format!("superseded by event-{count} "))
        );
    }
    invoke();
    assert_eq!(fs::read(&path).unwrap(), repaired);
}
