// Included in the existing canonical account/transport fixture module so this
// exercises the same public caller, Plan, Room, account and lifecycle producers.
#[test]
fn canonical_native_main_succession_preserves_contract_and_account_anchor() {
    use oms_command_operations as events;
    use oms_command_operations::value::{V, obj};
    const STAGE: &str = "OMS_TEST_NATIVE_SUCCESSION_STAGE";
    const ROOT: &str = "OMS_TEST_NATIVE_SUCCESSION_ROOT";
    const SERVER: &str = "OMS_TEST_NATIVE_SUCCESSION_SERVER";
    const SETTLED: &str = "OMS_TEST_NATIVE_SUCCESSION_SETTLED";
    const TEST: &str = "orchestrator::tests::account_tests::canonical_native_main_succession_preserves_contract_and_account_anchor";
    if std::env::var_os(STAGE).is_none() {
        for settled in [false, true] {
            let tmp = TempDir::new("native-succession");
            let mut main = MAIN.to_owned();
            for stage in ["prepare", "successor", "second", "stale"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env_clear()
                    .env("HOME", &tmp.0)
                    .env("PATH", std::env::var_os("PATH").unwrap())
                    .env(STAGE, stage)
                    .env(ROOT, &tmp.0)
                    .env(SERVER, std::process::id().to_string())
                    .env(SETTLED, if settled { "1" } else { "0" })
                    .env("TMUX_PANE", "%1")
                    .env("OMS_ROOM_ID", ROOM)
                    .env("OMS_ROOM_PARTICIPANT", MAIN)
                    .env(
                        "OMS_PANEL_MAIN_ATTEMPT",
                        if stage == "stale" { MAIN } else { &main },
                    )
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{stage} settled={settled}:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let flow: Value =
                    serde_json::from_slice(&std::fs::read(tmp.0.join("flow.json")).unwrap())
                        .unwrap();
                main = text(&flow, "main").to_owned();
            }
        }
        return;
    }
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let repo = private_dir(&root.join("repo"));
    let stage = std::env::var(STAGE).unwrap();
    let settled = std::env::var(SETTLED).unwrap() == "1";
    let mut flow: Value = if stage == "prepare" {
        json!({})
    } else {
        serde_json::from_slice(&std::fs::read(root.join("flow.json")).unwrap()).unwrap()
    };
    let mut spy = SpyPanel::new(repo.clone());
    spy.server_pid = std::env::var(SERVER).unwrap().parse().unwrap();
    spy.allow_board_queries.set(true);
    spy.real_events.set(true);
    spy.canonical_assignment.set(true);
    if stage != "prepare" {
        spy.main_attempt = text(&flow, "main").into();
        spy.main_owner.set(if stage == "successor" {
            "claude"
        } else {
            "codex"
        });
        spy.ordinary_parent = Some(MAIN.into());
        spy.leads.borrow_mut().push(FakeLead {
            window: "@4".into(),
            pane_id: "%4".into(),
            pid: 2001,
            active: false,
            provider: "codex".into(),
        });
        spy.codex_chat_marks
            .borrow_mut()
            .insert("%4".into(), "1".into());
    }
    let mut host = FakeHost::new(private_dir(&root.join("state")));
    host.accounts = Some(make_accounts(vec![(
        "codex",
        vec![("acct-a", private_dir(&root.join("codex")))],
    )]));
    let uid = rustix::process::getuid().as_raw();
    let identity = terminals::process(spy.main_pid).unwrap();
    host.owners.insert(
        spy.main_pid,
        owner_entry(spy.main_pid, &identity.birth, uid),
    );
    host.owners.insert(2001, owner_entry(2001, "p2001", uid));
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let plan = |args: &[&str]| {
        let (code, out, err) = oms_command_plan::run(&commands::strings(args), &repo, &spy.stop);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        out
    };
    let cancel = || {
        plan(&[
            "block",
            "--id",
            "native-one",
            "--reason",
            "Fixture cancellation selected",
        ]);
        let raw = std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap();
        let stored = oms_command_plan::json::parse(&raw).unwrap();
        let task = stored.get("tasks").get("native-one");
        let task_sha = oms_command_plan::json::hash(task).unwrap();
        let decision = ".oms/artifacts/succession-cancellation.md";
        let decision_bytes = b"Canonical fixture explicitly cancels this leased task.\n";
        std::fs::write(repo.join(decision), decision_bytes).unwrap();
        std::fs::set_permissions(repo.join(decision), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        plan(&[
            "cancel",
            "--id",
            "native-one",
            "--operator-consent",
            "cancel",
            "--reason",
            "Fixture canonical cancellation",
            "--expected-plan-sha256",
            &oms_panel_core::digest(&raw),
            "--expected-task-sha256",
            &task_sha,
            "--expected-state",
            "blocked",
            "--expected-lease-id",
            task.text("lease_id"),
            "--decision-artifact",
            decision,
            "--expected-decision-artifact-sha256",
            &oms_panel_core::digest(decision_bytes),
        ]);
    };
    let transition = |id: &str, state: &str, then: Option<&str>, reason: &str| {
        let mut args = commands::strings(&[
            "transition",
            "--attempt",
            id,
            "--state",
            state,
            "--reason-code",
            reason,
        ]);
        if let Some(then) = then {
            args.extend(commands::strings(&["--then", then]));
        }
        let (code, _, err) = events::run(events::App::Events, &args, &repo, &spy.stop);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    };
    let annotate_main = |id: &str| {
        let refs = json!({"panel_room_participant":MAIN,"panel_owner":spy.main_owner.get(),
            "panel_harness":spy.main_owner.get(),"panel_role":"main","panel_native_scheduling":"unmanaged",
            "panel_wrapper_pid":identity.pid.to_string(),"panel_wrapper_identity":identity.birth,
            "panel_native_pid":identity.pid.to_string(),"panel_native_identity":identity.birth,
            "panel_native_exit":"confirmed","panel_native_exit_code":"0"});
        let event = events::new_event(
            id,
            0usize.into(),
            "attempt.usage",
            obj([
                (
                    "actor",
                    obj([
                        ("kind", "owner".into()),
                        ("name", "succession-fixture".into()),
                    ]),
                ),
                (
                    "refs",
                    V::parse(&serde_json::to_vec(&refs).unwrap()).unwrap(),
                ),
            ]),
        )
        .unwrap();
        events::append_lifecycle(&repo, event, &spy.stop).unwrap();
    };
    let invoke = |args: &[&str]| -> Result<Value> {
        let mut args = commands::strings(args);
        args.extend(commands::strings(&["--room", ROOM]));
        let output = run_with_host(
            &repo,
            "codex",
            &Options::parse(&args)?,
            &spy,
            &spy.stop,
            &host,
        )?;
        serde_json::from_slice(&output.1).map_err(Error::new)
    };
    let send = |task: &str, brief: &str, after: Option<&str>| {
        let mut args = vec![
            "--orchestrator",
            "codex",
            "--task-id",
            task,
            "--task",
            brief,
        ];
        if let Some(after) = after {
            args.extend(["--after-native-result", after]);
        }
        invoke(&args)
    };
    let input = root.join("manifest.json");
    let source_path = root.join("return.json");
    let write_return = |assignment: &oms_panel_core::native_contract::CheckedAssignment,
                        source: &Value,
                        prior: Option<&str>| {
        let bytes = serde_json::to_vec(source).unwrap();
        std::fs::write(&source_path, &bytes).unwrap();
        let patch = source["candidate_patch"].as_str();
        let mut manifest = json!({"schema":1,"attempt_id":assignment.attempt_id(),"binding_sha256":assignment.sha256(),
            "kind":if patch.is_some() {"candidate"} else {"findings"},"summary":source["summary"],"source":{"path":source_path,"sha256":oms_panel_core::digest(&bytes)}});
        if let Some(patch) = patch {
            let reference = crate::rows(&source["evidence"])
                .iter()
                .find(|row| row["path"] == patch)
                .unwrap();
            manifest["patch"] = json!({"path":reference["path"],"sha256":reference["sha256"]});
        }
        manifest["verification"] = crate::rows(&source["evidence"])
            .iter()
            .filter(|row| Some(text(row, "path")) != patch)
            .map(|row| json!({"path":row["path"],"sha256":row["sha256"]}))
            .collect::<Vec<_>>()
            .into();
        if let Some(prior) = prior {
            manifest["previous_sha256"] = prior.into();
        }
        std::fs::write(&input, serde_json::to_vec(&manifest).unwrap()).unwrap();
    };
    let import = |prior: Option<&str>| {
        let mut args = vec![
            "--orchestrator",
            "codex",
            "--task-id",
            "native-one",
            "--native-result",
            input.to_str().unwrap(),
        ];
        if let Some(prior) = prior {
            args.extend(["--expected-previous-result-sha256", prior]);
        }
        invoke(&args)
    };
    let swap = |next_owner: &str, next_model: &str| {
        annotate_main(&spy.main_attempt);
        transition(
            &spy.main_attempt,
            "blocked",
            Some("abandoned"),
            "main_swapped",
        );
        let local = crate::native_panel::native_lifecycle_contract::RealCliParsers;
        let room = oms_command_room::Room::open(
            &repo,
            oms_command_room::Session::from_env(),
            &local,
            &spy.stop,
            false,
            false,
        )
        .unwrap();
        room.swap_main(
            ROOM,
            MAIN,
            next_owner,
            next_model,
            &spy.main_attempt,
            next_owner,
        )
        .unwrap();
        let refs = json!({"panel_role":"main","panel_owner":next_owner,"panel_harness":next_owner,
            "panel_model":next_model,"panel_room_id":ROOM,"panel_room_participant":MAIN,"panel_native_scheduling":"unmanaged"});
        let id = events::create_attempt(
            &repo,
            &events::StartRequest {
                provider: next_owner.into(),
                tool: "panel-main".into(),
                refs: V::parse(&serde_json::to_vec(&refs).unwrap()).unwrap(),
                ..Default::default()
            },
            &spy.stop,
        )
        .unwrap();
        transition(&id, "starting", Some("working"), "native_main_started");
        id
    };
    if stage == "prepare" {
        git(&["init", "-q"]);
        std::fs::write(repo.join(".gitignore"), ".oms/\n").unwrap();
        git(&["add", ".gitignore"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        write_synthetic_ledger(&repo, 0);
        for args in [
            vec![
                "new",
                "--id",
                ROOM,
                "--participant",
                MAIN,
                "--topic",
                "Succession",
            ],
            vec![
                "join",
                "--id",
                ROOM,
                "--participant",
                MAIN,
                "--provider",
                "codex",
            ],
        ] {
            let out = oms_command_room::run(&commands::strings(&args), &repo, &spy.stop);
            assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        }
        annotate_main(MAIN);
        plan(&[
            "init",
            "--goal",
            "Preserve native work across Main succession",
        ]);
        for task in ["native-one", "native-two", "native-unsent"] {
            plan(&[
                "add",
                "--id",
                task,
                "--title",
                "Fixture",
                "--allowed",
                "src",
                "--verify",
                "true",
            ]);
        }
        spy.load()
            .execute_with_host("codex", Some("acct-a"), None, &host)
            .unwrap();
        let direct = spy.load();
        assert_eq!(direct.connected(), json!(["codex"]));
        assert_eq!(
            direct.chat_pane_with_host("codex", &host).unwrap().lead.id,
            "%4"
        );
        spy.screens
            .borrow_mut()
            .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
        let first = send("native-one", "inspect assigned source", None).unwrap();
        let packet = std::fs::read(repo.join(text(&first, "binding"))).unwrap();
        let assignment = oms_panel_core::native_contract::CheckedAssignment::parse(&packet)
            .unwrap()
            .unwrap();
        let mut source = assignment.report_template();
        source["summary"] = "Original Lead findings".into();
        write_return(&assignment, &source, None);
        flow["first"] = first;
        flow["packet_sha256"] = assignment.sha256().into();
        flow["original_claim"] = assignment.task().clone();
        if settled {
            let returned = import(None).unwrap();
            flow["result_sha256"] = returned["result"]["sha256"].clone();
            cancel();
            git(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "Advance after cancellation",
            ]);
        }
        // This process is the outgoing wrapper/native identity. The outer test
        // waits for its actual exit before invoking the fresh Main process.
        flow["main"] = swap("claude", "fixture-successor").into();
        std::fs::write(root.join("flow.json"), serde_json::to_vec(&flow).unwrap()).unwrap();
        return;
    }
    if stage == "stale" {
        let before = std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
        assert!(
            invoke(&[
                "--native-adopt",
                text(&flow["first"], "attempt_id"),
                "--assignment-sha256",
                text(&flow, "packet_sha256")
            ])
            .is_err()
        );
        assert_eq!(
            std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap(),
            before
        );
        assert!(spy.mutating_calls().is_empty());
        return;
    }
    annotate_main(&spy.main_attempt);
    let require_private_lead = || {
        spy.calls.borrow_mut().clear();
        let panel = spy.load();
        assert_eq!(panel.connected(), json!([]));
        assert!(panel.focus("codex").is_err());
        assert!(panel.chat_pane_with_host("codex", &host).is_err());
        let mut report = json!({});
        panel.observe_with_host(&mut Observations::default(), &mut report, false, &host);
        assert_eq!(report["provider_sessions"], json!([]));
        assert!(report["provider_models"].get("codex").is_none());
        assert!(
            panel
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .is_err(),
            "a generic reconnect cannot consume the retained Lead"
        );
        assert!(spy.calls_named("select-window").is_empty());
        assert!(spy.calls_named("select-pane").is_empty());
        assert!(spy.calls_named("new-window").is_empty());
        assert!(
            spy.calls_named("capture-pane")
                .iter()
                .all(|args| { !args.iter().any(|value| value == "%4") })
        );
    };
    require_private_lead();
    let ledger = repo.join(".oms/lifecycle/events.jsonl");
    let before = std::fs::read(&ledger).unwrap();
    let unadopted = send("native-two", "must wait for exact adoption", None).unwrap_err();
    assert!(
        unadopted
            .to_string()
            .contains("requires explicit native adoption")
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before);
    assert!(spy.mutating_calls().is_empty());
    let first = flow["first"].clone();
    let attempt = text(&first, "attempt_id");
    let raw = std::fs::read(repo.join(text(&first, "binding"))).unwrap();
    let assignment = oms_panel_core::native_contract::CheckedAssignment::parse(&raw)
        .unwrap()
        .unwrap();
    let operation = native_assignment::current_operation(&spy.load(), attempt).unwrap();
    let current =
        oms_panel_core::native_results::metadata(&Store::open(&repo).unwrap(), &operation).unwrap();
    let adopt = |sha: &str, result: Option<&str>| {
        let mut args = vec!["--native-adopt", attempt, "--assignment-sha256", sha];
        if let Some(result) = result {
            args.extend(["--result-sha256", result]);
        }
        invoke(&args)
    };
    let current_binding = text(&operation["refs"], "native_binding_sha256").to_owned();
    let current_result = current["artifact_sha256"].as_str().map(str::to_owned);
    let view =
        oms_panel_core::results::results(&repo, Some("native-one"), &spy, &spy.stop).unwrap();
    assert_eq!(
        view["rows"][0]["native_assignment"]["next_action"]["kind"],
        "adopt"
    );
    assert!(adopt(&"0".repeat(64), current_result.as_deref()).is_err());
    assert!(import(None).is_err());
    let history = AttemptCache::default()
        .attempts(&repo, &spy, &spy.stop)
        .unwrap();
    for (field, bad) in [
        ("panel_model", "foreign-model"),
        ("panel_harness", "codex"),
        ("panel_room_id", "foreign-room"),
    ] {
        let mut changed = crate::rows(&history).to_vec();
        changed
            .iter_mut()
            .find(|row| row["attempt_id"] == spy.main_attempt)
            .unwrap()["refs"][field] = bad.into();
        if field == "panel_harness" && spy.main_owner.get() == "codex" {
            continue;
        }
        assert!(
            oms_panel_core::native_succession::prove_successor(
                &repo,
                &changed,
                MAIN,
                MAIN,
                &spy.main_attempt,
                ROOM,
                MAIN,
                &spy,
                &spy.stop
            )
            .is_err()
        );
    }
    for failure in [
        "foreign-old-room",
        "live-old-process",
        "unknown-old-process",
        "dual-live-main",
    ] {
        let mut changed = crate::rows(&history).to_vec();
        let old = changed
            .iter_mut()
            .find(|row| row["attempt_id"] == MAIN)
            .unwrap();
        match failure {
            "foreign-old-room" => old["refs"]["panel_room_id"] = "foreign-room".into(),
            "live-old-process" => {
                old["refs"]["panel_native_pid"] = identity.pid.to_string().into();
                old["refs"]["panel_native_identity"] = identity.birth.clone().into();
            }
            "unknown-old-process" => {
                old["refs"]
                    .as_object_mut()
                    .unwrap()
                    .remove("panel_native_identity");
            }
            "dual-live-main" => {
                old["terminal"] = false.into();
                old["state"] = "working".into();
            }
            _ => unreachable!(),
        }
        assert!(
            oms_panel_core::native_succession::prove_successor(
                &repo,
                &changed,
                MAIN,
                MAIN,
                &spy.main_attempt,
                ROOM,
                MAIN,
                &spy,
                &spy.stop
            )
            .is_err(),
            "{failure}"
        );
    }
    assert!(
        oms_panel_core::native_succession::prove_successor(
            &repo,
            crate::rows(&history),
            MAIN,
            "att_ffffffffffffffffffffffffffffffff",
            &spy.main_attempt,
            ROOM,
            MAIN,
            &spy,
            &spy.stop
        )
        .is_err(),
        "a predecessor outside the ordered canonical chain must be refused"
    );
    if stage == "successor" {
        // The immutable write may succeed before the lifecycle reference. Replay
        // must retain one transition and never claim, reset a round, or send input.
        let mut args = commands::strings(&[
            "--native-adopt",
            attempt,
            "--assignment-sha256",
            &current_binding,
        ]);
        if let Some(result) = &current_result {
            args.extend(commands::strings(&["--result-sha256", result]));
        }
        let options = Options::parse(&args).unwrap();
        let before = std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
        {
            let panel = spy.load().with_native_codex_candidate().unwrap();
            let store = Store::open(&repo).unwrap();
            let tx = store
                .transaction("succession-fixture", Duration::from_secs(3))
                .unwrap();
            assert!(
                native_succession::adopt_before_append(&panel, &options, &host, &tx, || spy
                    .stop
                    .store(true, Ordering::Release))
                .is_err()
            );
        }
        spy.stop.store(false, Ordering::Release);
        assert_eq!(
            std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap(),
            before
        );
    }
    let adopted = adopt(&current_binding, current_result.as_deref()).unwrap();
    assert!(spy.mutating_calls().is_empty());
    require_private_lead();
    let retirement = settled || stage == "second";
    assert_eq!(
        adopted["mode"],
        if retirement {
            "retirement"
        } else {
            "continuation"
        }
    );
    if retirement {
        assert!(text(&adopted, "next_action").contains("retire the settled assignment"));
    }
    let after = std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
    assert_eq!(
        adopt(&current_binding, current_result.as_deref()).unwrap()["reused"],
        true
    );
    assert_eq!(
        std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap(),
        after
    );
    assert_eq!(
        std::fs::read(repo.join(text(&first, "binding"))).unwrap(),
        raw
    );
    assert!(spy.mutating_calls().is_empty());
    if stage == "second" {
        let op = native_assignment::current_operation(&spy.load(), attempt).unwrap();
        let retained =
            oms_panel_core::native_succession::metadata(&Store::open(&repo).unwrap(), &op)
                .unwrap()
                .unwrap();
        assert_eq!(retained["main_attempt_id"], spy.main_attempt);
        let record_path = repo.join(text(&op["refs"], "native_succession_path"));
        let record: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        let previous = record["previous"]["sha256"].as_str().unwrap();
        let prior_path = repo.join(format!(
            ".oms/lifecycle/attempts/{attempt}/native-succession-{previous}.json"
        ));
        let prior_bytes = std::fs::read(&prior_path).unwrap();
        std::fs::write(&prior_path, b"changed predecessor").unwrap();
        assert!(
            oms_panel_core::native_succession::metadata(&Store::open(&repo).unwrap(), &op).is_err()
        );
        std::fs::remove_file(&prior_path).unwrap();
        assert!(
            oms_panel_core::native_succession::metadata(&Store::open(&repo).unwrap(), &op).is_err()
        );
        std::fs::write(&prior_path, &prior_bytes).unwrap();
        std::fs::set_permissions(&prior_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        for key in [
            "native_succession_schema",
            "native_succession_path",
            "native_succession_sha256",
            "native_custodian_attempt",
        ] {
            let mut partial = op.clone();
            partial["refs"].as_object_mut().unwrap().remove(key);
            assert!(
                oms_panel_core::native_succession::metadata(&Store::open(&repo).unwrap(), &partial)
                    .is_err(),
                "{key}"
            );
        }
        let second = &flow["second"];
        let second_op =
            native_assignment::current_operation(&spy.load(), text(second, "attempt_id")).unwrap();
        invoke(&[
            "--native-adopt",
            text(second, "attempt_id"),
            "--assignment-sha256",
            text(&second_op["refs"], "native_binding_sha256"),
        ])
        .unwrap();
        let replay = send("native-two", "next task after retirement", None).unwrap();
        assert_eq!(replay["attempt_id"], second["attempt_id"]);
        assert_eq!(replay["reused"], true);
        assert!(spy.mutating_calls().is_empty());
        return;
    }
    let mut sha = current_result.unwrap_or_default();
    if !settled {
        let returned = import(None).unwrap();
        sha = text(&returned["result"], "sha256").to_owned();
        assert_eq!(import(None).unwrap()["reused"], true);
        let old_source = std::fs::read(&source_path).unwrap();
        let mut source: Value = serde_json::from_slice(&old_source).unwrap();
        source["summary"] = "Revised retained findings".into();
        source["revision"] = 2.into();
        source["previous_report_sha256"] = oms_panel_core::digest(&old_source).into();
        write_return(&assignment, &source, Some(&sha));
        assert!(import(None).is_err());
        sha = text(&import(Some(&sha)).unwrap()["result"], "sha256").to_owned();
        invoke(&[
            "--native-reconcile",
            attempt,
            "--result-sha256",
            &sha,
            "--decision",
            "request-revision",
        ])
        .unwrap();
        spy.screens
            .borrow_mut()
            .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
        let next = send("native-one", "address the reviewed findings", Some(&sha)).unwrap();
        assert_eq!(next["attempt_id"], attempt);
        assert_eq!(spy.calls_named("send-keys").len(), 1);
        let next_bytes = std::fs::read(repo.join(text(&next, "binding"))).unwrap();
        let next_assignment =
            oms_panel_core::native_contract::CheckedAssignment::parse(&next_bytes)
                .unwrap()
                .unwrap();
        assert_eq!(next_assignment.round(), 2);
        assert_eq!(next_assignment.lease_id(), assignment.lease_id());
        assert_eq!(
            next_assignment.packet()["custody_succession_sha256"],
            adopted["succession_sha256"]
        );
        let mut source = next_assignment.report_template();
        source["summary"] = "Round two candidate for parent verification".into();
        let patch=b"diff --git a/src/value.txt b/src/value.txt\nnew file mode 100644\n--- /dev/null\n+++ b/src/value.txt\n@@ -0,0 +1 @@\n+answer\n";
        let patch_name = ".oms/artifacts/successor-candidate.patch";
        std::fs::write(repo.join(patch_name), patch).unwrap();
        source["candidate_patch"] = patch_name.into();
        source["changed_paths"] = json!(["src/value.txt"]);
        source["evidence"] =
            json!([{"path":patch_name,"sha256":oms_panel_core::digest(patch),"bytes":patch.len()}]);
        write_return(&next_assignment, &source, None);
        sha = text(&import(None).unwrap()["result"], "sha256").to_owned();
        let reviewed = invoke(&[
            "--native-reconcile",
            attempt,
            "--result-sha256",
            &sha,
            "--decision",
            "reviewed",
        ])
        .unwrap();
        assert_eq!(reviewed["acceptance"], "pending");
        let task = native_assignment::task(&spy.load(), "native-one").unwrap();
        assert_eq!(task["state"], "review");
        assert_eq!(task["review_lease_id"], assignment.lease_id());
        assert_eq!(task["review_evidence"]["artifact"]["sha256"], sha);
        assert_eq!(
            task["review_evidence"]["patch"]["sha256"],
            oms_panel_core::digest(patch)
        );
        assert_eq!(
            std::fs::read(repo.join(text(&task, "patch"))).unwrap(),
            patch
        );
        let mut substituted = task["review_evidence"].clone();
        substituted["patch"]["sha256"] = "0".repeat(64).into();
        let ledger = repo.join(".oms/lifecycle/events.jsonl");
        let before = std::fs::read(&ledger).unwrap();
        spy.task_snapshot_override.replace(Some(json!({
            "id":"native-one","review_evidence":substituted
        })));
        let refusal = invoke(&[
            "--native-reconcile",
            attempt,
            "--result-sha256",
            &sha,
            "--decision",
            "reviewed",
        ])
        .unwrap_err();
        assert!(refusal.message.contains("exact native evidence binding"));
        spy.task_snapshot_override.replace(None);
        assert_eq!(std::fs::read(&ledger).unwrap(), before);
        assert_eq!(
            native_assignment::task(&spy.load(), "native-one").unwrap()["lease_epoch"],
            1
        );
        cancel();
    } else {
        assert!(send("native-one", "must not reopen cancelled work", None).is_err());
    }
    let decision = [
        "--native-reconcile",
        attempt,
        "--result-sha256",
        &sha,
        "--decision",
        "reviewed",
    ];
    spy.task_transition.replace(Some((
        "room-leave-recorded",
        Box::new(|spy| spy.stop.store(true, Ordering::Release)),
    )));
    assert!(invoke(&decision).is_err());
    spy.stop.store(false, Ordering::Release);
    assert_eq!(invoke(&decision).unwrap()["status"], "retired");
    assert_eq!(invoke(&decision).unwrap()["reused"], true);
    spy.calls.borrow_mut().clear();
    spy.screens
        .borrow_mut()
        .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
    if !settled {
        native_assignment::CRASH_AFTER_BINDING_WRITE.with(|crash| crash.set(true));
        let recorded = send("native-unsent", "preserve the successor claim", None).unwrap_err();
        assert!(recorded.message.contains("test crash"));
        let history = AttemptCache::default()
            .attempts(&repo, &spy, &spy.stop)
            .unwrap();
        let pending = crate::rows(&history)
            .iter()
            .find(|row| row["task_id"] == "native-unsent")
            .unwrap();
        assert_eq!(pending["parent_attempt_id"], spy.main_attempt);
        let task = native_assignment::task(&spy.load(), "native-unsent").unwrap();
        assert_eq!(task["claimed_by_participant"], MAIN);
        let id = text(pending, "attempt_id");
        let artifact = repo.join(format!(
            ".oms/lifecycle/attempts/{id}/native-assignment.json"
        ));
        std::fs::remove_file(&artifact).unwrap();
        let plan_file = repo.join(".oms/plan/tasks.json");
        let before = std::fs::read(&plan_file).unwrap();
        let retired = invoke(&[
            "--native-reconcile",
            id,
            "--intent-sha256",
            text(&pending["refs"], "native_intent_sha256"),
            "--decision",
            "retire-unsent",
        ])
        .unwrap();
        assert_eq!(retired["status"], "retired-unsent");
        assert_eq!(std::fs::read(&plan_file).unwrap(), before);
        assert!(!artifact.exists());
        assert!(spy.mutating_calls().is_empty());
    }
    let second = send("native-two", "next task after retirement", None).unwrap();
    let second_raw = std::fs::read(repo.join(text(&second, "binding"))).unwrap();
    let second_assignment = oms_panel_core::native_contract::CheckedAssignment::parse(&second_raw)
        .unwrap()
        .unwrap();
    assert_eq!(second_assignment.parent_attempt_id(), spy.main_attempt);
    assert_eq!(second_assignment.parent_participant(), MAIN);
    assert_eq!(second_assignment.owner(), "claude");
    assert_eq!(second_assignment.lease_epoch(), 1);
    assert_eq!(
        second_assignment.packet()["account"],
        assignment.packet()["account"]
    );
    assert_eq!(second_assignment.packet()["lead_parent_attempt"], MAIN);
    assert_eq!(spy.calls_named("send-keys").len(), 1);
    assert!(spy.calls_named("kill-pane").is_empty());
    flow["second"] = second;
    flow["main"] = swap("codex", "fixture-second-successor").into();
    std::fs::write(root.join("flow.json"), serde_json::to_vec(&flow).unwrap()).unwrap();
}
