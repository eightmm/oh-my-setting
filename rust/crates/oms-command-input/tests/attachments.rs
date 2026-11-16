#![forbid(unsafe_code)]
use oms_context::Json as J;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "oms-native-input-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let path = path.canonicalize().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&path)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_INDEX_FILE")
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            path.join("source.txt"),
            b"first line\r\nselected line\r\nlast line\n",
        )
        .unwrap();
        Self(path)
    }
    fn call(
        &self,
        args: &[&OsStr],
        extra: &[(&str, &OsStr)],
        interrupted: bool,
    ) -> (i32, Vec<u8>, Vec<u8>) {
        let call = self
            .0
            .join(format!("call-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&call).unwrap();
        fs::write(
            call.join("args"),
            args.iter()
                .flat_map(|s| s.as_bytes().iter().copied().chain([0]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "native_call", "--ignored", "--nocapture"])
            .env("OMS_INPUT_FIXTURE", &call)
            .env("OMS_INPUT_REPO", &self.0)
            .env("OMS_INPUT_INTERRUPTED", if interrupted { "1" } else { "0" })
            .env("OMS_HARNESS_CHILD", "0")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "0");
        for key in [
            "OMS_ROOM_REPO",
            "OMS_ROOM_ID",
            "OMS_ROOM_PARTICIPANT",
            "OMS_INPUT_SERVICES",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            command.env_remove(key);
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let code = fs::read_to_string(call.join("code"))
            .unwrap()
            .parse()
            .unwrap();
        let out = fs::read(call.join("out")).unwrap();
        let err = fs::read(call.join("err")).unwrap();
        fs::remove_dir_all(call).unwrap();
        (code, out, err)
    }
    fn raw(&self, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        self.call(&args.iter().map(OsStr::new).collect::<Vec<_>>(), &[], false)
    }
    fn ok(&self, args: &[&str]) -> J {
        let (code, out, err) = self.raw(args);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        oms_context::parse_json(&out).unwrap()
    }
    fn bad(&self, args: &[&str]) {
        let (code, out, _) = self.raw(args);
        assert_eq!(code, 2);
        assert!(out.is_empty());
    }
    fn capture(&self) -> String {
        value(&self.ok(&["attach", "file", "source.txt"]), "id").to_owned()
    }
    fn folder(&self, id: &str) -> PathBuf {
        self.0.join(".oms/inputs/attachments").join(id)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(value: &'a J, key: &str) -> &'a J {
    let J::Object(fields) = value else {
        panic!("object");
    };
    &fields.iter().find(|(k, _)| k == key.as_bytes()).unwrap().1
}
fn value<'a>(v: &'a J, key: &str) -> &'a str {
    let J::String(bytes) = field(v, key) else {
        panic!("string");
    };
    std::str::from_utf8(bytes).unwrap()
}
fn array(v: &J) -> &[J] {
    let J::Array(a) = v else {
        panic!("array");
    };
    a
}

// The subprocess owns its environment. Tests never mutate process-global env or cwd.
#[test]
#[ignore = "subprocess transport fixture"]
fn native_call() {
    let Some(directory) = std::env::var_os("OMS_INPUT_FIXTURE") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let raw = fs::read(directory.join("args")).unwrap();
    let args = raw[..raw.len().saturating_sub(1)]
        .split(|b| *b == 0)
        .map(|b| OsString::from_vec(b.to_vec()))
        .collect::<Vec<_>>();
    let repo = PathBuf::from(std::env::var_os("OMS_INPUT_REPO").unwrap());
    let interrupted =
        AtomicBool::new(std::env::var_os("OMS_INPUT_INTERRUPTED").is_some_and(|v| v == "1"));
    let done = AtomicBool::new(false);
    let (code, out, err) = std::thread::scope(|scope| {
        if let Some(marker) = std::env::var_os("OMS_INPUT_INTERRUPT_WHEN_READY") {
            let interrupted = &interrupted;
            let done = &done;
            scope.spawn(move || {
                let deadline = std::time::Instant::now() + test_bound();
                while !done.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    if fs::read_to_string(&marker).is_ok_and(|s| s.split_whitespace().count() == 3)
                    {
                        interrupted.store(true, Ordering::Relaxed);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            });
        }
        let result = match std::env::var("OMS_INPUT_SERVICES").as_deref() {
            Ok(mode) => oms_command_input::run_with_services(
                &args,
                &repo,
                &interrupted,
                &RoomFixture(mode.to_owned()),
            ),
            Err(_) => oms_command_input::run(&args, &repo, &interrupted),
        };
        done.store(true, Ordering::Relaxed);
        result
    });
    fs::write(directory.join("code"), code.to_string()).unwrap();
    fs::write(directory.join("out"), out).unwrap();
    fs::write(directory.join("err"), err).unwrap();
}

#[test]
fn file_ranges_retrieval_listing_modes_and_drift() {
    let f = Fixture::new();
    assert!(array(field(&f.ok(&["attachments"]), "attachments")).is_empty());
    let record = f.ok(&[
        "--json",
        "attach",
        "--repo",
        f.0.to_str().unwrap(),
        "file",
        "./source.txt",
        "--start-line",
        "2",
        "--end-line",
        "2",
    ]);
    let id = value(&record, "id");
    let retrieved = f.ok(&["attachment", id]);
    assert_eq!(value(&retrieved, "content"), "selected line\r\n");
    assert_eq!(value(&retrieved, "source_status"), "current");
    assert_eq!(
        field(field(&record, "source"), "start_line"),
        &J::Number("2".into())
    );
    for name in ["meta.json", "content.txt"] {
        assert_eq!(
            fs::metadata(f.folder(id).join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(f.folder(id)).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        1
    );
    fs::write(f.0.join("source.txt"), b"different\n").unwrap();
    assert_eq!(
        value(&f.ok(&["attachment", id]), "source_status"),
        "drifted-or-unavailable"
    );
}

#[test]
fn selection_sources_text_file_unicode_and_full_excerpt() {
    let f = Fixture::new();
    let content = "a".repeat(8191) + "\u{1f600}".repeat(3).as_str();
    fs::write(f.0.join("text"), &content).unwrap();
    for kind in ["browser", "chat", "text"] {
        let record = f.ok(&[
            "attach",
            "selection",
            "--source",
            kind,
            "--origin",
            "https://example.org/paper",
            "--text-file",
            "text",
        ]);
        let id = value(&record, "id");
        let opened = f.ok(&["attachment", id]);
        assert_eq!(value(&opened, "content").len(), 8191);
        assert_eq!(field(&opened, "truncated"), &J::Bool(true));
        assert_eq!(value(&opened, "source_status"), "declared-unverified");
        assert_eq!(
            value(&f.ok(&["attachment", id, "--full"]), "content"),
            content
        );
    }
    let record = f.ok(&[
        "attach",
        "selection",
        "--source=text",
        "--origin=selection",
        "--text",
        "\u{1f600}\tparagraph",
    ]);
    assert_eq!(
        value(&f.ok(&["attachment", value(&record, "id")]), "content"),
        "\u{1f600}\tparagraph"
    );
}

#[test]
fn ranges_source_and_snapshot_byte_limits() {
    let f = Fixture::new();
    for (first, last) in [("-1", "1"), ("2", "1"), ("1", "4"), ("no", "1")] {
        f.bad(&[
            "attach",
            "file",
            "source.txt",
            "--start-line",
            first,
            "--end-line",
            last,
        ]);
    }
    let record = f.ok(&[
        "attach",
        "file",
        "source.txt",
        "--start-line",
        "0",
        "--end-line",
        "0",
    ]);
    assert_eq!(
        field(field(&record, "source"), "end_line"),
        &J::Number("3".into())
    );
    fs::write(f.0.join("empty"), b"").unwrap();
    f.bad(&["attach", "file", "empty"]);
    f.bad(&["attach", "file", "empty", "--start-line", "1"]);
    for length in [65536, 65537] {
        fs::write(f.0.join("bounded"), vec![b'x'; length]).unwrap();
        let result = f.raw(&["attach", "file", "bounded"]);
        assert_eq!(result.0, if length == 65536 { 0 } else { 2 });
    }
    for length in [4 * 1024 * 1024, 4 * 1024 * 1024 + 1] {
        let mut body = vec![b'x'; length];
        body[..3].copy_from_slice(b"ok\n");
        fs::write(f.0.join("huge"), body).unwrap();
        assert_eq!(
            f.raw(&["attach", "file", "huge", "--end-line", "1"]).0,
            if length == 4 * 1024 * 1024 { 0 } else { 2 }
        );
    }
    fs::write(f.0.join("boundaries"), "a\r\nb\rc\u{85}d\u{2028}e\u{2029}f").unwrap();
    let record = f.ok(&[
        "attach",
        "file",
        "boundaries",
        "--start-line",
        "3",
        "--end-line",
        "3",
    ]);
    assert_eq!(
        value(&f.ok(&["attachment", value(&record, "id")]), "content"),
        "c\u{85}"
    );
}

#[test]
fn refusal_before_private_state_and_source_reads() {
    let f = Fixture::new();
    for path in [
        "../outside",
        ".git/config",
        ".oms/inputs",
        "/absolute",
        "missing",
    ] {
        f.bad(&["attach", "file", path]);
    }
    symlink("source.txt", f.0.join("link")).unwrap();
    f.bad(&["attach", "file", "link"]);
    fs::hard_link(f.0.join("source.txt"), f.0.join("hard")).unwrap();
    f.bad(&["attach", "file", "hard"]);
    fs::write(f.0.join("invalid"), [0xff]).unwrap();
    f.bad(&["attach", "file", "invalid"]);
    let sensitive = ["api_", "key", ": private"].concat();
    fs::write(f.0.join("sensitive"), &sensitive).unwrap();
    let refused = f.raw(&["attach", "file", "sensitive"]);
    assert_eq!(refused.0, 2);
    assert!(refused.1.is_empty());
    assert!(!String::from_utf8_lossy(&refused.2).contains(&sensitive));
    let userinfo_origin = ["https://name", ":pass", "@example.org"].concat();
    for origin in [
        userinfo_origin.as_str(),
        "https://example.org/?%74o%6ben=private",
        "line\norigin",
        "https://[bad",
        "https://[example.org]",
    ] {
        f.bad(&[
            "attach",
            "selection",
            "--source",
            "browser",
            "--origin",
            origin,
            "--text",
            "paragraph",
        ]);
    }
    for content in ["", " ", "x\x01"] {
        f.bad(&[
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "note",
            "--text",
            content,
        ]);
    }
    assert!(!f.0.join(".oms").exists());
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        let result = f.call(
            &[
                OsStr::new("attach"),
                OsStr::new("file"),
                OsStr::new("source.txt"),
            ],
            &[(key, f.0.as_os_str())],
            false,
        );
        assert_eq!(result.0, 2);
        assert!(String::from_utf8_lossy(&result.2).contains("redirect"));
    }
    assert!(!f.0.join(".oms").exists());
}

#[test]
fn immutable_tamper_missing_malformed_and_unsafe_state() {
    let f = Fixture::new();
    let id = f.capture();
    let folder = f.folder(&id);
    let original = fs::read(folder.join("content.txt")).unwrap();
    fs::write(folder.join("content.txt"), b"tampered").unwrap();
    f.bad(&["attachment", &id]);
    fs::write(folder.join("content.txt"), original).unwrap();
    let meta = fs::read(folder.join("meta.json")).unwrap();
    for bad in [b"{}".as_slice(), b"null", b"[]", b"{", b""] {
        fs::write(folder.join("meta.json"), bad).unwrap();
        f.bad(&["attachment", &id]);
    }
    fs::remove_file(folder.join("meta.json")).unwrap();
    f.bad(&["attachment", &id]);
    assert!(array(field(&f.ok(&["attachments"]), "attachments")).is_empty());
    fs::write(folder.join("meta.json"), &meta).unwrap();
    fs::remove_file(folder.join("content.txt")).unwrap();
    symlink(f.0.join("source.txt"), folder.join("content.txt")).unwrap();
    f.bad(&["attachment", &id]);
    for id in ["../outside", "", ".hidden", "a/b"] {
        f.bad(&["attachment", id]);
    }
    fs::remove_file(folder.join("content.txt")).unwrap();
    fs::hard_link(f.0.join("source.txt"), folder.join("content.txt")).unwrap();
    f.bad(&["attachment", &id]);
    let f = Fixture::new();
    symlink("source.txt", f.0.join(".oms")).unwrap();
    f.bad(&["attach", "file", "source.txt"]);
}

#[test]
fn raw_os_source_paths_and_physical_repository_aliases() {
    let f = Fixture::new();
    #[cfg(not(target_os = "macos"))]
    {
        let path = OsString::from_vec(b"raw-\xff-\xf0\x9f\x98\x80".to_vec());
        fs::write(f.0.join(&path), "paragraph\n").unwrap();
        let result = f.call(
            &[OsStr::new("attach"), OsStr::new("file"), &path],
            &[],
            false,
        );
        assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
        assert!(String::from_utf8_lossy(&result.1).contains("\\udcff"));
        let record = oms_context::parse_json(&result.1).unwrap();
        assert_eq!(
            value(
                &f.ok(&["attachment", value(&record, "id")]),
                "source_status"
            ),
            "current"
        );
    }
    let alias = f.0.join("alias");
    symlink(&f.0, &alias).unwrap();
    f.ok(&["--repo", alias.to_str().unwrap(), "attachments"]);
    #[cfg(not(target_os = "macos"))]
    {
        let raw_repo = f.0.join(OsString::from_vec(b"repo-\xfe".to_vec()));
        fs::create_dir(&raw_repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&raw_repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(raw_repo.join("source"), b"native\n").unwrap();
        let result = f.call(
            &[
                OsStr::new("--repo"),
                raw_repo.as_os_str(),
                OsStr::new("attach"),
                OsStr::new("file"),
                OsStr::new("source"),
            ],
            &[],
            false,
        );
        assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
        let record = oms_context::parse_json(&result.1).unwrap();
        assert!(String::from_utf8_lossy(&result.1).contains("\\udcfe"));
        let result = f.call(
            &[
                OsStr::new("attachment"),
                OsStr::new(value(&record, "id")),
                OsStr::new("--repo"),
                raw_repo.as_os_str(),
            ],
            &[],
            false,
        );
        assert_eq!(result.0, 0);
        let mut option = b"--repo=".to_vec();
        option.extend_from_slice(raw_repo.as_os_str().as_bytes());
        let option = OsString::from_vec(option);
        let result = f.call(&[&option, OsStr::new("attachments")], &[], false);
        assert_eq!(result.0, 0);
    }
}

#[test]
fn concurrent_capture_interruption_and_incomplete_commit() {
    let f = Fixture::new();
    let codes = std::thread::scope(|scope| {
        (0..6)
            .map(|_| scope.spawn(|| f.raw(&["attach", "file", "source.txt"]).0))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(codes.iter().all(|c| *c == 0), "{codes:?}");
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        6
    );
    let incomplete = f.folder("attachment-incomplete");
    fs::create_dir(&incomplete).unwrap();
    fs::write(incomplete.join("content.txt"), b"partial").unwrap();
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        6
    );
    let f = Fixture::new();
    let result = f.call(
        &[
            OsStr::new("attach"),
            OsStr::new("file"),
            OsStr::new("source.txt"),
        ],
        &[],
        true,
    );
    assert_eq!(result.0, 2);
    assert!(!f.0.join(".oms").exists());
}

#[test]
fn incomplete_actions_fail_before_reads_and_children_fail_closed() {
    let f = Fixture::new();
    for args in [
        vec!["attach", "terminal"],
        vec!["add"],
        vec!["edit"],
        vec!["move"],
        vec!["show"],
        vec!["preview"],
        vec!["cancel"],
        vec!["deliver"],
        vec!["reconcile"],
        vec!["feedback"],
    ] {
        let owned = args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(oms_command_input::owns(&owned));
        let borrowed = owned.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        let (code, out, err) = f.call(&borrowed, &[], true);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(
            String::from_utf8_lossy(&err).starts_with("usage: oms input"),
            "{args:?}: {}",
            String::from_utf8_lossy(&err)
        );
    }
    assert!(!f.0.join(".oms").exists());
    let id = f.capture();
    for args in [
        vec!["attachments"],
        vec!["attach", "file", "source.txt"],
        vec!["attachment", &id],
    ] {
        let result = f.call(
            &args.iter().map(OsStr::new).collect::<Vec<_>>(),
            &[
                ("OMS_HARNESS_CHILD", OsStr::new("1")),
                ("OMS_ROOM_REPO", f.0.as_os_str()),
                ("OMS_ROOM_ID", OsStr::new("input-room")),
                ("OMS_ROOM_PARTICIPANT", OsStr::new("worker")),
            ],
            false,
        );
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
    }
    assert!(!f.0.join(".oms/threads").exists());
}

#[test]
fn argument_errors_help_tracked_state_and_count_bound() {
    let f = Fixture::new();
    let help = f.raw(&["--help"]);
    assert_eq!(help.0, 0);
    assert!(String::from_utf8_lossy(&help.1).starts_with("usage: oms input [-h]"));
    for args in [
        vec!["attachment"],
        vec!["attachment", "a", "extra"],
        vec!["attachments", "--full"],
        vec!["attach", "file", "source.txt", "--source", "text"],
        vec!["attach", "selection", "--source", "other"],
        vec![
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "note",
            "--text",
            "one",
            "--text-file",
            "source.txt",
        ],
    ] {
        f.bad(&args);
    }
    assert!(!f.0.join(".oms").exists());
    let id = f.capture();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(["add", "-f", ".oms/inputs/.gitignore"])
            .status()
            .unwrap()
            .success()
    );
    f.bad(&["attach", "file", "source.txt"]);
    let attachments = f.folder(&id).parent().unwrap().to_owned();
    for i in 1..1000 {
        fs::create_dir(attachments.join(format!("empty-{i}"))).unwrap();
    }
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        1
    );
    fs::create_dir(attachments.join("excess")).unwrap();
    f.bad(&["attachments"]);
}

#[test]
fn registered_scratch_uses_shared_state_and_inherited_conflicts_refuse() {
    use std::io::Write;
    use std::process::Stdio;
    let f = Fixture::new();
    // Seed a private repository object graph without a commit command or config.
    let mut tree = Command::new("git")
        .arg("-C")
        .arg(&f.0)
        .args(["hash-object", "-t", "tree", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(tree.stdin.take());
    let tree = String::from_utf8(tree.wait_with_output().unwrap().stdout).unwrap();
    let body = format!(
        "tree {}\nauthor Fixture <fixture@example.invalid> 1 +0000\ncommitter Fixture <fixture@example.invalid> 1 +0000\n\nfixture\n",
        tree.trim()
    );
    let mut commit = Command::new("git")
        .arg("-C")
        .arg(&f.0)
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    commit
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let commit = String::from_utf8(commit.wait_with_output().unwrap().stdout).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(["update-ref", "HEAD", commit.trim()])
            .status()
            .unwrap()
            .success()
    );
    let managed = f.0.join("managed");
    let residue = managed.join("oh-my-setting-scratch.fixture");
    fs::create_dir_all(&residue).unwrap();
    let work = residue.join("wt-fixture");
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(["worktree", "add", "-q", "--detach"])
            .arg(&work)
            .status()
            .unwrap()
            .success()
    );
    fs::write(work.join("selected.txt"), b"scratch paragraph\n").unwrap();
    let marker = format!(
        "kind=oh-my-setting-temp\ntemporary=1\nrepo={}\nworktree={}\n",
        f.0.display(),
        work.display()
    );
    fs::write(residue.join(".oh-my-setting-tmp"), marker).unwrap();
    let args = [
        OsStr::new("attach"),
        OsStr::new("file"),
        OsStr::new("selected.txt"),
        OsStr::new("--repo"),
        work.as_os_str(),
    ];
    let result = f.call(
        &args,
        &[("OMS_DELEGATE_WORKTREE_ROOT", managed.as_os_str())],
        false,
    );
    assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
    let record = oms_context::parse_json(&result.1).unwrap();
    let id = value(&record, "id");
    assert_eq!(value(&record, "repo"), f.0.to_str().unwrap());
    assert_eq!(
        value(field(&record, "source"), "worktree"),
        work.to_str().unwrap()
    );
    assert!(f.folder(id).join("meta.json").is_file());
    assert!(!work.join(".oms").exists());
    assert_eq!(
        value(&f.ok(&["attachment", id]), "source_status"),
        "current"
    );
    let other = Fixture::new();
    let result = f.call(
        &[OsStr::new("attachments")],
        &[
            ("OMS_ROOM_ID", OsStr::new("other-room")),
            ("OMS_ROOM_REPO", other.0.as_os_str()),
        ],
        false,
    );
    assert_eq!(result.0, 2);
    fs::write(residue.join(".oh-my-setting-tmp"), b"malformed\n").unwrap();
    let result = f.call(
        &args,
        &[("OMS_DELEGATE_WORKTREE_ROOT", managed.as_os_str())],
        false,
    );
    assert_eq!(result.0, 0);
    let record = oms_context::parse_json(&result.1).unwrap();
    assert_eq!(value(&record, "repo"), work.to_str().unwrap());
}

#[test]
fn selection_bounds_missing_text_and_source_files() {
    let f = Fixture::new();
    for length in [65536, 65537] {
        let body = "x".repeat(length);
        let result = f.raw(&[
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "note",
            "--text",
            &body,
        ]);
        assert_eq!(result.0, if length == 65536 { 0 } else { 2 });
        fs::write(f.0.join("body"), &body).unwrap();
        let result = f.raw(&[
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "note",
            "--text-file",
            "body",
        ]);
        assert_eq!(result.0, if length == 65536 { 0 } else { 2 });
    }
    for length in [0, 1000, 1001] {
        let origin = "x".repeat(length);
        let result = f.raw(&[
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            &origin,
            "--text",
            "paragraph",
        ]);
        assert_eq!(result.0, if length == 1000 { 0 } else { 2 });
    }
    for file in ["missing", "body-link", "body-hard"] {
        if file == "body-link" {
            symlink("body", f.0.join(file)).unwrap();
        }
        if file == "body-hard" {
            fs::hard_link(f.0.join("body"), f.0.join(file)).unwrap();
        }
        f.bad(&[
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "note",
            "--text-file",
            file,
        ]);
    }
    f.bad(&[
        "attach",
        "selection",
        "--source",
        "text",
        "--origin",
        "note",
    ]);
}

#[test]
fn existing_feedback_snapshots_preserve_full_bound_and_registry_status() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let id = f.capture();
    let registry = b"{\"data_sha256\":\"data-digest\",\"id\":\"view-fixture\"}";
    fs::create_dir(f.0.join(".oms/artifact-views")).unwrap();
    fs::write(f.0.join(".oms/artifact-views/view-fixture.json"), registry).unwrap();
    let body = "x".repeat(1024 * 1024);
    fs::write(f.folder(&id).join("content.txt"), &body).unwrap();
    let metadata = format!(
        "{{\"schema\":1,\"id\":{id:?},\"repo\":{repo:?},\"kind\":\"artifact-feedback\",\"content_bytes\":{count},\"content_sha256\":\"{digest:x}\",\"created_at\":\"2026-01-01T00:00:00Z\",\"source\":{{\"view_id\":\"view-fixture\",\"data_sha256\":\"data-digest\",\"registry_sha256\":\"{registry_digest:x}\"}}}}",
        repo = f.0.to_str().unwrap(),
        count = body.len(),
        digest = Sha256::digest(body.as_bytes()),
        registry_digest = Sha256::digest(registry)
    );
    fs::write(f.folder(&id).join("meta.json"), &metadata).unwrap();
    assert_eq!(
        value(&f.ok(&["attachment", &id]), "source_status"),
        "declared-unverified"
    );
    assert_eq!(
        value(&f.ok(&["attachment", &id, "--full"]), "content").len(),
        body.len()
    );
    fs::write(f.0.join(".oms/artifact-views/view-fixture.json"), b"{}").unwrap();
    assert_eq!(
        value(&f.ok(&["attachment", &id]), "source_status"),
        "drifted-or-unavailable"
    );
    fs::write(f.folder(&id).join("content.txt"), body + "x").unwrap();
    f.bad(&["attachment", &id, "--full"]);
}

#[test]
fn sensitive_variants_refuse_without_snapshot_leaks() {
    let f = Fixture::new();
    let mut bodies = Vec::new();
    for parts in [
        ["gh", "p_"],
        ["github", "_pat_"],
        ["s", "k-proj-"],
        ["gh", "p-"],
        ["Bearer", " "],
    ] {
        bodies.push(parts.concat() + &"q".repeat(30));
    }
    bodies.push(["-----BE", "GIN RSA PRIVATE ", "KEY-----"].concat());
    bodies.push(["{\"pass", "word\":\"redact\"}"].concat());
    for body in bodies {
        let (code, out, err) = f.raw(&[
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "note",
            "--text",
            &body,
        ]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(!String::from_utf8_lossy(&err).contains(&body));
    }
    assert!(!f.0.join(".oms").exists());
}

#[test]
fn malformed_metadata_and_reserved_output_fields() {
    let f = Fixture::new();
    let id = f.capture();
    let path = f.folder(&id).join("meta.json");
    let original = fs::read_to_string(&path).unwrap();
    for malformed in [
        original.replace("\"source\": {", "\"source\": null, \"unused\": {"),
        original.replace("\"source\": {", "\"unused\": {"),
        original.replace("\"full_file_sha256\":", "\"missing_digest\":"),
    ] {
        fs::write(&path, malformed).unwrap();
        f.bad(&["attachment", &id]);
    }
    let reserved = original.replacen(
        '{',
        "{\"content\":\"forged\",\"truncated\":true,\"source_status\":\"forged\",",
        1,
    );
    fs::write(path, reserved).unwrap();
    let opened = f.ok(&["attachment", &id]);
    assert_eq!(
        value(&opened, "content"),
        "first line\r\nselected line\r\nlast line\n"
    );
    assert_eq!(field(&opened, "truncated"), &J::Bool(false));
    assert_eq!(value(&opened, "source_status"), "current");
}

#[test]
fn canonical_leaf_help_and_diagnostic_bytes() {
    let f = Fixture::new();
    let fixture = oms_context::parse_json(include_bytes!("fixtures/cli.json")).unwrap();
    let rows = array(&fixture);
    assert!(rows.len() <= 128);
    for row in rows {
        let args = array(field(row, "args"))
            .iter()
            .map(|v| {
                let J::String(s) = v else {
                    panic!("argument");
                };
                OsStr::from_bytes(s)
            })
            .collect::<Vec<_>>();
        let expected = value(row, "stdout").as_bytes();
        let diagnostics = value(row, "stderr").as_bytes();
        assert!(expected.len() + diagnostics.len() <= 4096);
        let (code, out, err) = f.call(&args, &[], false);
        let J::Number(status) = field(row, "exit") else {
            panic!("exit");
        };
        assert_eq!(
            (code, out, err),
            (
                status.parse().unwrap(),
                expected.to_vec(),
                diagnostics.to_vec()
            ),
            "argv: {args:?}"
        );
    }
}

#[test]
fn top_level_help_is_native_and_global_options_reach_each_leaf() {
    let f = Fixture::new();
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["attach", "--help"],
        vec!["attach", "-h"],
        vec!["--json", "--help", "attach", "file"],
        vec!["attach", "--repo=x", "--help"],
    ] {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(oms_command_input::owns(&args));
        let borrowed = args.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        let (code, out, _) = f.call(&borrowed, &[], true);
        assert_eq!((code, out.starts_with(b"usage: oms input")), (0, true));
        // The Git redirect guard precedes parsing, so help refuses too.
        let (code, out, err) = f.call(&borrowed, &[("GIT_DIR", f.0.as_os_str())], true);
        assert_eq!((code, out.is_empty()), (2, true));
        assert!(String::from_utf8_lossy(&err).contains("redirect environment variables"));
    }
    let other = Fixture::new();
    for args in [
        vec![
            "--repo",
            other.0.to_str().unwrap(),
            "--json",
            "attach",
            "file",
            "source.txt",
        ],
        vec![
            "attach",
            "--repo",
            other.0.to_str().unwrap(),
            "file",
            "source.txt",
        ],
        vec![
            "attach",
            "file",
            "--repo",
            other.0.to_str().unwrap(),
            "source.txt",
            "--json",
        ],
    ] {
        assert_eq!(value(&f.ok(&args), "repo"), other.0.to_str().unwrap());
    }
    let record = f.ok(&[
        "attach",
        "selection",
        "--sou=text",
        "--ori=note",
        "--text=--paragraph",
    ]);
    assert_eq!(
        value(
            &f.ok(&["attachment", value(&record, "id"), "--fu"]),
            "content"
        ),
        "--paragraph"
    );
    fs::write(f.0.join("-1"), b"negative filename\n").unwrap();
    f.ok(&["attach", "file", "-1"]);
    fs::write(f.0.join("-٢"), b"negative Unicode filename\n").unwrap();
    f.ok(&["attach", "file", "-٢"]);
    fs::write(f.0.join("-file name"), b"dash filename with a space\n").unwrap();
    f.ok(&["attach", "file", "-file name"]);
    let negative = f.raw(&["attach", "file", "source.txt", "--start-line", "-1 "]);
    assert_eq!(negative.0, 2);
    assert!(String::from_utf8_lossy(&negative.2).contains("selected lines are outside"));
    f.ok(&[
        "attach",
        "file",
        "source.txt",
        "--start-line=+0_2",
        "--end-line",
        " 2 ",
    ]);
    f.ok(&[
        "attach",
        "file",
        "source.txt",
        "--start-line",
        "٢",
        "--end-line",
        "２",
    ]);
    f.ok(&[
        "--re",
        f.0.to_str().unwrap(),
        "attach",
        "file",
        "source.txt",
    ]);
    let equal = "9".repeat(4300);
    let at_limit = f.raw(&["attach", "file", "source.txt", "--start-line", &equal]);
    assert_eq!(at_limit.0, 2);
    assert!(String::from_utf8_lossy(&at_limit.2).contains("selected lines are outside"));
    let beyond = equal + "9";
    let over_limit = f.raw(&["attach", "file", "source.txt", "--start-line", &beyond]);
    assert_eq!(over_limit.0, 2);
    assert!(String::from_utf8_lossy(&over_limit.2).starts_with("usage: oms input attach file"));
}

#[test]
fn actual_snapshot_ignore_guard_refuses_narrow_foreign_ignore() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join(".oms/inputs")).unwrap();
    let ignore = f.0.join(".oms/inputs/.gitignore");
    fs::write(&ignore, b"queue.json\n").unwrap();
    let result = f.raw(&["attach", "file", "source.txt"]);
    assert_eq!(result.0, 2);
    assert!(result.1.is_empty());
    assert_eq!(fs::read(&ignore).unwrap(), b"queue.json\n");
    assert!(!f.0.join(".oms/inputs/attachments").exists());
    fs::write(&ignore, b"queue.json\nattachments/**/meta.json\n").unwrap();
    assert_eq!(f.raw(&["attach", "file", "source.txt"]).0, 2);
    assert!(!f.0.join(".oms/inputs/attachments").exists());
    fs::write(ignore, b"queue.json\nattachments/\n").unwrap();
    f.capture();
}

#[test]
fn listing_skips_uncommitted_strays_and_concurrent_cleanup() {
    let f = Fixture::new();
    f.capture();
    let attachments = f.0.join(".oms/inputs/attachments");
    fs::write(attachments.join("stray"), b"unrelated").unwrap();
    fs::create_dir(attachments.join(".malformed-id")).unwrap();
    fs::create_dir(attachments.join("incomplete")).unwrap();
    fs::write(attachments.join("incomplete/content.txt"), b"incomplete").unwrap();
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        1
    );
    let active = AtomicBool::new(true);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + test_bound();
            while active.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                let path = attachments.join("cleanup-race");
                let _ = fs::create_dir(&path);
                let _ = fs::remove_dir(&path);
            }
        });
        let report = f.raw(&["attachments"]);
        active.store(false, Ordering::Relaxed);
        assert_eq!(report.0, 0, "{}", String::from_utf8_lossy(&report.2));
    });
    fs::write(attachments.join(".malformed-id/meta.json"), b"{}").unwrap();
    f.bad(&["attachments"]);
    fs::remove_dir_all(attachments.join(".malformed-id")).unwrap();
    symlink(f.0.join("source.txt"), attachments.join("unsafe")).unwrap();
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        1
    );
    let id = value(
        &array(field(&f.ok(&["attachments"]), "attachments"))[0],
        "id",
    )
    .to_owned();
    let meta = f.folder(&id).join("meta.json");
    fs::remove_file(&meta).unwrap();
    symlink(f.0.join("source.txt"), meta).unwrap();
    f.bad(&["attachments"]);
}

