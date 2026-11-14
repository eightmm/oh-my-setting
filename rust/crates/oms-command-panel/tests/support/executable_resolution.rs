//! Resolution tests run in isolated subprocesses so environment changes stay local.
use crate::{managed, native_panel};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempRepo(PathBuf);
impl TempRepo {
    fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("oms-resolution-{}-{id}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let repo = Self(root.canonicalize().unwrap());
        let mut git = Command::new("git");
        clean_env_child(&mut git);
        assert!(
            git.args(["init", "-q"])
                .current_dir(&repo.0)
                .status()
                .unwrap()
                .success()
        );
        repo
    }
}
impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn clean_env_child(command: &mut Command) {
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        command.env("TMPDIR", tmp);
    }
}

fn write_file(path: &Path, mode: u32) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn resolution_probe(source: std::ffi::OsString) {
    let source = PathBuf::from(source);
    let expected = std::env::var_os("PANEL_RESOLUTION_EXPECTED").map(PathBuf::from);
    let expected_job = std::env::var_os("PANEL_RESOLUTION_JOB")
        .map(PathBuf::from)
        .or_else(|| expected.clone());
    let job = managed::test_job_executable(&source);
    match expected_job.filter(|_| std::env::var_os("PANEL_RESOLUTION_NO_JOB").is_none()) {
        Some(path) => assert_eq!(job.unwrap(), path),
        None => assert_eq!(
            job.unwrap_err().message,
            "oms supervisor executable unavailable"
        ),
    }
    let prefix = native_panel::test_entry_prefix(&source);
    match expected {
        Some(path) => {
            let mut expected = Vec::new();
            if std::env::var_os("PANEL_RESOLUTION_SCRIPT").is_some() {
                expected.push("bash".to_owned());
            }
            expected.push(path.to_string_lossy().into_owned());
            let prefix = prefix.unwrap();
            assert_eq!(prefix, expected);
            assert!(native_panel::test_entry_matches(
                &std::env::current_dir().unwrap(),
                &prefix
            ));
        }
        None => assert_eq!(
            prefix.unwrap_err().message,
            "oms panel executable unavailable"
        ),
    }
}

#[test]
fn native_and_legacy_resolution_order() {
    if let Some(source) = std::env::var_os("PANEL_RESOLUTION_SOURCE") {
        resolution_probe(source);
        return;
    }
    resolution_cases(&[
        "current",
        "sibling",
        "current_without_script",
        "sibling_without_script",
        "script",
        "path",
        "missing",
        "empty_path",
        "unset_path",
        "nonexec_sibling",
        "directory_sibling",
        "nonexec_path",
        "directory_path",
        "directory_script",
        "override",
        "empty_override",
        "missing_override",
        "relative_path",
        "bare_override",
        "relative_override",
    ]);
}

#[test]
fn source_paths_and_job_execute_access() {
    resolution_cases(&[
        "relative_source",
        "empty_source",
        "nonexec_script",
        "nonexec_script_without_path",
        "nonexec_override",
    ]);
}

#[test]
fn symlinked_path_keeps_oms_name() {
    resolution_cases(&["symlink_path", "symlink_path_dir"]);
}

