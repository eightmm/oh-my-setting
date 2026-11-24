use oms_command_attempt_bundle::{owns, run};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn ownership_respects_option_values_and_action() {
    assert!(owns(&args(&["--repo", "show", "show", "--bundle"])));
    assert!(owns(&args(&["show", "--attempt", "--bundle", "--bundle"])));
    assert!(owns(&args(&["show", "--attempt", "--bundle"])));
    assert!(!owns(&args(&["--repo", "show", "list", "--bundle"])));
    assert!(!owns(&args(&["show", "--json"])));
    assert_eq!(
        run(&args(&["list"]), Path::new("."), &AtomicBool::new(false)),
        (75, vec![], vec![])
    );
}

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
fn isolated(name: &str) -> bool {
    if std::env::var("OMS_BUNDLE_TEST_CASE").ok().as_deref() == Some(name) {
        return true;
    }
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("OMS_BUNDLE_TEST_CASE", name)
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_FAIL_LEDGER")
        .status()
        .unwrap();
    assert!(result.success());
    false
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "oms-bundle-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join(".git")).unwrap();
        fs::create_dir_all(path.join(".oms/lifecycle")).unwrap();
        fs::create_dir_all(path.join(".oms/artifacts")).unwrap();
        for (name, bytes) in [
            (
                ".oms/lifecycle/events.jsonl",
                include_bytes!("fixtures/events.jsonl").as_slice(),
            ),
            (
                ".oms/artifacts/index.jsonl",
                include_bytes!("fixtures/artifacts.jsonl").as_slice(),
            ),
            (
                ".oms/failures.jsonl",
                include_bytes!("fixtures/failures.jsonl").as_slice(),
            ),
            (
                "transcript.jsonl",
                include_bytes!("fixtures/transcript.jsonl").as_slice(),
            ),
        ] {
            fs::write(path.join(name), bytes).unwrap();
        }
        Self(path)
    }
    fn call(&self, extra: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        let mut a = args(&["show", "--attempt", "attempt1", "--bundle"]);
        a.extend(args(extra));
        run(&a, &self.0, &AtomicBool::new(false))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn canonical_success_json_and_text() {
    if !isolated("canonical_success_json_and_text") {
        return;
    }
    let f = Fixture::new();
    for (extra, expected) in [
        (
            &["--transcript", "transcript.jsonl", "--json"][..],
            include_bytes!("fixtures/bundle.json").as_slice(),
        ),
        (
            &["--transcript", "transcript.jsonl"][..],
            include_bytes!("fixtures/bundle.txt").as_slice(),
        ),
    ] {
        let (code, out, err) = f.call(extra);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert_eq!(
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(expected)
        );
    }
    assert_eq!(
        fs::read(f.0.join(".oms/lifecycle/events.jsonl")).unwrap(),
        include_bytes!("fixtures/events.jsonl")
    );
}
#[test]
fn unavailable_empty_and_recorded_transcript() {
    if !isolated("unavailable_empty_and_recorded_transcript") {
        return;
    }
    let f = Fixture::new();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 0, "{:?}", err);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("no transcript path recorded")
    );
    fs::write(f.0.join("empty.jsonl"), "").unwrap();
    let (code, out, _) = f.call(&["--json", "--transcript", "empty.jsonl"]);
    assert_eq!(code, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("\"steps_total\": 0")
    );
    fs::create_dir_all(f.0.join(".oms/hooks/panel-activity")).unwrap();
    fs::write(
        f.0.join(".oms/hooks/panel-activity/participant1.json"),
        r#"{"attempt":"attempt1","transcript_path":"transcript.jsonl"}"#,
    )
    .unwrap();
    let (code, out, _) = f.call(&["--json"]);
    assert_eq!(code, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("panel-activity/participant1.json")
    );
}
#[test]
fn corruption_in_other_attempt_and_unsafe_ledger_fail_before_output() {
    if !isolated("corruption_in_other_attempt_and_unsafe_ledger_fail_before_output") {
        return;
    }
    let f = Fixture::new();
    let path = f.0.join(".oms/lifecycle/events.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"schema\":1,\"event_id\":\"bad\",\"attempt_id\":\"other\",\"seq\":2,\"ts\":\"2026-01-01T00:00:00Z\",\"event_type\":\"attempt.created\",\"to_state\":\"queued\"}\n");
    fs::write(&path, &bytes).unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("other sequence is 2, expected 1")
    );
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(f.0.join("transcript.jsonl"), &path).unwrap();
    let (code, out, _) = f.call(&[]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
}
#[test]
fn artifact_scalar_is_withheld_and_unreadable_optional_sections_survive() {
    if !isolated("artifact_scalar_is_withheld_and_unreadable_optional_sections_survive") {
        return;
    }
    let f = Fixture::new();
    let synthetic = format!("{}{}{}", "gh", "p_", "a".repeat(30));
    let row = format!("{{\"attempt_id\":\"attempt1\",\"selected_model\":\"{synthetic}\"}}\n");
    fs::write(f.0.join(".oms/artifacts/index.jsonl"), row).unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 0, "{:?}", err);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("[withheld: secret-like]"));
    assert!(!text.contains(&synthetic));
    fs::write(f.0.join(".oms/artifacts/index.jsonl"), "not json").unwrap();
    let (code, out, _) = f.call(&["--json"]);
    assert_eq!(code, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("\"readable\": false")
    );
}
#[test]
fn timeline_and_artifact_limits_are_fifty() {
    if !isolated("timeline_and_artifact_limits_are_fifty") {
        return;
    }
    let f = Fixture::new();
    let mut timeline = String::new();
    let mut artifacts = String::new();
    for i in 0..51 {
        timeline.push_str(&format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"i{i}","name":"Bash","input":{{}}}}]}}}}
