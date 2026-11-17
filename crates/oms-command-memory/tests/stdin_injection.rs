#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{seq}-{nanos}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn stdin_injection_supports_append_and_review_capture() {
    let dir = TempDir::new("oms-memory-stdin-test");
    let repo = &dir.0;
    let init = Command::new("git")
        .arg("init")
        .arg(repo)
        .output()
        .expect("git init");
    assert!(init.status.success());
    std::fs::create_dir_all(repo.join(".oms")).unwrap();

    let interrupted = AtomicBool::new(false);
    let args = |items: &[&str]| -> Vec<OsString> { items.iter().map(OsString::from).collect() };

    let (code, _, err) =
        oms_command_memory::run_with_source(&args(&["init"]), repo, repo, &interrupted);
    assert_eq!(code, 0, "init failed: {}", String::from_utf8_lossy(&err));

    let payload = b"Prefer injected bytes over process stdin\n";
    let (code, _, err) = oms_command_memory::run_with_source_and_stdin(
        &args(&["append", "--stdin", "--agent", "codex"]),
        repo,
        repo,
        payload,
        &interrupted,
    );
    assert_eq!(
        code,
        0,
        "append --stdin failed: {}",
        String::from_utf8_lossy(&err)
    );

    let (code, out, err) =
        oms_command_memory::run_with_source(&args(&["show"]), repo, repo, &interrupted);
    assert_eq!(code, 0, "show failed: {}", String::from_utf8_lossy(&err));
    let shown = String::from_utf8_lossy(&out);
    assert!(shown.contains("Prefer injected bytes over process stdin"));

    // Exceeding capture 64 KiB cap stays silent exit 0 without blocking on stdin.
    let oversized = vec![b'x'; 65_537];
    let (code, out, err) = oms_command_memory::run_review_with_stdin(
        &args(&["capture", "--repo", repo.to_str().unwrap()]),
        repo,
        repo,
        &oversized,
        &interrupted,
    );
    assert_eq!(code, 0, "capture stderr: {}", String::from_utf8_lossy(&err));
    assert!(out.is_empty());
}
