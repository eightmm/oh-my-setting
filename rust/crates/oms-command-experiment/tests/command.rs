use oms_command_experiment::run;
use std::{ffi::OsString, fs, path::PathBuf, sync::atomic::AtomicBool};
const BASE: &str = r#"{"schema":2,"question":"q","hypothesis":"h","prediction":"p","baseline":{"x":1},"treatment":{"independent_change":"x"},"controlled_variables":{},"metrics":{"primary":{"name":"score","direction":"maximize"}},"seeds":[0],"success":{"min_improvement":0}}"#;
fn call(args: &[&str], root: &std::path::Path) -> (i32, Vec<u8>, Vec<u8>) {
    run(
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        root,
        &AtomicBool::new(false),
    )
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "oms-experiment-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn commands_and_real_files() {
    if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1") {
        let t = Temp::new();
        assert_eq!(call(&["template", "--output", "blocked.json"], &t.0).0, 2);
        assert!(!t.0.join("blocked.json").exists());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "commands_and_real_files"])
            .env_remove("OMS_HARNESS_CHILD")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let t = Temp::new();
    let (rc, out, _) = call(&["template", "--output", "nested/template.json"], &t.0);
    assert_eq!(rc, 0);
    assert!(out.ends_with(b"\n"));
    assert!(
        fs::read(t.0.join("nested/template.json"))
            .unwrap()
            .starts_with(b"{\n")
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("nested/template.json", t.0.join("link.json")).unwrap();
        assert_eq!(call(&["template", "--output", "link.json"], &t.0).0, 2);
        std::os::unix::fs::symlink("nested", t.0.join(".oms")).unwrap();
        assert_eq!(
            call(&["template", "--output", ".oms/sub/test.json"], &t.0).0,
            2
        );
        assert!(!t.0.join("nested/sub/test.json").exists());
    }
    let (status, _, error) = call(&["template", "--output", "-"], &t.0);
    assert_eq!(status, 0, "{}", String::from_utf8_lossy(&error));
    assert!(t.0.join("-").is_file());
    fs::remove_file(t.0.join("-")).unwrap();
    fs::create_dir(t.0.join("-")).unwrap();
    assert_eq!(call(&["--repo", "-", "template"], &t.0).0, 0);
    assert_eq!(call(&["template", "--repo", "-"], &t.0).0, 0);
    fs::write(t.0.join("contract.json"), BASE).unwrap();
    let (rc, out, _) = call(&["validate", "contract.json"], &t.0);
    assert_eq!(rc, 0);
    assert!(String::from_utf8(out).unwrap().contains("contract_digest"));
    let (rc, out, _) = call(&["compare", "contract.json", BASE], &t.0);
    assert_eq!(rc, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("\"comparable\":true")
    );
    assert_eq!(call(&["register", BASE], &t.0), (75, vec![], vec![]));
    assert_eq!(call(&["validate", "{}"], &t.0).0, 2);
    assert_eq!(call(&["compare", BASE], &t.0).0, 2);
    assert_eq!(call(&["template", "--help"], &t.0).0, 0);
}
#[test]
fn semantic_equality() {
    let t = Temp::new();
    let left = BASE.replace(
        "\"controlled_variables\":{}",
        "\"controlled_variables\":{\"x\":[1,{\"a\":true,\"b\":0.001}],\"n\":9007199254740993}",
    );
    let right = BASE.replace(
        "\"controlled_variables\":{}",
        "\"controlled_variables\":{\"x\":[1.0,{\"b\":1e-3,\"a\":1}],\"n\":9007199254740992.0}",
    );
    let (rc, out, _) = call(&["compare", &left, &right], &t.0);
    assert_eq!(rc, 0);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("controlled_variables.n"));
    assert!(!out.contains("controlled_variables.x"));
}
#[test]
fn tolerance_and_missing_null() {
    let t = Temp::new();
    let left = BASE.replace("\"controlled_variables\":{}", "\"controlled_variables\":{\"parameter_count\":1000,\"parameter_count_tolerance\":\" ١_٠e-3 \"}");
    let right = BASE.replace("\"controlled_variables\":{}", "\"controlled_variables\":{\"parameter_count\":1010,\"parameter_count_tolerance\":\" ١_٠e-3 \",\"missing\":null}");
    let (rc, out, _) = call(&["compare", &left, &right], &t.0);
    assert_eq!(rc, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("\"comparable\":true")
    );
    let bad = left.replace(" ١_٠e-3 ", "1__0");
    assert_eq!(call(&["compare", &bad, &right], &t.0).0, 1);
}

