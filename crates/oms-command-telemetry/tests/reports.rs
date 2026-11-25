mod support;
use std::ffi::OsString;
use std::sync::atomic::AtomicBool;
use support::{Temp, array, field, text};

#[test]
fn empty_report_is_successful_json() {
    let root = Temp::new();
    let result = root.call(&["--json"]);
    assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
    let report = oms_context::parse_json(&result.1).unwrap();
    assert_eq!(
        field(field(&report, "window"), "available_rows"),
        &oms_context::Json::Number("0".into())
    );
}

#[test]
fn resolution_reports_reject_malformed_foreign_and_ambiguous_relations() {
    let target = r#"{"schema":1,"event_id":"failed","operation_id":"op","artifact_id":"art","kind":"delegate","exit":1}"#;
    let resolver = r#"{"schema":1,"event_id":"settled","operation_id":"op","artifact_id":"art","kind":"artifact-resolution","resolves_event_id":"failed","parent_event_id":"failed","resolution":"resolved","exit":0}"#;
    let pair = |resolution: String| vec![target.to_owned(), resolution];
    let cases = [
        ("valid", pair(resolver.into()), 1, 0),
        (
            "malformed",
            pair(r#"{"kind":"artifact-resolution","resolves_event_id":"failed"}"#.into()),
            0,
            1,
        ),
        (
            "foreign-operation",
            pair(resolver.replace(r#""operation_id":"op""#, r#""operation_id":"other""#)),
            0,
            1,
        ),
        (
            "foreign-artifact",
            pair(resolver.replace(r#""artifact_id":"art""#, r#""artifact_id":"other""#)),
            0,
            1,
        ),
        (
            "wrong-parent",
            pair(resolver.replace(
                r#""parent_event_id":"failed""#,
                r#""parent_event_id":"other""#,
            )),
            0,
            1,
        ),
        ("out-of-order", vec![resolver.into(), target.into()], 0, 1),
        (
            "duplicate-target",
            vec![target.into(), resolver.into(), target.into()],
            0,
            2,
        ),
        (
            "duplicate-resolver",
            vec![target.into(), resolver.into(), resolver.into()],
            0,
            1,
        ),
        (
            "invalid-duplicate",
            vec![
                target.into(),
                resolver.into(),
                r#"{"event_id":"settled"}"#.into(),
            ],
            0,
            1,
        ),
        (
            "distinct-resolvers",
            vec![
                target.into(),
                resolver.into(),
                resolver.replace("settled", "settled-again"),
            ],
            1,
            0,
        ),
    ];
    let root = Temp::new();
    for (name, rows, resolved, unresolved) in cases {
        let bytes = format!("{}\n", rows.join("\n"));
        root.index(&bytes);
        let (code, out, err) = root.call(&["--json"]);
        assert_eq!(code, 0, "{name}: {}", String::from_utf8_lossy(&err));
        let report = oms_context::parse_json(&out).unwrap();
        let resolution = field(&report, "resolution");
        assert_eq!(
            field(resolution, "resolved_nonzero"),
            &oms_context::Json::Number(resolved.to_string()),
            "{name}"
        );
        assert_eq!(
            field(resolution, "unresolved_nonzero"),
            &oms_context::Json::Number(unresolved.to_string()),
            "{name}"
        );
        assert_eq!(
            std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
            bytes.as_bytes(),
            "{name}: reporting must not mutate evidence"
        );
    }
}

/// Golden bytes were captured from the public scripts/artifact-index.sh entry,
/// whose strict store preflight runs before artifact-telemetry.py. The fixture
/// covers cached and head/tail metrics, every grouping, range-before-limit,
/// resolutions, admission, effort, cumulative usage, replay and hook maxima.
#[test]
fn reports_match_canonical_public_fixtures() {
    let fixtures = oms_context::parse_json(include_bytes!("fixtures.json")).unwrap();
    for fixture in array(&fixtures) {
        let root = Temp::new();
        root.index(text(field(fixture, "index")));
        let oms_context::Json::Object(artifacts) = field(fixture, "artifacts") else {
            panic!("expected artifacts")
        };
        for (path, body) in artifacts {
            let body = if let oms_context::Json::String(s) = body {
                s.clone()
            } else {
                format!("- started: 2026-07-02T23:59:51Z\n## Prompt\n{}\n## Output\ntokens used\n17\nconfigured model\nconfigured\nusage detail: {{\"provider\":\"codex\",\"input_tokens\":20,\"cache_read_tokens\":9,\"output_tokens\":4,\"session_cumulative\":true}}\n","x".repeat(150000)).into_bytes()
            };
            root.write(std::str::from_utf8(path).unwrap(), body);
        }
        if let oms_context::Json::String(hooks) = field(fixture, "hooks") {
            root.write(".oms/hooks/events.jsonl", hooks);
        }
        let before = std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap();
        for case in array(field(fixture, "cases")) {
            let args: Vec<&str> = array(field(case, "args")).iter().map(text).collect();
            for (mode, expected) in [(false, "human"), (true, "json")] {
                let mut argv = args.clone();
                if mode {
                    argv.push("--json");
                }
                let (exit, stdout, stderr) = root.call(&argv);
                assert_eq!(
                    exit,
                    0,
                    "{} {argv:?}: {}",
                    text(field(fixture, "name")),
                    String::from_utf8_lossy(&stderr)
                );
                assert!(stderr.is_empty());
                assert_eq!(
                    String::from_utf8(stdout).unwrap(),
                    if cfg!(target_os = "macos") {
                        // Golden captures use GNU strftime; BSD pads years below 1000.
                        text(field(case, expected))
                            .replace("\"group\": \"1-01-01\"", "\"group\": \"0001-01-01\"")
                            .replace("1-01-01   ", "0001-01-01")
                    } else {
                        text(field(case, expected)).to_owned()
                    },
                    "{} {argv:?}",
                    text(field(fixture, "name"))
                );
            }
        }
        assert_eq!(
            std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
            before
        );
    }
}

#[test]
fn raw_paths_caller_directory_and_read_only_reentrancy() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    let root = Temp::new();
    let repo = root.0.join(if cfg!(target_os = "macos") {
        OsString::from("repo-space dir")
    } else {
        OsString::from_vec(b"repo-\xff".to_vec())
    });
    std::fs::create_dir_all(repo.join(".git/objects")).unwrap();
    std::fs::create_dir_all(repo.join(".git/refs")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(repo.join("nested")).unwrap();
    let custom = repo.join(if cfg!(target_os = "macos") {
        OsString::from("index-space.jsonl")
    } else {
        OsString::from_vec(b"index-\xfe.jsonl".to_vec())
    });
    std::fs::write(&custom, b"{\"kind\":\"call\",\"tokens\":7}\n").unwrap();
    let alias = root.0.join("alias");
    symlink(&repo, &alias).unwrap();
    let argv = [
        OsString::from("--repo"),
        OsString::from("alias/nested"),
        OsString::from("--file"),
        alias.join(custom.file_name().unwrap()).into_os_string(),
        OsString::from("telemetry"),
        OsString::from("--json"),
    ];
    let interrupted = AtomicBool::new(false);
    let first = oms_command_telemetry::run(&argv, &root.0, &interrupted);
    assert_eq!(first.0, 0, "{}", String::from_utf8_lossy(&first.2));
    assert!(String::from_utf8_lossy(&first.1).contains("\"total\": 7"));
    std::thread::scope(|scope| {
        let second = scope.spawn(|| oms_command_telemetry::run(&argv, &root.0, &interrupted));
        let third = oms_command_telemetry::run(&argv, &root.0, &interrupted);
        assert_eq!(third, first);
        assert_eq!(second.join().unwrap(), first);
    });
    assert_eq!(
        std::fs::read(custom).unwrap(),
        b"{\"kind\":\"call\",\"tokens\":7}\n"
    );
}

#[test]
fn artifact_paths_preserve_raw_bytes_and_external_paths_are_not_metrics() {
    use std::os::unix::fs::symlink;
    let root = Temp::new();
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStringExt;
        let raw = root
            .0
            .join(".oms/artifacts")
            .join(OsString::from_vec(b"result-\xff.md".to_vec()));
        std::fs::write(raw, b"## Output\ntokens used\n19\n").unwrap();
        root.index(b"{\"kind\":\"call\",\"artifact\":\".oms/artifacts/result-\\udcff.md\"}\n");
        let result = root.call(&["--json"]);
        assert_eq!(result.0, 0);
        assert!(String::from_utf8_lossy(&result.1).contains("\"total\": 19"));
    }
    let repo = root.0.join("isolated");
    std::fs::create_dir_all(repo.join(".oms/artifacts")).unwrap();
    let external = root.0.join("external.md");
    std::fs::write(&external, b"## Output\ntokens used\n999\n").unwrap();
    symlink(&external, repo.join(".oms/artifacts/link.md")).unwrap();
    std::fs::write(
        repo.join(".oms/artifacts/index.jsonl"),
        b"{\"kind\":\"call\",\"artifact\":\".oms/artifacts/link.md\",\"tokens\":7}\n",
    )
    .unwrap();
    let result = support::call(&repo, &["--json"]);
    assert_eq!(result.0, 0);
    let report = oms_context::parse_json(&result.1).unwrap();
    assert_eq!(
        field(field(&report, "coverage"), "artifact_files"),
        &oms_context::Json::Number("0".into())
    );
    assert_eq!(
        field(
            field(field(&report, "usage"), "provider_reported_tokens"),
            "total"
        ),
        &oms_context::Json::Number("7".into())
    );
}

#[test]
fn human_summary_only_encodes_printed_text() {
    let root = Temp::new();
    root.index(b"{\"ts\":\"\\ud800\",\"kind\":\"call\",\"fallback_reason\":\"\\ud800\",\"fallback_used\":true}\n");
    assert_eq!(root.call(&[]).0, 0);
    assert_eq!(root.call(&["--json"]).0, 2);
    root.index(b"{\"kind\":\"call\",\"provider\":\"\\ud800\"}\n");
    assert_eq!(root.call(&[]).0, 2);
    assert_eq!(root.call(&["--by", "provider"]).0, 2);
}

#[test]
fn environment_thresholds_follow_recorded_integer_rules() {
    if let Ok(expected) = std::env::var("OMS_TELEMETRY_TEST_EXPECTED") {
        let root = Temp::new();
        root.index(b"{\"kind\":\"call\",\"provider_usage\":{\"input_tokens\":1,\"cache_read_tokens\":1,\"output_tokens\":0}}\n");
        let result = root.call(&["--by", "role", "--json"]);
        assert_eq!(result.0, 0);
        let report = oms_context::parse_json(&result.1).unwrap();
        let block = field(&report, "usage_by");
        assert_eq!(
            field(field(block, "cache_warn"), "min_requests"),
            &oms_context::Json::Number(expected)
        );
        return;
    }
    // Command.env changes only the test child; concurrent handlers and tests
    // never mutate their process environment.
    let limit = "0".repeat(4300);
    let oversized = "0".repeat(4301);
    for (input, expected) in [
        ("0", "0"),
        ("-0", "0"),
        ("+80", "80"),
        ("1_000", "1000"),
        (" １０ ", "10"),
        ("١٠", "10"),
        ("00080", "80"),
        ("", "10"),
        ("-1", "10"),
        ("1__0", "10"),
        ("_1", "10"),
        ("1_", "10"),
        ("1.0", "10"),
        ("nan", "10"),
        ("+", "10"),
        ("80%", "10"),
        (&limit, "0"),
        (&oversized, "10"),
    ] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "environment_thresholds_follow_recorded_integer_rules",
            ])
            .env("OMS_TELEMETRY_CACHE_MIN_REQUESTS", input)
            .env("OMS_TELEMETRY_TEST_EXPECTED", expected)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{input:?}: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}
