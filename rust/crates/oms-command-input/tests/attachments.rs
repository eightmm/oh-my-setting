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
        let result = oms_command_input::run(&args, &repo, &interrupted);
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
    let alias = f.0.join("alias");
    symlink(&f.0, &alias).unwrap();
    f.ok(&["--repo", alias.to_str().unwrap(), "attachments"]);
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
fn unowned_actions_are_pure_and_children_fail_closed() {
    let f = Fixture::new();
    for args in [
        vec!["attach", "terminal"],
        vec!["add"],
        vec!["edit"],
        vec!["move"],
        vec!["list"],
        vec!["show"],
        vec!["preview"],
        vec!["cancel"],
        vec!["deliver"],
        vec!["reconcile"],
        vec!["feedback"],
    ] {
        let owned = args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(!oms_command_input::owns(&owned));
        assert_eq!(
            oms_command_input::run(&owned, Path::new("missing"), &AtomicBool::new(true)),
            (75, vec![], vec![])
        );
    }
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
    assert_eq!(f.raw(&["--help"]).0, 75);
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
fn top_level_help_remains_unowned_and_global_options_reach_each_leaf() {
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
        assert!(!oms_command_input::owns(&args));
        assert_eq!(
            oms_command_input::run(&args, Path::new("missing"), &AtomicBool::new(true)),
            (75, vec![], vec![])
        );
        let borrowed = args.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        assert_eq!(
            f.call(&borrowed, &[("GIT_DIR", f.0.as_os_str())], true),
            (75, vec![], vec![])
        );
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
