#![forbid(unsafe_code)]

use oms_command_lifecycle::{
    IncomingDiagnostic, IncomingPhase, IncomingSourceLease, IncomingTool, Output, ReleaseChannel,
    Services, UnavailableServices, apply_release_with_services, run_with_services,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[derive(Default)]
struct NativeServices(RefCell<Vec<String>>);
impl NativeServices {
    fn record(&self, name: &str) -> Output {
        self.0.borrow_mut().push(name.into());
        (0, b"{}\n".to_vec(), Vec::new())
    }
}
impl Services for NativeServices {
    fn install_extras(
        &self,
        phase: IncomingPhase,
        _: &Path,
        _: &[OsString],
        _: &AtomicBool,
    ) -> Output {
        self.record(&format!("extras:{phase:?}"))
    }
    fn hook_protection(&self, _: &Path, _: &AtomicBool) -> Output {
        self.record("protection")
    }
    fn apply_project_template(&self, _: &str, _: &Path, _: &Path, _: &AtomicBool) -> Output {
        self.record("template")
    }
    fn provider_hook(
        &self,
        _: &str,
        _: &[u8],
        _: &Path,
        _: &Path,
        _: bool,
        _: &AtomicBool,
    ) -> Output {
        self.record("hook")
    }
    fn incoming_lifecycle(
        &self,
        phase: IncomingPhase,
        _: &Path,
        _: &str,
        _: &[u8],
        _: &[OsString],
        _: &AtomicBool,
    ) -> Output {
        self.record(&format!("incoming:{phase:?}"))
    }
    fn tick_maintenance(&self, _: &Path, _: &Path, _: &AtomicBool) -> Output {
        self.record("maintenance");
        (
            0,
            serde_json::to_vec(&json!({
                "journal_rc":0,"ci_rc":0,"plugin_rc":0,"plan_retire_rc":0,
                "artifact_resolve_rc":0,"artifact_supersede_rc":0,"threads_closed":[],
                "tasks_closed":0,"plans_retired":0,"artifacts_resolved":0,
                "artifacts_superseded":0,"failures_retired":0
            }))
            .unwrap(),
            Vec::new(),
        )
    }
    fn tick_schedule(&self, args: &[OsString], _: &Path, _: &Path, _: &AtomicBool) -> Output {
        assert_eq!(args.first().unwrap(), "schedule");
        self.record("schedule")
    }
    fn refresh_catalogs(&self, _: bool, _: &Path, _: &AtomicBool) -> Output {
        self.record("catalogs")
    }
    fn garbage_collect(&self, _: &Path, _: &AtomicBool) -> Output {
        self.record("gc")
    }
    fn wake_idle_main(&self, _: bool, _: &Path, _: &AtomicBool) -> Output {
        self.record("wake");
        (0, b"{\"wake\":0,\"wakes\":[]}\n".to_vec(), Vec::new())
    }
}
struct HintHost;
impl Services for HintHost {
    fn provider_hook(
        &self,
        _: &str,
        _: &[u8],
        _: &Path,
        _: &Path,
        _: bool,
        _: &AtomicBool,
    ) -> Output {
        (0, b"route to plan\n".to_vec(), Vec::new())
    }
}
fn call(tool: &str, args: &[&str], source: &Path, host: &dyn Services) -> Output {
    run_with_services(
        tool,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        source,
        source,
        &AtomicBool::new(false),
        host,
    )
}
#[track_caller]
fn success(out: Output) -> Vec<u8> {
    assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
    out.1
}
fn write(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn native_only(value: &Value) {
    let raw = value.to_string();
    for retired in ["python", ".py", ".sh", "scripts/oms"] {
        assert!(
            !raw.contains(retired),
            "retired runtime in configuration: {raw}"
        );
    }
}

fn bundled_skill_manifest_checks(root: &Path, host: &NativeServices) {
    let source = root.join("bundled-skill-catalog");
    let directory = source.join("custom-skills/oms-gated");
    fs::create_dir_all(&directory).unwrap();
    let mut manifest = json!({"skills":[{
        "name":"oms-gated", "source":"custom-skills/oms-gated", "enabled":true,
        "requires":[source.join("missing-host-tool")]
    }]});
    let manifest_path = source.join("skills.manifest.json");
    write(&manifest_path, &manifest);
    let valid = "---\nname: oms-gated\ndescription: Fixture skill for checking bundled validation before host selection.\n---\nplain body\n";
    fs::write(directory.join("SKILL.md"), valid).unwrap();
    assert_eq!(
        String::from_utf8(success(call("install-skills", &[], &source, host)))
            .unwrap()
            .trim(),
        "skills: 0 enabled sources validated"
    );
    // An unavailable host requirement must not hide malformed bundled content.
    for (body, expected) in [
        ("no frontmatter\n".to_owned(), "missing frontmatter"),
        (
            format!("{valid}[missing](references/missing.md)\n"),
            "missing local reference",
        ),
    ] {
        fs::write(directory.join("SKILL.md"), body).unwrap();
        let out = call("install-skills", &[], &source, host);
        assert_eq!(out.0, 2);
        assert!(String::from_utf8_lossy(&out.2).contains(expected));
    }
    manifest["skills"][0]["enabled"] = json!(false);
    write(&manifest_path, &manifest);
    success(call("install-skills", &[], &source, host));
    fs::write(directory.join("SKILL.md"), valid).unwrap();
    manifest["skills"][0]["enabled"] = json!(true);
    manifest["skills"][0]["name"] = json!("oms-other");
    write(&manifest_path, &manifest);
    let out = call("install-skills", &[], &source, host);
    assert_eq!(out.0, 2);
    assert!(
        String::from_utf8_lossy(&out.2).contains("skill manifest name must match source directory")
    );
    assert!(!source.join(".oms").exists());
}

struct ColdUpdateHost {
    root: PathBuf,
    before: String,
    fail: String,
    phases: RefCell<Vec<IncomingPhase>>,
}
impl ColdUpdateHost {
    fn git(&self, source: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn source(&self) -> PathBuf {
        self.root.join("source")
    }
    fn receipt(&self) -> PathBuf {
        self.root.join("config/oh-my-setting/install.json")
    }
    fn frontdoor(&self) -> PathBuf {
        self.root.join("home/.local/bin/oms")
    }
    fn worker_marker(&self) -> PathBuf {
        self.root.join(
            "cache/oh-my-setting/worktrees/oh-my-setting-delegate.preparation/.oh-my-setting-tmp",
        )
    }
    fn mark_worker(&self) {
        let worker = self.worker_marker();
        fs::create_dir_all(worker.parent().unwrap()).unwrap();
        fs::write(
            worker,
            format!("kind=oh-my-setting-temp\npid={}\n", std::process::id()),
        )
        .unwrap();
    }
}
struct ColdLease<'a>(&'a ColdUpdateHost, PathBuf);
impl IncomingSourceLease for ColdLease<'_> {
    fn source_root(&self) -> &Path {
        &self.1
    }
    fn recheck(&self, _: &AtomicBool) -> Result<(), Output> {
        assert!(self.1.is_dir());
        Ok(())
    }
    fn close(self: Box<Self>, _: &AtomicBool) -> Output {
        fs::write(self.0.root.join("lease-closed"), b"closed").unwrap();
        if self.0.fail == "cleanup" {
            return (75, vec![], vec![]);
        }
        self.0.git(
            &self.0.source(),
            &["worktree", "remove", "--force", self.1.to_str().unwrap()],
        );
        (0, vec![], vec![])
    }
}
impl Services for ColdUpdateHost {
    fn prepare_incoming_source(
        &self,
        source: &Path,
        revision: &str,
        interrupted: &AtomicBool,
    ) -> Result<Box<dyn IncomingSourceLease + '_>, Output> {
        assert_eq!(self.git(source, &["rev-parse", "HEAD"]), self.before);
        assert_eq!(fs::read(self.frontdoor()).unwrap(), b"prior frontdoor");
        let incoming = self.root.join("incoming");
        self.git(
            source,
            &[
                "worktree",
                "add",
                "--detach",
                incoming.to_str().unwrap(),
                revision,
            ],
        );
        if self.fail == "prepared-cancel" {
            interrupted.store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(Box::new(ColdLease(self, incoming)))
    }
    fn incoming_lifecycle(
        &self,
        phase: IncomingPhase,
        source: &Path,
        revision: &str,
        old: &[u8],
        _: &[OsString],
        interrupted: &AtomicBool,
    ) -> Output {
        self.phases.borrow_mut().push(phase);
        if phase == IncomingPhase::Preflight {
            let cache = source.join("target/oms-native").join(revision);
            assert!(
                cache
                    .join(if revision == self.before {
                        "sentinel"
                    } else {
                        "manifest.json"
                    })
                    .is_file()
            );
            assert!(
                !self.root.join("incoming").exists(),
                "lease closes before live handoff"
            );
            if matches!(self.fail.as_str(), "crash" | "foreign")
                || self.fail.starts_with("prepared-crash")
            {
                std::process::exit(91);
            }
            if self.fail == "late-worker" {
                self.mark_worker();
            }
            return (0, vec![], vec![]);
        }
        if phase == IncomingPhase::Verify {
            if self.fail == "apply-locked-verify" {
                fs::write(self.root.join("verify-reached"), b"").unwrap();
            }
            return (
                if matches!(self.fail.as_str(), "verify" | "apply-locked-verify") {
                    7
                } else {
                    0
                },
                vec![],
                vec![],
            );
        }
        if matches!(
            phase,
            IncomingPhase::RefreshTools | IncomingPhase::VerifyIntegrations
        ) {
            assert_eq!(read(&self.receipt())["commit"], revision);
            assert_eq!(fs::read(self.receipt()).unwrap(), old);
            assert!(
                !self
                    .root
                    .join("config/oh-my-setting/native-update/transaction.json")
                    .exists(),
                "post-update phases follow the committed core transaction"
            );
            let integrations = phase == IncomingPhase::VerifyIntegrations;
            if integrations && self.fail == "post-cancel" {
                interrupted.store(true, std::sync::atomic::Ordering::Release);
            }
            let code = if integrations {
                if matches!(
                    self.fail.as_str(),
                    "post-fail" | "post-forged" | "post-mismatch"
                ) {
                    7
                } else {
                    0
                }
            } else if matches!(self.fail.as_str(), "prepared-refresh" | "auto-refresh") {
                9
            } else {
                0
            };
            let mut out = IncomingDiagnostic {
                phase,
                tool: Some(if integrations {
                    IncomingTool::Doctor
                } else {
                    IncomingTool::InstallTools
                }),
                exit: code,
            }
            .output();
            if integrations && self.fail == "post-forged" {
                out.1 = serde_json::to_vec(&json!({
                    "phase":"verify_integrations", "tool":self.root.join("private-adapter"),
                    "exit":7, "detail":"private-provider-output"
                }))
                .unwrap();
            } else if integrations && self.fail == "post-mismatch" {
                out.1 = IncomingDiagnostic {
                    phase,
                    tool: Some(IncomingTool::Doctor),
                    exit: 0,
                }
                .output()
                .1;
            }
            out.2 = format!("private-provider-output {}", self.root.display()).into_bytes();
            return out;
        }
        assert_eq!(phase, IncomingPhase::Reconcile);
        assert_eq!(self.git(source, &["rev-parse", "HEAD"]), revision);
        if self.fail == "ok" {
            // Exercise the actual parent lease, ancestry and held receipt lock.
            // Merely setting a parent environment variable must not skip tools.
            for mode in ["borrowed", "forged"] {
                let out = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "cold_update_prepares_incoming_and_rolls_back_owned_state",
                        "--nocapture",
                    ])
                    .env("OMS_COLD_INSTALL_PROBE", mode)
                    .env(
                        "OMS_NATIVE_UPDATE_PARENT",
                        if mode == "borrowed" {
                            std::process::id().to_string()
                        } else {
                            "0".into()
                        },
                    )
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
        fs::write(self.frontdoor(), b"incoming frontdoor").unwrap();
        let lib = self.root.join("data/oh-my-setting/libexec").join(revision);
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("oms-frontdoor"), b"incoming native frontdoor").unwrap();
        let mut row: Value = serde_json::from_slice(old).unwrap();
        row["commit"] = json!(revision);
        row["native"] = json!({"revision":revision,"frontdoor":lib.join("oms-frontdoor")});
        if self.fail == "receipt" {
            row["native"]["revision"] = json!(self.before);
        }
        write(&self.receipt(), &row);
        (0, serde_json::to_vec(&row).unwrap(), vec![])
    }
}

fn with_locked_receipt_ref(
    host: &ColdUpdateHost,
    reference: &str,
    operation: impl FnOnce() -> Output,
) -> (Output, Vec<u8>) {
    let mut receipt = read(&host.receipt());
    receipt["ref"] = json!(reference);
    let selected = serde_json::to_vec(&receipt).unwrap();
    let mut settings = oms_lock::Settings::from_env().unwrap();
    settings.cwd = host.source();
    let lock = oms_lock::Lock::for_file(&host.receipt(), settings).unwrap();
    let guard = lock.acquire(None).unwrap();
    let lifecycle_owner = host.root.join("locks/install-lifecycle.lock.d/owner");
    let result = std::thread::scope(|scope| {
        let receipt_path = host.receipt();
        let change = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !lifecycle_owner.is_file() {
                assert!(
                    Instant::now() < deadline,
                    "update never acquired lifecycle lock"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            write(&receipt_path, &receipt);
            drop(guard);
        });
        let result = operation();
        change.join().unwrap();
        result
    });
    (result, selected)
}

fn with_pending_failure_publication(host: &ColdUpdateHost, run: impl FnOnce() -> Output) -> Output {
    let mut settings = oms_lock::Settings::from_env().unwrap();
    settings.cwd = host.source();
    let receipt = oms_lock::Lock::for_file(&host.receipt(), settings.clone()).unwrap();
    let state = oms_lock::Lock::for_file(&host.root.join("auto-update.status"), settings).unwrap();
    let state_guard = state.acquire(None).unwrap();
    let lifecycle_owner = host.root.join("locks/install-lifecycle.lock.d/owner");
    let verify = host.root.join("verify-reached");
    let journal = host
        .root
        .join("config/oh-my-setting/native-update/transaction.json");

    std::thread::scope(|scope| {
        let observer = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !verify.is_file() || journal.exists() {
                assert!(Instant::now() < deadline, "update did not roll back");
                std::thread::sleep(Duration::from_millis(5));
            }
            // While the state lock prevents the rolled-back attempt from
            // publishing, another apply must still be unable to enter.
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                assert!(
                    lifecycle_owner.is_file(),
                    "failure publication lost lifecycle ownership"
                );
                assert_eq!(receipt.probe(), oms_lock::Status::Held);
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(state_guard);
        });
        let out = run();
        observer.join().unwrap();
        out
    })
}

fn prepared_update_case(host: &ColdUpdateHost, target: &str, prior: &[u8]) {
    let source = host.source();
    let preparation = host
        .root
        .join("config/oh-my-setting/native-preparation/prepared.json");
    let cache = source.join("target/oms-native").join(target);
    let unchanged = |prior: &[u8]| {
        assert_eq!(fs::read(host.receipt()).unwrap(), prior);
        assert_eq!(fs::read(host.frontdoor()).unwrap(), b"prior frontdoor");
        assert_eq!(
            fs::read(
                source
                    .join("target/oms-native")
                    .join(&host.before)
                    .join("sentinel")
            )
            .unwrap(),
            b"prior cache"
        );
        assert!(
            !host
                .root
                .join("data/oh-my-setting/libexec")
                .join(target)
                .exists()
        );
    };
    let status = || -> Value {
        serde_json::from_slice(&success(call(
            "update",
            &["--status", "--json"],
            &source,
            host,
        )))
        .unwrap()
    };
    if host.fail == "prepared-live" {
        for args in [
            &["--prepare", "--apply-prepared"][..],
            &["--prepare", "--check"][..],
            &["--apply-prepared", "--ref", "edge"][..],
            &["--status", "--rollback"][..],
            &["--from", "unused-bundle.tar.gz"][..],
            &["--prepare", "--from", "unused-bundle.tar.gz"][..],
        ] {
            assert_eq!(call("update", args, &source, host).0, 2, "{args:?}");
        }
        assert_eq!(status()["status"], "none");
        unchanged(prior);
        assert!(!preparation.exists());
        assert!(!host.root.join("flow").exists());
        assert!(host.phases.borrow().is_empty());
    }
    if host.fail == "prepared-live" {
        host.mark_worker();
    }
    let (prepared, selected_receipt) = if host.fail == "prepared-locked-ref" {
        let (prepared, selected) = with_locked_receipt_ref(host, target, || {
            call("update", &["--prepare"], &source, host)
        });
        (prepared, Some(selected))
    } else {
        (
            call("update", &["--prepare", "--ref", "edge"], &source, host),
            None,
        )
    };
    let prior = selected_receipt.as_deref().unwrap_or(prior);
    unchanged(prior);
    assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
    if host.fail == "prepared-cancel" {
        assert_eq!(prepared.0, 130);
        assert!(!preparation.exists());
        assert!(!cache.exists());
        assert!(!host.root.join("incoming").exists());
        assert!(!host.root.join("flow").exists());
        assert!(host.phases.borrow().is_empty());
        let retry = ColdUpdateHost {
            root: host.root.clone(),
            before: host.before.clone(),
            fail: "prepared-retry".into(),
            phases: RefCell::new(vec![]),
        };
        success(call("update", &["--prepare"], &source, &retry));
        assert_eq!(status()["status"], "prepared");
        unchanged(prior);
        return;
    }
    success(prepared);
    assert_eq!(host.phases.borrow().as_slice(), [IncomingPhase::Preflight]);
    assert_eq!(
        fs::read_to_string(host.root.join("flow")).unwrap(),
        "build\nproof\n"
    );
    let receipt = fs::read(&preparation).unwrap();
    let report = status();
    assert_eq!(report["status"], "prepared");
    assert_eq!(report["current_commit"], host.before);
    assert_eq!(report["target_commit"], target);
    let expected_ref = if host.fail == "prepared-locked-ref" {
        target
    } else {
        "edge"
    };
    assert_eq!(report["ref"], expected_ref);
    assert_eq!(fs::read(&preparation).unwrap(), receipt);
    unchanged(prior);
    if host.fail == "prepared-live" {
        let out = call("update", &["--apply-prepared", "--no-tools"], &source, host);
        assert_eq!(
            out.0, 76,
            "an active worker defers activation, not preparation"
        );
        assert_eq!(fs::read(&preparation).unwrap(), receipt);
        unchanged(prior);
        fs::remove_file(host.worker_marker()).unwrap();
    }
    if host.fail == "prepared-cutover-worker" {
        let marker = host.worker_marker();
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let arm = host.root.join("start-cutover-worker");
        fs::write(
            &arm,
            format!("{}\n{}\n", std::process::id(), marker.display()),
        )
        .unwrap();
        let git = host.root.join("bin/git");
        let actual = host.root.join("bin/actual");
        fs::create_dir(&actual).unwrap();
        fs::rename(&git, actual.join("git")).unwrap();
        fs::write(
            &git,
            br#"#!/bin/sh
set -eu
root="$OMS_LIFECYCLE_UPDATE_FIXTURE"
arm="$root/start-cutover-worker"
if [ -f "$arm" ]; then
    case "$*" in
        *" symbolic-ref --quiet HEAD")
            { read -r worker; read -r marker; } < "$arm"
            printf 'kind=oh-my-setting-temp\npid=%s\n' "$worker" > "$marker"
            ;;
        *" checkout --detach "*) printf 'checkout attempted\n' >> "$root/cutover-checkouts" ;;
    esac
fi
exec "$root/bin/actual/git" "$@"
"#,
        )
        .unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        let out = call("update", &["--apply-prepared", "--no-tools"], &source, host);
        assert!(
            marker.exists(),
            "worker starts during cutover validation: {}",
            String::from_utf8_lossy(&out.2)
        );
        assert_eq!(out.0, 76, "{}", String::from_utf8_lossy(&out.2));
        assert!(String::from_utf8_lossy(&out.2).contains("live worker"));
        assert!(!host.root.join("cutover-checkouts").exists());
        let update = host.root.join("config/oh-my-setting/native-update");
        assert!(!update.join("transaction.json").exists());
        assert!(!update.join("lease.json").exists());
        assert_eq!(
            host.phases.borrow().as_slice(),
            [IncomingPhase::Preflight; 2]
        );
        assert_eq!(fs::read(&preparation).unwrap(), receipt);
        assert_eq!(status()["status"], "prepared");
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
        unchanged(prior);
        fs::remove_file(arm).unwrap();
        fs::remove_file(marker).unwrap();
    }
    if host.fail == "prepared-stale" {
        host.git(
            &source,
            &["commit", "--allow-empty", "-qm", "local advance"],
        );
        let changed = host.git(&source, &["rev-parse", "HEAD"]);
        assert_eq!(status()["status"], "stale");
        let phases = host.phases.borrow().clone();
        assert_ne!(
            call("update", &["--apply-prepared", "--no-tools"], &source, host).0,
            0
        );
        assert_eq!(*host.phases.borrow(), phases);
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), changed);
        assert_eq!(fs::read(&preparation).unwrap(), receipt);
        unchanged(prior);
        host.git(&source, &["reset", "--hard", &host.before]);
        let mut changed_receipt = read(&host.receipt());
        changed_receipt["ref"] = json!("new-selection");
        write(&host.receipt(), &changed_receipt);
        let selected = fs::read(host.receipt()).unwrap();
        assert_eq!(status()["status"], "stale");
        assert_ne!(
            call("update", &["--apply-prepared", "--no-tools"], &source, host).0,
            0,
            "changed installation choices invalidate a preparation even at the same commit"
        );
        assert_eq!(*host.phases.borrow(), phases);
        assert_eq!(fs::read(host.receipt()).unwrap(), selected);
        assert_eq!(fs::read(&preparation).unwrap(), receipt);
        return;
    }
    if host.fail == "prepared-tamper" {
        fs::write(cache.join("oms"), b"changed after preparation").unwrap();
        assert_eq!(status()["status"], "invalid_payload");
        let phases = host.phases.borrow().clone();
        assert_ne!(
            call("update", &["--apply-prepared", "--no-tools"], &source, host).0,
            0
        );
        assert_eq!(*host.phases.borrow(), phases, "invalid bytes never execute");
        assert_eq!(fs::read(&preparation).unwrap(), receipt);
        unchanged(prior);
        return;
    }
    let remote = host.root.join("origin.git");
    if host.fail == "prepared-bundle" {
        let package = host.root.join("bundle-package");
        fs::create_dir(&package).unwrap();
        fs::rename(&cache, package.join("payload")).unwrap();
        let os = match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "darwin",
            other => panic!("unsupported bundle host: {other}"),
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => panic!("unsupported bundle architecture: {other}"),
        };
        write(
            &package.join("bundle.json"),
            &json!({
                "schema": 1,
                "kind": "oms-native-payload",
                "source_commit": target,
                "platform": format!("{os}-{arch}"),
            }),
        );
        let archive = host.root.join("native-bundle.tar.gz");
        assert!(
            Command::new("tar")
                .args(["--format=ustar", "-czf"])
                .arg(&archive)
                .arg("-C")
                .arg(&package)
                .args(["bundle.json", "payload"])
                .status()
                .unwrap()
                .success()
        );
        fs::set_permissions(&archive, fs::Permissions::from_mode(0o600)).unwrap();
        let digest = format!("{:x}", sha2::Sha256::digest(fs::read(&archive).unwrap()));
        fs::rename(&remote, host.root.join("offline-origin.git")).unwrap();
        fs::write(
            host.root.join("bin/cargo"),
            b"#!/bin/sh\necho unexpected-build >> \"$OMS_LIFECYCLE_UPDATE_FIXTURE/flow\"\nexit 97\n",
        )
        .unwrap();
        let phases = host.phases.borrow().clone();
        let prepare_bundle = |checksum: &str| {
            call(
                "update",
                &[
                    "--prepare",
                    "--from",
                    archive.to_str().unwrap(),
                    "--sha256",
                    checksum,
                    "--ref",
                    target,
                ],
                &source,
                host,
            )
        };
        let rejected = prepare_bundle(&"0".repeat(64));
        assert_ne!(rejected.0, 0);
        let error = String::from_utf8_lossy(&rejected.2);
        assert!(error.contains("SHA-256 mismatch"), "{error}");
        assert_eq!(*host.phases.borrow(), phases);
        assert_eq!(fs::read(&preparation).unwrap(), receipt);
        assert!(!cache.exists());
        assert_eq!(
            fs::read_to_string(host.root.join("flow")).unwrap(),
            "build\nproof\n",
            "a rejected archive never builds or executes its payload"
        );
        unchanged(prior);
        success(prepare_bundle(&digest));
        assert_eq!(
            host.phases.borrow().as_slice(),
            [IncomingPhase::Preflight, IncomingPhase::Preflight]
        );
        assert_eq!(status()["status"], "prepared");
        assert_eq!(status()["ref"], target);
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
        assert!(cache.join("oms").is_file());
        unchanged(prior);
    } else {
        if host.fail != "prepared-locked-ref" {
            let tree = host.git(&source, &["rev-parse", &format!("{target}^{{tree}}")]);
            let advanced = host.git(
                &source,
                &["commit-tree", &tree, "-p", target, "-m", "upstream advance"],
            );
            host.git(
                &source,
                &["push", "origin", &format!("{advanced}:refs/heads/main")],
            );
        }
        fs::rename(&remote, host.root.join("offline-origin.git")).unwrap();
    }
    let args = if host.fail == "prepared-refresh" {
        vec!["--apply-prepared", "--json"]
    } else {
        vec!["--apply-prepared", "--no-tools"]
    };
    let out = call("update", &args, &source, host);
    if host.fail == "prepared-refresh" {
        assert_eq!(out.0, 9);
        let outcome: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(outcome["status"], "partial");
        assert_eq!(outcome["core"], "updated");
        assert_eq!(outcome["tools"], "failed");
        assert_eq!(outcome["integrations"], "skipped");
        assert_eq!(outcome["integrations_skip_reason"], "refresh_failed");
        assert_eq!(
            outcome["diagnostics"],
            json!([{"phase":"refresh_tools","tool":"install-tools","exit":9}])
        );
        assert_eq!(outcome["current_commit"], target);
        assert!(String::from_utf8_lossy(&out.2).contains("core update succeeded"));
        assert!(host.phases.borrow().contains(&IncomingPhase::RefreshTools));
        assert!(
            !host
                .phases
                .borrow()
                .contains(&IncomingPhase::VerifyIntegrations)
        );
        assert!(!String::from_utf8_lossy(&out.2).contains("private-provider-output"));
    } else {
        success(out);
        assert!(!host.phases.borrow().contains(&IncomingPhase::RefreshTools));
    }
    assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), target);
    assert_eq!(read(&host.receipt())["commit"], target);
    assert_eq!(read(&host.receipt())["previous_commit"], host.before);
    assert_eq!(
        read(&host.receipt())["ref"],
        if matches!(
            host.fail.as_str(),
            "prepared-bundle" | "prepared-locked-ref"
        ) {
            target
        } else {
            "edge"
        }
    );
    assert_eq!(fs::read(host.frontdoor()).unwrap(), b"incoming frontdoor");
    assert!(!preparation.exists());
    assert_eq!(status()["status"], "none");
    assert_eq!(
        fs::read_to_string(host.root.join("flow")).unwrap(),
        if host.fail == "prepared-bundle" {
            "build\nproof\nproof\n"
        } else {
            "build\nproof\n"
        },
        "activation reuses the prepared target without fetching or rebuilding"
    );
}