"#));
        artifacts.push_str(&format!(
            "{{\"attempt_id\":\"attempt1\",\"event_id\":\"e{i}\"}}\n"
        ));
    }
    fs::write(f.0.join("transcript.jsonl"), timeline).unwrap();
    fs::write(f.0.join(".oms/artifacts/index.jsonl"), artifacts).unwrap();
    let (code, out, err) = f.call(&["--json", "--transcript", "transcript.jsonl"]);
    assert_eq!(code, 0, "{:?}", err);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("\"steps_omitted\": 1"));
    assert!(s.contains("\"steps_total\": 51"));
    assert!(s.contains("\"rows_omitted\": 1"));
    assert!(s.contains("\"rows_total\": 51"));
}

#[test]
fn child_refuses_before_missing_repository_read() {
    if std::env::var("OMS_BUNDLE_TEST_CASE").ok().as_deref() != Some("child") {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_refuses_before_missing_repository_read"])
            .env("OMS_BUNDLE_TEST_CASE", "child")
            .env("OMS_HARNESS_CHILD", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let (code, out, err) = run(
        &args(&["show", "--attempt", "a", "--bundle"]),
        Path::new("missing-repository"),
        &AtomicBool::new(false),
    );
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert_eq!(
        err,
        b"error: a harness child may only use read-only runtime actions\n"
    );
}
#[test]
fn scanner_failure_returns_no_output() {
    if std::env::var("OMS_BUNDLE_TEST_CASE").ok().as_deref() != Some("scanner") {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "scanner_failure_returns_no_output"])
            .env("OMS_BUNDLE_TEST_CASE", "scanner")
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_FAIL_LEDGER");
        for i in 0..4100 {
            command.env(format!("OMS_BUNDLE_ENV_{i}"), "x");
        }
        assert!(command.status().unwrap().success());
        return;
    }
    let f = Fixture::new();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("scan unavailable; nothing printed")
    );
}

