use oms_command_failure::{owns, run};
use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};
fn call(args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    run(
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        Path::new("."),
        &AtomicBool::new(false),
    )
}
#[test]
fn classification_and_options() {
    let (status, out, err) = call(&["classify", "verification failed"]);
    assert_eq!(status, 0);
    assert_eq!(out,b"{\"code\":\"verifier_failed\",\"context_escalation_allowed\":false,\"exit\":null,\"fallback_allowed\":false,\"matched\":\"verification fail\",\"recovery\":\"analyze_failure\",\"retryable\":true}\n");
    assert!(err.is_empty());
    assert!(
        String::from_utf8(call(&["classify", "", "--exit", "٠١٢٤"]).1)
            .unwrap()
            .contains("exit-124")
    );
    assert!(
        String::from_utf8(call(&["classify", "timeout", "--code", "provider_capacity"]).1)
            .unwrap()
            .contains("explicit")
    );
    assert!(
        String::from_utf8(call(&["classify", "", "--exit", "999999999999999999999999"]).1)
            .unwrap()
            .contains("999999999999999999999999")
    );
    assert!(
        String::from_utf8(call(&["catalog", "--pretty"]).1)
            .unwrap()
            .starts_with("{\n  \"authority_mismatch\"")
    );
}
#[test]
fn help_and_errors() {
    for args in [
        &[][..],
        &["bad"],
        &["classify"],
        &["catalog", "extra"],
        &["classify", "x", "--exit", "bad"],
        &["--repo"],
    ] {
        assert_eq!(call(args).0, 2, "{args:?}");
    }
    for args in [
        &["--help"][..],
        &["classify", "--help"],
        &["catalog", "--help"],
    ] {
        assert_eq!(call(args).0, 0);
    }
    assert_eq!(
        call(&["--repo", "__absent_oms_test__", "catalog"]).2,
        b"runtime-core: repository path is not a directory: __absent_oms_test__\n"
    );
    assert_eq!(
        run(
            &[OsString::from("catalog")],
            Path::new("."),
            &AtomicBool::new(true)
        )
        .0,
        130
    );
}
#[cfg(unix)]
#[test]
fn os_bytes_survive_matching() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let mut bytes = b"cred".to_vec();
    bytes.extend_from_slice(b"ential ");
    bytes.push(255);
    bytes.extend_from_slice(b" missing");
    let args = [
        OsString::from("classify"),
        OsStr::from_bytes(&bytes).to_owned(),
    ];
    let (status, out, _) = run(&args, Path::new("."), &AtomicBool::new(false));
    assert_eq!(status, 0);
    assert!(out.windows(bytes.len()).any(|s| s == bytes));
}

#[test]
fn parser_error_text_and_number_edges() {
    assert!(
        String::from_utf8(call(&["--", "classify", "--exit", "124", "x"]).1)
            .unwrap()
            .contains("exit-124")
    );
    let err = String::from_utf8(call(&["catalog", "--"]).2).unwrap();
    assert!(err.ends_with("unrecognized arguments: --\n"));
    assert!(err.contains("benchmark,resources,doctor"));
    let err = call(&["classify", "x", "--pretty=yes"]).2;
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("error: argument --pretty: ignored explicit argument 'yes'\n")
    );
    assert!(
        String::from_utf8(call(&["bad"]).2)
            .unwrap()
            .ends_with("invalid choice: 'bad' (choose from classify, catalog)\n")
    );
    for number in ["", "1__2", "_1", "1_", "1.0"] {
        assert_eq!(call(&["classify", "x", "--exit", number]).0, 2);
    }
    for number in ["0", "+000", "1_00", "-1"] {
        assert_eq!(call(&["classify", "x", "--exit", number]).0, 0);
    }
}

#[test]
fn repository_and_integer_boundaries() {
    let dir = std::env::temp_dir().join(format!("oms-failure-test-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let flag = AtomicBool::new(false);
    let args = [OsString::from("catalog")];
    assert_eq!(run(&args, &dir, &flag).0, 0);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir(&dir).unwrap();
    assert_eq!(run(&args, &dir, &flag).0, 2);
    let max = "1".repeat(4300);
    let over = "1".repeat(4301);
    assert_eq!(call(&["classify", "", "--exit", &max]).0, 0);
    assert_eq!(call(&["classify", "", "--exit", &over]).0, 2);
}

#[cfg(unix)]
#[test]
fn invalid_raw_repo_is_reported_losslessly() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let args = [
        OsString::from("--repo"),
        OsStr::from_bytes(b"missing-\xff").to_owned(),
        OsString::from("catalog"),
    ];
    let (status, _, err) = run(&args, Path::new("."), &AtomicBool::new(false));
    assert_eq!(status, 2);
    assert!(err.ends_with(b"missing-\xff\n"));
}

#[cfg(unix)]
#[test]
fn repo_user_expansion() {
    use users::os::unix::UserExt;
    let user = users::get_user_by_uid(users::get_current_uid()).unwrap();
    if user.home_dir().is_dir() {
        let mut repo = OsString::from("~");
        repo.push(user.name());
        let args = [OsString::from("--repo"), repo, OsString::from("catalog")];
        assert_eq!(run(&args, Path::new("."), &AtomicBool::new(false)).0, 0);
    }
    assert_eq!(
        call(&["--repo", "~oms-user-not-present-73691", "catalog"]).0,
        1
    );
}

#[cfg(unix)]
#[test]
fn argument_errors_preserve_raw_bytes_and_repr() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let raw = OsStr::from_bytes(b"--bad-\xff").to_owned();
    let args = [OsString::from("catalog"), raw];
    assert!(
        run(&args, Path::new("."), &AtomicBool::new(false))
            .2
            .ends_with(b"--bad-\xff\n")
    );
    let raw = OsStr::from_bytes(b"\xff").to_owned();
    let args = [
        OsString::from("classify"),
        OsString::from("x"),
        OsString::from("--exit"),
        raw,
    ];
    assert!(
        run(&args, Path::new("."), &AtomicBool::new(false))
            .2
            .ends_with(b"invalid int value: '\\udcff'\n")
    );
    assert!(
        String::from_utf8(call(&["can't"]).2)
            .unwrap()
            .ends_with("invalid choice: \"can't\" (choose from classify, catalog)\n")
    );
}

