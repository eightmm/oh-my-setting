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
            assert!(success(assign(&digest, "claude", "task")).flag("fresh"));
            let first = std::fs::read(&log).unwrap();
            assert_ne!(first, before);
            let again = success(assign(&digest, "claude", "task"));
            assert_eq!(
                fs::read(&log).unwrap(),
                first,
                "duplicate intent appends nothing"
            );
            assert_eq!(again.text("status"), "assigned");
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
            assert_eq!(success(status(&digest)).text("operation"), "");
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
                    &FixtureServices(&[terminal])
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
        },
    );
}
