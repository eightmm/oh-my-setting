#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{seq}-{nanos}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn stdin_injection_supports_append_and_review_capture() {
    let dir = TempDir::new("oms-memory-stdin-test");
    let repo = &dir.0;
    let init = Command::new("git")
        .arg("init")
        .arg(repo)
        .output()
        .expect("git init");
    assert!(init.status.success());
    std::fs::create_dir_all(repo.join(".oms")).unwrap();

    let interrupted = AtomicBool::new(false);
    let args = |items: &[&str]| -> Vec<OsString> { items.iter().map(OsString::from).collect() };

    let (code, _, err) =
        oms_command_memory::run_with_source(&args(&["init"]), repo, repo, &interrupted);
    assert_eq!(code, 0, "init failed: {}", String::from_utf8_lossy(&err));

    let payload = b"Prefer injected bytes over process stdin\n";
    let (code, _, err) = oms_command_memory::run_with_source_and_stdin(
        &args(&["append", "--stdin", "--agent", "codex"]),
        repo,
        repo,
        payload,
        &interrupted,
    );
    assert_eq!(
        code,
        0,
        "append --stdin failed: {}",
        String::from_utf8_lossy(&err)
    );

    let (code, out, err) =
        oms_command_memory::run_with_source(&args(&["show"]), repo, repo, &interrupted);
    assert_eq!(code, 0, "show failed: {}", String::from_utf8_lossy(&err));
    let shown = String::from_utf8_lossy(&out);
    assert!(shown.contains("Prefer injected bytes over process stdin"));

    // Exceeding capture 64 KiB cap stays silent exit 0 without blocking on stdin.
    let oversized = vec![b'x'; 65_537];
    let (code, out, err) = oms_command_memory::run_review_with_stdin(
        &args(&["capture", "--repo", repo.to_str().unwrap()]),
        repo,
        repo,
        &oversized,
        &interrupted,
    );
    assert_eq!(code, 0, "capture stderr: {}", String::from_utf8_lossy(&err));
    assert!(out.is_empty());
}

