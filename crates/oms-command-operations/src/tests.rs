use super::*;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use value::{V, obj};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(uuid("oms-operations-").unwrap());
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn call(&self, app: App, words: &[&str]) -> CommandResult {
        let mut args = vec![OsString::from("--repo"), self.0.as_os_str().into()];
        args.extend(words.iter().map(OsString::from));
        run(app, &args, &self.0, &AtomicBool::new(false))
    }
    fn ok(&self, app: App, words: &[&str]) -> String {
        let (code, out, error) = self.call(app, words);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&error));
        String::from_utf8(out).unwrap().trim().into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn lifecycle_idempotence_usage_cas_and_recovery() {
    let f = Fixture::new();
    let request = [
        "start",
        "--tool",
        "fixture",
        "--idempotency-key",
        "s1",
        "--then",
        "starting:s2",
        "--then",
        "working:s3",
    ];
    let id = f.ok(App::Events, &request);
    assert_eq!(f.ok(App::Events, &request), id);
    assert_eq!(read_rows(&event_path(&f.0)).unwrap().len(), 3);
    assert_eq!(
        f.call(
            App::Events,
            &["start", "--tool", "other", "--idempotency-key", "s1"]
        )
        .0,
        2
    );
    assert_eq!(
        f.call(
            App::Events,
            &[
                "transition",
                "--attempt",
                &id,
                "--state",
                "review",
                "--expected-seq",
                "2"
            ]
        )
        .0,
        2
    );
    for _ in 0..2 {
        f.ok(
            App::Events,
            &[
                "usage",
                "--attempt",
                &id,
                "--tokens",
                "0",
                "--duration-ms",
                "12",
                "--ref",
                "record=relative/file",
                "--idempotency-key",
                "u1",
            ],
        );
    }
    for invalid in ["", "-1", "bad"] {
        assert_eq!(
            f.call(
                App::Events,
                &["usage", "--attempt", &id, "--tokens", invalid]
            )
            .0,
            2
        );
    }
    f.ok(
        App::Events,
        &["reconcile", "--stale-seconds", "0", "--apply"],
    );
    let values = project_attempts(&read_rows(&event_path(&f.0)).unwrap()).unwrap();
    let sequence = values[0].get("sequence").py();
    f.ok(
        App::Events,
        &[
            "recover-verify-heartbeat",
            "--attempt",
            &id,
            "--expected-sequence",
            &sequence,
        ],
    );
    assert_eq!(
        f.call(
            App::Events,
            &[
                "recover-verify-heartbeat",
                "--attempt",
                &id,
                "--expected-sequence",
                &sequence
            ]
        )
        .0,
        2
    );
    f.ok(
        App::Events,
        &["transition", "--attempt", &id, "--state", "done"],
    );
    let values = project_attempts(&read_rows(&event_path(&f.0)).unwrap()).unwrap();
    assert_eq!(values[0].get("state").text(), Some("done"));
    assert_eq!(values[0].get("usage").get("duration_ms").py(), "12");
    assert_eq!(values[0].get("usage_reports").get("tokens").py(), "1");
    assert_ne!(f.ok(App::Events, &["resume", "--attempt", &id]), id);
}