#[test]
fn parsing_help_errors_and_raw_paths() {
    if !isolated("parsing_help_errors_and_raw_paths") {
        return;
    }
    let f = Fixture::new();
    let call = |a: &[&str]| run(&args(a), &f.0, &AtomicBool::new(false));
    assert!(owns(&args(&["show", "--bund", "--attempt=a"])));
    assert!(!owns(&args(&["show", "--", "--bundle"])));
    let (code, out, err) = call(&["show", "--bundle", "--help"]);
    assert_eq!(code, 0);
    assert!(err.is_empty());
    assert_eq!(out, include_bytes!("../src/help.txt"));
    let (code, out, err) = call(&["show", "--bundle"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("the following arguments are required: --attempt\n")
    );
    for a in [
        &["show", "--bundle", "--attempt"][..],
        &["show", "--bundle=yes", "--attempt=a"][..],
        &["show", "--bundle", "--attempt=a", "--unknown"][..],
    ] {
        let (code, out, _) = call(a);
        assert_eq!(code, 2);
        assert!(out.is_empty());
    }
    use std::os::unix::ffi::OsStringExt;
    let raw = if cfg!(target_os = "macos") {
        OsString::from("transcript space.jsonl")
    } else {
        OsString::from_vec(vec![b't', 0x80])
    };
    fs::copy(f.0.join("transcript.jsonl"), f.0.join(&raw)).unwrap();
    let mut a = args(&[
        "show",
        "--attempt",
        "attempt1",
        "--bundle",
        "--json",
        "--transcript",
    ]);
    a.push(raw);
    let (code, out, err) = run(&a, &f.0, &AtomicBool::new(false));
    assert_eq!(code, 0, "{:?}", err);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("\"available\": true")
    );
    let (code, out, _) = run(
        &args(&["show", "--attempt", "attempt1", "--bundle"]),
        &f.0,
        &AtomicBool::new(true),
    );
    assert_eq!(code, 2);
    assert!(out.is_empty());
}

