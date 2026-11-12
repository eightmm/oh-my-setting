use oms_context_source::{
    Error, inspect_injections, inspect_session, inspect_timeline, repo_label,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "oms-acquire-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn projections_and_boundaries() {
    let f = Fixture::new();
    let no = || false;
    assert_eq!(
        inspect_injections(&f.0, None, 10, no).unwrap()["injections"],
        0
    );
    let hooks = f.0.join(".oms/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let ledger = hooks.join("events.jsonl");
    std::fs::write(
        &ledger,
        b"{\"action\":\"injection\",\"status\":\"resume\",\"bytes\":7}\ninvalid\n",
    )
    .unwrap();
    let value = inspect_injections(&f.0, None, 10, no).unwrap();
    assert_eq!(value["bytes"], 7);
    assert_eq!(value["invalid_rows"], 1);
    assert!(matches!(
        inspect_injections(&f.0, None, 0, no),
        Err(Error::Projection(_))
    ));
    let path = f.0.join("transcript");
    std::fs::write(&path, b"{\"message\":{\"content\":[{\"type\":\"tool_result\",\"content\":\"hidden fixture\"}]}}\n").unwrap();
    let value = inspect_session(&f.0, &path, 10, no).unwrap();
    assert_eq!(value["lines"], 1);
    assert!(!value.to_string().contains("hidden fixture"));
    assert_eq!(inspect_timeline(&path, 10, no).unwrap()["lines"], 1);
    assert_eq!(inspect_timeline(&path, 10, || true), Err(Error::Cancelled));
    assert_eq!(
        inspect_timeline(&f.0.join("absent"), 10, no),
        Err(Error::Missing)
    );
    assert_eq!(inspect_timeline(&f.0, 10, no), Err(Error::Unsafe));
    assert_eq!(
        inspect_timeline(&path.join("."), 10, no),
        Err(Error::Unsafe)
    );
    assert_eq!(repo_label(&f.0, "missing/../tail").as_deref(), Some("tail"));
    assert_eq!(repo_label(&f.0, ".."), None);
    assert_eq!(repo_label(&f.0, ".").as_deref(), Some("."));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&path, f.0.join("link")).unwrap();
        assert_eq!(
            inspect_timeline(&f.0.join("link"), 10, no),
            Err(Error::Unsafe)
        );
        symlink("missing", f.0.join("dangling")).unwrap();
        assert_eq!(
            repo_label(&f.0, "dangling/file").as_deref(),
            Some("missing/file")
        );
        symlink("..", f.0.join("escape")).unwrap();
        assert_eq!(repo_label(&f.0, "escape/file"), None);
    }
}

