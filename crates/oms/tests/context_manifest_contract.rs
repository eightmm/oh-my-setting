use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "oms-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn call(&self, args: &[&str], child: bool) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_oms"));
        c.args(["runtime", "--repo"])
            .arg(&self.0)
            .arg("context-inspect")
            .args(args);
        if child {
            c.env("OMS_HARNESS_CHILD", "1");
        } else {
            c.env_remove("OMS_HARNESS_CHILD");
        }
        c.output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn corpus(kind: &str) -> Value {
    let root = std::env::var_os("OMS_CONTRACT_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|p| {
                    p.join("crates/oms-context/tests/fixtures/context-inspect-v1.json")
                        .exists()
                })
                .unwrap()
                .to_path_buf()
        });
    serde_json::from_slice(
        &std::fs::read(root.join(format!(
            "crates/oms-context/tests/fixtures/context-{kind}-v1.json"
        )))
        .unwrap(),
    )
    .unwrap()
}
fn vectors() -> Value {
    corpus("inspect")
}

fn decoded(value: &oms_context::Json) -> Value {
    use oms_context::Json;
    match value {
        Json::Null => Value::Null,
        Json::Bool(v) => (*v).into(),
        Json::Number(v) => serde_json::from_str(v).unwrap(),
        Json::String(v) => String::from_utf8(v.clone()).unwrap().into(),
        Json::Array(v) => v.iter().map(decoded).collect(),
        Json::Object(v) => v
            .iter()
            .map(|(k, v)| (String::from_utf8(k.clone()).unwrap(), decoded(v)))
            .collect(),
    }
}

#[test]
fn frozen_raw_read_vectors() {
    let data = corpus("read");
    assert_eq!(data["fixture_schema"], 1);
    assert_eq!(data["reference"]["entrypoint"], "read_manifest");
    assert_eq!(
        data["reference"]["commit"],
        "74f87f654a314961420be2aa8ab6dfc1999151e1"
    );
    assert_eq!(
        data["reference"]["source"],
        "scripts/lib/oms_runtime/context_inspect.py"
    );
    assert_eq!(
        data["reference"]["source_sha256"],
        "1b3bad09517f4f6abd1269481b95986ffb62e84c811e6dfb4555691663742da8"
    );
    assert_eq!(data["reference"]["arguments"], serde_json::json!({}));
    let cases = data["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    let mut names = std::collections::HashSet::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        assert!(names.insert(name));
        assert_ne!(
            case.get("expected").is_some(),
            case.get("expected_error").is_some(),
            "{name}"
        );
        let hex = case["input_hex"].as_str().unwrap().as_bytes();
        assert_eq!(hex.len() % 2, 0);
        let bytes: Vec<u8> = hex
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect();
        let f = Fixture::new();
        std::fs::write(f.0.join("pack.json"), bytes).unwrap();
        let root = oms_safe_fs::Root::open(&f.0).unwrap();
        let raw = root
            .read_manifest_detailed(Path::new("pack.json"), 4 * 1024 * 1024)
            .unwrap();
        let result = oms_context::parse_json(&raw);
        if let Some(expected) = case.get("expected") {
            assert_eq!(decoded(&result.unwrap()), *expected, "{name}");
        } else {
            assert_eq!(
                result.unwrap_err(),
                case["expected_error"].as_str().unwrap(),
                "{name}"
            );
        }
    }
}
#[test]
fn frozen_vectors_through_binary() {
    let f = Fixture::new();
    let data = vectors();
    assert_eq!(data["cases"].as_array().unwrap().len(), 40);
    for case in data["cases"].as_array().unwrap() {
        std::fs::write(
            f.0.join("pack.json"),
            serde_json::to_vec(&case["input"]).unwrap(),
        )
        .unwrap();
        // Boolean JSON arguments have no corresponding typed CLI option.
        let limit = case
            .get("arguments")
            .and_then(|a| a.get("limit"))
            .unwrap_or(&data["reference"]["arguments"]["limit"])
            .clone();
        if limit.is_boolean() {
            continue;
        }
        let limit = limit.to_string();
        for child in [false, true] {
            let out = f.call(&["pack.json", "--limit", &limit], child);
            if let Some(expected) = case.get("expected") {
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    *expected
                );
            } else {
                assert_eq!(out.status.code(), Some(2));
                assert!(out.stdout.is_empty());
                assert!(
                    String::from_utf8_lossy(&out.stderr)
                        .contains(case["expected_error"].as_str().unwrap())
                );
            }
        }
    }
}