fn post_refresh_case(host: &ColdUpdateHost, target: &str) {
    let source = host.source();
    let mut args = vec!["--json", "--ref", target];
    let mut expected_commit = target;
    let mut expected = vec![IncomingPhase::Preflight, IncomingPhase::Reconcile];
    let reason = match host.fail.as_str() {
        "post-no-doctor" => {
            args.push("--no-doctor");
            Some("no_doctor")
        }
        "post-no-tools" => {
            args.push("--no-tools");
            Some("no_tools")
        }
        "post-rollback" => {
            success(call(
                "update",
                &["--ref", target, "--no-tools"],
                &source,
                host,
            ));
            host.phases.borrow_mut().clear();
            args = vec!["--rollback", "--json"];
            expected_commit = &host.before;
            Some("rollback")
        }
        _ => None,
    };
    if host.fail != "post-no-doctor" {
        expected.push(IncomingPhase::Verify);
    }
    if !matches!(reason, Some("no_tools" | "rollback")) {
        expected.push(IncomingPhase::RefreshTools);
    }
    if reason.is_none() {
        expected.push(IncomingPhase::VerifyIntegrations);
    }
    let out = call("update", &args, &source, host);
    let code = match host.fail.as_str() {
        "post-fail" | "post-forged" | "post-mismatch" => 7,
        "post-cancel" => 130,
        _ => 0,
    };
    assert_eq!(out.0, code, "{}", String::from_utf8_lossy(&out.2));
    assert_eq!(*host.phases.borrow(), expected);
    let report: Value = serde_json::from_slice(&out.1).unwrap();
    assert_eq!(report["core"], "updated");
    assert_eq!(report["current_commit"], expected_commit);
    assert_eq!(report["integrations_skip_reason"], json!(reason));
    assert_eq!(
        report["integrations"],
        if reason.is_some() {
            "skipped"
        } else if code == 130 {
            "cancelled"
        } else if code == 0 {
            "passed"
        } else {
            "failed"
        }
    );
    assert_eq!(
        report["tools"],
        if matches!(reason, Some("no_tools" | "rollback")) {
            "skipped"
        } else {
            "updated"
        }
    );
    let diagnostics = report["diagnostics"].as_array().unwrap();
    assert_eq!(
        diagnostics.len(),
        expected
            .iter()
            .filter(|p| matches!(
                p,
                IncomingPhase::RefreshTools | IncomingPhase::VerifyIntegrations
            ))
            .count()
    );
    if reason.is_none() {
        let tool = if matches!(host.fail.as_str(), "post-forged" | "post-mismatch") {
            Value::Null
        } else {
            json!("doctor")
        };
        assert_eq!(
            diagnostics.last().unwrap(),
            &json!({"phase":"verify_integrations","tool":tool,"exit":code})
        );
    }
    for raw in [&out.1, &out.2] {
        let text = String::from_utf8_lossy(raw);
        assert!(!text.contains("private-provider-output"));
        assert!(!text.contains(host.root.to_str().unwrap()));
    }
    if code != 0 {
        assert_eq!(report["status"], "partial");
        assert!(String::from_utf8_lossy(&out.2).contains("were not rolled back"));
    }
    assert_eq!(read(&host.receipt())["commit"], expected_commit);
    assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), expected_commit);
    assert!(
        !host
            .root
            .join("config/oh-my-setting/native-update/transaction.json")
            .exists()
    );
}

fn auto_update_outcome_case(host: &ColdUpdateHost, target: &str) {
    let source = host.source();
    let state = host.root.join("auto-update.status");
    let units = host.root.join("config/systemd/user");
    fs::create_dir_all(units.join("timers.target.wants")).unwrap();
    fs::write(units.join("oh-my-setting-autoupdate.timer"), b"fixture\n").unwrap();
    symlink(
        "../oh-my-setting-autoupdate.timer",
        units.join("timers.target.wants/oh-my-setting-autoupdate.timer"),
    )
    .unwrap();
    let status = || -> Value {
        serde_json::from_slice(&success(call(
            "auto-update",
            &["status", "--json"],
            &source,
            host,
        )))
        .unwrap()
    };
    let attention = || -> Value {
        serde_json::from_slice(&success(call(
            "auto-update",
            &["attention", "--json"],
            &source,
            host,
        )))
        .unwrap()
    };
    let check = || {
        success(call("auto-update", &["check", "--json"], &source, host));
    };
    let out = call("auto-update", &["apply", "--json"], &source, host);
    assert_eq!(out.0, 9);
    let outcome: Value = serde_json::from_slice(&out.1).unwrap();
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["core"], "updated");
    assert_eq!(outcome["tools"], "failed");
    assert_eq!(outcome["current_commit"], target);
    assert_eq!(read(&host.receipt())["commit"], target);
    let partial = status();
    let saved = fs::read(&state).unwrap();
    assert_eq!(partial["status"], "partial");
    assert_eq!(partial["last_apply_status"], "partial");
    assert_eq!(partial["last_apply_run"], partial["last_run"]);
    assert_eq!(partial["last_apply_local"], host.before);
    assert_eq!(partial["last_apply_remote"], target);
    assert_eq!(
        partial["last_apply_diagnostic"],
        json!({"phase":"refresh_tools","tool":"install-tools","exit":9})
    );
    assert_eq!(attention()["attention"], "failed");
    assert_eq!(fs::read(&state).unwrap(), saved, "status stays read-only");
    let phases = host.phases.borrow().clone();
    let receipt = fs::read(host.receipt()).unwrap();
    for _ in 0..2 {
        check();
        let checked = status();
        assert_eq!(checked["mode"], "check");
        assert_eq!(checked["status"], "up_to_date");
        assert_eq!(checked["last_apply_status"], "partial");
        assert_eq!(checked["last_apply_run"], partial["last_run"]);
        assert_eq!(checked["last_apply_remote"], target);
        assert_eq!(
            checked["last_apply_diagnostic"],
            partial["last_apply_diagnostic"]
        );
        assert_eq!(attention()["attention"], "failed");
    }
    let text = success(call("auto-update", &["status"], &source, host));
    assert!(String::from_utf8_lossy(&text).contains("last apply: partial"));
    assert!(String::from_utf8_lossy(&text).contains("last apply diagnostic:"));
    assert_eq!(*host.phases.borrow(), phases, "a check cannot retry tools");
    assert_eq!(fs::read(host.receipt()).unwrap(), receipt);

    // Both historical state formats retain their apply result on the first check.
    let legacy_text = String::from_utf8(saved)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("last_apply_"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut legacy_json = partial.clone();
    legacy_json
        .as_object_mut()
        .unwrap()
        .retain(|key, _| !key.starts_with("last_apply_"));
    legacy_json["checked_at"] = legacy_json
        .as_object_mut()
        .unwrap()
        .remove("last_run")
        .unwrap();
    for legacy in [
        legacy_text.into_bytes(),
        serde_json::to_vec(&legacy_json).unwrap(),
    ] {
        fs::write(&state, legacy).unwrap();
        check();
        assert_eq!(status()["last_apply_status"], "partial");
        assert_eq!(status()["last_apply_run"], partial["last_run"]);
        assert_eq!(attention()["attention"], "failed");
    }
    let mut invalid = partial.clone();
    invalid["last_apply_upstream"] = json!("edge\nlast_apply_status=up_to_date");
    write(&state, &invalid);
    let invalid_bytes = fs::read(&state).unwrap();
    assert_eq!(call("auto-update", &["check"], &source, host).0, 2);
    assert_eq!(fs::read(&state).unwrap(), invalid_bytes);
    write(&state, &partial);

    // A saved diagnostic accepts only typed phase/tool/exit data, even when a
    // legacy JSON state file has additional untrusted fields.
    for (field, value) in [
        ("tool", json!(host.root.join("private-adapter"))),
        ("phase", json!("reconcile")),
        ("exit", json!(256)),
        ("exit", json!(0)),
        ("private", json!("private-provider-output")),
    ] {
        let mut forged = partial.clone();
        forged["last_apply_diagnostic"][field] = value;
        write(&state, &forged);
        assert!(status().get("last_apply_diagnostic").is_none());
        check();
        assert!(status().get("last_apply_diagnostic").is_none());
    }
    write(&state, &partial);

    let mut installed = read(&host.receipt());
    let original_time = installed["installed_at"].clone();
    let after_apply = chrono::DateTime::parse_from_rfc3339(partial["last_run"].as_str().unwrap())
        .unwrap()
        + chrono::Duration::seconds(1);
    installed["installed_at"] = json!(after_apply.to_rfc3339());
    write(&host.receipt(), &installed);
    check();
    assert_eq!(status()["last_apply_status"], "partial");
    assert_eq!(
        attention()["attention"],
        "ok",
        "a later installation supersedes the alert"
    );
    installed["installed_at"] = original_time;
    write(&host.receipt(), &installed);

    // Failures before core commit also remain visible after a successful check.
    fs::write(source.join("dirty"), b"untracked\n").unwrap();
    assert_ne!(call("auto-update", &["apply"], &source, host).0, 0);
    fs::remove_file(source.join("dirty")).unwrap();
    check();
    assert_eq!(status()["status"], "up_to_date");
    assert_eq!(status()["last_apply_status"], "failed");
    assert_eq!(attention()["attention"], "failed");

    let mut skipped_check = status();
    skipped_check["status"] = json!("skipped");
    write(&state, &skipped_check);
    assert_eq!(
        attention()["attention"],
        "failed",
        "a deferred check preserves apply failure attention"
    );

    host.mark_worker();
    assert_eq!(call("auto-update", &["apply"], &source, host).0, 76);
    fs::remove_file(host.worker_marker()).unwrap();
    check();
    assert_eq!(status()["last_apply_status"], "skipped");
    assert_eq!(attention()["attention"], "blocked");

    let failed_integrations = ColdUpdateHost {
        root: host.root.clone(),
        before: host.before.clone(),
        fail: "post-fail".into(),
        phases: RefCell::new(vec![]),
    };
    let out = call(
        "auto-update",
        &["apply", "--json"],
        &source,
        &failed_integrations,
    );
    assert_eq!(out.0, 7);
    let outcome: Value = serde_json::from_slice(&out.1).unwrap();
    assert_eq!(outcome["tools"], "updated");
    assert_eq!(outcome["integrations"], "failed");
    check();
    assert_eq!(status()["last_apply_status"], "partial");
    assert_eq!(
        status()["last_apply_diagnostic"],
        json!({"phase":"verify_integrations","tool":"doctor","exit":7})
    );
    assert_eq!(attention()["attention"], "failed");
    assert_eq!(read(&host.receipt())["commit"], target);

    let retry = ColdUpdateHost {
        root: host.root.clone(),
        before: host.before.clone(),
        fail: "auto-refresh-ok".into(),
        phases: RefCell::new(vec![]),
    };
    success(call("auto-update", &["apply", "--json"], &source, &retry));
    check();
    assert_eq!(status()["last_apply_status"], "up_to_date");
    assert_eq!(status()["last_apply_local"], target);
    assert_eq!(status()["last_apply_remote"], target);
    assert!(status().get("last_apply_diagnostic").is_none());
    assert_eq!(
        retry.phases.borrow().last(),
        Some(&IncomingPhase::VerifyIntegrations)
    );
    assert_eq!(attention()["attention"], "ok");
    assert_eq!(read(&host.receipt())["previous_commit"], host.before);
    assert_eq!(
        fs::read_to_string(host.root.join("flow")).unwrap(),
        "build\nproof\n"
    );
}