#[test]
fn approvals_cas_private_grants_and_recovery() {
    let valid_xdg = std::env::var_os("XDG_STATE_HOME")
        .is_some_and(|v| PathBuf::from(&v).starts_with(std::env::temp_dir()));
    if !valid_xdg {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sandbox =
            std::env::temp_dir().join(format!("oms-ops-xdg-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&sandbox).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::approvals_cas_private_grants_and_recovery",
                "--nocapture",
            ])
            .env("XDG_STATE_HOME", &sandbox)
            .status()
            .unwrap();
        let _ = std::fs::remove_dir_all(&sandbox);
        assert!(status.success(), "sandboxed approvals test failed");
        return;
    }
    let root = std::env::var_os("XDG_STATE_HOME").expect("disposable XDG_STATE_HOME required");
    assert!(PathBuf::from(root).starts_with(std::env::temp_dir()));
    let f = Fixture::new();
    for expiry in ["", "0", "604801", "bad"] {
        assert_eq!(
            f.call(
                App::Approvals,
                &[
                    "request",
                    "--action",
                    "fixture",
                    "--object-id",
                    "one",
                    "--summary",
                    "Fixture",
                    "--expires-in",
                    expiry
                ]
            )
            .0,
            2
        );
    }
    for expiry in ["1", "604800"] {
        f.ok(
            App::Approvals,
            &[
                "request",
                "--action",
                "fixture",
                "--object-id",
                "edge",
                "--summary",
                "Fixture",
                "--expires-in",
                expiry,
            ],
        );
    }
    let id = f.ok(
        App::Approvals,
        &[
            "request",
            "--action",
            "fixture",
            "--object-id",
            "one",
            "--summary",
            "Fixture",
            "--parameters-json",
            "{\"a\":1.0,\"b\":null}",
        ],
    );
    let proof = f.ok(
        App::Approvals,
        &[
            "decide",
            "--approval",
            &id,
            "--decision",
            "approve",
            "--expected-version",
            "1",
            "--actor",
            "human",
            "--changes-json",
            "{}",
        ],
    );
    assert_eq!(
        f.call(
            App::Approvals,
            &[
                "decide",
                "--approval",
                &id,
                "--decision",
                "deny",
                "--expected-version",
                "1",
                "--actor",
                "human"
            ]
        )
        .0,
        2
    );
    assert!(
        !f.ok(App::Approvals, &["show", "--approval", &id, "--json"])
            .contains("grant_")
    );
    for bad in ["", "x", "grant_x.short"] {
        assert_eq!(
            f.call(
                App::Approvals,
                &[
                    "begin-consume",
                    "--approval",
                    &id,
                    "--token",
                    bad,
                    "--expected-version",
                    "2",
                    "--consumer",
                    "fixture"
                ]
            )
            .0,
            2
        );
    }
    let (grant, material) = proof.split_once('.').unwrap();
    let replacement = if material.starts_with('A') { "B" } else { "A" };
    let wrong = format!("{grant}.{replacement}{}", &material[1..]);
    assert_eq!(
        f.call(
            App::Approvals,
            &[
                "begin-consume",
                "--approval",
                &id,
                "--token",
                &wrong,
                "--expected-version",
                "2",
                "--consumer",
                "fixture"
            ]
        )
        .0,
        2
    );
    f.ok(
        App::Approvals,
        &[
            "begin-consume",
            "--approval",
            &id,
            "--token",
            &proof,
            "--expected-version",
            "2",
            "--consumer",
            "fixture",
        ],
    );
    assert_eq!(
        f.call(
            App::Approvals,
            &[
                "begin-consume",
                "--approval",
                &id,
                "--token",
                &proof,
                "--expected-version",
                "2",
                "--consumer",
                "fixture"
            ]
        )
        .0,
        2
    );
    f.ok(
        App::Approvals,
        &[
            "finish-consume",
            "--approval",
            &id,
            "--expected-version",
            "3",
            "--result",
            "consumed",
            "--consumer",
            "fixture",
        ],
    );
    assert_eq!(
        load_approvals(&f.0)
            .unwrap()
            .iter()
            .find(|v| v.get("approval_id").text() == Some(&id))
            .unwrap()
            .get("state")
            .text(),
        Some("consumed")
    );
    f.ok(App::Approvals, &["validate"]);
    let _ = std::fs::remove_file(approval_path(&f.0, false).unwrap());
}