#[test]
fn frozen_comparisons_through_binary() {
    let data = corpus("compare");
    let f = Fixture::new();
    for case in data["cases"].as_array().unwrap() {
        let limit = case
            .get("arguments")
            .and_then(|a| a.get("limit"))
            .unwrap_or(&data["reference"]["arguments"]["limit"]);
        if limit.is_boolean() {
            continue;
        }
        std::fs::write(
            f.0.join("left.json"),
            serde_json::to_vec(&case["left"]).unwrap(),
        )
        .unwrap();
        std::fs::write(
            f.0.join("right.json"),
            serde_json::to_vec(&case["right"]).unwrap(),
        )
        .unwrap();
        for child in [false, true] {
            let out = f.call(
                &[
                    "left.json",
                    "--compare",
                    "right.json",
                    "--limit",
                    &limit.to_string(),
                ],
                child,
            );
            if let Some(expected) = case.get("expected") {
                assert!(
                    out.status.success(),
                    "{}: {}",
                    case["name"],
                    String::from_utf8_lossy(&out.stderr)
                );
                assert!(out.stderr.is_empty());
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    *expected,
                    "{}",
                    case["name"]
                );
            } else {
                assert_eq!(out.status.code(), Some(2), "{}", case["name"]);
                assert!(out.stdout.is_empty());
                assert!(String::from_utf8_lossy(&out.stderr).contains(&format!(
                    "runtime-core: {}\n",
                    case["expected_error"].as_str().unwrap()
                )));
            }
        }
    }
}