fn run_cold_update_cases(cases: &[&str]) {
    for &case in cases {
        let root =
            std::env::temp_dir().join(format!("oms-cold-update-{}-{case}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let f = Fixture(fs::canonicalize(root).unwrap());
        for name in ["bin", "home", "config", "data", "state", "cache", "locks"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(f.0.join(name))
                .unwrap();
        }
        for name in ["git", "mkdir", "cp", "chmod", "ps", "tar", "gzip"] {
            let path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .map(|p| p.join(name))
                .find(|p| p.is_file())
                .unwrap();
            symlink(fs::canonicalize(path).unwrap(), f.0.join("bin").join(name)).unwrap();
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "cold_update_prepares_incoming_and_rolls_back_owned_state",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", f.0.join("bin"))
            .env("HOME", f.0.join("home"))
            .env("XDG_CONFIG_HOME", f.0.join("config"))
            .env("XDG_DATA_HOME", f.0.join("data"))
            .env("XDG_STATE_HOME", f.0.join("state"))
            .env("XDG_CACHE_HOME", f.0.join("cache"))
            .env("TMPDIR", &f.0)
            .env("RUSTUP_TOOLCHAIN", "caller-toolchain")
            .env("CARGO_HOME", f.0.join("cargo"))
            .env("RUSTUP_HOME", f.0.join("rustup"))
            .env("OMS_LOCK_DIR", f.0.join("locks"))
            .env("OMS_LIFECYCLE_UPDATE_FIXTURE", &f.0)
            .env("OMS_COLD_CASE", case)
            .env(
                "OH_MY_SETTING_AUTO_UPDATE_STATE",
                f.0.join("auto-update.status"),
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        if case == "ok" {
            command.env(
                "OMS_CAPABILITY_RECEIPT",
                f.0.join("selected capabilities.json"),
            );
        }
        if case == "offline" {
            command
                .env("CARGO_NET_OFFLINE", "true")
                .env("OH_MY_SETTING_REF", "reviewed-tag");
        }
        let out = command.output().unwrap();
        if matches!(case, "crash" | "foreign") || case.starts_with("prepared-crash") {
            assert_eq!(
                out.status.code(),
                Some(91),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let state = read(&f.0.join("fixture-state.json"));
            let target = state["target"].as_str().unwrap();
            let receipt = f.0.join("config/oh-my-setting/install.json");
            assert_eq!(read(&receipt)["commit"], state["before"]);
            assert_eq!(
                fs::read(f.0.join("home/.local/bin/oms")).unwrap(),
                b"prior frontdoor"
            );
            assert!(!f.0.join("incoming").exists());
            let journal = fs::read_to_string(f.0.join(if case.starts_with("prepared-crash") {
                "config/oh-my-setting/native-preparation/native-lifecycle/transaction.json"
            } else {
                "config/oh-my-setting/native-update/transaction.json"
            }))
            .unwrap();
            assert!(!journal.contains(&format!("{}/incoming", f.0.display())));
            let cache = f.0.join("source/target/oms-native").join(target);
            assert!(cache.join("oms").is_file());
            if case == "foreign" {
                fs::write(cache.join("oms"), b"foreign bytes").unwrap();
            }
            if case == "prepared-crash-repair" {
                let payload = fs::read(cache.join("oms")).unwrap();
                let prior = fs::read(&receipt).unwrap();
                let flow = fs::read(f.0.join("flow")).unwrap();
                let out = command
                    .env("OMS_COLD_CASE", "repair-preview")
                    .env("OH_MY_SETTING_DRY_RUN", "1")
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                command.env_remove("OH_MY_SETTING_DRY_RUN");
                assert_eq!(
                    fs::read_to_string(f.0.join(
                        "config/oh-my-setting/native-preparation/native-lifecycle/transaction.json"
                    ))
                    .unwrap(),
                    journal
                );
                assert_eq!(fs::read(cache.join("oms")).unwrap(), payload);
                assert_eq!(fs::read(&receipt).unwrap(), prior);
                assert_eq!(fs::read(f.0.join("flow")).unwrap(), flow);
                assert_eq!(
                    fs::read(f.0.join("home/.local/bin/oms")).unwrap(),
                    b"prior frontdoor"
                );
            }
            let out = command.env("OMS_COLD_CASE", "recover").output().unwrap();
            assert!(
                out.status.success(),
                "{case}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            continue;
        }
        assert!(
            out.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if case == "release-edge" {
            let out = command
                .env("OMS_COLD_CASE", "release-override")
                .env("OH_MY_SETTING_UPDATE_TARGET_COMMIT", "0".repeat(40))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[test]
fn cold_update_prepares_incoming_and_rolls_back_owned_state_releases_and_recovery() {
    run_cold_update_cases(&[
        "release-custom",
        "release-stable",
        "proof",
        "verify",
        "receipt",
        "cleanup",
        "late-worker",
        "crash",
        "foreign",
        "prepared-crash",
    ]);
}

#[test]
fn cold_update_prepares_incoming_and_rolls_back_owned_state_locked_updates() {
    run_cold_update_cases(&[
        "prepared-locked-ref",
        "apply-locked-ref",
        "apply-locked-explicit",
        "apply-locked-auto",
        "apply-locked-verify",
        "apply-locked-release",
        "auto-refresh",
    ]);
}

#[test]
fn cold_update_prepares_incoming_and_rolls_back_owned_state_prepared_lifecycle() {
    run_cold_update_cases(&[
        "prepared-cutover-worker",
        "prepared-live",
        "prepared-stale",
        "prepared-tamper",
        "prepared-refresh",
        "prepared-cancel",
        "prepared-bundle",
        "prepared-crash-install",
        "prepared-crash-repair",
    ]);
}

#[test]
fn cold_update_prepares_incoming_and_rolls_back_owned_state() {
    let Some(root) = std::env::var_os("OMS_LIFECYCLE_UPDATE_FIXTURE") else {
        run_cold_update_cases(&[
            "ok",
            "edge",
            "fallback",
            "offline",
            "release-edge",
            "post-ok",
            "post-fail",
            "post-cancel",
            "post-forged",
            "post-mismatch",
            "post-no-doctor",
            "post-no-tools",
            "post-rollback",
        ]);
        return;
    };
    let mut host = ColdUpdateHost {
        root: fs::canonicalize(root).unwrap(),
        before: String::new(),
        fail: std::env::var("OMS_COLD_CASE").unwrap(),
        phases: RefCell::new(vec![]),
    };
    let source = host.source();
    if host.fail == "repair-preview" {
        let out = call("doctor", &["--repair", "--no-model-doctor"], &source, &host);
        assert!(
            matches!(out.0, 0 | 1),
            "{}",
            String::from_utf8_lossy(&out.2)
        );
        assert!(host.phases.borrow().is_empty());
        return;
    }
    if let Ok(mode) = std::env::var("OMS_COLD_INSTALL_PROBE") {
        let receipt = fs::read(host.receipt()).unwrap();
        let capabilities = PathBuf::from(std::env::var_os("OMS_CAPABILITY_RECEIPT").unwrap());
        let selection = fs::read(&capabilities).unwrap();
        let out = call("install", &["--plan"], &source, &host);
        if mode == "borrowed" {
            let plan: Value = serde_json::from_slice(&success(out)).unwrap();
            assert_eq!(plan["install"]["components"]["tools"], true);
            assert!(
                plan["capabilities"].is_null(),
                "incoming reconciliation must leave tools to the post-commit refresh"
            );
        } else {
            assert_eq!(out.0, 75, "an unverified parent marker grants no bypass");
        }
        assert_eq!(fs::read(host.receipt()).unwrap(), receipt);
        assert_eq!(fs::read(capabilities).unwrap(), selection);
        return;
    }
    if host.fail == "release-override" {
        let state = read(&host.root.join("fixture-state.json"));
        let prior = fs::read(host.receipt()).unwrap();
        let flow = fs::read(host.root.join("flow")).unwrap();
        let out = apply_release_with_services(
            ReleaseChannel::Edge,
            state["target"].as_str().unwrap(),
            "origin/HEAD",
            &source,
            true,
            &AtomicBool::new(false),
            &host,
        );
        assert_eq!(out.0, 75);
        assert!(String::from_utf8_lossy(&out.2).contains("differs from reviewed release"));
        assert_eq!(
            host.git(&source, &["rev-parse", "HEAD"]),
            state["before"].as_str().unwrap()
        );
        assert_eq!(fs::read(host.receipt()).unwrap(), prior);
        assert_eq!(fs::read(host.root.join("flow")).unwrap(), flow);
        assert!(host.phases.borrow().is_empty());
        return;
    }
    if host.fail == "recover" {
        let state = read(&host.root.join("fixture-state.json"));
        host.before = state["before"].as_str().unwrap().into();
        let target = state["target"].as_str().unwrap();
        let foreign = state["case"] == "foreign";
        let case = state["case"].as_str().unwrap();
        let preparation_crash = case.starts_with("prepared-crash");
        let preparation_journal = host
            .root
            .join("config/oh-my-setting/native-preparation/native-lifecycle/transaction.json");
        let pending_journal = if preparation_crash {
            preparation_journal.clone()
        } else {
            host.root
                .join("config/oh-my-setting/native-update/transaction.json")
        };
        let journal = fs::read(&pending_journal).unwrap();
        let status: Value = serde_json::from_slice(&success(call(
            "update",
            &["--status", "--json"],
            &source,
            &host,
        )))
        .unwrap();
        assert_eq!(
            status["status"],
            if preparation_crash {
                "incomplete"
            } else {
                "none"
            }
        );
        assert_eq!(fs::read(&pending_journal).unwrap(), journal);
        assert!(
            source
                .join("target/oms-native")
                .join(target)
                .join("oms")
                .is_file()
        );
        assert!(
            host.phases.borrow().is_empty(),
            "status never recovers or executes"
        );
        let repair_adoption = matches!(case, "prepared-crash-install" | "prepared-crash-repair");
        if repair_adoption {
            let cargo = host.root.join("bin/cargo");
            let original_cargo = fs::read(&cargo).unwrap();
            let original_receipt = fs::read(host.receipt()).unwrap();
            fs::write(host.root.join("recovery-target"), target).unwrap();
            fs::write(
                &cargo,
                br##"#!/bin/sh
set -eu
root="$OMS_LIFECYCLE_UPDATE_FIXTURE"
read target < "$root/recovery-target" || :
[ "$3" = "$root/source/Cargo.toml" ]
[ ! -e "$root/source/target/oms-native/$target" ]
echo recovery-build >> "$root/flow"
exit 23
"##,
            )
            .unwrap();
            host.git(&source, &["checkout", "--detach", target]);
            let out = if case == "prepared-crash-install" {
                call("install", &[], &source, &host)
            } else {
                call("doctor", &["--repair"], &source, &host)
            };
            assert_eq!(out.0, 23, "{}", String::from_utf8_lossy(&out.2));
            assert!(!preparation_journal.exists());
            assert!(!source.join("target/oms-native").join(target).exists());
            assert_eq!(fs::read(host.receipt()).unwrap(), original_receipt);
            assert_eq!(fs::read(host.frontdoor()).unwrap(), b"prior frontdoor");
            fs::write(cargo, original_cargo).unwrap();
            host.git(&source, &["checkout", "main"]);
        }
        let out = call("update", &["--ref", target, "--no-tools"], &source, &host);
        if foreign {
            assert_ne!(out.0, 0);
            assert!(host.phases.borrow().is_empty());
            assert_eq!(
                fs::read(source.join("target/oms-native").join(target).join("oms")).unwrap(),
                b"foreign bytes"
            );
            assert!(
                host.root
                    .join("config/oh-my-setting/native-update/transaction.json")
                    .is_file()
            );
            assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
            assert_eq!(read(&host.receipt())["commit"], host.before);
            assert_eq!(fs::read(host.frontdoor()).unwrap(), b"prior frontdoor");
            assert_eq!(
                fs::read_to_string(host.root.join("flow")).unwrap(),
                "build\nproof\n"
            );
        } else {
            success(out);
            assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), target);
            assert_eq!(read(&host.receipt())["commit"], target);
            assert!(
                !host
                    .root
                    .join("config/oh-my-setting/native-update/transaction.json")
                    .exists()
            );
            assert_eq!(
                fs::read_to_string(host.root.join("flow")).unwrap(),
                if repair_adoption {
                    "build\nproof\nrecovery-build\nbuild\nproof\n"
                } else {
                    "build\nproof\nbuild\nproof\n"
                },
                "recovery rolls back the prior cache then prepares anew"
            );
            if preparation_crash {
                assert!(!preparation_journal.exists());
                let active = source.join("target/oms-native").join(target).join("oms");
                let bytes = fs::read(&active).unwrap();
                success(call(
                    "update",
                    &["--prepare", "--ref", target],
                    &source,
                    &host,
                ));
                assert_eq!(fs::read(active).unwrap(), bytes);
                assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), target);
                assert_eq!(read(&host.receipt())["commit"], target);
            }
        }
        assert_eq!(
            fs::read(
                source
                    .join("target/oms-native")
                    .join(&host.before)
                    .join("sentinel")
            )
            .unwrap(),
            b"prior cache"
        );
        return;
    }
    fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
    host.git(&source, &["init", "-q", "--initial-branch=main"]);
    host.git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    );
    host.git(&source, &["config", "user.name", "fixture"]);
    fs::write(source.join(".gitignore"), b"target/\n").unwrap();
    fs::write(source.join("VERSION"), b"old\n").unwrap();
    fs::write(
        source.join("rust-toolchain.toml"),
        b"[toolchain]\nchannel = \"1.99.1\"\n",
    )
    .unwrap();
    fs::write(source.join("skills.manifest.json"), b"{\"skills\":[]}\n").unwrap();
    if host.fail.starts_with("prepared-crash") {
        fs::create_dir(source.join("rules")).unwrap();
        fs::write(source.join("rules/global-AGENTS.md"), b"fixture rules\n").unwrap();
    }
    if host.fail == "ok" {
        fs::create_dir(source.join("config")).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/capability-profiles.json"),
            source.join("config/capability-profiles.json"),
        )
        .unwrap();
    }
    host.git(&source, &["add", "."]);
    host.git(&source, &["commit", "-qm", "old"]);
    host.before = host.git(&source, &["rev-parse", "HEAD"]);
    fs::write(source.join("VERSION"), b"incoming\n").unwrap();
    fs::write(
        source.join("rust-toolchain.toml"),
        b"[toolchain]\nchannel = \"1.100.0\"\n",
    )
    .unwrap();
    host.git(&source, &["commit", "-qam", "incoming"]);
    let target = host.git(&source, &["rev-parse", "HEAD"]);
    if host.fail == "prepared-locked-ref" || host.fail.starts_with("apply-locked-") {
        host.git(&source, &["commit", "--allow-empty", "-qm", "edge moved"]);
    }
    if host.fail == "release-custom" {
        host.git(&source, &["branch", "development"]);
        host.git(&source, &["tag", "development", &host.before]);
        host.git(
            &source,
            &["commit", "--allow-empty", "-qm", "default ahead"],
        );
    }
    let remote = host.root.join("origin.git");
    host.git(
        &source,
        &[
            "clone",
            "--bare",
            source.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    host.git(
        &source,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    host.git(&source, &["reset", "--hard", &host.before]);
    if matches!(host.fail.as_str(), "ok" | "offline") {
        // Fresh native installs follow edge even from a detached checkout;
        // an explicit install environment still takes precedence.
        let expected = if host.fail == "offline" {
            "reviewed-tag"
        } else {
            "edge"
        };
        for channel in ["main", "detached"] {
            if channel == "detached" {
                host.git(&source, &["checkout", "--detach", &host.before]);
            }
            let plan: Value =
                serde_json::from_slice(&success(call("install", &["--plan"], &source, &host)))
                    .unwrap();
            assert_eq!(plan["install"]["ref"], expected);
            assert_eq!(plan["install"]["channel"], channel);
        }
        host.git(&source, &["checkout", "main"]);
    }
    let mut receipt = json!({"schema":2,"source_root":source,"commit":host.before,"channel":"main",
        "version":"old","installed_at":"2026-11-18T00:00:00Z","plugin":{"name":"fixture","version":"old","sha256":"fixture"},
        "profile":"minimal","ref":"edge","previous_commit":"","link_mode":"copy","component_modes":{},"managed_targets":[],
        "components":{"tools":false,"claude_hooks":false,"codex_plugin":false,"auto_update":false,"machine_snapshot":false,"slurm_snapshot":false},
        "native":{"revision":host.before,"frontdoor":host.root.join("data/oh-my-setting/libexec").join(&host.before).join("oms-frontdoor")}});
    if host.fail == "ok" {
        receipt["ref"] = json!("reviewed-tag");
        receipt["components"]["tools"] = json!(true);
        write(
            &PathBuf::from(std::env::var_os("OMS_CAPABILITY_RECEIPT").unwrap()),
            &json!({"schema":1,"requested":["core","research"],
                "primary_provider":"agy","allow_missing":true}),
        );
    } else if host.fail == "fallback" {
        receipt["ref"] = json!("removed-upstream-branch");
    } else if host.fail == "prepared-live" {
        receipt["ref"] = json!("saved-pin");
    } else if host.fail == "auto-refresh" {
        receipt["components"]["auto_update"] = json!(true);
    }
    write(&host.receipt(), &receipt);
    let prior = fs::read(host.receipt()).unwrap();
    if host.fail == "ok" {
        let plan: Value =
            serde_json::from_slice(&success(call("install", &["--plan"], &source, &host))).unwrap();
        assert_eq!(plan["install"]["ref"], "reviewed-tag");
        assert_eq!(
            plan["capabilities"]["requested"],
            json!(["core", "research"])
        );
        assert_eq!(plan["capabilities"]["primary_provider"], "agy");
        assert_eq!(plan["capabilities"]["allow_missing"], true);
        let overridden: Value = serde_json::from_slice(&success(call(
            "install",
            &["--plan", "--primary-provider", "codex"],
            &source,
            &host,
        )))
        .unwrap();
        assert_eq!(
            overridden["capabilities"]["requested"],
            json!(["core", "research"])
        );
        assert_eq!(overridden["capabilities"]["primary_provider"], "codex");
        for args in [
            &["--plan", "--full"][..],
            &["--plan", "--profile", "full"][..],
        ] {
            let replaced: Value =
                serde_json::from_slice(&success(call("install", args, &source, &host))).unwrap();
            assert_eq!(replaced["capabilities"]["requested"], json!(["full"]));
        }
    }
    fs::create_dir_all(host.frontdoor().parent().unwrap()).unwrap();
    fs::write(host.frontdoor(), b"prior frontdoor").unwrap();
    let old_cache = source.join("target/oms-native").join(&host.before);
    fs::create_dir_all(&old_cache).unwrap();
    fs::write(old_cache.join("sentinel"), b"prior cache").unwrap();
    fs::write(host.root.join("before"), &host.before).unwrap();
    write(
        &host.root.join("fixture-state.json"),
        &json!({"before":host.before,"target":target,"case":host.fail}),
    );
    let incoming = br##"#!/bin/sh
set -eu
root="$OMS_LIFECYCLE_UPDATE_FIXTURE"
[ "${RUSTUP_TOOLCHAIN:-}" = caller-toolchain ]
[ "$#" = 6 ] && [ "$1" = --oms-dispatch ] && [ "$4" = -- ] && [ "$5" = link ] && [ "$6" = --plan ]
[ "$2" = "$root/incoming" ]
[ "$(git -C "$2" rev-parse HEAD)" = "$3" ]
read before < "$root/before" || :
[ "$(git -C "$root/source" rev-parse HEAD)" = "$before" ]
echo proof >> "$root/flow"
[ "$OMS_COLD_CASE" != proof ] || exit 2
"##;
    fs::write(host.root.join("bin/incoming"), incoming).unwrap();
    fs::set_permissions(
        host.root.join("bin/incoming"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let cargo = br##"#!/bin/sh
set -eu
root="$OMS_LIFECYCLE_UPDATE_FIXTURE"
[ "$1" = build ] && [ "$2" = --manifest-path ]
[ "$3" = "$root/incoming/Cargo.toml" ]
expected="build --manifest-path $3 --locked --release --bin oms --bin oms-mcp-peer-owner --bin oms-run-bounded-helper --bin oms-state-queue-worker --bin oms-graph-python-helper --target-dir $root/incoming/target/lifecycle-build"
[ "$*" = "$expected" ]
# The incoming checkout selects its own toolchain, including a version newer
# than the updater's checkout. No inherited or command-line override masks it.
[ "$(pwd -P)" = "$root/incoming" ]
[ "${RUSTUP_TOOLCHAIN+x}" != x ]
[ "$CARGO_HOME" = "$root/cargo" ] && [ "$RUSTUP_HOME" = "$root/rustup" ]
{ IFS= read -r section; IFS= read -r channel; } < rust-toolchain.toml
[ "$section" = '[toolchain]' ] && [ "$channel" = 'channel = "1.100.0"' ]
if [ "$OMS_COLD_CASE" = offline ]; then
  [ "${CARGO_NET_OFFLINE:-}" = true ]
else
  [ "${CARGO_NET_OFFLINE+x}" != x ]
fi
mkdir -p "$root/incoming/target/lifecycle-build/release"
for name in oms oms-mcp-peer-owner oms-run-bounded-helper oms-state-queue-worker oms-graph-python-helper; do
  cp "$root/bin/incoming" "$root/incoming/target/lifecycle-build/release/$name"
  chmod 700 "$root/incoming/target/lifecycle-build/release/$name"
done
echo build >> "$root/flow"
"##;
    fs::write(host.root.join("bin/cargo"), cargo).unwrap();
    fs::set_permissions(
        host.root.join("bin/cargo"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    if host.fail.starts_with("prepared-") {
        prepared_update_case(&host, &target, &prior);
        assert!(
            !host
                .root
                .join("config/oh-my-setting/native-update/transaction.json")
                .exists()
        );
        return;
    }
    if host.fail.starts_with("apply-locked-") {
        let explicit = matches!(
            host.fail.as_str(),
            "apply-locked-explicit" | "apply-locked-release"
        );
        let (out, _) =
            with_locked_receipt_ref(&host, if explicit { &host.before } else { &target }, || {
                match host.fail.as_str() {
                    "apply-locked-auto" => call("auto-update", &["apply"], &source, &host),
                    "apply-locked-verify" => with_pending_failure_publication(&host, || {
                        call("auto-update", &["apply"], &source, &host)
                    }),
                    "apply-locked-explicit" => {
                        call("update", &["--ref", &target, "--no-tools"], &source, &host)
                    }
                    "apply-locked-release" => apply_release_with_services(
                        ReleaseChannel::Stable,
                        &target,
                        "origin/HEAD",
                        &source,
                        true,
                        &AtomicBool::new(false),
                        &host,
                    ),
                    _ => call("update", &["--no-tools"], &source, &host),
                }
            });
        if host.fail == "apply-locked-verify" {
            assert_eq!(out.0, 7, "{}", String::from_utf8_lossy(&out.2));
            let status: Value = serde_json::from_slice(&success(call(
                "auto-update",
                &["status", "--json"],
                &source,
                &host,
            )))
            .unwrap();
            assert_eq!(status["status"], "failed");
            assert_eq!(status["upstream"], target);
            assert_eq!(status["local"], host.before);
            assert_eq!(status["remote"], target);
            assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
            assert_eq!(read(&host.receipt())["commit"], host.before);
            assert_eq!(read(&host.receipt())["ref"], target);
            assert_eq!(fs::read(host.frontdoor()).unwrap(), b"prior frontdoor");
            assert!(
                !host
                    .root
                    .join("config/oh-my-setting/native-update/transaction.json")
                    .exists()
            );
            return;
        }
        success(out);
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), target);
        let installed = read(&host.receipt());
        assert_eq!(installed["commit"], target);
        assert_eq!(installed["ref"], target);
        assert_eq!(installed["previous_commit"], host.before);
        assert_eq!(
            fs::read_to_string(host.root.join("flow")).unwrap(),
            "build\nproof\n"
        );
        assert!(!host.root.join("incoming").exists());
        assert!(
            !host
                .root
                .join("config/oh-my-setting/native-update/transaction.json")
                .exists()
        );
        return;
    }
    if host.fail == "auto-refresh" {
        auto_update_outcome_case(&host, &target);
        return;
    }
    if host.fail.starts_with("post-") {
        post_refresh_case(&host, &target);
        return;
    }
    let release_channel = match host.fail.as_str() {
        "release-edge" | "release-custom" => Some(ReleaseChannel::Edge),
        "release-stable" => Some(ReleaseChannel::Stable),
        _ => None,
    };
    let follows_edge = matches!(host.fail.as_str(), "edge" | "fallback" | "release-edge");
    let mut args = vec!["--no-tools"];
    if host.fail == "fallback" {
        // Missing explicit refs remain errors unless fallback was requested.
        assert_ne!(call("update", &["--check", "--json"], &source, &host).0, 0);
        args.push("--fallback-to-edge");
    } else if !follows_edge {
        args.extend(["--ref", target.as_str()]);
    }
    let mut preview_args = args.clone();
    preview_args.push("--plan");
    let preview = call("update", &preview_args, &source, &host);
    success(preview);
    assert!(!host.root.join("flow").exists());
    if follows_edge {
        let mut check_args = args.clone();
        check_args.extend(["--check", "--json"]);
        let checked: Value =
            serde_json::from_slice(&success(call("update", &check_args, &source, &host))).unwrap();
        assert_eq!(checked["ref"], "edge");
        assert_eq!(checked["target_commit"], target);
        assert_eq!(checked["update_available"], true);
        assert_eq!(fs::read(host.receipt()).unwrap(), prior);
    }
    if host.fail == "release-edge" {
        let rejected = apply_release_with_services(
            ReleaseChannel::Edge,
            &host.before,
            "origin/HEAD",
            &source,
            true,
            &AtomicBool::new(false),
            &host,
        );
        assert_eq!(
            rejected.0, 75,
            "a moved upstream must not change the reviewed target"
        );
        assert!(!host.root.join("flow").exists());
        assert_eq!(fs::read(host.receipt()).unwrap(), prior);
        host.git(
            &source,
            &["commit", "--allow-empty", "-qm", "local divergence"],
        );
        let divergent = host.git(&source, &["rev-parse", "HEAD"]);
        let rejected = apply_release_with_services(
            ReleaseChannel::Edge,
            &target,
            "origin/HEAD",
            &source,
            true,
            &AtomicBool::new(false),
            &host,
        );
        assert_ne!(
            rejected.0, 0,
            "edge cannot replace a divergent local checkout"
        );
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), divergent);
        assert!(!host.root.join("flow").exists());
        assert_eq!(fs::read(host.receipt()).unwrap(), prior);
        host.git(&source, &["reset", "--hard", &host.before]);
    }
    let out = if let Some(channel) = release_channel {
        apply_release_with_services(
            channel,
            &target,
            if host.fail == "release-custom" {
                "origin/development"
            } else {
                "origin/HEAD"
            },
            &source,
            true,
            &AtomicBool::new(false),
            &host,
        )
    } else {
        call("update", &args, &source, &host)
    };
    assert_eq!(
        fs::read_to_string(host.root.join("flow")).unwrap(),
        "build\nproof\n"
    );
    assert!(host.root.join("lease-closed").is_file());
    let cache = source.join("target/oms-native").join(&target);
    if follows_edge || release_channel.is_some() || matches!(host.fail.as_str(), "ok" | "offline") {
        success(out);
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), target);
        assert_eq!(
            host.git(&source, &["rev-parse", "main"]),
            if follows_edge { &target } else { &host.before }.as_str()
        );
        let installed = read(&host.receipt());
        assert_eq!(installed["previous_commit"], host.before);
        let expected_ref = if follows_edge {
            "edge"
        } else if host.fail == "release-custom" {
            "refs/remotes/origin/development"
        } else {
            &target
        };
        assert_eq!(installed["ref"], expected_ref);
        assert_eq!(
            installed["channel"],
            if follows_edge { "main" } else { "detached" }
        );
        if follows_edge {
            assert_eq!(
                host.git(&source, &["symbolic-ref", "--short", "HEAD"]),
                "main"
            );
        }
        if follows_edge || host.fail == "release-custom" {
            // The next scheduled check follows the persisted edge ref without
            // another --fallback-to-edge flag or an explicit --ref override.
            let checked: Value = serde_json::from_slice(&success(call(
                "auto-update",
                &["check", "--json"],
                &source,
                &host,
            )))
            .unwrap();
            assert_eq!(checked["ref"], expected_ref);
            assert_eq!(checked["update_available"], false);
        }
        assert_eq!(
            *host.phases.borrow(),
            [
                IncomingPhase::Preflight,
                IncomingPhase::Reconcile,
                IncomingPhase::Verify
            ]
        );
        assert!(cache.join("oms").is_file());
        let manifest = read(&cache.join("manifest.json"));
        assert_eq!(manifest["source_commit"], target);
        success(call(
            "update",
            &["--rollback", "--no-tools"],
            &source,
            &host,
        ));
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
        assert_eq!(read(&host.receipt())["previous_commit"], target);
        if host.fail == "ok" {
            assert_eq!(
                read(&PathBuf::from(
                    std::env::var_os("OMS_CAPABILITY_RECEIPT").unwrap()
                )),
                json!({"schema":1,"requested":["core","research"],
                    "primary_provider":"agy","allow_missing":true}),
                "update --no-tools and rollback preserve the selected capabilities"
            );
            assert!(
                !host
                    .root
                    .join("config/oh-my-setting/capabilities.json")
                    .exists()
            );
        }
        assert_eq!(
            fs::read_to_string(host.root.join("flow")).unwrap(),
            "build\nproof\n",
            "warm rollback does not rebuild"
        );
    } else {
        assert_eq!(
            out.0,
            match host.fail.as_str() {
                "verify" => 7,
                "cleanup" => 75,
                "late-worker" => 76,
                _ => 2,
            },
            "{}",
            String::from_utf8_lossy(&out.2)
        );
        assert_eq!(host.git(&source, &["rev-parse", "HEAD"]), host.before);
        assert_eq!(
            host.git(&source, &["symbolic-ref", "--short", "HEAD"]),
            "main"
        );
        assert_eq!(fs::read(host.receipt()).unwrap(), prior);
        assert_eq!(fs::read(host.frontdoor()).unwrap(), b"prior frontdoor");
        assert!(!cache.exists());
        assert!(
            !host
                .root
                .join("data/oh-my-setting/libexec")
                .join(&target)
                .exists()
        );
        if matches!(host.fail.as_str(), "proof" | "cleanup") {
            assert!(host.phases.borrow().is_empty());
        }
        if host.fail == "receipt" {
            assert!(!host.phases.borrow().contains(&IncomingPhase::Verify));
        }
    }
    assert_eq!(
        fs::read(old_cache.join("sentinel")).unwrap(),
        b"prior cache"
    );
    assert!(
        !host
            .root
            .join("config/oh-my-setting/native-update/transaction.json")
            .exists()
    );
}

fn doctor_provider_capability_evidence(source: &Path, root: &Path, host: &NativeServices) {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let check = |table: &Path, evidence: &Path| {
        let out = call(
            "doctor",
            &[
                "--providers",
                "--check",
                "--table",
                table.to_str().unwrap(),
                "--root",
                evidence.to_str().unwrap(),
            ],
            source,
            host,
        );
        assert!(out.2.is_empty(), "{}", String::from_utf8_lossy(&out.2));
        let report: Value = serde_json::from_slice(&out.1).unwrap();
        (out.0, report)
    };
    let (code, report) = check(&shipped.join("config/provider-capabilities.json"), shipped);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["checked"], 23);
    assert_eq!(report["ok"], true);

    fs::write(
        root.join("provider-evidence.rs"),
        r###"pub(crate) async fn worker() {}
pub(super) const fn configured() -> bool { true }
pub unsafe extern "C" fn foreign_worker() {}
pub struct Native;
pub enum Outcome { Complete }
pub type Alias = Native;
pub const LIMIT: usize = 1;
pub static mut STATE: usize = 0;
fn fixture_test() {}
// fn comment_only() {}
/* outer /* nested */
fn block_only() {}
*/
const TEXT: &str = "
fn string_only() {}
";
const RAW: &str = r##"
fn raw_only() {}
"##;
const QUOTE: char = '"';
fn borrowed<'a>(value: &'a str) -> &'a str { value }
use elsewhere::import_only;
fn caller() { call_only(); }
fn worker_suffix() {}
"###,
    )
    .unwrap();
    let table_path = root.join("provider-evidence.json");
    let mut table = json!({"schema":1,"functions":[{"id":"worker_call"}],
        "providers":{"fixture":{"worker_call":{"status":"supported","verified":"fixture",
            "impl":["provider-evidence.rs::worker"],"test":["provider-evidence.rs::fixture_test"]}}}});
    for (symbol, expected) in [
        ("worker", true),
        ("configured", true),
        ("foreign_worker", true),
        ("Native", true),
        ("Outcome", true),
        ("Alias", true),
        ("LIMIT", true),
        ("STATE", true),
        ("borrowed", true),
        ("comment_only", false),
        ("block_only", false),
        ("string_only", false),
        ("raw_only", false),
        ("import_only", false),
        ("call_only", false),
        ("work", false),
    ] {
        table["providers"]["fixture"]["worker_call"]["impl"] =
            json!([format!("provider-evidence.rs::{symbol}")]);
        write(&table_path, &table);
        let (code, report) = check(&table_path, root);
        assert_eq!(code == 0, expected, "{symbol}: {report}");
        assert_eq!(report["ok"], expected, "{symbol}: {report}");
    }
    table["providers"]["fixture"]["worker_call"]["impl"] = json!(["provider-evidence.rs::worker"]);
    table["providers"]["fixture"]["worker_call"]["test"] =
        json!(["provider-evidence.rs::call_only"]);
    write(&table_path, &table);
    assert_eq!(check(&table_path, root).0, 1);
    table["providers"]["fixture"]["worker_call"]["test"] =
        json!(["retired-provider-test.rs::fixture_test"]);
    write(&table_path, &table);
    assert_eq!(check(&table_path, root).0, 1);
}

#[test]
fn doctor_and_model_doctor_share_bounded_probe_evidence() {
    if let Some(root) = std::env::var_os("OMS_PROBE_PARITY_ROOT").map(PathBuf::from) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let expected = std::env::var("OMS_TEST_VERSION_EXIT").unwrap() == "0"
            && std::env::var("OMS_TEST_HELP_EXIT").unwrap() == "0";
        let stop = AtomicBool::new(false);
        let narrow = oms_command_providers_skills::run_with_source(
            "model-doctor",
            &["--providers".into(), "claude".into(), "--json".into()],
            &root,
            source,
            &stop,
        );
        assert_eq!(
            narrow.0,
            i32::from(!expected),
            "model-doctor exit: stdout={} stderr={}",
            String::from_utf8_lossy(&narrow.1),
            String::from_utf8_lossy(&narrow.2)
        );
        assert!(
            narrow.2.is_empty(),
            "model-doctor stderr: {}",
            String::from_utf8_lossy(&narrow.2)
        );
        let report: Value = serde_json::from_slice(&narrow.1).unwrap();
        assert_eq!(report["providers"][0]["usable"], expected, "{report}");
        let cache = root.join("capabilities");
        let fields = |file: &Path| -> std::collections::BTreeMap<String, String> {
            fs::read_to_string(file)
                .unwrap()
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect()
        };
        let narrow_cache = fields(&cache.join("claude.env"));
        if expected {
            assert_eq!(report["providers"][0]["effort_mechanism"], "flag");
            assert_eq!(
                report["providers"][0]["effort_values"],
                json!(["low", "medium", "high", "xhigh"])
            );
            assert_eq!(narrow_cache["binary_key"], "previous-binary");
            assert_eq!(narrow_cache["models_probe"], "failed");
        }
        let aggregate = run_with_services(
            "doctor",
            &["--json".into()],
            &root,
            source,
            &stop,
            &NativeServices::default(),
        );
        assert!(
            aggregate.2.is_empty(),
            "{}",
            String::from_utf8_lossy(&aggregate.2)
        );
        let report: Value = serde_json::from_slice(&aggregate.1).unwrap();
        let failures = report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                row["section"] == "model doctor"
                    && row["message"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("claude:"))
            })
            .collect::<Vec<_>>();
        assert_eq!(failures.is_empty(), expected, "{report}");
        if let Some(failure) = failures.first() {
            let level = if std::env::var("OH_MY_SETTING_MODEL_DOCTOR").unwrap() == "1" {
                "error"
            } else {
                "warn"
            };
            assert_eq!(failure["level"], level);
        }
        let aggregate_cache = fields(&cache.join("claude.env"));
        if expected {
            for key in ["effort_mechanism", "effort_values"] {
                assert_eq!(narrow_cache[key], aggregate_cache[key], "{key}");
            }
        }
        assert_ne!(aggregate_cache["binary_key"], "previous-binary");
        assert_eq!(
            fs::read_to_string(cache.join("claude.models")).unwrap(),
            "cached-model\n"
        );
        assert_eq!(
            fs::read_to_string(cache.join("claude.efforts")).unwrap(),
            "cached-model\tlow high\n"
        );
        return;
    }
    let root = std::env::temp_dir().join(format!("oms-probe-parity-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let _fixture = Fixture(root.clone());
    // macOS temporary paths can have a symlinked prefix. Exercise that spelling
    // on every host, then use the physical root required by descriptor writes.
    let physical = root.join("physical");
    fs::create_dir(&physical).unwrap();
    let alias = root.join("alias");
    symlink(&physical, &alias).unwrap();
    let root = fs::canonicalize(alias).unwrap();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("claude");
    fs::write(&executable, "#!/bin/sh\ncase \"$1\" in\n--version) echo fixture; exit \"$OMS_TEST_VERSION_EXIT\";;\n--help) echo '--effort (low|medium|high|xhigh)'; exit \"$OMS_TEST_HELP_EXIT\";;\n*) exit 2;;\nesac\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    for (version, help, mode) in [(0, 0, "auto"), (1, 0, "auto"), (0, 1, "1"), (1, 1, "1")] {
        let case = root.join(format!("{version}-{help}"));
        let cache = case.join("capabilities");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("claude.env"), "binary_key=previous-binary\neffort_mechanism=none\neffort_values=low\nmodels_probe=failed\nprobed_at=1\n").unwrap();
        fs::write(cache.join("claude.models"), "cached-model\n").unwrap();
        fs::write(cache.join("claude.efforts"), "cached-model\tlow high\n").unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "doctor_and_model_doctor_share_bounded_probe_evidence",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", &bin)
            .env("HOME", &case)
            .env("TMPDIR", &case)
            .env("OMS_PROBE_PARITY_ROOT", &case)
            .env("OMS_CAPABILITY_DIR", &cache)
            .env("OMS_LOCK_DIR", case.join("locks"))
            .env("OH_MY_SETTING_MODEL_DOCTOR", mode)
            .env("OMS_TEST_VERSION_EXIT", version.to_string())
            .env("OMS_TEST_HELP_EXIT", help.to_string())
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "version={version}, help={help}: {} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
}

// Keep diagnostics in the canonical isolated lifecycle fixture: the same read-only
// contract applies to both inventory and pin comparison, without --pin or probes.
fn doctor_configuration_diagnostics(source: &Path, root: &Path, host: &NativeServices) {
    fn snapshot(root: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
        fn visit(base: &Path, path: &Path, rows: &mut Vec<(PathBuf, u32, Vec<u8>)>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let body = if metadata.is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else if metadata.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            rows.push((
                path.strip_prefix(base).unwrap().to_owned(),
                metadata.mode(),
                body,
            ));
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(base, &entry.unwrap().path(), rows);
                }
            }
        }
        let mut rows = Vec::new();
        visit(root, root, &mut rows);
        rows.sort();
        rows
    }
    let home = root.join("home");
    let command = root.join("bin/doctor-server");
    fs::write(
        &command,
        "#!/bin/sh\nprintf called > \"$OMS_LIFECYCLE_FIXTURE/doctor-executed\"\n",
    )
    .unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
    let hidden = "fixture-private-material";
    let spec = json!({"command":command,"args":[hidden],"env":{"PRIVATE_VALUE":hidden}});
    let user = home.join(".claude.json");
    let project = source.join(".mcp.json");
    let codex = home.join(".codex/config.toml");
    let agy_user = home.join(".gemini/settings.json");
    let agy_legacy = home.join(".gemini/config/mcp_config.json");
    let agy_project = source.join(".gemini/mcp_config.json");
    let agy_plugin = home.join(".gemini/config/plugins/fixture:variant/mcp_config.json");
    write(
        &user,
        &json!({"mcpServers":{"shared":spec,"user-only":spec}}),
    );
    write(
        &project,
        &json!({"mcpServers":{"shared":{"command":command,"disabled":true}}}),
    );
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(
        &codex,
        format!(
            "[mcp_servers.shared]\ncommand = \"{}\"\nenabled = false\nargs = [\"{hidden}\"]\n",
            command.display()
        ),
    )
    .unwrap();
    let remote = [
        "https://",
        "user:",
        hidden,
        "@example.test/private?",
        hidden,
    ]
    .concat();
    for file in [&agy_user, &agy_legacy, &agy_project, &agy_plugin] {
        write(
            file,
            &json!({"mcpServers":{"shared":{"url":remote,"headers":{"Private":hidden}}}}),
        );
    }
    let baseline = snapshot(root);
    let services = host.0.borrow().clone();
    for mode in ["--extensions", "--mcp"] {
        let raw = success(call("doctor", &[mode, "--json"], source, host));
        assert!(!String::from_utf8_lossy(&raw).contains(hidden));
        assert!(!String::from_utf8_lossy(&raw).contains("PRIVATE_VALUE"));
        let report: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(report["collisions"].as_array().unwrap().len(), 2);
        let claude = report["collisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["provider"] == "claude")
            .unwrap();
        assert_eq!(claude["scopes"], json!(["project", "user"]));
        let agy = report["collisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["provider"] == "antigravity")
            .unwrap();
        assert_eq!(
            agy["scopes"],
            json!(["legacy", "plugin-fixture:variant", "project", "user"])
        );
        assert!(
            report["servers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["effective"] == "unknown" && v["runtime"] == "unknown")
        );
        assert_eq!(
            report["servers"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["configuration"] == "disabled" && v["disabled"] == true)
                .count(),
            2
        );
        assert_eq!(report["unknown"], json!([]));
        assert!(
            report["config_layers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["status"] == "parsed")
        );
        assert!(
            report["servers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["server"] == "claude:user:user-only"
                    && v["configuration"] == "configured")
        );
    }
    assert_eq!(snapshot(root), baseline);
    assert_eq!(*host.0.borrow(), services);
    assert!(!root.join("doctor-executed").exists());
    assert!(!root.join("config/oh-my-setting/mcp-pins.json").exists());
    // A malformed peer never makes a readable entry effective. A missing layer
    // is distinguished from an unreadable one, without guessing runtime state.
    fs::write(&project, format!("{{invalid {hidden}")).unwrap();
    fs::remove_file(&agy_legacy).unwrap();
    fs::remove_file(&agy_project).unwrap();
    symlink(&agy_user, &agy_project).unwrap();
    fs::write(&codex, "[mcp_servers.invalid\n").unwrap();
    write(
        &agy_plugin,
        &json!({"mcpServers":{"invalid":42,"shared":spec}}),
    );
    let pins = root.join("config/oh-my-setting/mcp-pins.json");
    write(
        &pins,
        &json!({"schema":1,"pins":{"claude:project:previous":"prior-digest","codex:user:previous":"prior-digest"}}),
    );
    let baseline = snapshot(root);
    for mode in ["--extensions", "--mcp"] {
        let raw = success(call("doctor", &[mode, "--json"], source, host));
        assert!(!String::from_utf8_lossy(&raw).contains(hidden));
        let report: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(report["unknown"], json!(["antigravity", "claude", "codex"]));
        for (provider, scope, expected) in [
            ("claude", "project", "unknown"),
            ("codex", "user", "unknown"),
            ("antigravity", "legacy", "missing"),
            ("antigravity", "project", "unknown"),
            ("antigravity", "plugin-fixture:variant", "unknown"),
        ] {
            let layer = report["config_layers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["provider"] == provider && v["scope"] == scope)
                .unwrap();
            assert_eq!(layer["status"], expected);
            assert!(layer["reason"].is_string());
        }
        assert!(
            report["servers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["effective"] == "unknown"
                    && v["effective_reason"] == "provider configuration incomplete")
        );
        if mode == "--mcp" {
            // A manually supplied prior pin is comparison data, never permission
            // to probe or to overwrite a malformed provider's baseline.
            assert_eq!(report["probe"], Value::Null);
            assert_eq!(report["removed"], json!([]));
        }
    }
    assert_eq!(snapshot(root), baseline);
    assert_eq!(*host.0.borrow(), services);
    for file in [
        &user,
        &project,
        &codex,
        &agy_user,
        &agy_legacy,
        &agy_project,
        &agy_plugin,
        &command,
        &pins,
    ] {
        if fs::symlink_metadata(file).is_ok() {
            fs::remove_file(file).unwrap();
        }
    }
    fs::remove_dir(agy_plugin.parent().unwrap()).unwrap();

    let releases = root.join("data/oh-my-setting/tool-releases.json");
    assert!(!releases.exists());
    let inspect_releases = || -> Value {
        serde_json::from_slice(&success(call(
            "doctor",
            &["--tool-lock", "--json"],
            source,
            host,
        )))
        .unwrap()
    };
    let fingerprint = || -> Value {
        serde_json::from_slice(&success(call("status", &["--fingerprint"], source, host))).unwrap()
    };
    let report = inspect_releases();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["findings"][0]["level"], "info");
    let missing = fingerprint();
    assert_eq!(missing["digests"]["tools_lock"], "not configured");
    assert_eq!(missing["digests"]["tool_releases"], "missing");
    assert!(
        !releases.exists(),
        "diagnostics must not create release observations"
    );

    let observed = json!({
        "schema":2,"resolved_at":"2026-11-18T00:00:00Z",
        "node":{"version":"22.1.0","resolved_at":"2026-11-18T00:00:00Z",
            "url":"https://example.invalid/node.tar.gz","sha256":"a".repeat(64)},
        "npm":{"codex":{"version":"1.0.0","sha512":"b".repeat(128),
            "integrity":format!("sha512-{}==", "A".repeat(86))}}
    });
    write(&releases, &observed);
    assert_eq!(inspect_releases()["findings"][0]["level"], "ok");
    let before = fingerprint();
    let mut refreshed = observed.clone();
    refreshed["resolved_at"] = json!("2026-11-19T00:00:00Z");
    refreshed["node"]["resolved_at"] = json!("2026-11-19T00:00:00Z");
    write(&releases, &refreshed);
    assert_eq!(
        fingerprint(),
        before,
        "observation timestamps do not change installation identity"
    );
    refreshed["node"]["version"] = json!("22.2.0");
    write(&releases, &refreshed);
    let changed = fingerprint();
    assert_ne!(changed["fingerprint"], before["fingerprint"]);
    assert_ne!(
        changed["digests"]["tool_releases"],
        before["digests"]["tool_releases"]
    );

    for (field, malformed) in [
        ("sha512", "b".repeat(127)),
        ("sha512", "z".repeat(128)),
        ("integrity", format!("sha512-{}==", "A".repeat(85))),
        ("integrity", format!("sha512-{}==", "!".repeat(86))),
    ] {
        let mut invalid = observed.clone();
        invalid["npm"]["codex"][field] = json!(malformed);
        write(&releases, &invalid);
        let out = call("doctor", &["--tool-lock", "--json"], source, host);
        assert_ne!(out.0, 0, "malformed {field} must fail diagnostics");
        assert!(String::from_utf8_lossy(&out.2).contains("invalid recorded"));
    }
    fs::remove_file(releases).unwrap();
    assert_eq!(fingerprint(), missing);

    let capabilities = root.join("selected capabilities.json");
    assert!(!capabilities.exists());
    let inspect_selection = || -> Value {
        let raw = success(call("status", &["--json"], source, host));
        assert!(!String::from_utf8_lossy(&raw).contains(hidden));
        assert!(!String::from_utf8_lossy(&raw).contains("selected capabilities.json"));
        serde_json::from_slice::<Value>(&raw).unwrap()["capability_selection"].clone()
    };
    let selection = inspect_selection();
    assert_eq!(selection["status"], "missing");
    assert_eq!(selection["source"], "environment override");
    assert_eq!(selection["runtime"], "unknown");
    assert_eq!(selection["fingerprint"], "excluded from schema 1");
    let saved = root.join("fingerprint.json");
    write(&saved, &missing);
    let mut selected = json!({"schema":1,"requested":["core"],
        "primary_provider":"codex","allow_missing":false,"private":hidden});
    write(&capabilities, &selected);
    let before = inspect_selection();
    assert_eq!(before["status"], "recorded");
    assert_eq!(before["requested"], json!(["core"]));
    assert_eq!(before["primary_provider"], "codex");
    assert_eq!(before["allow_missing"], false);
    selected["requested"] = json!(["core", "research"]);
    selected["primary_provider"] = json!("agy");
    selected["allow_missing"] = json!(true);
    write(&capabilities, &selected);
    let after = inspect_selection();
    assert_ne!(after, before);
    assert_eq!(after["requested"], json!(["core", "research"]));
    assert_eq!(after["primary_provider"], "agy");
    assert_eq!(after["allow_missing"], true);
    assert_eq!(after["runtime"], "unknown");
    assert_eq!(
        fingerprint(),
        missing,
        "schema-1 identity remains compatible"
    );
    let compared: Value = serde_json::from_slice(&success(call(
        "status",
        &["--compare", saved.to_str().unwrap(), "--json"],
        source,
        host,
    )))
    .unwrap();
    assert_eq!(compared, json!({"same":true,"differences":[]}));
    let report = call("doctor", &["--json", "--no-model-doctor"], source, host);
    let report: Value = serde_json::from_slice(&report.1).unwrap();
    let selection = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["section"] == "capability selection")
        .unwrap();
    assert_eq!(selection["selection"], after);
    write(
        &capabilities,
        &json!({"schema":1,"plan":{"requested":["hpc"],"primary_provider":"claude"}}),
    );
    assert_eq!(inspect_selection()["requested"], json!(["hpc"]));
    for invalid in [
        json!({"schema":1,"requested":[]}),
        json!({"schema":1,"requested":["x".repeat(65)]}),
        json!({"schema":1,"requested":["core"],"allow_missing":"yes"}),
    ] {
        write(&capabilities, &invalid);
        assert_eq!(inspect_selection()["status"], "invalid");
    }
    fs::write(&capabilities, format!("{{invalid {hidden}")).unwrap();
    assert_eq!(inspect_selection()["status"], "invalid");
    let out = call("doctor", &["--json", "--no-model-doctor"], source, host);
    let report: Value = serde_json::from_slice(&out.1).unwrap();
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| { v["section"] == "capability selection" && v["level"] == "error" })
    );
    fs::remove_file(&capabilities).unwrap();
    symlink(&saved, &capabilities).unwrap();
    assert_eq!(inspect_selection()["status"], "unreadable");
    fs::remove_file(capabilities).unwrap();
    fs::remove_file(saved).unwrap();

    let rules = source.join("rules/global-AGENTS.md");
    fs::create_dir_all(rules.parent().unwrap()).unwrap();
    fs::write(&rules, b"managed rules\n").unwrap();
    let manifest = source.join("skills.manifest.json");
    write(&manifest, &json!({"skills":[]}));
    let receipt = root.join("config/oh-my-setting/install.json");
    write(
        &receipt,
        &json!({"schema":1,"source_root":source,"version":"1","commit":"a".repeat(40),
            "channel":"edge","installed_at":"fixture","profile":"custom",
            "plugin":{"name":"oh-my-setting","version":"1","sha256":"a".repeat(64)},
            "native":{"frontdoor":source.join("frontdoor")}}),
    );
    let target = home.join(".codex/AGENTS.md");
    fs::write(&target, b"managed rules\n").unwrap();
    let marker = home.join(".codex/.AGENTS.md.oh-my-setting-managed.json");
    write(
        &marker,
        &json!({"schema":1,"source":rules,"kind":"file",
            "sha256":format!("{:x}", Sha256::digest(b"file\0managed rules\n"))}),
    );
    let inspect_target = || -> Value {
        let before = snapshot(root);
        let out = call("doctor", &["--json", "--no-model-doctor"], source, host);
        let report: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(snapshot(root), before, "doctor is read-only");
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["section"] == "managed inventory" && v["target"] == ".codex/AGENTS.md")
            .unwrap()
            .clone()
    };
    let current = inspect_target();
    assert_eq!(current["owned"], true);
    assert_eq!(current["source_freshness"], "current");
    assert_eq!(current["level"], "ok");
    fs::write(&rules, b"updated rules\n").unwrap();
    let stale = inspect_target();
    assert_eq!(stale["owned"], true);
    assert_eq!(stale["source_freshness"], "stale");
    assert_eq!(stale["level"], "error");
    assert!(
        stale["message"]
            .as_str()
            .unwrap()
            .contains("oms doctor --repair")
    );
    fs::write(&target, b"local edit\n").unwrap();
    let modified = inspect_target();
    assert_eq!(modified["owned"], false);
    assert_eq!(modified["source_freshness"], "not checked");
    fs::write(&target, b"managed rules\n").unwrap();
    fs::remove_file(&rules).unwrap();
    symlink(&marker, &rules).unwrap();
    let unsafe_source = inspect_target();
    assert_eq!(unsafe_source["owned"], true);
    assert_eq!(unsafe_source["source_freshness"], "unknown");
    assert_eq!(unsafe_source["level"], "error");
    fs::remove_file(&rules).unwrap();
    assert_eq!(inspect_target()["source_freshness"], "unknown");
    fs::write(&rules, b"managed rules\n").unwrap();
    fs::remove_file(&target).unwrap();
    symlink(&rules, &target).unwrap();
    assert_eq!(inspect_target()["source_freshness"], "current");
    let alias = source.join("rules-alias");
    fs::hard_link(&rules, &alias).unwrap();
    let unsafe_link_source = inspect_target();
    assert_eq!(unsafe_link_source["owned"], true);
    assert_eq!(unsafe_link_source["source_freshness"], "unknown");
    assert_eq!(unsafe_link_source["level"], "error");
    fs::remove_file(alias).unwrap();
    fs::remove_file(&target).unwrap();
    symlink(&marker, &target).unwrap();
    let foreign = inspect_target();
    assert_eq!(foreign["owned"], false);
    assert_eq!(foreign["source_freshness"], "not checked");
    for file in [target, marker, rules, manifest, receipt] {
        fs::remove_file(file).unwrap();
    }
    fs::remove_dir(source.join("rules")).unwrap();
}