#[test]
fn reject_unsafe_inputs_and_observed_change() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let no = || false;
    let hooks = f.0.join(".oms/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let ledger = hooks.join("events.jsonl");
    std::fs::write(&ledger, b"{}\n").unwrap();
    let hard = f.0.join("hard");
    std::fs::hard_link(&ledger, &hard).unwrap();
    assert_eq!(
        inspect_injections(&f.0, None, 1, no).unwrap()["invalid_rows"],
        0
    );
    let checks = Cell::new(0);
    let result = inspect_timeline(&hard, 1, || {
        let n = checks.get();
        checks.set(n + 1);
        if n == 1 {
            std::fs::write(&hard, b"changed content").unwrap();
        }
        false
    });
    assert_eq!(result, Err(Error::Changed));
    std::fs::remove_file(&ledger).unwrap();
    symlink(&hard, &ledger).unwrap();
    assert_eq!(inspect_injections(&f.0, None, 1, no), Err(Error::Unsafe));
    let fifo = f.0.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(inspect_timeline(&fifo, 1, no), Err(Error::Unsafe));
    let socket = f.0.join("socket");
    match std::os::unix::net::UnixListener::bind(&socket) {
        Ok(_listener) => assert_eq!(inspect_timeline(&socket, 1, no), Err(Error::Unsafe)),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(e) => panic!("socket fixture: {e}"),
    }
    let large = f.0.join("large");
    let file = std::fs::File::create(&large).unwrap();
    file.set_len(oms_context_session::MAX_TRANSCRIPT_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(inspect_timeline(&large, 1, no), Err(Error::Oversize));
    symlink("loop2", f.0.join("loop1")).unwrap();
    symlink("loop1", f.0.join("loop2")).unwrap();
    assert_eq!(repo_label(&f.0, "loop1"), None);
}

#[test]
fn line_limits_and_raw_paths() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let no = || false;
    let path = f.0.join(std::ffi::OsString::from_vec(vec![b't', 0xff]));
    let mut body = vec![b' '; oms_context_session::MAX_LINE_BYTES];
    body.push(b'\n');
    body.extend_from_slice(b"\xff\n{}\n");
    if let Err(e) = std::fs::write(&path, body) {
        #[cfg(target_os = "macos")]
        if matches!(e.raw_os_error(), Some(22 | 92)) {
            return;
        }
        panic!("fixture: {e}");
    }
    let projected = inspect_timeline(&path, 200, no).unwrap();
    assert_eq!(projected["lines"], 3);
    assert_eq!(projected["unread_lines"], 2);
    assert!(matches!(
        inspect_timeline(&path, 0, no),
        Err(Error::Projection(_))
    ));
    assert!(matches!(
        inspect_timeline(&path, 201, no),
        Err(Error::Projection(_))
    ));
    assert!(matches!(
        inspect_session(&f.0, std::path::Path::new(""), 1, no),
        Err(Error::Projection(_))
    ));
    let actual = f.0.join("actual");
    std::fs::create_dir(&actual).unwrap();
    std::fs::write(actual.join("t"), b"{}\n").unwrap();
    symlink(&actual, f.0.join("alias")).unwrap();
    assert_eq!(
        inspect_timeline(&f.0.join("alias/t"), 1, no).unwrap()["lines"],
        1
    );
    symlink(&actual, f.0.join(".oms")).unwrap();
    assert_eq!(inspect_injections(&f.0, None, 1, no), Err(Error::Unsafe));
}

#[test]
fn reviewed_boundaries() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let no = || false;
    let transcript = f.0.join("t");
    std::fs::write(&transcript, b"{}\n").unwrap();
    assert_eq!(
        inspect_session(&f.0.join("missing"), std::path::Path::new(""), 1, no),
        Err(Error::Projection(oms_context_session::Error::Invalid(
            "--session needs a transcript path"
        )))
    );
    let absent = f.0.join("absent/t");
    assert_eq!(inspect_timeline(&absent, 1, || true), Err(Error::Cancelled));
    assert_eq!(
        inspect_session(&f.0, &absent, 1, || true),
        Err(Error::Cancelled)
    );
    assert_eq!(
        inspect_session(&f.0.join("missing"), &transcript, 1, no),
        Err(Error::Missing)
    );
    assert_eq!(
        inspect_session(&transcript, &transcript, 1, no),
        Err(Error::Unsafe)
    );
    assert_eq!(repo_label(&transcript, "src/file"), None);
    let first = f.0.join("first");
    let second = f.0.join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    std::fs::write(first.join("t"), b"{}\n").unwrap();
    std::fs::write(second.join("t"), b"{}\n").unwrap();
    let alias = f.0.join("alias");
    symlink(&first, &alias).unwrap();
    let count = Cell::new(0);
    let result = inspect_timeline(&alias.join("t"), 1, || {
        let n = count.get();
        count.set(n + 1);
        if n == 1 {
            std::fs::remove_file(&alias).unwrap();
            symlink(&second, &alias).unwrap();
        }
        false
    });
    assert_eq!(result, Err(Error::Changed));
}

