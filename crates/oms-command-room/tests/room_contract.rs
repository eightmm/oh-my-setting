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
