use oms_command_job_digest::run;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "oms-digest-edge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn call(&self, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        run(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &self.0,
            &AtomicBool::new(false),
        )
    }
    fn metrics(&self, records: &str) -> String {
        fs::write(self.0.join("metrics"), records).unwrap();
        let (code, out, err) = self.call(&["--metrics", "metrics"]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        String::from_utf8(out).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn float_steps_keep_python_integer_range() {
    let f = Fixture::new();
    let out = f.metrics(concat!(
        "{\"step\":1e30,\"name\":\"large\",\"value\":1}\n",
        "{\"step\":-1e30,\"name\":\"negative\",\"value\":1}\n",
        "{\"step\":-0.5,\"name\":\"zero\",\"value\":1}\n",
        "{\"step\":9007199254740993,\"name\":\"rounded\",\"value\":1}\n",
        "{\"step\":1e308,\"name\":\"same\",\"value\":1}\n",
        "{\"step\":1e308,\"name\":\"same\",\"value\":2}\n",
        "{\"step\":1e308,\"name\":\"same\",\"value\":3}\n"
    ));
    assert!(
        out.contains("| large | 1 | 1000000000000000019884624838656 |"),
        "{out}"
    );
    assert!(out.contains("| negative | 1 | -1000000000000000019884624838656 |"));
    assert!(out.contains("| zero | 1 | 0 |"));
    assert!(out.contains("| rounded | 1 | 9007199254740992 |"));
    assert!(
        out.lines()
            .find(|line| line.starts_with("| same |"))
            .unwrap()
            .ends_with("| n/a | 0 |")
    );
}

#[test]
fn nonfinite_steps_abort_metrics_gathering() {
    let f = Fixture::new();
    for value in ["NaN", "Infinity", "-Infinity", "\"nan\"", "1e999"] {
        let out = f.metrics(&format!(
            "{{\"step\":{value},\"name\":\"loss\",\"value\":1}}\n"
        ));
        assert!(
            out.contains("Metrics section failed; read the file directly."),
            "{value}: {out}"
        );
        assert!(!out.contains("## Metrics"));
        fs::write(f.0.join("metric.tsv"), format!("step\tvalue\n{value}\t1\n")).unwrap();
        if !value.starts_with('"') {
            let (_, out, _) = f.call(&["--metrics", "metric.tsv"]);
            assert!(
                String::from_utf8(out)
                    .unwrap()
                    .contains("Metrics section failed")
            );
        }
    }
    let out = f.metrics("{\"step\":\"bad\",\"name\":\"loss\",\"value\":1}\n");
    assert!(out.contains("1 record(s) skipped"));
}

#[test]
fn python_iso_forms_and_rejected_dates() {
    let f = Fixture::new();
    for ts in [
        "20000101T01",
        "2000-W01-1T01:02:03",
        "2000W011T010203",
        "2000-W01",
        "2000W01",
        "2000-01-01X01:02:03,123456789",
        "2000-01-01🐈01:02+01",
        "2000-01-01T01:02:03+010203.5",
        "20000101T0102-01:02:03,1",
        "2000-01-01T01.5",
        "2000-01-01T01:02Z",
        "2000-01-01T01:02+00:60",
        "2000-01-01T01:02+00:99:99",
        "2000-W01101",
        "2000W01101",
    ] {
        let out = f.metrics(&format!(
            "{{\"step\":1,\"name\":\"loss\",\"value\":1,\"ts\":\"{ts}\"}}\n"
        ));
        assert!(!out.contains("no timestamp in the file"), "{ts}: {out}");
    }
    for ts in [
        "",
        "2000-01",
        "2000-366",
        "2000-01-01T24:00",
        "2000-W54-1",
        "2000-01-01T00:00+24:00",
        "2000-01-01T01:02z",
        "2000-01-01Taéx",
    ] {
        let out = f.metrics(&format!(
            "{{\"step\":1,\"name\":\"loss\",\"value\":1,\"ts\":\"{ts}\"}}\n"
        ));
        assert!(out.contains("no timestamp in the file"), "{ts}: {out}");
    }
}

#[test]
fn numeric_formatting_and_surrogate_names() {
    let f = Fixture::new();
    let out = f.metrics(concat!(
        "{\"step\":1,\"name\":\"tie\",\"value\":1.234565}\n",
        "{\"step\":1,\"name\":\"zero\",\"value\":-0.0}\n",
        "{\"step\":\"1_000\",\"name\":\"underscore\",\"value\":\"1_2.5\"}\n",
        "{\"step\":\"١٢\",\"name\":\"unicode\",\"value\":\"٣.٥\"}\n"
    ));
    assert!(out.contains("| tie | 1 | 1 | 1.23456 |"), "{out}");
    assert!(out.contains("| zero | 1 | 1 | -0 |"));
    assert!(out.contains("| underscore | 1 | 1000 | 12.5 |"));
    assert!(out.contains("| unicode | 1 | 12 | 3.5 |"));
    fs::write(
        f.0.join("metrics"),
        "{\"step\":1,\"name\":\"raw\\udcff\",\"value\":1}\n",
    )
    .unwrap();
    let (_, out, _) = f.call(&["--metrics", "metrics"]);
    assert!(out.windows(6).any(|w| w == b"| raw\xff"));
    let out = f.metrics("{\"step\":1,\"name\":\"bad\\ud800\",\"value\":1}\n");
    assert!(out.contains("## Metrics"));
    assert!(out.contains("Metrics section failed"));
    assert!(!out.contains("record(s) skipped as malformed)\n\nNo metric"));
}

#[test]
fn huge_line_cap_and_unterminated_tail() {
    let f = Fixture::new();
    let text = format!("ERROR short\n{}\n마지막 줄 ok\n", "가나다".repeat(400_000));
    fs::write(f.0.join("log"), &text).unwrap();
    let (code, out, _) = f.call(&["--max-bytes", "2048", "log"]);
    assert_eq!(code, 0);
    assert!(out.len() <= 2048);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("ERROR short"));
    assert!(out.contains("마지막 줄 ok"));
    assert!(out.contains("omitted by --max-bytes 2048"));
    assert!(f.call(&["log"]).1.len() > 1_000_000);
    fs::write(f.0.join("log"), "first\nlast").unwrap();
    let (_, out, _) = f.call(&["--tail", "1", "log"]);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .ends_with("## Tail\n\n```\nlast```\n")
    );
    for option in ["--tail", "--patterns"] {
        for invalid in ["", "bad", "-1"] {
            assert_eq!(f.call(&[option, invalid, "log"]).0, 2);
        }
        assert_eq!(f.call(&[option, "0", "log"]).0, 0);
        assert_eq!(f.call(&[option, "1", "log"]).0, 0);
    }
    fs::write(f.0.join("log"), "").unwrap();
    assert_eq!(f.call(&["log"]).0, 0);
}

