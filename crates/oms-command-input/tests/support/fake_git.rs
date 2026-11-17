#![forbid(unsafe_code)]
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hold() {
    let bound = std::env::var("OMS_TEST_CALL_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(bound.max(6));
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn main() {
    let executable = std::env::current_exe().unwrap();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--descendant") {
        hold();
        return;
    }
    if executable.file_name().is_some_and(|name| name == "monitor") {
        if let Some(path) = std::env::var_os("OMS_INPUT_MONITOR_MARKER") {
            std::fs::write(path, b"executed").unwrap();
        }
        std::io::stdout().write_all(b"clock\0").unwrap();
        return;
    }
    let mode = std::env::var("OMS_INPUT_PROCESS_MODE").unwrap_or_default();
    if mode == "audit" {
        let record = format!(
            "{}\t{}\n",
            std::env::var("GIT_OPTIONAL_LOCKS").unwrap_or_default(),
            args.iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\t")
        );
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::var_os("OMS_INPUT_PROCESS_LOG").unwrap())
            .unwrap();
        file.write_all(record.as_bytes()).unwrap();
        let error = Command::new(std::env::var_os("OMS_INPUT_REAL_GIT").unwrap())
            .args(args)
            .exec();
        panic!("proxy Git: {error}");
    }
    let mut descendant = Command::new(&executable)
        .arg("--descendant")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let group = Command::new("ps")
        .args(["-o", "pgid=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    let record = format!(
        "{} {} {}\n",
        std::process::id(),
        descendant.id(),
        String::from_utf8(group.stdout).unwrap().trim()
    );
    std::fs::write(std::env::var_os("OMS_INPUT_PROCESS_LOG").unwrap(), record).unwrap();
    if mode == "overflow" {
        std::io::stdout().write_all(&vec![b'x'; 65537]).unwrap();
    }
    if mode != "orphan" {
        let _ = descendant.wait();
    }
}