#[test]
fn strict_projection_and_storage_boundaries() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new();
    let path = f.0.join("ledger");
    let canary = f.0.join("canary");
    std::fs::write(&canary, b"{}\n").unwrap();
    symlink(&canary, &path).unwrap();
    assert!(read_rows(&path).is_err());
    assert!(storage::append_row(&path, &mut obj([]), true).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&canary, &path).unwrap();
    assert!(read_rows(&path).is_err());
    assert!(storage::append_row(&path, &mut obj([]), true).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"{\"torn\":").unwrap();
    storage::append_row(&path, &mut obj([("event", "recovered".into())]), true).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{\"torn\":\n{\"event\":\"recovered\"}\n"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(read_rows(&path).is_err());
    for size in [0, 240, 241] {
        let mut request = StartRequest {
            tool: "fixture".into(),
            ..Default::default()
        };
        request.refs = obj([("path", "x".repeat(size).into())]);
        assert_eq!(
            create_attempt(&f.0, &request, &AtomicBool::new(false)).is_ok(),
            size <= 240
        );
    }
    for raw in ["NaN", "Infinity", "-Infinity", "1e999"] {
        assert!(
            parse_scalar_map(&format!("{{\"risk\":{raw}}}"), "parameters-json")
                .unwrap_err()
                .contains("finite")
        );
    }
    for count in [0, 32, 33] {
        let fields = (0..count)
            .map(|n| format!("\"p{n}\":null"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_scalar_map(&format!("{{{fields}}}"), "parameters-json").is_ok(),
            count <= 32
        );
    }
}

#[test]
fn follow_limit_help_and_interruption() {
    let f = Fixture::new();
    let id = f.ok(App::Events, &["start", "--tool", "fixture"]);
    let event = read_rows(&event_path(&f.0)).unwrap()[0]
        .get("event_id")
        .py();
    f.ok(App::Events, &["heartbeat", "--attempt", &id]);
    assert_eq!(
        f.ok(
            App::Events,
            &[
                "follow",
                "--after-event",
                &event,
                "--timeout",
                "0",
                "--limit",
                "1"
            ]
        )
        .lines()
        .count(),
        1
    );
    for after in ["missing", "!"] {
        assert_eq!(
            f.call(
                App::Events,
                &["follow", "--after-event", after, "--timeout", "0"]
            )
            .0,
            2
        );
    }
    assert_eq!(
        f.ok(App::Events, &["follow", "--timeout", "0", "--limit", "0"])
            .lines()
            .count(),
        2
    );
    assert_eq!(f.call(App::Events, &["list", "--limit", "bad"]).0, 2);
    assert!(f.ok(App::Events, &["start", "--help"]).contains("--then"));
    assert_eq!(
        run(
            App::Events,
            &[OsString::from("list")],
            &f.0,
            &AtomicBool::new(true)
        )
        .0,
        2
    );
    std::fs::write(event_path(&f.0), b"{\"z\":1,\"event_id\":\"skip\",\"unknown\":NaN}\n{\"z\":1,\"event_id\":\"last\",\"a\":2}\n").unwrap();
    assert_eq!(
        f.ok(
            App::Events,
            &[
                "follow",
                "--after-event",
                "skip",
                "--timeout",
                "0",
                "--limit",
                "1"
            ]
        ),
        r#"{"z":1,"event_id":"last","a":2}"#
    );
    assert_eq!(f.call(App::Events, &["follow", "--timeout", "0"]).0, 2);
}

#[test]
fn concurrent_start_lock_recovery_and_spec_repair() {
    let f = Fixture::new();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = f.0.clone();
            std::thread::spawn(move || {
                create_attempt(
                    &path,
                    &StartRequest {
                        tool: "fixture".into(),
                        idempotency_key: "shared".into(),
                        ..Default::default()
                    },
                    &AtomicBool::new(false),
                )
                .unwrap()
            })
        })
        .collect();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(read_rows(&event_path(&f.0)).unwrap().len(), 1);
    std::fs::remove_file(attempt_spec_path(&f.0, &ids[0])).unwrap();
    assert_eq!(
        create_attempt(
            &f.0,
            &StartRequest {
                tool: "fixture".into(),
                idempotency_key: "shared".into(),
                ..Default::default()
            },
            &AtomicBool::new(false)
        )
        .unwrap(),
        ids[0]
    );
    assert!(attempt_spec_path(&f.0, &ids[0]).exists());
    let path = f.0.join("lock-subject");
    let directory = lock::runtime_lock_dir(&path).unwrap();
    std::fs::create_dir(&directory).unwrap();
    let owner = obj([
        ("pid", 2_147_483_647usize.into()),
        ("owner_nonce", "dead".into()),
    ]);
    std::fs::write(directory.join("owner.json"), owner.json(false).unwrap()).unwrap();
    let observed = lock::snapshot(&directory).unwrap();
    assert!(lock::stale(&observed, 300));
    let guard = lock::FileLock::acquire(&path, &AtomicBool::new(false)).unwrap();
    let live = lock::snapshot(&directory).unwrap();
    guard.check().unwrap();
    assert!(!lock::stale(&live, 0));
    assert!(!lock::reclaim(&directory, &observed, 0).unwrap());
    let started = std::time::Instant::now();
    assert!(
        lock::FileLock::acquire_before(
            &path,
            &AtomicBool::new(false),
            started + std::time::Duration::from_millis(30)
        )
        .is_err()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(
        lock::snapshot(&directory).unwrap().generation,
        live.generation
    );
    assert!(
        lock::FileLock::acquire_before(
            &path,
            &AtomicBool::new(true),
            std::time::Instant::now() + std::time::Duration::from_secs(2)
        )
        .is_err()
    );
    guard.check().unwrap();
    drop(guard);
    assert!(!directory.exists());
    assert!(
        lock::FileLock::acquire_before(&path, &AtomicBool::new(false), std::time::Instant::now())
            .is_err()
    );
    assert!(!directory.exists());
    let guard = lock::FileLock::acquire_before(
        &path,
        &AtomicBool::new(false),
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    )
    .unwrap();
    let owner = std::fs::read(directory.join("owner.json")).unwrap();
    let retired = directory.with_extension("replaced-for-fixture");
    std::fs::rename(&directory, &retired).unwrap();
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("owner.json"), &owner).unwrap();
    assert!(guard.check().is_err());
    drop(guard);
    assert_eq!(std::fs::read(directory.join("owner.json")).unwrap(), owner);
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::remove_dir_all(&retired).unwrap();
    let guard = lock::FileLock::acquire(&path, &AtomicBool::new(false)).unwrap();
    let changed = obj([("owner_nonce", "changed-owner".into())])
        .json(false)
        .unwrap();
    std::fs::write(directory.join("owner.json"), &changed).unwrap();
    assert!(guard.check().is_err());
    drop(guard);
    assert_eq!(
        std::fs::read(directory.join("owner.json")).unwrap(),
        changed
    );
    std::fs::remove_dir_all(&directory).unwrap();
    let mut reused = live;
    reused.owner.set("process_start", "proc:impossible");
    assert!(lock::stale(&reused, 300));
    let unknown = lock::Snapshot {
        generation: String::new(),
        owner: V::Null,
        mtime: now_seconds() - 600.0,
    };
    assert!(lock::stale(&unknown, 300));
    assert!(!lock::stale(
        &lock::Snapshot {
            mtime: now_seconds() + 600.0,
            ..unknown
        },
        300
    ));
    let huge = lock::Snapshot {
        generation: String::new(),
        mtime: 0.0,
        owner: obj([
            ("pid", V::Num("9".repeat(100))),
            ("owner_nonce", "unprovable".into()),
        ]),
    };
    assert!(!lock::stale(&huge, 1));

    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let program = f.0.join("ps-fixture");
    let marker = f.0.join("ps-entered");
    let write_probe = |body: &str| {
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    write_probe("printf '  fixture   start  \\n'");
    assert_eq!(
        lock::process_start_ps_before(
            &program,
            1,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap(),
        Some("ps:fixture start".into())
    );
    let script = format!(
        "printf '%s' \"$$\" > '{}'; exec /bin/sleep 10",
        marker.to_string_lossy().replace('\'', "'\\''")
    );
    write_probe(&script);
    assert!(
        lock::process_start_ps_before(&program, 1, &AtomicBool::new(false), Instant::now())
            .is_err()
    );
    assert!(!marker.exists());
    let started = Instant::now();
    assert!(
        lock::process_start_ps_before(
            &program,
            1,
            &AtomicBool::new(false),
            started + Duration::from_millis(150)
        )
        .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let pid = std::fs::read_to_string(&marker)
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert!(!lock::pid_alive(pid));
    std::fs::remove_file(&marker).unwrap();
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !marker.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        });
        assert!(
            lock::process_start_ps_before(
                &program,
                1,
                &cancelled,
                Instant::now() + Duration::from_secs(2)
            )
            .is_err()
        );
    });
    let pid = std::fs::read_to_string(&marker)
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert!(!lock::pid_alive(pid));
    write_probe("while :; do printf '0123456789abcdef'; done");
    assert!(
        lock::process_start_ps_before(
            &program,
            1,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap_err()
        .contains("output exceeds bound")
    );
}