#[test]
fn namespace_owns_invalid_invocations() {
    assert!(owns(&[]));
    assert!(owns(&[OsString::from("not-an-action")]));
    assert!(owns(&[OsString::from("catalog"), OsString::from("--bad")]));
}

#[test]
fn dash_values_are_not_missing_options() {
    let (status, output, error) = call(&["classify", "x", "--code", "-"]);
    assert_eq!(status, 0);
    assert_eq!(output, b"{\"code\":\"unknown_failure\",\"context_escalation_allowed\":false,\"exit\":null,\"fallback_allowed\":false,\"matched\":\"\",\"recovery\":\"preserve_and_escalate\",\"retryable\":false}\n");
    assert!(error.is_empty());
    let (status, _, error) = call(&["classify", "x", "--exit", "-"]);
    assert_eq!(status, 2);
    assert!(error.ends_with(b"argument --exit: invalid int value: '-'\n"));
    let (status, _, error) = call(&["classify", "x", "--repo", "-"]);
    assert_eq!(status, 2);
    assert_eq!(
        error,
        b"runtime-core: repository path is not a directory: -\n"
    );
}

#[test]
fn dash_prefixed_arguments_with_spaces_are_values() {
    for text in [
        "-fail now",
        "--bad thing",
        "-1 failed",
        "-bash: command not found",
    ] {
        let (status, output, error) = call(&["classify", text]);
        assert_eq!(status, 0, "{text}: {}", String::from_utf8_lossy(&error));
        assert!(!output.is_empty());
        assert!(error.is_empty());
    }
    assert_eq!(call(&["classify", "x", "--code", "-a b"]).0, 0);
    let (status, output, error) = call(&["classify", "x", "--repo", "-a b"]);
    assert_eq!(status, 2);
    assert!(output.is_empty());
    assert_eq!(
        error,
        b"runtime-core: repository path is not a directory: -a b\n"
    );
    let (status, _, error) = call(&["classify", "x", "--exit", "-1 bad"]);
    assert_eq!(status, 2);
    assert!(error.ends_with(b"argument --exit: invalid int value: '-1 bad'\n"));
}

#[test]
fn integer_rejects_c0_separators_and_accepts_unicode_whitespace() {
    for separator in ['\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}'] {
        let raw = format!("{separator}124");
        let (status, _, error) = call(&["classify", "x", "--exit", &raw]);
        assert_eq!(status, 2);
        assert!(String::from_utf8(error).unwrap().ends_with(&format!(
            "argument --exit: invalid int value: '\\x{:02x}124'\n",
            separator as u32
        )));
    }
    assert!(
        String::from_utf8(call(&["classify", "x", "--exit", "\u{a0}١٢٤\u{a0}"]).1)
            .unwrap()
            .contains("exit-124")
    );
}

#[test]
fn short_help_and_unknown_user_error() {
    assert_eq!(call(&["catalog", "-hfoo"]), call(&["catalog", "-h"]));
    let (status, out, err) = call(&["--repo", "~oms-user-not-present-73691", "catalog"]);
    assert_eq!(status, 1);
    assert!(out.is_empty());
    assert_eq!(err, b"RuntimeError: Could not determine home directory.\n");
}

#[test]
fn choice_repr_escapes_c1_controls() {
    for (raw, repr) in [
        ("bad\u{85}choice", "bad\\x85choice"),
        ("bad\u{7f}choice", "bad\\x7fchoice"),
        ("bad\u{9f}choice", "bad\\x9fchoice"),
    ] {
        let (status, _, error) = call(&[raw]);
        assert_eq!(status, 2);
        assert!(String::from_utf8(error).unwrap().ends_with(&format!(
            "invalid choice: '{repr}' (choose from classify, catalog)\n"
        )));
    }
}

#[test]
fn gpu_and_collective_classifications_are_exposed() {
    for (input, code, recovery) in [
        (
            "memory fragmentation",
            "cuda_oom_fragmentation",
            "reduce_fragmentation",
        ),
        (
            "cublasStatusAllocFailed",
            "cuda_oom_peak",
            "reduce_batch_or_offload",
        ),
        (
            "ProcessGroupNCCL Watchdog",
            "nccl_watchdog_timeout",
            "restart_collective_with_debug",
        ),
    ] {
        let (status, output, error) = call(&["classify", input]);
        assert_eq!(status, 0, "{}", String::from_utf8_lossy(&error));
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(code));
        assert!(output.contains(recovery));
    }
}