#[test]
fn ownership_and_shared_options() {
    use oms_command_experiment::owns;
    let t = Temp::new();
    let words = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(owns(&words(&["--repo", ".", "--pretty", "validate", "{}"])));
    assert!(!owns(&words(&["--repo", "template"])));
    assert!(!owns(&words(&[
        "--repo", "invalid", "register", "template"
    ])));
    assert!(!owns(&words(&["show", "template"])));
    assert_eq!(call(&["--repo=.", "--pre", "validate", BASE], &t.0).0, 0);
    assert_eq!(call(&["validate", BASE, "--repo=."], &t.0).0, 0);
    let duplicate = BASE.replace("\"schema\":2", "\"schema\":1,\"schema\":2");
    assert_eq!(call(&["validate", &duplicate], &t.0).0, 0);
}

#[test]
fn nested_repo_keeps_input_cwd() {
    if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1") {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "nested_repo_keeps_input_cwd"])
            .env_remove("OMS_HARNESS_CHILD")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let t = Temp::new();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&t.0)
            .status()
            .unwrap()
            .success()
    );
    let nested = t.0.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("contract.json"), BASE).unwrap();
    assert_eq!(call(&["validate", "contract.json"], &nested).0, 0);
    assert_eq!(
        call(&["template", "--output=experiment.json"], &nested).0,
        0
    );
    assert!(t.0.join("experiment.json").exists());
    assert!(!nested.join("experiment.json").exists());
}

#[test]
fn input_size_and_dialect() {
    let t = Temp::new();
    let prefix = BASE.strip_suffix('}').unwrap();
    let padded = format!("{prefix},\"extra\":\"{}\"}}", "x".repeat(4 * 1024 * 1024));
    fs::write(t.0.join("large.json"), padded).unwrap();
    assert_eq!(call(&["validate", "large.json"], &t.0).0, 0);
    fs::write(t.0.join("huge.json"), vec![b'x'; 8 * 1024 * 1024 + 1]).unwrap();
    assert_eq!(call(&["validate", "huge.json"], &t.0).0, 2);
    fs::write(t.0.join("bom.json"), b"\xef\xbb\xbf{}").unwrap();
    assert_eq!(call(&["validate", "bom.json"], &t.0).0, 2);
}

#[test]
fn parser_diagnostics() {
    let t = Temp::new();
    let (_, _, err) = call(&["compare", "{}"], &t.0);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "usage: oms runtime experiment compare [-h] [--repo REPO] [--pretty] left right\noms runtime experiment compare: error: the following arguments are required: right\n"
    );
    let (_, _, err) = call(&["template", "--pretty=x"], &t.0);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("argument --pretty: ignored explicit argument 'x'\n")
    );
    let (_, _, err) = call(&["template", "--unknown", "x"], &t.0);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("oms runtime: error: unrecognized arguments: --unknown x\n")
    );
    let (_, _, err) = call(&["validate", "--unknown"], &t.0);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("the following arguments are required: contract\n")
    );
}

#[test]
fn interrupted_before_output() {
    let t = Temp::new();
    fs::write(t.0.join("existing.json"), b"original").unwrap();
    let args = ["template", "--output", "existing.json"].map(OsString::from);
    let result = run(&args, &t.0, &AtomicBool::new(true));
    assert_eq!(
        result,
        (130, vec![], b"runtime-core: interrupted\n".to_vec())
    );
    assert_eq!(fs::read(t.0.join("existing.json")).unwrap(), b"original");
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 1);
}

#[test]
fn coercion_error_format() {
    let t = Temp::new();
    let left = BASE.replace(
        "\"controlled_variables\":{}",
        "\"controlled_variables\":{\"parameter_count_tolerance\":\"\\u200b\"}",
    );
    let (code, _, err) = call(&["compare", &left, BASE], &t.0);
    assert_eq!(code, 1);
    assert_eq!(
        err,
        b"ValueError: could not convert string to float: '\\u200b'\n"
    );
    let (_, _, err) = call(&["validate", "{}", "extra", "--bad", "last"], &t.0);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .ends_with("unrecognized arguments: extra --bad last\n")
    );
}

#[test]
fn unowned_selection_is_always_inert() {
    use oms_command_experiment::owns;
    let missing = std::env::temp_dir().join("oms-missing-experiment-data-directory");
    for words in [
        vec![],
        vec!["bogus"],
        vec!["register", "{}"],
        vec!["--repo"],
        vec!["--repo", "template"],
    ] {
        let args = words.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(!owns(&args), "{words:?}");
        for interrupted in [false, true] {
            assert_eq!(
                run(&args, &missing, &AtomicBool::new(interrupted)),
                (75, vec![], vec![]),
                "{words:?}"
            );
        }
    }
}

