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

    // Valid `plan-replacement` rows written by `oms-command-plan` must not be flagged unsupported.
    let plan_id = format!("plan_{}", "a".repeat(32));
    let archive_body = format!(
        "{{\"schema\":1,\"plan_id\":\"{plan_id}\",\"tasks\":{{\"t1\":{{\"state\":\"ready\"}}}}}}"
    );
    let archive_bytes = archive_body.as_bytes();
    let hash = json::sha(archive_bytes);
    let archive_rel = format!(".oms/plan/tasks.{hash}.archive.json");
    fs::write(repo.path().join(&archive_rel), archive_bytes).unwrap();
    let replacement = format!(
        "{{\"schema\":1,\"kind\":\"plan-replacement\",\"ts\":\"2026-11-17T00:00:00Z\",\"replacement_id\":\"replace_{hash}\",\"plan_sha256\":\"{hash}\",\"plan_id\":\"{plan_id}\",\"disposition\":\"replaced\",\"archive\":\"{archive_rel}\",\"non_terminal_task_ids\":[\"t1\"]}}\n"
    );
    fs::write(
        repo.path().join(".oms/plan/retirements.jsonl"),
        replacement.as_bytes(),
    )
    .unwrap();
    let replacement_findings = verify_findings(repo.path(), &CustomHost { plan: GroupPlan });
    assert!(
        !replacement_findings
            .iter()
            .any(|f| f.contains("plan-retirement")),
        "{replacement_findings:?}"
    );
    // Bounded parser accepts >4 MiB valid JSON while default `json::parse` stays capped at 4 MiB.
    let oversized = format!("[\"{}\"]", "a".repeat(4 * 1024 * 1024 + 64));
    assert!(json::parse(oversized.as_bytes()).is_err());
    assert!(json::parse_bounded(oversized.as_bytes(), 8 * 1024 * 1024).is_ok());
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

// ---- Stage G: multi-host/parallel projections ----