fn doctor_integration_diagnostics(source: &Path, root: &Path, host: &NativeServices) {
    let home = root.join("home");
    let receipt = root.join("config/oh-my-setting/install.json");
    assert!(!receipt.exists());
    let settings = home.join(".claude/settings.json");
    let claude = home.join(".claude.json");
    let codex = home.join(".codex/config.toml");
    let cache =
        home.join(".codex/plugins/cache/oh-my-setting-local/oh-my-setting/1.0.0/hooks.json");
    let state = root.join("codex-state");
    let files = [&settings, &claude, &codex, &cache, &state];
    let before = files.map(|p| fs::read(p).ok());
    let inspect = |components: Value| -> Value {
        write(
            &receipt,
            &json!({"schema":1,"source_root":source,"version":"1","commit":"a".repeat(40),
            "channel":"edge","installed_at":"fixture","components":components,
            "plugin":{"name":"oh-my-setting","version":"1","sha256":"a".repeat(64)}}),
        );
        let saved = fs::read(&receipt).unwrap();
        let out = call("doctor", &["--integrations", "--json"], source, host);
        let report: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(out.0, report["exit"].as_i64().unwrap() as i32);
        assert_eq!(report["kind"], "oms-doctor-integrations");
        assert_eq!(report["runtime"], "not_probed");
        assert!(!String::from_utf8_lossy(&out.1).contains(root.to_str().unwrap()));
        assert!(!String::from_utf8_lossy(&out.1).contains("private-provider-output"));
        assert_eq!(
            fs::read(&receipt).unwrap(),
            saved,
            "diagnosis cannot change the receipt"
        );
        report
    };
    let skipped = inspect(json!({}));
    assert_eq!(skipped["exit"], 0);
    assert!(
        skipped["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["level"] == "note")
    );
    assert_eq!(inspect(json!({"codex_plugin":true}))["exit"], 0);
    write(
        &cache,
        &json!({"hooks":{},"private":"private-provider-output"}),
    );
    assert_eq!(inspect(json!({"codex_plugin":true}))["exit"], 1);
    assert_eq!(
        read(&cache)["private"],
        "private-provider-output",
        "doctor cannot repair the cache"
    );
    fs::write(&cache, before[3].as_ref().unwrap()).unwrap();
    fs::write(&state, b"gone\n").unwrap();
    assert_eq!(inspect(json!({"codex_plugin":true}))["exit"], 1);
    assert_eq!(fs::read(&state).unwrap(), b"gone\n");
    fs::write(&state, b"here\n").unwrap();

    let expected: Value = serde_json::from_slice(&success(call(
        "install-claude-hooks",
        &["--print-expected"],
        source,
        host,
    )))
    .unwrap();
    let mut hooks = serde_json::Map::new();
    for surface in expected["surfaces"].as_array().unwrap() {
        hooks.entry(surface["event"].as_str().unwrap()).or_insert(json!([])).as_array_mut().unwrap().push(
            json!({"matcher":surface["matcher"],"hooks":[{"type":"command","command":surface["command"],"timeout":surface["timeout"]}]}),
        );
    }
    write(&settings, &json!({"hooks":hooks}));
    assert_eq!(inspect(json!({"claude_hooks":true}))["exit"], 0);
    write(&settings, &json!({"hooks":{}}));
    assert_eq!(inspect(json!({"claude_hooks":true}))["exit"], 1);

    let executable = home.join(".local/bin/oms");
    let managed =
        json!({"mcpServers":{"oh-my-setting":{"command":executable,"args":["mcp-server"]}}});
    write(&claude, &managed);
    fs::write(
        &codex,
        format!(
            "[mcp_servers.oh-my-setting]\ncommand = {}\nargs = [\"mcp-server\"]\n",
            json!(executable)
        ),
    )
    .unwrap();
    assert_eq!(inspect(json!({"mcp":true}))["exit"], 0);
    for invalid in [json!({"args":[]}), json!({"disabled":true})] {
        let mut changed = managed.clone();
        changed["mcpServers"]["oh-my-setting"]
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        write(&claude, &changed);
        assert_eq!(inspect(json!({"mcp":true}))["exit"], 1);
    }
    fs::write(
        &claude,
        format!("{{private-provider-output {}", root.display()),
    )
    .unwrap();
    assert_eq!(inspect(json!({"mcp":true}))["exit"], 1);
    assert_eq!(inspect(json!({"agy_plugin":true}))["exit"], 1);
    for incompatible in ["--repair", "--surfaces", "--contract", "--live-models"] {
        assert_eq!(
            call("doctor", &["--integrations", incompatible], source, host).0,
            2
        );
    }
    fs::remove_file(receipt).unwrap();
    for (path, original) in files.into_iter().zip(before) {
        match original {
            Some(raw) => fs::write(path, raw).unwrap(),
            None => {
                if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }
        }
    }
}

fn run_fixture_cron(root: &Path) -> String {
    let cron = fs::read_to_string(root.join("cron")).unwrap();
    let command = cron
        .lines()
        .find_map(|line| line.strip_prefix("17 4 * * * "))
        .unwrap();
    let scheduled = Command::new("/bin/sh")
        .args(["-c", command])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        scheduled.status.success(),
        "{}",
        String::from_utf8_lossy(&scheduled.stderr)
    );
    fs::read_to_string(root.join("scheduled-environment")).unwrap()
}

