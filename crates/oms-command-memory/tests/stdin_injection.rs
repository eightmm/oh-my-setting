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
    run(&["review", "--retire", &ids[1], "--reason", "not reusable"]);
    run(&["retire", "--id", &ids[0], "--reason", "outdated"]);
    assert_context(&[3, 4]);
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
