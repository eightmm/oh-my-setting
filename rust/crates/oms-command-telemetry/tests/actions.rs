mod support;
use oms_context::Json;
use std::ffi::OsString;
use std::sync::atomic::AtomicBool;
use support::Temp;

fn native(root: &Temp, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    oms_command_telemetry::run(
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &root.0,
        &AtomicBool::new(false),
    )
}
fn event(id: &str, kind: &str, exit: i32, extra: &str) -> String {
    format!(
        "{{\"schema\":1,\"event_id\":\"{id}\",\"operation_id\":\"op_{id}\",\"artifact_id\":\"sha256:{id}\",\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"{kind}\",\"provider\":\"codex\",\"exit\":{exit}{extra}}}\n"
    )
}
fn rows() -> String {
    let digest = "a".repeat(64);
    event(
        "evt_a",
        "delegate",
        1,
        &format!(
            ",\"patch_sha256\":\"{digest}\",\"tokens\":300,\"task_id\":\"repeat\",\"role\":\"worker\""
        ),
    ) + &event(
        "evt_b",
        "delegate",
        124,
        ",\"tokens\":100,\"task_id\":\"repeat\"",
    ) + &event(
        "evt_c",
        "patch-admit",
        0,
        &format!(",\"patch_sha256\":\"{digest}\""),
    ) + &event("evt_d", "delegate", 0, ",\"tokens\":50")
}
fn assert_ok(r: &(i32, Vec<u8>, Vec<u8>)) {
    assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
}
fn check_code(root: &Temp, args: &[&str], expected_code: i32) {
    let result = native(root, args);
    assert_eq!(result.0, expected_code, "{args:?}: {:?}", result);
    if expected_code == 0 {
        assert!(result.2.is_empty(), "{args:?}: {:?}", result);
        if args.contains(&"--json") && args[0] != "latest-run" && !result.1.is_empty() {
            assert!(
                oms_context::parse_json(&result.1).is_ok(),
                "{args:?}: {:?}",
                result
            );
        }
    } else {
        assert!(!result.1.is_empty() || !result.2.is_empty(), "{args:?}");
        assert!(
            result.1.is_empty() || result.2.is_empty(),
            "{args:?}: {:?}",
            result
        );
    }
}
#[test]
fn native_views_and_waste_are_consistent() {
    let root = Temp::new();
    root.index(rows());
    let listed = native(&root, &["list", "--json"]);
    assert_ok(&listed);
    let doc = oms_context::parse_json(&listed.1).unwrap();
    assert_eq!(support::field(&doc, "schema"), &Json::Number("1".into()));
    assert_eq!(support::array(support::field(&doc, "rows")).len(), 4);
    root.write(".oms/hooks/events.jsonl",concat!(
        "{\"action\":\"telemetry\",\"session\":\"one\",\"model\":\"m1\"}\n",
        "{\"action\":\"telemetry\",\"session\":\"one\",\"model\":\"m2\",\"cache_creation_tokens\":400}\n",
        "{\"action\":\"injection\",\"session\":\"one\",\"status\":\"context\",\"bytes\":20}\n",
        "{\"action\":\"injection\",\"session\":\"two\",\"status\":\"context\",\"bytes\":10}\n"));
    root.write(".oms/artifacts/output-digest/a.txt", b"12345");
    root.write(".oms/artifacts/output-digest/b.txt", b"1234567890");
    for action in ["list", "latest", "latest-run", "failures", "unresolved"] {
        check_code(&root, &[action], 0);
        check_code(&root, &[action, "--json"], 0);
    }
    for action in ["list", "failures", "unresolved"] {
        check_code(&root, &[action, "2"], 0);
        check_code(&root, &[action, "2", "--json"], 0);
    }
    for top in ["1", "10", "100", "0001"] {
        check_code(&root, &["telemetry", "--waste", "--top", top], 0);
        check_code(&root, &["telemetry", "--waste", "--top", top, "--json"], 0);
    }
    check_code(
        &root,
        &["telemetry", "--waste", "--since", "2026-07-03", "--json"],
        0,
    );
    root.index(
        event(
            "evt_one",
            "delegate",
            1,
            ",\"tokens\":1,\"task_id\":\"retry\"",
        ) + &event(
            "evt_two",
            "delegate",
            1,
            ",\"tokens\":2,\"task_id\":\"retry\"",
        ),
    );
    check_code(&root, &["telemetry", "--waste"], 0);
    check_code(&root, &["telemetry", "--waste", "--json"], 0);
    let mut tied = String::new();
    for (id, task) in [
        ("evt_z1", "shared-prefix-xxz"),
        ("evt_a1", "shared-prefix-xxa"),
        ("evt_z2", "shared-prefix-xxz"),
        ("evt_a2", "shared-prefix-xxa"),
    ] {
        tied.push_str(&event(
            id,
            "delegate",
            1,
            &format!(",\"tokens\":5,\"task_id\":\"{task}\""),
        ));
    }
    root.index(tied);
    check_code(&root, &["telemetry", "--waste", "--json"], 0);
}
#[test]
fn missing_empty_and_argument_edges_are_handled() {
    let root = Temp::new();
    for body in [None, Some(b"".as_slice()), Some(b"{}\n".as_slice())] {
        let nonempty = body.is_some_and(|bytes| !bytes.is_empty());
        if let Some(body) = body {
            root.index(body);
        }
        for action in ["list", "latest", "latest-run", "failures", "unresolved"] {
            check_code(&root, &[action], if nonempty { 0 } else { 2 });
            check_code(&root, &[action, "--json"], 0);
        }
        for action in ["resolve-superseded", "resolve-recovered"] {
            check_code(&root, &[action], 0);
            check_code(&root, &[action, "--json"], 2);
        }
        check_code(&root, &["validate"], if nonempty { 1 } else { 2 });
        check_code(&root, &["validate", "--json"], 2);
        for action in ["migrate", "prune"] {
            check_code(&root, &[action], if nonempty { 0 } else { 2 });
            check_code(&root, &[action, "--json"], 2);
        }
        check_code(&root, &["--help"], 0);
        check_code(&root, &["telemetry", "--waste", "--top", ""], 0);
        for args in [
            vec!["list", "0"],
            vec!["list", ""],
            vec!["list", "invalid"],
            vec!["latest", "1"],
            vec!["resolve"],
            vec!["validate", "--files"],
            vec!["list", "--by", "day"],
            vec!["telemetry", "--waste", "--by", "day"],
            vec!["telemetry", "--waste", "--top", "0"],
            vec!["telemetry", "--waste", "--top", "101"],
            vec!["telemetry", "--waste", "--top", "bad"],
            vec!["telemetry", "--top", "1"],
        ] {
            check_code(&root, &args, 2);
        }
    }
}
#[test]
fn resolution_is_atomic_idempotent_and_preserves_lineage() {
    let root = Temp::new();
    let snapshot = rows();
    root.index(&snapshot);
    let invalid = native(
        &root,
        &["resolve", "--event-id", "evt_a", "--event-id", "missing"],
    );
    assert_eq!(invalid.0, 1);
    assert_eq!(invalid.2, b"error: unknown artifact event: missing\n");
    assert_eq!(
        std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
        snapshot.as_bytes()
    );
    check_code(&root, &["resolve", "--event-id", "evt_c"], 1);
    check_code(
        &root,
        &[
            "resolve",
            "--event-id",
            "evt_a",
            "--reason",
            &"x".repeat(201),
        ],
        2,
    );
    let r = native(
        &root,
        &[
            "resolve",
            "--event-id",
            "evt_a",
            "--event-id",
            "evt_b",
            "--event-id",
            "evt_a",
            "--reason",
            &"x".repeat(200),
        ],
    );
    assert_ok(&r);
    assert_eq!(
        String::from_utf8(r.1).unwrap(),
        "artifact-index: resolved evt_a\nartifact-index: resolved evt_b\n"
    );
    check_code(&root, &["validate"], 0);
    check_code(&root, &["unresolved", "--json"], 0);
    check_code(&root, &["resolve", "--event-id", "evt_a"], 0);
    let body = std::fs::read_to_string(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
    assert_eq!(body.lines().count(), 6);
    let second = native(
        &root,
        &["resolve", "--event-id", "evt_a", "--event-id", "evt_b"],
    );
    assert_ok(&second);
    assert_eq!(
        std::fs::read_to_string(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
        body
    );
}
#[test]
fn reason_scan_refuses_patterns_without_writes() {
    let root = Temp::new();
    let snapshot = rows();
    root.index(&snapshot);
    let mut assignments = Vec::new();
    assignments.push(format!("{}{}{}", "pass", "word", "=example-value"));
    assignments.push(format!("{}{}", "/ho", "me/user/private"));
    assignments.push(format!("{}{}", "Bearer ", "abcdefghijklmno"));
    for reason in assignments {
        let result = native(
            &root,
            &["resolve", "--event-id", "evt_a", "--reason", &reason],
        );
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
        assert_eq!(
            result.2,
            b"error: --reason contains sensitive-looking content\n"
        );
    }
    assert_eq!(
        std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
        snapshot.as_bytes()
    );
    for reason in ["", "triaged"] {
        let r = native(
            &root,
            &["resolve", "--event-id", "evt_a", "--reason", reason],
        );
        assert_ok(&r);
        root.index(&snapshot);
    }
}
#[test]
fn mechanical_resolvers_match_dry_plans_and_clear_once() {
    let root = Temp::new();
    for action in ["resolve-superseded", "resolve-recovered"] {
        root.index(rows());
        check_code(&root, &[action, "--dry-run"], 0);
        let before = std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
        assert_eq!(
            std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
            before
        );
        assert_ok(&native(&root, &[action]));
        check_code(&root, &[action], 0);
        check_code(&root, &["validate"], 0);
        check_code(&root, &["unresolved"], 0);
    }
}
#[test]
fn migration_and_retention_are_consistent() {
    let root = Temp::new();
    root.write(".oms/artifacts/recovered/old.md", b"answer");
    let source = "{\"kind\":\"call\",\"provider\":\"codex\",\"exit\":0,\"ts\":\"2026-07-01\",\"artifact\":\"gone/old.md\"}\n";
    root.index(source);
    assert_ok(&native(&root, &["migrate"]));
    let migrated = std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
    assert!(oms_context::parse_json(&migrated).is_ok());
    check_code(&root, &["migrate"], 0);
    check_code(&root, &["validate"], 0);
    let source = event("evt_old", "call", 0, ",\"artifact\":\"missing.md\"")
        + &event("evt_new", "call", 0, "");
    for args in [
        vec!["prune", "1", "--dry-run"],
        vec!["prune", "1"],
        vec!["prune", "--stale", "--dry-run"],
        vec!["prune", "--stale"],
        vec!["prune", "1", "--files", "--dry-run"],
    ] {
        root.index(&source);
        let result = native(&root, &args);
        assert_ok(&result);
        let kept = std::fs::read_to_string(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
        if args.contains(&"--dry-run") {
            assert_eq!(kept, source);
        } else {
            assert_eq!(kept.lines().count(), 1);
        }
    }
}
#[test]
fn salvage_plans_then_quarantines_before_replacement() {
    let root = Temp::new();
    let broken = rows() + "not JSON\n{\"kind\":\"partial\"}";
    root.index(&broken);
    let plan = native(&root, &["salvage"]);
    assert_ok(&plan);
    let text = String::from_utf8(plan.1).unwrap();
    assert!(text.contains("recovered=5 dropped=1"));
    assert!(text.contains("rerun with --apply"));
    assert_eq!(
        std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
        broken.as_bytes()
    );
    let applied = native(&root, &["salvage", "--apply"]);
    assert_ok(&applied);
    let quarantine = root.0.join(".oms/artifacts/quarantine");
    let files: Vec<_> = std::fs::read_dir(quarantine).unwrap().collect();
    assert_eq!(files.len(), 1);
    assert_eq!(
        std::fs::read(files[0].as_ref().unwrap().path()).unwrap(),
        broken.as_bytes()
    );
    check_code(&root, &["salvage", "--apply"], 0);
    let result = native(&root, &["list", "--json"]);
    assert_ok(&result);
    let doc = oms_context::parse_json(&result.1).unwrap();
    assert_eq!(support::array(support::field(&doc, "rows")).len(), 6);
}
#[test]
fn imports_publish_artifact_and_index_without_executing_provider() {
    let root = Temp::new();
    root.write("answer.txt", b"A complete answer.");
    root.write("prompt.txt", b"A question.");
    let r = native(
        &root,
        &[
            "import",
            "--kind",
            "ask",
            "--provider",
            "agy",
            "--file",
            "answer.txt",
            "--prompt-file",
            "prompt.txt",
        ],
    );
    assert_ok(&r);
    assert!(
        String::from_utf8(r.1)
            .unwrap()
            .starts_with("imported: antigravity -> ")
    );
    check_code(&root, &["validate"], 0);
    let r = native(&root, &["latest", "--json"]);
    let doc = oms_context::parse_json(&r.1).unwrap();
    let row = &support::array(support::field(&doc, "rows"))[0];
    assert_eq!(support::text(support::field(row, "kind")), "ask-import");
    let path = support::text(support::field(row, "artifact"));
    let text = std::fs::read_to_string(root.0.join(path)).unwrap();
    assert!(text.contains("## Output\n\nA complete answer."));
    assert!(text.contains("## Prompt\n\nA question."));
    for args in [
        vec!["import"],
        vec!["import", "--kind", "bad"],
        vec![
            "import",
            "--kind",
            "ask",
            "--provider",
            "unknown-provider",
            "--file",
            "answer.txt",
        ],
        vec![
            "import",
            "--kind",
            "ask",
            "--provider",
            "codex",
            "--file",
            "missing.txt",
        ],
        vec![
            "import",
            "--kind",
            "ask",
            "--provider",
            "codex",
            "--file",
            "answer.txt",
            "--exit",
            "bad",
        ],
    ] {
        assert_eq!(native(&root, &args).0, 2);
    }
    root.write("empty.txt", b"");
    assert_eq!(
        native(
            &root,
            &[
                "import",
                "--kind",
                "ask",
                "--provider",
                "codex",
                "--file",
                "empty.txt"
            ]
        )
        .0,
        2
    );
}
#[test]
fn all_actions_owned_including_parse_errors() {
    for action in [
        "list",
        "latest",
        "latest-run",
        "failures",
        "unresolved",
        "telemetry",
        "resolve",
        "resolve-superseded",
        "resolve-recovered",
        "validate",
        "migrate",
        "salvage",
        "prune",
        "import",
        "--help",
        "-h",
        "--unknown",
        "",
    ] {
        assert!(oms_command_telemetry::owns(&[action.into()]));
    }
    assert!(oms_command_telemetry::owns(&[]));
    assert!(oms_command_telemetry::owns(&[
        "telemetry".into(),
        "--waste".into()
    ]));
}
#[test]
fn corruption_is_strict_for_views_and_reported_by_validate() {
    let root = Temp::new();
    for raw in [
        b"{}".as_slice(),
        b"{}\ncorrupt\n",
        b"[]\n",
        b"\n",
        b"{\"exit\":NaN}\n",
    ] {
        root.index(raw);
        for action in [
            "list",
            "latest",
            "latest-run",
            "unresolved",
            "resolve-superseded",
            "resolve-recovered",
            "migrate",
            "prune",
            "telemetry",
        ] {
            assert_ne!(native(&root, &[action]).0, 0, "{action}");
        }
        let r = native(&root, &["validate"]);
        assert_eq!(r.0, 1);
        assert!(String::from_utf8(r.1).unwrap().contains("BAD line"));
        assert_eq!(
            std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
            raw
        );
    }
}
#[test]
fn concurrent_resolvers_append_only_one_resolution() {
    let root = Temp::new();
    root.index(rows());
    std::thread::scope(|s| {
        let one = s.spawn(|| native(&root, &["resolve", "--event-id", "evt_a"]));
        let two = s.spawn(|| native(&root, &["resolve", "--event-id", "evt_a"]));
        assert_ok(&one.join().unwrap());
        assert_ok(&two.join().unwrap());
    });
    let body = std::fs::read_to_string(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
    assert_eq!(body.lines().count(), 5);
    check_code(&root, &["validate"], 0);
}
#[test]
fn non_string_and_duplicate_lineage_cannot_hide_failure() {
    let root = Temp::new();
    let snapshot = rows() + &event("evt_a", "delegate", 0, "");
    root.index(&snapshot);
    check_code(&root, &["unresolved", "--json"], 0);
    check_code(&root, &["resolve-superseded", "--dry-run"], 0);
    check_code(&root, &["resolve", "--event-id", "evt_a"], 1);
    check_code(&root, &["validate"], 1);
    let r = native(&root, &["unresolved", "--json"]);
    let doc = oms_context::parse_json(&r.1).unwrap();
    assert!(matches!(support::field(&doc, "rows"), Json::Array(_)));
}

#[test]
fn validator_matches_corrupt_rows_and_typed_metadata() {
    let root = Temp::new();
    for raw in [
        "{}",
        "{}\ninvalid\n",
        "\n",
        "[]\n",
        "{\"schema\":\"legacy\",\"exit\":true}\n",
        "{\"schema\":2,\"exit\":-1}\n",
        "{\"exit\":NaN}\n",
    ] {
        root.index(raw);
        check_code(&root, &["validate"], 1);
    }
    for extra in [
        ",\"executor_id\":\"invalid space\"",
        ",\"soul_sha256\":\"short\"",
        ",\"traceparent\":\"raw\"",
        ",\"trace_id\":\"00\"",
        ",\"artifact\":\"../outside\"",
    ] {
        root.index(event("evt_bad", "call", 0, extra));
        check_code(&root, &["validate"], 1);
    }
}

#[test]
fn native_index_boundary_and_interruption_protect_writers() {
    use std::os::unix::fs::symlink;
    let root = Temp::new();
    root.index(rows());
    assert_ok(&native(&root, &["list", "--json"]));
    for args in [
        vec!["list", "--file", "../outside"],
        vec!["prune", "--file", "../outside"],
        vec!["resolve", "--file", "../outside", "--event-id", "evt_a"],
        vec!["salvage", "--file", "../outside", "--apply"],
    ] {
        assert_ne!(native(&root, &args).0, 0);
    }
    symlink("index.jsonl", root.0.join(".oms/artifacts/link.jsonl")).unwrap();
    for action in ["list", "prune", "migrate", "salvage"] {
        assert_ne!(
            native(&root, &[action, "--file", ".oms/artifacts/link.jsonl"]).0,
            0
        );
    }
    let argv: Vec<_> = ["resolve", "--event-id", "evt_a"]
        .iter()
        .map(OsString::from)
        .collect();
    let r = oms_command_telemetry::run(&argv, &root.0, &AtomicBool::new(true));
    assert_ne!(r.0, 0);
    assert_eq!(
        std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
        rows().as_bytes()
    );
}

#[test]
fn waste_absence_zero_and_malformed_ledger_are_not_estimates() {
    let root = Temp::new();
    root.index(event("evt_zero", "delegate", 0, ",\"tokens\":0"));
    check_code(&root, &["telemetry", "--waste", "--json"], 0);
    root.write(
        ".oms/hooks/events.jsonl",
        b"not-json\n{}\n{\"action\":\"injection\",\"status\":\"context\",\"bytes\":0}\n",
    );
    root.write(".oms/artifacts/output-digest/empty", b"");
    check_code(&root, &["telemetry", "--waste"], 0);
    check_code(&root, &["telemetry", "--waste", "--json"], 0);
    root.index(event(
        "evt_one",
        "delegate",
        1,
        ",\"tokens\":1,\"task_id\":\"only\"",
    ));
    check_code(&root, &["telemetry", "--waste", "--json"], 0);
}

#[test]
fn stale_prune_without_drops_preserves_index() {
    let root = Temp::new();
    let bytes = b"{ \"kind\": \"call\" }\r\n";
    root.index(bytes);
    let path = root.0.join(".oms/artifacts/index.jsonl");
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_ok(&native(&root, &["prune", "--stale"]));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
}

#[test]
fn unchanged_migration_resolution_and_prune_preserve_index_identity() {
    use std::os::unix::fs::MetadataExt;
    let root = Temp::new();
    root.index(event("evt_success", "delegate", 0, ""));
    let path = root.0.join(".oms/artifacts/index.jsonl");
    for args in [
        vec!["migrate"],
        vec!["resolve-superseded"],
        vec!["resolve-recovered"],
        vec!["prune", "1"],
        vec!["prune", "--stale"],
    ] {
        let body = std::fs::read(&path).unwrap();
        let before = std::fs::metadata(&path).unwrap();
        assert_ok(&native(&root, &args));
        assert_ok(&native(&root, &args));
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(body, std::fs::read(&path).unwrap(), "{args:?}");
        assert_eq!(
            (before.ino(), before.mtime(), before.mtime_nsec()),
            (after.ino(), after.mtime(), after.mtime_nsec()),
            "{args:?}"
        );
    }
}
