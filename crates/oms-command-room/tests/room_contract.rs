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
use oms_command_room::{
    LocalServices, Services, run_with_services,
    value::{Value, parse},
};
struct FixtureOperations<'a>(&'a [oms_command_room::Attempt]);
impl oms_command_room::OperationsRead for FixtureOperations<'_> {
    fn attempts(&self) -> &[oms_command_room::Attempt] {
        self.0
    }
    fn check(&self) -> oms_command_room::Result<()> {
        Ok(())
    }
}
struct FixtureServices<'a>(&'a [oms_command_room::Attempt]);
const EMPTY: FixtureServices = FixtureServices(&[]);
impl Services for FixtureServices<'_> {
    fn state_repository(&self, path: &Path) -> oms_command_room::Result<PathBuf> {
        LocalServices.state_repository(path)
    }
    fn canonical_provider(&self, provider: &str) -> oms_command_room::Result<String> {
        LocalServices.canonical_provider(provider)
    }
    fn operations<'a>(
        &'a self,
        _: &Path,
    ) -> oms_command_room::Result<Box<dyn oms_command_room::OperationsRead + 'a>> {
        Ok(Box::new(FixtureOperations(self.0)))
    }
}
fn run(repo: &Repo, values: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(&args(values), &repo.0, &STOP, &EMPTY)
}
fn success(output: (i32, Vec<u8>, Vec<u8>)) -> Value {
    assert_eq!(output.0, 0, "{}", String::from_utf8_lossy(&output.2));
    parse(&output.1, 1024 * 1024).unwrap()
}
#[test]
fn ownership_help_and_resolution() {
    isolated("ownership_help_and_resolution", || {
        let repo = Repo::new();
        assert!(oms_command_room::owns(&args(&["--invalid"])));
        let help = run(&repo, &["--help"]);
        assert_eq!(help.0, 0);
        assert!(String::from_utf8_lossy(&help.1).contains("--after"));
        repo.write("nested/file", "fixture");
        assert_eq!(
            LocalServices
                .state_repository(&repo.0.join("nested"))
                .unwrap(),
            repo.0
        );
        assert!(
            LocalServices
                .state_repository(&repo.0.join("absent"))
                .is_err()
        );
        assert!(
            LocalServices
                .state_repository(&repo.0.join("nested/file"))
                .is_err()
        );
        let managed = repo.0.join("oh-my-setting-scratch.fixture/wt-fixture");
        fs::create_dir_all(&managed).unwrap();
        let mut command = Command::new("git");
        assert!(
            clean_env_child(&mut command)
                .arg("-C")
                .arg(&managed)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            LocalServices
                .state_repository(&managed)
                .unwrap_err()
                .message
                .contains("managed scratch")
        );
    });
}
#[test]
fn desktop_participant_reader_pages_only_shared_messages() {
    isolated(
        "desktop_participant_reader_pages_only_shared_messages",
        || {
            let repo = Repo::new();
            success(run(
                &repo,
                &["new", "--id", "scope-room", "--participant", "owner"],
            ));
            let native = "01900000-0000-7000-8000-000000000002";
            for (id, provider, role) in [
                ("owner", "codex", "main"),
                ("agy-lead", "antigravity", "worker"),
                ("other", "claude", "worker"),
            ] {
                let mut argv = vec![
                    "join",
                    "--id",
                    "scope-room",
                    "--participant",
                    id,
                    "--provider",
                    provider,
                    "--role",
                    role,
                ];
                if id == "owner" {
                    argv.extend(["--native-session", native]);
                }
                success(run(&repo, &argv));
            }
            let post = |from: &str, to: &str, body: &str| {
                success(run(
                    &repo,
                    &[
                        "send",
                        "--id",
                        "scope-room",
                        "--participant",
                        from,
                        "--to",
                        to,
                        "--text",
                        body,
                    ],
                ))
            };
            post("other", "agy-lead", "private sibling text");
            post("owner", "other", "another participant only");
            for n in 1..=15 {
                post("agy-lead", "owner", &format!("answer {n}"));
            }
            use sha2::{Digest, Sha256};
            let hash = format!("{:x}", Sha256::digest(native.as_bytes()));
            let digest = &hash[..32];
            let room = oms_command_room::Room::open(
                &repo.0,
                Default::default(),
                &EMPTY,
                &STOP,
                false,
                false,
            )
            .unwrap();
            let latest = room
                .desktop_chat("scope-room", "agy-lead", digest, None)
                .unwrap();
            assert_eq!(latest.text("provider"), "antigravity");
            assert_eq!(latest.list("messages").len(), 12);
            assert_eq!(
                latest.list("messages").last().unwrap().text("text"),
                "answer 15"
            );
            let older = room
                .desktop_chat(
                    "scope-room",
                    "agy-lead",
                    digest,
                    latest["nextBefore"].as_u64(),
                )
                .unwrap();
            assert_eq!(older.list("messages").len(), 3);
            assert_eq!(older.list("messages")[0].text("text"), "answer 1");
            assert!(older["nextBefore"].is_null());
            for (id, who, selector) in [
                ("scope-room", "unknown", digest),
                ("unknown", "agy-lead", digest),
                ("scope-room", "agy-lead", "00000000000000000000000000000000"),
            ] {
                assert!(room.desktop_chat(id, who, selector, None).is_err());
            }
        },
    );
}