#[test]
fn review_controls_context_without_hiding_source_notes() {
    if std::env::var_os("OMS_MEMORY_CONTEXT_TEST").is_none() {
        let isolated = TempDir::new("oms-memory-context-env");
        for mode in ["compact", "full", "relevant"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "review_controls_context_without_hiding_source_notes",
                "--nocapture",
            ]);
            for (key, _) in std::env::vars_os() {
                if key.to_str().is_some_and(|key| key.starts_with("OMS_")) {
                    command.env_remove(key);
                }
            }
            let result = command
                .env("OMS_MEMORY_CONTEXT_TEST", "1")
                .env("OMS_AGENT_MEMORY_MODE", mode)
                .env("OH_MY_SETTING_GLOBAL_MEMORY", isolated.0.join("global.md"))
                .env("OMS_LOCK_DIR", isolated.0.join("locks"))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{mode}: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        return;
    }

    let dir = TempDir::new("oms-memory-context-test");
    let repo = &dir.0;
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(repo)
            .status()
            .unwrap()
            .success()
    );
    let interrupted = AtomicBool::new(false);
    let args = |items: &[&str]| -> Vec<OsString> { items.iter().map(OsString::from).collect() };
    let output = |result: oms_command_memory::Output| {
        let (code, out, err) = result;
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        String::from_utf8(out).unwrap()
    };
    let run = |items: &[&str]| {
        output(oms_command_memory::run_with_source(
            &args(items),
            repo,
            repo,
            &interrupted,
        ))
    };
    run(&["init"]);
    let shared = repo.join(".oms/memory/shared.md");
    // Existing metadata-free records remain distinct before native appends.
    let mut legacy = std::fs::read(&shared).unwrap();
    legacy.extend_from_slice(b"\n## 2025-01-01T00:00:00Z agent\n\nlegacyframe first note\n\n## 2025-01-02T00:00:00Z agent\n\nlegacyframe second note\n");
    std::fs::write(&shared, legacy).unwrap();
    run(&["append", "--text", "curationtopic manual note"]);
    run(&["pin", "--text", "curationtopic explicit pin"]);
    let legacy = run(&["search", "legacyframe", "--json"]);
    let legacy: Vec<serde_json::Value> = legacy
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(legacy.len(), 2);
    assert!(legacy.iter().all(|entry| entry["kind"] == "note"
        && entry["event_id"].as_str().unwrap().starts_with("legacy:")));
    let automatic = [
        ("task_close", "agent", "curationtopic task proposal"),
        ("distill", "journal", "curationtopic distill proposal"),
        (
            "imported",
            "agent",
            "## 2026-01-01T00:00:00 reviewer curationtopic imported proposal",
        ),
        (
            "note",
            "journal",
            "journal-distill: curationtopic legacy proposal",
        ),
        ("lesson", "agent", "curationtopic interrupted correction"),
    ];
    let ids: Vec<_> = automatic
        .iter()
        .map(|(kind, agent, body)| {
            if *kind == "imported" {
                let source = repo.join("import.md");
                std::fs::write(&source, format!("- {body}\n")).unwrap();
                run(&[
                    "import",
                    "--from",
                    source.to_str().unwrap(),
                    "--apply",
                    "--all",
                ]);
                let context = run(&["context"]);
                assert!(
                    !context.contains("curationtopic imported proposal"),
                    "pending public import leaked: {context}"
                );
                let pending: serde_json::Value =
                    serde_json::from_str(&run(&["review", "--kind", "imported", "--json"]))
                        .unwrap();
                assert_eq!(pending["pending"].as_array().unwrap().len(), 1);
                let id = pending["pending"][0]["id"].as_str().unwrap().to_owned();
                // Reproduce the old parser's index without changing its source stamp.
                let db = rusqlite::Connection::open(repo.join(".oms/memory/memory.sqlite3")).unwrap();
                let (ordinal, recorded, author, body): (i64, String, String, String) = db.query_row(
                    "select ordinal,occurred_at,agent,body from memory_entries where event_id=?",
                    [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                ).unwrap();
                let (heading, tail) = body.split_once('\n').unwrap();
                let (when, agent) = heading.strip_prefix("## ").unwrap().split_once(' ').unwrap();
                let rendered = format!("{heading}\n\n{}", tail.trim());
                use sha2::Digest;
                let orphan = format!("legacy:shared:{}:{}", ordinal + 1, &format!("{:x}", sha2::Sha256::digest(&rendered))[..16]);
                db.execute("update memory_entries set body='',rendered=? where event_id=?", rusqlite::params![format!("## {recorded} {author}"), id]).unwrap();
                db.execute("insert into memory_entries (event_id,source,ordinal,occurred_at,agent,kind,task_id,session_hash,git_sha,git_dirty,git_state,body,rendered) values (?,'shared',?,?,?,'note','','','',null,'',?,?)", rusqlite::params![orphan, ordinal + 1, when, agent, tail.trim(), rendered]).unwrap();
                let context = output(oms_command_memory::provider_context(repo, "curationtopic", &interrupted));
                assert!(!context.contains("curationtopic imported proposal"), "old import index leaked: {context}");
                assert_eq!(db.query_row("select count(*) from memory_entries where event_id=?", [&orphan], |r| r.get::<_, i64>(0)).unwrap(), 0);
                let before = std::fs::read(&shared).unwrap();
                run(&["review", "--accept", &id]);
                let context = output(oms_command_memory::provider_context(repo, "curationtopic", &interrupted));
                let recall = context.split("### relevant recall (project)\n").nth(1).unwrap_or("");
                assert!(recall.contains("curationtopic imported proposal"), "accepted import missing from repaired recall: {context}");
                assert_eq!(std::fs::read(&shared).unwrap(), before);
                return id;
            }
            if *kind == "distill" {
                let result = oms_command_memory::append_journal_batch(
                    repo,
                    &"b".repeat(64),
                    &[oms_command_memory::JournalProposal {
                        key: &"c".repeat(64),
                        source_sha256: &"d".repeat(64),
                        body,
                        source_current: true,
                    }],
                    &interrupted,
                ).unwrap();
                let oms_command_memory::JournalPublication::Appended(id) = &result.outcomes[0] else {
                    panic!("journal proposal was not published: {:?}", result.outcomes);
                };
                return id.clone();
            }
            output(oms_command_memory::append_project_note(
                repo,
                agent,
                kind,
                body,
                &interrupted,
            ))
            .trim()
            .to_owned()
        })
        .collect();
    let summary = repo.join(".oms/memory/summary.md");
    // A crash can leave the correction note before its acceptance receipt.
    let source = std::fs::read_to_string(&shared).unwrap().replace(
        &format!("event_id: {}\n", ids[4]),
        &format!(
            "accepted_correction: pend-interrupted\nevent_id: {}\n",
            ids[4]
        ),
    );
    std::fs::write(&shared, source).unwrap();
    std::fs::write(
        repo.join(".oms/memory/pending.jsonl"),
        b"{\"schema\":1,\"id\":\"pend-interrupted\",\"text\":\"curationtopic interrupted correction\"}\n",
    ).unwrap();
    std::fs::write(
        repo.join(".oms/memory/reviews.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"schema": 1, "id": ids[2], "decision": "rejected"})
        ),
    )
    .unwrap();
    run(&["compact"]);
    let original = std::fs::read(&shared).unwrap();
    let pending: serde_json::Value = serde_json::from_str(&run(&["review", "--json"])).unwrap();
    assert_eq!(
        pending["pending"].as_array().unwrap().len(),
        automatic.len() - 1
    );
    let attention = oms_command_memory::pending_review(repo, &interrupted).unwrap();
    assert_eq!(
        attention.pending,
        pending["pending"].as_array().unwrap().len()
    );
    assert_eq!(attention.skipped, 0);
    assert_eq!(attention.candidate_fingerprints.len(), attention.pending);
    // One bad receipt line among good ones skips and counts the bad line without hiding good rows.
    std::fs::write(
        repo.join(".oms/memory/reviews.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"schema": 1, "id": ids[2], "decision": "rejected"}),
            serde_json::json!({"schema": 2, "id": "missing-schema-1", "decision": "accepted"}),
        ),
    )
    .unwrap();
    let degraded_attention = oms_command_memory::pending_review(repo, &interrupted).unwrap();
    assert_eq!(degraded_attention.pending, attention.pending);
    assert_eq!(degraded_attention.skipped, 1);
    let degraded_json: serde_json::Value =
        serde_json::from_str(&run(&["review", "--json"])).unwrap();
    assert_eq!(
        degraded_json["pending"].as_array().unwrap().len(),
        attention.pending
    );
    assert_eq!(degraded_json["skipped"], 1);
    let degraded_text = run(&["review"]);
    assert!(
        degraded_text.contains("1 invalid row(s) skipped"),
        "{degraded_text}"
    );
    assert!(
        degraded_text.contains("curationtopic task proposal"),
        "{degraded_text}"
    );
    std::fs::write(
        repo.join(".oms/memory/reviews.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"schema": 1, "id": ids[2], "decision": "rejected"})
        ),
    )
    .unwrap();
    let database = repo.join(".oms/memory/memory.sqlite3");
    let before_resume = [
        std::fs::read(&database).unwrap(),
        std::fs::read(&summary).unwrap(),
    ];
    let resumed = output(oms_command_memory::resume_context(
        repo,
        "task",
        &interrupted,
    ));
    assert!(resumed.contains("curationtopic explicit pin"), "{resumed}");
    assert!(
        !resumed.contains("curationtopic task proposal"),
        "{resumed}"
    );
    assert_eq!(
        before_resume,
        [
            std::fs::read(&database).unwrap(),
            std::fs::read(&summary).unwrap()
        ]
    );
    for command in ["search", "recall"] {
        let audit = run(&[command, "curationtopic"]);
        for (_, _, body) in automatic {
            assert!(audit.contains(body), "{command}: {audit}");
        }
    }
    let assert_context = |accepted: &[usize]| {
        let mut views = vec![
            run(&["context"]),
            run(&["context", "--full"]),
            output(oms_command_memory::provider_context(
                repo,
                "curationtopic",
                &interrupted,
            )),
            std::fs::read_to_string(&summary).unwrap(),
        ];
        for view in ["summary", "full", "pins"] {
            views.push(output(oms_command_memory::run_database(
                &args(&[
                    "context",
                    "--repo",
                    repo.to_str().unwrap(),
                    "--db",
                    repo.join(".oms/memory/memory.sqlite3").to_str().unwrap(),
                    "--shared",
                    shared.to_str().unwrap(),
                    "--pins",
                    repo.join(".oms/memory/pins.md").to_str().unwrap(),
                    "--view",
                    view,
                ]),
                repo,
                &interrupted,
            )));
        }
        for (index, text) in views.iter().enumerate() {
            assert!(
                text.contains(if index == 6 {
                    "curationtopic explicit pin"
                } else {
                    "curationtopic manual note"
                }),
                "view {index}: {text}"
            );
            for (i, (_, _, body)) in automatic.iter().enumerate() {
                assert_eq!(
                    text.contains(body),
                    index != 6 && accepted.contains(&i),
                    "view {index}: {body}: {text}"
                );
            }
        }
    };
    assert_context(&[]);
    run(&["review", "--accept", &ids[0]]);
    run(&["review", "--accept", &ids[3]]);
    run(&["review", "--accept", "pend-interrupted"]);
    assert_eq!(std::fs::read(&shared).unwrap(), original);
    assert_context(&[0, 3, 4]);
    let resumed = output(oms_command_memory::resume_context(
        repo,
        "task",
        &interrupted,
    ));
    assert!(resumed.contains("curationtopic task proposal"), "{resumed}");
    assert!(resumed.contains(&ids[0]), "{resumed}");
    assert_eq!(
        oms_command_memory::pending_review(repo, &interrupted)
            .unwrap()
            .pending,
        1
    );
    run(&["review", "--retire", &ids[1], "--reason", "not reusable"]);
    run(&["retire", "--id", &ids[0], "--reason", "outdated"]);
    assert_context(&[3, 4]);
    assert_eq!(
        oms_command_memory::pending_review(repo, &interrupted)
            .unwrap()
            .pending,
        0
    );
    let resumed = output(oms_command_memory::resume_context(
        repo,
        "task",
        &interrupted,
    ));
    assert!(
        !resumed.contains("curationtopic task proposal"),
        "{resumed}"
    );
    assert!(std::fs::read(&shared).unwrap().starts_with(&original));
    let audit = run(&["search", "curationtopic", "--include-superseded"]);
    for (_, _, body) in automatic {
        assert!(audit.contains(body), "{audit}");
    }
    std::fs::write(
        repo.join(".oms/failures.jsonl"),
        b"{\"fingerprint\":\"memory-source-test\",\"ts\":\"2026-01-01T00:00:00Z\",\"agent\":\"journal\",\"summary\":\"journal-distill: failureonly remains visible\",\"exit\":1}\n",
    ).unwrap();
    let context = output(oms_command_memory::provider_context(
        repo,
        "failureonly",
        &interrupted,
    ));
    assert!(
        context.contains("journal-distill: failureonly remains visible"),
        "{context}"
    );
}