#[test]
fn lifecycle_and_failure_limits_and_empty_ledger() {
    if !isolated("lifecycle_and_failure_limits_and_empty_ledger") {
        return;
    }
    let f = Fixture::new();
    for count in [4usize, 200, 201] {
        let mut ledger =
            String::from_utf8(include_bytes!("fixtures/events.jsonl").to_vec()).unwrap();
        for seq in 5..=count {
            ledger.push_str(&format!("{{\"schema\":1,\"event_id\":\"heartbeat{seq}\",\"attempt_id\":\"attempt1\",\"seq\":{seq},\"ts\":\"2026-01-01T00:00:03Z\",\"event_type\":\"attempt.heartbeat\"}}\n"));
        }
        fs::write(f.0.join(".oms/lifecycle/events.jsonl"), ledger).unwrap();
        let (code, out, err) = f.call(&["--json"]);
        assert_eq!(code, 0, "{:?}", err);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(&format!("\"events_total\": {count}")));
        assert!(s.contains(&format!(
            "\"events_omitted\": {}",
            count.saturating_sub(200)
        )));
    }
    for count in [0, 50, 51] {
        let rows = "{\"ts\":\"2026-01-01T00:00:01Z\",\"kind\":\"cmd\"}\n".repeat(count);
        fs::write(f.0.join(".oms/failures.jsonl"), rows).unwrap();
        let (code, out, err) = f.call(&["--json"]);
        assert_eq!(code, 0, "{:?}", err);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains(&format!("\"rows_omitted\": {}", count.saturating_sub(50)))
        );
    }
    // Exact native reference IDs are useful outside the time window; nearby
    // unrelated or legacy records never acquire that correlation.
    let digest = "a".repeat(64);
    let exact = format!(
        r#"{{"schema":2,"ts":"2026-01-02T00:00:00Z","agent":"codex","event":"verified","observed_fingerprint":"exact","verification":{{"scope":"agent-task-reference-v1","active_task_id":"task-one","action":"verification-rerun","snapshot_attempt_id":"snapshot-one","snapshot_sha256":"{digest}","outcome_attempt_id":"attempt1","outcome_sha256":"{digest}","command_sha256":"{digest}","result":"accepted","verified_state":"tree-one","prior_failures":["{digest}"]}}}}"#
    );
    let other = exact
        .replace("observed_fingerprint", "fingerprint")
        .replace("2026-01-02T00:00:00Z", "2026-01-01T00:00:01Z")
        .replace("\"exact\"", "\"other\"")
        .replace("\"task-one\"", "\"task-other\"")
        .replace("\"attempt1\"", "\"other-attempt\"");
    let malformed = exact
        .replace("\"snapshot-one\"", "null")
        .replace("\"exact\"", "\"invalid\"");
    // Exact IDs without the canonical schema are legacy/unknown input.
    let unschemed = other
        .replace("\"schema\":2,", "")
        .replace("\"other\"", "\"unschemed\"")
        .replace("\"other-attempt\"", "\"attempt1\"");
    let legacy = r#"{"ts":"2026-01-01T00:00:01Z","event":"fail","fingerprint":"legacy"}"#;
    fs::write(
        f.0.join(".oms/failures.jsonl"),
        format!("{exact}\n{other}\n{malformed}\n{unschemed}\n{legacy}\n"),
    )
    .unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let body = String::from_utf8(out).unwrap();
    assert!(body.contains("\"fingerprint\": \"exact\""), "{body}");
    assert!(body.contains("\"fingerprint\": \"other\""));
    assert!(body.contains("\"fingerprint\": \"legacy\""));
    assert!(body.contains("\"fingerprint\": \"unschemed\""));
    assert!(!body.contains("\"fingerprint\": \"invalid\""));
    assert_eq!(
        body.matches("\"correlation\": \"native-reference-attempt\"")
            .count(),
        1
    );
    assert_eq!(
        body.matches("\"correlation\": \"time-window-only\"")
            .count(),
        3
    );
    assert!(body.contains("neither proves a repair cause or current verification"));
    assert!(body.contains("\"outcome_sha256\""));
    assert!(body.contains("\"prior_failures\""));

    // Timestamp candidates must not fill the bounded view before an exact
    // accepted record. Valid foreign receipts remain advisory. Malformed native
    // receipts do not take a display slot or change the physical line pointer.
    let nearby = (0..51)
        .map(|n| legacy.replace("\"legacy\"", &format!("\"nearby-{n:02}\"")))
        .collect::<Vec<_>>()
        .join("\n");
    let malformed_nearby = malformed.replace("2026-01-02T00:00:00Z", "2026-01-01T00:00:01Z");
    for event in ["verified", "resolved"] {
        let accepted = if event == "resolved" {
            exact
                .replace("\"verified\"", "\"resolved\"")
                .replace("observed_fingerprint", "fingerprint")
        } else {
            exact.clone()
        };
        fs::write(
            f.0.join(".oms/failures.jsonl"),
            format!("{nearby}\n{other}\n{malformed_nearby}\n{accepted}\n"),
        )
        .unwrap();
        let (code, out, err) = f.call(&["--json"]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let body = String::from_utf8(out).unwrap();
        let exact_at = body
            .find("\"fingerprint\": \"exact\"")
            .expect("exact accepted evidence was displaced by timestamp candidates");
        assert!(body.contains(&format!("\"event\": \"{event}\"")));
        assert!(body.contains("\"line\": 54"));
        assert!(body.contains("\"rows_total\": 53"));
        assert!(body.contains("\"rows_omitted\": 3"));
        assert_eq!(
            body.matches("\"correlation\": \"native-reference-attempt\"")
                .count(),
            1
        );
        assert_eq!(
            body.matches("\"correlation\": \"time-window-only\"")
                .count(),
            49
        );
        let mut previous = exact_at;
        for n in 0..49 {
            let current = body
                .find(&format!("\"fingerprint\": \"nearby-{n:02}\""))
                .expect("timestamp candidates retain their ledger order");
            assert!(previous < current);
            previous = current;
        }
        for omitted in ["nearby-49", "nearby-50", "other", "invalid"] {
            assert!(!body.contains(&format!("\"fingerprint\": \"{omitted}\"")));
        }
        assert!(body.contains("neither proves a repair cause or current verification"));
    }

    // Malformed native receipts within the window cannot crowd out a valid
    // nearby failure either. The reported line pointer includes blank records.
    let nearby_exact = exact.replace("2026-01-02T00:00:00Z", "2026-01-01T00:00:01Z");
    let bad_envelope = nearby_exact.replace("\"agent\":\"codex\",", "");
    let duplicate = nearby_exact.replace(
        &format!("[\"{digest}\"]"),
        &format!("[\"{digest}\",\"{digest}\"]"),
    );
    let unknown_reset = nearby_exact
        .replace("observed_fingerprint", "fingerprint")
        .replace("\"verified\"", "\"resolved\"")
        .replace(&format!("[\"{digest}\"]"), "null");
    let conflict = nearby_exact.replace(
        "\"observed_fingerprint\":",
        "\"fingerprint\":\"other\",\"observed_fingerprint\":",
    );
    let bad_exit = nearby_exact.replace("\"agent\":", "\"exit\":-1,\"agent\":");
    let missing_prior = nearby_exact.replace(&format!(",\"prior_failures\":[\"{digest}\"]"), "");
    let unknown_schema = nearby_exact.replace("\"schema\":2", "\"schema\":9");
    let failed = nearby_exact
        .replace("observed_fingerprint", "fingerprint")
        .replace("\"verified\"", "\"fail\"")
        .replace("\"accepted\"", "\"failed\"");
    let exact_fail = failed
        .replace(
            "\"agent\":",
            "\"cmd\":\"false\",\"kind\":\"cmd\",\"exit\":1,\"agent\":",
        )
        .replace("\"exact\"", "\"native-fail\"");
    let records = [
        bad_envelope,
        duplicate,
        unknown_reset,
        conflict,
        bad_exit,
        missing_prior,
        unknown_schema,
        failed,
    ]
    .join("\n")
        + "\n";
    let unframed = nearby_exact.replace("\"exact\"", "\"unframed\"");
    let history = format!(
        "{}{legacy}\n\n{exact}\r\n{exact_fail}\n{unframed}\r{unframed}\n{unframed}",
        records.repeat(7)
    );
    fs::write(f.0.join(".oms/failures.jsonl"), history).unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let body = String::from_utf8(out).unwrap();
    assert_eq!(
        body.matches("\"correlation\": \"native-reference-attempt\"")
            .count(),
        2
    );
    assert_eq!(
        body.matches("\"correlation\": \"time-window-only\"")
            .count(),
        1
    );
    assert!(body.contains("\"fingerprint\": \"legacy\""));
    assert!(body.contains("\"fingerprint\": \"native-fail\""));
    assert!(body.contains("\"line\": 59"));
    assert!(body.contains("\"rows_total\": 3"));
    assert!(!body.contains("\"fingerprint\": \"unframed\""));

    // Exact identities still obey the cap and keep their ledger order.
    let exact_rows = (0..51)
        .map(|n| exact.replace("\"exact\"", &format!("\"exact-{n:02}\"")))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        f.0.join(".oms/failures.jsonl"),
        format!("{other}\n{malformed_nearby}\n{exact_rows}\n"),
    )
    .unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let body = String::from_utf8(out).unwrap();
    assert!(body.contains("\"rows_total\": 52"));
    assert!(body.contains("\"rows_omitted\": 2"));
    assert_eq!(
        body.matches("\"correlation\": \"native-reference-attempt\"")
            .count(),
        50
    );
    assert!(!body.contains("\"correlation\": \"time-window-only\""));
    let mut previous = 0;
    for n in 0..50 {
        let current = body
            .find(&format!("\"fingerprint\": \"exact-{n:02}\""))
            .expect("exact evidence remains in ledger order");
        assert!(previous < current);
        previous = current;
    }
    assert!(!body.contains("\"fingerprint\": \"exact-50\""));
    fs::write(f.0.join(".oms/lifecycle/events.jsonl"), "").unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert_eq!(err, b"error: unknown attempt: attempt1\n");
    fs::write(f.0.join(".oms/lifecycle/events.jsonl"), "not-json\n").unwrap();
    let (code, out, err) = f.call(&["--json"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("Expecting value: line 1 column 1 (char 0)\n")
    );
}