#[test]
fn room_lifecycle_and_cursor() {
    isolated("room_lifecycle_and_cursor", || {
        let repo = Repo::new();
        assert_eq!(
            success(run(&repo, &["new", "--id", "room-fixture", "--json"]))["id"].as_str(),
            Some("room-fixture")
        );
        let reserved_room = "oms-app-fixture";
        let status_args = args(&["desktop-start-status", "--id", reserved_room, "--json"]);
        let lookup = |attempts: &[oms_command_room::Attempt]| {
            success(run_with_services(
                &status_args,
                &repo.0,
                &STOP,
                &FixtureServices(attempts),
            ))
        };
        assert_eq!(lookup(&[]).text("status"), "not_found");
        success(run(&repo, &["new", "--id", reserved_room, "--json"]));
        let pending = lookup(&[]);
        assert_eq!(pending.text("status"), "reserved");
        assert!(pending["attempt"].is_null());
        let first = oms_command_room::Attempt {
            attempt_id: "first-attempt".into(),
            tool: "panel-main".into(),
            provider: "codex".into(),
            state: "done".into(),
            terminal: true,
            reason_code: None,
            created_at: String::new(),
            refs: oms_command_room::value::obj([
                ("panel_harness", "pi".into()),
                ("panel_role", "main".into()),
                ("panel_room_id", reserved_room.into()),
            ]),
        };
        let mut later = first.clone();
        later.attempt_id = "replacement".into();
        later.terminal = false;
        later.state = "working".into();
        let mut unrelated = first.clone();
        unrelated.refs.set("panel_room_id", "different-room");
        let mut history = vec![unrelated.clone(); 16];
        history.push(first);
        history.push(later);
        history.extend(vec![unrelated; 16]);
        let recorded = lookup(&history);
        assert_eq!(recorded.text("room"), reserved_room);
        assert_eq!(recorded.text("attempt"), "first-attempt");
        assert_eq!(recorded.text("status"), "recorded");
        assert_eq!(recorded.text("provider"), "pi");
        assert!(recorded.flag("terminal"));
        for who in ["owner", "reader"] {
            success(run(
                &repo,
                &[
                    "join",
                    "--id",
                    "room-fixture",
                    "--participant",
                    who,
                    "--provider",
                    "codex",
                    "--role",
                    if who == "owner" { "main" } else { "worker" },
                    "--json",
                ],
            ));
        }
        use sha2::{Digest, Sha256};
        let synthetic_thread = "01900000-0000-7000-8000-000000000001";
        let consumer = format!("{:x}", Sha256::digest(synthetic_thread.as_bytes()));
        let consumer = &consumer[..32];
        let chat = |selector: &str, attempts: &[oms_command_room::Attempt]| {
            let room = oms_command_room::Room::open(
                &repo.0,
                Default::default(),
                &EMPTY,
                &STOP,
                false,
                false,
            )
            .unwrap();
            room.conversation_main(
                selector,
                attempts
                    .iter()
                    .map(|a| oms_command_room::ConversationPanelAttempt {
                        room: a.refs.text("panel_room_id"),
                        participant: a.refs.text("panel_room_participant"),
                        terminal: a.terminal,
                    }),
            )
            .unwrap()
        };
        assert!(chat(consumer, &[])["conversationMain"].is_null());
        success(run(
            &repo,
            &[
                "bind",
                "--id",
                "room-fixture",
                "--participant",
                "owner",
                "--native-session",
                synthetic_thread,
            ],
        ));
        let log = repo.0.join(".oms/threads/room-fixture.jsonl");
        let before = fs::read(&log).unwrap();
        let linked = chat(consumer, &[]);
        assert_eq!(linked["conversationMain"].text("room"), "room-fixture");
        assert_eq!(linked["conversationMain"].text("participant"), "owner");
        assert_eq!(linked["conversationMain"].text("state"), "joined");
        assert_eq!(
            linked["conversationMain"].get("harness"),
            Some(&Value::Null)
        );
        assert_eq!(fs::read(&log).unwrap(), before);
        assert!(chat(&"0".repeat(32), &[])["conversationMain"].is_null());
        let mut terminated = history[16].clone();
        terminated.refs.set("panel_room_participant", "owner");
        terminated.refs.set("panel_room_id", "room-fixture");
        let mut attempts = vec![terminated];
        attempts.extend(vec![history[0].clone(); 20]);
        assert!(chat(consumer, &attempts)["conversationMain"].is_null());
        // A damaged unrelated log makes discovery partial without withdrawing a proven membership.
        let bad_room = repo.0.join(".oms/threads/unrelated-fixture.jsonl");
        fs::write(&bad_room, b"bad\n").unwrap();
        assert!(chat(consumer, &[])["conversationMain"].flag("incomplete"));
        // An incomplete unrelated scan and a pointer to an unfinished rollover
        // must not publish a Room before its canonical predecessor does.
        let core =
            oms_command_room::Room::open(&repo.0, Default::default(), &EMPTY, &STOP, false, false)
                .unwrap();
        let unpublished = oms_command_room::successor_id("room-fixture");
        core.create(
            Some(&unpublished),
            "pending rollover",
            None,
            None,
            Some("room-fixture"),
        )
        .unwrap();
        let state = core.status("room-fixture", true).unwrap();
        core.carry(&state, &unpublished, false).unwrap();
        let current = fs::read_to_string(repo.0.join(".oms/threads/CURRENT")).unwrap();
        assert_eq!(
            current.split_whitespace().next(),
            Some(unpublished.as_str())
        );
        assert_eq!(
            chat(consumer, &[])["conversationMain"].text("room"),
            "room-fixture"
        );
        fs::remove_file(bad_room).unwrap();
        success(run(
            &repo,
            &[
                "join",
                "--id",
                reserved_room,
                "--participant",
                "other",
                "--role",
                "main",
                "--provider",
                "codex",
                "--native-session",
                synthetic_thread,
            ],
        ));
        assert_eq!(chat(consumer, &[]).text("conversationBinding"), "ambiguous");
        success(run(
            &repo,
            &["close", "--id", reserved_room, "--participant", "other"],
        ));
        assert_eq!(
            chat(consumer, &[])["conversationMain"].text("room"),
            "room-fixture"
        );
        let call = |action: &str, more: &[&str]| {
            let mut values = vec![
                action,
                "--id",
                "room-fixture",
                "--participant",
                "reader",
                "--json",
            ];
            values.extend_from_slice(more);
            run(&repo, &values)
        };
        success(call("show", &[]));
        success(call("scope", &["--owns", "src/**"]));
        assert_eq!(call("scope", &["--owns", "../escape"]).0, 2);
        let first = success(call("updates", &[]));
        let cursor = first["cursor"].as_str().unwrap();
        assert!(
            success(call("updates", &["--after", cursor]))["turns"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let sent = success(run(
            &repo,
            &[
                "send",
                "--id",
                "room-fixture",
                "--participant",
                "owner",
                "--to",
                "reader",
                "--text",
                "Inspect this change",
                "--json",
            ],
        ));
        let log = repo.0.join(".oms/threads/room-fixture.jsonl");
        let before = fs::read(&log).unwrap();
        let updates = success(call("updates", &["--after", cursor]));
        assert_eq!(fs::read(&log).unwrap(), before);
        assert_eq!(success(call("updates", &["--after", cursor])), updates);
        let turns = updates["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 1);
        let mid = turns[0]["room_event"]["id"].as_str().unwrap();
        assert!(!mid.is_empty(), "{sent:?}");
        let consumed = success(call("ack", &["--message", mid]));
        assert_eq!(consumed["approval"].as_bool(), Some(false));
        let acknowledged = fs::read(&log).unwrap();
        success(call("ack", &["--message", mid]));
        assert_eq!(fs::read(&log).unwrap(), acknowledged);
        assert_eq!(call("updates", &["--after", "malformed"]).0, 2);
        assert_eq!(call("ack", &[]).0, 2);
        success(run(
            &repo,
            &[
                "join",
                "--id",
                "room-fixture",
                "--participant",
                "oms-desktop",
                "--role",
                "advisor",
                "--provider",
                "codex",
            ],
        ));
        success(run(
            &repo,
            &[
                "send",
                "--id",
                "room-fixture",
                "--participant",
                "owner",
                "--to",
                "oms-desktop",
                "--kind",
                "question",
                "--message-id",
                "review-one",
                "--text",
                "Approve the next test run?",
            ],
        ));
        let listed = success(run(&repo, &["desktop-requests"]));
        let approval = &listed.list("approvals")[0];
        assert_eq!(approval.text("summary"), "Approve the next test run?");
        let version = approval.number("version").to_string();
        let decide = |ver: &str, digest: &str, choice: &str| {
            run(
                &repo,
                &[
                    "desktop-decide",
                    "--id",
                    "room-fixture",
                    "--message",
                    "review-one",
                    "--version",
                    ver,
                    "--digest",
                    digest,
                    "--decision",
                    choice,
                ],
            )
        };
        let digest = approval.text("digest");
        assert_eq!(decide("1", digest, "approve").0, 2);
        assert_eq!(decide(&version, &"0".repeat(64), "approve").0, 2);
        let (yes, no) = std::thread::scope(|scope| {
            let yes = scope.spawn(|| decide(&version, digest, "approve").0);
            let no = scope.spawn(|| decide(&version, digest, "deny").0);
            (yes.join().unwrap(), no.join().unwrap())
        });
        assert!(matches!((yes, no), (0, 2) | (2, 0)));
        let accepted = if yes == 0 { "approve" } else { "deny" };
        let decided = fs::read(&log).unwrap();
        success(decide(&version, digest, accepted));
        assert_eq!(fs::read(&log).unwrap(), decided);
        assert!(
            success(run(&repo, &["desktop-requests"]))
                .list("approvals")
                .is_empty()
        );
        success(call("scope", &["--clear"]));
        let core = oms_command_room::Room::open(
            &repo.0,
            oms_command_room::Session::default(),
            &EMPTY,
            &STOP,
            false,
            false,
        )
        .unwrap();
        let mut join = oms_command_room::Join::new("checked-reader", "codex");
        join.role = "advisor".into();
        join.parent = Some("owner".into());
        core.join("room-fixture", join).unwrap();
        let state = core.status("room-fixture", false).unwrap();
        let generation = state
            .list("participants")
            .iter()
            .find(|p| p.text("participant") == "checked-reader")
            .unwrap()
            .number("seq");
        let before = fs::read(&log).unwrap();
        assert!(
            core.leave_if_enrolled("room-fixture", "checked-reader", generation + 1, "owner")
                .is_err()
        );
        assert!(
            core.leave_if_enrolled("room-fixture", "checked-reader", generation, "owner")
                .is_err(),
            "missing native parent producer cannot grant checked leave"
        );
        assert_eq!(fs::read(&log).unwrap(), before);
        core.leave("room-fixture", "checked-reader").unwrap();
        assert!(
            !core
                .status("room-fixture", false)
                .unwrap()
                .list("participants")
                .iter()
                .find(|p| p.text("participant") == "checked-reader")
                .unwrap()
                .flag("joined"),
            "legacy leave remains available"
        );
    });
}
#[test]
fn missing_services_fail_closed() {
    isolated("missing_services_fail_closed", || {
        let repo = Repo::new();
        assert!(LocalServices.operations(&repo.0).is_err());
        assert!(LocalServices.current_claude_session().is_err());
        assert!(!LocalServices.delivery_available(oms_command_room::App::Codex));
        assert!(
            LocalServices
                .deliver(oms_command_room::DeliveryRequest {
                    repository: &repo.0,
                    app: oms_command_room::App::Codex,
                    text: "fixture",
                    consumer: None,
                    allow_wakeup: false
                })
                .is_err()
        );
    });
}
#[test]
fn desktop_assignment_rechecks_partial_membership_with_a_bounded_complete_inventory() {
    isolated(
        "desktop_assignment_rechecks_partial_membership_with_a_bounded_complete_inventory",
        || {
            use sha2::{Digest, Sha256};
            let repo = Repo::new();
            let thread = "01900000-0000-7000-8000-000000000022";
            let digest = format!("{:x}", Sha256::digest(thread.as_bytes()));
            let consumer = &digest[..32];
            let request = "aa000000-0000-4000-8000-000000000022";
            let log = |id: &str| repo.0.join(format!(".oms/threads/{id}.jsonl"));
            for id in ["big-history", "second-history", "ask-room"] {
                success(run(&repo, &["new", "--id", id, "--participant", "owner"]));
                success(run(
                    &repo,
                    &[
                        "join",
                        "--id",
                        id,
                        "--participant",
                        "owner",
                        "--provider",
                        "codex",
                        "--role",
                        "main",
                    ],
                ));
            }
            let bind = |id| {
                success(run(
                    &repo,
                    &[
                        "bind",
                        "--id",
                        id,
                        "--participant",
                        "owner",
                        "--native-session",
                        thread,
                    ],
                ))
            };
            bind("ask-room");
            let pad = |id| {
                // Valid whitespace between turns exceeds 8 MiB without synthesizing participants or messages.
                let path = log(id);
                let original = fs::read(&path).unwrap();
                let last = original[..original.len() - 1]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .unwrap()
                    + 1;
                let mut padded = original[..last].to_vec();
                let mut line = vec![b' '; 32 * 1024];
                *line.last_mut().unwrap() = b'\n';
                for _ in 0..288 {
                    padded.extend_from_slice(&line);
                }
                padded.extend_from_slice(&original[last..]);
                fs::write(path, padded).unwrap();
            };
            let snapshot = || {
                let room = oms_command_room::Room::open(
                    &repo.0,
                    Default::default(),
                    &EMPTY,
                    &STOP,
                    false,
                    false,
                )
                .unwrap();
                let seen = room.conversation_main(consumer, []).unwrap();
                assert_eq!(seen.text("conversationBinding"), "matched");
                assert_eq!(seen["conversationMain"].text("room"), "ask-room");
                assert!(seen["conversationMain"].flag("incomplete"));
            };
            let assign = |request| {
                run(
                    &repo,
                    &[
                        "desktop-assign",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        consumer,
                        "--request",
                        request,
                        "--provider",
                        "claude",
                        "--label",
                        "Review",
                        "--text",
                        "Review the change",
                    ],
                )
            };
            let status = || {
                run(
                    &repo,
                    &[
                        "desktop-assign-status",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        consumer,
                        "--request",
                        request,
                    ],
                )
            };
            pad("big-history");
            snapshot();
            assert!(success(assign(request)).flag("fresh"));
            assert_eq!(success(status()).text("status"), "assigned");
            let before = fs::read(log("ask-room")).unwrap();
            let refused = "aa000000-0000-4000-8000-000000000023";
            let assert_refused = |reason| {
                for err in [assign(refused), status()] {
                    assert_eq!(err.0, 2);
                    assert!(String::from_utf8_lossy(&err.2).contains(reason));
                }
                assert_eq!(fs::read(log("ask-room")).unwrap(), before);
            };
            let original = fs::read(log("second-history")).unwrap();
            pad("second-history");
            snapshot();
            assert_refused("room_inventory_incomplete");
            fs::write(log("second-history"), b"damaged\n").unwrap();
            assert_refused("room_inventory_incomplete");
            fs::write(log("second-history"), original).unwrap();
            // This second matching owner is invisible to the smaller display census regardless of enumeration order.
            bind("big-history");
            snapshot();
            assert_refused("conversation is not the owner");
        },
    );
}

#[test]
fn reviewed_issue_receipts_belong_to_one_main_and_never_dispatch() {
    isolated(
        "reviewed_issue_receipts_belong_to_one_main_and_never_dispatch",
        || {
            use sha2::{Digest, Sha256};
            use std::sync::{Mutex, atomic::AtomicUsize};
            type Swap = (u8, Box<dyn FnOnce() + Send>);
            struct Issued<'a> {
                repo: &'a Repo,
                reads: AtomicUsize,
                open: AtomicBool,
                terminal: AtomicBool,
                during: Mutex<Option<Box<dyn FnOnce() + Send + 'a>>>,
                before_receipt: &'a Mutex<Option<Swap>>,
                ended: Vec<oms_command_room::Attempt>,
            }
            struct IssueOperations<'a, 'b> {
                service: &'a Issued<'b>,
                checks: std::cell::Cell<u8>,
            }
            impl oms_command_room::OperationsRead for IssueOperations<'_, '_> {
                fn attempts(&self) -> &[oms_command_room::Attempt] {
                    if self.service.terminal.load(Ordering::SeqCst) {
                        &self.service.ended
                    } else {
                        &[]
                    }
                }
                fn check(&self) -> oms_command_room::Result<()> {
                    let count = self.checks.get() + 1;
                    self.checks.set(count);
                    let mut hook = self.service.before_receipt.lock().unwrap();
                    if hook.as_ref().is_some_and(|(when, _)| *when == count) {
                        hook.take().unwrap().1();
                    }
                    Ok(())
                }
            }
            impl Services for Issued<'_> {
                fn state_repository(&self, p: &Path) -> oms_command_room::Result<PathBuf> {
                    LocalServices.state_repository(p)
                }
                fn canonical_provider(&self, p: &str) -> oms_command_room::Result<String> {
                    LocalServices.canonical_provider(p)
                }
                fn operations<'a>(
                    &'a self,
                    _: &Path,
                ) -> oms_command_room::Result<Box<dyn oms_command_room::OperationsRead + 'a>>
                {
                    Ok(Box::new(IssueOperations {
                        service: self,
                        checks: 0.into(),
                    }))
                }
                fn check_reviewed_issue(
                    &self,
                    checkout: &Path,
                    state: &Path,
                    issue: &str,
                    pin: Option<&str>,
                ) -> oms_command_room::Result<()> {
                    assert_eq!(checkout, self.repo.0);
                    assert_eq!(state, self.repo.0);
                    if pin.is_some() {
                        self.reads.fetch_add(1, Ordering::SeqCst);
                        if let Some(f) = self.during.lock().unwrap().take() {
                            f();
                        }
                        if !self.open.load(Ordering::SeqCst)
                            || !["a".repeat(64), "b".repeat(64), "c".repeat(64)]
                                .iter()
                                .any(|p| Some(p.as_str()) == pin)
                        {
                            return Err(oms_command_room::Error::new("source closed or changed"));
                        }
                    }
                    if issue != "example-org/project#7" {
                        return Err(oms_command_room::Error::new("foreign issue"));
                    }
                    Ok(())
                }
            }
            let repo = Repo::new();
            let thread = "01900000-0000-7000-8000-0000000000aa";
            let digest = format!("{:x}", Sha256::digest(thread.as_bytes()))[..32].to_owned();
            let sha = "a".repeat(64);
            let reference = "example-org/project#7";
            let join = |room| {
                success(run(&repo, &["new", "--id", room, "--participant", "owner"]));
                success(run(
                    &repo,
                    &[
                        "join",
                        "--id",
                        room,
                        "--participant",
                        "owner",
                        "--provider",
                        "codex",
                        "--role",
                        "main",
                        "--native-session",
                        thread,
                    ],
                ));
            };
            join("issue-room");
            let ended = oms_command_room::Attempt {
                attempt_id: "stopped".into(),
                tool: "panel-main".into(),
                provider: "codex".into(),
                state: "failed".into(),
                terminal: true,
                reason_code: None,
                created_at: String::new(),
                refs: oms_command_room::value::obj([
                    ("panel_room_id", "issue-room".into()),
                    ("panel_room_participant", "owner".into()),
                ]),
            };
            let before_receipt = Mutex::new(None);
            let svc = Issued {
                repo: &repo,
                reads: 0.into(),
                open: false.into(),
                terminal: false.into(),
                during: Mutex::new(None),
                before_receipt: &before_receipt,
                ended: vec![ended],
            };
            let call =
                |request: &str, owner: &str, consumer: &str, source: Option<(&str, &str)>| {
                    let mut argv = vec![
                        if source.is_some() {
                            "desktop-issue-record"
                        } else {
                            "desktop-issue-status"
                        },
                        "--id",
                        "issue-room",
                        "--owner",
                        owner,
                        "--conversation-digest",
                        consumer,
                        "--request",
                        request,
                    ];
                    if let Some((reference, pin)) = source {
                        argv.extend(["--issue", reference, "--digest", pin]);
                    }
                    run_with_services(&args(&argv), &repo.0, &STOP, &svc)
                };
            let lookup = |pin: &str| {
                success(run_with_services(
                    &args(&[
                        "desktop-issue-status",
                        "--id",
                        "issue-room",
                        "--owner",
                        "owner",
                        "--conversation-digest",
                        &digest,
                        "--issue",
                        reference,
                        "--digest",
                        pin,
                    ]),
                    &repo.0,
                    &STOP,
                    &svc,
                ))
            };
            let first = lookup(&sha);
            let req = first.text("requestId");
            assert_eq!(first.text("status"), "not_found");
            assert_eq!(lookup(&sha), first);
            assert_ne!(lookup(&"b".repeat(64)).text("requestId"), req);
            assert_ne!(
                call(
                    "4d6acd01-bd18-4567-bf69-ea05fb048b27",
                    "owner",
                    &digest,
                    Some((reference, &sha))
                )
                .0,
                0
            );
            let record = || call(req, "owner", &digest, Some((reference, &sha)));
            let status = || call(req, "owner", &digest, None);
            let room_log = repo.0.join(".oms/threads/issue-room.jsonl");
            let before = fs::read(&room_log).unwrap();
            assert_eq!(success(status()).text("status"), "not_found");
            assert_ne!(call(req, "another", &digest, Some((reference, &sha))).0, 0);
            assert_ne!(
                call(req, "owner", &"f".repeat(32), Some((reference, &sha))).0,
                0
            );
            assert_eq!(svc.reads.load(Ordering::SeqCst), 0);
            assert_ne!(record().0, 0);
            assert_eq!(success(status()).text("status"), "not_found");
            svc.open.store(true, Ordering::SeqCst);
            let delivered = success(record());
            assert_eq!(delivered.text("status"), "recorded");
            assert_eq!(delivered.text("sha256"), sha);
            assert_eq!(delivered.text("issue"), reference);
            assert!(delivered.get("binding").is_none() && delivered.get("body").is_none());
            assert_eq!(lookup(&sha), delivered);
            // A historical receipt can still be addressed and replayed by its original UUID.
            let legacy = "4d6acd01-bd18-4567-bf69-ea05fb048b28";
            let receipts = repo.0.join(".oms/reviewed-issue-receipts");
            // Replace only the named receipt directory while its original fd stays live.
            let detached = receipts.with_file_name("detached-receipts");
            let swap = || {
                let receipts = receipts.clone();
                let detached = detached.clone();
                Box::new(move || {
                    fs::rename(&receipts, detached).unwrap();
                    fs::create_dir(&receipts).unwrap();
                    fs::set_permissions(
                        &receipts,
                        std::os::unix::fs::PermissionsExt::from_mode(0o700),
                    )
                    .unwrap();
                })
            };
            let restore = || {
                fs::remove_dir(&receipts).unwrap();
                fs::rename(&detached, &receipts).unwrap();
            };
            *svc.before_receipt.lock().unwrap() = Some((2, swap()));
            assert_ne!(
                status().0,
                0,
                "returned a receipt from a detached directory"
            );
            assert_eq!(success(status()).text("status"), "not_found");
            restore();
            let next_pin = "c".repeat(64);
            let next = lookup(&next_pin);
            let next_id = next.text("requestId");
            let before_write = swap();
            let hook = &before_receipt;
            *svc.during.lock().unwrap() = Some(Box::new(move || {
                *hook.lock().unwrap() = Some((2, before_write));
            }));
            assert_ne!(
                call(next_id, "owner", &digest, Some((reference, &next_pin))).0,
                0,
                "acknowledged a write to a detached directory"
            );
            assert_eq!(lookup(&next_pin).text("status"), "not_found");
            restore();
            assert_eq!(lookup(&next_pin).text("status"), "not_found");
            assert_eq!(success(status()), delivered);
            // A change after the rename has an uncertain outcome until the exact
            // receipt is read again under its original directory generation.
            let after_write = swap();
            *svc.during.lock().unwrap() = Some(Box::new(move || {
                *hook.lock().unwrap() = Some((3, after_write));
            }));
            assert_ne!(
                call(next_id, "owner", &digest, Some((reference, &next_pin))).0,
                0,
                "confirmed a receipt after its published path was replaced"
            );
            assert_eq!(lookup(&next_pin).text("status"), "not_found");
            restore();
            assert_eq!(lookup(&next_pin).text("status"), "recorded");
            let old = receipts.join(format!("{legacy}.json"));
            let original = receipts.join(format!("{req}.json"));
            fs::copy(&original, &old).unwrap();
            let mut historical =
                oms_command_room::value::parse(&fs::read(&original).unwrap(), 2048).unwrap();
            historical.remove("boundAddress");
            historical.set("requestId", legacy);
            fs::write(
                &old,
                oms_command_room::value::encode(&historical, false, true).unwrap(),
            )
            .unwrap();
            assert_eq!(
                success(call(legacy, "owner", &digest, None)).text("requestId"),
                legacy
            );
            assert_eq!(
                success(call(legacy, "owner", &digest, Some((reference, &sha)))).text("status"),
                "recorded"
            );
            let reads = svc.reads.load(Ordering::SeqCst);
            svc.open.store(false, Ordering::SeqCst);
            assert_eq!(success(status()), delivered);
            assert_eq!(success(record()), delivered);
            assert_ne!(
                call(req, "owner", &digest, Some((reference, &"b".repeat(64)))).0,
                0
            );
            assert_ne!(
                call(req, "owner", &digest, Some(("example-org/project#8", &sha))).0,
                0
            );
            assert_eq!(svc.reads.load(Ordering::SeqCst), reads);
            assert_eq!(fs::read(&room_log).unwrap(), before);
            assert!(!repo.0.join(".oms/plan").exists());
            // The alias, Room log and receipt directory survive replacing the checkout itself.
            let staging = Repo::new();
            let original_root = staging.0.join("original");
            fs::rename(&repo.0, &original_root).unwrap();
            fs::create_dir(&repo.0).unwrap();
            for entry in [".git", ".oms"] {
                fs::rename(original_root.join(entry), repo.0.join(entry)).unwrap();
            }
            let replaced = lookup(&sha);
            assert_eq!(replaced.text("status"), "not_found");
            assert_ne!(replaced.text("requestId"), req);
            assert_ne!(status().0, 0);
            assert_ne!(record().0, 0);
            for entry in [".git", ".oms"] {
                fs::rename(repo.0.join(entry), original_root.join(entry)).unwrap();
            }
            fs::remove_dir_all(&repo.0).unwrap();
            fs::rename(original_root, &repo.0).unwrap();
            assert_eq!(lookup(&sha), delivered);
            svc.terminal.store(true, Ordering::SeqCst);
            assert_ne!(status().0, 0);
            svc.terminal.store(false, Ordering::SeqCst);
            join("duplicate-owner");
            assert_ne!(status().0, 0);
            success(run(
                &repo,
                &["close", "--id", "duplicate-owner", "--participant", "owner"],
            ));
            assert_eq!(success(status()), delivered);
            svc.open.store(true, Ordering::SeqCst);
            let next = lookup(&"b".repeat(64));
            let later = next.text("requestId");
            *svc.during.lock().unwrap() = Some(Box::new(|| {
                success(run(
                    &repo,
                    &["close", "--id", "issue-room", "--participant", "owner"],
                ));
            }));
            assert_ne!(
                call(later, "owner", &digest, Some((reference, &"b".repeat(64)))).0,
                0
            );
            assert!(
                !repo
                    .0
                    .join(format!(".oms/reviewed-issue-receipts/{later}.json"))
                    .exists()
            );
            assert_ne!(status().0, 0);
        },
    );
}