fn doctor_hook_evidence(source: &Path, root: &Path, host: &NativeServices, hooks: &[u8]) {
    let home = root.join("home");
    let config = home.join(".codex/config.toml");
    let settings = home.join(".claude/settings.json");
    let original_config = fs::read(&config).unwrap();
    let original_settings = fs::read(&settings).unwrap();
    let codex = root.join("bin/codex");
    let original_codex = fs::read(&codex).unwrap();
    fs::write(
        &codex,
        "#!/bin/sh\nprintf invoked > \"$HOME/doctor-provider-invoked\"\nexit 1\n",
    )
    .unwrap();
    let report = || {
        let out = call("doctor", &["--surfaces", "--json"], source, host);
        serde_json::from_slice::<Value>(&out.1)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.2)))
    };
    let diagnostic = |value: &Value| {
        value["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["section"] == "codex hook diagnostics")
            .unwrap()
            .clone()
    };
    let baseline = report();
    let guards: Vec<_> = baseline["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["section"] == "main write guard")
        .collect();
    assert_eq!(guards.len(), 4);
    assert_eq!(guards[0]["provider"], "claude");
    assert_eq!(guards[0]["registration"], "registered");
    assert_eq!(
        guards[0]["file_tool_coverage"],
        json!(["Edit", "Write", "MultiEdit", "NotebookEdit"])
    );
    for row in &guards {
        assert_eq!(row["shell_write_enforcement"], "unsupported");
        assert_eq!(row["effective_running_main"], "unknown");
    }
    for row in &guards[1..] {
        assert_eq!(row["registration"], "unsupported");
        assert_eq!(row["file_tool_coverage"], json!([]));
    }
    assert_eq!(diagnostic(&baseline)["trace"]["status"], "missing");
    assert_eq!(
        diagnostic(&baseline)["configuration"]["plugin_configuration"],
        "not_declared"
    );
    let cache = home.join(".codex/plugins/cache/oh-my-setting-local/oh-my-setting/1.0.0");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("hooks.json"), hooks).unwrap();
    fs::write(
        &config,
        "[plugins.\"oh-my-setting@oh-my-setting-local\"]\nenabled = true\n",
    )
    .unwrap();
    let configured = diagnostic(&report());
    assert_eq!(
        configured["configuration"]["plugin_configuration"],
        "declared_enabled"
    );
    assert_eq!(
        configured["configuration"]["generated_hooks"],
        "matches_expected"
    );
    assert_eq!(configured["configuration"]["runtime_loading"], "unverified");
    assert_eq!(
        configured["configuration"]["native_inventory"],
        "not_probed"
    );
    assert_eq!(configured["trace"]["diagnosis"], "no_callback_evidence");
    write(&cache.join("hooks.json"), &json!({"hooks":{}}));
    assert_eq!(
        diagnostic(&report())["configuration"]["generated_hooks"],
        "drifted"
    );

    let trace = source.join(".oms/hooks/codex-trace.jsonl");
    fs::create_dir_all(trace.parent().unwrap()).unwrap();
    let mut rows = Vec::new();
    for (id, handler, phase, outcome) in [
        (1, "resume-hook", "exit", Some("returned")),
        (2, "turn-guard", "exit", Some("internal_failure")),
        (3, "skill-router", "rejected", Some("input_oversize")),
        (4, "telemetry-hook", "received", None),
        (5, "precompact-handoff", "entry", None),
        (6, "syntax-guard-hook", "exit", Some("nonzero")),
    ] {
        let mut row = json!({"schema":1,"nonce":format!("{id:032x}"),"handler":handler,
            "received_at":"2026-11-23T12:00:00Z","event":"unknown","phase":"received",
            "ignored_payload":"private-fixture-marker"});
        rows.push(row.clone());
        if phase != "received" && phase != "rejected" {
            row["phase"] = json!("entry");
            rows.push(row.clone());
        }
        if let Some(outcome) = outcome {
            row["phase"] = json!("exit");
            row["outcome"] = json!(outcome);
            row["elapsed_us"] = json!(10);
            if phase != "rejected" {
                row["inner_exit"] = json!(if outcome == "nonzero" { 2 } else { 0 });
            }
            rows.push(row);
        }
    }
    let raw = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    fs::write(&trace, &raw).unwrap();
    fs::set_permissions(&trace, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::metadata(&trace).unwrap();
    // Match a normal CI /tmp ancestor even on hosts whose /tmp is private.
    // The checkout and its diagnostic paths remain owned, without group/world write.
    let parent_permissions = fs::metadata(root).unwrap().permissions();
    fs::set_permissions(root, fs::Permissions::from_mode(0o1777)).unwrap();
    let observed = diagnostic(&report());
    fs::set_permissions(root, parent_permissions).unwrap();
    let after = fs::metadata(&trace).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.mtime_nsec()),
        (after.ino(), after.mtime(), after.mtime_nsec())
    );
    assert_eq!(fs::read(&trace).unwrap(), raw.as_bytes());
    assert_eq!(observed["trace"]["attempts"], 6);
    assert_eq!(
        observed["trace"]["counts"],
        json!({"returned":1,"handler_nonzero":1,
        "handler_internal_failure":1,"before_handler_failure":1,"received_without_entry":1,
        "entry_without_exit":1,"invalid_attempts":0})
    );
    assert_eq!(
        observed["trace"]["handlers"]["turn-guard"]["handler_internal_failure"],
        1
    );
    assert_eq!(observed["trace"]["active_session"], "unknown");
    assert_eq!(observed["trace"]["provider_loading"], "unverified");
    assert!(!observed.to_string().contains("private-fixture-marker"));
    assert!(!observed.to_string().contains("2026-11-23"));
    let text = String::from_utf8(success(call("doctor", &["--surfaces"], source, host))).unwrap();
    assert!(text.contains("turn-guard handler_internal_failure=1"));
    assert!(text.contains("retained evidence cannot prove current account/session delivery"));
    // Shared ancestors are allowed; unsafe local diagnostic storage is not.
    fs::set_permissions(&trace, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(diagnostic(&report())["trace"]["status"], "unavailable");
    fs::set_permissions(&trace, fs::Permissions::from_mode(0o600)).unwrap();
    let hardlink = trace.with_extension("copy");
    fs::hard_link(&trace, &hardlink).unwrap();
    assert_eq!(diagnostic(&report())["trace"]["status"], "unavailable");
    fs::remove_file(hardlink).unwrap();
    let hooks_directory = trace.parent().unwrap();
    let hooks_permissions = fs::metadata(hooks_directory).unwrap().permissions();
    fs::set_permissions(hooks_directory, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(diagnostic(&report())["trace"]["status"], "unavailable");
    fs::set_permissions(hooks_directory, hooks_permissions).unwrap();
    let checkout_permissions = fs::metadata(source).unwrap().permissions();
    fs::set_permissions(source, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(diagnostic(&report())["trace"]["status"], "unavailable");
    fs::set_permissions(source, checkout_permissions).unwrap();
    // A conflicting repeat and a truncated row cannot become clean success evidence.
    fs::write(&trace, format!("{raw}{}\n{{", rows[0])).unwrap();
    let invalid = diagnostic(&report());
    assert_eq!(invalid["trace"]["diagnosis"], "incomplete_evidence");
    assert_eq!(invalid["trace"]["counts"]["returned"], 0);
    assert_eq!(invalid["trace"]["invalid_records"], 2);
    fs::remove_file(&trace).unwrap();
    symlink(&settings, &trace).unwrap();
    assert_eq!(diagnostic(&report())["trace"]["status"], "unavailable");
    fs::remove_file(&trace).unwrap();
    fs::write(&trace, vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert_eq!(diagnostic(&report())["trace"]["status"], "unavailable");
    fs::remove_file(&trace).unwrap();
    fs::remove_file(&settings).unwrap();
    let missing_settings = report();
    assert_eq!(missing_settings["status"], "failed");
    assert_eq!(diagnostic(&missing_settings)["trace"]["status"], "missing");
    assert!(
        missing_settings["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["provider"] == "claude" && row["registration"] == "unavailable")
    );
    assert!(!home.join("doctor-provider-invoked").exists());
    fs::write(&codex, original_codex).unwrap();
    fs::write(&settings, original_settings).unwrap();
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&config, original_config).unwrap();
    fs::remove_dir_all(cache).unwrap();
    fs::remove_dir(trace.parent().unwrap()).unwrap();
    fs::remove_dir(source.join(".oms")).unwrap();
}

#[test]
fn native_configuration_and_services() {
    if let Some(root) = std::env::var_os("OMS_LIFECYCLE_SCHEDULER_PATHS_FIXTURE") {
        let root = PathBuf::from(root);
        let source = root.join("source");
        success(call(
            "install-autoupdate",
            &["--method", "cron"],
            &source,
            &NativeServices::default(),
        ));
        let environment = run_fixture_cron(&root);
        assert_eq!(
            environment
                .lines()
                .skip(9)
                .map(PathBuf::from)
                .collect::<Vec<_>>(),
            [
                "selected tools.json",
                "custom codex",
                "custom codex/config.toml",
                "custom claude/settings.json",
                "custom pi",
            ]
            .map(|path| source.join(path))
        );
        return;
    }
    if let Some(root) =
        std::env::var_os("OMS_LIFECYCLE_SYSTEMD_UNINSTALL_FIXTURE").map(PathBuf::from)
    {
        let source = root.join("source");
        let host = NativeServices::default();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&source)
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::write(source.join("VERSION"), "1.0.0\n").unwrap();
        fs::create_dir(source.join("rules")).unwrap();
        fs::write(source.join("rules/global-AGENTS.md"), "managed rules\n").unwrap();
        write(&source.join("skills.manifest.json"), &json!({"skills":[]}));
        success(call("uninstall", &["--yes"], &source, &host));
        return;
    }
    if std::env::var_os("OMS_LIFECYCLE_FIXTURE").is_none() {
        let root = std::env::temp_dir().join(format!("oms-native-test-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let fixture = Fixture(root);
        let git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(git.status.success());
        let git_bin = String::from_utf8(git.stdout).unwrap();
        let git_bin = git_bin.trim();
        fs::create_dir(fixture.0.join("bin")).unwrap();
        symlink(git_bin, fixture.0.join("bin/git")).unwrap();
        let codex = fixture.0.join("bin/codex");
        // Removal records state so a later inventory proves the plugin is gone;
        // only shell builtins are available on this PATH.
        fs::write(&codex, br##"#!/bin/sh
state="$OMS_LIFECYCLE_FIXTURE/codex-state"
now=here
[ -e "$state" ] && read now < "$state"
case "$*" in
  'plugin remove '*|'plugin marketplace remove '*) echo gone > "$state" ;;
  'plugin add '*|'plugin marketplace add '*) echo here > "$state" ;;
  'plugin list --json')
    if [ "$now" = gone ]; then printf '%s\n' '{"installed":[]}'
    else printf '%s\n' '{"installed":[{"pluginId":"oh-my-setting@oh-my-setting-local","installed":true}]}'; fi ;;
  'plugin marketplace list') [ "$now" = gone ] || printf 'oh-my-setting-local %s/source\n' "$OMS_LIFECYCLE_FIXTURE" ;;
esac
"##).unwrap();
        // agy-hook reads one payload from stdin; the re-exec child inherits this file.
        fs::write(
            fixture.0.join("agy-stdin.json"),
            serde_json::to_vec(&json!({"workspacePaths":[fixture.0.join("source")],
                "invocationNum":1,"conversationId":"c1","modelName":"m"}))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        for dir in ["home", "source", "config", "data", "state"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(fixture.0.join(dir))
                .unwrap();
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_configuration_and_services",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::from(
                fs::File::open(fixture.0.join("agy-stdin.json")).unwrap(),
            ))
            .env_clear()
            .env("PATH", fixture.0.join("bin"))
            .env("HOME", fixture.0.join("home"))
            .env("XDG_CONFIG_HOME", fixture.0.join("config"))
            .env("XDG_DATA_HOME", fixture.0.join("data"))
            .env("XDG_STATE_HOME", fixture.0.join("state"))
            .env("XDG_CACHE_HOME", fixture.0.join("cache"))
            .env("CARGO_HOME", fixture.0.join("cargo home'$(unused)%"))
            .env("RUSTUP_HOME", fixture.0.join("rustup home$literal"))
            .env("RUSTUP_TOOLCHAIN", "must-not-be-persisted")
            .env(
                "OMS_CAPABILITY_RECEIPT",
                fixture.0.join("selected capabilities.json"),
            )
            .env("TMPDIR", &fixture.0)
            .env("OMS_LIFECYCLE_FIXTURE", &fixture.0)
            .env(
                "OH_MY_SETTING_AUTO_UPDATE_CRON_FILE",
                fixture.0.join("cron"),
            )
            .env("OMS_TICK_WAKE", "1")
            .env("OMS_TICK_GC", "1")
            .output()
            .unwrap();
        if !out.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let systemd_root = fixture.0.join("systemd-uninstall");
        for dir in [
            "bin", "home", "source", "config", "data", "state", "runtime",
        ] {
            fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(systemd_root.join(dir))
                .unwrap();
        }
        symlink(git_bin, systemd_root.join("bin/git")).unwrap();
        let systemctl = systemd_root.join("bin/systemctl");
        fs::write(
            &systemctl,
            br#"#!/bin/sh
case "${1:-} ${2:-}" in
  '--user show-environment') exit 0 ;;
  '--user disable')
    printf 'Failed to disable unit: Unit file %s does not exist.\n' "${4:-}" >&2
    exit 1 ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
        let crontab = systemd_root.join("bin/crontab");
        fs::write(
            &crontab,
            br#"#!/bin/sh
case "${1:-}" in
  -l)
    printf 'no crontab for fixture\n' >&2
    exit 1 ;;
  -) exit 99 ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&crontab, fs::Permissions::from_mode(0o700)).unwrap();
        let systemd_out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_configuration_and_services",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", systemd_root.join("bin"))
            .env("HOME", systemd_root.join("home"))
            .env("XDG_CONFIG_HOME", systemd_root.join("config"))
            .env("XDG_DATA_HOME", systemd_root.join("data"))
            .env("XDG_STATE_HOME", systemd_root.join("state"))
            .env("XDG_RUNTIME_DIR", systemd_root.join("runtime"))
            .env("TMPDIR", &systemd_root)
            .env("OMS_LIFECYCLE_SYSTEMD_UNINSTALL_FIXTURE", &systemd_root)
            .output()
            .unwrap();
        if !systemd_out.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&systemd_out.stderr));
        }
        assert!(
            systemd_out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&systemd_out.stdout),
            String::from_utf8_lossy(&systemd_out.stderr)
        );
        return;
    }
    let root = PathBuf::from(std::env::var_os("OMS_LIFECYCLE_FIXTURE").unwrap());
    let source = root.join("source");
    let home = root.join("home");
    let host = NativeServices::default();
    bundled_skill_manifest_checks(&root, &host);
    doctor_provider_capability_evidence(&source, &root, &host);
    doctor_configuration_diagnostics(&source, &root, &host);
    assert!(
        Command::new("git")
            .arg("init")
            .arg(&source)
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::write(source.join("VERSION"), "1.0.0\n").unwrap();
    fs::create_dir(source.join("rules")).unwrap();
    fs::write(source.join("rules/global-AGENTS.md"), "managed rules\n").unwrap();
    write(&source.join("skills.manifest.json"), &json!({"skills":[]}));
    write(
        &source.join(".agents/plugins/marketplace.json"),
        &json!({"name":"oh-my-setting-local"}),
    );
    write(
        &source.join("plugins/oh-my-setting/.codex-plugin/plugin.json"),
        &json!({"name":"oh-my-setting","version":"1.0.0","hooks":"./hooks.json"}),
    );
    let template = source.join("plugins/oh-my-setting/hooks.json");
    write(
        &template,
        &json!({"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"bash \"${CLAUDE_PLUGIN_ROOT:-.}/scripts/harness-hook.sh\" skill-router","timeout":5}]}]}}),
    );
    success(call("install-codex-plugin", &[], &source, &host));
    let cache = home.join(".codex/plugins/cache/oh-my-setting-local/oh-my-setting/1.0.0");
    native_only(&read(&cache.join("hooks.json")));
    assert!(!cache.join("scripts").exists());
    let payload = fs::read(cache.join("hooks.json")).unwrap();
    doctor_integration_diagnostics(&source, &root, &host);
    success(call("install-codex-plugin", &[], &source, &host));
    assert_eq!(fs::read(cache.join("hooks.json")).unwrap(), payload);
    // The native inventory does not read the legacy executable template.
    fs::remove_file(&template).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    assert_eq!(fs::read(cache.join("hooks.json")).unwrap(), payload);
    let hooks_file = cache.join("hooks.json");
    write(&hooks_file, &json!({"hooks":{},"foreign":true}));
    assert_ne!(call("install-codex-plugin", &[], &source, &host).0, 0);
    assert_eq!(read(&hooks_file)["foreign"], true);
    fs::write(&hooks_file, &payload).unwrap();
    // Antigravity injects its routing hint through the current systemMessage envelope.
    let hint: Value = serde_json::from_slice(&success(call(
        "agy-hook",
        &["preinvocation"],
        &source,
        &HintHost,
    )))
    .unwrap();
    assert_eq!(
        hint,
        json!({"injectSteps":[{"systemMessage":{"systemMessage":"route to plan"}}]})
    );
    // Codex HUD: the shipped block is six items; the factory bare model is
    // displaced reversibly instead of blocking installation.
    let codex_config = home.join(".codex/config.toml");
    assert!(
        fs::read_to_string(&codex_config)
            .unwrap()
            .contains("\"git-branch\", \"current-dir\"]")
    );
    success(call("install-codex-plugin", &["--remove"], &source, &host));
    assert!(
        !fs::read_to_string(&codex_config)
            .unwrap()
            .contains("managed Codex HUD")
    );
    let bare = "[tui]\nstatus_line = [\"model\"]\n";
    fs::write(&codex_config, bare).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    let displaced = fs::read_to_string(&codex_config).unwrap();
    assert!(displaced.contains("# displaced: status_line = [\"model\"]\n"));
    assert!(displaced.contains("\"current-dir\"]"));
    success(call("install-codex-plugin", &["--remove"], &source, &host));
    assert_eq!(fs::read_to_string(&codex_config).unwrap(), bare);
    // A custom status line is never displaced.
    let custom = "[tui]\nstatus_line = [\"model\", \"git-branch\"]\n";
    fs::write(&codex_config, custom).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    assert!(fs::read_to_string(&codex_config).unwrap().contains(custom));
    success(call("install-codex-plugin", &["--remove"], &source, &host));
    assert_eq!(fs::read_to_string(&codex_config).unwrap(), custom);
    let notify_begin = "# >>> oh-my-setting managed Codex usage (notify) >>>";
    let notify_end = "# <<< oh-my-setting managed Codex usage (notify) <<<";
    let retired = source.join("scripts/lib/codex_turn_notify.py");
    let legacy_notify = format!(
        "notify = [{}, {}]",
        json!("/usr/bin/python3"),
        json!(retired)
    );
    let legacy = format!("{notify_begin}\r\n{legacy_notify}\r\n{notify_end}\r\n{custom}");
    let native_notify = format!(
        "notify = {}",
        json!([home.join(".local/bin/oms"), "codex-turn-notify"])
    );
    fs::write(&codex_config, &legacy).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    let migrated = fs::read_to_string(&codex_config).unwrap();
    assert!(migrated.contains(&legacy.replace(&legacy_notify, &native_notify)));
    success(call("install-codex-plugin", &[], &source, &host));
    assert_eq!(fs::read_to_string(&codex_config).unwrap(), migrated);
    success(call("install-codex-plugin", &["--remove"], &source, &host));
    assert_eq!(fs::read_to_string(&codex_config).unwrap(), custom);
    fs::write(&codex_config, &legacy).unwrap();
    success(call("install-codex-plugin", &["--remove"], &source, &host));
    assert_eq!(fs::read_to_string(&codex_config).unwrap(), custom);
    for override_config in [
        format!("{legacy_notify}\n{custom}"),
        format!("{notify_begin}\n{legacy_notify} # customized\n{notify_end}\n{custom}"),
        format!(
            "{notify_begin}\nnotify = [{}, {}]\n{notify_end}\n{custom}",
            json!("/usr/bin/python3"),
            json!(root.join("other/scripts/lib/codex_turn_notify.py"))
        ),
    ] {
        fs::write(&codex_config, &override_config).unwrap();
        success(call("install-codex-plugin", &[], &source, &host));
        assert!(
            fs::read_to_string(&codex_config)
                .unwrap()
                .contains(&override_config)
        );
        success(call("install-codex-plugin", &["--remove"], &source, &host));
        assert_eq!(fs::read_to_string(&codex_config).unwrap(), override_config);
    }
    fs::write(&codex_config, custom).unwrap();
    let manifest: Value = serde_json::from_slice(&success(call(
        "install-claude-hooks",
        &["--print-expected"],
        &source,
        &host,
    )))
    .unwrap();
    native_only(&manifest);
    assert_eq!(manifest["surfaces"].as_array().unwrap().len(), 14);
    assert_eq!(manifest["hooks_schema"], 13);
    let settings = home.join(".claude/settings.json");
    let legacy = |script: &str| {
        format!(
            "bash '{}' bash '{}'",
            source.join("scripts/lib/python-entry.sh").display(),
            source.join("scripts").join(script).display()
        )
    };
    write(
        &settings,
        &json!({"custom":true,"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[
        {"type":"command","command":legacy("command-guard-hook.sh"),"onFailure":"block"},
        {"type":"command","command":"custom-hook"}
    ]}]},"statusLine":{"type":"command","command":format!("python3 '{}'",source.join("scripts/claude-statusline.py").display())}}),
    );
    success(call("install-claude-hooks", &[], &source, &host));
    let installed = read(&settings);
    native_only(&installed);
    assert_eq!(installed["custom"], true);
    assert!(installed.to_string().contains("custom-hook"));
    assert!(installed.to_string().contains("onFailure"));
    success(call("install-claude-hooks", &[], &source, &host));
    assert_eq!(read(&settings), installed);
    let report: Value = serde_json::from_slice(&success(call(
        "doctor",
        &["--surfaces", "--json"],
        &source,
        &host,
    )))
    .unwrap();
    assert!(!report.to_string().contains("missing or drifted"));
    doctor_hook_evidence(&source, &root, &host, &payload);
    success(call("install-claude-hooks", &["--remove"], &source, &host));
    let removed = read(&settings);
    assert!(removed.to_string().contains("custom-hook"));
    assert!(removed.get("statusLine").is_none());
    write(&settings, &json!({"hooks":[]}));
    assert_ne!(call("install-claude-hooks", &[], &source, &host).0, 0);
    assert_eq!(read(&settings), json!({"hooks":[]}));
    fs::remove_file(&settings).unwrap();
    fs::create_dir_all(source.join("crates/oms")).unwrap();
    fs::write(source.join("crates/oms/Cargo.toml"), "[package]\n").unwrap();
    fs::write(source.join("Cargo.toml"), "[workspace]\n").unwrap();
    let account = home.join("account");
    fs::DirBuilder::new().mode(0o700).create(&account).unwrap();
    let account_settings = account.join("settings.json");
    let side = account.join(".oms-claude-hooks.json");
    let identity = fs::metadata(&account).unwrap();
    let effects = root.join("config/oh-my-setting/provider-account-effects.json");
    write(
        &effects,
        &json!({"schema":1,"accounts":[{"provider":"claude","home":account,
        "identity":{"st_dev":identity.dev(),"st_ino":identity.ino()},"settings":account_settings,
        "hooks_receipt":side,"source_root":source}]}),
    );
    let account_args = [
        "--settings",
        account_settings.to_str().unwrap(),
        "--ownership-receipt",
        side.to_str().unwrap(),
    ];
    let account_safe = account.ancestors().all(|path| {
        let m = fs::metadata(path).unwrap();
        [0, rustix::process::geteuid().as_raw()].contains(&m.uid())
            && (m.mode() & 0o022 == 0 || m.mode() & 0o1000 != 0)
    });
    if account_safe {
        success(call("install-claude-hooks", &account_args, &source, &host));
        native_only(&read(&account_settings));
        let account_installed = read(&account_settings);
        success(call("install-claude-hooks", &account_args, &source, &host));
        assert_eq!(read(&account_settings), account_installed);
        let mut remove = account_args.to_vec();
        remove.push("--remove");
        success(call("install-claude-hooks", &remove, &source, &host));
        assert!(read(&account_settings).get("statusLine").is_none());
    } else {
        let out = call("install-claude-hooks", &account_args, &source, &host);
        assert_ne!(out.0, 0);
        assert!(String::from_utf8_lossy(&out.2).contains("unsafe account ancestor"));
        assert!(!account_settings.exists());
        eprintln!(
            "account round trip skipped: sandbox ancestors have unmapped ownership; refusal verified"
        );
    }
    fs::remove_file(&effects).unwrap();
    success(call(
        "install-hooks",
        &["--quick", "--check-protection"],
        &source,
        &host,
    ));
    let push = fs::read_to_string(source.join(".git/hooks/pre-push")).unwrap();
    assert!(push.contains(" pre-push-check"));
    assert!(!push.contains(".sh"));
    success(call("install-hooks", &[], &source, &host));
    assert!(
        fs::read_to_string(source.join(".git/hooks/pre-push"))
            .unwrap()
            .contains("--full")
    );
    assert_ne!(
        call(
            "install-hooks",
            &["--quick", "--check-protection"],
            &source,
            &UnavailableServices
        )
        .0,
        0
    );
    let hooks_dir = source.join(".git/hooks");
    // The exact e0b92359 hook is upgraded to the native full hook; any variant stays protected.
    let old_push = "#!/usr/bin/env bash\n# Installed by scripts/install-hooks.sh \u{2014} runs the repo gate before every push.\n# Bypass for an emergency with: git push --no-verify\nset -euo pipefail\nroot=\"$(git rev-parse --show-toplevel)\"\nexec \"$root/scripts/check.sh\"\n";
    let push_hook = hooks_dir.join("pre-push");
    let variant = format!("{old_push}echo mine\n");
    fs::write(&push_hook, &variant).unwrap();
    assert_ne!(call("install-hooks", &[], &source, &host).0, 0);
    assert_eq!(fs::read_to_string(&push_hook).unwrap(), variant);
    fs::write(&push_hook, old_push).unwrap();
    success(call("install-hooks", &[], &source, &host));
    let upgraded = fs::read_to_string(&push_hook).unwrap();
    assert!(upgraded.contains(" pre-push-check") && upgraded.contains("--full"));
    // --repo converges only the selected distinct OMS worktree's hooks; the canonical hooks stay put.
    let other = root.join("other-repo");
    let git_in = |dir: &Path, args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git_in(&root, &["init", other.to_str().unwrap()]);
    fs::create_dir_all(other.join("crates/oms")).unwrap();
    fs::write(other.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(other.join("crates/oms/Cargo.toml"), "[package]\n").unwrap();
    fs::create_dir_all(other.join("scripts")).unwrap();
    fs::write(other.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
    let other_push = other.join(".git/hooks/pre-push");
    fs::write(&other_push, old_push).unwrap();
    let canonical_push = fs::read_to_string(&push_hook).unwrap();
    let repo_arg = ["--repo", other.to_str().unwrap()];
    success(call("install-hooks", &repo_arg, &source, &host));
    let converged = fs::read_to_string(&other_push).unwrap();
    assert!(converged.contains(" pre-push-check") && converged.contains("--full"));
    assert!(other.join(".git/hooks/pre-merge-commit").exists());
    assert_eq!(fs::read_to_string(&push_hook).unwrap(), canonical_push);
    let edited = format!("{old_push}echo mine\n");
    fs::write(&other_push, &edited).unwrap();
    assert_ne!(call("install-hooks", &repo_arg, &source, &host).0, 0);
    assert_eq!(fs::read_to_string(&other_push).unwrap(), edited);
    // A linked worktree shares the common-dir hooks; a non-OMS or subdirectory target is refused.
    fs::write(&other_push, old_push).unwrap();
    git_in(
        &other,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "x",
        ],
    );
    let linked = root.join("linked-repo");
    git_in(
        &other,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(linked.join("crates/oms")).unwrap();
    fs::write(linked.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(linked.join("crates/oms/Cargo.toml"), "[package]\n").unwrap();
    fs::create_dir_all(linked.join("scripts")).unwrap();
    fs::write(linked.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
    success(call(
        "install-hooks",
        &["--repo", linked.to_str().unwrap()],
        &source,
        &host,
    ));
    assert!(fs::read_to_string(&other_push).unwrap().contains("--full"));
    assert_ne!(
        call(
            "install-hooks",
            &["--repo", other.join("crates").to_str().unwrap()],
            &source,
            &host
        )
        .0,
        0
    );
    let plain = root.join("plain-repo");
    git_in(&root, &["init", plain.to_str().unwrap()]);
    assert_ne!(
        call(
            "install-hooks",
            &["--repo", plain.to_str().unwrap()],
            &source,
            &host
        )
        .0,
        0
    );
    assert!(!plain.join(".git/hooks/pre-push").exists());
    // An external core.hooksPath would write beyond the selected repository: refused, bytes kept.
    let external = root.join("external-hooks");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("pre-push"), old_push).unwrap();
    let prior = fs::read_to_string(&other_push).unwrap();
    fs::write(&other_push, old_push).unwrap();
    git_in(
        &other,
        &["config", "core.hooksPath", external.to_str().unwrap()],
    );
    assert_ne!(call("install-hooks", &repo_arg, &source, &host).0, 0);
    assert_eq!(fs::read_to_string(&other_push).unwrap(), old_push);
    assert_eq!(
        fs::read_to_string(external.join("pre-push")).unwrap(),
        old_push
    );
    git_in(&other, &["config", "--unset", "core.hooksPath"]);
    fs::write(&other_push, prior).unwrap();
    // Protection binds to the validated selected target, never the canonical owner.
    struct ProtectedOnly(PathBuf, RefCell<Vec<PathBuf>>);
    impl Services for ProtectedOnly {
        fn hook_protection(&self, source: &Path, _: &AtomicBool) -> Output {
            self.1.borrow_mut().push(source.to_path_buf());
            (i32::from(source != self.0), Vec::new(), Vec::new())
        }
    }
    fn quick(repo: &Path) -> [&str; 4] {
        [
            "--quick",
            "--check-protection",
            "--repo",
            repo.to_str().unwrap(),
        ]
    }
    let wrong_owner = ProtectedOnly(fs::canonicalize(&source).unwrap(), RefCell::default());
    assert_ne!(
        call("install-hooks", &quick(&other), &source, &wrong_owner).0,
        0
    );
    assert_eq!(fs::read_to_string(&other_push).unwrap(), converged);
    assert_ne!(
        call("install-hooks", &quick(&plain), &source, &wrong_owner).0,
        0
    );
    assert!(!plain.join(".git/hooks/pre-push").exists());
    assert_ne!(
        call(
            "install-hooks",
            &quick(&other.join("crates")),
            &source,
            &wrong_owner
        )
        .0,
        0
    );
    assert!(wrong_owner.1.borrow().iter().all(|p| p.starts_with(&other)));
    // Explicit installation needs the target's own gate script; removal does not.
    fs::remove_file(other.join("scripts/check.sh")).unwrap();
    assert_ne!(call("install-hooks", &repo_arg, &source, &host).0, 0);
    assert_eq!(fs::read_to_string(&other_push).unwrap(), converged);
    fs::write(other.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
    let right = ProtectedOnly(fs::canonicalize(&other).unwrap(), RefCell::default());
    success(call("install-hooks", &quick(&other), &source, &right));
    assert!(!fs::read_to_string(&other_push).unwrap().contains("--full"));
    assert_eq!(*right.1.borrow(), vec![fs::canonicalize(&other).unwrap()]);
    assert_eq!(fs::read_to_string(&push_hook).unwrap(), canonical_push);
    // Explicit removal must prove its target: non-Git and bare_repo targets fail, metadata stays.
    let no_git = root.join("no-git-repo");
    let bare_repo = root.join("bare_repo-repo");
    for dir in [&no_git, &bare_repo] {
        fs::create_dir_all(dir.join("crates/oms")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(dir.join("crates/oms/Cargo.toml"), "[package]\n").unwrap();
    }
    git_in(
        &root,
        &["init", "-q", "--bare", bare_repo.to_str().unwrap()],
    );
    for dir in [&no_git, &bare_repo] {
        assert_ne!(
            call(
                "install-hooks",
                &["--remove", "--repo", dir.to_str().unwrap()],
                &source,
                &host
            )
            .0,
            0
        );
    }
    assert!(other_push.exists());
    assert_eq!(
        fs::read_to_string(external.join("pre-push")).unwrap(),
        old_push
    );
    // A valid explicit removal needs no gate script.
    fs::remove_file(other.join("scripts/check.sh")).unwrap();
    success(call(
        "install-hooks",
        &["--remove", "--repo", other.to_str().unwrap()],
        &source,
        &host,
    ));
    assert!(!other_push.exists());
    assert_eq!(fs::read_to_string(&push_hook).unwrap(), canonical_push);
    let merge = hooks_dir.join("pre-merge-commit");
    let merge_text = fs::read_to_string(&merge).unwrap();
    assert!(merge_text.starts_with("#!/bin/sh\n# oh-my-setting native pre-merge-commit\n"));
    assert!(merge_text.contains(" pre-push-check --pre-merge \"$@\"\n"));
    assert_eq!(
        fs::metadata(&merge).unwrap().permissions().mode() & 0o111,
        0o111
    );
    // An unmanaged merge hook is never replaced, and the managed pre-push stays untouched.
    fs::write(&merge, "#!/bin/sh\necho mine\n").unwrap();
    assert_ne!(call("install-hooks", &[], &source, &host).0, 0);
    assert_eq!(
        fs::read_to_string(&merge).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    fs::remove_file(&merge).unwrap();
    success(call("install-hooks", &[], &source, &host));
    assert!(fs::read_to_string(&merge).unwrap().contains("--pre-merge"));
    // Withdrawal removes OMS hooks only; a user's own hook survives it.
    fs::write(&merge, "#!/bin/sh\necho mine\n").unwrap();
    success(call("install-hooks", &["--remove"], &source, &host));
    assert_eq!(
        fs::read_to_string(&merge).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    assert!(!hooks_dir.join("pre-push").exists());
    fs::remove_file(&merge).unwrap();
    success(call(
        "install-autoupdate",
        &["--method", "cron"],
        &source,
        &host,
    ));
    let cron = fs::read_to_string(root.join("cron")).unwrap();
    assert!(cron.contains(" auto-update apply"));
    assert!(!cron.contains(".sh"));
    let cargo_home = PathBuf::from(std::env::var_os("CARGO_HOME").unwrap());
    let cargo = cargo_home.join("bin/cargo");
    fs::create_dir_all(cargo.parent().unwrap()).unwrap();
    fs::write(&cargo, format!(
        "#!/bin/sh\n[ \"$1\" = --version ] || exit 9\nprintf '%s\\n' \"$CARGO_HOME\" \"$RUSTUP_HOME\" \"$XDG_CONFIG_HOME\" \"$XDG_DATA_HOME\" \"$XDG_STATE_HOME\" \"$XDG_CACHE_HOME\" \"$OMS_INSTALL_RECEIPT\" \"$OMS_CAPABILITY_RECEIPT\" \"${{RUSTUP_TOOLCHAIN-unset}}\" \"${{OH_MY_SETTING_TOOL_LOCK-}}\" \"${{CODEX_HOME-}}\" \"${{OMS_CODEX_CONFIG-}}\" \"${{OMS_CLAUDE_SETTINGS-}}\" \"${{PI_CODING_AGENT_DIR-}}\" > '{}/scheduled-environment'\n",
        root.display()
    )).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
    let scheduled_oms = home.join(".local/bin/oms");
    fs::create_dir_all(scheduled_oms.parent().unwrap()).unwrap();
    fs::write(&scheduled_oms, "#!/bin/sh\nexec cargo --version\n").unwrap();
    fs::set_permissions(&scheduled_oms, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        run_fixture_cron(&root),
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\nunset\n\n\n\n\n\n",
            cargo_home.display(),
            root.join("rustup home$literal").display(),
            root.join("config").display(),
            root.join("data").display(),
            root.join("state").display(),
            root.join("cache").display(),
            root.join("config/oh-my-setting/install.json").display(),
            root.join("selected capabilities.json").display()
        )
    );
    let overrides = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_configuration_and_services",
            "--nocapture",
        ])
        .env("OMS_LIFECYCLE_SCHEDULER_PATHS_FIXTURE", &root)
        .env("OH_MY_SETTING_TOOL_LOCK", "selected tools.json")
        .env("CODEX_HOME", "custom codex")
        .env("OMS_CODEX_CONFIG", "custom codex/config.toml")
        .env("OMS_CLAUDE_SETTINGS", "custom claude/settings.json")
        .env("PI_CODING_AGENT_DIR", "custom pi")
        .output()
        .unwrap();
    assert!(
        overrides.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&overrides.stdout),
        String::from_utf8_lossy(&overrides.stderr)
    );
    fs::remove_file(scheduled_oms).unwrap();
    success(call(
        "install-autoupdate",
        &["--method", "cron", "--check-only"],
        &source,
        &host,
    ));
    let check_cron = fs::read_to_string(root.join("cron")).unwrap();
    assert!(check_cron.contains(" auto-update check"));
    assert!(!check_cron.contains(" auto-update apply"));
    success(call("uninstall-autoupdate", &[], &source, &host));
    write(
        &home.join(".claude.json"),
        &json!({"custom":true,"mcpServers":{"oh-my-setting":{"type":"stdio","command":"python3","args":[source.join("scripts/oms-mcp-server.py")],"env":{"USER_CUSTOM":"1"}}}}),
    );
    assert_ne!(call("install-mcp", &["--remove"], &source, &host).0, 0);
    write(
        &home.join(".claude.json"),
        &json!({"custom":true,"mcpServers":{"oh-my-setting":{"type":"stdio","command":"python3","args":[source.join("scripts/oms-mcp-server.py")],"env":{}}}}),
    );
    let legacy_toml = format!(
        "{custom}\n[mcp_servers.oh-my-setting]\ncommand = \"python3\"\nargs = [\"{}\"]\n",
        source.join("scripts/oms-mcp-server.py").display()
    );
    fs::write(&codex_config, &legacy_toml).unwrap();
    success(call("install-mcp", &["--remove"], &source, &host));
    assert_eq!(read(&home.join(".claude.json"))["custom"], true);
    assert!(
        read(&home.join(".claude.json"))["mcpServers"]
            .get("oh-my-setting")
            .is_none()
    );
    let cleaned_toml = fs::read_to_string(&codex_config).unwrap();
    assert!(!cleaned_toml.contains("mcp_servers"));
    assert!(cleaned_toml.contains(custom));
    fs::write(
        &codex_config,
        format!("{custom}\n[mcp_servers.oh-my-setting]\ncommand = \"custom-mcp\"\nargs = []\n"),
    )
    .unwrap();
    let refused_mcp = call("install-mcp", &["--remove"], &source, &host);
    assert_ne!(refused_mcp.0, 0);
    assert!(
        String::from_utf8_lossy(&refused_mcp.2)
            .contains("an unmanaged TOML table already occupies this registration")
    );
    fs::write(&codex_config, custom).unwrap();
    write(
        &source.join("plugins/antigravity/plugin.json"),
        &json!({"name":"oh-my-setting","version":"0.0.0","description":"oms-mcp"}),
    );
    let agy_plugin = home.join(".gemini/config/plugins/oh-my-setting");
    let legacy_python = root.join("data/oh-my-setting/python-runtime/launchers/3.12.15/python3");
    write(
        &agy_plugin.join("plugin.json"),
        &json!({"name":"oh-my-setting","version":"0.7.0","description":"oms-mcp"}),
    );
    write(
        &agy_plugin.join("mcp_config.json"),
        &json!({"mcpServers":{"oh-my-setting":{"command":legacy_python,"args":[source.join("scripts/oms-mcp-server.py")]}}}),
    );
    write(
        &agy_plugin.join("hooks.json"),
        &json!({"oh-my-setting":{"PreToolUse":[{"matcher":"invoke_subagent","hooks":[{"type":"command","command":format!("{} pretooluse", source.join("scripts/oms").display())}]}]}}),
    );
    success(call("install-agy-plugin", &["--remove"], &source, &host));
    assert!(!agy_plugin.exists());
    write(
        &agy_plugin.join("plugin.json"),
        &json!({"name":"other","version":"0.7.0","description":"oms-mcp"}),
    );
    write(
        &agy_plugin.join("mcp_config.json"),
        &json!({"mcpServers":{"oh-my-setting":{"command":legacy_python,"args":[source.join("scripts/oms-mcp-server.py")]}}}),
    );
    assert_ne!(
        call("install-agy-plugin", &["--remove"], &source, &host).0,
        0
    );
    fs::remove_dir_all(&agy_plugin).unwrap();
    write(
        &source.join("config/capability-profiles.json"),
        &json!({"schema":1,"profiles":{"core":{
            "required":["python3","git"],"any_of":[],"optional":[],"managed_tools":[]
        }}}),
    );
    let tools: Value =
        serde_json::from_slice(&success(call("install-tools", &["--plan"], &source, &host)))
            .unwrap();
    assert_eq!(tools["missing"], json!([]));
    assert_eq!(tools["native_runtime"], true);
    // Missing native authority fails before any install or MCP publication.
    assert_ne!(call("install-mcp", &[], &source, &host).0, 0);
    let plan: Value =
        serde_json::from_slice(&success(call("install", &["--plan"], &source, &host))).unwrap();
    native_only(&plan);
    // Antigravity auto-enables only when its CLI directory exists.
    assert_eq!(plan["install"]["components"]["agy_plugin"], false);
    let agy_dir = home.join(".gemini/antigravity-cli");
    fs::create_dir_all(&agy_dir).unwrap();
    let plan: Value =
        serde_json::from_slice(&success(call("install", &["--plan"], &source, &host))).unwrap();
    assert_eq!(plan["install"]["components"]["agy_plugin"], true);
    // statusLine removal drops only the OMS-owned entry and keeps user settings.
    let agy_settings = agy_dir.join("settings.json");
    let owned = format!("'{}' agy-statusline", home.join(".local/bin/oms").display());
    write(
        &agy_settings,
        &json!({"theme":"x","statusLine":{"type":"command","command":owned,"enabled":true}}),
    );
    success(call("install-agy-plugin", &["--remove"], &source, &host));
    assert_eq!(read(&agy_settings), json!({"theme":"x"}));
    let user_line = json!({"statusLine":{"type":"command","command":"my-hud"}});
    write(&agy_settings, &user_line);
    success(call("install-agy-plugin", &["--remove"], &source, &host));
    assert_eq!(read(&agy_settings), user_line);
    native_only(&plan);
    assert_ne!(call("install", &[], &source, &host).0, 0);
    success(call("tick", &["schedule", "--list"], &source, &host));
    success(call("tick", &["refresh-catalogs"], &source, &host));
    fs::create_dir(source.join(".oms")).unwrap();
    // A real sweep reaches the native maintenance, catalog, GC and wake callbacks.
    let sweep = call(
        "tick",
        &["run", "--repo", source.to_str().unwrap(), "--json"],
        &source,
        &host,
    );
    let body: Value = serde_json::from_slice(&sweep.1).unwrap();
    assert_eq!(body["sweeps"][0]["tasks_closed"], 0);
    for method in [
        "protection",
        "schedule",
        "catalogs",
        "maintenance",
        "gc",
        "wake",
    ] {
        assert!(
            host.0.borrow().iter().any(|s| s == method),
            "missing {method}"
        );
    }
    assert_ne!(
        call(
            "tick",
            &["run", "--repo", source.to_str().unwrap()],
            &source,
            &UnavailableServices
        )
        .0,
        0
    );
    // An invalid previous-day wake row refuses the sweep and keeps .oms/tick/last.json unchanged.
    fs::create_dir_all(source.join(".oms/tick")).unwrap();
    let bad_prev = "{\"wakes\":[{\"day\":\"2026-11-09\",\"main\":\"m\"}]}";
    fs::write(source.join(".oms/tick/last.json"), bad_prev).unwrap();
    let bad_sweep = call(
        "tick",
        &["run", "--repo", source.to_str().unwrap()],
        &source,
        &host,
    );
    assert_ne!(bad_sweep.0, 0);
    assert!(
        String::from_utf8_lossy(&bad_sweep.2).contains("malformed; prior receipt retained"),
        "{:?}",
        String::from_utf8_lossy(&bad_sweep.2)
    );
    assert_eq!(
        fs::read_to_string(source.join(".oms/tick/last.json")).unwrap(),
        bad_prev
    );
    // A wake callback reporting `refused:` with omitted wakes refuses sweep publication even when kept_receipt succeeded.
    struct RefusingWakeHost<'a>(&'a NativeServices);
    impl Services for RefusingWakeHost<'_> {
        fn hook_protection(&self, source: &Path, stop: &AtomicBool) -> Output {
            self.0.hook_protection(source, stop)
        }
        fn tick_maintenance(&self, repo: &Path, source: &Path, stop: &AtomicBool) -> Output {
            self.0.tick_maintenance(repo, source, stop)
        }
        fn refresh_catalogs(&self, preview: bool, source: &Path, stop: &AtomicBool) -> Output {
            self.0.refresh_catalogs(preview, source, stop)
        }
        fn wake_idle_main(&self, _: bool, _: &Path, _: &AtomicBool) -> Output {
            (
                0,
                b"{\"wake\":0,\"status\":\"refused: tick receipt or its wakes ledger is malformed\"}\n".to_vec(),
                Vec::new(),
            )
        }
    }
    let kept_valid = "{\"schema\":1,\"wakes\":[]}";
    fs::write(source.join(".oms/tick/last.json"), kept_valid).unwrap();
    fs::write(source.join(".oms/tick/schedules.jsonl"), "").unwrap();
    let refused_sweep = call(
        "tick",
        &["run", "--repo", source.to_str().unwrap()],
        &source,
        &RefusingWakeHost(&host),
    );
    assert_ne!(refused_sweep.0, 0);
    assert!(
        String::from_utf8_lossy(&refused_sweep.2).contains("sweep publication refused"),
        "{:?}",
        String::from_utf8_lossy(&refused_sweep.2)
    );
    assert_eq!(
        fs::read_to_string(source.join(".oms/tick/last.json")).unwrap(),
        kept_valid
    );
    fs::remove_file(source.join(".oms/tick/schedules.jsonl")).unwrap();
    fs::remove_file(source.join(".oms/tick/last.json")).unwrap();
    // Uninstall migrates a legacy owned interpreter link without requiring its producer.
    fs::create_dir_all(home.join(".local/bin")).unwrap();
    symlink(
        root.join("data/oh-my-setting/python-runtime/bin/python3"),
        home.join(".local/bin/python3"),
    )
    .unwrap();
    // Uninstall withdraws every owned HUD and git hook, whatever the receipt says.
    fs::write(&codex_config, bare).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    assert!(
        fs::read_to_string(&codex_config)
            .unwrap()
            .contains("# displaced:")
    );
    write(
        &agy_settings,
        &json!({"theme":"x","statusLine":{"type":"command","command":owned,"enabled":true}}),
    );
    success(call("install-hooks", &[], &source, &host));
    assert!(hooks_dir.join("pre-push").exists() && hooks_dir.join("pre-merge-commit").exists());
    success(call("uninstall", &["--yes"], &source, &host));
    assert_eq!(fs::read_to_string(&codex_config).unwrap(), bare);
    assert_eq!(read(&agy_settings), json!({"theme":"x"}));
    assert!(!hooks_dir.join("pre-push").exists() && !hooks_dir.join("pre-merge-commit").exists());
    assert!(fs::symlink_metadata(home.join(".local/bin/python3")).is_err());
    symlink(
        "../../../data/oh-my-setting/python-runtime/bin/python3",
        home.join(".local/bin/python3"),
    )
    .unwrap();
    success(call("unlink", &[], &source, &host));
    assert!(fs::symlink_metadata(home.join(".local/bin/python3")).is_err());
    symlink("foreign-runtime", home.join(".local/bin/python3")).unwrap();
    success(call("unlink", &[], &source, &host));
    assert_eq!(
        fs::read_link(home.join(".local/bin/python3")).unwrap(),
        PathBuf::from("foreign-runtime")
    );
    // A tampered native cache: bare install refuses it in place; doctor --repair
    // moves it aside (dangling symlinks too) before reinstalling.
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=fixture"])
            .arg("-c")
            .arg(format!("user.email=fixture@{}", "example.invalid"))
            .args(args)
            .current_dir(&source)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["commit", "-q", "--allow-empty", "-m", "fixture"]);
    let revision = git(&["rev-parse", "HEAD"]);
    let native = source.join("target/oms-native");
    let cached = native.join(&revision);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&cached)
        .unwrap();
    fs::write(cached.join("manifest.json"), b"{}").unwrap();
    fs::write(cached.join("oms"), b"tampered").unwrap();
    let quarantined = || -> Vec<PathBuf> {
        let prefix = format!(".broken-{revision}-");
        fs::read_dir(&native)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&prefix)
            })
            .collect()
    };
    let bare = call("install", &[], &source, &host);
    assert_eq!(bare.0, 2);
    assert!(String::from_utf8_lossy(&bare.2).contains("native payload/source proof failed"));
    assert_eq!(fs::read(cached.join("oms")).unwrap(), b"tampered");
    assert!(quarantined().is_empty());
    let repaired = call("doctor", &["--repair"], &source, &host);
    assert_ne!(repaired.0, 0);
    assert!(String::from_utf8_lossy(&repaired.2).contains("cargo unavailable"));
    assert!(fs::symlink_metadata(&cached).is_err());
    let moved = quarantined();
    assert_eq!(moved.len(), 1);
    assert_eq!(fs::read(moved[0].join("oms")).unwrap(), b"tampered");
    symlink("missing-target", &cached).unwrap();
    assert_ne!(call("doctor", &["--repair"], &source, &host).0, 0);
    assert!(fs::symlink_metadata(&cached).is_err());
    let moved = quarantined();
    assert_eq!(moved.len(), 2);
    assert!(
        moved
            .iter()
            .any(|p| fs::symlink_metadata(p).unwrap().file_type().is_symlink())
    );
    // Exercise every unavailable method: none may silently certify native work.
    let missing: &dyn Services = &UnavailableServices;
    let flag = AtomicBool::new(false);
    let operations = [
        missing.install_extras(IncomingPhase::Preflight, &source, &[], &flag),
        missing.hook_protection(&source, &flag),
        missing.apply_project_template("general", &root, &source, &flag),
        missing.provider_hook("skill-router", b"{}", &source, &source, true, &flag),
        missing.incoming_lifecycle(IncomingPhase::Verify, &source, "", b"{}", &[], &flag),
        missing.tick_maintenance(&source, &source, &flag),
        missing.tick_schedule(&[], &source, &source, &flag),
        missing.refresh_catalogs(false, &source, &flag),
        missing.garbage_collect(&source, &flag),
        missing.wake_idle_main(false, &source, &flag),
    ];
    assert!(
        operations
            .iter()
            .all(|out| out.0 == 2 && out.1.is_empty() && !out.2.is_empty())
    );
    // Host implementors keep all ten signatures, including every incoming phase.
    success(host.apply_project_template("general", &root, &source, &flag));
    success(host.provider_hook("skill-router", b"{}", &source, &source, true, &flag));
    for phase in [
        IncomingPhase::Preflight,
        IncomingPhase::Reconcile,
        IncomingPhase::Verify,
        IncomingPhase::RefreshTools,
        IncomingPhase::VerifyIntegrations,
    ] {
        success(host.install_extras(phase, &source, &[], &flag));
        success(host.incoming_lifecycle(phase, &source, "", b"{}", &[], &flag));
    }
}