#[test]
fn compact_preserves_parents_and_kept_row_bytes() {
    let f = Fixture::new();
    let parent = f.ok(App::Events, &["start", "--tool", "parent"]);
    f.ok(
        App::Events,
        &["transition", "--attempt", &parent, "--state", "cancelled"],
    );
    let child = f.ok(
        App::Events,
        &["start", "--tool", "child", "--parent-attempt-id", &parent],
    );
    let removed = f.ok(App::Events, &["start", "--tool", "removed"]);
    f.ok(
        App::Events,
        &["transition", "--attempt", &removed, "--state", "cancelled"],
    );
    let path = event_path(&f.0);
    let mut rows = read_rows(&path).unwrap();
    for row in &mut rows {
        row.set("ts", "2000-01-01T00:00:00Z");
    }
    let raw: Vec<_> = rows
        .iter()
        .map(|row| {
            format!(
                " {} \n",
                String::from_utf8(row.json(false).unwrap()).unwrap()
            )
        })
        .collect();
    std::fs::write(&path, raw.concat()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let attempts = f.0.join(".oms/lifecycle/attempts");
    let gone = attempts.join(&removed);
    std::fs::create_dir_all(&gone).unwrap();
    std::fs::write(gone.join("spec.json"), b"{}").unwrap();
    let kept_dir = attempts.join(&child);
    std::fs::create_dir_all(&kept_dir).unwrap();
    let dry = f.ok(App::Events, &["compact", "--days", "0"]);
    assert!(dry.contains("would drop 2 event(s) across 1"), "{dry}");
    assert!(dry.contains("1 attempt dir(s) to remove"), "{dry}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(gone.join("spec.json").exists());
    let applied = f.ok(App::Events, &["compact", "--days", "0", "--apply"]);
    assert!(applied.contains("removed 1 attempt dir(s)"), "{applied}");
    assert!(!gone.exists() && kept_dir.exists());
    assert_eq!(std::fs::read(&path).unwrap(), raw[..3].concat().as_bytes());
    let projected = project_attempts(&read_rows(&path).unwrap()).unwrap();
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[1].get("attempt_id").text(), Some(child.as_str()));
    assert!(
        f.ok(App::Events, &["compact", "--days", "0", "--apply"])
            .contains("nothing to drop")
    );
    for days in ["", "-1", "bad"] {
        assert_eq!(f.call(App::Events, &["compact", "--days", days]).0, 2);
    }
    std::fs::write(&path, b"{}\n").unwrap();
    assert_eq!(
        f.call(App::Events, &["compact", "--days", "0", "--apply"])
            .0,
        2
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");
}

#[test]
fn approval_expiry_and_unknown_outcome_defer_patch_land() {
    let f = Fixture::new();
    let mut ids = Vec::new();
    for action in ["fixture", "patch-land"] {
        let id = f.ok(
            App::Approvals,
            &[
                "request",
                "--action",
                action,
                "--object-id",
                "one",
                "--summary",
                "Fixture",
            ],
        );
        let proof = f.ok(
            App::Approvals,
            &[
                "decide",
                "--approval",
                &id,
                "--decision",
                "approve",
                "--expected-version",
                "1",
                "--actor",
                "human",
            ],
        );
        f.ok(
            App::Approvals,
            &[
                "begin-consume",
                "--approval",
                &id,
                "--token",
                &proof,
                "--expected-version",
                "2",
                "--consumer",
                "fixture",
            ],
        );
        ids.push(id);
    }
    let path = approval_path(&f.0, false).unwrap();
    let mut rows = read_rows(&path).unwrap();
    for row in &mut rows {
        row.set("ts", "2000-01-01T00:00:00Z");
    }
    let bytes: Vec<u8> = rows
        .iter()
        .flat_map(|v| {
            let mut b = v.json(false).unwrap();
            b.push(b'\n');
            b
        })
        .collect();
    std::fs::write(&path, &bytes).unwrap();
    let selected = f.ok(
        App::Approvals,
        &["reconcile", "--older-than-seconds", "1", "--json"],
    );
    assert_eq!(V::parse(selected.as_bytes()).unwrap().array().len(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    f.ok(
        App::Approvals,
        &["reconcile", "--older-than-seconds", "604800", "--apply"],
    );
    let projected = load_approvals(&f.0).unwrap();
    assert_eq!(projected[0].get("state").text(), Some("interrupted"));
    assert_eq!(projected[0].get("outcome").text(), Some("unknown"));
    assert_eq!(projected[1].get("state").text(), Some("consuming"));
    for older in ["", "0", "604801", "bad"] {
        assert_eq!(
            f.call(
                App::Approvals,
                &["reconcile", "--older-than-seconds", older]
            )
            .0,
            2
        );
    }
    let expired = f.ok(
        App::Approvals,
        &[
            "request",
            "--action",
            "fixture",
            "--object-id",
            "expired",
            "--summary",
            "Fixture",
        ],
    );
    let mut rows = read_rows(&path).unwrap();
    for row in &mut rows {
        if row.get("approval_id").text() == Some(&expired) {
            row.set("expires_at", "2000-01-01T00:00:00Z");
        }
    }
    let bytes: Vec<u8> = rows
        .iter()
        .flat_map(|v| {
            let mut b = v.json(false).unwrap();
            b.push(b'\n');
            b
        })
        .collect();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        f.call(
            App::Approvals,
            &[
                "decide",
                "--approval",
                &expired,
                "--decision",
                "approve",
                "--expected-version",
                "1",
                "--actor",
                "human"
            ]
        )
        .0,
        2
    );
    assert!(
        f.ok(App::Approvals, &["expire", "--json"])
            .contains(&expired)
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    f.ok(App::Approvals, &["expire", "--apply"]);
    assert_eq!(
        load_approvals(&f.0)
            .unwrap()
            .last()
            .unwrap()
            .get("state")
            .text(),
        Some("expired")
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn python_numbers_and_parse_errors_before_effects() {
    use crate::value::parse_integer;
    for raw in ["0", "-0", "+000", "٠", "０"] {
        assert_eq!(parse_integer(raw).unwrap().py(), "0");
    }
    assert_eq!(parse_integer(" +١_٢３ ").unwrap().py(), "123");
    for raw in ["", "_1", "1_", "1__0", "1.0", "1e1"] {
        assert!(parse_integer(raw).is_err());
    }
    assert_eq!(V::Bool(true), V::Num("1.0".into()));
    assert_ne!(
        V::Num("9007199254740993".into()),
        V::Num("9007199254740992.0".into())
    );
    let f = Fixture::new();
    assert_eq!(
        f.call(
            App::Events,
            &[
                "start",
                "--tool",
                "fixture",
                "--max-tokens",
                "bad",
                "--help"
            ]
        )
        .0,
        2
    );
    assert!(!event_path(&f.0).exists());
    assert_eq!(
        f.call(
            App::Approvals,
            &[
                "request",
                "--action",
                "fixture",
                "--object-id",
                "one",
                "--summary",
                "-x"
            ]
        )
        .0,
        2
    );
    assert_eq!(f.call(App::Events, &["list", "--help=x"]).0, 2);
    let id = f.ok(
        App::Events,
        &["start", "--tool", "fixture", "--max-tokens", "1_000"],
    );
    assert!(!id.is_empty());
    assert_eq!(
        f.ok(
            App::Events,
            &["follow", "--timeout", "0", "--interval", "-1"]
        )
        .lines()
        .count(),
        1
    );
    assert_eq!(
        f.call(
            App::Events,
            &["follow", "--timeout", "1", "--interval", "-1"]
        )
        .0,
        2
    );
}

#[test]
fn atomic_unchanged_bytes_skip_rewrite_but_still_reconcile_mode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = Fixture::new();
    let path = f.0.join("sub/state.json");
    let alias = f.0.join("sub/alias.json");
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    storage::atomic(&path, b"{}\n", 0o600, false).unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();
    storage::atomic(&path, b"{}\n", 0o600, false).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(mode(&path), 0o600);
    // Unchanged path must still execute Ledger::open hardlink/durability verification.
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(storage::atomic(&path, b"{}\n", 0o600, false).is_err());
    std::fs::remove_file(&alias).unwrap();
    storage::atomic(&path, b"{}\n", 0o600, false).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    storage::atomic(&path, b"{}\n", 0o640, false).unwrap();
    assert_eq!(mode(&path), 0o640);
    storage::atomic(&path, b"{\"a\":1}\n", 0o640, false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":1}\n");
}

#[test]
fn compact_converges_stray_attempt_dirs_and_keeps_unexpected_layouts() {
    let f = Fixture::new();
    let live = f.ok(App::Events, &["start", "--tool", "live"]);
    let path = event_path(&f.0);
    let before = std::fs::read(&path).unwrap();
    let attempts = f.0.join(".oms/lifecycle/attempts");
    std::fs::create_dir_all(&attempts).unwrap();
    let live_dir = attempts.join(&live);
    std::fs::create_dir_all(&live_dir).unwrap();
    let stray = attempts.join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("spec.json"), b"{}").unwrap();
    let nested = attempts.join("nested");
    std::fs::create_dir_all(nested.join("inner")).unwrap();
    std::fs::write(nested.join("spec.json"), b"{}").unwrap();
    let linked = attempts.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(&f.0, linked.join("escape")).unwrap();
    let crowded = attempts.join("crowded");
    std::fs::create_dir_all(&crowded).unwrap();
    for n in 0..65 {
        std::fs::write(crowded.join(format!("f{n}")), b"").unwrap();
    }
    let at_limit = attempts.join("at-limit");
    std::fs::create_dir_all(&at_limit).unwrap();
    for n in 0..64 {
        std::fs::write(at_limit.join(format!("f{n}")), b"").unwrap();
    }
    let dry = f.ok(App::Events, &["compact", "--days", "0"]);
    assert!(dry.contains("2 attempt dir(s) to remove"), "{dry}");
    assert!(stray.exists());
    let applied = f.ok(App::Events, &["compact", "--days", "0", "--apply"]);
    assert!(applied.contains("removed 2 attempt dir(s)"), "{applied}");
    assert!(!stray.exists() && !at_limit.exists());
    assert!(live_dir.exists() && nested.join("inner").is_dir());
    assert!(linked.join("escape").exists() && crowded.exists());
    assert_eq!(std::fs::read(&path).unwrap(), before, "ledger untouched");
    assert!(
        f.ok(App::Events, &["compact", "--days", "0", "--apply"])
            .contains("nothing to drop")
    );
    // Descriptor-anchored removal refuses a symlinked attempt directory swap.
    let victim = f.0.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), b"keep").unwrap();
    let swapped = attempts.join("swapped");
    std::os::unix::fs::symlink(&victim, &swapped).unwrap();
    assert!(!crate::events::remove_attempt_dir(&f.0, &swapped).unwrap());
    assert!(victim.join("keep.txt").is_file());
    assert!(swapped.is_symlink());
}

#[test]
fn transition_batch_refuses_invalid_suffix_before_publication() {
    let f = Fixture::new();
    let id = f.ok(App::Events, &["start", "--tool", "fixture"]);
    let path = event_path(&f.0);
    let before = std::fs::read(&path).unwrap();
    let (code, _, _) = f.call(
        App::Events,
        &[
            "transition",
            "--attempt",
            &id,
            "--state",
            "starting",
            "--then",
            "done:bad",
        ],
    );
    assert_eq!(code, 2);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let words = [
        "transition",
        "--attempt",
        &id,
        "--state",
        "starting",
        "--idempotency-key",
        "one",
        "--then",
        "working:two",
    ];
    f.ok(App::Events, &words);
    let after = std::fs::read(&path).unwrap();
    f.ok(App::Events, &words);
    assert_eq!(std::fs::read(&path).unwrap(), after);
    assert_eq!(read_rows(&path).unwrap().len(), 3);
}

#[test]
fn incremental_follow_reads_only_new_rows_and_reloads_generations() {
    use std::io::Write;
    let f = Fixture::new();
    let path = f.0.join("follow.jsonl");
    let mut tail = storage::FollowTail::default();
    assert!(tail.read(&path).unwrap().1.is_empty());
    std::fs::write(&path, b"{\"n\":1}\n").unwrap();
    let (reset, rows) = tail.read(&path).unwrap();
    assert!(reset);
    assert_eq!(rows.len(), 1);
    assert!(tail.read(&path).unwrap().1.is_empty());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"n\":2}\n")
        .unwrap();
    let (reset, rows) = tail.read(&path).unwrap();
    assert!(!reset);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.get("n"), &V::from(2usize));
    std::fs::write(&path, b"{}\n").unwrap();
    assert!(tail.read(&path).unwrap().0);
    let replacement = f.0.join("replacement");
    std::fs::write(&replacement, b"{\"n\":3}\n").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert!(tail.read(&path).unwrap().0);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"bad\n")
        .unwrap();
    assert!(tail.read(&path).is_err());
}