fn compiled_process_fixture(f: &Fixture) -> PathBuf {
    let bin = f.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let result = Command::new("rustc")
        .args(["+1.99.1", "--edition=2024"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_git.rs"))
        .arg("-o")
        .arg(bin.join("git"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    bin
}
fn test_bound() -> std::time::Duration {
    std::time::Duration::from_secs(
        std::env::var("OMS_TEST_CALL_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60),
    )
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + test_bound();
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "condition exceeded suite call timeout"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
struct ProcessCleanup(PathBuf);
impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        if let Ok(log) = fs::read_to_string(&self.0) {
            for raw in log.split_whitespace().take(2) {
                if let Some(pid) = raw
                    .parse::<i32>()
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
        }
    }
}

#[test]
fn git_fsmonitor_and_child_environment_are_inert() {
    let f = Fixture::new();
    let bin = compiled_process_fixture(&f);
    fs::copy(bin.join("git"), f.0.join("monitor")).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(["add", "source.txt"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(["config", "core.fsmonitor"])
            .arg(f.0.join("monitor"))
            .status()
            .unwrap()
            .success()
    );
    let marker = f.0.join("monitor-ran");
    let result = f.call(
        &[
            OsStr::new("attach"),
            OsStr::new("file"),
            OsStr::new("source.txt"),
        ],
        &[("OMS_INPUT_MONITOR_MARKER", marker.as_os_str())],
        false,
    );
    assert_eq!(result.0, 0);
    assert!(!marker.exists(), "source-configured monitor executed");
    let real_git = Command::new("git").arg("--exec-path").output().unwrap();
    let real_git = PathBuf::from(String::from_utf8(real_git.stdout).unwrap().trim()).join("git");
    let real_git = if real_git.is_file() {
        real_git
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join("git"))
            .find(|p| p.is_file())
            .unwrap()
    };
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let log = f.0.join("audit");
    let result = f.call(
        &[OsStr::new("attachments")],
        &[
            ("PATH", OsStr::new(&path)),
            ("OMS_INPUT_PROCESS_MODE", OsStr::new("audit")),
            ("OMS_INPUT_PROCESS_LOG", log.as_os_str()),
            ("OMS_INPUT_REAL_GIT", real_git.as_os_str()),
        ],
        false,
    );
    assert_eq!(result.0, 0);
    for line in fs::read_to_string(log).unwrap().lines() {
        assert!(line.starts_with("0\t-c\tcore.fsmonitor=false\t"), "{line}");
    }
}

#[test]
fn git_timeout_overflow_and_interruption_terminate_only_owned_group() {
    let f = Fixture::new();
    let bin = compiled_process_fixture(&f);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut unrelated = OwnedChild(
        Command::new(bin.join("git"))
            .arg("--descendant")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    for mode in ["orphan", "overflow", "interrupt"] {
        let log = f.0.join(format!("process-{mode}"));
        let _cleanup = ProcessCleanup(log.clone());
        let mut extra = vec![
            ("PATH", OsStr::new(&path)),
            ("OMS_INPUT_PROCESS_MODE", OsStr::new(mode)),
            ("OMS_INPUT_PROCESS_LOG", log.as_os_str()),
        ];
        if mode == "interrupt" {
            extra.push(("OMS_INPUT_INTERRUPT_WHEN_READY", log.as_os_str()));
        }
        let (code, out, err) = f.call(&[OsStr::new("attachments")], &extra, false);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        if mode == "interrupt" {
            assert!(String::from_utf8_lossy(&err).contains("interrupted"));
        }
        let pids: Vec<i32> = fs::read_to_string(&log)
            .unwrap()
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 3);
        assert_eq!(pids[0], pids[2], "Git did not own its process group");
        let leader = rustix::process::Pid::from_raw(pids[0]).unwrap();
        assert!(
            rustix::process::test_kill_process(leader).is_err(),
            "direct child was not reaped"
        );
        wait_for(|| {
            let result = Command::new("ps")
                .args(["-o", "stat=", "-p", &pids[1].to_string()])
                .output()
                .unwrap();
            result.stdout.is_empty()
                || String::from_utf8_lossy(&result.stdout)
                    .trim_start()
                    .starts_with('Z')
        });
        assert!(unrelated.0.try_wait().unwrap().is_none());
    }
    unrelated.0.kill().unwrap();
    unrelated.0.wait().unwrap();
}

#[test]
fn accepted_registry_float_numbers_use_python_canonical_hash() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let id = f.capture();
    let digest = "d".repeat(64);
    // schema 1e0 compares equal to schema 1 in the accepted feedback reader.
    let registry = format!(
        "{{\"schema\":1e0,\"id\":\"view-float\",\"data_sha256\":\"{digest}\",\"row_keys\":[],\"scale\":1e-5,\"zero\":-0}} "
    );
    let canonical = format!(
        "{{\"data_sha256\":\"{digest}\",\"id\":\"view-float\",\"row_keys\":[],\"scale\":1e-05,\"schema\":1.0,\"zero\":0}}"
    );
    let folder = f.0.join(".oms/artifact-views");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("view-float.json"), &registry).unwrap();
    let body = b"feedback";
    fs::write(f.folder(&id).join("content.txt"), body).unwrap();
    let metadata = format!(
        "{{\"schema\":1,\"id\":{id:?},\"repo\":{repo:?},\"kind\":\"artifact-feedback\",\"content_bytes\":8,\"content_sha256\":\"{body_hash:x}\",\"created_at\":\"2026-01-01T00:00:00Z\",\"source\":{{\"view_id\":\"view-float\",\"data_sha256\":\"{digest}\",\"registry_sha256\":\"{registry_hash:x}\"}}}}",
        repo = f.0.to_str().unwrap(),
        body_hash = Sha256::digest(body),
        registry_hash = Sha256::digest(canonical.as_bytes())
    );
    fs::write(f.folder(&id).join("meta.json"), metadata).unwrap();
    assert_eq!(
        value(&f.ok(&["attachment", &id]), "source_status"),
        "declared-unverified"
    );
}

#[test]
fn shared_oms_modes_and_owner_survive_private_attachment_capture() {
    use std::os::unix::fs::MetadataExt;
    for mode in [0o750, 0o755] {
        let f = Fixture::new();
        let shared = f.0.join(".oms");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(mode)).unwrap();
        let before = fs::metadata(&shared).unwrap();
        let id = f.capture();
        let after = fs::metadata(&shared).unwrap();
        assert_eq!(after.mode() & 0o777, mode);
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        for path in [
            shared.join("inputs"),
            shared.join("inputs/attachments"),
            f.folder(&id),
        ] {
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o700);
        }
        for name in ["content.txt", "meta.json"] {
            assert_eq!(
                fs::metadata(f.folder(&id).join(name)).unwrap().mode() & 0o777,
                0o600
            );
        }
    }
    let f = Fixture::new();
    f.capture();
    assert_eq!(
        fs::metadata(f.0.join(".oms")).unwrap().mode() & 0o777,
        0o700
    );
}

/// Stands in for `oms room send`: appends a message row to the fixture's room log.
struct RoomFixture(String);
impl oms_command_input::HostServices for RoomFixture {
    fn with_queue_lock(
        &self,
        queue_path: &Path,
        interrupted: &AtomicBool,
        action: &mut dyn FnMut() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        use oms_command_input::{DefaultHostServices, LOCK_TIMEOUT};
        match self.0.as_str() {
            "locktimeout" => Err(LOCK_TIMEOUT),
            "hooked" => {
                let repo = queue_path.ancestors().nth(3).unwrap();
                let mut log = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(repo.join("hook.log"))
                    .unwrap();
                std::io::Write::write_all(
                    &mut log,
                    format!("{}\n", queue_path.display()).as_bytes(),
                )
                .unwrap();
                action()
            }
            _ => DefaultHostServices.with_queue_lock(queue_path, interrupted, action),
        }
    }
    fn send_room_note(
        &self,
        repo: &Path,
        room: &str,
        from: &str,
        to: &str,
        text: &str,
        message_id: &str,
    ) -> Result<(), String> {
        match self.0.as_str() {
            "fail" => return Err("append refused".to_owned()),
            "drop" => return Ok(()),
            _ => {}
        }
        let text = if self.0 == "tamper" { "other" } else { text };
        if self.0 == "rebind" {
            // The recipient leaves and re-enrolls while the append is in flight.
            append_event(
                repo,
                room,
                "Left",
                r#"{"kind": "leave", "participant": "worker"}"#,
            );
            append_event(repo, room, "Joined: worker", REJOIN);
        }
        let event = format!(
            r#"{{"kind": "message", "id": "{message_id}", "sender": "{from}", "recipient": "{to}", "message_kind": "note"}}"#
        );
        append_event(repo, room, text, &event);
        Ok(())
    }
}
const REJOIN: &str = r#"{"kind": "join", "participant": "worker", "provider": "claude", "role": "worker", "model": null, "label": "worker", "owns": [], "parent": "main"}"#;
fn append_event(repo: &Path, room: &str, text: &str, event: &str) {
    let path = repo.join(".oms/threads").join(format!("{room}.jsonl"));
    let seq = fs::read_to_string(&path).unwrap().lines().count() + 1;
    let row = format!(
        r#"{{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "{room}", "seq": {seq}, "role": "note", "agent": "x", "text": {}, "room_event": {event}}}"#,
        quote(text)
    );
    let mut log = fs::OpenOptions::new().append(true).open(path).unwrap();
    std::io::Write::write_all(&mut log, format!("{row}\n").as_bytes()).unwrap();
}
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const ROOM_ROWS: [&str; 4] = [
    r#"{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "input-room", "seq": 1, "role": "topic", "agent": "x", "text": "Input fixture", "live": true}"#,
    r#"{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "input-room", "seq": 2, "role": "note", "agent": "x", "text": "Input fixture", "room_event": {"kind": "created", "participant": "main"}}"#,
    r#"{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "input-room", "seq": 3, "role": "note", "agent": "x", "text": "Joined: main", "room_event": {"kind": "join", "participant": "main", "provider": "codex", "role": "main", "model": null, "label": "main", "owns": [], "consumer": "c1"}}"#,
    r#"{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "input-room", "seq": 4, "role": "note", "agent": "x", "text": "Joined: worker", "room_event": {"kind": "join", "participant": "worker", "provider": "claude", "role": "worker", "model": null, "label": "worker", "owns": [], "parent": "main"}}"#,
];
const ROUTE: [&str; 6] = ["--room", "input-room", "--from", "main", "--to", "worker"];

impl Fixture {
    /// The standard room under leading-underscore names: `_room`, `_sender`, `_worker`.
    fn underscore_room(&self) {
        let dir = self.0.join(".oms/threads");
        fs::create_dir_all(&dir).unwrap();
        let rows = ROOM_ROWS
            .iter()
            .map(|r| {
                r.replace("input-room", "_room")
                    .replace(r#""participant": "main""#, r#""participant": "_sender""#)
                    .replace(r#""parent": "main""#, r#""parent": "_sender""#)
                    .replace(r#""participant": "worker""#, r#""participant": "_worker""#)
            })
            .collect::<Vec<_>>();
        fs::write(dir.join("_room.jsonl"), rows.join("\n") + "\n").unwrap();
    }
    fn room(&self) {
        let dir = self.0.join(".oms/threads");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("input-room.jsonl"), ROOM_ROWS.join("\n") + "\n").unwrap();
    }
    fn log(&self) -> PathBuf {
        self.0.join(".oms/threads/input-room.jsonl")
    }
    fn push_row(&self, event: &str) {
        let seq = fs::read_to_string(self.log()).unwrap().lines().count() + 1;
        let row = format!(
            r#"{{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "input-room", "seq": {seq}, "role": "note", "agent": "x", "text": "event", "room_event": {event}}}"#
        );
        let mut log = fs::OpenOptions::new()
            .append(true)
            .open(self.log())
            .unwrap();
        std::io::Write::write_all(&mut log, format!("{row}\n").as_bytes()).unwrap();
    }
    fn with(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, J, String) {
        let extra = extra
            .iter()
            .map(|(k, v)| (*k, OsStr::new(*v)))
            .collect::<Vec<_>>();
        let (code, out, err) = self.call(
            &args.iter().map(OsStr::new).collect::<Vec<_>>(),
            &extra,
            false,
        );
        let report = if out.is_empty() {
            J::Null
        } else {
            oms_context::parse_json(&out).unwrap()
        };
        (code, report, String::from_utf8(err).unwrap())
    }
    fn parent(&self, args: &[&str], services: &str) -> (i32, J, String) {
        self.with(args, &[("OMS_INPUT_SERVICES", services)])
    }
    fn refused(&self, args: &[&str], message: &str) {
        let (code, out, err) = self.with(args, &[]);
        assert_eq!((code, &out), (2, &J::Null), "{args:?}: {err}");
        assert!(err.contains(message), "{args:?}: {err}");
    }
    fn queue(&self, mode: &str, text: &str, extra: &[&str]) -> J {
        let mut args = vec!["add"];
        args.extend(ROUTE);
        args.extend(["--mode", mode, "--text", text]);
        args.extend(extra);
        let (code, report, err) = self.with(&args, &[]);
        assert_eq!(code, 0, "{err}");
        report
    }
    fn state(&self) -> J {
        oms_context::parse_json(&fs::read(self.0.join(".oms/inputs/queue.json")).unwrap()).unwrap()
    }
    fn threads(&self) -> usize {
        fs::read_to_string(self.log()).unwrap().lines().count()
    }
}
fn text_of<'a>(v: &'a J, key: &str) -> &'a str {
    value(v, key)
}
fn number_of(v: &J, key: &str) -> i64 {
    let J::Number(n) = field(v, key) else {
        panic!("number")
    };
    n.parse().unwrap()
}
fn raw(v: &J, key: &str) -> Option<J> {
    let J::Object(fields) = v else { return None };
    fields
        .iter()
        .find(|(k, _)| k == key.as_bytes())
        .map(|(_, v)| match v {
            J::Null => J::Null,
            J::Bool(b) => J::Bool(*b),
            J::Number(n) => J::Number(n.clone()),
            J::String(s) => J::String(s.clone()),
            _ => J::Null,
        })
}

#[test]
fn queue_add_edit_move_list_show_cancel_and_bounds() {
    let f = Fixture::new();
    f.room();
    let ids: Vec<String> = (0..9).map(|_| f.capture()).collect();
    let first = f.queue("next", "first", &["--attachment", &ids[0]]);
    assert_eq!(text_of(&first, "status"), "queued");
    assert_eq!(number_of(&first, "version"), 1);
    assert_eq!(text_of(&first, "mode"), "next");
    assert_eq!(
        text_of(&first, "message_id"),
        format!("msg-{}", text_of(&first, "id"))
    );
    assert_eq!(value(&array(field(&first, "attachments"))[0], "id"), ids[0]);
    assert_eq!(
        value(field(field(&first, "binding"), "identity"), "root_room"),
        "input-room"
    );
    let note = f.0.join("note.txt");
    fs::write(&note, "second\n").unwrap();
    let (code, second, err) = f.with(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--mode",
            "steer",
            "--text-file",
            note.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let (first_id, second_id) = (text_of(&first, "id"), text_of(&second, "id"));
    let listed = f.ok(&["list"]);
    assert_eq!(
        (number_of(&listed, "schema"), number_of(&listed, "revision")),
        (1, 2)
    );
    assert_eq!(array(field(&listed, "items")).len(), 2);
    assert_eq!(text_of(&f.ok(&["show", first_id]), "text"), "first");

    // Edits: text, mode, replacement and cleared attachments, each bumping the version.
    let edited = f.ok(&["edit", first_id, "--text", "edited", "--mode", "steer"]);
    assert_eq!(
        (text_of(&edited, "text"), number_of(&edited, "version")),
        ("edited", 2)
    );
    assert!(raw(&edited, "edited_at").is_some());
    assert_eq!(array(field(&edited, "attachments")).len(), 1);
    f.refused(
        &[
            "edit",
            first_id,
            "--attachment",
            &ids[1],
            "--clear-attachments",
        ],
        "choose replacement attachments",
    );
    f.refused(
        &[
            "edit",
            first_id,
            "--attachment",
            &ids[1],
            "--attachment",
            &ids[1],
        ],
        "at most eight distinct",
    );
    assert_eq!(
        array(field(
            &f.ok(&["edit", first_id, "--attachment", &ids[1]]),
            "attachments"
        ))
        .len(),
        1
    );
    assert!(
        array(field(
            &f.ok(&["edit", first_id, "--clear-attachments"]),
            "attachments"
        ))
        .is_empty()
    );
    // An empty --text-file value counts as absent, so this edit changes only the version.
    assert_eq!(
        number_of(&f.ok(&["edit", first_id, "--text-file", ""]), "version"),
        5
    );
    f.refused(&["edit", first_id, "--text", "   "], "text is empty");
    f.refused(
        &["edit", "input-missing", "--text", "x"],
        "input item does not exist",
    );

    // Ordering, including the no-write self move.
    let order = |f: &Fixture| {
        array(field(&f.ok(&["list"]), "items"))
            .iter()
            .map(|i| value(i, "id").to_owned())
            .collect::<Vec<_>>()
    };
    f.ok(&["move", second_id, "--before", first_id]);
    assert_eq!(order(&f), [second_id, first_id]);
    f.ok(&["move", second_id]);
    assert_eq!(order(&f), [first_id, second_id]);
    let revision = number_of(&f.ok(&["list"]), "revision");
    f.ok(&["move", first_id, "--before", first_id]);
    assert_eq!(number_of(&f.ok(&["list"]), "revision"), revision);
    f.refused(
        &["move", first_id, "--before", "input-missing"],
        "input item does not exist",
    );

    // Cancellation is terminal.
    assert_eq!(text_of(&f.ok(&["cancel", first_id]), "status"), "cancelled");
    f.refused(&["cancel", first_id], "only queued inputs may be cancelled");
    f.refused(
        &["edit", first_id, "--text", "x"],
        "only queued inputs may be edited",
    );
    f.refused(&["move", first_id], "only queued inputs may be reordered");
    f.refused(
        &["move", second_id, "--before", first_id],
        "move target must be queued",
    );

    // Text, attachment and identity edges.
    fn route<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["add"];
        args.extend(ROUTE);
        args.extend(extra);
        args
    }
    let limit = "x".repeat(3500);
    f.ok(&route(&["--text", &limit]));
    f.refused(
        &route(&["--text", &format!("{limit}x")]),
        "text is empty, oversized",
    );
    f.refused(&route(&["--text", ""]), "text is empty");
    f.refused(&route(&["--text", "bell\u{7}"]), "unsupported control");
    f.refused(
        &route(&["--text", "k", "--attachment", "../x"]),
        "exact local identifier",
    );
    f.refused(
        &route(&["--text", "k", "--attachment", "attachment-missing"]),
        "attachment is missing",
    );
    let eight: Vec<&str> = ids[..8]
        .iter()
        .flat_map(|i| ["--attachment", i.as_str()])
        .collect();
    f.ok(&[route(&["--text", "eight"]), eight.clone()].concat());
    f.refused(
        &[
            route(&["--text", "nine"]),
            eight,
            vec!["--attachment", &ids[0]],
        ]
        .concat(),
        "at most eight distinct",
    );
    for (who, to, message) in [
        ("main", "main", "distinct explicit"),
        ("main", "all", "distinct explicit"),
        ("main", "ghost", "unavailable, or unsafe"),
        ("worker", "ghost", "unavailable, or unsafe"),
    ] {
        f.refused(
            &[
                "add",
                "--room",
                "input-room",
                "--from",
                who,
                "--to",
                to,
                "--text",
                "x",
            ],
            message,
        );
    }
    f.refused(
        &[
            "add", "--room", "no-room", "--from", "main", "--to", "worker", "--text", "x",
        ],
        "unavailable, or unsafe",
    );
    f.refused(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--mode",
            "steer",
            "--text",
            "x",
            "--attachment",
            "",
        ],
        "exact local identifier",
    );

    // Room and participant names may lead with an underscore; attachment and input IDs may not.
    let u = Fixture::new();
    u.underscore_room();
    let underscored = |extra: &[&str]| {
        let mut a = vec![
            "add", "--room", "_room", "--from", "_sender", "--to", "_worker",
        ];
        a.extend(extra);
        u.with(&a, &[])
    };
    let (code, row, err) = underscored(&["--text", "underscored"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(text_of(field(&row, "binding"), "room"), "_room");
    assert_eq!(text_of(field(&row, "binding"), "to"), "_worker");
    u.ok(&["preview", text_of(&row, "id")]);
    assert_eq!(underscored(&["--text", "x", "--attachment", "_bad"]).0, 2);
    u.refused(&["show", "_bad"], "exact local identifier");
    f.room();
    f.refused(
        &[
            "add", "--room", "_room", "--from", "main", "--to", "worker", "--text", "x",
        ],
        "unavailable, or unsafe",
    );

    // The 256-item bound counts existing rows; one below still accepts.
    let dummy = |n: usize| {
        (0..n)
            .map(|i| format!(r#"{{"id": "input-d{i}", "status": "cancelled"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    };
    let queue = f.0.join(".oms/inputs/queue.json");
    let repo = f.0.to_str().unwrap();
    for (count, accepted) in [(255, true), (256, false)] {
        fs::write(&queue, format!(r#"{{"feedback_batches": {{}}, "items": [{}], "repo": "{repo}", "revision": 7, "schema": 1}}"#, dummy(count))).unwrap();
        let (code, _, err) = f.with(&route(&["--text", "bound"]), &[]);
        assert_eq!(code == 0, accepted, "{err}");
        if !accepted {
            assert!(err.contains("256-item bound"));
        }
    }
    fs::write(&queue, format!(r#"{{"feedback_batches": {{}}, "items": [{}], "repo": "{repo}", "revision": 7, "schema": 1}}"#, dummy(257))).unwrap();
    f.refused(&["list"], "invalid or oversized input queue");
    for bad in [
        r#"{"feedback_batches": {}, "items": [], "repo": "/elsewhere", "revision": 0, "schema": 1}"#.to_owned(),
        format!(r#"{{"feedback_batches": {{}}, "items": [], "repo": "{repo}", "revision": 0, "schema": 2}}"#),
        format!(r#"{{"feedback_batches": [], "items": [], "repo": "{repo}", "revision": 0, "schema": 1}}"#),
    ] {
        fs::write(&queue, bad).unwrap();
        f.refused(&["list"], "invalid or oversized input queue");
    }
    fs::remove_file(&queue).unwrap();
    assert_eq!(number_of(&f.ok(&["list"]), "revision"), 0);
    fs::write(f.0.join("real.json"), "{}").unwrap();
    symlink(f.0.join("real.json"), &queue).unwrap();
    f.bad(&["list"]);
    fs::remove_file(&queue).unwrap();
    // The bound applies to the indented bytes actually written: compact 1 MB, indented over 4 MiB.
    let pad = vec!["1"; 500_000].join(",");
    let big = format!(
        r#"{{"feedback_batches": {{}}, "items": [{{"id": "input-pad", "status": "queued", "pad": [{pad}]}}], "repo": "{repo}", "revision": 3, "schema": 1}}"#
    );
    assert!(big.len() < 4 * 1024 * 1024);
    fs::write(&queue, &big).unwrap();
    f.refused(&["cancel", "input-pad"], "exceeds its 4 MiB bound");
    assert_eq!(fs::read_to_string(&queue).unwrap(), big);
    // The rejected write left a state that still reads.
    assert_eq!(f.raw(&["list"]).0, 0);
    fs::remove_file(&queue).unwrap();
    // Git-tracked private state is refused before any write.
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(["add", "-f", ".oms/inputs/.gitignore"])
            .status()
            .unwrap()
            .success()
    );
    f.refused(&route(&["--text", "tracked"]), "must not be tracked by Git");
}

#[test]
fn concurrent_adds_serialize_on_the_queue_lock() {
    let f = Fixture::new();
    f.room();
    std::thread::scope(|scope| {
        for n in 0..6 {
            let f = &f;
            scope.spawn(move || {
                f.queue("steer", &format!("parallel {n}"), &[]);
            });
        }
    });
    let state = f.state();
    assert_eq!(array(field(&state, "items")).len(), 6);
    assert_eq!(number_of(&state, "revision"), 6);
    let leftovers = fs::read_dir(f.0.join(".oms/inputs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".queue.json."))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn deliver_preview_digest_boundary_order_and_dedup() {
    let f = Fixture::new();
    f.room();
    let first = f.queue("next", "one", &[]);
    let second = f.queue("next", "two", &[]);
    let (first_id, second_id) = (text_of(&first, "id"), text_of(&second, "id"));
    let revision = number_of(&f.ok(&["list"]), "revision");
    let preview = f.ok(&["preview", first_id]);
    assert_eq!(field(&preview, "at_boundary_required"), &J::Bool(true));
    assert_eq!(field(&preview, "executes_model"), &J::Bool(false));
    let digest = text_of(&preview, "payload_sha256").to_owned();
    assert_eq!(text_of(&preview, "room"), "input-room");
    assert!(
        text_of(&preview, "text")
            .starts_with(&format!("OMS queued input / next / {first_id}\n\none"))
    );
    let dry = f.ok(&["deliver", first_id, "--expected-sha256", &digest]);
    assert_eq!(field(&dry, "dry_run"), &J::Bool(true));
    assert!(
        f.ok(&["deliver", first_id, "--expected-sha256", ""])
            .is_dry()
    );
    assert_eq!(number_of(&f.ok(&["list"]), "revision"), revision);
    assert_eq!(f.threads(), 4);
    f.refused(
        &["deliver", first_id, "--expected-sha256", &"0".repeat(64)],
        "changed since preview",
    );
    for malformed in ["abc", &digest.to_uppercase(), &format!("{digest}0")] {
        f.refused(
            &["deliver", first_id, "--expected-sha256", malformed],
            "full SHA-256 digest",
        );
    }
    let (code, _, err) = f.parent(&["deliver", first_id, "--apply"], "append");
    assert_eq!(code, 2);
    assert!(err.contains("--at-boundary"));
    assert_eq!(f.threads(), 4);
    // The second input waits behind the first of its target and mode.
    let (code, _, err) = f.parent(
        &["deliver", second_id, "--apply", "--at-boundary"],
        "append",
    );
    assert_eq!(code, 2);
    assert!(err.contains("earlier input is queued"));
    let (code, sent, err) = f.parent(
        &[
            "deliver",
            first_id,
            "--apply",
            "--at-boundary",
            "--expected-sha256",
            &digest,
        ],
        "append",
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(text_of(&sent, "status"), "delivered");
    let receipt = field(&sent, "receipt");
    assert_eq!(
        (text_of(receipt, "room"), number_of(receipt, "seq")),
        ("input-room", 5)
    );
    assert_eq!(field(receipt, "acknowledged"), &J::Bool(false));
    assert_eq!(f.threads(), 5);
    // A delivered input is never sent twice, applied or not.
    for apply in [
        vec!["deliver", first_id],
        vec!["deliver", first_id, "--apply", "--at-boundary"],
    ] {
        let (code, again, _) = f.parent(&apply, "append");
        assert_eq!((code, field(&again, "deduplicated")), (0, &J::Bool(true)));
    }
    assert_eq!(f.threads(), 5);
    let (code, _, _) = f.parent(
        &["deliver", second_id, "--apply", "--at-boundary"],
        "append",
    );
    assert_eq!(code, 0);
    let steer = f.queue("steer", "steer now", &[]);
    let (code, sent, _) = f.parent(&["deliver", text_of(&steer, "id"), "--apply"], "append");
    assert_eq!((code, text_of(&sent, "status")), (0, "delivered"));
    f.refused(&["cancel", first_id], "only queued inputs may be cancelled");
}
trait Dry {
    fn is_dry(&self) -> bool;
}
impl Dry for J {
    fn is_dry(&self) -> bool {
        field(self, "dry_run") == &J::Bool(true)
    }
}

#[test]
fn uncertain_delivery_exits_three_and_reconciles_without_duplicates() {
    let f = Fixture::new();
    f.room();
    let item = f.queue("steer", "careful", &[]);
    let id = text_of(&item, "id");
    let (code, report, err) = f.parent(&["deliver", id, "--apply"], "drop");
    assert_eq!(
        (code, text_of(&report, "status")),
        (3, "uncertain"),
        "{err}"
    );
    assert!(raw(&report, "delivery_error").is_none());
    let (code, shown, _) = f.with(&["show", id], &[]);
    assert_eq!((code, text_of(&shown, "status")), (3, "uncertain"));
    for args in [
        vec!["deliver", id, "--apply"],
        vec!["cancel", id],
        vec!["edit", id, "--text", "x"],
    ] {
        let (code, _, err) = f.parent(&args, "append");
        assert_eq!(code, 2, "{args:?}");
        assert!(
            err.contains("not queued") || err.contains("only queued"),
            "{err}"
        );
    }
    let (code, report, _) = f.with(&["reconcile", id], &[]);
    assert_eq!(
        (
            code,
            field(&report, "observed"),
            field(&report, "retry_allowed")
        ),
        (3, &J::Bool(false), &J::Bool(false))
    );
    assert!(raw(&report, "last_reconciled_at").is_some());
    assert_eq!(f.threads(), 4);
    // The message lands after the fact; reconcile finds it and never resends.
    let delivery = text_of(field(&shown, "delivery"), "text").to_owned();
    let message = text_of(&shown, "message_id").to_owned();
    let row = format!(
        r#"{{"kind": "message", "id": "{message}", "sender": "main", "recipient": "worker", "message_kind": "note"}}"#
    );
    let line = format!(
        r#"{{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "input-room", "seq": 5, "role": "note", "agent": "x", "text": {}, "room_event": {row}}}"#,
        quote(&delivery)
    );
    let mut log = fs::OpenOptions::new().append(true).open(f.log()).unwrap();
    std::io::Write::write_all(&mut log, format!("{line}\n").as_bytes()).unwrap();
    let (code, report, err) = f.with(&["reconcile", id], &[]);
    assert_eq!(
        (code, text_of(&report, "status")),
        (0, "delivered"),
        "{err}"
    );
    assert_eq!(field(&report, "observed"), &J::Bool(true));
    assert_eq!(f.threads(), 5);
    f.push_row(&format!(
        r#"{{"kind": "received", "participant": "worker", "messages": ["{message}"]}}"#
    ));
    let report = f.ok(&["reconcile", id]);
    assert_eq!(
        field(field(&report, "receipt"), "acknowledged"),
        &J::Bool(true)
    );
    assert_eq!(
        array(field(field(&report, "receipt"), "acknowledgments")).len(),
        1
    );
    let queued = f.queue("steer", "never sent", &[]);
    f.refused(
        &["reconcile", text_of(&queued, "id")],
        "only attempted deliveries",
    );
    f.ok(&["cancel", text_of(&queued, "id")]);
    f.refused(&["reconcile", "input-missing"], "input item does not exist");

    // A failed append and a conflicting message both stay uncertain with an error note.
    for mode in ["fail", "tamper"] {
        let item = f.queue("steer", mode, &[]);
        let id = text_of(&item, "id");
        let (code, report, _) = f.parent(&["deliver", id, "--apply"], mode);
        assert_eq!(
            (code, text_of(&report, "status")),
            (3, "uncertain"),
            "{mode}"
        );
        assert!(text_of(&report, "delivery_error").contains("room append failed"));
        let (code, _, err) = f.with(&["reconcile", id], &[]);
        let expected = if mode == "fail" { 3 } else { 2 };
        assert_eq!(code, expected, "{mode}: {err}");
    }
}

#[test]
fn identity_change_blocks_preview_and_delivery() {
    let f = Fixture::new();
    f.room();
    let id = text_of(&f.queue("steer", "bound", &[]), "id").to_owned();
    f.ok(&["preview", &id]);
    f.push_row(r#"{"kind": "bind", "participant": "worker", "consumer": "new"}"#);
    for args in [vec!["preview", &id], vec!["deliver", &id]] {
        f.refused(&args, "sender, recipient, or room owner changed");
    }
    let (code, _, err) = f.parent(&["deliver", &id, "--apply"], "append");
    assert_eq!(code, 2, "{err}");
    assert_eq!(f.threads(), 5);
    f.push_row(r#"{"kind": "leave", "participant": "worker"}"#);
    f.refused(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--text",
            "x",
        ],
        "unavailable, or unsafe",
    );
    f.push_row(r#"{"kind": "closed", "participant": "main"}"#);
    f.refused(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "main",
            "--text",
            "x",
        ],
        "closed, sealed, or incompletely carried",
    );
}

fn digest_of(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn feedback_fixture(f: &Fixture, next: &str) -> String {
    feedback_fixture_with(f, next, "c")
}
/// `comment` is spliced into JSON text as is, so callers pass it already escaped.
fn feedback_fixture_with(f: &Fixture, next: &str, comment: &str) -> String {
    let data = "d".repeat(64);
    let canonical = format!(
        r#"{{"data_sha256":"{data}","events":[{{"action":"select","comment":"{comment}","row_key":"r1"}},{{"action":"reject","comment":"","row_key":"r2"}}],"report":{{"next":"{next}"}},"view_id":"view-1"}}"#
    );
    let batch = digest_of(&canonical);
    let views = f.0.join(".oms/artifact-views");
    fs::create_dir_all(&views).unwrap();
    fs::write(
        views.join("view-1.json"),
        format!(
            r#"{{"schema": 1, "id": "view-1", "data_sha256": "{data}", "row_keys": ["r1", "r2"]}}"#
        ),
    )
    .unwrap();
    let row = format!(
        r#"{{"schema": 1, "batch_id": "{batch}", "view_id": "view-1", "data_sha256": "{data}", "events": [{{"row_key": "r1", "action": "select", "comment": "{comment}"}}, {{"row_key": "r2", "action": "reject", "comment": ""}}], "report": {{"next": "{next}"}}}}"#
    );
    fs::write(views.join("feedback.jsonl"), row + "\n").unwrap();
    batch
}

#[test]
fn feedback_batch_queues_one_attachment_and_deduplicates() {
    let f = Fixture::new();
    f.room();
    let batch = feedback_fixture(&f, "ship it");
    let mut args = vec!["feedback", "--batch", &batch];
    args.extend(ROUTE);
    let queued = f.ok(&args);
    assert_eq!(field(&queued, "deduplicated"), &J::Bool(false));
    let item = field(&queued, "input");
    let attachment = value(&array(field(item, "attachments"))[0], "id").to_owned();
    assert!(text_of(item, "text").contains("Selections and decisions: reject=1, select=1."));
    assert!(text_of(item, "text").contains("Requested next step:\nship it"));
    assert_eq!(
        text_of(field(item, "provenance"), "kind"),
        "artifact-feedback"
    );
    assert_eq!(
        array(field(field(item, "provenance"), "selected_rows")).len(),
        1
    );
    let record = f.ok(&["attachment", &attachment]);
    assert_eq!(
        (text_of(&record, "kind"), text_of(&record, "source_status")),
        ("artifact-feedback", "current")
    );
    let again = f.ok(&args);
    assert_eq!(field(&again, "deduplicated"), &J::Bool(true));
    assert_eq!(text_of(field(&again, "input"), "id"), text_of(item, "id"));
    assert_eq!(
        array(field(&f.ok(&["attachments"]), "attachments")).len(),
        1
    );
    let mut steer = args.clone();
    steer.extend(["--mode", "steer"]);
    f.refused(
        &steer,
        "already mapped to a different target, identity, or mode",
    );
    let unknown = "a".repeat(64);
    let mut other = vec!["feedback", "--batch", unknown.as_str()];
    other.extend(ROUTE);
    f.refused(&other, "not in the local artifact-view import ledger");
    let mut short = vec!["feedback", "--batch", "abc"];
    short.extend(ROUTE);
    f.refused(&short, "full SHA-256 digest");
    // Registry drift is caught at preview, before any delivery.
    fs::write(f.0.join(".oms/artifact-views/view-1.json"), format!(r#"{{"schema": 1, "id": "view-1", "data_sha256": "{}", "row_keys": ["r1", "r2", "r3"]}}"#, "d".repeat(64))).unwrap();
    f.refused(
        &["preview", text_of(item, "id")],
        "feedback view source changed",
    );
}

#[test]
fn feedback_bounds_next_step_and_ledger_conflicts() {
    let f = Fixture::new();
    f.room();
    let ok = feedback_fixture(&f, &"n".repeat(2000));
    let mut args = vec!["feedback", "--batch", &ok];
    args.extend(ROUTE);
    f.ok(&args);
    let g = Fixture::new();
    g.room();
    let long = feedback_fixture(&g, &"n".repeat(3000));
    let mut args = vec!["feedback", "--batch", &long];
    args.extend(ROUTE);
    g.refused(&args, "exceeds the room input budget");
    assert!(!g.0.join(".oms/inputs/attachments").exists());
    assert_ne!(ok, long);
    // Two ledger rows for one batch must agree.
    // Escaped quotes hide an assignment from the serialized bytes; decoded strings are scanned.
    let s = Fixture::new();
    s.room();
    let name = ["api", "key"].join("_");
    for comment in [
        format!(r#"{{\"{name}\":\"v\"}}"#),
        format!(r#"plain {name}: v"#),
    ] {
        let batch = feedback_fixture_with(&s, "x", &comment);
        let mut args = vec!["feedback", "--batch", &batch];
        args.extend(ROUTE);
        s.refused(&args, "secret-like content");
        assert!(!s.0.join(".oms/inputs/queue.json").exists());
        assert!(!s.0.join(".oms/inputs/attachments").exists());
    }
    let u = Fixture::new();
    u.underscore_room();
    let batch = feedback_fixture(&u, "x");
    let (code, queued, err) = u.with(
        &[
            "feedback", "--batch", &batch, "--room", "_room", "--from", "_sender", "--to",
            "_worker",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        text_of(field(field(&queued, "input"), "binding"), "to"),
        "_worker"
    );
    let h = Fixture::new();
    h.room();
    let batch = feedback_fixture(&h, "x");
    let path = h.0.join(".oms/artifact-views/feedback.jsonl");
    let row = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        format!(
            "{row}{}",
            row.replace("\"comment\": \"c\"", "\"comment\": \"d\"")
        ),
    )
    .unwrap();
    let mut args = vec!["feedback", "--batch", &batch];
    args.extend(ROUTE);
    h.refused(&args, "conflicting import records");
}

fn fake_tmux(f: &Fixture) -> (PathBuf, String) {
    let bin = f.0.join("fake-bin");
    fs::create_dir(&bin).unwrap();
    let script = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LOG"
[ "$4" = "%7" ] || exit 1
case "$1" in
display-message) printf '%%7\t%s\t%s\t%s\n' "${FAKE_PID:-12345}" "$FAKE_CWD" "${FAKE_DEAD:-0}" ;;
capture-pane) [ -z "$FAKE_BYTES" ] || { /usr/bin/head -c "$FAKE_BYTES" /dev/zero | /usr/bin/tr '\0' x; exit 0; }
    printf '%b' "${FAKE_CAPTURE:-old line\nselected output\nlast line\n}" ;;
*) exit 9 ;;
esac
"#;
    fs::write(bin.join("tmux"), script).unwrap();
    fs::set_permissions(bin.join("tmux"), fs::Permissions::from_mode(0o755)).unwrap();
    let git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    symlink(git.trim(), bin.join("git")).unwrap();
    let log = f.0.join("tmux.log");
    (bin, log.to_str().unwrap().to_owned())
}

#[test]
fn terminal_capture_is_bounded_read_only_and_drift_checked() {
    let f = Fixture::new();
    f.room();
    let (bin, log) = fake_tmux(&f);
    let cwd = f.0.to_str().unwrap().to_owned();
    let path = bin.to_str().unwrap().to_owned();
    let env = |extra: &[(&str, &str)]| {
        let mut all = vec![
            ("PATH", path.as_str()),
            ("FAKE_LOG", log.as_str()),
            ("FAKE_CWD", cwd.as_str()),
        ];
        all.extend(extra.iter().copied());
        all.into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect::<Vec<_>>()
    };
    let run = |args: &[&str], extra: &[(&str, &str)]| {
        let pairs = env(extra);
        let pairs = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>();
        f.with(args, &pairs)
    };
    let (code, record, err) = run(&["attach", "terminal", "--pane", "%7", "--lines", "2"], &[]);
    assert_eq!(code, 0, "{err}");
    let source = field(&record, "source");
    assert_eq!(
        (
            text_of(&record, "kind"),
            text_of(source, "pane"),
            text_of(source, "pid")
        ),
        ("terminal", "%7", "12345")
    );
    assert_eq!(field(source, "verified"), &J::Bool(true));
    assert_eq!(number_of(source, "lines"), 2);
    let id = text_of(&record, "id").to_owned();
    let (_, shown, _) = run(&["attachment", &id], &[]);
    assert_eq!(
        (text_of(&shown, "content"), text_of(&shown, "source_status")),
        ("selected output\nlast line\n", "current")
    );
    // Only the two read-only subcommands are ever issued.
    let issued = fs::read_to_string(&log).unwrap();
    assert!(
        issued
            .lines()
            .all(|l| l.starts_with("display-message") || l.starts_with("capture-pane")),
        "{issued}"
    );
    assert!(issued.contains("capture-pane -p -t %7 -S -2"));
    // Default 80 lines; the 1 and 200 limits pass, 0 and 201 refuse before tmux runs.
    assert_eq!(run(&["attach", "terminal", "--pane", "%7"], &[]).0, 0);
    for lines in ["1", "200"] {
        assert_eq!(
            run(
                &["attach", "terminal", "--pane", "%7", "--lines", lines],
                &[]
            )
            .0,
            0
        );
    }
    let before = fs::read_to_string(&log).unwrap().lines().count();
    for lines in ["0", "201", "-1"] {
        let (code, _, err) = run(
            &["attach", "terminal", "--pane", "%7", "--lines", lines],
            &[],
        );
        assert_eq!(code, 2);
        assert!(err.contains("1 to 200 lines"), "{err}");
    }
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), before);
    for (pane, message) in [
        ("7", "exact tmux pane ID"),
        ("%7x", "exact tmux pane ID"),
        ("%", "exact tmux pane ID"),
        ("%8", "local capture failed"),
    ] {
        let (code, _, err) = run(&["attach", "terminal", "--pane", pane], &[]);
        assert_eq!(code, 2);
        assert!(err.contains(message), "{pane}: {err}");
    }
    let (_, _, err) = run(
        &["attach", "terminal", "--pane", "%7"],
        &[("FAKE_DEAD", "1")],
    );
    assert!(err.contains("unavailable or not live"), "{err}");
    let outside = fs::canonicalize(std::env::temp_dir()).unwrap();
    let outside = outside.to_str().unwrap();
    let (_, _, err) = run(
        &["attach", "terminal", "--pane", "%7"],
        &[("FAKE_CWD", outside)],
    );
    assert!(err.contains("outside the selected repository"), "{err}");
    let (_, _, err) = run(
        &["attach", "terminal", "--pane", "%7"],
        &[("FAKE_CAPTURE", "\\n")],
    );
    assert!(err.contains("text is empty"), "{err}");
    // The 64 KiB capture bound: equal passes, one byte more refuses.
    let (code, _, err) = run(
        &["attach", "terminal", "--pane", "%7"],
        &[("FAKE_BYTES", "65536")],
    );
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run(
        &["attach", "terminal", "--pane", "%7"],
        &[("FAKE_BYTES", "65537")],
    );
    assert_eq!(code, 2);
    assert!(
        err.contains("local capture exceeded its byte bound"),
        "{err}"
    );
    // tmux absent from PATH.
    let empty = f.0.join("empty-bin");
    fs::create_dir(&empty).unwrap();
    symlink(bin.join("git"), empty.join("git")).unwrap();
    let (_, _, err) = f.with(
        &["attach", "terminal", "--pane", "%7"],
        &[("PATH", empty.to_str().unwrap())],
    );
    assert!(err.contains("tmux is required"), "{err}");
    // A changed pane makes the snapshot drift and blocks queueing.
    let (_, drifted, _) = run(&["attachment", &id], &[("FAKE_PID", "999")]);
    assert_eq!(text_of(&drifted, "source_status"), "drifted-or-unavailable");
    let (code, _, err) = run(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--text",
            "x",
            "--attachment",
            &id,
        ],
        &[("FAKE_PID", "999")],
    );
    assert_eq!(code, 2);
    assert!(
        err.contains("terminal pane identity or cwd changed"),
        "{err}"
    );
    assert_eq!(
        run(
            &[
                "add",
                "--room",
                "input-room",
                "--from",
                "main",
                "--to",
                "worker",
                "--text",
                "x",
                "--attachment",
                &id
            ],
            &[]
        )
        .0,
        0
    );
}

#[test]
fn sealed_worker_reads_only_attachments_delivered_to_its_identity() {
    let f = Fixture::new();
    f.room();
    let (delivered, other) = (f.capture(), f.capture());
    let item = f.queue("steer", "see attachment", &["--attachment", &delivered]);
    let id = text_of(&item, "id").to_owned();
    let child = |participant: Option<&'static str>| {
        let mut env = vec![
            ("OMS_HARNESS_CHILD", "1"),
            ("OMS_ROOM_REPO", f.0.to_str().unwrap()),
            ("OMS_ROOM_ID", "input-room"),
        ];
        env.extend(participant.map(|p| ("OMS_ROOM_PARTICIPANT", p)));
        env
    };
    let refusal = "admitted worker identity";
    let ask = |who: Option<&'static str>, attachment: &str| {
        f.with(&["attachment", attachment], &child(who))
    };
    // Queued but undelivered is no grant.
    let (code, out, err) = ask(Some("worker"), &delivered);
    assert_eq!((code, out), (2, J::Null));
    assert!(err.contains(refusal), "{err}");
    let (code, _, _) = f.parent(&["deliver", &id, "--apply"], "append");
    assert_eq!(code, 0);
    let (code, record, err) = ask(Some("worker"), &delivered);
    assert_eq!(code, 0, "{err}");
    assert_eq!(text_of(&record, "id"), delivered);
    // The room repository default applies to read-only worker calls.
    let elsewhere = Fixture::new();
    let (code, _, err) = elsewhere.with(&["attachment", &delivered], &child(Some("worker")));
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = ask(Some("worker"), &other);
    assert_eq!(code, 2);
    assert!(err.contains(refusal), "{err}");
    for (who, message) in [
        (Some("main"), "admitted child participant"),
        (Some("ghost"), "unavailable, or unsafe"),
        (None, "admitted room repository and participant"),
    ] {
        let (code, out, err) = ask(who, &delivered);
        assert_eq!((code, out), (2, J::Null), "{who:?}");
        assert!(err.contains(message), "{who:?}: {err}");
    }
    // Worker-readable evidence is the persisted receipt; removing it revokes the grant.
    let kept = fs::read_to_string(f.log()).unwrap();
    let truncated = kept.lines().take(4).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(f.log(), truncated).unwrap();
    assert_eq!(ask(Some("worker"), &delivered).0, 2);
    fs::write(f.log(), kept).unwrap();
    assert_eq!(ask(Some("worker"), &delivered).0, 0);
    // Another room identity (rebound worker) is not the delivered identity.
    f.push_row(r#"{"kind": "bind", "participant": "worker", "consumer": "other"}"#);
    assert_eq!(ask(Some("worker"), &delivered).0, 2);
    // Every queue command stays parent-only.
    for args in [
        vec!["list"],
        vec!["attachments"],
        vec!["show", id.as_str()],
        vec!["preview", id.as_str()],
        vec!["deliver", id.as_str()],
        vec!["cancel", id.as_str()],
        vec!["reconcile", id.as_str()],
        vec!["move", id.as_str()],
        vec!["edit", id.as_str(), "--text", "x"],
        vec![
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--text",
            "x",
        ],
        vec![
            "feedback", "--batch", "a", "--room", "r", "--from", "a", "--to", "b",
        ],
        vec!["attach", "terminal", "--pane", "%7"],
        vec!["attach", "file", "source.txt"],
    ] {
        let (code, out, err) = f.with(&args, &child(Some("worker")));
        assert_eq!((code, out), (2, J::Null), "{args:?}");
        assert!(
            err.contains("queue inspection is parent-only"),
            "{args:?}: {err}"
        );
    }
    // Delegate depth marks a child too.
    let (code, _, err) = f.with(&["list"], &[("OMS_HARNESS_DELEGATE_DEPTH", "1")]);
    assert_eq!(code, 2);
    assert!(err.contains("parent-only"));
}

#[test]
fn git_redirect_environment_refuses_every_action_before_any_read() {
    let f = Fixture::new();
    let id = f.capture();
    for args in [
        vec!["attachments"],
        vec!["attachment", id.as_str()],
        vec!["attach", "file", "source.txt"],
        vec!["attach", "terminal", "--pane", "%7"],
        vec![
            "add", "--room", "r", "--from", "a", "--to", "b", "--text", "x",
        ],
        vec!["list"],
        vec!["show", "x"],
        vec!["deliver", "x", "--apply"],
        vec!["bogus-action"],
    ] {
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            let (code, out, err) = f.with(&args, &[(key, f.0.to_str().unwrap())]);
            assert_eq!((code, out), (2, J::Null), "{key} {args:?}");
            assert!(
                err.starts_with("error: input refuses Git repository/index redirect"),
                "{err}"
            );
        }
    }
}

#[test]
fn every_input_invocation_is_native() {
    for args in [
        vec![],
        vec!["list"],
        vec!["attach", "terminal"],
        vec!["--help"],
        vec!["no-such-action"],
        vec!["--repo", "x", "show", "i"],
    ] {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(oms_command_input::owns(&args), "{args:?}");
    }
}

#[test]
fn default_services_invoke_the_native_room_send_without_python() {
    let f = Fixture::new();
    f.room();
    let bin = f.0.join("oms-bin");
    fs::create_dir(&bin).unwrap();
    let script = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\nexit \"${FAKE_EXIT:-0}\"\n";
    fs::write(bin.join("oms"), script).unwrap();
    fs::set_permissions(bin.join("oms"), fs::Permissions::from_mode(0o755)).unwrap();
    let git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap()
        .stdout;
    symlink(String::from_utf8(git).unwrap().trim(), bin.join("git")).unwrap();
    let (path, log) = (
        bin.to_str().unwrap().to_owned(),
        f.0.join("oms.log").to_str().unwrap().to_owned(),
    );
    for exit in ["0", "7"] {
        let item = f.queue("steer", exit, &[]);
        let id = text_of(&item, "id");
        let env = [
            ("PATH", path.as_str()),
            ("FAKE_LOG", log.as_str()),
            ("FAKE_EXIT", exit),
        ];
        let (code, report, err) = f.with(&["deliver", id, "--apply"], &env);
        // The stand-in appends nothing, so the delivery cannot be confirmed.
        assert_eq!(
            (code, text_of(&report, "status")),
            (3, "uncertain"),
            "{err}"
        );
        assert_eq!(raw(&report, "delivery_error").is_some(), exit == "7");
        let logged = fs::read_to_string(&log).unwrap();
        let call = &logged[logged.rfind("room send --repo ").unwrap()..];
        let message = text_of(&report, "message_id");
        assert!(
            call.starts_with("room send --repo ")
                && call.contains(
                    " --id input-room --participant main --to worker --kind note --message-id "
                )
                && call.contains(message)
                && call.contains(&format!(" --text OMS queued input / steer / {id}")),
            "{call}"
        );
    }
    // Nothing reached the room log, so reconcile finds nothing and never retries.
    assert_eq!(f.threads(), 4);
}

#[test]
fn identity_change_during_append_cannot_claim_confirmed_delivery() {
    let f = Fixture::new();
    f.room();
    let id = text_of(&f.queue("steer", "raced", &[]), "id").to_owned();
    let (code, report, err) = f.parent(&["deliver", &id, "--apply"], "rebind");
    assert_eq!(
        (code, text_of(&report, "status")),
        (3, "uncertain"),
        "{err}"
    );
    assert!(text_of(&report, "delivery_error").contains("room append failed"));
    // The message exists, but under a different enrollment of the recipient.
    assert_eq!(f.threads(), 7);
    f.refused(
        &["reconcile", &id],
        "different participant or room-owner identity",
    );
    f.refused(&["deliver", &id, "--apply"], "not queued");
    assert_eq!(f.threads(), 7);
    // A worker that left and rejoined holds no attachment grant from before.
    let g = Fixture::new();
    g.room();
    let attachment = g.capture();
    let sent = g.queue("steer", "grant", &["--attachment", &attachment]);
    assert_eq!(
        g.parent(&["deliver", text_of(&sent, "id"), "--apply"], "append")
            .0,
        0
    );
    g.push_row(r#"{"kind": "leave", "participant": "worker"}"#);
    g.push_row(REJOIN);
    let env = [
        ("OMS_HARNESS_CHILD", "1"),
        ("OMS_ROOM_REPO", g.0.to_str().unwrap()),
        ("OMS_ROOM_ID", "input-room"),
        ("OMS_ROOM_PARTICIPANT", "worker"),
    ];
    assert_eq!(g.with(&["attachment", &attachment], &env).0, 2);
    // Another admitted worker never receives it either.
    g.push_row(r#"{"kind": "join", "participant": "other-worker", "provider": "claude", "role": "worker", "model": null, "label": "other", "owns": [], "parent": "main"}"#);
    let env = [
        env[0],
        env[1],
        env[2],
        ("OMS_ROOM_PARTICIPANT", "other-worker"),
    ];
    assert_eq!(g.with(&["attachment", &attachment], &env).0, 2);
}

const SEALED: &str = "room-72fd19647ec9";
fn successor(f: &Fixture) {
    f.push_row(&format!(
        r#"{{"kind": "successor", "participant": "main", "target": "{SEALED}"}}"#
    ));
    let rows = ROOM_ROWS
        .iter()
        .map(|row| row.replace("input-room", SEALED).replace(
            r#""kind": "created", "participant": "main""#,
            r#""kind": "created", "participant": "main", "predecessor": "input-room""#,
        ))
        .chain([format!(
            r#"{{"schema": 1, "ts": "2026-01-01T00:00:00Z", "thread": "{SEALED}", "seq": 5, "role": "note", "agent": "x", "text": "Carried", "room_event": {{"kind": "carried"}}}}"#
        )])
        .collect::<Vec<_>>();
    fs::write(
        f.0.join(format!(".oms/threads/{SEALED}.jsonl")),
        rows.join("\n") + "\n",
    )
    .unwrap();
}

#[test]
fn rollover_preserves_addressing_lineage_and_historical_receipts() {
    let f = Fixture::new();
    f.room();
    let first = f.queue("steer", "before seal", &[]);
    let second = f.queue("next", "after seal", &[]);
    let (first_id, second_id) = (text_of(&first, "id"), text_of(&second, "id"));
    assert_eq!(f.parent(&["deliver", first_id, "--apply"], "append").0, 0);
    let message = text_of(&first, "message_id").to_owned();
    f.push_row(&format!(
        r#"{{"kind": "received", "participant": "worker", "messages": ["{message}"]}}"#
    ));
    successor(&f);
    // The unsealed id now resolves to its successor; the receipt stays in the sealed log.
    let preview = f.ok(&["preview", second_id]);
    assert_eq!(text_of(&preview, "room"), SEALED);
    let report = f.ok(&["reconcile", first_id]);
    let receipt = field(&report, "receipt");
    assert_eq!(
        (text_of(receipt, "room"), number_of(receipt, "seq")),
        ("input-room", 5)
    );
    assert_eq!(field(receipt, "acknowledged"), &J::Bool(true));
    assert_eq!(array(field(receipt, "acknowledgments")).len(), 1);
    // New inputs bind to the successor but keep the original root room.
    let fresh = f.queue("steer", "carried", &[]);
    let route = field(&fresh, "binding");
    assert_eq!(text_of(route, "resolved_room"), SEALED);
    assert_eq!(text_of(field(route, "identity"), "root_room"), "input-room");
    let (code, sent, err) = f.parent(&["deliver", text_of(&fresh, "id"), "--apply"], "append");
    assert_eq!(code, 0, "{err}");
    assert_eq!(text_of(field(&sent, "receipt"), "room"), SEALED);
    // A seal that the successor does not name back is not followed.
    let g = Fixture::new();
    g.room();
    g.push_row(r#"{"kind": "successor", "participant": "main", "target": "room-aaaaaaaaaaaa"}"#);
    let unlinked = g.0.join(".oms/threads/room-aaaaaaaaaaaa.jsonl");
    fs::write(
        &unlinked,
        ROOM_ROWS
            .iter()
            .map(|r| r.replace("input-room", "room-aaaaaaaaaaaa"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    g.refused(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--text",
            "x",
        ],
        "closed, sealed, or incompletely carried",
    );
}

#[test]
fn concurrent_delivery_has_one_durable_message() {
    let f = Fixture::new();
    f.room();
    let id = text_of(&f.queue("steer", "once", &[]), "id").to_owned();
    let results = std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|_| scope.spawn(|| f.parent(&["deliver", &id, "--apply"], "append")))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        results.iter().all(|(code, ..)| *code == 0),
        "{:?}",
        results.iter().map(|r| &r.2).collect::<Vec<_>>()
    );
    assert_eq!(f.threads(), 5);
    assert_eq!(
        results
            .iter()
            .filter(|(_, r, _)| raw(r, "deduplicated").is_some())
            .count(),
        1
    );
}

#[test]
fn payload_bound_lazy_item_ids_and_terminal_byte_limit() {
    let f = Fixture::new();
    f.room();
    let attachment = f.capture();
    let text = "t".repeat(3500);
    let base = [
        "add",
        "--room",
        "input-room",
        "--from",
        "main",
        "--to",
        "worker",
        "--text",
        &text,
    ];
    f.ok(&[&base[..], &["--attachment", &attachment]].concat());
    let extra: Vec<String> = (0..2).map(|_| f.capture()).collect();
    let many: Vec<&str> = std::iter::once(attachment.as_str())
        .chain(extra.iter().map(String::as_str))
        .flat_map(|a| ["--attachment", a])
        .collect();
    // 3500 text bytes plus three references exceed the 4000-byte message bound.
    f.refused(&[&base[..], &many[..]].concat(), "text is empty, oversized");
    // An empty queue never validates the identifier; a populated one does.
    let g = Fixture::new();
    g.room();
    for args in [
        vec!["show", "!"],
        vec!["cancel", "!"],
        vec!["move", "!", "--before", "?"],
        vec!["reconcile", "!"],
        vec!["preview", "!"],
    ] {
        g.refused(&args, "input item does not exist");
    }
    g.queue("steer", "one", &[]);
    for args in [vec!["show", "!"], vec!["cancel", "!"], vec!["deliver", "!"]] {
        g.refused(&args, "exact local identifier");
    }
}

#[test]
fn runtime_lock_path_matches_the_legacy_hash_and_directory() {
    use oms_command_input::runtime_lock_path;
    // Python: sha256("/q/.oms/inputs/queue.json")[:32]; no ancestor exists but "/".
    let queue = Path::new("/q/.oms/inputs/queue.json");
    assert_eq!(
        runtime_lock_path(queue, Some(Path::new("locks"))),
        Some(PathBuf::from(
            "locks/runtime-685069c842e65a72180cf0990f94e6f9.lock"
        ))
    );
    // resolve(strict=False): relative, dotted and symlinked spellings agree before and after
    // the parent directory exists.
    let f = Fixture::new();
    let absolute = f.0.join(".oms/inputs/queue.json");
    let expected = PathBuf::from(format!(
        "locks/runtime-{}.lock",
        &digest_of(absolute.to_str().unwrap())[..32]
    ));
    let depth = std::env::current_dir().unwrap().components().count() - 1;
    let relative = PathBuf::from("../".repeat(depth)).join(absolute.strip_prefix("/").unwrap());
    let dotted = f.0.join("x/../././.oms/inputs/queue.json");
    fs::create_dir(f.0.join("real")).unwrap();
    symlink(f.0.join("real"), f.0.join("link")).unwrap();
    let linked = f.0.join("link/.oms/inputs/queue.json");
    let linked_expected = PathBuf::from(format!(
        "locks/runtime-{}.lock",
        &digest_of(f.0.join("real/.oms/inputs/queue.json").to_str().unwrap())[..32]
    ));
    // `link -> sub/target`: `..` pops the resolved target (`sub`), not the link's parent.
    fs::create_dir_all(f.0.join("dir/sub/target")).unwrap();
    symlink("sub/target", f.0.join("dir/link")).unwrap();
    let through = f.0.join("dir/link/../.oms/inputs/queue.json");
    let through_expected = PathBuf::from(format!(
        "locks/runtime-{}.lock",
        &digest_of(f.0.join("dir/sub/.oms/inputs/queue.json").to_str().unwrap())[..32]
    ));
    for stage in 0..2 {
        assert_eq!(
            runtime_lock_path(&through, Some(Path::new("locks"))).as_ref(),
            Some(&through_expected),
            "stage {stage}"
        );
        for spelling in [&absolute, &relative, &dotted] {
            assert_eq!(
                runtime_lock_path(spelling, Some(Path::new("locks"))).as_ref(),
                Some(&expected),
                "stage {stage}: {spelling:?}"
            );
        }
        assert_eq!(
            runtime_lock_path(&linked, Some(Path::new("locks"))).as_ref(),
            Some(&linked_expected),
            "stage {stage}"
        );
        fs::create_dir_all(f.0.join(".oms/inputs")).unwrap();
        fs::create_dir_all(f.0.join("real/.oms/inputs")).unwrap();
        fs::create_dir_all(f.0.join("dir/sub/.oms/inputs")).unwrap();
    }
}

#[test]
fn queue_mutations_run_under_the_injected_lock_and_timeouts_exit_75() {
    let f = Fixture::new();
    f.room();
    let id = text_of(
        &f.parent(
            &[
                "add",
                "--room",
                "input-room",
                "--from",
                "main",
                "--to",
                "worker",
                "--mode",
                "steer",
                "--text",
                "a",
            ],
            "hooked",
        )
        .1,
        "id",
    )
    .to_owned();
    let hooks = |f: &Fixture| {
        fs::read_to_string(f.0.join("hook.log"))
            .unwrap()
            .lines()
            .count()
    };
    assert_eq!(hooks(&f), 1);
    for (args, expected) in [
        (vec!["edit", id.as_str(), "--text", "b"], 2),
        (vec!["move", id.as_str()], 3),
        (vec!["list"], 3),
        (vec!["show", id.as_str()], 3),
        (vec!["preview", id.as_str()], 3),
        (vec!["deliver", id.as_str()], 3),
        (vec!["deliver", id.as_str(), "--apply"], 4),
        (vec!["reconcile", id.as_str()], 5),
    ] {
        f.parent(&args, "hooked");
        assert_eq!(hooks(&f), expected, "{args:?}");
    }
    let queue = f.0.join(".oms/inputs/queue.json").canonicalize().unwrap();
    assert!(
        fs::read_to_string(f.0.join("hook.log"))
            .unwrap()
            .lines()
            .all(|l| Path::new(l) == queue)
    );
    // A timeout from the host exits 75 before any state is touched.
    let g = Fixture::new();
    g.room();
    for args in [
        vec![
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--text",
            "x",
        ],
        vec!["edit", "input-x", "--text", "x"],
        vec!["move", "input-x"],
        vec!["cancel", "input-x"],
        vec!["reconcile", "input-x"],
        vec!["deliver", "input-x", "--apply"],
    ] {
        let (code, out, err) = g.parent(&args, "locktimeout");
        assert_eq!((code, out), (75, J::Null), "{args:?}");
        assert!(err.contains("timed out waiting for state lock"), "{err}");
    }
    assert!(!g.0.join(".oms/inputs/queue.json").exists());
}

#[test]
fn default_lock_held_past_the_timeout_exits_75() {
    let f = Fixture::new();
    f.room();
    f.capture();
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(f.0.join(".oms/inputs/.queue.lock"))
        .unwrap();
    lock.lock().unwrap();
    let started = std::time::Instant::now();
    let (code, out, err) = f.with(
        &[
            "add",
            "--room",
            "input-room",
            "--from",
            "main",
            "--to",
            "worker",
            "--text",
            "x",
        ],
        &[],
    );
    assert_eq!((code, out), (75, J::Null), "{err}");
    assert!(started.elapsed() >= std::time::Duration::from_secs(14));
    // Reads are lock-free, and releasing the lock lets the same call succeed.
    f.ok(&["list"]);
    drop(lock);
    assert_eq!(
        f.with(
            &[
                "add",
                "--room",
                "input-room",
                "--from",
                "main",
                "--to",
                "worker",
                "--text",
                "x"
            ],
            &[]
        )
        .0,
        0
    );
}