#[test]
fn session_and_injections_through_binary() {
    let f = Fixture::new();
    let repo = f.0.join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(f.0.join("session"), b"{}\n").unwrap();
    let call = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_oms"))
            .current_dir(&f.0)
            .args(["runtime", "--repo"])
            .arg(&repo)
            .arg("context-inspect")
            .args(args)
            .env_remove("OMS_HARNESS_CHILD")
            .output()
            .unwrap()
    };
    let session = call(&["--session", "session"]);
    assert!(
        session.status.success(),
        "{}",
        String::from_utf8_lossy(&session.stderr)
    );
    let expected = concat!(
        r#"{"async_context":{"conflicting_identities":0,"context_rows":0,"duplicate_rows":0,"identified_bytes":0,"identified_contexts":0,"identity_limit_rows":0,"invalid_context_rows":0,"model_use":"unknown","note":"Provider-recorded asynchronous context, not proof of model use or OMS attribution. Recorded bytes include repeated rows; identified bytes count each unambiguous session/UUID once, requiring both identities. Missing identities and entries beyond the identity limit are not deduplicated. No observation does not establish non-delivery; synchronous hooks and other transcript formats are not counted.","producer_attribution":"unknown","recorded_bytes":0,"response_rows":0,"source":"attachment.async_hook_response.response.hookSpecificOutput.additionalContext","status":"not_observed","unidentified_rows":0,"without_context_rows":0},"by_tool":{"items":[],"omitted":0,"total":0},"file_reads":{"distinct_in_repository":0,"outside_repository":0,"repeated":{"items":[],"omitted":0,"total":0}},"format":"unknown","kind":"oms-context-session","lines":1,"note":"Tool-result text as it was recorded, in bytes; the provider may have cleared or summarized it since. Not tokens, not a bill and not the current context window. Only Claude Code JSONL transcripts are read; any other file reports format unknown.","result_bytes":0,"schema":1,"tool_calls":0,"tool_results":0,"unit":"utf-8 bytes","unmatched_results":0,"unread_lines":0}"#,
        "\n"
    );
    assert_eq!(session.stdout, expected.as_bytes());
    assert!(session.stderr.is_empty());
    let expanded = Command::new(env!("CARGO_BIN_EXE_oms"))
        .current_dir(&f.0)
        .env("HOME", &f.0)
        .env_remove("OMS_HARNESS_CHILD")
        .args(["runtime", "--repo"])
        .arg(&repo)
        .args(["context-inspect", "--session", "~/session"])
        .output()
        .unwrap();
    assert_eq!(expanded.status.code(), Some(0));
    assert_eq!(expanded.stdout, session.stdout);
    assert!(expanded.stderr.is_empty());
    let call_home = |home: &std::ffi::OsStr, path: &str| {
        Command::new(env!("CARGO_BIN_EXE_oms"))
            .current_dir(&f.0)
            .env("HOME", home)
            .env_remove("OMS_HARNESS_CHILD")
            .args(["runtime", "--repo"])
            .arg(&repo)
            .args(["context-inspect", "--session", path])
            .output()
            .unwrap()
    };
    let file_home = f.0.join("session");
    let slash = call_home(file_home.as_os_str(), "~/");
    assert_eq!(slash.status.code(), Some(2));
    assert!(slash.stdout.is_empty());
    let trailing = format!("{}/", file_home.display());
    let plain = call_home(std::ffi::OsStr::new(&trailing), "~");
    assert_eq!(plain.status.code(), Some(0));
    assert_eq!(plain.stdout, session.stdout);
    assert_eq!(
        serde_json::from_slice::<Value>(&session.stdout).unwrap()["lines"],
        1
    );
    let timeline = call(&["--session", "session", "--timeline"]);
    assert!(timeline.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&timeline.stdout).unwrap()["kind"],
        "oms-context-timeline"
    );
    let rows = b"{\"timestamp\":\"1969-12-31T23:59:59.5\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"Read\",\"input\":{\"file_path\":\"foo\"}}]}}\n{\"timestamp\":\"1970-01-01T01:00:00.5+01:00\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"a\",\"content\":\"ok\"}]}}\n";
    std::fs::write(f.0.join("session"), rows).unwrap();
    let timeline = call(&["--session", "session", "--timeline", "--limit", "0"]);
    assert!(timeline.status.success());
    let timeline_reference = concat!(
        r#"{"errors":0,"first_error":null,"format":"claude-jsonl","kind":"oms-context-timeline","lines":2,"note":"Tool names, order, recorded timestamps and byte sizes only; arguments and outputs are not returned. `first_error` is the first result the provider flagged is_error, not a cause. Durations are the gap between the call row and its result row; null when not recorded. `repeat_of` names an earlier step with the same tool and identical arguments. Only Claude Code JSONL transcripts are read; any other file reports format unknown. Observation only, never verification or approval evidence.","pending":0,"schema":1,"steps":[{"argument_bytes":19,"duration_ms":1000,"error":false,"ok":true,"repeat_of":null,"result_bytes":2,"step":1,"tool":"Read"}],"steps_after_first_error":null,"steps_not_recorded":0,"steps_omitted":0,"steps_total":1,"unit":"utf-8 bytes","unmatched_results":0,"unread_lines":0}"#,
        "\n"
    );
    assert_eq!(timeline.stdout, timeline_reference.as_bytes());
    assert!(timeline.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&timeline.stdout).unwrap()["steps"][0]["duration_ms"],
        1000
    );
    let hooks = repo.join(".oms/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("events.jsonl"),
        b"{\"action\":\"injection\",\"status\":\"resume\",\"bytes\":7,\"session\":\"s1\"}\n",
    )
    .unwrap();
    let injections = call(&["--injections"]);
    assert!(injections.status.success());
    let injections_reference = concat!(
        r#"{"by_hook":{"items":[{"bytes":7,"calls":1,"hook":"resume","largest":7}],"omitted":0,"total":1},"by_session":{"items":[{"bytes":7,"calls":1,"largest":7,"session":"s1"}],"omitted":0,"total":1},"by_session_start_source":{"items":[{"bytes":7,"calls":1,"largest":7,"source":"unrecorded"}],"omitted":0,"total":1},"bytes":7,"injections":1,"invalid_rows":0,"kind":"oms-context-injections","ledger":".oms/hooks/events.jsonl","not_counted":["skill-router state hints","fail-ledger hook advice","syntax-guard hook findings","git-move-guard refusals","provider-native context (rules, skills, tool output)"],"note":"Bytes OMS hooks printed for the model, recorded without their text. Not tokens, not a bill and not the provider's context window; rows older than the hook ledger's retention are gone.","schema":1,"session":null,"unit":"utf-8 bytes"}"#,
        "\n"
    );
    assert_eq!(injections.stdout, injections_reference.as_bytes());
    assert!(injections.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&injections.stdout).unwrap()["injections"],
        1
    );
    let filtered = call(&["--injections", "--session-hash", "other"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&filtered.stdout).unwrap()["injections"],
        0
    );
    let pretty = call(&["--injections", "--pretty"]);
    assert!(pretty.status.success());
    assert!(pretty.stdout.ends_with(b"\n"));
    assert!(!pretty.stdout.ends_with(b"\n\n"));
    for args in [
        &["--injections", "--limit", "0"][..],
        &["--session", "session", "--session-hash", "x"],
        &["--injections", "--timeline"],
        &["--injections", "--format", "text"],
        &["--session", "missing"],
    ] {
        let out = call(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
    }
    for (args, message) in [
        (
            &["--injections", "--format", "text", "--pretty"][..],
            "--pretty applies only to JSON output",
        ),
        (
            &["--session", "session", "--compare", "missing"][..],
            "context-inspect takes one of:",
        ),
        (
            &["--injections", "--session-hash", "", "--limit", "100"][..],
            "--session-hash must be a recorded session hash",
        ),
        (
            &["--session", "", "--limit", "1"][..],
            "--session needs a transcript path",
        ),
    ] {
        let out = call(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let child = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--repo"])
        .arg(f.0.join("missing-repo"))
        .args(["context-inspect", "--session", "missing", "--timeline"])
        .env("OMS_HARNESS_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(child.status.code(), Some(2));
    assert!(child.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&child.stderr)
            .contains("a harness child may only use read-only runtime actions")
    );
    let row = br#"{"message":{"content":[{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"\udc80"}}]}}"#;
    std::fs::write(
        f.0.join("session"),
        [row.as_slice(), b"\n", row.as_slice()].concat(),
    )
    .unwrap();
    let out = call(&["--session", "session"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
#[test]
fn compare_current_text_and_denials() {
    let f = Fixture::new();
    let data = vectors()["cases"][0]["input"].clone();
    std::fs::write(f.0.join("pack.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let pretty = f.call(&["pack.json", "--pretty"], false);
    assert!(pretty.status.success());
    assert!(!pretty.stdout.ends_with(b"\n\n"));
    let value: Value = serde_json::from_slice(&pretty.stdout).unwrap();
    assert_eq!(value, vectors()["cases"][0]["expected"]);
    let out = f.call(&["pack.json", "--verify-current"], false);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["missing"], 1);
    std::fs::write(f.0.join("src.py"), vec![b'x'; 100]).unwrap();
    let out = f.call(&["pack.json", "--verify-current"], false);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["unchanged"], 1);
    std::fs::write(f.0.join("src.py"), b"changed").unwrap();
    let out = f.call(&["pack.json", "--verify-current"], false);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["changed"], 1);
    std::fs::remove_file(f.0.join("src.py")).unwrap();
    std::os::unix::fs::symlink("pack.json", f.0.join("src.py")).unwrap();
    let out = f.call(&["pack.json", "--verify-current"], false);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["unreadable"], 1);

    let out = f.call(
        &["pack.json", "--compare", "pack.json", "--format", "text"],
        true,
    );
    assert!(out.status.success());
    assert!(out.stdout.ends_with(b"\n"));
    let out = f.call(
        &["pack.json", "--compare", "pack.json", "--verify-current"],
        false,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let out = f.call(&["--session", "missing"], true);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("a harness child may only use read-only runtime actions")
    );
}

#[test]
fn sources_beyond_display_limit_and_path_safety() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let mut data = vectors()["cases"][0]["input"].clone();
    let mut other = data["selected"][0].clone();
    other["path"] = serde_json::json!("second.py");
    data["selected"].as_array_mut().unwrap().push(other);
    data["selected_bytes"] = serde_json::json!(240);
    data["remaining_bytes"] = serde_json::json!(760);
    std::fs::write(f.0.join("pack.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let out = f.call(&["pack.json", "--verify-current", "--limit", "1"], false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["missing"], 2);
    assert_eq!(v["sources_current"]["missing_paths"]["omitted"], 1);
    symlink("pack.json", f.0.join("link")).unwrap();
    let out = f.call(&["link"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    std::fs::hard_link(f.0.join("pack.json"), f.0.join("hard")).unwrap();
    let out = f.call(&["hard"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("context manifest must be an owned, bounded regular file without links")
    );
    std::fs::remove_file(f.0.join("hard")).unwrap();
    for path in ["link", "../pack.json", "missing"] {
        let out = f.call(&[path], false);
        assert_eq!(out.status.code(), Some(2), "{path}");
        assert!(out.stdout.is_empty());
    }
    for limit in ["0", "101", "bad", ""] {
        assert_eq!(
            f.call(&["pack.json", "--limit", limit], false)
                .status
                .code(),
            Some(2)
        );
    }
    assert!(
        f.call(&["pack.json", "--limit", "100"], false)
            .status
            .success()
    );
}

#[test]
fn help_and_parse_failures_precede_child_policy() {
    let f = Fixture::new();
    let root_help = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--help"])
        .output()
        .unwrap();
    assert!(root_help.status.success());
    assert!(root_help.stdout.starts_with(b"usage: oms runtime [-h]"));
    let empty_repo = Command::new(env!("CARGO_BIN_EXE_oms"))
        .current_dir(&f.0)
        .args(["runtime", "--repo", "", "context-inspect", "missing"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&empty_repo.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    std::fs::create_dir(f.0.join("nested")).unwrap();
    let tilde = Command::new(env!("CARGO_BIN_EXE_oms"))
        .env("HOME", &f.0)
        .args([
            "runtime",
            "--repo",
            "~//nested",
            "context-inspect",
            "missing",
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&tilde.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    let ambiguous = f.call(&["--=foo"], true);
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("ambiguous option: --=foo could match --help, --repo, --pretty")
    );
    let help = f.call(&["--help"], false);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(
        help.stdout
            .starts_with(b"usage: oms runtime context-inspect")
    );
    for args in [
        &["--session", "missing", "-x"][..],
        &["--session", "missing", "--limit", "++5"][..],
        &["--session", "missing", "--limit", "+-5"][..],
        &["--session", "missing", "--=foo"][..],
        &["--session", "missing", "--limit", "bad"][..],
        &["--session", "--pretty"][..],
        &["--format", "invalid"][..],
    ] {
        let out = f.call(args, true);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("error:"));
    }
}

#[test]
fn interrupt_reaps_blocked_git() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let fifo = f.0.join("git-config");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--repo"])
        .arg(&f.0)
        .args(["context-inspect", "missing"])
        .env("GIT_CONFIG_GLOBAL", &fifo)
        .env_remove("OMS_HARNESS_CHILD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let timeout = std::env::var("OMS_TEST_CALL_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut writer = loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&fifo)
        {
            break file;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("git did not open fixture");
        }
        std::thread::yield_now();
    };
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("interrupted process did not exit");
        }
        std::thread::yield_now();
    }
    use std::io::Write;
    assert_eq!(
        writer.write_all(b"x").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    drop(writer);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130));
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"runtime-core: interrupted\n");
}

#[test]
fn argv_boundaries_and_parse_precedence() {
    let f = Fixture::new();
    for name in ["--help", "-h", "--version", "--repo"] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(["runtime", "--", name])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("invalid choice"));
    }
    std::fs::write(
        f.0.join("-2026.json"),
        serde_json::to_vec(&vectors()["cases"][0]["input"]).unwrap(),
    )
    .unwrap();
    for child in [false, true] {
        for args in [
            &["missing", "--limit", "0"][..],
            &["./-2026.json", "--compare", "missing", "--limit", "0"][..],
        ] {
            let out = f.call(args, child);
            assert_eq!(out.status.code(), Some(2));
            assert!(out.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&out.stderr)
                    .contains("runtime-core: cannot read a valid repo-local context manifest\n")
            );
        }
    }
    let rejected = f.call(&["-2026.json"], false);
    assert!(rejected.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("unrecognized arguments: -2026.json")
    );
    let out = f.call(&["--session", "-1abc"], true);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("argument --session: expected one argument")
    );
    let out = f.call(&["--limit", "-.5", "pack.json"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid int value: '-.5'"));
    let out = f.call(&["-.5"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    for args in [
        &["runtime", "--unknown", "--help"][..],
        &["runtime", "--unknown", "--version"][..],
        &["runtime", "context-inspect", "--unknown", "--help"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
    }
    for args in [
        &["runtime", "--repo", "context-inspect"][..],
        &["runtime", "--repo", "--pretty", "context-inspect"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stderr.starts_with(b"usage: oms runtime [-h]"));
    }
}

#[test]
fn unreadable_repository_preserves_error_order() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let dir = f.0.join("sealed");
    std::fs::create_dir(&dir).unwrap();
    for mode in [0o000, 0o111] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let output = |args: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_oms"))
                .args(["runtime", "--repo"])
                .arg(&dir)
                .arg("context-inspect")
                .args(args)
                .output()
                .unwrap()
        };
        let unreadable = std::fs::read_dir(&dir).is_err();
        let pretty = output(&["missing", "--format", "text", "--pretty"]);
        let missing = output(&[]);
        let read = output(&["missing"]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            String::from_utf8_lossy(&pretty.stderr)
                .contains("--pretty applies only to JSON output")
        );
        assert!(String::from_utf8_lossy(&missing.stderr).contains("context-inspect takes one of:"));
        if unreadable {
            assert!(
                String::from_utf8_lossy(&read.stderr)
                    .contains("cannot read a valid repo-local context manifest")
            );
        }
    }
}

#[test]
fn git_output_larger_than_pipe_is_drained() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let source = f.0.join("fixture.rs");
    let fake = f.0.join("git");
    std::fs::write(
        &source,
        r#"use std::io::Write;
fn main() {
    let mut out = std::io::stdout().lock();
    out.write_all(&vec![b' '; 131072]).unwrap();
    out.write_all(std::env::var_os("OMS_FAKE_GIT_ROOT").unwrap().as_encoded_bytes()).unwrap();
    out.write_all(b"\r\n").unwrap();
}"#,
    )
    .unwrap();
    assert!(
        Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&fake)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        f.0.join("pack.json"),
        serde_json::to_vec(&vectors()["cases"][0]["input"]).unwrap(),
    )
    .unwrap();
    let nested = f.0.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--repo"])
        .arg(&nested)
        .args(["context-inspect", "pack.json"])
        .env("PATH", &f.0)
        .env("OMS_FAKE_GIT_ROOT", &f.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let timeout = std::env::var("OMS_TEST_CALL_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI did not drain Git output");
        }
        std::thread::yield_now();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        vectors()["cases"][0]["expected"]
    );
}