#[test]
fn slope_overflow_and_signed_nonfinite_follow_python() {
    let f = Fixture::new();
    let out = f.metrics(concat!(
        "{\"step\":0,\"name\":\"loss\",\"value\":1e308}\n",
        "{\"step\":1,\"name\":\"loss\",\"value\":1e308}\n",
        "{\"step\":2,\"name\":\"loss\",\"value\":1e308}\n"
    ));
    assert!(out.contains("| +nan | 0 |"), "{out}");
    let out = f.metrics(concat!(
        "{\"step\":0,\"name\":\"loss\",\"value\":1}\n",
        "{\"step\":1e200,\"name\":\"loss\",\"value\":2}\n",
        "{\"step\":2e200,\"name\":\"loss\",\"value\":3}\n"
    ));
    assert!(out.contains("## Metrics"));
    assert!(out.contains("Metrics section failed"));
    assert!(!out.contains("| loss |"));
}

#[test]
fn tsv_discovery_preserves_names_and_does_not_follow_directory_links() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let dir = f.0.join("dvc");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join(".tsv"), "step\tvalue\n1\t2\n").unwrap();
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStringExt;
        let filename = OsString::from_vec(b"loss-\xff.tsv".to_vec());
        fs::write(dir.join(filename), "step\tvalue\r1\t3\r2\t2\r").unwrap();
        let (_, out, _) = f.call(&["--metrics", "dvc"]);
        assert!(
            out.windows(b"| .tsv | 1 | 1 | 2 |".len())
                .any(|w| w == b"| .tsv | 1 | 1 | 2 |")
        );
        assert!(
            out.windows(b"| loss-\xff | 2 | 2 | 2 |".len())
                .any(|w| w == b"| loss-\xff | 2 | 2 | 2 |")
        );
    }
    #[cfg(target_os = "macos")]
    {
        let (_, out, _) = f.call(&["--metrics", "dvc"]);
        assert!(
            out.windows(b"| .tsv | 1 | 1 | 2 |".len())
                .any(|w| w == b"| .tsv | 1 | 1 | 2 |")
        );
    }
    let other = f.0.join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("ignored.tsv"), "step\tvalue\n1\t1\n").unwrap();
    symlink(&other, dir.join("linked")).unwrap();
    let (_, out, _) = f.call(&["--metrics", "dvc"]);
    assert!(!String::from_utf8_lossy(&out).contains("| ignored |"));
}

