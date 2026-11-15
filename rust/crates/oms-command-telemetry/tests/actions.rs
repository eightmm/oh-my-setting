mod support;
use oms_context::Json;
use std::ffi::OsString;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use support::Temp;

fn native(root: &Temp, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    oms_command_telemetry::run(
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &root.0,
        &AtomicBool::new(false),
    )
}
fn oracle(root: &Temp, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let r = Command::new("bash")
        .args(["-c", "umask 077; exec bash \"$@\"", "oracle"])
        .arg(repo.join("scripts/artifact-index.sh"))
        .args(args)
        .current_dir(&root.0)
        .output()
        .unwrap();
    (r.status.code().unwrap(), r.stdout, r.stderr)
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
fn compare(root: &Temp, args: &[&str]) {
    let path = root.0.join(".oms/artifacts/index.jsonl");
    let before = std::fs::read(&path).ok();
    let got = native(root, args);
    let after = std::fs::read(&path).ok();
    if let Some(body) = before {
        root.index(body);
    } else if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    let expected = oracle(root, args);
    if let Some(body) = after {
        root.index(body);
    } else if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    let decode = |r: (i32, Vec<u8>, Vec<u8>)| {
        let body = if args.contains(&"--json") {
            oms_context::parse_json(&r.1)
                .ok()
                .map(|j| format!("{:?}", normalized(j)))
                .unwrap_or_else(|| String::from_utf8_lossy(&r.1).into_owned())
        } else {
            String::from_utf8_lossy(&r.1).into_owned()
        };
        (r.0, body, String::from_utf8_lossy(&r.2).into_owned())
    };
    assert_eq!(decode(got), decode(expected), "{args:?}");
}
#[test]
fn native_views_and_waste_match_public_oracle() {
    let root = Temp::new();
    root.index(rows());
    root.write(".oms/hooks/events.jsonl",concat!(
        "{\"action\":\"telemetry\",\"session\":\"one\",\"model\":\"m1\"}\n",
        "{\"action\":\"telemetry\",\"session\":\"one\",\"model\":\"m2\",\"cache_creation_tokens\":400}\n",
        "{\"action\":\"injection\",\"session\":\"one\",\"status\":\"context\",\"bytes\":20}\n",
        "{\"action\":\"injection\",\"session\":\"two\",\"status\":\"context\",\"bytes\":10}\n"));
    root.write(".oms/artifacts/output-digest/a.txt", b"12345");
    root.write(".oms/artifacts/output-digest/b.txt", b"1234567890");
    for action in ["list", "latest", "latest-run", "failures", "unresolved"] {
        compare(&root, &[action]);
        compare(&root, &[action, "--json"]);
    }
    for action in ["list", "failures", "unresolved"] {
        compare(&root, &[action, "2"]);
        compare(&root, &[action, "2", "--json"]);
    }
    for top in ["1", "10", "100", "0001"] {
        compare(&root, &["telemetry", "--waste", "--top", top]);
        compare(&root, &["telemetry", "--waste", "--top", top, "--json"]);
    }
    compare(
        &root,
        &["telemetry", "--waste", "--since", "2026-07-03", "--json"],
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
    compare(&root, &["telemetry", "--waste"]);
    compare(&root, &["telemetry", "--waste", "--json"]);
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
    compare(&root, &["telemetry", "--waste", "--json"]);
}
#[test]
fn missing_empty_and_argument_edges_match_oracle() {
    let root = Temp::new();
    for body in [None, Some(b"".as_slice()), Some(b"{}\n".as_slice())] {
        if let Some(body) = body {
            root.index(body);
        }
        for action in [
            "list",
            "latest",
            "latest-run",
            "failures",
            "unresolved",
            "resolve-superseded",
            "resolve-recovered",
            "validate",
            "migrate",
            "prune",
        ] {
            compare(&root, &[action]);
            compare(&root, &[action, "--json"]);
        }
        for args in [
            vec!["--help"],
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
            vec!["telemetry", "--waste", "--top", ""],
            vec!["telemetry", "--top", "1"],
        ] {
            compare(&root, &args);
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
    assert_eq!(
        std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
        snapshot.as_bytes()
    );
    for args in [
        vec!["resolve", "--event-id", "evt_c"],
        vec![
            "resolve",
            "--event-id",
            "evt_a",
            "--reason",
            &"x".repeat(201),
        ],
    ] {
        compare(&root, &args);
    }
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
    compare(&root, &["validate"]);
    compare(&root, &["unresolved", "--json"]);
    compare(&root, &["resolve", "--event-id", "evt_a"]);
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
        compare(
            &root,
            &["resolve", "--event-id", "evt_a", "--reason", &reason],
        );
        assert_eq!(
            native(
                &root,
                &["resolve", "--event-id", "evt_a", "--reason", &reason]
            )
            .0,
            2
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
        compare(&root, &[action, "--dry-run"]);
        let before = std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
        let expected = oracle(&root, &[action]);
        root.index(&before);
        assert_eq!(native(&root, &[action]), expected);
        compare(&root, &[action]);
        compare(&root, &["validate"]);
        compare(&root, &["unresolved"]);
    }
}
#[test]
fn migration_and_retention_match_public_oracle() {
    let root = Temp::new();
    root.write(".oms/artifacts/recovered/old.md", b"answer");
    let source = "{\"kind\":\"call\",\"provider\":\"codex\",\"exit\":0,\"ts\":\"2026-07-01\",\"artifact\":\"gone/old.md\"}\n";
    root.index(source);
    let expected = oracle(&root, &["migrate"]);
    let migrated = std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
    root.index(source);
    assert_eq!(native(&root, &["migrate"]), expected);
    assert_eq!(
        normalized(
            oms_context::parse_json(
                &std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap()
            )
            .unwrap()
        ),
        normalized(oms_context::parse_json(&migrated).unwrap())
    );
    compare(&root, &["migrate"]);
    compare(&root, &["validate"]);
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
        let expected = oracle(&root, &args);
        let kept = std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
        root.index(&source);
        assert_eq!(native(&root, &args), expected, "{args:?}");
        assert_eq!(
            std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
            kept
        );
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
    compare(&root, &["salvage", "--apply"]);
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
    compare(&root, &["validate"]);
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
    compare(&root, &["validate"]);
}
#[test]
fn non_string_and_duplicate_lineage_cannot_hide_failure() {
    let root = Temp::new();
    let snapshot = rows() + &event("evt_a", "delegate", 0, "");
    root.index(&snapshot);
    compare(&root, &["unresolved", "--json"]);
    compare(&root, &["resolve-superseded", "--dry-run"]);
    compare(&root, &["resolve", "--event-id", "evt_a"]);
    compare(&root, &["validate"]);
    let r = native(&root, &["unresolved", "--json"]);
    let doc = oms_context::parse_json(&r.1).unwrap();
    assert!(matches!(support::field(&doc, "rows"), Json::Array(_)));
}

fn normalized(mut value: Json) -> Json {
    fn sort(value: &mut Json) {
        match value {
            Json::Object(fields) => {
                for (_, v) in fields.iter_mut() {
                    sort(v);
                }
                fields.sort_by(|a, b| a.0.cmp(&b.0));
            }
            Json::Array(items) => {
                for v in items {
                    sort(v);
                }
            }
            _ => (),
        }
    }
    sort(&mut value);
    value
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
        compare(&root, &["validate"]);
    }
    for extra in [
        ",\"executor_id\":\"invalid space\"",
        ",\"soul_sha256\":\"short\"",
        ",\"traceparent\":\"raw\"",
        ",\"trace_id\":\"00\"",
        ",\"artifact\":\"../outside\"",
    ] {
        root.index(event("evt_bad", "call", 0, extra));
        compare(&root, &["validate"]);
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
    compare(&root, &["telemetry", "--waste", "--json"]);
    root.write(
        ".oms/hooks/events.jsonl",
        b"not-json\n{}\n{\"action\":\"injection\",\"status\":\"context\",\"bytes\":0}\n",
    );
    root.write(".oms/artifacts/output-digest/empty", b"");
    compare(&root, &["telemetry", "--waste"]);
    compare(&root, &["telemetry", "--waste", "--json"]);
    root.index(event(
        "evt_one",
        "delegate",
        1,
        ",\"tokens\":1,\"task_id\":\"only\"",
    ));
    compare(&root, &["telemetry", "--waste", "--json"]);
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