#[test]
fn lf_parser_borrows_and_batch_uses_verified_snapshot() {
    use std::borrow::Cow;
    assert!(matches!(storage::normalized(b"{}\n"), Cow::Borrowed(_)));
    assert!(matches!(storage::normalized(b"{}\r"), Cow::Owned(_)));
    for bytes in [b"{}\n{}\n".as_slice(), b"{}\r\n{}\r", b"\n\n{}"] {
        assert!(
            !storage::parse_rows(std::path::Path::new("rows"), bytes)
                .unwrap()
                .is_empty()
        );
    }
    assert!(storage::parse_rows(std::path::Path::new("rows"), b"bad\n").is_err());
    let f = Fixture::new();
    let path = f.0.join("batch.jsonl");
    std::fs::write(&path, b"{}").unwrap();
    let mut ledger = storage::Ledger::open(&path, true, false).unwrap();
    assert!(ledger.append_batch(&mut [obj([])], &[]).is_err());
    let history = storage::parse_rows(&path, &ledger.read().unwrap()).unwrap();
    ledger.append_batch(&mut [], &history).unwrap();
    ledger
        .append_batch(&mut [obj([]), obj([])], &history)
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{}\n{}\n{}\n");
    let mut stale = storage::Ledger::open(&path, true, false).unwrap();
    stale.read().unwrap();
    storage::append_row(&path, &mut obj([]), false).unwrap();
    assert!(stale.append_batch(&mut [obj([])], &history).is_err());
}