#[test]
fn table_limits_recent_window_and_malformed_records() {
    let f = Fixture::new();
    for count in [0, 20, 21] {
        let records: String = (0..count)
            .map(|i| format!("{{\"step\":0,\"name\":\"metric{i:02}\",\"value\":1}}\n"))
            .collect();
        let out = f.metrics(&records);
        assert_eq!(
            out.matches("| metric").count(),
            count.min(20) + usize::from(count != 0)
        );
        assert_eq!(out.contains("1 more metric(s)"), count == 21);
        assert_eq!(out.contains("No metric records found"), count == 0);
    }
    let mut records = "null\n[]\ntrue\n{\"step\":true,\"name\":\"loss\",\"value\":1}\n".to_owned();
    for step in 0..11 {
        records.push_str(&format!(
            "{{\"step\":{step},\"name\":\"loss\",\"value\":{}}}\n",
            if step == 0 { 1000 } else { step * 2 }
        ));
    }
    let out = f.metrics(&records);
    assert!(out.contains("4 record(s) skipped"));
    assert!(
        out.contains("| loss | 11 | 10 | 20 | 2 @1 (min) | +2 | 0 |"),
        "{out}"
    );
    fs::create_dir(f.0.join("empty")).unwrap();
    let (_, out, _) = f.call(&["--metrics", "empty"]);
    assert!(String::from_utf8(out).unwrap().contains("0 file(s)"));
    fs::write(f.0.join("bad.tsv"), "value\n1\n").unwrap();
    let (_, out, _) = f.call(&["--metrics", "bad.tsv"]);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("1 record(s) skipped")
    );
}

#[test]
fn numeric_rounding_matches_frozen_source_outputs() {
    let f = Fixture::new();
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("support/numbers.json")).unwrap();
    for (index, case) in cases.as_array().unwrap().iter().enumerate() {
        let out = f.metrics(case["records"].as_str().unwrap());
        assert!(
            out.ends_with(case["expected"].as_str().unwrap()),
            "reference batch {index}: {out}"
        );
    }
}

#[test]
fn json_surrogates_and_integer_conversion_errors() {
    let f = Fixture::new();
    let out = f.metrics("{\"ignored\\ud800\":0,\"step\":1,\"name\":\"loss\",\"value\":1}\n");
    assert!(out.contains("| loss | 1 | 1 | 1 |"), "{out}");
    let out = f.metrics(&format!(
        "{{\"step\":1,\"name\":\"loss\",\"value\":{}}}\n",
        "9".repeat(400)
    ));
    assert!(out.contains("Metrics section failed"));
    let out = f.metrics(&format!(
        "{{\"ignored\":{},\"step\":1,\"name\":\"loss\",\"value\":1}}\n",
        "9".repeat(4301)
    ));
    assert!(out.contains("1 record(s) skipped"));
    assert!(!out.contains("| loss |"));
    let out = f.metrics("{\"step\":1,\"name\":\"face\\ud83d\\udc08\",\"value\":1}\n");
    assert!(out.contains("| face🐈 |"));
}

#[test]
fn python_whitespace_and_dotfile_metric_stems() {
    let f = Fixture::new();
    let out =
        f.metrics("\u{1c}{\"step\":\"\\u001c1\\u001f\",\"name\":\"loss\",\"value\":1}\u{1f}\n");
    assert!(out.contains("| loss | 1 | 1 | 1 |"), "{out}");
    fs::write(f.0.join("..tsv"), "step\tvalue\n0\t1\n").unwrap();
    let (_, out, _) = f.call(&["--metrics", "..tsv"]);
    assert!(String::from_utf8(out).unwrap().contains("| ..tsv |"));
}

