//! The job reactor requires a single-threaded process, so this contract runner
//! deliberately uses no libtest worker threads or global environment mutation.
use oms_panel_core::services::ExecutionRequest;
use oms_panel_core::{
    CommandOutput, Error, NativeCommand, PeerExecutor, Result, Services, Store, Value,
};
use oms_panel_core::{history, jobs, results, review, routing, terminals, workspaces};
use serde_json::json;
use std::cell::RefCell;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);
fn output(value: Value) -> CommandOutput {
    CommandOutput {
        stdout: serde_json::to_vec(&value).unwrap(),
        cleanup_complete: true,
        output_complete: true,
        ..Default::default()
    }
}
#[derive(Default)]
struct Fake {
    catalog: RefCell<Option<Value>>,
    council_output: RefCell<Option<CommandOutput>>,
    council_finish: RefCell<Vec<OsString>>,
    attempts: RefCell<Vec<Value>>,
    room: RefCell<Value>,
    panes: RefCell<String>,
    sessions: RefCell<bool>,
    server: RefCell<String>,
    history: RefCell<Vec<Value>>,
    plans: RefCell<Vec<Vec<OsString>>>,
    fail_plan: RefCell<bool>,
    publication: RefCell<Vec<u8>>,
    publication_calls: std::cell::Cell<usize>,
    replace_artifacts_on_publication: std::cell::Cell<usize>,
    retirement_calls: std::cell::Cell<usize>,
    fail_retirement: std::cell::Cell<bool>,
    land_calls: std::cell::Cell<usize>,
}
impl PeerExecutor for Fake {
    fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
        let args = request
            .argv
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if args.first().is_some_and(|v| v == "git") {
            let mut command = Command::new(&request.argv[0]);
            command
                .env("GIT_AUTHOR_NAME", "Fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "Fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .args(&request.argv[1..])
                .current_dir(&request.cwd)
                .envs(request.environment.clone())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for name in &request.remove_environment {
                if !request.environment.iter().any(|(key, _)| key == name) {
                    command.env_remove(name);
                }
            }
            let mut child = command.spawn().map_err(Error::new)?;
            if let Some(bytes) = &request.stdin {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(bytes)
                    .map_err(Error::new)?;
            } else {
                drop(child.stdin.take());
            }
            let done = child.wait_with_output().map_err(Error::new)?;
            return Ok(CommandOutput {
                code: done.status.code().unwrap_or(1),
                stdout: done.stdout,
                stderr: done.stderr,
                cleanup_complete: true,
                output_complete: true,
                limited: false,
            });
        }
        if args.first().is_some_and(|v| v == "bash") {
            return Ok(output(Value::Null));
        }
        let verb = args.get(1).map(String::as_str).unwrap_or("");
        let text = match verb {
            "display-message" if args.len() == 4 => self.server.borrow().clone(),
            "display-message" => format!("$1\t{}\tsocket", std::process::id()),
            "show-options" => request.cwd.to_string_lossy().into_owned(),
            "list-panes" => self.panes.borrow().clone(),
            "has-session" if !*self.sessions.borrow() => {
                return Ok(CommandOutput {
                    code: 1,
                    stderr: b"can't find session".to_vec(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                });
            }
            "has-session" => String::new(),
            _ => return Err(Error::new("unexpected execution")),
        };
        Ok(CommandOutput {
            stdout: text.into_bytes(),
            cleanup_complete: true,
            output_complete: true,
            ..Default::default()
        })
    }
}
impl Services for Fake {
    fn accepted_writes(
        &self,
        repo: &Path,
        request: &results::AcceptRequest,
        record: &Value,
        _: &AtomicBool,
    ) -> Result<Option<Value>> {
        self.retirement_calls.set(self.retirement_calls.get() + 1);
        let plan: Value =
            serde_json::from_slice(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap())
                .unwrap();
        assert_eq!(plan["tasks"][&request.task_id]["panel_accept"], *record);
        let saved: Value = serde_json::from_slice(
            &std::fs::read(repo.join(format!(
                ".oms/panel/acceptances/{}-{}.json",
                request.task_id,
                record["patch_sha256"].as_str().unwrap()
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved["stage"], "recorded",
            "settlement must already be retryable"
        );
        let rows = results::index_rows(repo)?;
        assert!(
            rows.iter().any(|row| row["kind"] == "panel-accept"
                && row["patch_sha256"] == record["patch_sha256"])
        );
        if self.fail_retirement.get() {
            return Err(Error::new(
                "fixture custody writer interrupted after acceptance",
            ));
        }
        Ok(Some(json!({"status":"retired"})))
    }
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        _: &AtomicBool,
    ) -> Result<CommandOutput> {
        match command {
            NativeCommand::ProviderCatalog => {
                if let Some(catalog) = self.catalog.borrow().as_ref() {
                    return Ok(output(catalog.clone()));
                }
                let policy = routing::Policy::bundled()?;
                let mut rows = Vec::new();
                for provider in ["codex", "claude", "antigravity"] {
                    let models = policy
                        .tiers
                        .values()
                        .flatten()
                        .chain(policy.council.extra.iter())
                        .filter(|v| v.provider == provider)
                        .map(|v| v.model.clone())
                        .collect::<Vec<_>>();
                    rows.push(json!({"provider":provider,"installed":true,"models":models}));
                }
                Ok(output(rows.into()))
            }
            NativeCommand::Events => {
                if args.first().is_some_and(|v| v == "show") {
                    let id = args
                        .iter()
                        .position(|v| v == "--attempt")
                        .map(|i| args[i + 1].to_string_lossy())
                        .unwrap();
                    return Ok(output(
                        self.attempts
                            .borrow()
                            .iter()
                            .find(|row| row["attempt_id"].as_str() == Some(id.as_ref()))
                            .cloned()
                            .unwrap_or(Value::Null),
                    ));
                }
                if args.first().is_some_and(|v| v == "start")
                    && args
                        .windows(2)
                        .any(|v| v[0] == "--tool" && v[1] == "panel-council")
                {
                    if self.council_output.borrow().is_none() {
                        return Err(Error::new("fixture stopped before starting council"));
                    }
                    return Ok(CommandOutput {
                        stdout: b"att-council\n".to_vec(),
                        ..output(Value::Null)
                    });
                }
                if args.first().is_some_and(|v| v == "transition") {
                    if args
                        .windows(2)
                        .any(|v| v[0] == "--attempt" && v[1] == "att-council")
                    {
                        self.council_finish.replace(args.to_vec());
                        return Ok(output(Value::Null));
                    }
                    let value = |name: &str| {
                        args.iter()
                            .position(|v| v == name)
                            .map(|i| args[i + 1].to_string_lossy().into_owned())
                            .unwrap()
                    };
                    let id = value("--attempt");
                    let mut attempts = self.attempts.borrow_mut();
                    let row = attempts.iter_mut().find(|v| v["attempt_id"] == id).unwrap();
                    assert_eq!(row["state"], value("--expected-state"));
                    assert_eq!(
                        row["sequence"].as_u64().unwrap().to_string(),
                        value("--expected-seq")
                    );
                    row["state"] = value("--state").into();
                    row["terminal"] = true.into();
                }
                Ok(output(self.attempts.borrow().clone().into()))
            }
            NativeCommand::PeerAsk => self
                .council_output
                .borrow()
                .clone()
                .ok_or_else(|| Error::new("unexpected council execution")),
            NativeCommand::Room => Ok(output(self.room.borrow().clone())),
            NativeCommand::Land => {
                self.land_calls.set(self.land_calls.get() + 1);
                let at = args.iter().position(|v| v == "--panel-accept").unwrap();
                let fence: Value = serde_json::from_str(args[at + 1].to_str().unwrap()).unwrap();
                results::verify_accept_fence(repo, repo, &fence, self, &STOP)?;
                for key in ["owner", "main_attempt_id", "task_id", "base", "verify"] {
                    let mut stale = fence.clone();
                    stale[key] = "changed".into();
                    assert!(results::verify_accept_fence(repo, repo, &stale, self, &STOP).is_err());
                }
                let mut stale = fence.clone();
                stale["snapshot"]["review_gate"]["exit"] = 1.into();
                assert!(results::verify_accept_fence(repo, repo, &stale, self, &STOP).is_err());
                let artifact_index = Path::new(".oms/artifacts/index.jsonl");
                let raw = std::fs::read(repo.join(artifact_index)).unwrap();
                for (task, base, already_applied) in [
                    (&fence["task_id"], Value::Null, true),
                    (&json!(""), fence["base"].clone(), true),
                    (&json!(""), json!("0".repeat(40)), false),
                ] {
                    let mut with_landing = raw.clone();
                    with_landing.extend(
                        serde_json::to_vec(&json!({
                            "kind": "patch-land",
                            "task_id": task,
                            "patch_sha256": fence["snapshot"]["candidate"]["patch_sha256"],
                            "base": base,
                            "exit": 0
                        }))
                        .unwrap(),
                    );
                    with_landing.push(b'\n');
                    store_file(repo, artifact_index, &with_landing);
                    let result = results::verify_accept_fence(repo, repo, &fence, self, &STOP);
                    store_file(repo, artifact_index, &raw);
                    if already_applied {
                        assert!(result.unwrap_err().message.contains("already applied"));
                    } else {
                        result.unwrap();
                    }
                }

                let plan_path = Path::new(".oms/plan/tasks.json");
                let plan_raw = std::fs::read(repo.join(plan_path)).unwrap();
                let mut plan: Value = serde_json::from_slice(&plan_raw).unwrap();
                let task_id = fence["task_id"].as_str().unwrap();
                for state in ["review", "done"] {
                    plan["tasks"][task_id]["state"] = state.into();
                    store_file(repo, plan_path, &serde_json::to_vec(&plan).unwrap());
                    let mut historical = fence.clone();
                    historical["snapshot"]["task"]["state"] = state.into();
                    let err = results::verify_accept_fence(repo, repo, &historical, self, &STOP);
                    store_file(repo, plan_path, &plan_raw);
                    assert!(
                        err.unwrap_err().message.contains("claimed/running"),
                        "{state}"
                    );
                }
                let position = args.iter().position(|v| v == "--patch").unwrap();
                let status = Command::new("git")
                    .args([
                        OsString::from("apply"),
                        OsString::from("--binary"),
                        args[position + 1].clone(),
                    ])
                    .current_dir(repo)
                    .status()
                    .unwrap();
                assert!(status.success());
                let verify = args.iter().position(|arg| arg == "--verify").unwrap();
                let status = Command::new("sh")
                    .arg("-c")
                    .arg(&args[verify + 1])
                    .current_dir(repo)
                    .status()
                    .unwrap();
                if status.success() {
                    let mut rows = results::index_rows(repo)?;
                    let mut landing = json!({
                        "kind": "patch-land", "exit": 0, "base": fence["base"],
                        "task_id": "", "patch_sha256": fence["snapshot"]["candidate"]["patch_sha256"]
                    });
                    if let Some(manifest) = fence["snapshot"].get("composition") {
                        rows.push(json!({
                            "kind": "patch-admit",
                            "event_id": "integration-admit",
                            "exit": 0,
                            "verify_exit": 0,
                            "verification": "command",
                            "base": fence["base"],
                            "patch_sha256": manifest["patch_sha256"]
                        }));
                        landing["panel_composition"] = json!({
                                "manifest_sha256": fence["snapshot"]["candidate"]["artifact_sha256"],
                                "participant": fence["snapshot"]["task"]["claimed_by_participant"],
                                "tree": manifest["tree"],
                                "verify_sha256": oms_panel_core::digest(args[verify + 1].as_encoded_bytes()),
                                "admission_event_id": "integration-admit"
                        });
                    }
                    rows.push(landing);
                    index(repo, &rows);
                }
                Ok(CommandOutput {
                    code: status.code().unwrap_or(1),
                    ..output(Value::Null)
                })
            }
            NativeCommand::Plan => {
                self.plans.borrow_mut().push(args.to_vec());
                if *self.fail_plan.borrow() {
                    return Err(Error::new("injected plan refusal"));
                }
                let path = args
                    .iter()
                    .position(|v| v == "--file")
                    .map(|i| PathBuf::from(&args[i + 1]))
                    .unwrap_or_else(|| repo.join(".oms/plan/tasks.json"));
                let mut plan: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                let task = &mut plan["tasks"]["task-1"];
                if args[0] == "finish" {
                    task["state"] = "done".into();
                    let commit = args
                        .iter()
                        .position(|v| v == "--landed-commit")
                        .map(|i| args[i + 1].to_string_lossy().into_owned())
                        .unwrap();
                    task["landing"] = json!({"sha":commit,"receipt_sha256":oms_panel_core::digest(&self.publication.borrow())});
                } else {
                    task["state"] = "review".into();
                    task["review_lease_id"] = task["lease_id"].clone();
                    for key in ["artifact", "patch"] {
                        let name = format!("--{key}");
                        task[key] = args[args.iter().position(|v| v == &*name).unwrap() + 1]
                            .to_string_lossy()
                            .as_ref()
                            .into();
                    }
                }
                std::fs::write(path, serde_json::to_vec(&plan).unwrap()).unwrap();
                Ok(output(Value::Null))
            }
            _ => Err(Error::new("unexpected native command")),
        }
    }
    fn swap_main_host(
        &self,
        _: &Path,
        request: &oms_panel_core::release::MainSwapRequest,
        main: &Value,
        _: &AtomicBool,
    ) -> Result<Value> {
        let mut attempts = self.attempts.borrow_mut();
        let old = attempts
            .iter_mut()
            .find(|v| v["attempt_id"] == request.attempt_id)
            .unwrap();
        old["state"] = "abandoned".into();
        old["terminal"] = true.into();
        old["reason_code"] = "main_swapped".into();
        old["refs"]["panel_native_exit"] = "confirmed".into();
        old["refs"]["panel_native_exit_code"] = "0".into();
        let mut replacement = main.clone();
        replacement["attempt_id"] = "main-replacement".into();
        replacement["provider"] = request.provider.clone().into();
        replacement["refs"]["panel_owner"] = request.provider.clone().into();
        replacement["refs"]["panel_harness"] = request.harness.clone().into();
        replacement["refs"]["panel_model"] = request.model.clone().into();
        attempts.push(replacement);
        *self.history.borrow_mut() = vec![
            json!({"thread":"room-1","room_event":{"kind":"join","participant":"main-member","role":"main","provider":main["provider"]}}),
            json!({"thread":"room-1","room_event":{"kind":"main-swap","participant":"main-member","attempt":request.attempt_id,"provider":request.provider}}),
        ];
        Ok(
            json!({"provider":request.harness,"model":request.model,"room":"room-1","participant":"main-member"}),
        )
    }
    fn room_history(&self, _: &Path, _: &str, _: &AtomicBool) -> Result<Vec<Value>> {
        Ok(self.history.borrow().clone())
    }
    fn publication_receipt(
        &self,
        repo: &Path,
        _: &Path,
        _: &str,
        _: &str,
        _: &AtomicBool,
    ) -> Result<Vec<u8>> {
        let call = self.publication_calls.get() + 1;
        self.publication_calls.set(call);
        if call == self.replace_artifacts_on_publication.get() {
            let artifacts = repo.join(".oms/artifacts");
            let prev = repo.join(".oms/artifacts-prev");
            let _ = std::fs::remove_dir_all(&prev);
            std::fs::rename(&artifacts, &prev).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&artifacts)
                .unwrap();
        }
        Ok(self.publication.borrow().clone())
    }
    fn host_absent(&self, _: &Path, _: &Value) -> Result<bool> {
        Ok(true)
    }
    fn command_context(
        &self,
        command: NativeCommand,
        request: &ExecutionRequest,
        stop: &AtomicBool,
    ) -> Result<CommandOutput> {
        let result = self.command(command, &request.cwd, &request.argv, stop)?;
        if command == NativeCommand::Plan && request.argv[0] == "review" {
            let raw = request
                .environment
                .iter()
                .find(|(k, _)| k == "OMS_PANEL_ACCEPT_RECORD_JSON")
                .unwrap();
            let path = request.cwd.join(".oms/plan/tasks.json");
            let mut plan: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            plan["tasks"]["task-1"]["panel_accept"] =
                serde_json::from_str(&raw.1.to_string_lossy()).unwrap();
            std::fs::write(path, serde_json::to_vec(&plan).unwrap()).unwrap();
        }
        Ok(result)
    }
}
fn private(repo: &Path, kind: &str) -> terminals::PrivateStore {
    terminals::PrivateStore::open(repo, kind, false, true).unwrap()
}
fn write(store: &terminals::PrivateStore, ident: &str, name: &str, bytes: &[u8]) {
    store_file(&store.root, &PathBuf::from(ident).join(name), bytes);
}
fn store_file(root: &Path, path: &Path, bytes: &[u8]) {
    let store = Store::open(root).unwrap();
    store
        .lock_at(Path::new("lock"), Duration::from_secs(3))
        .unwrap()
        .write(path, bytes, false, 32 * 1024 * 1024)
        .unwrap();
}
fn row(store: &terminals::PrivateStore, ident: &str, value: &Value) {
    write(store, ident, "record", &serde_json::to_vec(value).unwrap());
}
fn repo(name: &str) -> PathBuf {
    let root = PathBuf::from(std::env::var_os("OMS_PANEL_CORE_FIXTURE").unwrap()).join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    root
}
fn proof() -> terminals::SessionProof {
    terminals::SessionProof {
        session: "$1".into(),
        server_pid: std::process::id(),
        server_birth: terminals::process(std::process::id()).unwrap().birth,
        socket_digest: oms_panel_core::digest(b"socket"),
    }
}

fn uuid_vectors() {
    let fake = Fake::default();
    let policy = routing::Policy::bundled().unwrap();
    let mut request = review::CouncilRequest {
        owner: "codex".into(),
        task_id: "python.org".into(),
        label: "council".into(),
        prompt: "Discuss the implementation".into(),
        thread: None,
        rounds: Some(1),
        workload: "routine".into(),
        lenses: None,
        until_agreement: false,
        max_rounds: None,
        allow_partial: true,
        dry_run: true,
        room: None,
        main_attempt: None,
        participant: None,
    };
    for (name, expected) in [
        ("python.org", "7af94e2b4dd950f09c9a8a48"),
        ("task-1", "e7a652431d6e5e528e3585db"),
    ] {
        request.task_id = name.into();
        let first = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
        assert_eq!(first["thread"], format!("panel-council-{expected}"));
        assert_eq!(
            first,
            review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap()
        );
    }
    for (length, expected) in [
        (1, "e1407479313656c09908bb02"),
        (39, "615eac0e03db567b8580916c"),
        (40, "c5c985c868755a42b6e32872"),
        (47, "60100e11180f59cb911fbdb3"),
        (48, "aa33c5229edc58a5b5afe2a1"),
        (49, "5f34ff08c93f5aca811f32e7"),
        (55, "216292ab33d555d99441e27c"),
        (56, "699537c5641d5db180100f64"),
        (64, "37a282a89bf958e8af1a344d"),
        (120, "ddd61f4eee2155c28b4fe9df"),
        (160, "da3099f7beac5e3d8bb613cc"),
    ] {
        request.task_id = "a".repeat(length);
        assert_eq!(
            review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap()["thread"],
            format!("panel-council-{expected}")
        );
    }
    request.task_id = "task-1".into();
    request.rounds = Some(0);
    request.lenses = Some(vec!["risk".into(), "scope".into()]);
    let first = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
    let second = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
    assert_ne!(first["thread"], second["thread"]);
    assert_eq!(
        first["thread"].as_str().unwrap().len(),
        "panel-council-".len() + 32
    );
    request.thread = Some("x".repeat(160));
    assert_eq!(
        review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap()["thread"]
            .as_str()
            .unwrap()
            .len(),
        159
    );
    request.thread = Some(String::new());
    assert!(review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).is_err());
}

fn council_catalog_shapes() {
    let fake = Fake::default();
    let policy = routing::Policy::bundled().unwrap();
    let mut request = review::CouncilRequest {
        owner: "codex".into(),
        task_id: "task-1".into(),
        label: "council".into(),
        prompt: "Review private repository changes".into(),
        thread: None,
        rounds: Some(1),
        workload: "routine".into(),
        lenses: None,
        until_agreement: false,
        max_rounds: None,
        allow_partial: false,
        dry_run: true,
        room: None,
        main_attempt: None,
        participant: None,
    };
    let catalog = json!({"providers":[
        {"provider":"pi", "present":true, "catalog_probe":"ok", "routable":[]},
        {"provider":"claude", "present":true, "catalog_probe":"unsupported", "catalog_models":[]},
        {"provider":"codex", "present":true, "catalog_models":["gpt-6.1-terra", "gpt-6.2-terra"], "catalog_probe":"failed", "catalog_age_seconds":86401, "snapshot_age_seconds":172801, "model_effort_scales":{"gpt-6.1-terra":["low"],"gpt-6.2-terra":["medium"]}},
        {"provider":"antigravity", "present":true, "routable":["gemini-4-carbon-high\tGemini Carbon High"]}
    ]});
    *fake.catalog.borrow_mut() = Some(catalog.clone());
    let plan = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
    assert_eq!(
        plan["argv"][1],
        "claude:effort=medium:model=claude-sonnet-5-5,codex:effort=medium:model=gpt-6.2-terra,antigravity:effort=high:model=gemini-4-carbon-high"
    );
    assert_eq!(plan["families"], 3);
    assert_eq!(plan["primary_calls"], 6);
    assert_eq!(plan["max_calls"], 12);
    assert_eq!(plan["executes"], false);
    assert_eq!(plan["skipped"][0]["provider"], "pi");
    assert_eq!(
        plan["skipped"][0]["reason"],
        "council requires private routes"
    );
    assert_eq!(plan["catalog_evidence"][0]["catalog_status"], "unsupported");
    assert_eq!(plan["catalog_evidence"][0]["model_selection"], "configured");
    assert_eq!(plan["catalog_evidence"][1]["reported_probe"], "failed");
    assert_eq!(plan["catalog_evidence"][1]["catalog_age_seconds"], 86401);
    assert_eq!(plan["catalog_evidence"][1]["probe_age_seconds"], 172801);
    assert_eq!(
        plan["catalog_evidence"][1]["effort_support"],
        "reported_supported"
    );

    let mut fixed = policy.clone();
    fixed.council.extra[0].model = "gemini-pro-orchestrator-argon".into();
    fixed.council.extra[0].effort.clear();
    let mut mixed = catalog.clone();
    mixed["providers"][3]["routable"] = json!([
        "gemini-4-carbon-high\tGemini Carbon High",
        "gemini-pro-orchestrator-argon\tOrchestrator Argon"
    ]);
    *fake.catalog.borrow_mut() = Some(mixed);
    let plan = review::council_plan(Path::new("."), &request, &fixed, &fake, &STOP).unwrap();
    assert_eq!(
        plan["argv"][1],
        "claude:effort=medium:model=claude-sonnet-5-5,codex:effort=medium:model=gpt-6.2-terra,antigravity:effort=auto:model=gemini-pro-orchestrator-argon"
    );
    assert_eq!(plan["seat_routes"][0]["effort"], "medium");
    assert_eq!(plan["seat_routes"][2]["effort"], "");
    assert_eq!(plan["catalog_evidence"][2]["effort_support"], "fixed");
    assert!(plan["catalog_evidence"][2]["requested_effort"].is_null());

    let mut expired = catalog.clone();
    expired["providers"][2]["model_effort_scales"]["gpt-6.2-terra"] = json!(["low"]);
    expired["providers"][2]["capability_evidence"] =
        json!({"schema":1,"source":"fixture","observed_at":1,"expires_at":2});
    *fake.catalog.borrow_mut() = Some(expired);
    let plan = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
    assert_eq!(plan["catalog_evidence"][1]["effort_support"], "unknown");

    let mut unsupported = catalog.clone();
    unsupported["providers"][2]["model_effort_scales"]["gpt-6.2-terra"] = json!(["low"]);
    *fake.catalog.borrow_mut() = Some(unsupported);
    request.dry_run = false;
    assert_eq!(
        review::council_execute(Path::new("."), &request, &policy, &fake, &STOP)
            .unwrap_err()
            .message,
        "council: codex seat unavailable: resolved model does not report support for council reasoning effort"
    );
    request.dry_run = true;
    request.allow_partial = true;
    let plan = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
    assert_eq!(
        plan["catalog_evidence"][1]["effort_support"],
        "reported_unsupported"
    );
    assert_eq!(plan["skipped"][1]["provider"], "codex");
    assert_eq!(plan["families"], 2);
    let mut missing_exact = catalog.clone();
    missing_exact["providers"][2]["catalog_models"] = json!(["gpt-6.0-terra"]);
    *fake.catalog.borrow_mut() = Some(missing_exact);
    let plan = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
    assert_eq!(plan["catalog_evidence"][1]["catalog_status"], "available");
    assert_eq!(plan["skipped"][1]["provider"], "codex");
    assert_eq!(
        plan["skipped"][1]["reason"],
        "resolved model absent from routable catalog"
    );
    for (models, status, reason) in [
        (Some(json!([])), "empty", "empty model catalog"),
        (None, "unavailable", "model catalog unavailable"),
    ] {
        let mut changed = catalog.clone();
        let row = changed["providers"][3].as_object_mut().unwrap();
        if let Some(models) = models {
            row.insert("routable".into(), models);
        } else {
            row.remove("routable");
        }
        *fake.catalog.borrow_mut() = Some(changed);
        let plan = review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).unwrap();
        assert_eq!(plan["catalog_evidence"][2]["catalog_status"], status);
        assert_eq!(plan["skipped"][1]["reason"], reason);
    }
    request.allow_partial = false;
    for bad in [
        json!([]),
        json!([42]),
        json!(["gemini-4-carbon-high\tlabel\textra"]),
    ] {
        let mut changed = catalog.clone();
        changed["providers"][3]["routable"] = bad;
        *fake.catalog.borrow_mut() = Some(changed);
        assert!(review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).is_err());
    }
    for (probe, models) in [("ok", json!([])), ("unsupported", json!([42]))] {
        let mut changed = catalog.clone();
        changed["providers"][1]["catalog_probe"] = probe.into();
        changed["providers"][1]["catalog_models"] = models;
        *fake.catalog.borrow_mut() = Some(changed);
        assert!(review::council_plan(Path::new("."), &request, &policy, &fake, &STOP).is_err());
    }
    *fake.catalog.borrow_mut() = Some(catalog);
    request.workload = "deep".into();
    let mut restricted = policy.clone();
    for seat in restricted
        .tiers
        .values_mut()
        .flatten()
        .chain(restricted.council.extra.iter_mut())
    {
        seat.public_only = true;
    }
    assert!(review::council_plan(Path::new("."), &request, &restricted, &fake, &STOP).is_err());

    request.workload = "routine".into();
    request.dry_run = false;
    request.room = Some("room-council".into());
    request.main_attempt = Some("att-main".into());
    request.participant = Some("att-main".into());
    *fake.attempts.borrow_mut() = vec![json!({
        "attempt_id":"att-main", "tool":"panel-main", "provider":"codex", "terminal":false,
        "refs":{"panel_room_id":"room-council"}
    })];
    let room = json!({
        "id":"room-council", "closed":false,
        "participants":[{"participant":"att-main", "provider":"codex", "role":"main", "joined":true}]
    });
    *fake.room.borrow_mut() = room.clone();
    let execute = |request: &review::CouncilRequest| {
        review::council_execute(Path::new("."), request, &policy, &fake, &STOP)
            .unwrap_err()
            .message
    };
    assert_eq!(execute(&request), "fixture stopped before starting council");
    request.participant = Some("att-foreign".into());
    assert_eq!(execute(&request), "council main room binding differs");
    request.participant = Some("att-main".into());
    for (key, value) in [
        ("id", json!("room-foreign")),
        ("successor", json!("room-next")),
        ("closed", json!(true)),
        (
            "participants",
            json!([room["participants"][0], room["participants"][0]]),
        ),
    ] {
        let mut invalid = room.clone();
        invalid[key] = value;
        *fake.room.borrow_mut() = invalid;
        assert_eq!(
            execute(&request),
            "council owner is not joined to an open room"
        );
    }

    *fake.room.borrow_mut() = room;
    let diagnostic = |provider: &str, round: &str, stage: &str| {
        format!("error: council seat {provider} round {round}: {stage} failed; reconcile before retrying\n").into_bytes()
    };
    let run = |result: CommandOutput| {
        fake.council_finish.borrow_mut().clear();
        fake.council_output.replace(Some(result));
        review::council_execute(Path::new("."), &request, &policy, &fake, &STOP)
    };
    let response = |stderr| CommandOutput {
        code: 2,
        stderr,
        cleanup_complete: true,
        output_complete: true,
        ..Default::default()
    };
    let finish = |state| {
        let args = fake.council_finish.borrow();
        assert!(args.windows(2).any(|v| v[0] == "--state" && v[1] == state));
        assert!(
            args.windows(2)
                .any(|v| v[0] == "--reason-code" && v[1] == "council_incomplete")
        );
    };
    for stage in ["artifact write", "artifact index write", "thread append"] {
        let mut stderr = diagnostic("claude", "1", stage);
        stderr.extend_from_slice(b"hint: run `oms peer-ask --help` for usage\n");
        let error = run(response(stderr)).unwrap_err();
        finish("failed");
        assert_eq!(error.code, 2);
        assert!(error.publication_uncertain);
        assert_eq!(
            error.message,
            format!("council seat claude round 1: {stage} failed")
        );
    }
    let exact = diagnostic("claude", "0", "thread append");
    let extra = [exact.as_slice(), b"unexpected text\n"].concat();
    for invalid in [
        response(extra),
        response(diagnostic("pi", "0", "thread append")),
        response(diagnostic("claude", "2", "thread append")),
        response(diagnostic("claude", "00", "thread append")),
        response(diagnostic("claude", "0", "foreign stage")),
        CommandOutput {
            stdout: b"incomplete\n".to_vec(),
            ..response(exact.clone())
        },
        CommandOutput {
            output_complete: false,
            ..response(exact.clone())
        },
        CommandOutput {
            limited: true,
            ..response(exact.clone())
        },
    ] {
        let error = run(invalid).unwrap_err();
        finish("failed");
        assert!(!error.publication_uncertain);
        assert_eq!(
            error.message,
            "native command failed or completion evidence is incomplete"
        );
    }
    let error = run(CommandOutput {
        code: 130,
        ..response(exact.clone())
    })
    .unwrap_err();
    finish("cancelled");
    assert!(error.publication_uncertain);
    assert_eq!(error.code, 2);
    let error = run(CommandOutput {
        cleanup_complete: false,
        ..response(exact)
    })
    .unwrap_err();
    assert!(fake.council_finish.borrow().is_empty());
    assert!(error.publication_uncertain);
    assert_eq!(
        error.message,
        "council cleanup unconfirmed; preserve live lifecycle and reconcile"
    );
}