#[test]
fn help_on_closed_stdout_exits_without_panic() {
    // Other parallel tests spawn processes; isolate the pipe so none can inherit its reader.
    if std::env::var_os("OMS_TEST_CLOSED_PIPE_ISOLATED").is_none() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "help_on_closed_stdout_exits_without_panic"])
            .env("OMS_TEST_CLOSED_PIPE_ISOLATED", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    for args in [
        &["runtime", "--help"][..],
        &["runtime", "--version"][..],
        &["runtime", "context-inspect", "--help"][..],
    ] {
        let (reader, writer) = nix::unistd::pipe().unwrap();
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .stdout(std::process::Stdio::from(writer))
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
    }
}

#[test]
fn unicode_limits_and_help_forms() {
    let f = Fixture::new();
    std::fs::write(
        f.0.join("pack.json"),
        serde_json::to_vec(&vectors()["cases"][0]["input"]).unwrap(),
    )
    .unwrap();
    for limit in ["١٠", "０１", "+१२", "1_٠"] {
        let out = f.call(&["pack.json", "--limit", limit], false);
        assert!(
            out.status.success(),
            "{limit}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = f.call(&["--limit", limit, "--help"], false);
        assert!(out.status.success(), "{limit}");
    }
    let mut data = vectors()["cases"][0]["input"].clone();
    let mut second = data["selected"][0].clone();
    second["path"] = serde_json::json!("second.py");
    data["selected"].as_array_mut().unwrap().push(second);
    data["selected_bytes"] = serde_json::json!(240);
    data["remaining_bytes"] = serde_json::json!(760);
    std::fs::write(f.0.join("pack.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let out = f.call(&["pack.json", "--limit", "０１"], false);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["sources"]["omitted"], 1);
    for malformed in ["", "١__٢", "\u{1c}1"] {
        let out = f.call(&["--limit", malformed, "--help"], false);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    for token in ["-١", "-.٥"] {
        let out = f.call(&["--session", token], true);
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("a harness child may only use read-only runtime actions")
        );
    }
    let out = f.call(&["--session", "-١x"], true);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("argument --session: expected one argument")
    );
    for token in ["-hh", "-hhh", "-hfoo"] {
        assert!(f.call(&[token], false).status.success(), "{token}");
    }
    for token in ["-h=foo", "-h--"] {
        let out = f.call(&[token], false);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("argument -h/--help: ignored explicit argument")
        );
    }
    for args in [
        &["runtime", "--pretty=foo"][..],
        &["runtime", "--version=foo"][..],
        &["runtime", "context-inspect", "--help=foo"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("ignored explicit argument 'foo'"));
    }
}