#[test]
fn fatal_metrics_report_exception_causes() {
    let f = Fixture::new();
    for (record, class, message) in [
        (
            "{\"step\":NaN,\"name\":\"loss\",\"value\":1}\n",
            "ValueError",
            "NaN",
        ),
        (
            "{\"step\":Infinity,\"name\":\"loss\",\"value\":1}\n",
            "OverflowError",
            "infinity",
        ),
        (
            "{\"step\":1,\"name\":\"bad\\ud800\",\"value\":1}\n",
            "UnicodeEncodeError",
            "surrogate",
        ),
    ] {
        fs::write(f.0.join("metrics"), record).unwrap();
        let (code, out, err) = f.call(&["--metrics", "metrics"]);
        assert_eq!(code, 0);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Metrics section failed; read the file directly.")
        );
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains(class) && err.contains(message), "{err}");
        assert!(!err.contains("Traceback"));
    }
    let records = "{\"step\":0,\"name\":\"loss\",\"value\":1}\n{\"step\":1e200,\"name\":\"loss\",\"value\":2}\n{\"step\":2e200,\"name\":\"loss\",\"value\":3}\n";
    fs::write(f.0.join("metrics"), records).unwrap();
    let (_, out, err) = f.call(&["--metrics", "metrics"]);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("Metrics section failed")
    );
    assert!(String::from_utf8(err).unwrap().contains("OverflowError"));
    fs::write(
        f.0.join("metrics"),
        format!(
            "{{\"step\":1,\"name\":\"loss\",\"value\":{}}}\n",
            "9".repeat(400)
        ),
    )
    .unwrap();
    let (_, _, err) = f.call(&["--metrics", "metrics"]);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("int too large to convert to float")
    );
    fs::write(f.0.join("metrics"), "malformed\n").unwrap();
    let (code, out, err) = f.call(&["--metrics", "metrics"]);
    assert_eq!(code, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("1 record(s) skipped")
    );
    assert!(err.is_empty());
}

