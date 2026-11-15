#![forbid(unsafe_code)]
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
fn clean_env_child(command: &mut Command) -> &mut Command {
    command.env_clear();
    for name in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
}
const CHILD_ENV: &str = "CONTRACT_CHILD";
fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        body();
        return;
    }
    let home = Repo::new();
    let mut command = Command::new(std::env::current_exe().unwrap());
    let output = clean_env_child(&mut command)
        .args(["--exact", name, "--nocapture"])
        .env("HOME", &home.0)
        .env(CHILD_ENV, name)
        .output()
        .unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{out}");
    assert!(out.contains("1 passed;"), "child ran no test: {out}");
}
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "oms-contract-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let repo = Self(path.canonicalize().unwrap());
        repo.git(&["init", "-q"]);
        repo
    }
    fn git(&self, args: &[&str]) {
        let mut command = Command::new("git");
        let output = clean_env_child(&mut command)
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn write(&self, name: &str, body: impl AsRef<[u8]>) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
static STOP: AtomicBool = AtomicBool::new(false);
use oms_command_peer::{Output, V, owns, run_with_source};
fn run(repo: &Repo, source: &Path, tool: &str, values: &[&str]) -> Output {
    run_with_source(tool, &args(values), &repo.0, source, &STOP)
}
fn success(output: Output) -> Vec<u8> {
    assert_eq!(output.0, 0, "{}", String::from_utf8_lossy(&output.2));
    output.1
}
#[test]
fn ownership_and_help() {
    isolated("ownership_and_help", || {
        let repo = Repo::new();
        for tool in oms_command_peer::TOOLS {
            assert!(owns(tool, &args(&["--malformed"])));
            assert!(!success(run(&repo, &repo.0, tool, &["--help"])).is_empty());
        }
        assert!(!owns("room", &[]));
        assert_eq!(run(&repo, &repo.0, "unknown", &[]), (75, vec![], vec![]));
    });
}
#[test]
fn view_embedded_and_installed_assets() {
    isolated("view_embedded_and_installed_assets", || {
        let repo = Repo::new();
        let source = Repo::new();
        repo.write(
            "results.json",
            br#"{"schema":1,"kind":"oms-study-results","rows":[]}"#,
        );
        let build = |output: &str| {
            run(
                &repo,
                &source.0,
                "artifact-view",
                &[
                    "build",
                    "--input",
                    "results.json",
                    "--output",
                    output,
                    "--title",
                    "A < B",
                ],
            )
        };
        let meta = V::parse(&success(build("embedded.html"))).unwrap();
        assert_eq!(meta.get("path").text(), Some("embedded.html"));
        let html = fs::read_to_string(repo.0.join("embedded.html")).unwrap();
        assert!(html.contains("A &lt; B"));
        for asset in [
            include_str!("../assets/analysis.js"),
            include_str!("../assets/view.js"),
            include_str!("../assets/view.css"),
        ] {
            assert!(html.contains(asset));
        }
        for (name, body) in [
            ("analysis.js", include_str!("../assets/analysis.js")),
            ("view.js", include_str!("../assets/view.js")),
            ("view.css", include_str!("../assets/view.css")),
            ("view.html", include_str!("../assets/view.html")),
        ] {
            oms_command_peer::safety::outbound(body.as_bytes()).unwrap();
            source.write(&format!("scripts/lib/artifact_view_assets/{name}"), body);
        }
        success(build("installed.html"));
        assert_eq!(
            fs::read_to_string(repo.0.join("installed.html")).unwrap(),
            html
        );
        fs::remove_dir_all(source.0.join("scripts/lib/artifact_view_assets")).unwrap();
        source.write(
            "scripts/lib/artifact_view_assets/view.css",
            "/* installed override */",
        );
        success(build("mixed.html"));
        assert!(
            fs::read_to_string(repo.0.join("mixed.html"))
                .unwrap()
                .contains("/* installed override */")
        );
        source.write("scripts/lib/artifact_view_assets/view.css", "");
        success(build("empty.html"));
        assert!(
            !fs::read_to_string(repo.0.join("empty.html"))
                .unwrap()
                .contains(include_str!("../assets/view.css"))
        );
        source.write("scripts/lib/artifact_view_assets/view.css", [255]);
        assert_eq!(build("invalid.html").0, 2);
        assert!(!repo.0.join("invalid.html").exists());
        fs::remove_file(source.0.join("scripts/lib/artifact_view_assets/view.css")).unwrap();
        std::os::unix::fs::symlink(
            source.0.join("missing"),
            source.0.join("scripts/lib/artifact_view_assets/view.css"),
        )
        .unwrap();
        assert_eq!(build("linked.html").0, 2);
        assert!(!repo.0.join("linked.html").exists());
        fs::remove_dir_all(source.0.join("scripts/lib/artifact_view_assets")).unwrap();
        std::os::unix::fs::symlink(
            source.0.join("absent"),
            source.0.join("scripts/lib/artifact_view_assets"),
        )
        .unwrap();
        assert_eq!(build("ancestor.html").0, 2);
        assert!(!repo.0.join("ancestor.html").exists());
    });
}
#[test]
fn peer_dry_runs_and_ml_context() {
    isolated("peer_dry_runs_and_ml_context", || {
        let repo = Repo::new();
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ]);
        for tool in ["peer-ask", "peer-delegate"] {
            let values = [
                if tool == "peer-ask" {
                    "--providers"
                } else {
                    "--to"
                },
                "codex",
                "--model",
                "fixture",
                "--dry-run",
                "--no-verify",
                "Inspect behavior",
            ];
            success(run(&repo, &repo.0, tool, &values));
            assert_eq!(run(&repo, &repo.0, tool, &["--to"]).0, 2);
        }
        repo.write("train.py", "import torch\n");
        success(run(
            &repo,
            &repo.0,
            "peer-ask",
            &[
                "--providers",
                "codex",
                "--model",
                "fixture",
                "--dry-run",
                "--ml-context",
                "Inspect training",
            ],
        ));
        let prompts = fs::read_dir(repo.0.join(".oms/artifacts/ask"))
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(prompts.contains("ML Agent Context Digest"));
        assert!(prompts.contains("train.py"));
        for (option, message) in [
            ("--apply", "patch-admission"),
            ("--graph-context", "context compilation"),
            ("--plan-task", "plan lease"),
        ] {
            let mut values = vec![option];
            if option == "--plan-task" {
                values.push("T1");
            }
            let output = run(&repo, &repo.0, "peer-delegate", &values);
            assert_eq!(output.0, 2);
            assert!(String::from_utf8_lossy(&output.2).contains(message));
        }
    });
}
#[test]
fn review_classification_and_validation() {
    isolated("review_classification_and_validation", || {
        let repo = Repo::new();
        let artifact = "review/codex-20260101T000000Z-1.md";
        for (answer, exit, expected, state) in [
            ("GATE: pass", "0", 0, "pass"),
            ("GATE: fail", "0", 1, "fail"),
            ("GATE: pass", "1", 2, "incomplete"),
            ("", "0", 2, "incomplete"),
            ("GATE: pass", "bad", 2, "incomplete"),
        ] {
            repo.write(artifact, format!("## Output\n{answer}\n## Exit\n{exit}\n"));
            let output = run(
                &repo,
                &repo.0,
                "peer-review",
                &["verdicts", "review", "--json"],
            );
            assert_eq!(output.0, expected);
            let value = V::parse(&output.1).unwrap();
            assert_eq!(
                value.get("seats").array()[0].get("verdict").text(),
                Some(state)
            );
        }
        for option in ["--verify", "--covers", "--confirm", "--security-audit"] {
            let values = [option, "fixture"];
            let output = run(&repo, &repo.0, "peer-review", &values);
            assert_eq!(output.0, 2);
            assert!(String::from_utf8_lossy(&output.2).contains("requires --gate"));
        }
    });
}
#[test]
fn unavailable_services_refuse() {
    use oms_runtime::Services;
    let services = oms_runtime::services::Unavailable;
    assert!(services.repository_view(Path::new(".")).is_err());
    assert!(
        services
            .task_status(Path::new("."), Path::new("."), b"")
            .is_err()
    );
    assert!(
        services
            .plan_status(Path::new("."), Path::new("."), b"")
            .is_err()
    );
}
#[test]
fn review_depth_preserves_risk_globs() {
    isolated("review_depth_preserves_risk_globs", || {
        let repo = Repo::new();
        let source = Repo::new();
        source.write("config/review-depth.json", br#"{"schema":1,"none_lines":10,"gate_lines":200,"gate_files":10,"council_lines":800,"council_files":30,"contract_paths":[]}"#);
        for path in [
            "hooks/pre-commit",
            "plugins/fixture/hooks.json",
            "scripts/lib/hook_state.py",
            "rust/crates/oms-command-room/src/lib.rs",
            "hooks.json",
        ] {
            repo.write(path, "baseline\n");
        }
        repo.git(&["add", "."]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        let advice = || {
            V::parse(&success(run(
                &repo,
                &source.0,
                "peer-review",
                &["advise", "--json"],
            )))
            .unwrap()
        };
        assert_eq!(advice().get("depth").text(), Some("none"));
        for (path, risk) in [
            ("hooks/pre-commit", true),
            ("plugins/fixture/hooks.json", true),
            ("scripts/lib/hook_state.py", true),
            ("rust/crates/oms-command-room/src/lib.rs", false),
            ("hooks.json", false),
        ] {
            repo.write(path, "changed\n");
            let value = advice();
            assert_eq!(
                value.get("depth").text(),
                Some(if risk { "gate+security" } else { "none" }),
                "{path}"
            );
            assert_eq!(!value.get("risk_paths").array().is_empty(), risk, "{path}");
            repo.write(path, "baseline\n");
        }
    });
}

// Linux exposes bytes actually read, so the hot-path check needs no timing
// threshold. Other supported hosts still exercise the same output contracts.
fn read_bytes() -> Option<u64> {
    fs::read_to_string("/proc/thread-self/io")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("rchar: ")?.trim().parse().ok())
}

#[test]
fn thread_hot_paths_read_each_log_once_and_keep_stale_contracts() {
    isolated(
        "thread_hot_paths_read_each_log_once_and_keep_stale_contracts",
        || {
            use oms_command_peer::{obj, providers::answer_quality, storage};
            use std::os::unix::fs::MetadataExt;
            let repo = Repo::new();
            let source = Repo::new();
            let row = |role: &str, body: &str, provider: &str| {
                obj([
                    ("thread", "history".into()),
                    ("role", role.into()),
                    ("text", body.into()),
                    ("ts", "2000-01-01T00:00:00Z".into()),
                    ("provider", provider.into()),
                    ("quality", "ok".into()),
                ])
            };
            let mut rows = vec![row("question", "first question", "codex")];
            rows.push(row("topic", "  chosen\n topic  ", ""));
            for i in 0..64 {
                rows.push(row(
                    if i == 0 { "question" } else { "answer" },
                    &"x".repeat(4096),
                    if i % 2 == 0 { "claude" } else { "codex" },
                ));
            }
            let mut bytes = Vec::new();
            for row in &rows {
                bytes.extend(row.json(false).unwrap());
                bytes.push(b'\n');
            }
            // Calibrate host /proc/*/stat scan overhead on an empty thread listing
            // so hosts with hundreds of processes do not masquerade as extra log reads.
            let base = {
                let b0 = read_bytes();
                let _ = run(&repo, &source.0, "thread", &["list", "--json"]);
                b0.zip(read_bytes())
                    .map_or(0, |(start, end)| end.saturating_sub(start))
            };
            repo.write(".oms/threads/history.jsonl", &bytes);
            let minted = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            repo.write(".oms/threads/CURRENT", format!("history {minted}\n"));
            for action in ["list", "stats", "show"] {
                let before = read_bytes();
                let output = success(run(&repo, &source.0, "thread", &[action, "--json"]));
                if let (Some(before), Some(after)) = (before, read_bytes()) {
                    assert!(
                        after - before < bytes.len() as u64 + base + 32768,
                        "{action} read {} bytes for a {} byte log (baseline {base})",
                        after - before,
                        bytes.len()
                    );
                }
                let value = V::parse(&output).unwrap();
                match action {
                    "list" => {
                        let entry = &value.get("threads").array()[0];
                        assert_eq!(entry.get("topic").text(), Some("chosen topic"));
                        assert_eq!(
                            entry.get("providers"),
                            &V::Arr(vec!["codex".into(), "claude".into()])
                        );
                        assert_eq!(entry.get("staleness").text(), Some("current"));
                    }
                    "stats" => {
                        for key in ["threads", "open", "second_question", "multi_provider"] {
                            assert_eq!(value.get("stats").get(key).py(), "1");
                        }
                        assert_eq!(value.get("stats").get("answers_ok").py(), "63");
                    }
                    "show" => assert_eq!(value, V::Arr(rows.clone())),
                    _ => unreachable!(),
                }
            }
            fs::remove_file(repo.0.join(".oms/threads/CURRENT")).unwrap();
            for ttl in ["0", "9223372036854775807"] {
                let output = V::parse(&success(run(
                    &repo,
                    &source.0,
                    "thread",
                    &["stats", "--json", "--stale-ttl", ttl],
                )))
                .unwrap();
                assert_eq!(output.get("stale_ttl_seconds").py(), ttl);
                assert_eq!(
                    output.get("stats").get("stale_open").py(),
                    if ttl == "0" { "1" } else { "0" }
                );
            }
            for ttl in ["", "-1", "bad", "9223372036854775808"] {
                for action in ["list", "stats", "close"] {
                    let mut argv = vec![action, "--stale-ttl", ttl];
                    if action == "close" {
                        argv.push("--stale");
                    }
                    assert_eq!(run(&repo, &source.0, "thread", &argv).0, 2, "{argv:?}");
                }
            }
            assert_eq!(
                run(&repo, &source.0, "thread", &["close", "--stale-ttl", "0"]).0,
                2
            );
            let room = row("note", "room", "");
            let mut room = room;
            room.set("thread", "room-0123456789ab");
            repo.write(
                ".oms/threads/room-0123456789ab.jsonl",
                format!(
                    "{}\n",
                    String::from_utf8(room.json(false).unwrap()).unwrap()
                ),
            );
            let mut event = row("note", "room event", "");
            event.set("thread", "event-room");
            event.set("room_event", obj([("kind", "created".into())]));
            repo.write(
                ".oms/threads/event-room.jsonl",
                format!(
                    "{}\n",
                    String::from_utf8(event.json(false).unwrap()).unwrap()
                ),
            );
            success(run(
                &repo,
                &source.0,
                "thread",
                &["close", "--stale", "--stale-ttl", "0", "--apply"],
            ));
            for name in ["room-0123456789ab", "event-room"] {
                let value = V::parse(&success(run(
                    &repo,
                    &source.0,
                    "thread",
                    &["show", "--id", name, "--json"],
                )))
                .unwrap();
                assert_eq!(value.array().len(), 1, "room remains open");
            }
            let value = V::parse(&success(run(
                &repo,
                &source.0,
                "thread",
                &["show", "--id", "history", "--json"],
            )))
            .unwrap();
            assert_eq!(
                value.array().last().unwrap().get("role").text(),
                Some("closed")
            );

            let records = vec![
                obj([("id", "b".into()), ("n", 1usize.into())]),
                obj([("id", "a".into())]),
                obj([("id", "b".into()), ("n", 2usize.into())]),
            ];
            assert_eq!(
                storage::latest(&records).unwrap(),
                vec![records[2].clone(), records[1].clone()]
            );
            assert!(storage::latest(&[]).unwrap().is_empty());
            assert!(storage::latest(&[obj([])]).is_err());
            for (raw, expected) in [
                ("", "empty"),
                ("Answer:\nVerification:\r\n", "empty"),
                ("a substantive response", "ok"),
                (
                    "stop-reason: provider=codex reason=refusal subtype=test is_error=0",
                    "blocked",
                ),
                (
                    "[agy] print timeout after 5s with turn in progress; returning partial output",
                    "truncated",
                ),
                (
                    "stop-reason: provider=codex reason=stream_truncated subtype=test is_error=0",
                    "truncated",
                ),
                (
                    "stop-reason: provider=codex reason=refusal subtype=test is_error=0\n## Output\nanswer",
                    "ok",
                ),
                (
                    "## Output\r\nstop-reason: provider=codex reason=refusal subtype=test is_error=1",
                    "blocked",
                ),
                ("served model\nvalue\nconfigured model\nvalue", "empty"),
                ("jetski: no output produced", "blocked"),
                ("jetski: no output produced\nanswer", "ok"),
            ] {
                assert_eq!(answer_quality(raw), expected);
                assert_eq!(answer_quality(raw), expected, "cached expression parity");
            }
            repo.write(
                "review/_findings-first.jsonl",
                concat!(
                    "{\"kind\":\"review-finding\",\"id\":\"a\",\"run\":\"r1\"}\n",
                    "{\"kind\":\"review-finding\",\"id\":\"b\",\"run\":\"r1\"}\n",
                    "{\"kind\":\"review-finding\",\"id\":\"b\",\"run\":\"r2\"}\n",
                ),
            );
            repo.write("review/_finding-events.jsonl", concat!(
                "{\"kind\":\"review-finding-event\",\"id\":\"a\",\"state\":\"open\"}\n",
                "{\"kind\":\"review-finding-confirmation\",\"id\":\"a\",\"result\":\"old\"}\n",
                "{\"kind\":\"review-finding-event\",\"id\":\"b\",\"state\":\"fixed\",\"runs\":[\"r1\"]}\n",
                "{\"kind\":\"review-finding-event\",\"id\":\"a\",\"state\":\"fixed\",\"runs\":[\"r1\"]}\n",
                "{\"kind\":\"review-finding-confirmation\",\"id\":\"a\",\"result\":\"new\"}\n",
                "{\"kind\":\"review-finding-event\",\"id\":\"absent\",\"state\":\"fixed\"}\n",
            ));
            let findings = V::parse(&success(run(
                &repo,
                &source.0,
                "peer-review",
                &["findings", "list", "--artifact-dir", "review", "--json"],
            )))
            .unwrap();
            assert_eq!(findings.array().len(), 2);
            assert_eq!(findings.array()[0].get("status").text(), Some("fixed"));
            assert_eq!(findings.array()[0].get("history").array().len(), 2);
            assert_eq!(
                findings.array()[0].get("confirmation").get("result").text(),
                Some("new")
            );
            assert_eq!(findings.array()[1].get("status").text(), Some("open"));
            storage::write(
                &repo.0,
                Path::new("same.json"),
                b"{}\n".to_vec(),
                true,
                &STOP,
            )
            .unwrap();
            let first = fs::metadata(repo.0.join("same.json")).unwrap();
            storage::write(
                &repo.0,
                Path::new("same.json"),
                b"{}\n".to_vec(),
                true,
                &STOP,
            )
            .unwrap();
            let second = fs::metadata(repo.0.join("same.json")).unwrap();
            assert_eq!(
                (first.ino(), first.mtime(), first.mtime_nsec()),
                (second.ino(), second.mtime(), second.mtime_nsec())
            );
        },
    );
}