#[test]
fn resume_context_preserves_scope_filters_global_fallback_and_child_boundary() {
    if std::env::var_os("OMS_MEMORY_RESUME_TEST").is_none() {
        for scenario in ["unscoped", "targeted", "child"] {
            let isolated = TempDir::new("oms-memory-resume-env");
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "resume_context_preserves_scope_filters_global_fallback_and_child_boundary",
                "--nocapture",
            ]);
            for (key, _) in std::env::vars_os() {
                if key.to_str().is_some_and(|key| key.starts_with("OMS_")) {
                    command.env_remove(key);
                }
            }
            command
                .env("OMS_MEMORY_RESUME_TEST", scenario)
                .env(
                    "OH_MY_SETTING_GLOBAL_MEMORY",
                    isolated.0.join("global/shared.md"),
                )
                .env("OMS_LOCK_DIR", isolated.0.join("locks"));
            if scenario == "targeted" {
                command.env("OMS_MEMORY_TARGET_PATHS", "src/lib.rs");
            } else if scenario == "child" {
                command.env("OMS_HARNESS_CHILD", "1");
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{scenario}: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        return;
    }
    let dir = TempDir::new("oms-memory-resume");
    let repo = &dir.0;
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
            .unwrap()
            .success()
    );
    let flag = AtomicBool::new(false);
    for (paths, text) in [
        ("src/*", "scopetopic relevant project source"),
        ("docs/*", "scopetopic unrelated project documentation"),
    ] {
        let args: Vec<_> = ["append", "--paths", paths, "--text", text]
            .into_iter()
            .map(OsString::from)
            .collect();
        let (code, _, err) = oms_command_memory::run_with_source(&args, repo, repo, &flag);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    }
    let global = PathBuf::from(std::env::var_os("OH_MY_SETTING_GLOBAL_MEMORY").unwrap());
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(
        &global,
        "## 2026-01-01T00:00:00Z agent\n\nscopetopic global fallback lesson\n",
    )
    .unwrap();
    std::fs::write(
        global.parent().unwrap().join("pins.md"),
        "- 2026-01-01T00:00:00Z [agent] Global explicit pin\n",
    )
    .unwrap();
    let (code, out, err) = oms_command_memory::resume_context(repo, "scopetopic", &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Global explicit pin"), "{text}");
    assert!(!text.contains("unrelated project documentation"), "{text}");
    match std::env::var("OMS_MEMORY_RESUME_TEST").unwrap().as_str() {
        "targeted" => {
            assert!(text.contains("relevant project source"), "{text}");
            assert!(!text.contains("global fallback lesson"), "{text}");
        }
        "child" => assert!(!text.contains("- recall"), "{text}"),
        _ => {
            assert!(text.contains("global fallback lesson"), "{text}");
            assert!(!text.contains("relevant project source"), "{text}");
        }
    }
    std::fs::write(repo.join(".oms/memory/reviews.jsonl"), b"{invalid}\n").unwrap();
    let (code, out, err) = oms_command_memory::resume_context(repo, "scopetopic", &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let project_broken = String::from_utf8(out).unwrap();
    assert!(
        !project_broken.contains("relevant project source"),
        "unavailable review eligibility must withhold project recall: {project_broken}"
    );
    assert!(
        project_broken.contains("- unavailable project: memory review receipt is invalid"),
        "{project_broken}"
    );
    assert!(
        project_broken.contains("Global explicit pin"),
        "{project_broken}"
    );

    std::fs::remove_file(repo.join(".oms/memory/reviews.jsonl")).unwrap();
    let pin_args: Vec<_> = [
        "pin",
        "--text",
        "Project explicit pin survives broken global",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let (code, _, err) = oms_command_memory::run_with_source(&pin_args, repo, repo, &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));

    std::fs::write(
        global.parent().unwrap().join("reviews.jsonl"),
        b"{invalid}\n",
    )
    .unwrap();
    let (code, out, err) = oms_command_memory::resume_context(repo, "scopetopic", &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let malformed_global = String::from_utf8(out).unwrap();
    assert!(
        malformed_global.contains("Project explicit pin survives broken global"),
        "{malformed_global}"
    );
    assert!(
        malformed_global.contains("- unavailable global: memory review receipt is invalid"),
        "{malformed_global}"
    );
    assert!(
        !malformed_global.contains("Global explicit pin"),
        "{malformed_global}"
    );

    std::fs::remove_file(global.parent().unwrap().join("reviews.jsonl")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&global)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let (code, out, err) = oms_command_memory::resume_context(repo, "scopetopic", &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let oversized_global = String::from_utf8(out).unwrap();
    assert!(
        oversized_global.contains("Project explicit pin survives broken global"),
        "{oversized_global}"
    );
    assert!(
        oversized_global
            .contains("- unavailable global: memory: source exceeds 4194304-byte limit"),
        "{oversized_global}"
    );
    if std::env::var("OMS_MEMORY_RESUME_TEST").unwrap() == "targeted" {
        assert!(
            oversized_global.contains("relevant project source"),
            "{oversized_global}"
        );
    }

    // When recall fails for project after pin selection, project pins are dropped before
    // truncating to RESUME_RECORDS so all three surviving global pins still appear.
    std::fs::write(
        &global,
        "## 2026-01-01T00:00:00Z agent\n\nscopetopic global fallback lesson\n",
    )
    .unwrap();
    std::fs::write(
        global.parent().unwrap().join("pins.md"),
        "- 2026-01-01T00:00:00Z [agent] Global explicit pin one\n\
         - 2026-01-02T00:00:00Z [agent] Global explicit pin two\n\
         - 2026-01-03T00:00:00Z [agent] Global explicit pin three\n",
    )
    .unwrap();
    for extra in ["Project newer pin two", "Project newer pin three"] {
        let args: Vec<_> = ["pin", "--text", extra]
            .into_iter()
            .map(OsString::from)
            .collect();
        let (code, _, err) = oms_command_memory::run_with_source(&args, repo, repo, &flag);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    }
    let project_shared = repo.join(".oms/memory/shared.md");
    let shared_bytes = std::fs::read(&project_shared).unwrap();
    let mut duplicated = shared_bytes.clone();
    duplicated.push(b'\n');
    duplicated.extend_from_slice(&shared_bytes);
    std::fs::write(&project_shared, duplicated).unwrap();
    let (code, out, err) = oms_command_memory::resume_context(repo, "scopetopic", &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let recall_failed_project = String::from_utf8(out).unwrap();
    for expected in [
        "Global explicit pin three",
        "Global explicit pin two",
        "Global explicit pin one",
        "- unavailable project:",
    ] {
        assert!(
            recall_failed_project.contains(expected),
            "missing {expected:?}: {recall_failed_project}"
        );
    }
    assert!(
        !recall_failed_project.contains("Project explicit pin")
            && !recall_failed_project.contains("Project newer pin"),
        "{recall_failed_project}"
    );
}

#[test]
fn journal_batches_reconcile_keys_refuse_collisions_and_repair_derived_outputs() {
    use oms_command_memory::{JournalProposal, JournalPublication, append_journal_batch};
    if std::env::var_os("OMS_MEMORY_BATCH_TEST").is_none() {
        let isolated = TempDir::new("oms-memory-batch-env");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "journal_batches_reconcile_keys_refuse_collisions_and_repair_derived_outputs",
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|key| key.starts_with("OMS_")) {
                command.env_remove(key);
            }
        }
        let result = command
            .env("OMS_MEMORY_BATCH_TEST", "1")
            .env("OMS_LOCK_DIR", isolated.0.join("locks"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let dir = TempDir::new("oms-memory-batch-test");
    let repo = &dir.0;
    let flag = AtomicBool::new(false);
    let batch = "a".repeat(64);
    let source_digest = "b".repeat(64);
    let candidate_ids: Vec<_> = ["c", "d", "e", "f"]
        .map(|s| s.repeat(64))
        .into_iter()
        .collect();
    let proposals = [
        JournalProposal {
            key: &candidate_ids[0],
            source_sha256: &source_digest,
            body: "journal-distill: stable first record",
            source_current: true,
        },
        JournalProposal {
            key: &candidate_ids[1],
            source_sha256: &source_digest,
            body: "journal-distill: stable second record",
            source_current: true,
        },
        JournalProposal {
            key: &candidate_ids[2],
            source_sha256: &source_digest,
            body: "<!-- oms-memory\nreserved framing",
            source_current: true,
        },
        JournalProposal {
            key: &candidate_ids[3],
            source_sha256: &source_digest,
            body: "journal-distill: source changed before append",
            source_current: false,
        },
    ];
    let result = append_journal_batch(repo, &batch, &proposals[..1], &flag).unwrap();
    let JournalPublication::Appended(first_id) = &result.outcomes[0] else {
        panic!("missing first append")
    };
    let first_id = first_id.clone();
    let shared = repo.join(".oms/memory/shared.md");
    let before = std::fs::read(&shared).unwrap();
    let reports = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            append_journal_batch(repo, &batch, &proposals, &AtomicBool::new(false)).unwrap()
        });
        let second = scope.spawn(|| {
            append_journal_batch(repo, &batch, &proposals, &AtomicBool::new(false)).unwrap()
        });
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(
        reports
            .iter()
            .flat_map(|r| &r.outcomes)
            .filter(|o| matches!(o, JournalPublication::Appended(_)))
            .count(),
        1
    );
    for result in &reports {
        assert_eq!(
            result.outcomes[0],
            JournalPublication::AlreadyPresent(first_id.clone())
        );
        assert!(matches!(result.outcomes[2], JournalPublication::Refused(_)));
        assert!(matches!(result.outcomes[3], JournalPublication::Refused(_)));
    }
    let published = std::fs::read(&shared).unwrap();
    assert!(published.starts_with(&before));
    assert_eq!(
        String::from_utf8_lossy(&published)
            .matches("journal_candidate:")
            .count(),
        2
    );
    for collision in [
        JournalProposal {
            body: "changed payload under the same identity",
            ..proposals[0]
        },
        JournalProposal {
            source_sha256: &"0".repeat(64),
            ..proposals[0]
        },
    ] {
        let report = append_journal_batch(repo, &batch, &[collision], &flag).unwrap();
        assert!(matches!(report.outcomes[0], JournalPublication::Refused(_)));
        assert_eq!(std::fs::read(&shared).unwrap(), published);
    }
    assert!(append_journal_batch(repo, &batch, &[proposals[0], proposals[0]], &flag).is_err());
    assert!(append_journal_batch(repo, "invalid", &proposals, &flag).is_err());
    assert!(append_journal_batch(repo, &batch, &proposals, &AtomicBool::new(true)).is_err());
    assert_eq!(std::fs::read(&shared).unwrap(), published);

    let run = |args: &[&str]| {
        let args: Vec<_> = args.iter().map(OsString::from).collect();
        let (code, output, error) = oms_command_memory::run_with_source(&args, repo, repo, &flag);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&error));
        String::from_utf8(output).unwrap()
    };
    assert!(!run(&["context"]).contains("stable first record"));
    run(&["review", "--accept", &first_id]);
    assert!(run(&["context"]).contains("stable first record"));
    run(&["redact", "--id", &first_id, "--apply"]);
    let redacted = std::fs::read(&shared).unwrap();
    let report = append_journal_batch(repo, &batch, &proposals, &flag).unwrap();
    assert_eq!(
        report.outcomes[0],
        JournalPublication::AlreadyPresent(first_id)
    );
    assert_eq!(
        std::fs::read(&shared).unwrap(),
        redacted,
        "retry must not resurrect redacted text"
    );
    assert!(!run(&["context"]).contains("stable first record"));

    let summary = repo.join(".oms/memory/summary.md");
    let database = repo.join(".oms/memory/memory.sqlite3");
    for path in [&summary, &database] {
        std::fs::remove_file(path).unwrap();
        std::fs::create_dir(path).unwrap();
    }
    let failed_refresh = append_journal_batch(repo, &batch, &proposals, &flag).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&failed_refresh.warnings)
            .lines()
            .count(),
        2,
        "one compact and one index warning per batch"
    );
    assert_eq!(std::fs::read(&shared).unwrap(), redacted);
    for path in [&summary, &database] {
        std::fs::remove_dir(path).unwrap();
    }
    let repaired = append_journal_batch(repo, &batch, &proposals, &flag).unwrap();
    assert!(repaired.warnings.is_empty());
    assert!(summary.is_file() && database.is_file());
    assert_eq!(std::fs::read(&shared).unwrap(), redacted);
    assert!(run(&["search", "stable second record"]).contains("stable second record"));
}