#[test]
fn unreadable_inputs_preserve_paths_and_failure_boundaries() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let (name, needle): (OsString, &[u8]) = if cfg!(target_os = "macos") {
        (OsString::from("private-space.log"), b"private-space.log")
    } else {
        (
            OsString::from_vec(b"private-\xff.log".to_vec()),
            b"private-\xff.log",
        )
    };
    let file = f.0.join(&name);
    fs::write(&file, b"last\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
    // A privileged test process can still read mode 000; the disappearance
    // observer fixture exercises the post-classification failure on every host.
    if fs::File::open(&file).is_err() {
        let args = [
            OsString::from("--max-bytes"),
            OsString::from("512"),
            name.clone(),
        ];
        let (code, out, err) = run(&args, &f.0, &AtomicBool::new(false));
        assert_eq!(code, 1);
        assert!(out.len() <= 512);
        assert!(out.starts_with(b"# Job digest"));
        assert!(err.windows(needle.len()).any(|w| w == needle));
        assert!(String::from_utf8_lossy(&err).contains("Permission denied"));
        let args = [OsString::from("--metrics"), name];
        let (code, out, err) = run(&args, &f.0, &AtomicBool::new(false));
        assert_eq!(code, 0);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Metrics section failed")
        );
        assert!(String::from_utf8_lossy(&err).contains("PermissionError"));
        assert!(err.windows(needle.len()).any(|w| w == needle));
    }
    fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn large_inputs_fit_a_bounded_native_child() {
    use std::io::{Seek, SeekFrom, Write};
    if let Some(root) = std::env::var_os("OMS_DIGEST_MEMORY_ROOT") {
        let root = PathBuf::from(root);
        let mode = std::env::var("OMS_DIGEST_MEMORY_MODE").unwrap();
        let args: &[&str] = if mode == "metrics" {
            &["--metrics", "many"]
        } else if mode == "huge" {
            &["--max-bytes", "512", "huge"]
        } else {
            &["--max-bytes", "512", "--tail", "1", "many"]
        };
        let (code, out, err) = run(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &root,
            &AtomicBool::new(false),
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(err.is_empty());
        let out = String::from_utf8(out).unwrap();
        if mode == "metrics" {
            assert!(out.contains("| loss | 20480 | 1 | 1 |"));
        } else {
            assert!(out.len() <= 512);
            assert!(out.contains("omitted by --max-bytes 512"));
        }
        return;
    }
    let Some(prlimit) = native_tool("prlimit") else {
        eprintln!("bounded memory fixture skipped: prlimit unavailable");
        return;
    };
    let f = Fixture::new();
    let mut many = fs::File::create(f.0.join("many")).unwrap();
    let prefix = "{\"step\":1,\"name\":\"loss\",\"value\":1,\"padding\":\"";
    let record = format!("{prefix}{}\"}}\n", "a".repeat(4096 - prefix.len() - 3));
    assert_eq!(record.len(), 4096);
    let batch = record.repeat(16);
    for _ in 0..1280 {
        many.write_all(batch.as_bytes()).unwrap();
    }
    drop(many);
    let mut huge = fs::File::create(f.0.join("huge")).unwrap();
    huge.write_all(b"ERROR ").unwrap();
    huge.set_len(80 * 1024 * 1024).unwrap();
    huge.seek(SeekFrom::End(0)).unwrap();
    huge.write_all(b"\nlast\n").unwrap();
    drop(huge);
    let mut failures = Vec::new();
    for mode in ["log", "metrics", "huge"] {
        let status = std::process::Command::new(&prlimit)
            .args(["--data=67108864", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "large_inputs_fit_a_bounded_native_child",
                "--nocapture",
            ])
            .env("PATH", &f.0)
            .env("OMS_DIGEST_MEMORY_ROOT", &f.0)
            .env("OMS_DIGEST_MEMORY_MODE", mode)
            .current_dir(&f.0)
            .status()
            .unwrap();
        if !status.success() {
            failures.push(mode);
        }
    }
    assert!(
        failures.is_empty(),
        "bounded memory modes failed: {failures:?}"
    );
}

#[test]
fn streamed_boundaries_keep_exact_log_bytes_and_metric_records() {
    let f = Fixture::new();
    let crossing = format!("{}ERROR crossing\n", "a".repeat(8190));
    let traceback = format!("{}Traceback (most recent call last):\n", "b".repeat(8180));
    let mut log = crossing.as_bytes().to_vec();
    log.extend_from_slice(traceback.as_bytes());
    for i in 1..=45 {
        log.extend_from_slice(format!("trace {i}\n").as_bytes());
    }
    log.extend_from_slice(b"ERROR last \xff\r\nlast-\xfe");
    fs::write(f.0.join("log"), &log).unwrap();
    let (code, out, err) = f.call(&["--tail", "1", "--patterns", "1", "log"]);
    assert_eq!(code, 0);
    assert!(err.is_empty());
    let expected = b"## Error patterns\n\n```\n48:ERROR last \xff\r\n```\n";
    assert!(out.windows(expected.len()).any(|s| s == expected));
    assert!(out.ends_with(b"## Tail\n\n```\nlast-\xfe```\n"));
    let text = String::from_utf8_lossy(&out);
    let trace = text
        .split("## Last traceback\n\n```\n")
        .nth(1)
        .unwrap()
        .split("```\n")
        .next()
        .unwrap();
    assert!(trace.starts_with(&traceback));
    assert!(trace.ends_with("trace 39\n"));
    assert!(!trace.contains("trace 40\n"));
    let (_, out, _) = f.call(&["--tail", "0", "--patterns", "0", "log"]);
    assert!(out.ends_with(b"## Tail\n\n```\n```\n"));
    let prefix = "{\"step\":1,\"name\":\"loss\",\"value\":2,\"padding\":\"";
    let record = format!("{prefix}{}\"}}\r\n", "x".repeat(8191 - prefix.len() - 2));
    assert_eq!(record.as_bytes()[8191], b'\r');
    let mut records = record.into_bytes();
    records.extend_from_slice(b"{\"step\":2,\"name\":\"loss\",\"value\":1}\r\r\n");
    fs::write(f.0.join("metrics"), records).unwrap();
    let (_, out, err) = f.call(&["--metrics", "metrics"]);
    let out = String::from_utf8(out).unwrap();
    assert!(err.is_empty());
    assert!(out.contains("0 record(s) skipped"));
    assert!(out.contains("| loss | 2 | 2 | 1 | 1 @2 (min) | n/a | 0 |"));
}

#[test]
fn streamed_caps_and_long_valid_records_keep_source_boundaries() {
    let f = Fixture::new();
    fs::write(f.0.join("log"), "x\n").unwrap();
    let (_, full, _) = f.call(&["log"]);
    let marker = b"## Tail\n\n```\n";
    let position = full
        .windows(marker.len())
        .position(|s| s == marker)
        .unwrap()
        + marker.len();
    let fits = 512 - 160 - position - 1;
    for length in [0, fits, fits + 1] {
        fs::write(f.0.join("log"), format!("{}\n", "x".repeat(length))).unwrap();
        let (_, out, err) = f.call(&["--max-bytes", "512", "log"]);
        assert!(err.is_empty());
        assert!(out.len() <= 512);
        let text = String::from_utf8(out).unwrap();
        if length == fits {
            assert!(text.contains(&format!("\n{}\n", "x".repeat(length))));
            assert!(text.contains("1 line(s), 4 bytes omitted"));
        } else if length > fits {
            assert!(!text.contains(&"x".repeat(length)));
            assert!(text.contains(&format!("1 line(s), {} bytes omitted", length + 1)));
        }
    }
    let record = format!(
        "{{\"step\":1,\"name\":\"loss\",\"value\":1,\"padding\":\"{}\"}}\n",
        "a".repeat(3_600_000)
    );
    fs::write(f.0.join("metrics"), record).unwrap();
    let (_, out, err) = f.call(&["--metrics", "metrics"]);
    assert!(err.is_empty());
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("| loss | 1 | 1 | 1 |")
    );
}