fn resolution_cases(cases: &[&str]) {
    for &case in cases {
        let repo = TempRepo::new();
        let root = &repo.0;
        let bin = root.join("bin");
        let path_dir = root.join("search path");
        let source = root.join("source tree");
        for dir in [&bin, &path_dir, &source, &source.join("scripts")] {
            std::fs::create_dir(dir).unwrap();
        }
        let executable = bin.join(if case.starts_with("current") {
            "oms"
        } else {
            "panel-probe"
        });
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let sibling = bin.join("oms");
        let script = source.join("scripts/oms");
        let on_path = path_dir.join("oms");
        let mut expected = None;
        let mut expected_job = None;
        let mut legacy = false;
        match case {
            "current" | "current_without_script" => {
                if case == "current" {
                    write_file(&script, 0o600);
                }
                write_file(&on_path, 0o700);
                expected = Some(executable.clone());
            }
            "sibling" | "sibling_without_script" => {
                write_file(&sibling, 0o700);
                if case == "sibling" {
                    write_file(&script, 0o600);
                }
                write_file(&on_path, 0o700);
                expected = Some(sibling.clone());
            }
            "script" | "relative_source" => {
                write_file(&script, 0o700);
                write_file(&on_path, 0o700);
                expected = Some(script.clone());
                legacy = true;
            }
            "path" | "relative_path" | "empty_override" | "override" | "missing_override"
            | "bare_override" | "relative_override" | "nonexec_sibling" | "directory_sibling"
            | "directory_script" => {
                write_file(&on_path, 0o700);
                expected = Some(on_path.clone());
                match case {
                    "nonexec_sibling" => write_file(&sibling, 0o600),
                    "directory_sibling" => std::fs::create_dir(&sibling).unwrap(),
                    "directory_script" => std::fs::create_dir(&script).unwrap(),
                    "override" => {
                        let path = root.join("explicit supervisor");
                        write_file(&path, 0o700);
                        expected_job = Some(path);
                    }
                    "missing_override" => expected_job = Some(root.join("missing supervisor")),
                    "bare_override" => expected_job = Some(PathBuf::from("oms")),
                    "relative_override" => expected_job = Some(PathBuf::from("search path/oms")),
                    _ => {}
                }
            }
            "nonexec_script" | "nonexec_script_without_path" => {
                write_file(&script, 0o600);
                expected = Some(script.clone());
                legacy = true;
                if case == "nonexec_script" {
                    write_file(&on_path, 0o700);
                    expected_job = Some(on_path.clone());
                }
            }
            "symlink_path" | "symlink_path_dir" => {
                let target = path_dir.join("oms-0.7.0");
                write_file(&target, 0o700);
                std::os::unix::fs::symlink(&target, &on_path).unwrap();
                expected = Some(on_path.clone());
            }
            "empty_source" => {
                let scripts = root.join("scripts");
                std::fs::create_dir(&scripts).unwrap();
                let script = scripts.join("oms");
                write_file(&script, 0o700);
                expected = Some(script);
                legacy = true;
            }
            "nonexec_override" => {
                let path = root.join("nonexec supervisor");
                write_file(&path, 0o600);
                expected_job = Some(path);
            }
            "nonexec_path" => write_file(&on_path, 0o600),
            "directory_path" => std::fs::create_dir(&on_path).unwrap(),
            _ => {}
        }
        let mut child = Command::new(executable);
        clean_env_child(&mut child);
        child
            .current_dir(root)
            .args([
                "--exact",
                "executable_resolution::native_and_legacy_resolution_order",
                "--nocapture",
            ])
            .env("PATH", &path_dir)
            .env("PANEL_RESOLUTION_SOURCE", &source);
        if let Some(path) = &expected {
            child.env("PANEL_RESOLUTION_EXPECTED", path);
        }
        if let Some(path) = &expected_job {
            child.env("PANEL_RESOLUTION_JOB", path);
            if matches!(
                case,
                "override"
                    | "missing_override"
                    | "nonexec_override"
                    | "bare_override"
                    | "relative_override"
            ) {
                child.env("OMS_PANEL_JOB_EXECUTABLE", path);
            }
        }
        if legacy {
            child.env("PANEL_RESOLUTION_SCRIPT", "1");
        }
        if case == "empty_override" {
            child.env("OMS_PANEL_JOB_EXECUTABLE", "");
        }
        if case == "empty_path" {
            child.env("PATH", "");
        }
        if case == "unset_path" {
            child.env_remove("PATH");
        }
        if case == "relative_path" {
            child.env("PATH", "missing:search path");
        }
        if matches!(
            case,
            "nonexec_script_without_path" | "missing_override" | "nonexec_override"
        ) {
            child.env("PANEL_RESOLUTION_NO_JOB", "1");
        }
        if case == "relative_source" {
            child.env("PANEL_RESOLUTION_SOURCE", "source tree");
        }
        if case == "empty_source" {
            child.env("PANEL_RESOLUTION_SOURCE", "");
        }
        if case == "symlink_path_dir" {
            let alias = root.join("search alias");
            std::os::unix::fs::symlink(&path_dir, &alias).unwrap();
            child.env("PATH", alias);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "probe did not run"
        );
    }
}
