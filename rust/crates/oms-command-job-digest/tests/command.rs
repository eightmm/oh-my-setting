use oms_command_job_digest::run;
use std::ffi::OsString;
use std::fs;
use std::sync::atomic::AtomicBool;

fn call(repo: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (code, out, err) = run(&args, repo, &AtomicBool::new(false));
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn logs_metrics_and_limits() {
    let repo = std::env::temp_dir().join(format!("oms-job-test-{}", std::process::id()));
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("run.log"),
        "ok\nTraceback (most recent call last):\nCUDA failure\nlast\n",
    )
    .unwrap();
    fs::write(repo.join("metrics.jsonl"), "{\"step\":1,\"name\":\"loss\",\"value\":2}\n{\"step\":2,\"name\":\"loss\",\"value\":1}\n{\"step\":3,\"name\":\"loss\",\"value\":\"nan\"}\ninvalid\n").unwrap();
    let (code, out, err) = call(
        &repo,
        &["--metrics", "metrics.jsonl", "--tail", "1", "run.log"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("3:CUDA failure"));
    assert!(out.contains("| loss | 3 | 3 | nan | 1 @2 (min) | n/a | 1, first @3 |"));
    assert!(out.contains("1 record(s) skipped"));
    assert!(out.contains("## Tail\n\n```\nlast\n```"));
    let (code, _, err) = call(&repo, &["--max-bytes", "511", "run.log"]);
    assert_eq!(code, 2);
    assert!(err.contains("at least 512"));
    let (code, out, _) = call(&repo, &["--max-bytes", "512", "run.log"]);
    assert_eq!(code, 0);
    assert!(out.len() <= 512);
    assert_eq!(call(&repo, &["--wait-timeout", "0", "1"]).0, 2);
    assert_eq!(call(&repo, &["--tail", "bad", "run.log"]).0, 2);
    assert_eq!(call(&repo, &[]).0, 2);
    assert_eq!(call(&repo, &["--help"]).0, 0);
    fs::remove_dir_all(repo).unwrap();
}

#[test]
fn scheduler_observer() {
    if let Some(repo) = std::env::var_os("OMS_JOB_FIXTURE") {
        let repo = std::path::PathBuf::from(repo);
        let hanging = std::env::var_os("OMS_JOB_HANG").is_some();
        let (code, out, err) = call(
            &repo,
            &[
                "--wait",
                "--wait-timeout",
                if hanging { "1" } else { "5" },
                "123",
            ],
        );
        if hanging {
            assert_eq!(code, 124);
            assert!(out.contains("wait: pending"));
            assert!(err.contains("queue state may be unknown"));
            return;
        }
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("no longer queued"));
        assert!(out.contains("## Slurm accounting"));
        assert!(out.contains("COMPLETED"));
        return;
    }
    let repo = std::env::temp_dir().join(format!("oms-job-scheduler-{}", std::process::id()));
    fs::create_dir_all(&repo).unwrap();
    let source = repo.join("fixture.rs");
    fs::write(&source, "fn main() { if std::env::var_os(\"OMS_JOB_HANG\").is_some() { if std::env::args().nth(1).as_deref() != Some(\"child\") { let _ = std::process::Command::new(std::env::current_exe().unwrap()).arg(\"child\").spawn(); } std::thread::sleep(std::time::Duration::from_secs(30)); } else if std::env::args().next().unwrap().ends_with(\"sacct\") { println!(\"header\\nrule\\nCOMPLETED\"); } }").unwrap();
    let fixture = repo.join("squeue");
    assert!(
        std::process::Command::new("rustc")
            .args(["--edition", "2024"])
            .arg(&source)
            .arg("-o")
            .arg(&fixture)
            .status()
            .unwrap()
            .success()
    );
    fs::copy(&fixture, repo.join("sacct")).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "scheduler_observer"])
        .env("OMS_JOB_FIXTURE", &repo)
        .env("PATH", &repo)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "scheduler_observer"])
        .env("OMS_JOB_FIXTURE", &repo)
        .env("OMS_JOB_HANG", "1")
        .env("PATH", &repo)
        .status()
        .unwrap();
    assert!(status.success());
    fs::remove_dir_all(repo).unwrap();
}

#[test]
fn raw_log_and_python_constants() {
    use std::os::unix::ffi::OsStringExt;
    let repo = std::env::temp_dir().join(format!("oms-job-raw-{}", std::process::id()));
    fs::create_dir_all(&repo).unwrap();
    let filename = OsString::from_vec(b"run-\xff.log".to_vec());
    fs::write(
        repo.join(&filename),
        b"ERROR \xff\nTraceback (most recent call last):\n\xfe\n",
    )
    .unwrap();
    fs::write(
        repo.join("metrics"),
        b"{\"step\":1,\"name\":\"loss\",\"value\":9,\"value\":NaN}\n",
    )
    .unwrap();
    let args = vec![
        OsString::from("--metrics"),
        OsString::from("metrics"),
        filename.clone(),
    ];
    let (code, out, _) = run(&args, &repo, &AtomicBool::new(false));
    assert_eq!(code, 0);
    assert!(
        out.windows(b"- log: run-\xff.log".len())
            .any(|x| x == b"- log: run-\xff.log")
    );
    assert!(out.windows(b"ERROR \xff".len()).any(|x| x == b"ERROR \xff"));
    assert!(
        out.windows(b"| loss | 1 | 1 | nan |".len())
            .any(|x| x == b"| loss | 1 | 1 | nan |")
    );
    assert!(oms_command_job_digest::owns(&args));
    let mut capped_args = vec![OsString::from("--max-bytes"), OsString::from("512")];
    capped_args.extend(args);
    let (code, capped, _) = run(&capped_args, &repo, &AtomicBool::new(false));
    assert_eq!(code, 0);
    assert!(capped.len() <= 512);
    assert!(
        capped
            .windows(b"ERROR \xff".len())
            .any(|x| x == b"ERROR \xff")
    );
    fs::remove_dir_all(repo).unwrap();
}

#[test]
fn metrics_numeric_format_and_date() {
    let repo = std::env::temp_dir().join(format!("oms-job-number-{}", std::process::id()));
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("metrics"), "{\"step\":1,\"name\":\"large\",\"value\":999999.9,\"ts\":\"2000-01-01\"}\n{\"step\":1,\"name\":\"small\",\"value\":0.000012345}\n").unwrap();
    let (code, out, err) = call(&repo, &["--metrics", "metrics"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("| large | 1 | 1 | 1e+06 |"));
    assert!(out.contains("| small | 1 | 1 | 1.2345e-05 |"));
    assert!(!out.contains("no timestamp in the file"));
    fs::remove_dir_all(repo).unwrap();
}