#[test]
fn agy_native_calls_reach_panel_attempts() {
    use oms_command_operations::{StartRequest, create_attempt, load_projection, value::obj};
    if let Some(root) = std::env::var_os("OMS_AGY_CALL_FIXTURE").map(PathBuf::from) {
        let action = std::env::var("OMS_AGY_CALL_ACTION").unwrap();
        let host = NativeServices::default();
        let output = call("agy-hook", &[&action], &root, &host);
        assert_eq!(output.0, 0);
        fs::write(root.join("last-hooks.txt"), host.0.borrow().join(",")).unwrap();
        print!("{}", String::from_utf8(output.1).unwrap());
        return;
    }
    if std::env::var_os("OMS_AGY_TEST_RUN").is_none() {
        let locks = std::env::temp_dir().join(format!("oms-agy-locks-{}", std::process::id()));
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agy_native_calls_reach_panel_attempts",
                "--nocapture",
            ])
            .env("OMS_AGY_TEST_RUN", "1")
            .env("OMS_LOCK_DIR", &locks)
            .output()
            .unwrap();
        if locks.exists() {
            fs::remove_dir_all(locks).unwrap();
        }
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let root = std::env::temp_dir().join(format!("oms-agy-calls-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let fixture = Fixture(root.clone());
    fs::create_dir(root.join(".oms")).unwrap();
    let flag = AtomicBool::new(false);
    let main = create_attempt(
        &root,
        &StartRequest {
            provider: "antigravity".into(),
            tool: "agy".into(),
            refs: obj([
                ("panel_role", "main".into()),
                ("panel_room_id", "room-test".into()),
                ("native_session", "main".into()),
            ]),
            ..Default::default()
        },
        &flag,
    )
    .unwrap();
    let invoke = |action: &str, payload: Value| -> Value {
        let input = root.join("input.json");
        fs::write(&input, serde_json::to_vec(&payload).unwrap()).unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agy_native_calls_reach_panel_attempts",
                "--nocapture",
            ])
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .env("OMS_LOCK_DIR", root.join("locks"))
            .env("OMS_AGY_CALL_FIXTURE", &root)
            .env("OMS_AGY_CALL_ACTION", action)
            .env_remove("OMS_PANEL_MAIN_ATTEMPT")
            .env_remove("OMS_ROOM_ID")
            .stdin(fs::File::open(input).unwrap())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        let start = stdout.find('{').unwrap();
        serde_json::Deserializer::from_str(&stdout[start..])
            .into_iter::<Value>()
            .next()
            .unwrap()
            .unwrap()
    };
    for (id, name, args) in [
        ("s", "invoke_subagent", json!({"TypeName":"research"})),
        ("w", "swarm", json!({"action":"add"})),
        ("f", "run_workflow", json!({})),
    ] {
        assert_eq!(
            invoke(
                "pretooluse",
                json!({"workspacePaths":[root],"conversationId":"main", "toolCall":{"id":id,"name":name,"args":args}})
            ),
            json!({"decision":"allow"})
        );
    }
    let (_, rows) = load_projection(&root).unwrap();
    let children: Vec<_> = rows
        .iter()
        .filter(|v| v.get("parent_attempt_id").text() == Some(main.as_str()))
        .collect();
    assert_eq!(children.len(), 3);
    assert!(
        children
            .iter()
            .all(|v| v.get("state").text() == Some("working")
                && v.get("refs").get("panel_room_id").text() == Some("room-test")),
        "{children:?}"
    );
    invoke(
        "posttooluse",
        json!({"workspacePaths":[root],"toolCall":{"id":"f"},"error":"failure"}),
    );
    invoke(
        "posttooluse",
        json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"k-fail","name":"manage_subagents","args":{"Action":"kill_all"}},"error":"kill failed"}),
    );
    let (_, after_fail_kill) = load_projection(&root).unwrap();
    assert_eq!(
        after_fail_kill
            .iter()
            .find(|v| v.get("tool").text() == Some("agy-subagent"))
            .unwrap()
            .get("state")
            .text(),
        Some("working")
    );
    invoke(
        "posttooluse",
        json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"k-ok","name":"manage_subagents","args":{"Action":"kill_all"}}}),
    );
    invoke(
        "stop",
        json!({"workspacePaths":[root],"conversationId":"main","fullyIdle":true}),
    );
    let (_, rows) = load_projection(&root).unwrap();
    for (tool, state) in [
        ("agy-subagent", "cancelled"),
        ("agy-swarm", "done"),
        ("agy-workflow", "failed"),
    ] {
        assert_eq!(
            rows.iter()
                .find(|v| v.get("tool").text() == Some(tool))
                .unwrap()
                .get("state")
                .text(),
            Some(state)
        );
    }
    fs::write(
        root.join(".oms/panel-state/agy-native-calls.json"),
        b"{corrupt",
    )
    .unwrap();
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"c2","name":"invoke_subagent","args":{"TypeName":"self","Prompt":"inspect c2"}}})
        ),
        json!({"decision":"allow"})
    );
    let (_, recovered) = load_projection(&root).unwrap();
    assert!(
        recovered
            .iter()
            .any(|v| v.get("refs").get("agy_tool_call_id").text() == Some("c2"))
    );

    // An unrelated main conversation does not hijack a single pending subagent,
    // while matching initial prompt correlates PreInvocation -> child Stop.
    let other_pre = invoke(
        "preinvocation",
        json!({"workspacePaths":[root],"conversationId":"other-main","invocationNum":1,"lastUserInput":"unrelated prompt"}),
    );
    assert!(other_pre["injectSteps"].is_array(), "{other_pre}");
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"other-main","fullyIdle":true,"terminationReason":"GOAL"})
        ),
        json!({})
    );
    assert_eq!(
        fs::read_to_string(root.join("last-hooks.txt")).unwrap(),
        "hook,hook"
    );
    let (_, untouched) = load_projection(&root).unwrap();
    assert_eq!(
        untouched
            .iter()
            .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("c2"))
            .unwrap()
            .get("state")
            .text(),
        Some("working")
    );

    assert_eq!(
        invoke(
            "preinvocation",
            json!({"workspacePaths":[root],"conversationId":"child-c2","invocationNum":1,"lastUserInput":"inspect c2"})
        ),
        json!({})
    );
    assert_eq!(fs::read_to_string(root.join("last-hooks.txt")).unwrap(), "");
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"child-c2","fullyIdle":true,"terminationReason":"GOAL"})
        ),
        json!({})
    );
    assert_eq!(fs::read_to_string(root.join("last-hooks.txt")).unwrap(), "");
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"child-c2","fullyIdle":true,"terminationReason":"GOAL"})
        ),
        json!({})
    );
    assert_eq!(fs::read_to_string(root.join("last-hooks.txt")).unwrap(), "");
    let (_, settled) = load_projection(&root).unwrap();
    assert_eq!(
        settled
            .iter()
            .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("c2"))
            .unwrap()
            .get("state")
            .text(),
        Some("done")
    );
    for (id, role) in [("p1", "first"), ("p2", "second")] {
        assert_eq!(
            invoke(
                "pretooluse",
                json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":id,"name":"invoke_subagent","args":{"TypeName":"self","Role":role,"Prompt":"shared prompt"}}})
            ),
            json!({"decision":"allow"})
        );
    }
    let ambig_pre = invoke(
        "preinvocation",
        json!({"workspacePaths":[root],"conversationId":"ambig-child","invocationNum":1,"lastUserInput":"shared prompt"}),
    );
    assert!(ambig_pre["injectSteps"].is_array(), "{ambig_pre}");
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"ambig-child","fullyIdle":true,"terminationReason":"GOAL"})
        ),
        json!({})
    );
    let (_, still_working) = load_projection(&root).unwrap();
    for id in ["p1", "p2"] {
        assert_eq!(
            still_working
                .iter()
                .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some(id))
                .unwrap()
                .get("state")
                .text(),
            Some("working")
        );
    }
    // Unrelated conversations without verified ownership never attach to the active main.
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"other-main","toolCall":{"id":"unrel-1","name":"invoke_subagent","args":{"TypeName":"research","Prompt":"unrelated work"}}})
        ),
        json!({"decision":"allow"})
    );
    let (_, after_unrelated) = load_projection(&root).unwrap();
    assert!(
        after_unrelated
            .iter()
            .all(
                |v| v.get("refs").get("agy_tool_call_id").text() != Some("unrel-1")
                    && v.get("refs").get("agy_parent_conversation").text() != Some("other-main")
            )
    );

    // Replay after interruption restores agy-native-calls.json for an already working attempt.
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"replay-1","name":"invoke_subagent","args":{"TypeName":"worker","Role":"Replay Test","Prompt":"replay prompt"}}})
        ),
        json!({"decision":"allow"})
    );
    let (_, before_drop) = load_projection(&root).unwrap();
    let replay_row = before_drop
        .iter()
        .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("replay-1"))
        .unwrap();
    assert_eq!(replay_row.get("state").text(), Some("working"));
    let replay_attempt = replay_row.get("attempt_id").text().unwrap().to_owned();
    let calls_path = root.join(".oms/panel-state/agy-native-calls.json");
    let mut dropped = read(&calls_path);
    dropped["calls"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["call_id"] != "replay-1");
    write(&calls_path, &dropped);
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"replay-1","name":"invoke_subagent","args":{"TypeName":"worker","Role":"Replay Test","Prompt":"replay prompt"}}})
        ),
        json!({"decision":"allow"})
    );
    let restored = read(&calls_path);
    assert!(
        restored["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| { e["call_id"] == "replay-1" && e["attempt_id"] == replay_attempt.as_str() })
    );
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"main","fullyIdle":true})
        ),
        json!({})
    );
    let (_, after_replay_stop) = load_projection(&root).unwrap();
    assert_eq!(
        after_replay_stop
            .iter()
            .find(|v| v.get("attempt_id").text() == Some(replay_attempt.as_str()))
            .unwrap()
            .get("state")
            .text(),
        Some("done")
    );

    // Another active main submitting the exact same prompt is never attached as a child,
    // and losing agy-native-calls.json while children run recovers from the durable ledger on Stop.
    let main2 = create_attempt(
        &root,
        &StartRequest {
            provider: "antigravity".into(),
            tool: "agy".into(),
            refs: obj([
                ("panel_role", "main".into()),
                ("panel_room_id", "room-test".into()),
                ("native_session", "second-main".into()),
            ]),
            ..Default::default()
        },
        &flag,
    )
    .unwrap();
    let _ = main2;
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"same-prompt-1","name":"invoke_subagent","args":{"TypeName":"worker","Role":"Shared Prompt","Prompt":"shared main prompt"}}})
        ),
        json!({"decision":"allow"})
    );
    let second_pre = invoke(
        "preinvocation",
        json!({"workspacePaths":[root],"conversationId":"second-main","invocationNum":1,"lastUserInput":"shared main prompt"}),
    );
    assert!(second_pre["injectSteps"].is_array(), "{second_pre}");
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"second-main","fullyIdle":true,"terminationReason":"GOAL"})
        ),
        json!({})
    );
    let (_, after_second_main) = load_projection(&root).unwrap();
    assert_eq!(
        after_second_main
            .iter()
            .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("same-prompt-1"))
            .unwrap()
            .get("state")
            .text(),
        Some("working")
    );
    fs::remove_file(&calls_path).unwrap();
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"main","fullyIdle":true})
        ),
        json!({})
    );
    let (_, recovered_stop) = load_projection(&root).unwrap();
    assert_eq!(
        recovered_stop
            .iter()
            .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("same-prompt-1"))
            .unwrap()
            .get("state")
            .text(),
        Some("done")
    );

    // Positive WaitMsBeforeAsync can time out and background a workflow; non-error PostToolUse keeps it working until Stop.
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"wf-wait","name":"run_workflow","args":{"Description":"Timed wait","WaitMsBeforeAsync":5000}}})
        ),
        json!({"decision":"allow"})
    );
    assert_eq!(
        invoke(
            "posttooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"wf-wait","name":"run_workflow","args":{"Description":"Timed wait","WaitMsBeforeAsync":5000}}})
        ),
        json!({})
    );
    let (_, after_wait_post) = load_projection(&root).unwrap();
    assert_eq!(
        after_wait_post
            .iter()
            .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("wf-wait"))
            .unwrap()
            .get("state")
            .text(),
        Some("working")
    );
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"main","fullyIdle":true})
        ),
        json!({})
    );
    let (_, after_wait_stop) = load_projection(&root).unwrap();
    assert_eq!(
        after_wait_stop
            .iter()
            .find(|v| v.get("refs").get("agy_tool_call_id").text() == Some("wf-wait"))
            .unwrap()
            .get("state")
            .text(),
        Some("done")
    );

    // Distinct conversations reusing the same call_id stay isolated across PreToolUse, PostToolUse errors, and Stop.
    let main2_id = create_attempt(
        &root,
        &StartRequest {
            provider: "antigravity".into(),
            tool: "agy".into(),
            refs: obj([
                ("panel_role", "main".into()),
                ("panel_room_id", "room-test".into()),
                ("native_session", "conv-shared-2".into()),
            ]),
            ..Default::default()
        },
        &flag,
    )
    .unwrap();
    for (conv, desc) in [("main", "Main shared"), ("conv-shared-2", "Second shared")] {
        assert_eq!(
            invoke(
                "pretooluse",
                json!({"workspacePaths":[root],"conversationId":conv,"toolCall":{"id":"shared-id","name":"run_workflow","args":{"Description":desc}}})
            ),
            json!({"decision":"allow"})
        );
    }
    let (_, shared_created) = load_projection(&root).unwrap();
    let shared_rows: Vec<_> = shared_created
        .iter()
        .filter(|v| {
            v.get("refs").get("agy_tool_call_id").text() == Some("shared-id")
                && v.get("state").text() == Some("working")
        })
        .collect();
    assert_eq!(shared_rows.len(), 2, "{shared_rows:?}");
    assert!(
        shared_rows
            .iter()
            .any(|v| v.get("parent_attempt_id").text() == Some(main.as_str()))
    );
    assert!(
        shared_rows
            .iter()
            .any(|v| v.get("parent_attempt_id").text() == Some(main2_id.as_str()))
    );
    invoke(
        "posttooluse",
        json!({"workspacePaths":[root],"conversationId":"conv-shared-2","toolCall":{"id":"shared-id","name":"run_workflow","args":{}},"error":"boom"}),
    );
    let (_, after_second_err) = load_projection(&root).unwrap();
    assert_eq!(
        after_second_err
            .iter()
            .find(
                |v| v.get("refs").get("agy_tool_call_id").text() == Some("shared-id")
                    && v.get("parent_attempt_id").text() == Some(main2_id.as_str())
            )
            .unwrap()
            .get("state")
            .text(),
        Some("failed")
    );
    assert_eq!(
        after_second_err
            .iter()
            .find(
                |v| v.get("refs").get("agy_tool_call_id").text() == Some("shared-id")
                    && v.get("parent_attempt_id").text() == Some(main.as_str())
            )
            .unwrap()
            .get("state")
            .text(),
        Some("working")
    );
    assert_eq!(
        invoke(
            "stop",
            json!({"workspacePaths":[root],"conversationId":"main","fullyIdle":true})
        ),
        json!({})
    );
    let (_, after_shared_stop) = load_projection(&root).unwrap();
    assert_eq!(
        after_shared_stop
            .iter()
            .find(
                |v| v.get("refs").get("agy_tool_call_id").text() == Some("shared-id")
                    && v.get("parent_attempt_id").text() == Some(main.as_str())
            )
            .unwrap()
            .get("state")
            .text(),
        Some("done")
    );

    drop(fixture);
}