fn view(tool: &str, repo: &Path, extra: &[&str]) -> (i32, String, String) {
    let mut argv = vec!["--repo"];
    let repo_text = repo.to_string_lossy().into_owned();
    argv.push(&repo_text);
    argv.extend_from_slice(extra);
    let (code, out, err) = run_with_host(
        tool,
        &args(&argv),
        repo,
        &AtomicBool::new(false),
        &mut Dummy,
        &CustomHost { plan: GroupPlan },
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}
fn view_json(tool: &str, repo: &Path) -> json::Value {
    let (_, out, err) = view(tool, repo, &["--json"]);
    json::parse(out.as_bytes()).unwrap_or_else(|_| panic!("{tool}: {out} {err}"))
}
fn inbox_codes(repo: &Path) -> Vec<String> {
    view_json("inbox", repo)
        .get("items")
        .array()
        .unwrap()
        .iter()
        .map(|i| i.text("code").to_owned())
        .collect()
}
fn put(repo: &Path, rel: &str, body: &str, mode: u32) {
    let path = repo.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
}
fn tree(repo: &Path) -> Vec<(String, u64, u32)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, u64, u32)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if rel == ".git" {
                continue;
            }
            out.push((rel, meta.len(), meta.permissions().mode()));
            if meta.is_dir() {
                walk(&path, root, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(repo, repo, &mut out);
    out.sort();
    out
}
const SECTIONS: [&str; 6] = [
    "remote_targets",
    "exec_snapshots",
    "remote_operations",
    "slurm_submissions",
    "plan_contracts",
    "late_results",
];
fn digest(value: &json::Value, drop: &[&str]) -> String {
    let mut payload = value.clone();
    for key in drop {
        payload.remove(key);
    }
    json::hash(&payload).unwrap()
}

fn seed_fleet(repo: &Path) {
    seed(repo);
    let fp = "a".repeat(64);
    put(
        repo,
        ".oms/runtime/targets.json",
        &format!(
            r#"{{"schema":1,"targets":{{"gpu":{{"ssh_alias":"gpu1","workdir":"~/r","scheduler":"slurm","execution_approved":true,"approved_fingerprint":"{fp}"}},"lab":{{"ssh_alias":"lab1","workdir":"~/r","execution_approved":true,"approved_fingerprint":""}}}}}}"#
        ),
        0o600,
    );
    let mut snap = json::parse(
        br#"{"schema":1,"head_sha":"0000000000000000000000000000000000000000","tree_sha":"1111111111111111111111111111111111111111","lockfiles":{}}"#,
    )
    .unwrap();
    let d = digest(&snap, &[]);
    snap.set("snapshot_sha256", json::string(&d)).unwrap();
    put(
        repo,
        &format!(".oms/runtime/snapshots/{d}.json"),
        &String::from_utf8(json::canonical(&snap).unwrap()).unwrap(),
        0o644,
    );
    for (id, state) in [
        ("op-run", "running"),
        ("op-unk", "unknown"),
        ("op-bad", "failed"),
    ] {
        put(
            repo,
            &format!(".oms/runtime/remote-ops/{id}.json"),
            &format!(
                r#"{{"schema":1,"op_id":"{id}","action":"exec","state":"{state}","target":"gpu","remote_dir":"~/hidden/dir","stdout":"TOPSECRET-OUT","stderr":"TOPSECRET-ERR","started_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:0{}Z","exit_code":1}}"#,
                id.len() % 10
            ),
            0o600,
        );
    }
    let rows = [
        r#"{"schema":1,"type":"intent","key":"k1","attempt":"a1","task_id":"t1","attempt_id":"att-1","exec_snapshot_sha256":"abc"}"#,
        r#"{"schema":1,"type":"submitted","key":"k1","attempt":"a1","job_id":"77","cluster_name":"c1"}"#,
        r#"{"schema":1,"type":"intent","key":"k2","attempt":"a2"}"#,
        r#"{"schema":1,"type":"intent","key":"k3","attempt":"a3"}"#,
        r#"{"schema":1,"type":"failed","key":"k3","attempt":"a3","exit":1}"#,
        r#"{"schema":1,"type":"intent","key":"k4","attempt":"a4"}"#,
        r#"{"schema":1,"type":"submitted","key":"k4","attempt":"a4","job_id":"88"}"#,
        r#"{"schema":1,"type":"reconciled","key":"k4","attempt":"a4","job_id":"88","state":"succeeded"}"#,
    ];
    put(
        repo,
        ".oms/slurm-submits.jsonl",
        &(rows.join("\n") + "\n"),
        0o600,
    );
    let mut version = json::parse(br#"{"version":1,"summary":"v1"}"#).unwrap();
    let d = digest(&version, &[]);
    version.set("status", json::string("proposed")).unwrap();
    version.set("sha256", json::string(d)).unwrap();
    let contract = json::object([
        ("id", json::string("api")),
        ("accepted_version", json::number(0)),
        ("accepted_sha256", json::string("")),
        ("versions", json::object([("1", version)])),
    ]);
    let registry = json::object([
        ("schema", json::number(1)),
        ("contracts", json::object([("api", contract)])),
    ]);
    put(
        repo,
        ".oms/plan/contracts.json",
        &String::from_utf8(json::canonical(&registry).unwrap()).unwrap(),
        0o600,
    );
    put(
        repo,
        ".oms/plan/tasks.json",
        r#"{"schema":1,"tasks":{"t1":{"id":"t1","title":"t","state":"ready","depends":[],"allowed_paths":["a"],"forbidden_paths":[],"verify":"true","role":"builder","assignment":{},"provider":"","ttl":"","artifact":"","patch":"","reason":"","executor_id":"","executor_soul_sha256":"","autopilot_owner_id":"","lease_epoch":0,"lease_id":"","review_lease_id":"","repair_count":0,"repair_artifact":"","created":"2026-01-01T00:00:00Z","updated":"2026-01-01T00:00:00Z","contracts":["api@v1"]}}}"#,
        0o664,
    );
    put(
        repo,
        ".oms/plan/late-results.jsonl",
        "{\"schema\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"task_id\":\"t1\",\"supplied_lease_id\":\"old\",\"current_lease_id\":\"new\",\"reason\":\"stale_lease\",\"patch\":\"SECRET-PATCH.diff\"}\n",
        0o600,
    );
}

#[test]
fn fleet_sections_default_clean_and_reads_write_nothing() {
    let repo = TempRepo::new();
    seed(repo.path());
    let before = tree(repo.path());
    let state = view_json("state", repo.path());
    for key in SECTIONS {
        let s = state.get(key);
        assert!(
            s.get("healthy").truth() && !s.get("present").truth(),
            "{key}"
        );
        assert_eq!(s.get("total").int().unwrap(), 0, "{key}");
    }
    let codes = inbox_codes(repo.path());
    for code in [
        "unknown-remote-operations",
        "unknown-slurm-submissions",
        "stale-plan-contracts",
        "quarantined-late-results",
    ] {
        assert!(!codes.iter().any(|c| c == code), "{codes:?}");
    }
    let verify = verify_findings(repo.path(), &CustomHost { plan: GroupPlan });
    assert!(
        !SECTIONS
            .iter()
            .any(|s| verify.iter().any(|f| f.starts_with(s))),
        "{verify:?}"
    );
    assert!(
        view("dashboard", repo.path(), &[])
            .1
            .contains("OMS dashboard")
    );
    assert!(
        view_json("dashboard", repo.path())
            .get("fleet")
            .object()
            .is_ok()
    );
    assert_eq!(tree(repo.path()), before, "read views must not write");
    assert!(!repo.path().join(".oms/runtime").exists());
}

#[test]
fn fleet_valid_records_project_across_every_view_without_leaking() {
    let repo = TempRepo::new();
    seed_fleet(repo.path());
    let before = tree(repo.path());
    let state = view_json("state", repo.path());
    for key in SECTIONS {
        assert!(state.get(key).get("healthy").truth(), "{key}: {state:?}");
        assert!(state.get(key).get("present").truth(), "{key}");
    }
    let t = state.get("remote_targets");
    assert_eq!(
        [
            t.get("total"),
            t.get("approved"),
            t.get("unapproved"),
            t.get("slurm_targets")
        ]
        .map(|v| v.int().unwrap()),
        [2, 1, 1, 1]
    );
    assert_eq!(state.get("exec_snapshots").get("total").int().unwrap(), 1);
    let ops = state.get("remote_operations");
    assert_eq!(
        [
            ops.get("total"),
            ops.get("running"),
            ops.get("unknown"),
            ops.get("failed")
        ]
        .map(|v| v.int().unwrap()),
        [3, 1, 1, 1]
    );
    let slurm = state.get("slurm_submissions");
    assert_eq!(
        [
            slurm.get("total_keys"),
            slurm.get("active_submitted"),
            slurm.get("unknown"),
            slurm.get("failed"),
            slurm.get("reconciled"),
        ]
        .map(|v| v.int().unwrap()),
        [4, 1, 1, 1, 1]
    );
    assert!(
        slurm
            .get("recent")
            .array()
            .unwrap()
            .iter()
            .any(|r| r.text("job_id") == "77"
                && r.text("task_id") == "t1"
                && r.text("cluster") == "c1")
    );
    let contracts = state.get("plan_contracts");
    assert_eq!(contracts.get("total").int().unwrap(), 1);
    assert_eq!(contracts.get("proposed_unaccepted").int().unwrap(), 1);
    assert_eq!(contracts.get("stale_task_count").int().unwrap(), 1);
    assert_eq!(state.get("late_results").get("total").int().unwrap(), 1);
    assert_eq!(state.get("plan").get("contract_count").int().unwrap(), 1);

    let codes = inbox_codes(repo.path());
    for code in [
        "unknown-remote-operations",
        "unknown-slurm-submissions",
        "stale-plan-contracts",
        "quarantined-late-results",
    ] {
        assert!(codes.iter().any(|c| c == code), "{code} in {codes:?}");
    }
    let inbox = view_json("inbox", repo.path());
    assert!(inbox.get("safe_actions").array().unwrap().is_empty());
    let unknown = inbox
        .get("items")
        .array()
        .unwrap()
        .iter()
        .find(|i| i.text("code") == "unknown-remote-operations")
        .unwrap();
    assert!(
        unknown.text("command").contains("reconcile") && unknown.text("command").contains("op-unk")
    );
    assert_eq!(unknown.text("priority"), "P1");

    let verify = verify_findings(repo.path(), &CustomHost { plan: GroupPlan });
    assert!(
        !SECTIONS
            .iter()
            .any(|s| verify.iter().any(|f| f.starts_with(s))),
        "{verify:?}"
    );

    let dashboard = view_json("dashboard", repo.path());
    assert_eq!(
        dashboard
            .get("fleet")
            .get("remote_operations")
            .get("unknown")
            .int()
            .unwrap(),
        1
    );
    let mut text = view("dashboard", repo.path(), &[]).1;
    text.push_str(&view("state", repo.path(), &[]).1);
    for needle in ["fleet remote ops:", "## Fleet", "1 unknown"] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
    let everything = [
        serde_like(&state),
        serde_like(&dashboard),
        serde_like(&inbox),
        text,
    ]
    .join("\n");
    for secret in ["TOPSECRET", "hidden/dir", "SECRET-PATCH"] {
        assert!(!everything.contains(secret), "{secret} leaked");
    }
    assert_eq!(tree(repo.path()), before, "read views must not write");
}
fn serde_like(value: &json::Value) -> String {
    String::from_utf8(json::canonical(value).unwrap()).unwrap()
}

#[test]
fn fleet_corruption_is_isolated_into_degraded_sections_and_findings() {
    let repo = TempRepo::new();
    seed_fleet(repo.path());
    let root = repo.path();
    put(
        root,
        ".oms/runtime/remote-ops/loose.json",
        r#"{"schema":1,"op_id":"loose","state":"failed"}"#,
        0o644,
    );
    put(
        root,
        ".oms/runtime/remote-ops/mismatch.json",
        r#"{"schema":1,"op_id":"other","state":"failed"}"#,
        0o600,
    );
    std::os::unix::fs::symlink(
        "op-run.json",
        root.join(".oms/runtime/remote-ops/link.json"),
    )
    .unwrap();
    put(
        root,
        &format!(".oms/runtime/snapshots/{}.json", "b".repeat(64)),
        r#"{"schema":1,"snapshot_sha256":"x"}"#,
        0o644,
    );
    put(
        root,
        ".oms/runtime/snapshots/garbage.json",
        "not json",
        0o644,
    );
    fs::write(
        root.join(".oms/slurm-submits.jsonl"),
        fs::read_to_string(root.join(".oms/slurm-submits.jsonl")).unwrap()
            + "not-json\n{\"schema\":1,\"type\":\"bogus\",\"key\":\"k\",\"attempt\":\"a\"}\n",
    )
    .unwrap();
    fs::set_permissions(
        root.join(".oms/plan/contracts.json"),
        Permissions::from_mode(0o644),
    )
    .unwrap();
    put(
        root,
        ".oms/plan/late-results.jsonl",
        "{\"schema\":2,\"task_id\":\"t1\"}\n",
        0o600,
    );
    put(root, ".oms/runtime/targets.json", "[1]", 0o600);
    let before = tree(root);

    let (code, _, err) = view("state", root, &["--json"]);
    assert_eq!(code, 0, "{err}");
    let state = view_json("state", root);
    for key in SECTIONS {
        assert!(
            !state.get(key).get("healthy").truth(),
            "{key} should degrade"
        );
        assert!(state.get(key).get("invalid").int().unwrap() > 0, "{key}");
    }
    // Healthy records beside corrupt siblings still count.
    assert_eq!(
        state.get("remote_operations").get("unknown").int().unwrap(),
        1
    );
    let (code, out, _) = view("state-verify", root, &["--json"]);
    assert_eq!(code, 1);
    let report = json::parse(out.as_bytes()).unwrap();
    let findings = report.get("findings").array().unwrap();
    for key in SECTIONS {
        assert!(findings.iter().any(|f| f.text("family") == key), "{key}");
    }
    let all: String = findings
        .iter()
        .map(|f| f.text("message"))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "mode 0600",
        "symlink",
        "mismatched op_id",
        "digest verification",
        "malformed ledger row",
    ] {
        assert!(all.contains(needle), "{needle}\n{all}");
    }
    assert!(view("dashboard", root, &[]).1.contains("DEGRADED"));
    assert_eq!(tree(root), before, "read views must not write");
}

#[test]
fn fleet_scan_bound_degrades_one_section_without_failing_views() {
    let repo = TempRepo::new();
    seed(repo.path());
    let dir = repo.path().join(".oms/runtime/snapshots");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..=10_000 {
        fs::write(dir.join(format!("{i}.json")), b"").unwrap();
    }
    let state = view_json("state", repo.path());
    let s = state.get("exec_snapshots");
    assert!(!s.get("healthy").truth());
    assert!(
        s.get("problems").array().unwrap()[0]
            .str()
            .unwrap()
            .contains("cannot be scanned")
    );
    assert!(state.get("remote_operations").get("healthy").truth());
}

#[test]
fn state_verify_survives_more_than_fifty_thousand_nonjsonl_artifacts() {
    let repo = TempRepo::new();
    seed(repo.path());
    let dir = repo.path().join(".oms/artifacts/blobs");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..50_001 {
        fs::write(dir.join(format!("{i}.out")), b"").unwrap();
    }
    let (code, out, err) = view("state-verify", repo.path(), &["--json"]);
    assert!(!err.contains("scan entry limit"), "{err}");
    assert!(code == 0 || code == 1, "{code}: {out} {err}");
    assert!(
        json::parse(out.as_bytes())
            .unwrap()
            .get("findings")
            .array()
            .is_ok()
    );
}