#[test]
fn desktop_assignment_is_owner_proofed_idempotent_and_unforgeable() {
    isolated(
        "desktop_assignment_is_owner_proofed_idempotent_and_unforgeable",
        || {
            use sha2::{Digest, Sha256};
            let repo = Repo::new();
            let thread = "01900000-0000-7000-8000-0000000000aa";
            let digest = format!("{:x}", Sha256::digest(thread.as_bytes()))[..32].to_owned();
            let other = "f".repeat(32);
            let req = "4d6acd01-bd18-4567-bf69-ea05fb048b27";
            success(run(
                &repo,
                &["new", "--id", "ask-room", "--participant", "owner"],
            ));
            success(run(
                &repo,
                &[
                    "join",
                    "--id",
                    "ask-room",
                    "--participant",
                    "owner",
                    "--provider",
                    "codex",
                    "--role",
                    "main",
                    "--native-session",
                    thread,
                ],
            ));
            let assign = |d: &str, provider: &str, text: &str| {
                run(
                    &repo,
                    &[
                        "desktop-assign",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        d,
                        "--request",
                        req,
                        "--provider",
                        provider,
                        "--label",
                        "Find callers",
                        "--text",
                        text,
                    ],
                )
            };
            // Foreign or unbound conversations, unsupported providers: nothing is written.
            let log = repo.0.join(".oms/threads/ask-room.jsonl");
            let before = fs::read(&log).unwrap();
            assert_eq!(assign(&other, "codex", "task").0, 2);
            assert_eq!(assign(&digest, "pi", "task").0, 2);
            assert_eq!(assign(&digest, "codex", "task").0, 2);
            let (intent, simultaneous) = std::thread::scope(|scope| {
                let other = scope.spawn(|| success(assign(&digest, "claude", "task")));
                (
                    success(assign(&digest, "claude", "task")),
                    other.join().unwrap(),
                )
            });
            assert_ne!(
                intent.flag("fresh"),
                simultaneous.flag("fresh"),
                "exactly one dispatch permission"
            );
            assert_eq!(intent.text("assignedAt"), simultaneous.text("assignedAt"));
            let assigned_at = intent.text("assignedAt");
            assert!(!assigned_at.is_empty());
            let first = std::fs::read(&log).unwrap();
            assert_ne!(first, before);
            let again = success(assign(&digest, "claude", "task"));
            assert_eq!(
                fs::read(&log).unwrap(),
                first,
                "duplicate intent appends nothing"
            );
            assert_eq!(again.text("status"), "assigned");
            assert_eq!(again.text("assignedAt"), assigned_at);
            assert_eq!(again.text("role"), "researcher");
            assert!(again.flag("readOnly"));
            assert!(!again.flag("fresh"));
            assert_eq!(
                assign(&digest, "claude", "different task").0,
                2,
                "same request, other content"
            );
            let status = |d: &str| {
                run(
                    &repo,
                    &[
                        "desktop-assign-status",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        d,
                        "--request",
                        req,
                    ],
                )
            };
            assert_eq!(status(&other).0, 2);
            let read = success(status(&digest));
            assert_eq!(read.text("assignedAt"), assigned_at);
            assert_eq!(read.text("operation"), "");
            assert_eq!(fs::read(&log).unwrap(), first);
            let terminal = oms_command_room::Attempt {
                attempt_id: "owner".into(),
                tool: "panel-main".into(),
                provider: "codex".into(),
                state: "done".into(),
                terminal: true,
                reason_code: None,
                created_at: String::new(),
                refs: oms_command_room::value::obj([("panel_room_id", "ask-room".into())]),
            };
            assert_eq!(
                run_with_services(
                    &args(&[
                        "desktop-assign-status",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        &digest,
                        "--request",
                        req
                    ]),
                    &repo.0,
                    &STOP,
                    &FixtureServices(std::slice::from_ref(&terminal))
                )
                .0,
                2
            );
            let receipt = |d: &str, op: &str| {
                run(
                    &repo,
                    &[
                        "desktop-assign-receipt",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        d,
                        "--request",
                        req,
                        "--operation",
                        op,
                    ],
                )
            };
            assert_eq!(receipt(&other, "ask-1").0, 2);
            let started = success(receipt(&digest, "ask-20261124T000000Z-abcd"));
            assert_eq!(started.text("status"), "started");
            let linked = fs::read(&log).unwrap();
            assert_eq!(
                success(receipt(&digest, started.text("operation"))).text("assignedAt"),
                assigned_at
            );
            assert_eq!(fs::read(&log).unwrap(), linked);
            assert_eq!(started.text("operation"), "ask-20261124T000000Z-abcd");
            assert_eq!(started.text("thread"), "ask-20261124T000000Z-abcd");
            assert_eq!(receipt(&digest, "ask-other").0, 2, "receipt is write-once");
            // A participant or worker session cannot write the trusted association,
            // and a second bound conversation cannot claim the Room.
            let forged = |participant: Option<&str>, child: bool, id: &str, text: &str| {
                let session = oms_command_room::Session {
                    participant: participant.map(str::to_owned),
                    child,
                    ..Default::default()
                };
                let core =
                    oms_command_room::Room::open(&repo.0, session, &EMPTY, &STOP, false, false)
                        .unwrap();
                core.send(
                    "ask-room",
                    oms_command_room::Send {
                        sender: "owner",
                        recipient: &format!("ask-{req}"),
                        text,
                        kind: "status",
                        reply_to: None,
                        message_id: Some(id),
                        linked: true,
                        quiet: false,
                    },
                )
            };
            let foreign = "5d6acd01-bd18-4567-bf69-ea05fb048b27";
            let text = format!("operation foreign-op thread oms-assign-{foreign}");
            assert!(forged(Some("owner"), false, &format!("dispatch-{foreign}"), &text).is_err());
            assert!(forged(None, true, &format!("dispatch-{foreign}"), &text).is_err());
            assert!(
                forged(None, false, &format!("dispatch-{foreign}"), &text).is_err(),
                "no matching assignment"
            );
            assert!(forged(None, false, &format!("assign-{foreign}"), "task").is_err());
            let read = oms_command_room::Room::open(
                &repo.0,
                Default::default(),
                &EMPTY,
                &STOP,
                false,
                false,
            )
            .unwrap();
            let chat = read
                .desktop_chat("ask-room", &format!("ask-{req}"), &digest, None)
                .unwrap();
            let texts = chat
                .list("messages")
                .iter()
                .map(|m| m.text("kind").to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                texts,
                ["handoff", "status"],
                "assignment is readable through the Room reader"
            );
            assert_eq!(chat.text("provider"), "claude");
            assert!(again["purpose"].is_null());

            let lead = |action, digest: &str, provider: &str, extra: &[&str]| {
                let mut v = vec![
                    action,
                    "--id",
                    "ask-room",
                    "--conversation-digest",
                    digest,
                    "--provider",
                    provider,
                ];
                v.extend_from_slice(extra);
                run(&repo, &v)
            };
            let before = fs::read(&log).unwrap();
            for (d, p, extra) in [
                (other.as_str(), "claude", vec![]),
                (digest.as_str(), "pi", vec![]),
                (digest.as_str(), "codex", vec!["--label", "claim"]),
                (digest.as_str(), "codex", vec!["--text", "inspect"]),
                (digest.as_str(), "codex", vec!["--request", req]),
            ] {
                assert_ne!(lead("desktop-lead-open", d, p, &extra).0, 0);
            }
            assert_eq!(fs::read(&log).unwrap(), before);
            let closed_main = run_with_services(
                &args(&[
                    "desktop-lead-open",
                    "--id",
                    "ask-room",
                    "--conversation-digest",
                    &digest,
                    "--provider",
                    "codex",
                ]),
                &repo.0,
                &STOP,
                &FixtureServices(std::slice::from_ref(&terminal)),
            );
            assert_ne!(closed_main.0, 0);
            assert_eq!(fs::read(&log).unwrap(), before);
            let mut requests = std::collections::BTreeSet::new();
            for (provider, canonical, title) in [
                ("codex", "codex", "OMS Lead · Codex"),
                ("claude", "claude", "OMS Lead · Claude"),
                ("agy", "antigravity", "OMS Lead · AGY"),
            ] {
                let before = fs::read(&log).unwrap();
                let empty = success(lead("desktop-lead-status", &digest, provider, &[]));
                let request = empty.text("request");
                assert_eq!(empty.text("status"), "not_found");
                assert_eq!(fs::read(&log).unwrap(), before);
                assert!(requests.insert(request.to_owned()));
                let open = || {
                    success(lead(
                        "desktop-lead-open",
                        &digest,
                        provider,
                        &["--request", request],
                    ))
                };
                let (first, second) = std::thread::scope(|scope| {
                    let other = scope.spawn(open);
                    (open(), other.join().unwrap())
                });
                assert_ne!(first.flag("fresh"), second.flag("fresh"));
                assert_eq!(first.text("role"), "worker");
                assert_eq!(first.text("purpose"), "provider-lead");
                assert_eq!(first.text("title"), title);
                assert_eq!(first.text("provider"), canonical);
                assert_eq!(first.text("participant"), format!("ask-{request}"));
                assert_eq!(first.text("status"), "assigned");
                assert!(first["thread"].is_null() && first.flag("readOnly"));
                if provider == "claude" {
                    // Pre-upgrade joins remain bound to the same provider and replay byte-for-byte.
                    let current = fs::read_to_string(&log).unwrap();
                    let legacy = current.replace(title, "Claude Lead");
                    assert_ne!(current, legacy);
                    fs::write(&log, legacy.replace("Claude Lead", "Codex Lead")).unwrap();
                    assert_ne!(lead("desktop-lead-status", &digest, provider, &[]).0, 0);
                    fs::write(&log, &legacy).unwrap();
                    assert_eq!(open().text("title"), "Claude Lead");
                    assert_eq!(fs::read_to_string(&log).unwrap(), legacy);
                }
                let op = format!("fixture-{canonical}");
                let linked = success(run(
                    &repo,
                    &[
                        "desktop-assign-receipt",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        &digest,
                        "--request",
                        request,
                        "--operation",
                        &op,
                    ],
                ));
                assert_eq!(linked.text("thread"), op);
                let bytes = fs::read(&log).unwrap();
                let recovered = success(lead("desktop-lead-status", &digest, canonical, &[]));
                assert_eq!(recovered, linked);
                assert!(!open().flag("fresh"));
                let prior = success(run(
                    &repo,
                    &[
                        "desktop-assign-status",
                        "--id",
                        "ask-room",
                        "--conversation-digest",
                        &digest,
                        "--request",
                        request,
                    ],
                ));
                assert_eq!(prior, recovered);
                assert_eq!(fs::read(&log).unwrap(), bytes);
            }
        },
    );
}