#[test]
fn cancellation_and_missing_ledger_race() {
    use std::cell::Cell;
    let f = Fixture::new();
    let state = f.0.join(".oms");
    std::fs::create_dir(&state).unwrap();
    let calls = Cell::new(0);
    let result = inspect_injections(&f.0, None, 1, || {
        let n = calls.get();
        calls.set(n + 1);
        if n == 2 {
            std::fs::rename(&state, f.0.join("old")).unwrap();
            std::fs::create_dir(&state).unwrap();
        }
        false
    });
    assert_eq!(result, Err(Error::Changed));
    let path = f.0.join("t");
    std::fs::write(&path, b"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"1\",\"name\":\"Read\",\"input\":{\"file_path\":\"a/b/c\"}}]}}\n").unwrap();
    let calls = Cell::new(0);
    let result = inspect_session(&f.0, &path, 1, || {
        let n = calls.get();
        calls.set(n + 1);
        n >= 5
    });
    assert_eq!(result, Err(Error::Cancelled));
}

#[test]
fn traverse_only_parent_and_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::getuid().as_raw() == 0 {
        return;
    }
    let f = Fixture::new();
    let dir = f.0.join("traverse");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("t");
    std::fs::write(&path, b"{}\n").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o111)).unwrap();
    let result = inspect_timeline(&path, 1, || false);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap()["lines"], 1);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
    let result = inspect_timeline(&path, 1, || false);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(result, Err(Error::Unreadable));
    let other = f.0.join("other");
    std::fs::write(&other, b"{}\n").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o0)).unwrap();
    let result = inspect_session(&dir, &other, 1, || false);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result, Err(Error::Unreadable));
}

#[test]
fn ledger_growth_and_root_replacement() {
    use std::cell::Cell;
    let f = Fixture::new();
    let hooks = f.0.join(".oms/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join("events.jsonl");
    std::fs::write(&path, b"{}\n").unwrap();
    let calls = Cell::new(0);
    let result = inspect_injections(&f.0, None, 1, || {
        let n = calls.get();
        calls.set(n + 1);
        if n == 4 {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(oms_context_session::MAX_LEDGER_BYTES as u64 + 1)
                .unwrap();
        }
        false
    });
    assert_eq!(result, Err(Error::Oversize));
    assert_eq!(
        inspect_injections(&f.0, None, 1, || false),
        Err(Error::Oversize)
    );
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(oms_context_session::MAX_LEDGER_BYTES as u64)
        .unwrap();
    assert_eq!(
        inspect_injections(&f.0, None, 1, || false).unwrap()["invalid_rows"],
        1
    );
    let maximum = f.0.join("maximum");
    let file = std::fs::File::create(&maximum).unwrap();
    file.set_len(oms_context_session::MAX_TRANSCRIPT_BYTES as u64)
        .unwrap();
    assert_eq!(
        inspect_timeline(&maximum, 1, || false).unwrap()["unread_lines"],
        1
    );
    let root = f.0.join("repo");
    let old = f.0.join("oldrepo");
    std::fs::create_dir(&root).unwrap();
    let transcript = f.0.join("transcript");
    std::fs::write(&transcript, b"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"1\",\"name\":\"Read\",\"input\":{\"file_path\":\"src\"}}]}}\n").unwrap();
    let calls = Cell::new(0);
    let result = inspect_session(&root, &transcript, 1, || {
        let n = calls.get();
        calls.set(n + 1);
        if n == 5 {
            std::fs::rename(&root, &old).unwrap();
            std::fs::create_dir(&root).unwrap();
        }
        false
    });
    assert_eq!(result, Err(Error::Changed));
}

#[test]
fn ledger_selected_root_retarget() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let a = f.0.join("a");
    let b = f.0.join("b");
    let alias = f.0.join("root");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    symlink(&a, &alias).unwrap();
    let calls = Cell::new(0);
    let result = inspect_injections(&alias, None, 1, || {
        let n = calls.get();
        calls.set(n + 1);
        if n == 1 {
            std::fs::remove_file(&alias).unwrap();
            symlink(&b, &alias).unwrap();
        }
        false
    });
    assert_eq!(result, Err(Error::Changed));
}