#[test]
fn agy_surface_probe_exercises_tool_hooks() {
    if let Some(root) = std::env::var_os("OMS_AGY_SURFACE_ROOT").map(PathBuf::from) {
        let out = call(
            "update",
            &["--probe-agy-surfaces"],
            &root,
            &NativeServices::default(),
        );
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        let text = String::from_utf8_lossy(&out.1);
        assert!(text.contains("agy-surfaces: verified (ok)"), "{text}");
        return;
    }
    let root = std::env::temp_dir().join(format!("oms-agy-probe-{}", std::process::id()));
    fs::create_dir_all(root.join("host-home")).unwrap();
    let _fixture = Fixture(root.clone());
    let fake_bin = root.join("fake-agy");
    let script = r#"#!/usr/bin/env bash
set -euo pipefail
cap_state="${HOME}/fake-agy-plugin"
case "${1:-}" in
  --version)
    echo "agy 0.1.0-test"
    exit 0
    ;;
  plugin)
    if [ "${2:-}" = "validate" ]; then
      if [ "${OMS_FAKE_AGY_REJECT_TOOL_SCHEMA:-0}" = "1" ] && grep -q "PreToolUse" "${3:?}/hooks.json"; then
        exit 1
      fi
      printf '{"valid":true,"components":{"hooks":2}}\n'
      exit 0
    elif [ "${2:-}" = "install" ]; then
      printf '%s' "${3:?}" > "$cap_state"
      exit 0
    fi
    exit 2
    ;;