#[test]
fn readable_regular_log_does_not_require_seek_support() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let f = Fixture::new();
    let (code, out, err) = f.call(&["--tail", "1", "/proc/self/status"]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    assert!(err.is_empty());
    assert!(String::from_utf8(out).unwrap().contains("## Tail"));
}

fn native_tool(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap().join(p)
            }
        })
}

#[test]
fn log_reads_one_scan_plus_selected_ranges() {
    use std::io::Write;
    fn counters() -> (u64, u64) {
        let text = fs::read_to_string("/proc/self/io").unwrap();
        let value = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        (value("rchar:"), value("syscr:"))
    }
    if let Some(root) = std::env::var_os("OMS_DIGEST_TRACE_ROOT") {
        let root = PathBuf::from(root);
        let before = counters();
        let (code, _, err) = run(
            &["--patterns", "3", "--tail", "2", "heavy"].map(OsString::from),
            &root,
            &AtomicBool::new(false),
        );
        let after = counters();
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        fs::write(
            root.join("counts"),
            format!("{} {}", after.0 - before.0, after.1 - before.1),
        )
        .unwrap();
        return;
    }
    if !std::path::Path::new("/proc/self/io").is_file() {
        eprintln!(
            "kernel read accounting skipped: procfs unavailable; compiled-reader instrumentation remains active"
        );
        return;
    }
    let f = Fixture::new();
    let record = format!("ERROR {}\n", "x".repeat(4089));
    assert_eq!(record.len(), 4096);
    let mut file = fs::File::create(f.0.join("heavy")).unwrap();
    let batch = record.repeat(16);
    for _ in 0..128 {
        file.write_all(batch.as_bytes()).unwrap();
    }
    drop(file);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "log_reads_one_scan_plus_selected_ranges",
            "--nocapture",
        ])
        .env("PATH", &f.0)
        .env("OMS_DIGEST_TRACE_ROOT", &f.0)
        .status()
        .unwrap();
    assert!(status.success());
    let counts = fs::read_to_string(f.0.join("counts")).unwrap();
    let bytes = counts
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let selected = 5 * 4095;
    let bound = 8 * 1024 * 1024 + selected + 4096;
    eprintln!(
        "native kernel accounting: {counts}; bound {bound} bytes (one scan, selected ranges, procfs accounting)"
    );
    assert!(
        bytes <= bound,
        "extra sequential log reads: {bytes} > {bound}"
    );
}
