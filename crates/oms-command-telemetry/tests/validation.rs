mod support;
use std::ffi::OsString;
use std::fs;
use std::sync::atomic::AtomicBool;

#[test]
fn rejects_invalid_options_and_corrupt_index() {
    let fixture = support::Temp::new();
    let root = fixture.0.clone();
    fs::create_dir_all(root.join(".oms/artifacts")).unwrap();
    let interrupted = AtomicBool::new(false);
    let call = |args: &[&str]| {
        let argv: Vec<OsString> = std::iter::once(OsString::from("telemetry"))
            .chain(args.iter().map(OsString::from))
            .collect();
        oms_command_telemetry::run(&argv, &root, &interrupted)
    };
    assert_eq!(call(&[]).0, 0);
    assert_eq!(call(&["--top", "0"]).0, 2);
    assert_eq!(call(&["--top", "101"]).0, 2);
    assert_eq!(call(&["--by", "day", "--waste"]).0, 2);
    fs::write(
        root.join(".oms/artifacts/index.jsonl"),
        b"{\"kind\":\"call\"}",
    )
    .unwrap();
    let result = call(&[]);
    assert_eq!(result.0, 2);
    assert!(
        String::from_utf8(result.2)
            .unwrap()
            .contains("partial final")
    );
}

#[test]
fn errors_match_canonical_public_preflight_and_options() {
    use support::{Temp, array, field, text};
    // Start with a successful report so reverting the implementation also
    // proves this suite cannot pass through the recovered classifier alone.
    let root = Temp::new();
    assert_eq!(root.call(&["--json"]).0, 0);
    let fixtures = oms_context::parse_json(include_bytes!("errors.json")).unwrap();
    for case in array(&fixtures) {
        let raw: Vec<u8> = array(field(case, "index"))
            .iter()
            .map(|v| {
                if let oms_context::Json::Number(n) = v {
                    n.parse().unwrap()
                } else {
                    panic!("expected byte")
                }
            })
            .collect();
        root.index(&raw);
        let args: Vec<&str> = array(field(case, "args")).iter().map(text).collect();
        let (exit, stdout, stderr) = root.call(&args);
        let oms_context::Json::Number(expected) = field(case, "exit") else {
            panic!("expected exit")
        };
        assert_eq!(exit, expected.parse::<i32>().unwrap(), "{args:?} {raw:?}");
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            text(field(case, "stderr")),
            "{args:?} {raw:?}"
        );
        assert_eq!(
            std::fs::read(root.0.join(".oms/artifacts/index.jsonl")).unwrap(),
            raw
        );
    }
}

#[test]
fn row_and_index_limits_and_read_boundaries() {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::symlink;
    let root = support::Temp::new();
    assert_eq!(root.call(&[]).0, 0);
    let index = root.0.join(".oms/artifacts/index.jsonl");
    for size in [1024 * 1024 - 1, 1024 * 1024, 1024 * 1024 + 1] {
        let mut row = b"{}".to_vec();
        row.resize(size - 1, b' ');
        row.push(b'\n');
        root.index(&row);
        let result = root.call(&["--json"]);
        assert_eq!(result.0, if size <= 1024 * 1024 { 0 } else { 2 });
    }
    // Sparse fixtures exercise the actual 256 MiB boundary without writing a
    // large fixture. At the bound, the read succeeds and row validation fails;
    // one byte over must fail the read before any JSON parsing.
    for size in [256 * 1024 * 1024u64, 256 * 1024 * 1024 + 1] {
        let mut file = std::fs::File::create(&index).unwrap();
        file.set_len(size).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);
        let result = root.call(&[]);
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
        let error = String::from_utf8(result.2).unwrap();
        assert!(
            error.contains(if size == 256 * 1024 * 1024 {
                "row limit"
            } else {
                "recovery limit"
            }),
            "{error}"
        );
    }
    std::fs::remove_file(&index).unwrap();
    symlink("missing.jsonl", &index).unwrap();
    assert_eq!(root.call(&[]).0, 2);
    std::fs::remove_file(&index).unwrap();
    root.index(b"{}\n");
    std::fs::hard_link(&index, root.0.join("linked")).unwrap();
    assert_eq!(root.call(&[]).0, 2);
    std::fs::remove_file(root.0.join("linked")).unwrap();
    std::fs::remove_file(index).unwrap();
    std::fs::remove_dir(root.0.join(".oms/artifacts")).unwrap();
    symlink(&root.0, root.0.join(".oms/artifacts")).unwrap();
    assert_eq!(root.call(&[]).0, 2);
}

#[test]
fn date_and_numeric_overflow_fail_without_partial_reports() {
    let root = support::Temp::new();
    for digits in [4300, 4301] {
        root.write(
            ".oms/artifacts/count.md",
            format!("## Output\ntokens used\n1{}\n", "0".repeat(digits - 1)),
        );
        root.index(b"{\"kind\":\"call\",\"artifact\":\".oms/artifacts/count.md\"}\n");
        for args in [vec![], vec!["--json"]] {
            assert_eq!(root.call(&args).0, if digits == 4300 { 0 } else { 2 });
        }
    }
    let large = "9".repeat(4300);
    root.index(format!(
        "{{\"kind\":\"call\",\"tokens\":{large}}}\n{{\"kind\":\"call\",\"tokens\":{large}}}\n"
    ));
    for args in [vec![], vec!["--json"], vec!["--by", "provider"]] {
        let result = root.call(&args);
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
    }
    root.index(b"");

    assert_eq!(root.call(&["--since", "0001-01-01"]).0, 0);
    for args in [
        vec!["--until", "9999-12-31"],
        vec!["--since", "0001-01-01T00:00:00+01:00"],
        vec!["--until", "9999-12-31T23:59:59-01:00"],
    ] {
        let result = root.call(&args);
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
    }
    root.index(b"{\"kind\":\"call\",\"ts\":\"0001-01-01T00:00:00+01:00\"}\n");
    assert_eq!(root.call(&[]).0, 0);
    assert_eq!(root.call(&["--by", "day"]).0, 2);
    for field in ["duration_s", "cost_usd"] {
        root.index(format!(
            "{{\"kind\":\"call\",\"{field}\":1{}}}\n",
            "0".repeat(400)
        ));
        let result = root.call(&["--json"]);
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
    }
    root.index(format!("{{\"kind\":\"delegate\",\"tokens\":1{},\"patch_sha256\":\"{}\"}}\n{{\"kind\":\"patch-admit\",\"exit\":0,\"patch_sha256\":\"{}\"}}\n","0".repeat(400),"a".repeat(64),"a".repeat(64)));
    assert_eq!(root.call(&["--json"]).0, 0);
    let result = root.call(&["--by", "role", "--json"]);
    assert_eq!(result.0, 2);
    assert!(result.1.is_empty());
}