#[test]
fn invalid_session_root_is_error() {
    let f = Fixture::new();
    let path = f.0.join("t");
    std::fs::write(&path, b"{}\n").unwrap();
    assert_eq!(
        inspect_session(&f.0.join("missing"), &path, 1, || false),
        Err(Error::Missing)
    );
    assert_eq!(
        inspect_session(&path, &path, 1, || false),
        Err(Error::Unsafe)
    );
}
#[test]
fn regular_file_is_not_a_label_root() {
    let f = Fixture::new();
    let path = f.0.join("f");
    std::fs::write(&path, b"x").unwrap();
    assert_eq!(repo_label(&path, "new"), None);
}
#[test]
fn selected_parent_retarget_is_detected() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let a = f.0.join("a");
    let b = f.0.join("b");
    let alias = f.0.join("alias");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("t"), b"{}\n").unwrap();
    std::fs::write(b.join("t"), b"{}\n").unwrap();
    symlink(&a, &alias).unwrap();
    let calls = Cell::new(0);
    let value = inspect_timeline(&alias.join("t"), 1, || {
        let n = calls.get();
        calls.set(n + 1);
        if n == 1 {
            std::fs::remove_file(&alias).unwrap();
            symlink(&b, &alias).unwrap();
        }
        false
    });
    assert_eq!(value, Err(Error::Changed));
}
#[test]
fn label_resolution_observes_cancellation() {
    use std::cell::Cell;
    let f = Fixture::new();
    let path = f.0.join("t");
    std::fs::write(&path, b"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"1\",\"name\":\"Read\",\"input\":{\"file_path\":\"a/b/c\"}}]}}\n").unwrap();
    let calls = Cell::new(0);
    let value = inspect_session(&f.0, &path, 1, || {
        let n = calls.get();
        calls.set(n + 1);
        n >= 5
    });
    assert_eq!(value, Err(Error::Cancelled));
}

#[test]
fn long_acyclic_links_and_repeated_expansions() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    for n in 0..50 {
        let target = if n == 49 {
            "missing/file".to_owned()
        } else {
            format!("link{}", n + 1)
        };
        symlink(target, f.0.join(format!("link{n}"))).unwrap();
    }
    assert_eq!(repo_label(&f.0, "link0").as_deref(), Some("missing/file"));
    std::fs::create_dir(f.0.join("real")).unwrap();
    symlink("real", f.0.join("again")).unwrap();
    assert_eq!(
        repo_label(&f.0, "again/../again/missing").as_deref(),
        Some("real/missing")
    );
    symlink("cycle2", f.0.join("cycle1")).unwrap();
    symlink("cycle1", f.0.join("cycle2")).unwrap();
    assert_eq!(repo_label(&f.0, "cycle1/tail"), None);
    for n in 0..20 {
        let target = if n == 19 {
            ".".to_owned()
        } else {
            format!("branch{}/branch{}", n + 1, n + 1)
        };
        symlink(target, f.0.join(format!("branch{n}"))).unwrap();
    }
    assert_eq!(repo_label(&f.0, "branch6").as_deref(), Some("."));
    assert_eq!(repo_label(&f.0, "branch0").as_deref(), Some("."));
    let component = "x".repeat(240);
    let prefix = format!("{component}/../").repeat(4);
    for n in 0..10_000 {
        let target = if n == 9_999 {
            "end".to_owned()
        } else {
            format!("{prefix}uncached{}", n + 1)
        };
        symlink(target, f.0.join(format!("uncached{n}"))).unwrap();
    }
    assert_eq!(repo_label(&f.0, "uncached9999").as_deref(), Some("end"));
    assert_eq!(repo_label(&f.0, "uncached0"), None);
    let transcript = f.0.join("transcript");
    std::fs::write(&transcript, b"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"1\",\"name\":\"Read\",\"input\":{\"file_path\":\"link0\"}},{\"type\":\"tool_use\",\"id\":\"2\",\"name\":\"Read\",\"input\":{\"file_path\":\"missing/file\"}},{\"type\":\"tool_use\",\"id\":\"3\",\"name\":\"Read\",\"input\":{\"file_path\":\"branch0\"}}]}}\n").unwrap();
    let value = inspect_session(&f.0, &transcript, 10, || false).unwrap();
    assert_eq!(value["file_reads"]["distinct_in_repository"], 2);
    assert_eq!(value["file_reads"]["outside_repository"], 0);
    assert_eq!(value["file_reads"]["repeated"]["items"][0]["reads"], 2);
}