#[test]
fn file_and_inline_error_origins() {
    let t = Temp::new();
    fs::write(t.0.join("broken.json"), b"{").unwrap();
    let (code, _, stderr) = call(&["validate", "broken.json"], &t.0);
    assert_eq!(code, 2);
    assert!(stderr.starts_with(b"runtime-core: invalid JSON in broken.json: "));
    assert!(
        call(&["validate", "{"], &t.0)
            .2
            .starts_with(b"runtime-core: expected JSON or a JSON file: ")
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let raw_name = OsString::from_vec(b"broken-\xff.json".to_vec());
        fs::write(t.0.join(&raw_name), b"{").unwrap();
        let args = [OsString::from("validate"), raw_name.clone()];
        let (code, _, stderr) = run(&args, &t.0, &AtomicBool::new(false));
        assert_eq!(code, 2);
        let mut prefix = b"runtime-core: invalid JSON in ".to_vec();
        prefix.extend_from_slice(b"broken-\\udcff.json");
        prefix.extend_from_slice(b": ");
        assert!(stderr.starts_with(&prefix));
        let link = OsString::from_vec(b"linked-\xff.json".to_vec());
        std::os::unix::fs::symlink("broken.json", t.0.join(&link)).unwrap();
        let args = [OsString::from("validate"), link];
        let (code, _, stderr) = run(&args, &t.0, &AtomicBool::new(false));
        assert_eq!(code, 2);
        assert!(stderr.starts_with(b"runtime-core: expected JSON or a JSON file: "));
    }
}

#[test]
fn large_controlled_mapping_uses_indexed_comparison() {
    use std::fmt::Write;
    const KEYS: usize = 24_000;
    let t = Temp::new();
    let value = "x".repeat(320);
    let mut left = String::from("{");
    let mut right = String::from("{");
    for i in 0..KEYS {
        if i != 0 {
            left.push(',');
            right.push(',');
        }
        write!(left, "\"k{i:05}\":\"{value}\"").unwrap();
        let j = KEYS - i - 1;
        write!(
            right,
            "\"k{j:05}\":\"{}\"",
            if j == KEYS - 1 { "changed" } else { &value }
        )
        .unwrap();
    }
    left.push_str(",\"absent\":null}");
    right.push('}');
    let left = BASE.replace(
        "\"controlled_variables\":{}",
        &format!("\"controlled_variables\":{left}"),
    );
    let right = BASE.replace(
        "\"controlled_variables\":{}",
        &format!("\"controlled_variables\":{right}"),
    );
    for text in [&left, &right] {
        assert!((7 * 1024 * 1024..=8 * 1024 * 1024).contains(&text.len()));
    }
    fs::write(t.0.join("left.json"), left).unwrap();
    fs::write(t.0.join("right.json"), right).unwrap();
    let (code, stdout, stderr) = call(&["compare", "left.json", "right.json"], &t.0);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("\"field\":\"controlled_variables.k23999\""));
    assert_eq!(output.matches("\"field\":").count(), 1);
    assert!(output.contains(&format!("\"left\":\"{value}\"")));
    assert!(output.contains("\"right\":\"changed\""));
}

#[test]
fn unknown_repo_account_is_a_runtime_error() {
    let t = Temp::new();
    let (code, stdout, stderr) = call(
        &[
            "template",
            "--repo",
            "~__oms_nonexistent_account_for_fixture__",
        ],
        &t.0,
    );
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        b"RuntimeError: Could not determine home directory.\n"
    );
}

#[cfg(unix)]
#[test]
fn raw_repo_arguments_remain_lossless() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let t = Temp::new();
    let name = OsString::from_vec(b"repo-\xff".to_vec());
    fs::create_dir(t.0.join(&name)).unwrap();
    let mut option = b"--repo=".to_vec();
    option.extend_from_slice(name.as_bytes());
    let args = [OsString::from("template"), OsString::from_vec(option)];
    assert_eq!(run(&args, &t.0, &AtomicBool::new(false)).0, 0);
    let missing = OsString::from_vec(b"missing-\xfe".to_vec());
    let args = [
        OsString::from("template"),
        OsString::from("--repo"),
        missing.clone(),
    ];
    let (code, _, stderr) = run(&args, &t.0, &AtomicBool::new(false));
    assert_eq!(code, 2);
    let mut expected = b"runtime-core: repository path is not a directory: ".to_vec();
    expected.extend_from_slice(b"missing-\\udcfe");
    expected.push(b'\n');
    assert_eq!(stderr, expected);
}