esac
prompt=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    prompt="${1:-}"
  fi
  shift || true
done
plugin_dir="$(cat "$cap_state")"
ws="$PWD"
printf '%s\n' "$prompt" >> "${HOME}/prompts.log"
(
  cd "$plugin_dir"
  printf '{"conversationId":"probe-conv","workspacePaths":["%s"]}\n' "$ws" | bash ./probe-hook.sh preinvocation >/dev/null
  if printf '%s' "$prompt" | grep -q "manage_subagents" && [ "${OMS_FAKE_AGY_SKIP_TOOLS:-0}" != "1" ]; then
    if [ "${OMS_FAKE_AGY_OMIT_CALL_ID:-0}" = "1" ]; then
      printf '{"conversationId":"probe-conv","workspacePaths":["%s"],"toolCall":{"name":"manage_subagents","args":{"Action":"list"}}}\n' "$ws" | bash ./probe-hook.sh pretooluse >/dev/null
      printf '{"conversationId":"probe-conv","workspacePaths":["%s"],"toolCall":{"name":"manage_subagents","args":{"Action":"list"}}}\n' "$ws" | bash ./probe-hook.sh posttooluse >/dev/null
    else
      printf '{"conversationId":"probe-conv","workspacePaths":["%s"],"toolCall":{"id":"t1","name":"manage_subagents","args":{"Action":"list"}}}\n' "$ws" | bash ./probe-hook.sh pretooluse >/dev/null
      printf '{"conversationId":"probe-conv","workspacePaths":["%s"],"toolCall":{"id":"t1","name":"manage_subagents","args":{"Action":"list"}}}\n' "$ws" | bash ./probe-hook.sh posttooluse >/dev/null
    fi
  fi
  printf '{"conversationId":"probe-conv","workspacePaths":["%s"],"terminationReason":"GOAL"}\n' "$ws" | bash ./probe-hook.sh stop >/dev/null
)
printf 'ok\n'
"#;
    fs::write(&fake_bin, script).unwrap();
    fs::set_permissions(&fake_bin, fs::Permissions::from_mode(0o700)).unwrap();

    for (case, skip_tools, reject_schema, omit_id, expected) in [
        ("with-tools", "0", "0", "0", "tool_hooks_verified=yes"),
        ("without-tools", "1", "0", "0", "tool_hooks_verified=no"),
        (
            "tool-schema-rejected",
            "0",
            "1",
            "0",
            "tool_hooks_verified=no",
        ),
        ("missing-call-id", "0", "0", "1", "tool_hooks_verified=no"),
    ] {
        let cap = root.join(format!("cap-{case}"));
        let locks = root.join(format!("locks-{case}"));
        let home = root.join("host-home").join(case);
        fs::create_dir_all(&cap).unwrap();
        fs::create_dir_all(&home).unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agy_surface_probe_exercises_tool_hooks",
                "--nocapture",
            ])
            .env_remove("OMS_HARNESS_CHILD")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("XDG_STATE_HOME", &home)
            .env("XDG_CACHE_HOME", &home)
            .env("OMS_AGY_SURFACE_ROOT", &root)
            .env("OMS_AGY_BIN", &fake_bin)
            .env("OMS_CAPABILITY_DIR", &cap)
            .env("OMS_LOCK_DIR", &locks)
            .env("OMS_FAKE_AGY_SKIP_TOOLS", skip_tools)
            .env("OMS_FAKE_AGY_REJECT_TOOL_SCHEMA", reject_schema)
            .env("OMS_FAKE_AGY_OMIT_CALL_ID", omit_id)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let env_file = fs::read_to_string(cap.join("agy-surfaces.env")).unwrap();
        assert!(
            env_file.contains("verdict=verified\n") && env_file.contains("reason=ok\n"),
            "{case}:\n{env_file}"
        );
        assert!(
            env_file.contains(&format!("{expected}\n")),
            "{case}:\n{env_file}"
        );
    }
}

#[test]
fn doctor_adapter_conformance() {
    const FIXTURE: &str = "OMS_ADAPTER_CONFORMANCE_FIXTURE";
    let Some(root) = std::env::var_os(FIXTURE).map(PathBuf::from) else {
        let fixture = Fixture(
            std::env::temp_dir().join(format!("oms-adapter-conformance-{}", std::process::id())),
        );
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&fixture.0)
            .unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "doctor_adapter_conformance", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", fixture.0.join("home"))
            .env("XDG_CONFIG_HOME", fixture.0.join("config"))
            .env("XDG_DATA_HOME", fixture.0.join("data"))
            .env("XDG_STATE_HOME", fixture.0.join("state"))
            .env("TMPDIR", &fixture.0)
            .env(FIXTURE, &fixture.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    };
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::create_dir(root.join("home")).unwrap();
    let adapters = source.join("adapters");
    fs::create_dir(&adapters).unwrap();
    let log = adapters.join("calls");
    let host = NativeServices::default();
    let script = |name: &str, body: &str| {
        let path = adapters.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"${{0%/*}}/calls\"\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    let probe = |adapter: &Path, extra: &[&str]| {
        let mut args = vec![
            "--providers",
            "--conformance",
            adapter.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = call("doctor", &args, &source, &host);
        assert!(out.2.is_empty(), "{}", String::from_utf8_lossy(&out.2));
        let value: Value = serde_json::from_slice(&out.1).unwrap();
        (out.0, value)
    };
    let check = |value: &Value, name: &str| {
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap()
            .clone()
    };
    let good = script(
        "oms-agent-adapter-fixture",
        "case \"${1:-}\" in --version|--help) printf 'private probe detail\\n' >&2; exit 0 ;; *) exit 71 ;; esac",
    );
    let (code, result) = probe(
        Path::new("adapters/oms-agent-adapter-fixture"),
        &["--probe-bound", "08"],
    );
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["kind"], "oms-adapter-conformance");
    assert_eq!(result["mode"], "probe");
    assert_eq!(result["ok"], true);
    assert_eq!(result["adapter"], "fixture");
    assert_eq!(result["probe_bound_seconds"], 8);
    assert_eq!(check(&result, "version-probe")["status"], "ok");
    assert_eq!(check(&result, "help-probe")["status"], "ok");
    assert_eq!(check(&result, "read-call")["status"], "unknown");
    assert_eq!(check(&result, "isolation")["status"], "unknown");
    assert!(!result.to_string().contains("private probe detail"));
    assert_eq!(fs::read_to_string(&log).unwrap(), "--version\n--help\n");
    assert!(!source.join(".oms").exists());
    assert!(!root.join("config").exists());
    assert!(!root.join("data").exists());

    for name in [
        "Bad_Name".to_owned(),
        "oms-agent-adapter-.bad".to_owned(),
        "oms-agent-adapter-bad..id".to_owned(),
        format!("oms-agent-adapter-{}", "a".repeat(65)),
    ] {
        let adapter = script(&name, "exit 0");
        let before = fs::read(&log).unwrap();
        let (code, result) = probe(&adapter, &[]);
        assert_eq!(code, 1, "{result}");
        assert_eq!(check(&result, "name")["status"], "fail");
        assert_eq!(fs::read(&log).unwrap(), before, "invalid name executed");
    }
    let no_exec = script("oms-agent-adapter-disabled", "exit 0");
    fs::set_permissions(&no_exec, fs::Permissions::from_mode(0o600)).unwrap();
    let (code, result) = probe(&no_exec, &[]);
    assert_eq!(code, 1);
    assert_eq!(check(&result, "executable")["status"], "fail");
    let (code, result) = probe(&adapters.join("oms-agent-adapter-missing"), &[]);
    assert_eq!(code, 1);
    assert_eq!(check(&result, "executable")["status"], "fail");

    let failed = script(
        "oms-agent-adapter-failure",
        "case \"$1\" in --version) printf 'private failure detail\\n' >&2; exit 7 ;; --help) exit 0 ;; *) exit 71 ;; esac",
    );
    let (code, result) = probe(&failed, &[]);
    assert_eq!(code, 1);
    assert_eq!(check(&result, "version-probe")["exit"], 7);
    assert_eq!(check(&result, "version-probe")["reason"], "nonzero_exit");
    assert_eq!(check(&result, "help-probe")["status"], "ok");
    assert!(!result.to_string().contains("private failure detail"));

    for redirect in ["", " >&2"] {
        let flood = script(
            "oms-agent-adapter-overflow",
            &format!(
                "case \"$1\" in --version) exit 0 ;; --help) dd if=/dev/zero bs=65536 count=1{redirect} 2>/dev/null ;; *) exit 71 ;; esac"
            ),
        );
        let (code, result) = probe(&flood, &[]);
        assert_eq!(code, 1, "{result}");
        assert_eq!(check(&result, "help-probe")["reason"], "output_limit");
        assert!(result.to_string().len() < 4096);
    }
    let slow = script("oms-agent-adapter-slow", "exec sleep 30");
    let started = Instant::now();
    let (code, result) = probe(&slow, &["--probe-bound", "1"]);
    assert_eq!(code, 1, "{result}");
    assert_eq!(check(&result, "version-probe")["reason"], "deadline");
    assert!(started.elapsed() < Duration::from_secs(15));

    let interrupted = AtomicBool::new(false);
    let before_length = fs::metadata(&log).unwrap().len();
    let cancelled = std::thread::scope(|scope| {
        scope.spawn(|| {
            let until = Instant::now() + Duration::from_secs(3);
            while fs::metadata(&log).unwrap().len() == before_length && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            interrupted.store(true, std::sync::atomic::Ordering::Release);
        });
        run_with_services(
            "doctor",
            &[
                "--providers".into(),
                "--conformance".into(),
                slow.as_os_str().into(),
            ],
            &source,
            &source,
            &interrupted,
            &host,
        )
    });
    assert_eq!(
        cancelled.0,
        130,
        "{}",
        String::from_utf8_lossy(&cancelled.2)
    );
    let result: Value = serde_json::from_slice(&cancelled.1).unwrap();
    assert_eq!(check(&result, "version-probe")["reason"], "cancelled");
    assert_eq!(check(&result, "help-probe")["reason"], "not_started");

    let before = fs::read(&log).unwrap();
    for args in [
        vec!["--conformance", good.to_str().unwrap()],
        vec!["--providers", "--probe-bound", "1"],
        vec![
            "--providers",
            "--conformance",
            good.to_str().unwrap(),
            "--check",
        ],
        vec![
            "--providers",
            "--conformance",
            good.to_str().unwrap(),
            "--contract",
        ],
        vec![
            "--providers",
            "--conformance",
            good.to_str().unwrap(),
            "--table",
            "unused",
        ],
        vec![
            "--providers",
            "--conformance",
            good.to_str().unwrap(),
            "--run",
        ],
        vec![
            "--providers",
            "--conformance",
            good.to_str().unwrap(),
            "--probe-bound",
            "0",
        ],
        vec![
            "--providers",
            "--conformance",
            good.to_str().unwrap(),
            "--probe-bound",
            "999999999999999999999",
        ],
    ] {
        let out = call("doctor", &args, &source, &host);
        assert_eq!(out.0, 2, "{args:?}: {}", String::from_utf8_lossy(&out.2));
    }
    assert_eq!(fs::read(&log).unwrap(), before, "invalid options executed");

    // The established table/reference checker remains independent of probes.
    doctor_provider_capability_evidence(&source, &root, &host);
    assert_eq!(fs::read(&log).unwrap(), before);
    assert!(host.0.borrow().is_empty());
}

#[test]
fn agy_preinvocation_forwards_boolean_intake_intent_only() {
    use oms_command_operations::{StartRequest, create_attempt, load_projection, value::obj};

    #[derive(Default)]
    struct IntakeHost(RefCell<Vec<String>>, RefCell<Option<Vec<u8>>>);
    impl Services for IntakeHost {
        fn provider_hook(
            &self,
            name: &str,
            input: &[u8],
            _: &Path,
            _: &Path,
            _: bool,
            _: &AtomicBool,
        ) -> Output {
            self.0.borrow_mut().push(name.into());
            if name != "skill-router" {
                return (0, Vec::new(), Vec::new());
            }
            *self.1.borrow_mut() = Some(input.to_vec());
            let v: Value = serde_json::from_slice(input).expect("adapted json");
            assert_eq!(
                (&v["hook_event_name"], &v["prompt"]),
                (&json!("PreInvocation"), &json!(""))
            );
            let body = if v["intake_eligible"].as_bool() == Some(true) {
                b"[oms intake] reminder\n".to_vec()
            } else {
                Vec::new()
            };
            (0, body, Vec::new())
        }
    }

    if let Some(root) = std::env::var_os("OMS_AGY_INTAKE_FIXTURE").map(PathBuf::from) {
        let action = std::env::var("OMS_AGY_INTAKE_ACTION").unwrap();
        let host = IntakeHost::default();
        let output = call("agy-hook", &[&action], &root, &host);
        assert_eq!(output.0, 0);
        fs::write(root.join("last-hooks.txt"), host.0.borrow().join(",")).unwrap();
        if let Some(forwarded) = host.1.borrow().as_ref() {
            fs::write(root.join("forwarded.json"), forwarded).unwrap();
        }
        print!("{}", String::from_utf8(output.1).unwrap());
        return;
    }

    let root = std::env::temp_dir().join(format!("oms-agy-intake-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let _fixture = Fixture(root.clone());

    let invoke =
        |action: &str, payload: Value, extra: &[(&str, &str)]| -> (Value, Option<Value>, String) {
            let (input, fwd_path, hooks_path) = (
                root.join("input.json"),
                root.join("forwarded.json"),
                root.join("last-hooks.txt"),
            );
            let _ = fs::remove_file(&fwd_path);
            fs::write(&input, serde_json::to_vec(&payload).unwrap()).unwrap();
            let mut cmd = Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "agy_preinvocation_forwards_boolean_intake_intent_only",
                "--nocapture",
            ])
            .env_remove("OH_MY_SETTING_AGY_HOOKS")
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .env_remove("OMS_SKILL_ROUTER_OFF")
            .env_remove("OMS_PANEL_MAIN_ATTEMPT")
            .env_remove("OMS_ROOM_ID")
            .env("OMS_LOCK_DIR", root.join("locks"))
            .env("OMS_AGY_INTAKE_FIXTURE", &root)
            .env("OMS_AGY_INTAKE_ACTION", action)
            .stdin(fs::File::open(input).unwrap());
            cmd.envs(extra.iter().copied());
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8(out.stdout).unwrap();
            let resp = serde_json::Deserializer::from_str(&stdout[stdout.find('{').unwrap()..])
                .into_iter::<Value>()
                .next()
                .unwrap()
                .unwrap();
            let fwd = fs::read_to_string(&fwd_path).ok().map(|s| {
                if let Some(raw) = payload["lastUserInput"].as_str().filter(|u| !u.is_empty()) {
                    assert!(!s.contains(raw), "raw lastUserInput leaked: {s}");
                }
                serde_json::from_str(&s).unwrap()
            });
            (
                resp,
                fwd,
                fs::read_to_string(&hooks_path).unwrap_or_default(),
            )
        };
    let pre = |conv: &str, num: i64, input: Option<&str>| {
        let mut p = json!({"workspacePaths":[root],"conversationId":conv,"invocationNum":num,"modelName":"gemini-test"});
        if let Some(text) = input {
            p["lastUserInput"] = json!(text);
        }
        p
    };

    // Short real input emits [oms intake] without echoing or forwarding raw text.
    let (resp, fwd, _) = invoke("preinvocation", pre("conv-1", 1, Some("이어서")), &[]);
    let msg = resp["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap_or("");
    assert!(
        msg.contains("[oms intake]") && !msg.contains("이어서"),
        "{resp}"
    );
    assert_eq!(fwd.unwrap()["intake_eligible"], Value::Bool(true));

    // Absent, empty, automated, relay lastUserInput and invocationNum > 1 stay quiet.
    for quiet in [
        None,
        Some(""),
        Some("?"),
        Some("<task-notification>done</task-notification>"),
        Some("Task from Overall Main. Work within this task and report the result here. x"),
    ] {
        let (resp, fwd, _) = invoke("preinvocation", pre("conv-q", 1, quiet), &[]);
        assert_eq!(resp, json!({}), "{quiet:?}");
        assert!(fwd.unwrap()["intake_eligible"].is_null(), "{quiet:?}");
    }
    let (later, fwd, _) = invoke("preinvocation", pre("conv-1", 2, Some("이어서")), &[]);
    assert_eq!((later, fwd), (json!({}), None));

    // Nonzero OMS_HARNESS_DELEGATE_DEPTH suppresses [oms intake] while keeping AGY tracking/observation intact.
    fs::create_dir_all(root.join(".oms")).unwrap();
    let flag = AtomicBool::new(false);
    let main = create_attempt(
        &root,
        &StartRequest {
            provider: "antigravity".into(),
            tool: "agy".into(),
            refs: obj([
                ("panel_role", "main".into()),
                ("panel_room_id", "room-intake".into()),
                ("native_session", "main".into()),
            ]),
            ..Default::default()
        },
        &flag,
    )
    .unwrap();
    let depth = [("OMS_HARNESS_DELEGATE_DEPTH", "1")];
    assert_eq!(
        invoke(
            "pretooluse",
            json!({"workspacePaths":[root],"conversationId":"main","toolCall":{"id":"sub-1","name":"invoke_subagent","args":{"TypeName":"self","Role":"worker","Prompt":"inspect child scope"}}}),
            &depth,
        )
        .0,
        json!({"decision": "allow"})
    );
    let (_, rows) = load_projection(&root).unwrap();
    assert!(
        rows.iter()
            .any(|v| v.get("parent_attempt_id").text() == Some(main.as_str())
                && v.get("refs").get("agy_tool_call_id").text() == Some("sub-1")
                && v.get("state").text() == Some("working")),
        "{rows:?}"
    );
    let (resp, fwd, hooks) = invoke(
        "preinvocation",
        pre("conv-depth", 1, Some("이어서")),
        &depth,
    );
    assert_eq!(
        (resp, hooks.as_str()),
        (json!({}), "telemetry-hook,skill-router")
    );
    assert!(fwd.unwrap()["intake_eligible"].is_null());
    let (resp, _, hooks) = invoke(
        "stop",
        json!({"workspacePaths":[root],"conversationId":"conv-depth","executionNum":1}),
        &depth,
    );
    assert_eq!(
        (resp, hooks.as_str()),
        (json!({}), "turn-guard,telemetry-hook")
    );

    // OMS_HARNESS_CHILD=1 and correlated AGY child (is_child) stay completely quiet.
    assert_eq!(
        invoke(
            "preinvocation",
            pre("conv-child-env", 1, Some("이어서")),
            &[("OMS_HARNESS_CHILD", "1")]
        ),
        (json!({}), None, String::new())
    );
    assert_eq!(
        invoke(
            "preinvocation",
            pre("child-sub-1", 1, Some("inspect child scope")),
            &[]
        ),
        (json!({}), None, String::new())
    );
}
