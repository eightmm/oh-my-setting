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

fn synth_row(ev_num: usize, att: &str, seq: usize, kind: &str, extra: &str) -> String {
    format!(
        "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:00Z\",\"attempt_id\":\"{att}\",\"seq\":{seq},\"event_type\":\"{kind}\"{extra}}}\n"
    )
}

fn append_raw(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn assert_list_json_parity(cache: &EventsProjectionCache, repo: &std::path::Path) -> Vec<u8> {
    let idle = AtomicBool::new(false);
    let cached = cache.list_json(repo, &idle).unwrap();
    let cold = run(
        App::Events,
        &[OsString::from("list"), OsString::from("--json")],
        repo,
        &idle,
    );
    assert_eq!(cold.0, 0, "{}", String::from_utf8_lossy(&cold.2));
    assert_eq!(
        cached,
        cold.1,
        "list_json mismatch: cached={} cold={}",
        String::from_utf8_lossy(&cached),
        String::from_utf8_lossy(&cold.1),
    );
    cached
}

#[test]
fn incremental_list_json_parity_and_single_repo_slot() {
    use crate::incremental::SyncDisposition;

    let idle = AtomicBool::new(false);
    let cache = EventsProjectionCache::new();
    let repo_a = Fixture::new();

    // Missing/empty ledger -> Empty, !is_cached(), exact list --json parity.
    let (disp, rows) = cache.list_with_disposition(&repo_a.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Empty);
    assert!(rows.is_empty());
    assert!(!cache.is_cached());
    assert_eq!(assert_list_json_parity(&cache, &repo_a.0), b"[]\n\n");
    assert!(!cache.is_cached());

    // Cold -> Unchanged -> Incremental +1 usage & +2 transitions -> equal-updated_at tie-breaking.
    let id1 = create_attempt(
        &repo_a.0,
        &StartRequest {
            tool: "alpha".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    for att in ["att_tie_a", "att_tie_b"] {
        let mut created = new_event(
            att,
            1usize.into(),
            "attempt.created",
            obj([
                ("from_state", V::Null),
                ("to_state", "queued".into()),
                ("provider", "local".into()),
                ("tool", "tie".into()),
            ]),
        )
        .unwrap();
        created.set("ts", "2026-01-01T00:00:00Z");
        append_lifecycle(&repo_a.0, created, &idle).unwrap();
    }
    for (seq, state, ts) in [
        (2usize, "starting", "2026-01-02T00:00:00Z"),
        (3usize, "working", "2026-01-02T00:00:01Z"),
    ] {
        let mut tr = new_event(
            &id1,
            seq.into(),
            "attempt.state_changed",
            obj([("to_state", state.into())]),
        )
        .unwrap();
        tr.set("ts", ts);
        append_lifecycle(&repo_a.0, tr, &idle).unwrap();
    }

    let (disp, listed) = cache.list_with_disposition(&repo_a.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(listed.len(), 3);
    assert!(cache.is_cached());
    assert_eq!(listed[0].get("attempt_id").text(), Some("att_tie_a"));
    assert_eq!(listed[1].get("attempt_id").text(), Some("att_tie_b"));
    assert_eq!(listed[2].get("attempt_id").text(), Some(id1.as_str()));
    assert_eq!(
        cache.list_with_disposition(&repo_a.0, &idle).unwrap().0,
        SyncDisposition::Unchanged
    );
    assert_list_json_parity(&cache, &repo_a.0);

    // +1 usage row -> Incremental { appended_rows: 1 }.
    let mut usage1 = new_event(
        &id1,
        4usize.into(),
        "attempt.usage",
        obj([(
            "usage",
            obj([
                ("tokens", 42usize.into()),
                ("duration_ms", 7usize.into()),
                ("cost_microusd", 3usize.into()),
            ]),
        )]),
    )
    .unwrap();
    usage1.set("ts", "2026-01-02T00:00:02Z");
    append_lifecycle(&repo_a.0, usage1, &idle).unwrap();
    let (disp, listed) = cache.list_with_disposition(&repo_a.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Incremental { appended_rows: 1 });
    assert_eq!(listed[2].get("usage").get("tokens").py(), "42");
    assert_list_json_parity(&cache, &repo_a.0);

    // +2 transition rows -> Incremental { appended_rows: 2 }.
    for (seq, state, ts) in [
        (5usize, "review", "2026-01-02T00:00:03Z"),
        (6usize, "done", "2026-01-02T00:00:04Z"),
    ] {
        let mut tr = new_event(
            &id1,
            seq.into(),
            "attempt.state_changed",
            obj([("to_state", state.into())]),
        )
        .unwrap();
        tr.set("ts", ts);
        append_lifecycle(&repo_a.0, tr, &idle).unwrap();
    }
    let (disp, listed) = cache.list_with_disposition(&repo_a.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Incremental { appended_rows: 2 });
    assert_eq!(listed[2].get("state").text(), Some("done"));
    assert_list_json_parity(&cache, &repo_a.0);

    // Canonical symlink & ./sub/.. alias hit Unchanged; switching repo replaces slot.
    let sub_holder = Fixture::new();
    std::fs::create_dir(repo_a.0.join("sub")).unwrap();
    let alias_dir = sub_holder.0.join("repo_alias");
    std::os::unix::fs::symlink(&repo_a.0, &alias_dir).unwrap();
    assert_eq!(
        cache.list_with_disposition(&alias_dir, &idle).unwrap().0,
        SyncDisposition::Unchanged
    );
    assert_eq!(
        cache
            .list_with_disposition(&repo_a.0.join("./sub/.."), &idle)
            .unwrap()
            .0,
        SyncDisposition::Unchanged
    );
    assert!(cache.is_cached());

    let repo_b = Fixture::new();
    let id_b = create_attempt(
        &repo_b.0,
        &StartRequest {
            tool: "beta".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    let (disp_b, rows_b) = cache.list_with_disposition(&repo_b.0, &idle).unwrap();
    assert_eq!(disp_b, SyncDisposition::Cold);
    assert_eq!(rows_b.len(), 1);
    assert_eq!(rows_b[0].get("attempt_id").text(), Some(id_b.as_str()));
    assert_list_json_parity(&cache, &repo_b.0);
    assert_eq!(
        cache.list_with_disposition(&repo_a.0, &idle).unwrap().0,
        SyncDisposition::Cold
    );
    assert_list_json_parity(&cache, &repo_a.0);
}

#[test]
fn incremental_prefix_hash_read_fence_and_stale_negatives() {
    use crate::incremental::SyncDisposition;
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Arc, atomic::Ordering};

    let idle = AtomicBool::new(false);
    let f = Fixture::new();

    // Capped fallible read allocation + sparse huge file cancellation without abort.
    let huge_vec = storage::new_read_vec(1_u64 << 40).unwrap();
    assert!(huge_vec.capacity() <= 131_072);
    assert!(storage::new_read_vec((isize::MAX as u64) + 1).is_err());

    let sparse_repo = Fixture::new();
    let sparse_path = event_path(&sparse_repo.0);
    std::fs::create_dir_all(sparse_path.parent().unwrap()).unwrap();
    {
        let sparse = std::fs::File::create(&sparse_path).unwrap();
        sparse.set_len(1_u64 << 30).unwrap();
    }
    let mut sparse_file = std::fs::File::open(&sparse_path).unwrap();
    let mut sparse_buf = vec![0u8; 4096];
    let mut sparse_out = storage::new_read_vec(1_u64 << 30).unwrap();
    assert!(sparse_out.capacity() <= 131_072);
    let mut sparse_hasher = Sha256::new();
    assert_eq!(
        storage::read_into(
            &mut sparse_file,
            &mut sparse_buf,
            1_u64 << 30,
            &mut sparse_hasher,
            &mut sparse_out,
            &AtomicBool::new(true),
        )
        .unwrap_err(),
        "operation interrupted"
    );
    let sparse_cache = EventsProjectionCache::new();
    assert_eq!(
        sparse_cache
            .list_json(&sparse_repo.0, &AtomicBool::new(true))
            .unwrap_err(),
        "operation interrupted"
    );

    // Direct storage::read_into byte-length + 1-byte probe fence.
    let fence_path = f.0.join("fence.bin");
    std::fs::write(&fence_path, vec![b'x'; 10_000]).unwrap();
    let mut fence_file = std::fs::File::open(&fence_path).unwrap();
    let mut fence_buf = vec![0u8; 4096];
    let mut hasher = Sha256::new();
    let mut out = Vec::new();
    assert!(
        !storage::read_into(
            &mut fence_file,
            &mut fence_buf,
            1_000,
            &mut hasher,
            &mut out,
            &idle
        )
        .unwrap()
    );
    assert_eq!(out.len(), 1_000);
    assert_eq!(fence_file.stream_position().unwrap(), 1_001);

    fence_file.seek(SeekFrom::Start(0)).unwrap();
    out.clear();
    hasher = Sha256::new();
    assert!(
        !storage::read_into(
            &mut fence_file,
            &mut fence_buf,
            20_000,
            &mut hasher,
            &mut out,
            &idle
        )
        .unwrap()
    );

    fence_file.seek(SeekFrom::Start(0)).unwrap();
    out.clear();
    hasher = Sha256::new();
    assert!(
        storage::read_into(
            &mut fence_file,
            &mut fence_buf,
            10_000,
            &mut hasher,
            &mut out,
            &idle
        )
        .unwrap()
    );
    assert_eq!(out.len(), 10_000);

    // Bounded continuous-writer thread test + post-stop convergence parity.
    let repo_race = Fixture::new();
    let race_id = create_attempt(
        &repo_race.0,
        &StartRequest {
            tool: "race".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    let race_path = event_path(&repo_race.0);
    let race_cache = EventsProjectionCache::new();
    let stop_writer = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop_writer);
    let writer_path = race_path.clone();
    let writer_id = race_id.clone();
    let writer = std::thread::spawn(move || {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&writer_path)
            .unwrap();
        let mut seq = 2usize;
        while !stop_flag.load(Ordering::Relaxed) && seq <= 10_000 {
            let row = synth_row(seq, &writer_id, seq, "attempt.heartbeat", "");
            f.write_all(row.as_bytes()).unwrap();
            f.flush().unwrap();
            seq += 1;
            std::thread::yield_now();
        }
    });
    for _ in 0..80 {
        match race_cache.list_json(&repo_race.0, &idle) {
            Ok(bytes) => assert!(bytes.starts_with(b"[") && bytes.ends_with(b"\n")),
            Err(err) => assert_eq!(err, "ledger changed during access"),
        }
        std::thread::yield_now();
    }
    stop_writer.store(true, Ordering::Release);
    writer.join().unwrap();
    assert_list_json_parity(&race_cache, &repo_race.0);

    // Same-inode middle-prefix rewrite (> 24 KiB, identical 8 KiB head & tail), same-size & grown.
    let mid_cache = EventsProjectionCache::new();
    let repo_mid = Fixture::new();
    let mid_path = event_path(&repo_mid.0);
    std::fs::create_dir_all(mid_path.parent().unwrap()).unwrap();
    let mut mid_buf = synth_row(
        1,
        "att_mid",
        1,
        "attempt.created",
        ",\"from_state\":null,\"to_state\":\"queued\",\"provider\":\"local\",\"tool\":\"mid\"",
    );
    let mut middle_offset = 0usize;
    for seq in 2..=200 {
        if seq == 100 {
            let line = synth_row(
                seq,
                "att_mid",
                seq,
                "attempt.usage",
                ",\"usage\":{\"tokens\":1000,\"duration_ms\":10,\"cost_microusd\":1}",
            );
            middle_offset = mid_buf.len() + line.find("1000").unwrap();
            mid_buf.push_str(&line);
        } else {
            mid_buf.push_str(&synth_row(seq, "att_mid", seq, "attempt.heartbeat", ""));
        }
    }
    assert!(mid_buf.len() > 24 * 1024);
    assert!(middle_offset > 8192 && middle_offset + 4 < mid_buf.len() - 8192);
    std::fs::write(&mid_path, mid_buf.as_bytes()).unwrap();
    let mid_ino = std::fs::metadata(&mid_path).unwrap().ino();
    let mid_len = std::fs::metadata(&mid_path).unwrap().len();
    let orig_bytes = std::fs::read(&mid_path).unwrap();

    let (disp, rows) = mid_cache.list_with_disposition(&repo_mid.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("usage").get("tokens").py(), "1000");

    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&mid_path)
            .unwrap();
        file.seek(SeekFrom::Start(middle_offset as u64)).unwrap();
        file.write_all(b"2000").unwrap();
        file.sync_all().unwrap();
    }
    let after_same = std::fs::read(&mid_path).unwrap();
    assert_eq!(std::fs::metadata(&mid_path).unwrap().ino(), mid_ino);
    assert_eq!(std::fs::metadata(&mid_path).unwrap().len(), mid_len);
    assert_eq!(&after_same[..8192], &orig_bytes[..8192]);
    assert_eq!(
        &after_same[after_same.len() - 8192..],
        &orig_bytes[orig_bytes.len() - 8192..]
    );
    let (disp, rows) = mid_cache.list_with_disposition(&repo_mid.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("usage").get("tokens").py(), "2000");
    assert_list_json_parity(&mid_cache, &repo_mid.0);

    let extra_row = synth_row(201, "att_mid", 201, "attempt.heartbeat", "");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&mid_path)
            .unwrap();
        file.seek(SeekFrom::Start(middle_offset as u64)).unwrap();
        file.write_all(b"3000").unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(extra_row.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    let after_grow = std::fs::read(&mid_path).unwrap();
    assert_eq!(std::fs::metadata(&mid_path).unwrap().ino(), mid_ino);
    assert!(after_grow.len() as u64 > mid_len);
    assert_eq!(&after_grow[..8192], &orig_bytes[..8192]);
    assert_eq!(
        &after_grow[mid_len as usize - 8192..mid_len as usize],
        &orig_bytes[orig_bytes.len() - 8192..]
    );
    let (disp, rows) = mid_cache.list_with_disposition(&repo_mid.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("usage").get("tokens").py(), "3000");
    assert_list_json_parity(&mid_cache, &repo_mid.0);

    // Valid unterminated EOF -> Cold on append_lifecycle.
    let eof_cache = EventsProjectionCache::new();
    let repo_eof = Fixture::new();
    let eof_id = create_attempt(
        &repo_eof.0,
        &StartRequest {
            tool: "eof".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    let eof_path = event_path(&repo_eof.0);
    let row_unterm = format!(
        "{{\"schema\":1,\"event_id\":\"aevt_{:032x}\",\"ts\":\"2026-01-01T00:00:05Z\",\"attempt_id\":\"{eof_id}\",\"seq\":2,\"event_type\":\"attempt.heartbeat\"}}",
        9001usize
    );
    append_raw(&eof_path, row_unterm.as_bytes());
    let (disp, rows) = eof_cache.list_with_disposition(&repo_eof.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "2");
    let mut hb3 = new_event(&eof_id, 3usize.into(), "attempt.heartbeat", obj([])).unwrap();
    hb3.set("ts", "2026-01-01T00:00:06Z");
    append_lifecycle(&repo_eof.0, hb3, &idle).unwrap();
    let (disp, rows) = eof_cache.list_with_disposition(&repo_eof.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "3");
    assert_list_json_parity(&eof_cache, &repo_eof.0);

    // Atomic new-inode rewrites, set_len truncation, deletion, hardlink/symlink swaps, repo wipe/recreate.
    let neg_cache = EventsProjectionCache::new();
    let repo_neg = Fixture::new();
    let neg_id = create_attempt(
        &repo_neg.0,
        &StartRequest {
            tool: "neg".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    let neg_path = event_path(&repo_neg.0);
    assert_eq!(
        neg_cache
            .list_with_disposition(&repo_neg.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    let orig_neg = std::fs::read(&neg_path).unwrap();
    let orig_ino = std::fs::metadata(&neg_path).unwrap().ino();
    let same_len_neg = String::from_utf8(orig_neg.clone())
        .unwrap()
        .replacen("\"tool\":\"neg\"", "\"tool\":\"new\"", 1)
        .into_bytes();
    assert_eq!(same_len_neg.len(), orig_neg.len());
    storage::atomic(&neg_path, &same_len_neg, 0o644, false).unwrap();
    assert_ne!(std::fs::metadata(&neg_path).unwrap().ino(), orig_ino);
    let (disp, rows) = neg_cache.list_with_disposition(&repo_neg.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("tool").text(), Some("new"));

    let mut grown_neg = orig_neg.clone();
    grown_neg.extend_from_slice(synth_row(4001, &neg_id, 2, "attempt.heartbeat", "").as_bytes());
    let before_grow_ino = std::fs::metadata(&neg_path).unwrap().ino();
    storage::atomic(&neg_path, &grown_neg, 0o644, false).unwrap();
    assert_ne!(std::fs::metadata(&neg_path).unwrap().ino(), before_grow_ino);
    let (disp, rows) = neg_cache.list_with_disposition(&repo_neg.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "2");

    std::fs::OpenOptions::new()
        .write(true)
        .open(&neg_path)
        .unwrap()
        .set_len(orig_neg.len() as u64)
        .unwrap();
    let (disp, rows) = neg_cache.list_with_disposition(&repo_neg.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "1");

    let hardlink_alias = neg_path.with_file_name("events.hardlink");
    std::fs::hard_link(&neg_path, &hardlink_alias).unwrap();
    assert!(neg_cache.list_with_disposition(&repo_neg.0, &idle).is_err());
    assert!(!neg_cache.is_cached());
    std::fs::remove_file(&hardlink_alias).unwrap();

    assert_eq!(
        neg_cache
            .list_with_disposition(&repo_neg.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    std::fs::remove_file(&neg_path).unwrap();
    assert_eq!(
        neg_cache
            .list_with_disposition(&repo_neg.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Empty
    );
    assert!(!neg_cache.is_cached());

    std::fs::write(&neg_path, &orig_neg).unwrap();
    let swap_targets = [
        (neg_path.clone(), repo_neg.0.join("real_events.jsonl")),
        (
            repo_neg.0.join(".oms/lifecycle"),
            repo_neg.0.join(".oms/lifecycle_real"),
        ),
        (repo_neg.0.join(".oms"), repo_neg.0.join(".oms_real")),
    ];
    for (live_path, backup_path) in swap_targets {
        assert_eq!(
            neg_cache
                .list_with_disposition(&repo_neg.0, &idle)
                .unwrap()
                .0,
            SyncDisposition::Cold
        );
        std::fs::rename(&live_path, &backup_path).unwrap();
        std::os::unix::fs::symlink(&backup_path, &live_path).unwrap();
        assert!(neg_cache.list_with_disposition(&repo_neg.0, &idle).is_err());
        assert!(!neg_cache.is_cached());
        std::fs::remove_file(&live_path).unwrap();
        std::fs::rename(&backup_path, &live_path).unwrap();
    }

    assert_eq!(
        neg_cache
            .list_with_disposition(&repo_neg.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    std::fs::remove_dir_all(&repo_neg.0).unwrap();
    assert!(neg_cache.list_with_disposition(&repo_neg.0, &idle).is_err());
    assert!(!neg_cache.is_cached());
    std::fs::create_dir(&repo_neg.0).unwrap();
    assert_eq!(
        neg_cache
            .list_with_disposition(&repo_neg.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Empty
    );

    let _old_recreated = create_attempt(
        &repo_neg.0,
        &StartRequest {
            tool: "before-wipe".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    assert_eq!(
        neg_cache
            .list_with_disposition(&repo_neg.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    std::fs::remove_dir_all(&repo_neg.0).unwrap();
    std::fs::create_dir(&repo_neg.0).unwrap();
    let new_recreated = create_attempt(
        &repo_neg.0,
        &StartRequest {
            tool: "after-wipe".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    let (disp, rows) = neg_cache.list_with_disposition(&repo_neg.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("attempt_id").text(),
        Some(new_recreated.as_str())
    );
}

#[test]
fn incremental_cache_drops_on_partial_replay_malformed_eof_and_cancellation() {
    use crate::incremental::{SyncDisposition, apply_snapshot};
    use std::sync::atomic::Ordering;

    let idle = AtomicBool::new(false);
    let repo_err = Fixture::new();
    let err_id = create_attempt(
        &repo_err.0,
        &StartRequest {
            tool: "err".into(),
            ..Default::default()
        },
        &idle,
    )
    .unwrap();
    let err_path = event_path(&repo_err.0);

    // Direct apply_snapshot(Incremental, None) returns Err without panicking.
    let orphan_inc = storage::EventsReadSnapshot::Incremental {
        canonical_repo: repo_err.0.clone(),
        path: err_path.clone(),
        meta: storage::LedgerPrefixMeta {
            repo_id: (0, 0),
            oms_id: (0, 0),
            parent_id: (0, 0),
            file_id: (0, 0),
            offset: 0,
            prefix_sha256: [0; 32],
            unterminated: false,
        },
        delta: Vec::new(),
    };
    assert!(apply_snapshot(orphan_inc, None, &idle).is_err());

    // Torn partial JSON EOF -> exact cold diagnostic, !is_cached(), recovery after fix.
    let mut hb2 = new_event(&err_id, 2usize.into(), "attempt.heartbeat", obj([])).unwrap();
    hb2.set("ts", "2026-01-01T00:00:02Z");
    append_lifecycle(&repo_err.0, hb2, &idle).unwrap();
    let err_cache = EventsProjectionCache::new();
    assert_eq!(
        err_cache
            .list_with_disposition(&repo_err.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    let len_after_2 = std::fs::metadata(&err_path).unwrap().len();
    append_raw(&err_path, b"{\"schema\":1");
    let torn_err = err_cache.list_json(&repo_err.0, &idle).unwrap_err();
    let torn_cold = run(
        App::Events,
        &[OsString::from("list"), OsString::from("--json")],
        &repo_err.0,
        &idle,
    );
    assert_eq!(torn_cold.0, 2);
    assert!(torn_err.contains("is not valid JSON"), "{torn_err}");
    assert_eq!(
        format!("error: {torn_err}\n"),
        String::from_utf8_lossy(&torn_cold.2)
    );
    assert!(!err_cache.is_cached());
    let torn_tail = format!(
        ",\"event_id\":\"aevt_{:032x}\",\"ts\":\"2026-01-01T00:00:03Z\",\"attempt_id\":\"{err_id}\",\"seq\":3,\"event_type\":\"attempt.heartbeat\"}}\n",
        3003usize
    );
    append_raw(&err_path, torn_tail.as_bytes());
    let (disp, rows) = err_cache.list_with_disposition(&repo_err.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "3");
    assert!(len_after_2 < std::fs::metadata(&err_path).unwrap().len());

    // Sequence gap after valid appended row -> drops cache, identical retry error, truncating recovers.
    append_raw(
        &err_path,
        synth_row(3004, &err_id, 4, "attempt.heartbeat", "").as_bytes(),
    );
    let len_after_4 = std::fs::metadata(&err_path).unwrap().len();
    append_raw(
        &err_path,
        synth_row(3005, &err_id, 99, "attempt.heartbeat", "").as_bytes(),
    );
    let err1 = err_cache
        .list_with_disposition(&repo_err.0, &idle)
        .unwrap_err();
    assert!(err1.contains("sequence is 99, expected 5"), "{err1}");
    assert!(!err_cache.is_cached());
    let err2 = err_cache
        .list_with_disposition(&repo_err.0, &idle)
        .unwrap_err();
    assert_eq!(err2, err1);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&err_path)
        .unwrap()
        .set_len(len_after_4)
        .unwrap();
    let (disp, rows) = err_cache.list_with_disposition(&repo_err.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "4");

    // Duplicate event_id after valid appended row -> drops cache, identical retry error, truncating recovers.
    append_raw(
        &err_path,
        synth_row(3006, &err_id, 5, "attempt.heartbeat", "").as_bytes(),
    );
    let len_after_5 = std::fs::metadata(&err_path).unwrap().len();
    append_raw(
        &err_path,
        synth_row(3006, &err_id, 6, "attempt.heartbeat", "").as_bytes(),
    );
    let dup1 = err_cache
        .list_with_disposition(&repo_err.0, &idle)
        .unwrap_err();
    assert!(dup1.contains("duplicate lifecycle event_id"), "{dup1}");
    assert!(!err_cache.is_cached());
    let dup2 = err_cache
        .list_with_disposition(&repo_err.0, &idle)
        .unwrap_err();
    assert_eq!(dup2, dup1);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&err_path)
        .unwrap()
        .set_len(len_after_5)
        .unwrap();
    let (disp, rows) = err_cache.list_with_disposition(&repo_err.0, &idle).unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "5");

    // Pre-call cancellation on warm cache drops entry and next uncancelled read reloads Cold.
    let stopped = AtomicBool::new(true);
    assert_eq!(
        err_cache
            .list_with_disposition(&repo_err.0, &stopped)
            .unwrap_err(),
        "operation interrupted"
    );
    assert!(!err_cache.is_cached());
    assert_eq!(
        err_cache
            .list_with_disposition(&repo_err.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    assert_eq!(
        storage::parse_rows_at(&err_path, b"{}\n", 0, &stopped).unwrap_err(),
        "operation interrupted"
    );
    let mut interrupted_proj = projection::Projection::default();
    assert_eq!(
        interrupted_proj
            .extend_checked(&read_rows(&err_path).unwrap(), &stopped)
            .unwrap_err(),
        "operation interrupted"
    );

    // Post-projection / post-sort cancellation on Cold path drops entry before cache restore.
    let cancel_cache = EventsProjectionCache::new();
    let cancel_cold = AtomicBool::new(false);
    let cold_cancel_err = cancel_cache
        .sync_with(&repo_err.0, &cancel_cold, |_| {
            cancel_cold.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap_err();
    assert_eq!(cold_cancel_err, "operation interrupted");
    assert!(!cancel_cache.is_cached());

    // Warm Cold load, append valid row 6, then cancel post-projection/post-sort on Incremental path.
    assert_eq!(
        cancel_cache
            .list_with_disposition(&repo_err.0, &idle)
            .unwrap()
            .0,
        SyncDisposition::Cold
    );
    assert!(cancel_cache.is_cached());
    append_raw(
        &err_path,
        synth_row(3007, &err_id, 6, "attempt.heartbeat", "").as_bytes(),
    );
    let cancel_inc = AtomicBool::new(false);
    let inc_cancel_err = cancel_cache
        .sync_with(&repo_err.0, &cancel_inc, |_| {
            cancel_inc.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap_err();
    assert_eq!(inc_cancel_err, "operation interrupted");
    assert!(!cancel_cache.is_cached());
    let (disp, rows) = cancel_cache
        .list_with_disposition(&repo_err.0, &idle)
        .unwrap();
    assert_eq!(disp, SyncDisposition::Cold);
    assert_eq!(rows[0].get("sequence").py(), "6");
}

#[test]
#[ignore = "opt-in 107k-row local benchmark; run with --ignored --nocapture"]
fn incremental_events_107k_append_vs_cold_measurement() {
    use std::fmt::Write as _;
    use std::time::Instant;

    let f = Fixture::new();
    let path = event_path(&f.0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let attempts = 200usize;
    let events_per_attempt = 535usize;
    let mut buf = String::with_capacity(attempts * events_per_attempt * 200);
    let mut ev_num = 1usize;

    for a in 0..attempts {
        let att_id = format!("att_{a:04}");
        writeln!(
            buf,
            "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:00Z\",\"attempt_id\":\"{att_id}\",\"seq\":1,\"event_type\":\"attempt.created\",\"from_state\":null,\"to_state\":\"queued\",\"provider\":\"local\",\"tool\":\"bench\"}}"
        )
        .unwrap();
        ev_num += 1;
        writeln!(
            buf,
            "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:00Z\",\"attempt_id\":\"{att_id}\",\"seq\":2,\"event_type\":\"attempt.state_changed\",\"from_state\":\"queued\",\"to_state\":\"starting\"}}"
        )
        .unwrap();
        ev_num += 1;
        writeln!(
            buf,
            "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:00Z\",\"attempt_id\":\"{att_id}\",\"seq\":3,\"event_type\":\"attempt.state_changed\",\"from_state\":\"starting\",\"to_state\":\"working\"}}"
        )
        .unwrap();
        ev_num += 1;
        writeln!(
            buf,
            "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:00Z\",\"attempt_id\":\"{att_id}\",\"seq\":4,\"event_type\":\"attempt.usage\",\"usage\":{{\"tokens\":10,\"duration_ms\":5,\"cost_microusd\":1}}}}"
        )
        .unwrap();
        ev_num += 1;
        for seq in 5..=events_per_attempt {
            writeln!(
                buf,
                "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:00Z\",\"attempt_id\":\"{att_id}\",\"seq\":{seq},\"event_type\":\"attempt.heartbeat\"}}"
            )
            .unwrap();
            ev_num += 1;
        }
    }
    assert_eq!(ev_num - 1, 107_000);
    std::fs::write(&path, buf.as_bytes()).unwrap();
    let total_bytes = std::fs::metadata(&path).unwrap().len();

    let idle = AtomicBool::new(false);
    let cache = EventsProjectionCache::new();
    let t_cold = Instant::now();
    let cold_json = cache.list_json(&f.0, &idle).unwrap();
    let cold_elapsed = t_cold.elapsed();
    assert!(!cold_json.is_empty());

    let extra_row = format!(
        "{{\"schema\":1,\"event_id\":\"aevt_{ev_num:032x}\",\"ts\":\"2026-01-01T00:00:01Z\",\"attempt_id\":\"att_0000\",\"seq\":536,\"event_type\":\"attempt.heartbeat\"}}\n"
    );
    append_raw(&path, extra_row.as_bytes());

    let t_inc = Instant::now();
    let inc_json = cache.list_json(&f.0, &idle).unwrap();
    let inc_elapsed = t_inc.elapsed();

    let fresh = EventsProjectionCache::new();
    let t_fresh = Instant::now();
    let fresh_json = fresh.list_json(&f.0, &idle).unwrap();
    let fresh_cold_elapsed = t_fresh.elapsed();

    let cli_cold = run(
        App::Events,
        &[OsString::from("list"), OsString::from("--json")],
        &f.0,
        &idle,
    );
    assert_eq!(cli_cold.0, 0, "{}", String::from_utf8_lossy(&cli_cold.2));
    assert_eq!(inc_json, fresh_json);
    assert_eq!(inc_json, cli_cold.1);

    let cold_ms = cold_elapsed.as_secs_f64() * 1000.0;
    let inc_ms = inc_elapsed.as_secs_f64() * 1000.0;
    let fresh_ms = fresh_cold_elapsed.as_secs_f64() * 1000.0;
    let speedup = fresh_cold_elapsed.as_secs_f64() / inc_elapsed.as_secs_f64();
    eprintln!(
        "[107k-bench] bytes={total_bytes} cold_ms={cold_ms:.2} inc_1_append_ms={inc_ms:.2} fresh_cold_ms={fresh_ms:.2} speedup={speedup:.2}x"
    );
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