#[test]
fn regression_blank_line_dialects() {
    if !isolated("regression_blank_line_dialects") {
        return;
    }
    let f = Fixture::new();
    let mut life = b"\x1c\x1d\x1e\x1f\r\n".to_vec();
    life.extend_from_slice(include_bytes!("fixtures/events.jsonl"));
    fs::write(f.0.join(".oms/lifecycle/events.jsonl"), &life).unwrap();
    let mut failures = vec![];
    for extra in [
        &["--transcript", "transcript.jsonl", "--json"][..],
        &["--transcript", "transcript.jsonl"][..],
    ] {
        let (code, out, err) = f.call(extra);
        if code != 0 {
            failures.push("lifecycle C0 blank");
            continue;
        }
        assert!(err.is_empty());
        let expected = if extra.contains(&"--json") {
            include_bytes!("fixtures/bundle.json").as_slice()
        } else {
            include_bytes!("fixtures/bundle.txt").as_slice()
        };
        assert_eq!(out, expected);
    }
    fs::write(
        f.0.join(".oms/lifecycle/events.jsonl"),
        include_bytes!("fixtures/events.jsonl"),
    )
    .unwrap();
    for blank in ["\u{a0}", "\u{85}", "\u{1c}"] {
        let mut rows = include_bytes!("fixtures/artifacts.jsonl").to_vec();
        rows.extend_from_slice(blank.as_bytes());
        rows.push(b'\n');
        fs::write(f.0.join(".oms/artifacts/index.jsonl"), &rows).unwrap();
        let (code, out, err) = f.call(&["--json"]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        if !String::from_utf8(out)
            .unwrap()
            .contains("\"readable\": false")
        {
            failures.push("non-ASCII byte blank");
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn regression_parser_value_and_help_order() {
    if !isolated("regression_parser_value_and_help_order") {
        return;
    }
    let usage = format!(
        "{}\n",
        include_str!("../src/help.txt")
            .split("\n\n")
            .next()
            .unwrap()
    );
    let mut failures = vec![];
    for (argv, message) in [
        (
            &["show", "--attempt", "--bundle"][..],
            "argument --attempt: expected one argument",
        ),
        (
            &["show", "--bundle=x", "--help"][..],
            "argument --bundle: ignored explicit argument 'x'",
        ),
        (
            &["show", "--bundle", "--attempt", "--odd"][..],
            "argument --attempt: expected one argument",
        ),
        (
            &["show", "--bundle", "--attempt", "-1e3"][..],
            "argument --attempt: expected one argument",
        ),
        (
            &["show", "--bundle", "--unknown"][..],
            "the following arguments are required: --attempt",
        ),
        (
            &["show", "--bundle", "--attempt", "a", "--help=x"][..],
            "argument -h/--help: ignored explicit argument 'x'",
        ),
    ] {
        let owned = owns(&args(argv));
        let actual = run(
            &args(argv),
            Path::new("missing-repository"),
            &AtomicBool::new(false),
        );
        let expected = (
            2,
            vec![],
            format!("{usage}agent-events.sh show: error: {message}\n").into_bytes(),
        );
        if !owned || actual != expected {
            failures.push(format!("{argv:?}"));
        }
    }
    assert!(failures.is_empty(), "parser mismatches: {failures:?}");
    let f = Fixture::new();
    for value in ["-", "-1", "-.5", "--odd value"] {
        let (code, out, err) = run(
            &args(&["show", "--bundle", "--attempt", value]),
            &f.0,
            &AtomicBool::new(false),
        );
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert_eq!(
            err,
            b"error: attempt must match [A-Za-z0-9][A-Za-z0-9._:-]{0,159}\n"
        );
    }
    assert!(!owns(&args(&["show", "--attempt=--bundle"])));
    let help = run(
        &args(&["show", "--help", "--bundle=x"]),
        Path::new("missing-repository"),
        &AtomicBool::new(false),
    );
    assert_eq!(
        help,
        (0, include_bytes!("../src/help.txt").to_vec(), vec![])
    );
}

#[test]
fn regression_final_output_scan() {
    let marker = std::env::var("OMS_BUNDLE_TEST_CASE").unwrap_or_default();
    if !marker.starts_with("final-output-") {
        let name = ["OMS_", "TO", "KEN"].concat();
        let mut failures = vec![];
        for (case, value) in [
            ("numeric", "12345678.9"),
            ("key", "artifact_rows"),
            ("escaped", r"alpha\nbeta123"),
            ("text", "event_id=artifact1"),
        ] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "regression_final_output_scan", "--nocapture"])
                .env("OMS_BUNDLE_TEST_CASE", format!("final-output-{case}"))
                .env_remove("OMS_HARNESS_CHILD")
                .env_remove("OMS_FAIL_LEDGER")
                .env(&name, value)
                .status()
                .unwrap();
            if !status.success() {
                failures.push(case);
            }
        }
        assert!(failures.is_empty(), "surviving output hits: {failures:?}");
        return;
    }
    let f = Fixture::new();
    let modes = if marker == "final-output-text" {
        vec![vec![]]
    } else {
        vec![vec!["--json"], vec![]]
    };
    if marker == "final-output-numeric" {
        fs::write(
            f.0.join(".oms/artifacts/index.jsonl"),
            "{\"attempt_id\":\"attempt1\",\"duration_s\":12345678.9}\n",
        )
        .unwrap();
    }
    if marker == "final-output-escaped" {
        fs::write(
            f.0.join(".oms/artifacts/index.jsonl"),
            "{\"attempt_id\":\"attempt1\",\"kind\":\"alpha\\nbeta123\"}\n",
        )
        .unwrap();
    }
    for mode in modes {
        let (code, out, err) = f.call(&mode);
        assert_eq!(code, 2, "case {marker}");
        assert!(out.is_empty());
        assert_eq!(
            err,
            b"error: post-mortem bundle failed: secret scan unavailable; nothing printed\n"
        );
    }
}

#[test]
fn relative_failure_override_uses_caller_directory() {
    if std::env::var("OMS_BUNDLE_TEST_CASE").ok().as_deref() != Some("relative-failure") {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "relative_failure_override_uses_caller_directory"])
            .env("OMS_BUNDLE_TEST_CASE", "relative-failure")
            .env_remove("OMS_HARNESS_CHILD")
            .env("OMS_FAIL_LEDGER", "override.jsonl")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let repo = Fixture::new();
    let caller = Fixture::new();
    fs::write(
        repo.0.join("override.jsonl"),
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"fingerprint\":\"wrong-repo\"}\n",
    )
    .unwrap();
    fs::write(
        caller.0.join("override.jsonl"),
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"fingerprint\":\"caller-override\"}\n",
    )
    .unwrap();
    let mut argv = args(&["--repo"]);
    argv.push(repo.0.as_os_str().to_owned());
    argv.extend(args(&[
        "show",
        "--attempt",
        "attempt1",
        "--bundle",
        "--json",
    ]));
    let (code, out, err) = run(&argv, &caller.0, &AtomicBool::new(false));
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("caller-override"));
    assert!(!text.contains("wrong-repo"));
}