fn forget_hosts() {
    let repo = repo("forget-hosts");
    let fake = Fake::default();
    *fake.server.borrow_mut() = format!("{}\tsocket", std::process::id());
    for (kind, record_kind) in [("terminals", "shell"), ("workspaces", "workspace")] {
        let store = private(&repo, kind);
        let mut item = store
            .allocate(
                "term",
                record_kind,
                &json!({"pane":"%8","session":"$8","session_proof":proof(),"origin_proof":proof()}),
            )
            .unwrap();
        let ident = item["id"].as_str().unwrap().to_owned();
        *fake.panes.borrow_mut() = "%8".into();
        *fake.sessions.borrow_mut() = true;
        let forget = || {
            if kind == "terminals" {
                terminals::forget_terminal(&repo, &ident, &fake, &STOP)
            } else {
                workspaces::forget_workspace(&repo, &ident, &fake, &STOP)
            }
        };
        assert!(forget().is_err());
        assert!(store.record(&ident).is_ok());
        *fake.panes.borrow_mut() = "%9".into();
        *fake.sessions.borrow_mut() = false;
        write(&store, &ident, "unexpected", b"keep");
        assert!(forget().is_err());
        assert!(store.root.join(&ident).join("record").exists());
        std::fs::remove_file(store.root.join(&ident).join("unexpected")).unwrap();
        let field = if kind == "terminals" {
            "pane"
        } else {
            "session"
        };
        let original = item[field].clone();
        item[field] = "malformed".into();
        row(&store, &ident, &item);
        assert!(forget().is_err());
        item[field] = original;
        row(&store, &ident, &item);
        assert_eq!(forget().unwrap()["forgotten"], true);
        assert!(!store.root.join(&ident).exists());
        assert!(forget().is_err());
    }
}
fn forget_generations() {
    let repo = repo("forget-jobs");
    let store = private(&repo, "jobs");
    let mut item = store
        .allocate("job", "job", &json!({"generation":1,"state":"running"}))
        .unwrap();
    let ident = item["id"].as_str().unwrap().to_owned();
    write(&store, &ident, "recipe", b"{}");
    write(&store, &ident, "output", b"retained");
    assert!(jobs::forget_job(&repo, &ident, 0).is_err());
    assert!(jobs::forget_job(&repo, &ident, 1).is_err());
    item["state"] = "exited".into();
    item["completion"] = "reaped-all-children".into();
    row(&store, &ident, &item);
    assert!(jobs::forget_job(&repo, &ident, 2).is_err());
    write(&store, &ident, "unexpected", b"keep");
    assert!(jobs::forget_job(&repo, &ident, 1).is_err());
    std::fs::remove_file(store.root.join(&ident).join("unexpected")).unwrap();
    assert!(history::forget_record(&repo, "jobs", &ident, |_| Ok(true), &["record"]).is_err());
    assert!(jobs::forget_job(&repo, "../invalid", 1).is_err());
    history::archive(&repo, "jobs", &ident, |_| Ok(true)).unwrap();
    assert_eq!(
        jobs::forget_job(&repo, &ident, 1).unwrap()["forgotten"],
        true
    );
}
fn wait(jobs: &mut jobs::Jobs, ident: &str) -> Value {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let row = jobs.poll(ident).unwrap();
        if row["state"] == "exited" {
            return row;
        }
        assert!(Instant::now() < until, "job completion deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn restart_generations() {
    let repo = repo("restart");
    if !cfg!(target_os = "linux") {
        assert!(jobs::Jobs::open(&repo).is_err());
        return;
    }
    let mut jobs = jobs::Jobs::open(&repo).unwrap();
    let started = jobs
        .run_job(
            vec!["/bin/sh".into(), "-c".into(), "printf fresh".into()],
            &repo,
            "fixture",
        )
        .unwrap();
    let ident = started["id"].as_str().unwrap();
    assert!(jobs.restart_job(ident, 1).is_err());
    wait(&mut jobs, ident);
    let store = private(&repo, "jobs");
    let old = store.record(ident).unwrap();
    assert!(jobs.restart_job(ident, 0).is_err());
    assert!(jobs.restart_job(ident, 2).is_err());
    let recipe = store.root.join(ident).join("recipe");
    let original = std::fs::read(&recipe).unwrap();
    for bytes in [
        Vec::new(),
        vec![b'x'; jobs::LOG_LIMIT],
        vec![b'x'; jobs::LOG_LIMIT + 1],
    ] {
        write(&store, ident, "output", &bytes);
        if bytes.len() > jobs::LOG_LIMIT {
            assert!(jobs.restart_job(ident, 1).is_err());
            continue;
        }
        let mut changed: Value = serde_json::from_slice(&original).unwrap();
        changed["argv"] = json!([repo.join("missing-executable")]);
        let changed = serde_json::to_vec(&changed).unwrap();
        write(&store, ident, "recipe", &changed);
        let mut adjusted = old.clone();
        adjusted["recipe_sha256"] = oms_panel_core::digest(&changed).into();
        row(&store, ident, &adjusted);
        let error = jobs.restart_job(ident, 1).unwrap_err();
        assert!(error.message.contains("launch failed"), "{error}");
        assert_eq!(
            std::fs::read(store.root.join(ident).join("output")).unwrap(),
            bytes
        );
        assert_eq!(store.record(ident).unwrap(), adjusted);
        write(&store, ident, "recipe", &original);
        row(&store, ident, &old);
    }
    write(&store, ident, "output", b"old log");
    let new = jobs.restart_job(ident, 1).unwrap();
    assert_eq!(new["generation"], 2);
    assert!(
        std::fs::read(store.root.join(ident).join("output"))
            .unwrap()
            .is_empty()
    );
    wait(&mut jobs, ident);
    assert_eq!(
        std::fs::read(store.root.join(ident).join("output")).unwrap(),
        b"fresh"
    );
    assert!(jobs.restart_job(ident, 1).is_err());
    jobs::forget_job(&repo, ident, 2).unwrap();
}

fn main_attempt() -> Value {
    json!({"attempt_id":"main-1","tool":"panel-main","provider":"codex","state":"working","terminal":false,
        "created_at":"2026-01-01T00:00:00Z","refs":{"panel_role":"main","panel_owner":"codex",
        "panel_room_participant":"main-member","panel_room_id":"room-1"}})
}
fn worker(id: &str, state: &str, second: u64) -> Value {
    json!({"attempt_id":id,"tool":"peer-delegate","provider":"codex","state":state,"terminal":state != "review",
        "sequence":1,"task_id":"task-1","parent_attempt_id":"main-1","created_at":format!("2026-01-01T00:00:{second:02}Z"),
        "refs":{"panel_role":"worker","panel_access":"write","panel_purpose":"implement",
            "panel_room_id":"room-1","panel_room_participant":id,"panel_lease_id":format!("lease_{}","1".repeat(32))}})
}
fn prepare(fake: &Fake, workers: &[Value]) {
    *fake.attempts.borrow_mut() = std::iter::once(main_attempt())
        .chain(workers.iter().cloned())
        .collect();
    *fake.room.borrow_mut() = json!({"closed":false,"participants":std::iter::once(json!({"participant":"main-member","role":"main","joined":true}))
        .chain(workers.iter().map(|v| json!({"participant":v["attempt_id"],"role":"worker","owns":["product"]}))).collect::<Vec<_>>()});
}
fn index(repo: &Path, rows: &[Value]) {
    let mut raw = Vec::new();
    for row in rows {
        raw.extend(serde_json::to_vec(row).unwrap());
        raw.push(b'\n');
    }
    store_file(repo, Path::new(".oms/artifacts/index.jsonl"), &raw);
}
fn patch(repo: &Path, id: &str, content: &[u8], exit: i32) -> Value {
    let path = format!(".oms/artifacts/delegate/{id}.patch");
    store_file(repo, Path::new(&path), content);
    let artifact = format!(".oms/artifacts/delegate/{id}.md");
    let report = b"## Output\nA bounded result\n## Exit\n0\n";
    store_file(repo, Path::new(&artifact), report);
    json!({"schema":1,"provider":"codex","operation_id":format!("operation-{id}"),"artifact_id":format!("artifact-{id}"),"ts":"2026-01-01T00:00:00Z","kind":"delegate","event_id":format!("event-{id}"),"attempt_id":id,"task_id":"task-1","exit":exit,
        "verify_exit":if exit == 0 { 0 } else { 1 },"patch":path,"patch_sha256":oms_panel_core::digest(content),
        "artifact":artifact,"artifact_sha256":oms_panel_core::digest(report),"plan_id":format!("plan_{}","2".repeat(32))})
}
fn finalize_request(repo: &Path) -> results::FinalizeRequest {
    store_file(repo, Path::new("summary.md"), b"Reviewed fixture result");
    results::FinalizeRequest {
        owner: "codex".into(),
        task_id: "task-1".into(),
        main_attempt_id: "main-1".into(),
        summary_file: "summary.md".into(),
        outcome: "completed".into(),
        verify: None,
        evidence: vec![],
        verify_timeout: Duration::from_secs(1),
        execution_repo: repo.into(),
        notify: false,
        landed_commit: None,
    }
}
fn plan(repo: &Path) {
    store_file(repo,Path::new(".oms/plan/tasks.json"),&serde_json::to_vec(&json!({"plan_id":format!("plan_{}","2".repeat(32)),"tasks":{"task-1":{
        "id":"task-1","state":"running","provider":"codex","claimed_by_participant":"main-member",
        "lease_id":format!("lease_{}","1".repeat(32)),"verify":"true","allowed_paths":["product"],"forbidden_paths":[]}}})).unwrap());
}
fn retry_projection() {
    let repo = repo("retry-projection");
    let fake = Fake::default();
    let old = worker("old", "failed", 1);
    let new = worker("new", "done", 2);
    prepare(&fake, &[old, new]);
    let first = patch(&repo, "old", b"", 1);
    let second = patch(&repo, "new", b"", 0);
    index(&repo, &[first.clone(), second.clone()]);
    let request = results::ResultsRequest {
        task_id: Some("task-1".into()),
        ..Default::default()
    };
    let projected = results::results_page(&repo, &request, &fake, &STOP).unwrap();
    assert_eq!(
        projected["rows"][0]["stages"]["implement"]["state"],
        "passed"
    );
    assert_eq!(projected["rows"][0]["stages"]["verify"]["state"], "passed");
    assert_eq!(
        projected["rows"][0]["calls"][1]["answer"],
        "A bounded result"
    );
    fake.room.borrow_mut()["participants"][2]["owns"] = json!(["other"]);
    assert_eq!(
        results::results_page(&repo, &request, &fake, &STOP).unwrap()["rows"][0]["stages"]["implement"]
            ["state"],
        "failed"
    );
    fake.attempts.borrow_mut()[2]["refs"]["panel_continues"] = "old".into();
    assert_eq!(
        results::results_page(&repo, &request, &fake, &STOP).unwrap()["rows"][0]["stages"]["implement"]
            ["state"],
        "passed"
    );
    let mut page = request.clone();
    page.limit = 0;
    assert!(results::results_page(&repo, &page, &fake, &STOP).is_err());
    page.limit = 1;
    page.task_id = Some("absent".into());
    assert_eq!(
        results::results_page(&repo, &page, &fake, &STOP).unwrap()["truncated"],
        true
    );
    page.task_id = Some("../invalid".into());
    assert!(results::results_page(&repo, &page, &fake, &STOP).is_err());
    let review = |run: &str, overall: i32| {
        json!({"kind":"review-outcome","task_id":"task-1","exit":if overall==0 {0}else{1},
        "review":{"run":run,"overall":overall,"gate_verify_exit":0}})
    };
    index(
        &repo,
        &[
            first.clone(),
            second.clone(),
            review("20260101T000000Z-10", 0),
            review("20260101T000000Z-9", 1),
        ],
    );
    assert_eq!(
        results::results_page(&repo, &request, &fake, &STOP).unwrap()["rows"][0]["stages"]["review"]
            ["state"],
        "passed"
    );
    // Pagination is applied before off-page results are loaded.
    let mut other = patch(&repo, "other", b"", 0);
    other["task_id"] = "task-2".into();
    other["ts"] = "2026-01-02T00:00:00Z".into();
    index(&repo, &[first, second, other]);
    let mut page = results::ResultsRequest {
        limit: 1,
        ..Default::default()
    };
    let head = results::results_page(&repo, &page, &fake, &STOP).unwrap();
    assert_eq!(head["rows"][0]["task_id"], "task-2");
    assert_eq!(head["truncated"], true);
    page.offset = 1;
    let tail = results::results_page(&repo, &page, &fake, &STOP).unwrap();
    assert_eq!(tail["rows"][0]["task_id"], "task-1");
    assert_eq!(tail["rows"].as_array().unwrap().len(), 1);
}
fn multiworker_finalize() {
    let repo = repo("multiworker");
    let fake = Fake::default();
    prepare(
        &fake,
        &[worker("old", "failed", 1), worker("new", "done", 2)],
    );
    let first = patch(&repo, "old", b"", 1);
    let second = patch(&repo, "new", b"", 0);
    index(&repo, &[first, second]);
    plan(&repo);
    let request = finalize_request(&repo);
    let result = results::finalize(&repo, &request, &fake, &STOP).unwrap();
    assert_eq!(result["superseded_attempts"][0]["attempt_id"], "old");
    assert_eq!(result["superseded_attempts"][0]["superseded_by"], "new");
    assert_eq!(
        results::outcomes(&repo, &["task-1".into()]).unwrap().len(),
        1
    );
    let store = oms_panel_core::Store::open(&repo).unwrap();
    let rows = results::index_rows(&repo).unwrap();
    let plan_value: Value =
        serde_json::from_slice(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let cache = oms_panel_core::cache::Cache::new(8, Duration::from_secs(60)).unwrap();
    let tasks = ["task-1".to_owned()];
    let first = results::outcomes_cached(&store, &plan_value, &rows, &tasks, Some((&cache, &repo)))
        .unwrap();
    let latest = rows
        .iter()
        .rev()
        .find(|v| v["kind"] == "panel-result")
        .unwrap();
    let artifact = repo.join(latest["artifact"].as_str().unwrap());
    let print = format!(
        "{}:{}",
        latest["artifact_sha256"].as_str().unwrap(),
        oms_panel_core::cache::stamp(&artifact)
    );
    assert!(
        cache.read("result:task-1", &print).is_some(),
        "entry cached by stamp"
    );
    let again = results::outcomes_cached(&store, &plan_value, &rows, &tasks, Some((&cache, &repo)))
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(oms_panel_core::cache::stamp(&repo.join("absent")), "absent");
    let mut changed: Value =
        serde_json::from_slice(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    changed["tasks"]["task-1"]["verify"] = "changed".into();
    store_file(
        &repo,
        Path::new(".oms/plan/tasks.json"),
        &serde_json::to_vec(&changed).unwrap(),
    );
    assert!(
        results::outcomes(&repo, &["task-1".into()])
            .unwrap()
            .is_empty()
    );
    fake.attempts.borrow_mut()[2]["parent_attempt_id"] = "foreign-main".into();
    assert!(results::finalize(&repo, &request, &fake, &STOP).is_err());
}
fn git(repo: &Path, args: &[&str], input: &[u8]) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let done = child.wait_with_output().unwrap();
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    String::from_utf8(done.stdout).unwrap().trim().into()
}
fn composed_acceptance() {
    for combined_passes in [true, false] {
        let repo = repo(if combined_passes {
            "composed-acceptance"
        } else {
            "composed-verifier-failure"
        });
        let fake = Fake::default();
        git(&repo, &["init", "--quiet"], b"");
        for name in ["product-a", "product-b"] {
            std::fs::write(repo.join(name), b"before\n").unwrap();
        }
        git(&repo, &["add", "product-a", "product-b"], b"");
        let tree = git(&repo, &["write-tree"], b"");
        let base = git(
            &repo,
            &["commit-tree", &tree, "-m", "composition base"],
            b"",
        );
        git(&repo, &["update-ref", "HEAD", &base], b"");
        std::fs::write(repo.join(".git/info/exclude"), b".oms/\nlock\nsummary.md\n").unwrap();
        let mut candidates = Vec::new();
        let mut reviews = Vec::new();
        for (id, name, after) in [
            ("worker-a", "product-a", "after-a"),
            ("worker-b", "product-b", "after-b"),
        ] {
            let body = format!(
                "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1 +1 @@\n-before\n+{after}\n"
            );
            let mut candidate = patch(&repo, id, body.as_bytes(), 0);
            candidate["base_sha"] = base.clone().into();
            reviews.push(json!({"kind":"review-outcome","task_id":"task-1","event_id":format!("review-{id}"),"exit":0,
                "review":{"candidate":{"patch_sha256":candidate["patch_sha256"]},"gate_verify_exit":0,"seats":[{"verdict":"pass"}]}}));
            candidates.push(candidate);
        }
        let mut rows = candidates.clone();
        rows.extend(reviews);
        index(&repo, &rows);
        plan(&repo);
        let verify = if combined_passes {
            "grep -qx after-a product-a && grep -qx after-b product-b"
        } else {
            "! (grep -qx after-a product-a && grep -qx after-b product-b)"
        };
        let plan_path = repo.join(".oms/plan/tasks.json");
        let mut task: Value = serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
        task["tasks"]["task-1"]["allowed_paths"] = json!(["product-a", "product-b"]);
        task["tasks"]["task-1"]["verify"] = verify.into();
        store_file(
            &repo,
            Path::new(".oms/plan/tasks.json"),
            &serde_json::to_vec(&task).unwrap(),
        );
        prepare(
            &fake,
            &[worker("worker-a", "done", 1), worker("worker-b", "done", 2)],
        );
        fake.room.borrow_mut()["participants"][1]["owns"] = json!(["product-a"]);
        fake.room.borrow_mut()["participants"][2]["owns"] = json!(["product-b"]);
        for worker in fake.attempts.borrow().iter().skip(1) {
            dispatch_envelopes(&repo, worker, true, false);
        }
        let mut request = results::AcceptRequest {
            owner: "codex".into(),
            task_id: "task-1".into(),
            main_attempt_id: "main-1".into(),
            message: "feat: integrate reviewed components".into(),
            verify: verify.into(),
            execution_repo: repo.clone(),
            expected_patch_sha256: None,
            allow_verifier_change: false,
            allow_floor_change: None,
        };
        let mut stale = rows.clone();
        stale[1]["base_sha"] = "0".repeat(40).into();
        index(&repo, &stale);
        assert!(
            !results::accept(&repo, &request, &fake, &STOP)
                .unwrap_err()
                .publication_uncertain
        );
        let mut overlap = rows.clone();
        overlap[1]["patch"] = candidates[0]["patch"].clone();
        overlap[1]["patch_sha256"] = candidates[0]["patch_sha256"].clone();
        fake.room.borrow_mut()["participants"][2]["owns"] = json!(["product-a"]);
        index(&repo, &overlap);
        assert!(
            results::accept(&repo, &request, &fake, &STOP)
                .unwrap_err()
                .message
                .contains("overlap")
        );
        fake.room.borrow_mut()["participants"][2]["owns"] = json!(["product-b"]);
        let mut failed_review = rows.clone();
        failed_review[3]["exit"] = 1.into();
        index(&repo, &failed_review);
        assert!(results::accept(&repo, &request, &fake, &STOP).is_err());
        index(&repo, &rows);
        assert_eq!(git(&repo, &["rev-parse", "HEAD"], b""), base);
        let joined = candidates
            .iter()
            .flat_map(|candidate| {
                std::fs::read(repo.join(candidate["patch"].as_str().unwrap())).unwrap()
            })
            .collect::<Vec<_>>();
        request.expected_patch_sha256 = Some(oms_panel_core::digest(&joined));
        *fake.fail_plan.borrow_mut() = true;
        let error = results::accept(&repo, &request, &fake, &STOP).unwrap_err();
        assert!(error.publication_uncertain, "{error}");
        let head = git(&repo, &["rev-parse", "HEAD"], b"");
        *fake.fail_plan.borrow_mut() = false;
        let plans = fake.plans.borrow().len();
        let landings = fake.land_calls.get();
        assert!(
            results::accept(&repo, &request, &fake, &STOP)
                .unwrap_err()
                .message
                .contains("requires recorded acceptance")
        );
        assert_eq!(fake.plans.borrow().len(), plans);
        assert_eq!(fake.land_calls.get(), landings);
        if !combined_passes {
            assert_eq!(fake.retirement_calls.get(), 0);
            assert_eq!(head, base, "failed integration verification cannot commit");
            git(&repo, &["add", "product-a", "product-b"], b"");
            assert!(
                results::recover_accept(&repo, &request, &fake, &STOP)
                    .unwrap_err()
                    .message
                    .contains("integration verifier")
            );
            let recovery_path = PathBuf::from(format!(
                ".oms/panel/acceptances/{}-{}.json",
                request.task_id,
                request.expected_patch_sha256.as_deref().unwrap(),
            ));
            let mut forged: Value =
                serde_json::from_slice(&std::fs::read(repo.join(&recovery_path)).unwrap()).unwrap();
            forged["integration_verified"] = true.into();
            store_file(&repo, &recovery_path, &serde_json::to_vec(&forged).unwrap());
            assert!(
                results::recover_accept(&repo, &request, &fake, &STOP)
                    .unwrap_err()
                    .message
                    .contains("lacks native integration verifier evidence")
            );
            assert_eq!(git(&repo, &["rev-parse", "HEAD"], b""), base);
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(&plan_path).unwrap()).unwrap()["tasks"]
                    ["task-1"]["state"],
                "running"
            );
            continue;
        }
        assert_ne!(head, base);
        fake.fail_retirement.set(true);
        let accepted = results::recover_accept(&repo, &request, &fake, &STOP).unwrap();
        assert_eq!(accepted["sha"], head);
        assert_eq!(accepted["resource_retirement"]["status"], "unknown");
        assert_eq!(fake.retirement_calls.get(), 1);
        let indexed_acceptance = std::fs::read(repo.join(".oms/artifacts/index.jsonl")).unwrap();
        fake.fail_retirement.set(false);
        let plans = fake.plans.borrow().len();
        let recovered = results::accept(&repo, &request, &fake, &STOP).unwrap();
        assert_eq!(recovered["settlement_only"], true);
        assert_eq!(recovered["resource_retirement"]["status"], "retired");
        assert_eq!(recovered["sha"], head);
        assert_eq!(fake.retirement_calls.get(), 2);
        assert_eq!(fake.plans.borrow().len(), plans);
        assert_eq!(fake.land_calls.get(), landings);
        assert_eq!(
            indexed_acceptance,
            std::fs::read(repo.join(".oms/artifacts/index.jsonl")).unwrap()
        );
        let accepted =
            serde_json::from_slice::<Value>(&std::fs::read(&plan_path).unwrap()).unwrap();
        let record = &accepted["tasks"]["task-1"]["panel_accept"];
        assert_eq!(record["schema"], 2);
        assert_eq!(record["integration_verify"]["exit"], 0);
        let manifest_path = repo.join(record["composition"]["path"].as_str().unwrap());
        let manifest_raw = std::fs::read(&manifest_path).unwrap();
        let manifest: Value = serde_json::from_slice(&manifest_raw).unwrap();
        assert_eq!(manifest["components"].as_array().unwrap().len(), 2);
        assert_eq!(
            manifest["tree"],
            git(&repo, &["rev-parse", "HEAD^{tree}"], b"")
        );
        let rows_after = results::index_rows(&repo).unwrap();
        assert_eq!(
            rows_after
                .iter()
                .filter(|row| row["kind"] == "panel-accept")
                .count(),
            3
        );
        // A kill between aggregate and component registration must not strand
        // the remaining worker behind the aggregate's idempotence check.
        let incomplete = rows_after
            .into_iter()
            .filter(|row| !(row["kind"] == "panel-accept" && row["attempt_id"] == "worker-b"))
            .collect::<Vec<_>>();
        index(&repo, &incomplete);
        let settlements = fake.retirement_calls.get();
        assert!(
            results::accept(&repo, &request, &fake, &STOP)
                .unwrap_err()
                .message
                .contains("incomplete candidate registration")
        );
        assert_eq!(fake.retirement_calls.get(), settlements);
        results::recover_accept(&repo, &request, &fake, &STOP).unwrap();
        assert_eq!(
            results::index_rows(&repo)
                .unwrap()
                .iter()
                .filter(|row| row["kind"] == "panel-accept" && row["attempt_id"] == "worker-b")
                .count(),
            1
        );
        let prior = std::fs::read(repo.join(".oms/artifacts/index.jsonl")).unwrap();
        results::recover_accept(&repo, &request, &fake, &STOP).unwrap();
        assert_eq!(
            prior,
            std::fs::read(repo.join(".oms/artifacts/index.jsonl")).unwrap()
        );
        std::fs::write(&manifest_path, b"changed manifest").unwrap();
        assert!(results::accept(&repo, &request, &fake, &STOP).is_err());
        assert!(results::recover_accept(&repo, &request, &fake, &STOP).is_err());
        std::fs::write(&manifest_path, &manifest_raw).unwrap();
        let mut finalize = finalize_request(&repo);
        finalize.outcome = "accepted".into();
        finalize.verify = Some(verify.into());
        finalize.landed_commit = Some(head.clone());
        finalize.evidence = vec![record["artifact"].as_str().unwrap().into()];
        *fake.publication.borrow_mut() =
            serde_json::to_vec(&json!({"push":{"base":base}})).unwrap();
        assert_eq!(
            results::finalize(&repo, &finalize, &fake, &STOP).unwrap()["outcome"],
            "accepted"
        );
    }
}
fn acceptance_recovery() {
    let repo = repo("acceptance-recovery");
    let fake = Fake::default();
    git(&repo, &["init", "--quiet"], b"");
    std::fs::write(repo.join("product"), b"before\n").unwrap();
    git(&repo, &["add", "product"], b"");
    let tree = git(&repo, &["write-tree"], b"");
    let base = git(&repo, &["commit-tree", &tree, "-m", "fixture"], b"");
    git(&repo, &["update-ref", "HEAD", &base], b"");
    // Panel evidence is private fixture state, excluded from the product tree.
    std::fs::write(repo.join(".git/info/exclude"), b".oms/\nlock\nsummary.md\n").unwrap();
    let body = b"diff --git a/product b/product\n--- a/product\n+++ b/product\n@@ -1 +1 @@\n-before\n+after\n";
    let candidate = patch(&repo, "call-new", body, 0);
    let review = json!({"kind":"review-outcome","task_id":"task-1","event_id":"review-1","exit":0,
        "review":{"candidate":{"patch_sha256":candidate["patch_sha256"]},"gate_verify_exit":0,"seats":[{"verdict":"pass"}]}});
    index(&repo, &[candidate.clone(), review]);
    plan(&repo);
    prepare(
        &fake,
        &[worker("old", "failed", 1), worker("call-new", "done", 2)],
    );
    *fake.fail_plan.borrow_mut() = true;
    let request = results::AcceptRequest {
        owner: "codex".into(),
        task_id: "task-1".into(),
        main_attempt_id: "main-1".into(),
        message: "feat: reviewed product".into(),
        verify: "true".into(),
        execution_repo: repo.clone(),
        expected_patch_sha256: candidate["patch_sha256"].as_str().map(str::to_owned),
        allow_verifier_change: false,
        allow_floor_change: None,
    };
    let mut restricted: Value =
        serde_json::from_slice(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    restricted["tasks"]["task-1"]["allowed_paths"] = json!([]);
    store_file(
        &repo,
        Path::new(".oms/plan/tasks.json"),
        &serde_json::to_vec(&restricted).unwrap(),
    );
    assert!(
        !results::accept(&repo, &request, &fake, &STOP)
            .unwrap_err()
            .publication_uncertain
    );
    dispatch_envelopes(&repo, &fake.attempts.borrow()[2], true, true);
    let failed = results::accept(&repo, &request, &fake, &STOP).unwrap_err();
    assert!(failed.publication_uncertain, "{failed}");
    let head = git(&repo, &["rev-parse", "HEAD"], b"");
    assert_ne!(head, base, "{failed}");
    *fake.fail_plan.borrow_mut() = false;
    assert!(
        results::accept(&repo, &request, &fake, &STOP)
            .unwrap_err()
            .message
            .contains("requires recorded acceptance")
    );
    assert_eq!(
        results::recover_accept(&repo, &request, &fake, &STOP).unwrap()["sha"],
        head
    );
    let plans = fake.plans.borrow().len();
    let landings = fake.land_calls.get();
    let recovered = results::accept(&repo, &request, &fake, &STOP).unwrap();
    assert_eq!(recovered["sha"], head);
    assert_eq!(recovered["settlement_only"], true);
    assert_eq!(fake.plans.borrow().len(), plans);
    assert_eq!(fake.land_calls.get(), landings);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"], b""), head);
    let mut wrong = request.clone();
    wrong.verify = "false".into();
    assert!(results::accept(&repo, &wrong, &fake, &STOP).is_err());
    assert!(results::recover_accept(&repo, &wrong, &fake, &STOP).is_err());
    wrong = request.clone();
    wrong.expected_patch_sha256 = Some("0".repeat(64));
    assert!(results::accept(&repo, &wrong, &fake, &STOP).is_err());
    wrong.expected_patch_sha256 = None;
    assert!(results::accept(&repo, &wrong, &fake, &STOP).is_err());
    assert!(results::recover_accept(&repo, &wrong, &fake, &STOP).is_err());
    let recovery_path = PathBuf::from(format!(
        ".oms/panel/acceptances/{}-{}.json",
        request.task_id,
        request.expected_patch_sha256.as_deref().unwrap()
    ));
    let saved = std::fs::read(repo.join(&recovery_path)).unwrap();
    let mut changed: Value = serde_json::from_slice(&saved).unwrap();
    changed["base"] = head.clone().into();
    store_file(
        &repo,
        &recovery_path,
        &serde_json::to_vec(&changed).unwrap(),
    );
    assert!(results::accept(&repo, &request, &fake, &STOP).is_err());
    store_file(&repo, &recovery_path, &saved);
    let index_path = Path::new(".oms/artifacts/index.jsonl");
    let indexed = std::fs::read(repo.join(index_path)).unwrap();
    let without_landing = results::index_rows(&repo)
        .unwrap()
        .into_iter()
        .filter(|row| row["kind"] != "patch-land")
        .collect::<Vec<_>>();
    index(&repo, &without_landing);
    assert!(
        results::accept(&repo, &request, &fake, &STOP)
            .unwrap_err()
            .message
            .contains("unique native landing evidence")
    );
    store_file(&repo, index_path, &indexed);
    assert_eq!(fake.plans.borrow().len(), plans);
    assert_eq!(fake.land_calls.get(), landings);
    let mut finalize = finalize_request(&repo);
    finalize.outcome = "accepted".into();
    finalize.verify = Some("true".into());
    finalize.evidence = vec![PathBuf::from(candidate["artifact"].as_str().unwrap())];
    finalize.landed_commit = Some(head.clone());
    *fake.publication.borrow_mut() = serde_json::to_vec(&json!({"push":{"base":base}})).unwrap();
    fake.publication_calls.set(0);
    fake.replace_artifacts_on_publication.set(2);
    assert!(results::finalize(&repo, &finalize, &fake, &STOP).is_err());
    let still_review: Value =
        serde_json::from_slice(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    assert_ne!(still_review["tasks"]["task-1"]["state"], "done");
    std::fs::remove_dir_all(repo.join(".oms/artifacts")).unwrap();
    std::fs::rename(
        repo.join(".oms/artifacts-prev"),
        repo.join(".oms/artifacts"),
    )
    .unwrap();
    fake.publication_calls.set(0);
    fake.replace_artifacts_on_publication.set(0);
    assert_eq!(
        results::finalize(&repo, &finalize, &fake, &STOP).unwrap()["outcome"],
        "accepted"
    );
    let mut raw = std::fs::read(repo.join(".oms/artifacts/index.jsonl")).unwrap();
    let mut admission = candidate.clone();
    admission["kind"] = "patch-admit".into();
    admission["event_id"] = "admit-1".into();
    raw.extend(serde_json::to_vec(&admission).unwrap());
    raw.push(b'\n');
    store_file(&repo, Path::new(".oms/artifacts/index.jsonl"), &raw);
    finalize.evidence.clear();
    let automatic = results::finalize(&repo, &finalize, &fake, &STOP).unwrap();
    assert_eq!(automatic["evidence"].as_array().unwrap().len(), 1);
    finalize.evidence = vec![PathBuf::from(candidate["artifact"].as_str().unwrap()); 9];
    assert!(results::finalize(&repo, &finalize, &fake, &STOP).is_err());
    finalize.evidence.truncate(8);
    assert_eq!(
        results::finalize(&repo, &finalize, &fake, &STOP).unwrap()["evidence"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    finalize.landed_commit = Some("0".into());
    assert!(results::finalize(&repo, &finalize, &fake, &STOP).is_err());
}

fn python_bytes(value: &Value) -> Vec<u8> {
    oms_json::canonical_json(&oms_json::parse_strict(&serde_json::to_vec(value).unwrap()).unwrap())
        .unwrap()
}
fn dispatch_envelopes(repo: &Path, worker: &Value, valid: bool, rework: bool) {
    use std::os::unix::fs::MetadataExt;
    let member = worker["refs"]["panel_room_participant"].as_str().unwrap();
    let control = terminals::controller_store(repo, true).unwrap();
    let root = control.repo().join(member);
    let boot = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .to_owned()
    } else {
        "unknown".into()
    };
    let process = json!({"pid":std::process::id(),"identity":"fixture-generation"});
    let identity = json!({"schema":1,"repo":repo,"room":"room-1","participant":member,"parent":"main-member",
        "task_id":"task-1","nonce":"3".repeat(32),"boot_id":boot,"controller":process,"root":root});
    let call = json!({"schema":2,"kind":"oms-panel-call","room":"room-1","participant":member,"parent":"main-member",
        "task_id":"task-1","nonce":"3".repeat(32),"backend":"subreaper"});
    let body = python_bytes(&call);
    let public_path = format!(".oms/artifacts/panel-{member}.json");
    store_file(repo, Path::new(&public_path), &body);
    let info = std::fs::symlink_metadata(repo.join(public_path)).unwrap();
    let mut anchor = json!({"call":call,"generation":[info.dev(),info.ino(),info.size(),
        info.mtime() as i128*1_000_000_000+info.mtime_nsec() as i128,info.ctime() as i128*1_000_000_000+info.ctime_nsec() as i128],"execution_repo":repo});
    if rework {
        let brief = format!(".oms/artifacts/panel-{member}-brief.md");
        store_file(repo, Path::new(&brief), b"Reviewed extra scope");
        anchor["rework"] = json!({"role":"worker","scope":["product"],"workload":"deep","brief":brief,"brief_sha256":oms_panel_core::digest(b"Reviewed extra scope")});
    }
    let receipt = json!({"launch_sha256":if valid { oms_panel_core::digest(&body) } else { "0".repeat(64) },"pid":std::process::id(),"all_reaped":true,"exit":0});
    store_file(
        control.repo(),
        &PathBuf::from(member).join("identity.json"),
        &serde_json::to_vec(&identity).unwrap(),
    );
    for (suffix, record) in [
        ("launch", anchor),
        ("supervisor", receipt),
        ("process", json!({"pid":std::process::id()})),
    ] {
        let envelope = json!({"schema":1,"repo":repo,"participant":member,"launch":identity,"boot_id":boot,"writer":process,"supervisor":null,"record":record});
        store_file(
            control.repo(),
            &PathBuf::from(member).join(format!("{member}-{suffix}.json")),
            &serde_json::to_vec(&envelope).unwrap(),
        );
    }
}
fn supervisor_settlement() {
    let repo = repo("supervisor-settlement");
    let fake = Fake::default();
    let worker = worker("call-current", "review", 1);
    prepare(&fake, std::slice::from_ref(&worker));
    let request = finalize_request(&repo);
    dispatch_envelopes(&repo, &worker, false, false);
    assert!(results::finalize(&repo, &request, &fake, &STOP).is_err());
    assert!(!repo.join(".oms/artifacts/panel-results").exists());
    dispatch_envelopes(&repo, &worker, true, false);
    let result = results::finalize(&repo, &request, &fake, &STOP).unwrap();
    assert_eq!(result["settlement"]["applied"], json!(["call-current"]));
    assert_eq!(fake.attempts.borrow()[1]["state"], "done");
}
fn swap_lineage() {
    let repo = repo("swap-lineage");
    let fake = Fake::default();
    let mut old_main = main_attempt();
    old_main["attempt_id"] = "main-old".into();
    old_main["provider"] = "claude".into();
    old_main["state"] = "abandoned".into();
    old_main["terminal"] = true.into();
    old_main["reason_code"] = "main_swapped".into();
    old_main["refs"]["panel_owner"] = "claude".into();
    old_main["refs"]["panel_native_exit"] = "confirmed".into();
    old_main["refs"]["panel_native_exit_code"] = "0".into();
    let mut worker = worker("call-old", "done", 1);
    worker["parent_attempt_id"] = "main-old".into();
    prepare(&fake, &[worker]);
    fake.attempts.borrow_mut().insert(0, old_main);
    let request = finalize_request(&repo);
    assert!(results::finalize_snapshot(&repo, &request, &fake, &STOP).is_err());
    *fake.history.borrow_mut() = vec![
        json!({"thread":"room-1","room_event":{"kind":"join","participant":"main-member","role":"main","provider":"claude"}}),
        json!({"thread":"room-1","room_event":{"kind":"main-swap","participant":"main-member","attempt":"main-old","provider":"codex"}}),
    ];
    assert!(results::finalize_snapshot(&repo, &request, &fake, &STOP).is_ok());
    fake.history.borrow_mut()[1]["room_event"]["attempt"] = "foreign-attempt".into();
    assert!(results::finalize_snapshot(&repo, &request, &fake, &STOP).is_err());
}
fn adapter_environment() {
    let fake = Fake::default();
    assert!(fake.patch_encoding_warnings(b"").unwrap().is_empty());
    assert!(fake.patch_encoding_warnings(b"ASCII").unwrap().is_empty());
    assert!(fake.patch_encoding_warnings("한글".as_bytes()).is_err());
    let service = oms_panel_core::SubcommandServices::at(PathBuf::from("/bin/false")).unwrap();
    let mut request = ExecutionRequest::new(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf %s \"$OMS_PANEL_TEST_VALUE\"".into(),
        ],
        Path::new("."),
    );
    request
        .environment
        .push(("OMS_PANEL_TEST_VALUE".into(), "explicit".into()));
    request
        .remove_environment
        .push("OMS_PANEL_TEST_VALUE".into());
    let result = service.execute(&request, &STOP).unwrap();
    result.success().unwrap();
    assert_eq!(result.stdout, b"explicit");
}

fn salvage_missing_artifact() {
    let repo = repo("salvage-missing");
    let fake = Fake::default();
    git(&repo, &["init", "--quiet"], b"");
    let empty = git(&repo, &["mktree"], b"");
    let base = git(&repo, &["commit-tree", &empty, "-m", "fixture"], b"");
    git(&repo, &["update-ref", "HEAD", &base], b"");
    let root = PathBuf::from(std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT").unwrap());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let relative = "oh-my-setting-delegate.fixture/wt-fixture";
    let mut worker = worker("call-salvage", "failed", 1);
    worker["refs"]["panel_location"] = format!("worktree:{relative}").into();
    prepare(&fake, &[worker.clone()]);
    dispatch_envelopes(&repo, &worker, true, false);
    let mut candidate = patch(&repo,"call-salvage",b"diff --git a/product b/product\nnew file mode 100644\n--- /dev/null\n+++ b/product\n@@ -0,0 +1 @@\n+recovered\n",1);
    candidate["base_sha"] = base.clone().into();
    index(&repo, &[candidate.clone()]);
    let mut request = oms_panel_core::salvage::SalvageRequest {
        task_id: "task-1".into(),
        attempt_id: "call-salvage".into(),
        member: "call-salvage".into(),
        kept: root.join(relative),
        execution_repo: repo.clone(),
        base: base.clone(),
    };
    let result = oms_panel_core::salvage::salvage(&repo, &request, &fake, &STOP).unwrap();
    assert_eq!(result["salvaged_from"]["recovered_from_artifact"], true);
    assert_eq!(result["recovered_artifact"], candidate["patch"]);
    request.base = "0".repeat(40);
    assert!(oms_panel_core::salvage::salvage(&repo, &request, &fake, &STOP).is_err());
    request.base = base;
    request.kept = root.join("oh-my-setting-delegate.foreign/wt-foreign");
    assert!(oms_panel_core::salvage::salvage(&repo, &request, &fake, &STOP).is_err());
}

fn main_swap_workflow() {
    let repo = repo("main-swap");
    let fake = Fake::default();
    prepare(&fake, &[]);
    fake.attempts.borrow_mut()[0]["refs"]["panel_host"] = "tmux".into();
    fake.attempts.borrow_mut()[0]["refs"]["panel_harness"] = "codex".into();
    fake.attempts.borrow_mut()[0]["refs"]["panel_model"] = "previous-model".into();
    fake.room.borrow_mut()["participants"][0]["provider"] = "codex".into();
    let mut request = oms_panel_core::release::MainSwapRequest {
        attempt_id: "main-1".into(),
        provider: "claude".into(),
        model: String::new(),
        harness: "claude".into(),
    };
    assert!(oms_panel_core::release::swap_main(&repo, &request, &fake, &STOP).is_err());
    request.model = routing::Policy::bundled().unwrap().main["claude"]
        .model
        .clone();
    assert_eq!(
        oms_panel_core::release::swap_main(&repo, &request, &fake, &STOP).unwrap()["provider"],
        "claude"
    );
    assert!(oms_panel_core::release::swap_main(&repo, &request, &fake, &STOP).is_err());
}

fn live_reader_fence() {
    let repo = repo("live-reader");
    let fake = Fake::default();
    prepare(&fake, &[worker("call-done", "done", 1)]);
    let request = finalize_request(&repo);
    let entry = Path::new(".oms/locks/canonical-sync.d/review-fixture/pid");
    for body in [b"".as_slice(), b"0", b"malformed"] {
        store_file(&repo, entry, body);
        assert_eq!(
            results::finalize(&repo, &request, &fake, &STOP)
                .unwrap_err()
                .code,
            75
        );
        assert!(!repo.join(".oms/artifacts/panel-results").exists());
    }
    store_file(&repo, entry, std::process::id().to_string().as_bytes());
    assert_eq!(
        results::finalize(&repo, &request, &fake, &STOP)
            .unwrap_err()
            .code,
        75
    );
    std::fs::remove_file(repo.join(entry)).unwrap();
    std::fs::remove_dir(repo.join(entry).parent().unwrap()).unwrap();
    std::fs::remove_dir(repo.join(".oms/locks/canonical-sync.d")).unwrap();
    std::os::unix::fs::symlink("missing-target", repo.join(".oms/locks/canonical-sync.d")).unwrap();
    assert!(results::finalize(&repo, &request, &fake, &STOP).is_err());
}
fn empty_salvage_inventory() {
    let repo = repo("salvage-empty");
    let fake = Fake::default();
    git(&repo, &["init", "--quiet"], b"");
    let tree = git(&repo, &["mktree"], b"");
    let base = git(&repo, &["commit-tree", &tree, "-m", "fixture"], b"");
    git(&repo, &["update-ref", "HEAD", &base], b"");
    let root = PathBuf::from(std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT").unwrap());
    if !root.exists() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
    }
    let relative = "oh-my-setting-delegate.empty/wt-empty";
    let mut worker = worker("call-empty", "failed", 1);
    worker["refs"]["panel_location"] = format!("worktree:{relative}").into();
    prepare(&fake, &[worker.clone()]);
    dispatch_envelopes(&repo, &worker, true, false);
    let request = oms_panel_core::salvage::SalvageRequest {
        task_id: "task-1".into(),
        attempt_id: "call-empty".into(),
        member: "call-empty".into(),
        kept: root.join(relative),
        execution_repo: repo.clone(),
        base,
    };
    let result = oms_panel_core::salvage::salvage(&repo, &request, &fake, &STOP).unwrap();
    assert_eq!(result["outcome"], "nothing_to_recover");
    assert!(
        oms_panel_core::salvage::empty_provenance(
            &Store::open(&repo).unwrap(),
            "task-1",
            "call-empty"
        )
        .unwrap()
    );
    let candidate = patch(
        &repo,
        "call-empty",
        b"diff --git a/product b/product\n--- a/product\n+++ b/product\n@@ -1 +1 @@\n-old\n+new\n",
        1,
    );
    index(&repo, &[candidate]);
    assert!(oms_panel_core::salvage::salvage(&repo, &request, &fake, &STOP).is_err());
}

fn multi_provider_accounts() {
    assert_eq!(
        routing::model_family("x", "Orchestrator").unwrap(),
        "google"
    );
    assert_eq!(
        routing::model_family("x", "Gemini 3 Pro").unwrap(),
        "google"
    );
    assert_eq!(
        routing::model_family("x", "Claude Opus").unwrap(),
        "anthropic"
    );
    assert_eq!(routing::model_family("x", "GPT 6").unwrap(), "openai");
    assert!(routing::model_family("x", "mystery").is_err());

    let root = repo("accounts");
    let home = |name: &str| {
        let path = root.join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        path
    };
    let config = home("config");
    let (c1, c2, k1, p1) = (home("c1"), home("c2"), home("k1"), home("p1"));
    let file = config.join("provider-accounts.json");
    let write_config = |value: Value| {
        std::fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        routing::Accounts::load(&file)
    };
    let entry = |id: &str, dir: &Path| json!({"id": id, "home": dir});
    let good = json!({"schema":1,"providers":{
        "codex":{"accounts":[entry("a", &c1), entry("work", &c2)],"policy":"spread"},
        "claude":{"accounts":[entry("work", &k1)]},
        "pi":{"accounts":[entry("main", &p1)]}}});
    let accounts = write_config(good).unwrap().unwrap();
    assert_eq!(accounts.providers.len(), 3);

    let mut bad = json!({"schema":1,"providers":{"antigravity":{"accounts":[entry("a", &c1)]}}});
    let err = write_config(bad.clone()).unwrap_err();
    assert!(err.message.contains("isolation is unproved"));
    bad = json!({"schema":1,"providers":{
        "codex":{"accounts":[entry("a", &c1)]},"claude":{"accounts":[entry("b", &c1)]}}});
    assert!(
        write_config(bad)
            .unwrap_err()
            .message
            .contains("duplicate account home")
    );
    let dup =
        json!({"schema":1,"providers":{"pi":{"accounts":[entry("a", &p1), entry("a", &k1)]}}});
    assert!(write_config(dup).is_err());
    let empty = json!({"schema":1,"providers":{"pi":{"accounts":[]}}});
    assert!(write_config(empty).is_err());

    // Claude's private registry fills in when the shared file omits Claude.
    write_config(json!({"schema":1,"providers":{"codex":{"accounts":[entry("a", &c1)]}}})).unwrap();
    let sibling = config.join("provider-accounts.claude.json");
    let claude = json!({"schema":1,"provider":"claude","accounts":[entry("work", &k1)]});
    std::fs::write(&sibling, serde_json::to_vec(&claude).unwrap()).unwrap();
    let loaded = routing::Accounts::load(&file).unwrap().unwrap();
    let ids: Vec<_> = loaded.providers["claude"]
        .accounts
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert!(ids.contains(&"work"));
    let wrong = json!({"schema":1,"provider":"codex","accounts":[entry("work", &k1)]});
    std::fs::write(&sibling, serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(routing::Accounts::load(&file).is_err());
    std::fs::write(&sibling, serde_json::to_vec(&claude).unwrap()).unwrap();

    // Selection: lower bucketed use wins, exhausted accounts are skipped unless
    // explicit, running load spreads equal buckets, a requested account pins.
    let accounts = write_config(json!({"schema":1,"providers":{
        "codex":{"accounts":[entry("a", &c1), entry("work", &c2)],"policy":"spread"},
        "claude":{"accounts":[entry("work", &k1)]}}}))
    .unwrap()
    .unwrap();
    let now = oms_panel_core::timestamp();
    let reading = |used: u64| json!({"weekly_used": used, "observed_at": now});
    let none = std::collections::BTreeMap::new();
    let readings = json!({"a": reading(55), "work": reading(12)});
    let (picked, _) = accounts
        .select("codex", "m", &readings, &none, false, None)
        .unwrap();
    assert_eq!(picked.id, "work");
    let readings = json!({"a": reading(95), "work": reading(97)});
    assert!(
        accounts
            .select("codex", "m", &readings, &none, false, None)
            .is_err()
    );
    let (picked, reason) = accounts
        .select("codex", "m", &readings, &none, true, None)
        .unwrap();
    assert_eq!(
        (picked.id.as_str(), reason.as_str()),
        ("a", "explicit target")
    );
    let tied = json!({"a": reading(51), "work": reading(52)});
    let running = std::collections::BTreeMap::from([("codex:a".to_owned(), 2)]);
    assert_eq!(
        accounts
            .select("codex", "m", &tied, &running, false, None)
            .unwrap()
            .0
            .id,
        "work"
    );
    assert_eq!(
        accounts
            .select("codex", "m", &tied, &running, true, None)
            .unwrap()
            .0
            .id,
        "work"
    );
    let (picked, reason) = accounts
        .select("codex", "m", &tied, &running, false, Some("a"))
        .unwrap();
    assert_eq!(
        (picked.id.as_str(), reason.as_str()),
        ("a", "requested account")
    );
    assert!(
        accounts
            .select("codex", "m", &tied, &none, false, Some("nope"))
            .is_err()
    );
    assert!(
        accounts
            .select("pi", "m", &tied, &none, false, None)
            .is_err()
    );
    assert_eq!(
        accounts
            .select("claude", "m", &Value::Null, &none, false, None)
            .unwrap()
            .0
            .id,
        "work"
    );
    assert_eq!(
        accounts.home("claude", "work").unwrap(),
        k1.canonicalize().unwrap()
    );
    assert!(accounts.home("claude", "a").is_err());
    std::fs::set_permissions(&k1, std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
    assert!(
        accounts
            .select("claude", "m", &Value::Null, &none, false, None)
            .is_err()
    );
    std::fs::set_permissions(&k1, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();

    // Allocation pins only an explicitly requested account.
    let policy = routing::Policy::bundled().unwrap();
    let mut inputs = routing::RoutingInputs {
        installed: Some(["claude".to_owned()].into()),
        accounts: Some(accounts),
        explicit_target: true,
        preferred: Some("claude".into()),
        ..Default::default()
    };
    let route = routing::allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
    assert_eq!(
        (route.account.as_deref(), route.account_pinned),
        (Some("work"), false)
    );
    inputs.requested_account = Some("work".into());
    let route = routing::allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
    let snapshot = route.snapshot.as_ref().unwrap();
    assert_eq!(snapshot.selected_account.as_deref(), Some("work"));
    assert_eq!(snapshot.requested_account.as_deref(), Some("work"));
    assert!(snapshot.account_pinned);
    assert_eq!(snapshot.served_account, None);
    assert_eq!(snapshot.capabilities.account_alias, None);
    assert_eq!(snapshot.catalog.account_alias, None);
    assert_eq!(
        (route.account.as_deref(), route.account_pinned),
        (Some("work"), true)
    );
    inputs.requested_account = Some("other".into());
    assert!(routing::allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());

    // A requested account skips providers without a registry, and an exhausted
    // account stays usable when requested.
    inputs.requested_account = Some("work".into());
    inputs.explicit_target = false;
    inputs.preferred = None;
    inputs.installed = Some(["claude".to_owned(), "codex".to_owned(), "pi".to_owned()].into());
    inputs.status = json!({"claude": {"accounts": {"work": reading(97)}}});
    let route = routing::allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
    assert_eq!(route.candidate.provider, "claude");
    assert!(route.account_pinned);
    inputs.requested_account = Some("nope".into());
    assert!(routing::allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());

    // When only one native provider has a weekly reading (!all_known), recent room
    // dispatches still spread across native providers rather than starving Codex.
    let spread_base = routing::RoutingInputs {
        installed: Some(["claude".to_owned(), "codex".to_owned()].into()),
        status: json!({"codex": {"weekly_used": 17, "observed_at": now}}),
        ..Default::default()
    };
    let mut s = spread_base.clone();
    assert_eq!(
        routing::allocate(&policy, "antigravity", "builder", None, "write", None, &s)
            .unwrap()
            .candidate
            .provider,
        "claude"
    );
    s.recent.insert("claude".into(), 1);
    assert_eq!(
        routing::allocate(&policy, "antigravity", "builder", None, "write", None, &s)
            .unwrap()
            .candidate
            .provider,
        "codex"
    );
    s.recent.insert("codex".into(), 1);
    assert_eq!(
        routing::allocate(&policy, "antigravity", "builder", None, "write", None, &s)
            .unwrap()
            .candidate
            .provider,
        "claude"
    );

    let pick = |input: &routing::RoutingInputs| {
        routing::allocate(
            &policy,
            "antigravity",
            "builder",
            None,
            "write",
            None,
            input,
        )
        .unwrap()
        .candidate
        .provider
    };
    s.recent.insert("codex".into(), 3);
    s.in_flight.insert("claude".into(), 1);
    assert_eq!(
        pick(&s),
        "codex",
        "in-flight wins when quota evidence is incomplete"
    );
    s.status = json!({"codex":{"weekly_used":30}, "claude":{"weekly_used":40}});
    assert_eq!(
        pick(&s),
        "codex",
        "in-flight wins inside the weekly tie band"
    );
    s.status["claude"]["weekly_used"] = json!(5);
    assert_eq!(
        pick(&s),
        "claude",
        "a strong weekly gap wins over in-flight"
    );
    s.explicit_target = true;
    s.preferred = Some("codex".into());
    assert_eq!(pick(&s), "codex", "explicit target remains pinned");

    // A fresh Codex provider row of the active home stands in for that one
    // labelled account when its own newest log lacks a weekly window; a
    // mismatched, absent or stale label, or Claude's HUD row, is never lent.
    let lend_file = home("lend").join("provider-accounts.json");
    let snu = home("snu");
    let lend_config = |value: Value| {
        std::fs::write(&lend_file, serde_json::to_vec(&value).unwrap()).unwrap();
        routing::Accounts::load(&lend_file).unwrap().unwrap()
    };
    let lend = lend_config(json!({"schema":1,"providers":{
        "codex":{"policy":"ordered","accounts":[entry("snu", &snu)]}}}));
    assert!(lend.provider("claude").is_none());
    let day = 86_400;
    let codex_row = |used: u64, label: Option<&str>, observed: Option<u64>| {
        let mut row = json!({"weekly_used": used, "weekly_resets_at": now + day,
            "accounts": {"snu": {"weekly_used": null}}});
        if let Some(label) = label {
            row["account_label"] = label.into();
        }
        if let Some(observed) = observed {
            row["observed_at"] = observed.into();
        }
        row
    };
    let lend_inputs = routing::RoutingInputs {
        installed: Some(["claude".to_owned(), "codex".to_owned()].into()),
        accounts: Some(lend),
        ..Default::default()
    };
    let route_of = |owner: &str, codex: Value, inputs: &routing::RoutingInputs| {
        let mut inputs = inputs.clone();
        inputs.status = json!({"codex": codex, "claude": {"weekly_used": 85, "observed_at": now}});
        let route =
            routing::allocate(&policy, owner, "builder", None, "write", None, &inputs).unwrap();
        (route.candidate.provider, route.account)
    };
    let codex_snu = ("codex".to_owned(), Some("snu".to_owned()));
    let claude = ("claude".to_owned(), None);
    // Owner "codex" breaks unknown ties toward Claude, so 10% must be credited
    // to win; owner "claude" breaks them toward Codex, so 95% must be credited
    // to exclude it.
    let cases = [
        ("codex", codex_row(10, Some("snu"), Some(now))),
        ("codex", codex_row(10, None, Some(now))),
        ("codex", codex_row(10, Some("default"), Some(now))),
        ("codex", codex_row(10, Some("snu"), Some(now - 8 * day))),
        ("codex", codex_row(10, Some("snu"), None)),
        ("claude", codex_row(95, Some("snu"), Some(now))),
        ("claude", codex_row(95, None, Some(now))),
        ("claude", codex_row(95, Some("snu"), Some(now - 8 * day))),
        ("claude", codex_row(95, Some("snu"), None)),
    ];
    let got: Vec<_> = cases
        .iter()
        .map(|(owner, row)| route_of(owner, row.clone(), &lend_inputs))
        .collect();
    let want = [
        &codex_snu, &claude, &claude, &claude, &claude, &claude, &codex_snu, &codex_snu, &codex_snu,
    ];
    assert_eq!(got.iter().collect::<Vec<_>>(), want);
    // A requested account still pins snu at a credited 95%.
    let mut pinned = lend_inputs.clone();
    pinned.requested_account = Some("snu".into());
    assert_eq!(
        route_of("claude", codex_row(95, Some("snu"), Some(now)), &pinned),
        codex_snu
    );
    // Claude's provider row is never lent to a labelled Claude account.
    let kw = home("kw");
    let mut claude_lend = lend_inputs.clone();
    claude_lend.accounts = Some(lend_config(json!({"schema":1,"providers":{
        "claude":{"accounts":[entry("work", &kw)]}}})));
    claude_lend.status = json!({
        "codex": {"weekly_used": 50, "observed_at": now},
        "claude": {"weekly_used": 95, "observed_at": now, "weekly_resets_at": now + day,
            "account_label": "work", "accounts": {"work": {"weekly_used": null}}}});
    let route = routing::allocate(
        &policy,
        "codex",
        "builder",
        None,
        "write",
        None,
        &claude_lend,
    )
    .unwrap();
    assert_eq!(
        (route.candidate.provider.as_str(), route.account.as_deref()),
        ("claude", Some("work"))
    );

    // The default-home fallback needs a private directory; the child case sets
    // the home variable because this runner never mutates its own environment.
    let wide = home("wide");
    std::fs::set_permissions(&wide, std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
    let fake_home = repo("claude-default-home");
    for (mode, ok) in [(0o777, false), (0o700, true)] {
        std::fs::set_permissions(&wide, std::os::unix::fs::PermissionsExt::from_mode(mode))
            .unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("zz_fallback_child")
            .env("HOME", &fake_home)
            .env("CLAUDE_CONFIG_DIR", &wide)
            .env("OMS_PARITY_FALLBACK_OK", if ok { "1" } else { "0" })
            .status()
            .unwrap();
        assert!(status.success(), "fallback child mode {mode:o}");
    }
}

fn zz_fallback_child() {
    let Some(ok) = std::env::var_os("OMS_PARITY_FALLBACK_OK") else {
        return;
    };
    let dir = repo(&format!("fallback-child-{}", ok.to_string_lossy()));
    let blocked = dir.join("missing");
    let file = dir.join("provider-accounts.json");
    let body = json!({"schema":1,"providers":{"claude":{"accounts":[{"id":"w","home":blocked}]}}});
    std::fs::write(&file, serde_json::to_vec(&body).unwrap()).unwrap();
    let accounts = routing::Accounts::load(&file).unwrap().unwrap();
    let none = std::collections::BTreeMap::new();
    let picked = accounts.select("claude", "m", &Value::Null, &none, true, None);
    assert_eq!(picked.is_ok(), ok == "1", "{picked:?}");
    if let Ok((account, _)) = picked {
        assert_eq!(account.id, "default");
    }
    if ok == "1" {
        let claude_dir = repo("claude-implicit-default");
        let work = claude_dir.join("work");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .unwrap();
        let reg = claude_dir.join("provider-accounts.claude.json");
        std::fs::write(
            &reg,
            serde_json::to_vec(
                &json!({"schema":1,"provider":"claude","accounts":[{"id":"work","home":work}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let loaded = routing::Accounts::load(&claude_dir.join("provider-accounts.json"))
            .unwrap()
            .unwrap();
        assert!(loaded.home("claude", "default").is_err());
        assert!(
            accounts
                .select("claude", "m", &Value::Null, &none, false, Some("default"))
                .is_err()
        );
        let dot_claude = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".claude");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dot_claude)
            .unwrap();
        let (pinned_default, reason) = accounts
            .select("claude", "m", &Value::Null, &none, false, Some("default"))
            .unwrap();
        assert_eq!(pinned_default.home, dot_claude.canonicalize().unwrap());
        assert_eq!(reason, "requested account; default home");
        let loaded = routing::Accounts::load(&claude_dir.join("provider-accounts.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.home("claude", "default").unwrap(),
            dot_claude.canonicalize().unwrap()
        );
        let shared = claude_dir.join("provider-accounts.json");
        std::fs::write(
            &shared,
            serde_json::to_vec(
                &json!({"schema":1,"providers":{"codex":{"accounts":[{"id":"steal","home":dot_claude}]}}}),
            )
            .unwrap(),
        )
        .unwrap();
        let dup_home = routing::Accounts::load(&shared).unwrap_err();
        assert!(
            dup_home.message.contains("duplicate account home"),
            "{dup_home}"
        );
        let _ = std::fs::remove_file(&shared);
        std::fs::write(
            &reg,
            serde_json::to_vec(
                &json!({"schema":1,"provider":"claude","accounts":[{"id":"default","home":work}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let dup_id = routing::Accounts::load(&shared).unwrap_err();
        assert!(dup_id.message.contains("duplicate account id"), "{dup_id}");

        let alt_reg = dir.join("registered-default-bad.json");
        std::fs::write(
            &alt_reg,
            serde_json::to_vec(
                &json!({"schema":1,"providers":{"claude":{"accounts":[{"id":"default","home":blocked}]}}}),
            )
            .unwrap(),
        )
        .unwrap();
        let alt_loaded = routing::Accounts::load(&alt_reg).unwrap().unwrap();
        assert!(
            alt_loaded
                .select("claude", "m", &Value::Null, &none, false, Some("default"))
                .is_err()
        );
        assert!(
            alt_loaded
                .select("claude", "m", &Value::Null, &none, true, None)
                .is_err()
        );

        let stolen_reg = dir.join("stolen-default-home.json");
        std::fs::write(
            &stolen_reg,
            serde_json::to_vec(&json!({
                "schema": 1,
                "providers": {
                    "codex": {"accounts": [{"id": "steal", "home": dot_claude}]},
                    "claude": {"accounts": [{"id": "blocked", "home": blocked}]}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let stolen = routing::Accounts::load(&stolen_reg).unwrap().unwrap();
        assert!(
            stolen
                .select("claude", "m", &Value::Null, &none, false, Some("default"))
                .is_err()
        );

        let active_claude = std::env::var_os("CLAUDE_CONFIG_DIR").unwrap();
        let active_stolen_reg = dir.join("stolen-active-home.json");
        std::fs::write(
            &active_stolen_reg,
            serde_json::to_vec(&json!({
                "schema": 1,
                "providers": {
                    "codex": {"accounts": [{"id": "steal", "home": PathBuf::from(&active_claude)}]},
                    "claude": {"accounts": [{"id": "blocked", "home": blocked}]}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let active_stolen = routing::Accounts::load(&active_stolen_reg)
            .unwrap()
            .unwrap();
        assert!(
            active_stolen
                .select("claude", "m", &Value::Null, &none, true, None)
                .is_err()
        );

        let omitted_reg = dir.join("stolen-omitted-provider.json");
        std::fs::write(
            &omitted_reg,
            serde_json::to_vec(&json!({
                "schema": 1,
                "providers": {
                    "codex": {"accounts": [{"id": "steal", "home": PathBuf::from(&active_claude)}]}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let omitted = routing::Accounts::load(&omitted_reg).unwrap().unwrap();
        let policy = routing::Policy::bundled().unwrap();
        let inputs = routing::RoutingInputs {
            installed: Some(["claude".to_owned()].into()),
            accounts: Some(omitted),
            explicit_target: true,
            preferred: Some("claude".into()),
            ..Default::default()
        };
        assert!(
            routing::allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err()
        );
    }
}

fn main() {
    if std::env::var_os("OMS_PANEL_CORE_FIXTURE").is_none() {
        let root = std::env::temp_dir().join(format!(
            "oms-panel-contracts-{}-{}",
            std::process::id(),
            oms_panel_core::timestamp()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(std::env::args_os().skip(1))
            .env("OMS_PANEL_CORE_FIXTURE", &root)
            .env("XDG_STATE_HOME", root.join("state"))
            .env("TMPDIR", &root)
            .env("OMS_DELEGATE_WORKTREE_ROOT", root.join("worktrees"))
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .env_remove("OMS_ROOM_ID")
            .env_remove("OMS_ROOM_PARTICIPANT")
            .env_remove("OMS_ROOM_REPO")
            .env_remove("OMS_COUNCIL_ALLOW_PARTIAL")
            .status()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
    let filter = std::env::args()
        .skip(1)
        .find(|v| !v.starts_with('-'))
        .unwrap_or_default();
    let cases: &[(&str, fn())] = &[
        ("uuid_vectors", uuid_vectors),
        ("council_catalog_shapes", council_catalog_shapes),
        ("forget_hosts", forget_hosts),
        ("forget_generations", forget_generations),
        ("retry_projection", retry_projection),
        ("multiworker_finalize", multiworker_finalize),
        ("acceptance_recovery", acceptance_recovery),
        ("composed_acceptance", composed_acceptance),
        ("supervisor_settlement", supervisor_settlement),
        ("swap_lineage", swap_lineage),
        ("main_swap_workflow", main_swap_workflow),
        ("adapter_environment", adapter_environment),
        ("salvage_missing_artifact", salvage_missing_artifact),
        ("live_reader_fence", live_reader_fence),
        ("empty_salvage_inventory", empty_salvage_inventory),
        ("restart_generations", restart_generations),
        ("multi_provider_accounts", multi_provider_accounts),
        ("zz_fallback_child", zz_fallback_child),
    ];
    let mut count = 0;
    for (name, run) in cases {
        if !filter.is_empty() && !name.contains(&filter) {
            continue;
        }
        print!("test {name} ... ");
        run();
        println!("ok");
        count += 1;
    }
    assert!(count > 0, "no matching contract case");
    println!("test result: ok. {count} passed; 0 failed");
}