#[test]
fn health_distinguishes_syncable_lag_from_duplicate_ids_and_unwritable_store() {
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os("OMS_MEMORY_HEALTH_TEST").is_none() {
        let isolated = TempDir::new("oms-memory-health-env");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "health_distinguishes_syncable_lag_from_duplicate_ids_and_unwritable_store",
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|k| k.starts_with("OMS_")) {
                command.env_remove(key);
            }
        }
        let result = command
            .env("OMS_MEMORY_HEALTH_TEST", "1")
            .env("HOME", &isolated.0)
            .env("OH_MY_SETTING_GLOBAL_MEMORY", isolated.0.join("global.md"))
            .env("OMS_LOCK_DIR", isolated.0.join("locks"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    let dir = TempDir::new("oms-memory-health-repo");
    let repo = &dir.0;
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(repo)
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::create_dir_all(repo.join(".oms")).unwrap();

    let flag = AtomicBool::new(false);
    let call = |args: &[&str]| {
        let argv: Vec<_> = args.iter().map(OsString::from).collect();
        oms_command_memory::run_with_source(&argv, repo, repo, &flag)
    };
    let health = |sync: bool| -> (i32, Value) {
        let out = if sync {
            call(&["health", "--sync", "--json"])
        } else {
            call(&["health", "--json"])
        };
        let parsed: Value = serde_json::from_slice(&out.1).unwrap_or_else(|e| {
            panic!(
                "invalid health json ({e}): stdout={} stderr={}",
                String::from_utf8_lossy(&out.1),
                String::from_utf8_lossy(&out.2)
            )
        });
        (out.0, parsed)
    };

    assert_eq!(call(&["init"]).0, 0);
    assert_eq!(
        call(&["append", "--agent", "codex", "--text", "baseline note"]).0,
        0
    );
    let (code, ok) = health(false);
    assert_eq!(code, 0, "{ok}");
    assert_eq!(ok["status"], "healthy");
    assert_eq!(ok["index"]["current"], true);
    assert_eq!(ok["index"]["writable"], true);
    assert_eq!(ok["index"]["syncable"], true);
    assert_eq!(ok["index"]["sync"], "not-run");

    // 1. A failures.jsonl append after the last sync is stale-but-syncable without
    // mutating the index on read-only health, and becomes healthy/current on --sync.
    let ledger = repo.join(".oms/failures.jsonl");
    let failure_one = "{\"ts\":\"2026-11-28T00:00:00Z\",\"event\":\"fail\",\"fingerprint\":\"fp-1\",\"cmd\":\"cargo test\",\"exit\":1,\"summary\":\"unit test failed\"}\n";
    std::fs::write(&ledger, failure_one).unwrap();
    let db_path = repo.join(".oms/memory/memory.sqlite3");
    let db_before = std::fs::read(&db_path).unwrap();
    let (code, trailing) = health(false);
    assert_eq!(code, 1, "{trailing}");
    assert_eq!(trailing["status"], "stale");
    assert_eq!(trailing["problems"], json!([]));
    assert_eq!(
        trailing["index"]["problems"],
        json!(["source-index-mismatch"])
    );
    assert_eq!(trailing["index"]["current"], false);
    assert_eq!(trailing["index"]["writable"], true);
    assert_eq!(trailing["index"]["syncable"], true);
    assert_eq!(trailing["index"]["sync"], "not-run");
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        db_before,
        "plain health must remain strictly read-only"
    );

    let (code, synced) = health(true);
    assert_eq!(code, 0, "{synced}");
    assert_eq!(synced["status"], "healthy");
    assert_eq!(synced["index"]["current"], true);
    assert_eq!(synced["index"]["syncable"], true);
    assert_eq!(synced["index"]["sync"], "synced");
    assert_eq!(synced["index"]["by_source"]["failures"], 1);
    let (code, after_sync) = health(false);
    assert_eq!(code, 0, "{after_sync}");
    assert_eq!(after_sync["status"], "healthy");
    assert_eq!(after_sync["index"]["current"], true);

    // Malformed failure rows surface as failure-ledger-invalid / degraded, never stale.
    std::fs::write(&ledger, format!("{failure_one}not-json\n")).unwrap();
    for sync_flag in [false, true] {
        let (code, bad_ledger) = health(sync_flag);
        assert_eq!(code, 1, "{bad_ledger}");
        assert_eq!(bad_ledger["status"], "degraded");
        assert_eq!(bad_ledger["index"]["syncable"], false);
        assert_eq!(bad_ledger["problems"], json!(["failure-ledger-invalid"]));
    }
    std::fs::write(&ledger, failure_one).unwrap();
    assert_eq!(health(true).0, 0);

    // 2. Duplicate event_id in shared.md surfaces as degraded (duplicate-event-id), never stale.
    let shared_path = repo.join(".oms/memory/shared.md");
    let clean_shared = std::fs::read(&shared_path).unwrap();
    let dup_record = concat!(
        "<!-- oms-memory\nevent_id: mem-dup-1\nkind: note\nschema: 1\n-->\n",
        "## 2026-11-28T00:01:00Z codex\n\nfirst duplicate entry\n\n",
        "<!-- oms-memory\nevent_id: mem-dup-1\nkind: note\nschema: 1\n-->\n",
        "## 2026-11-28T00:02:00Z codex\n\nsecond duplicate entry\n\n",
    );
    let mut with_dup = clean_shared.clone();
    with_dup.extend_from_slice(dup_record.as_bytes());
    std::fs::write(&shared_path, &with_dup).unwrap();

    let (code, dup_readonly) = health(false);
    assert_eq!(code, 1, "{dup_readonly}");
    assert_eq!(dup_readonly["status"], "degraded");
    assert_eq!(dup_readonly["index"]["syncable"], false);
    assert_eq!(dup_readonly["problems"], json!(["duplicate-event-id"]));

    let (code, dup_sync) = health(true);
    assert_eq!(code, 1, "{dup_sync}");
    assert_eq!(dup_sync["status"], "degraded");
    assert_eq!(dup_sync["index"]["sync"], "failed");
    assert_eq!(dup_sync["index"]["syncable"], false);
    assert_eq!(dup_sync["problems"], json!(["duplicate-event-id"]));
    assert!(
        dup_sync["index"]["problems"]
            .as_array()
            .unwrap()
            .contains(&json!("sync-failed")),
        "{dup_sync}"
    );
    std::fs::write(&shared_path, &clean_shared).unwrap();
    assert_eq!(health(true).0, 0);

    // 3a. Read-only index file surfaces as degraded (index-unwritable), both current and trailing.
    let failure_two = format!(
        "{failure_one}{{\"ts\":\"2026-11-28T00:03:00Z\",\"event\":\"fail\",\"fingerprint\":\"fp-2\",\"cmd\":\"cargo check\",\"exit\":1,\"summary\":\"check failed\"}}\n"
    );
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (code, ro_current) = health(false);
    assert_eq!(code, 1, "{ro_current}");
    assert_eq!(ro_current["status"], "degraded");
    assert_eq!(ro_current["index"]["writable"], false);
    assert_eq!(ro_current["index"]["syncable"], false);
    assert_eq!(ro_current["index"]["problems"], json!(["index-unwritable"]));

    std::fs::write(&ledger, &failure_two).unwrap();
    let (code, ro_trailing) = health(false);
    assert_eq!(code, 1, "{ro_trailing}");
    assert_eq!(ro_trailing["status"], "degraded");
    assert_eq!(ro_trailing["index"]["syncable"], false);
    assert!(
        ro_trailing["index"]["problems"]
            .as_array()
            .unwrap()
            .contains(&json!("index-unwritable")),
        "{ro_trailing}"
    );
    let (code, ro_sync) = health(true);
    assert_eq!(code, 1, "{ro_sync}");
    assert_eq!(ro_sync["status"], "degraded");
    assert_eq!(ro_sync["index"]["sync"], "failed");
    assert!(
        ro_sync["index"]["problems"]
            .as_array()
            .unwrap()
            .contains(&json!("index-unwritable")),
        "{ro_sync}"
    );
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(health(true).0, 0);

    // 3b. Read-only memory directory surfaces as degraded (memory-dir-unwritable).
    let mem_dir = repo.join(".oms/memory");
    let failure_three = format!(
        "{failure_two}{{\"ts\":\"2026-11-28T00:04:00Z\",\"event\":\"fail\",\"fingerprint\":\"fp-3\",\"cmd\":\"cargo clippy\",\"exit\":1,\"summary\":\"lint failed\"}}\n"
    );
    std::fs::write(&ledger, &failure_three).unwrap();
    std::fs::set_permissions(&mem_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (code, ro_dir) = health(false);
    let (sync_code, ro_dir_sync) = health(true);
    std::fs::set_permissions(&mem_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(code, 1, "{ro_dir}");
    assert_eq!(ro_dir["status"], "degraded");
    assert_eq!(ro_dir["index"]["writable"], false);
    assert_eq!(ro_dir["index"]["syncable"], false);
    assert!(
        ro_dir["index"]["problems"]
            .as_array()
            .unwrap()
            .contains(&json!("memory-dir-unwritable")),
        "{ro_dir}"
    );
    assert_eq!(sync_code, 1, "{ro_dir_sync}");
    assert_eq!(ro_dir_sync["status"], "degraded");
    assert_eq!(ro_dir_sync["index"]["sync"], "failed");
    assert!(
        ro_dir_sync["index"]["problems"]
            .as_array()
            .unwrap()
            .contains(&json!("memory-dir-unwritable")),
        "{ro_dir_sync}"
    );

    assert_eq!(call(&["init", "--sync"]).0, 2);
}
