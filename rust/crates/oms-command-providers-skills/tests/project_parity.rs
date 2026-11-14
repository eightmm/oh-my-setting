use oms_command_providers_skills::run_with_source;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("oms-{prefix}-{}-{nanos}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("create tempdir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn oms_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates")
        .parent()
        .expect("rust")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn call(tool: &str, args: &[&str], cwd: &Path) -> (i32, String, String) {
    let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let stop = AtomicBool::new(false);
    let (code, out, err) = run_with_source(tool, &os_args, cwd, &oms_source(), &stop);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[test]
fn apply_and_remove_template_on_missing_and_live_dirs() {
    let tmp = TempDir::new("apply-live");
    let target = tmp.path().join("new-project");
    assert!(!target.exists());

    let target_str = target.to_str().expect("utf8");
    let (code, out, err) = call(
        "apply-project-template",
        &["general", target_str, "--no-private"],
        tmp.path(),
    );
    assert_eq!(code, 0, "apply failed: out={out}\nerr={err}");
    assert!(target.is_dir(), "target directory should be created");
    assert!(target.join("PROJECT.md").is_file(), "PROJECT.md missing");
    assert!(target.join("AGENTS.md").is_file(), "AGENTS.md missing");

    let (rm_code, rm_out, rm_err) = call(
        "remove-project-template",
        &["general", target_str],
        tmp.path(),
    );
    assert_eq!(rm_code, 0, "remove failed: out={rm_out}\nerr={rm_err}");
    assert!(
        rm_out.contains("removed general block(s)"),
        "unexpected remove output: {rm_out}"
    );
}

#[test]
fn apply_template_dry_run_on_missing_dir() {
    if let Some(target_str) = std::env::var_os("OMS_TEST_DRY_APPLY_TARGET") {
        let target = PathBuf::from(&target_str);
        let parent = target.parent().expect("parent");
        let (code, out, err) = call(
            "apply-project-template",
            &["general", target.to_str().expect("utf8"), "--no-private"],
            parent,
        );
        assert_eq!(code, 0, "dry apply failed: out={out}\nerr={err}");
        assert!(out.contains("would ensure"), "missing would ensure: {out}");
        assert!(out.contains("PROJECT.md"), "missing PROJECT.md note: {out}");
        assert!(!target.exists(), "dry run must not create target dir");
        return;
    }

    let tmp = TempDir::new("apply-dry");
    let target = tmp.path().join("absent-project");
    let exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(exe)
        .env("OMS_TEST_DRY_APPLY_TARGET", &target)
        .env("OH_MY_SETTING_DRY_RUN", "1")
        .args([
            "--exact",
            "apply_template_dry_run_on_missing_dir",
            "--nocapture",
        ])
        .output()
        .expect("spawn child test");
    assert!(
        status.status.success(),
        "child failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(!target.exists(), "dry run leaked target dir");
}

#[test]
fn project_doctor_checks_slurm_contract_and_notebook_hygiene() {
    let tmp = TempDir::new("doctor");
    let repo = tmp.path();
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .status()
        .expect("git init");
    assert!(init.success());

    let (code, _, err) = call("apply-project-template", &["ml", ".", "--no-private"], repo);
    assert_eq!(code, 0, "apply ml failed: {err}");

    let (code, _, err) = call(
        "apply-project-template",
        &["slurm", ".", "--no-private"],
        repo,
    );
    assert_eq!(code, 0, "apply slurm failed: {err}");

    let spec_path = repo.join("PROJECT.md");
    let spec = fs::read_to_string(&spec_path).expect("read PROJECT.md");
    let confirmed_spec = spec.replacen("draft", "confirmed", 1);
    fs::write(&spec_path, confirmed_spec).expect("write PROJECT.md");

    let nb = serde_json::json!({
        "cells": [{
            "cell_type": "code",
            "outputs": [{"output_type": "stream", "text": ["a\n", "b\n", &"x".repeat(4500)]}]
        }],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    });
    fs::write(repo.join("explore.ipynb"), nb.to_string()).expect("write ipynb");
    let add = Command::new("git")
        .args(["add", "explore.ipynb"])
        .current_dir(repo)
        .status()
        .expect("git add");
    assert!(add.success());

    let (doc_code, doc_out, doc_err) = call("project-doctor", &["."], repo);
    assert!(
        doc_code == 0 || doc_code == 1,
        "unexpected doctor exit {doc_code}: {doc_err}"
    );
    assert!(
        doc_out.contains("Slurm execution contract"),
        "expected Slurm execution contract check in:\n{doc_out}"
    );
    assert!(
        doc_out.contains("explore.ipynb"),
        "expected notebook hygiene check in:\n{doc_out}"
    );
}

#[test]
fn init_prints_feature_readiness_checklist() {
    let tmp = TempDir::new("init");
    let repo = tmp.path();
    let git_init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .status()
        .expect("git init");
    assert!(git_init.success());

    let (code, out, err) = call("init", &["--no-private"], repo);
    assert_eq!(code, 0, "init failed: out={out}\nerr={err}");
    assert!(
        out.contains("Feature readiness: council and panel workers need no PROJECT.md."),
        "missing feature readiness line:\n{out}"
    );
}

#[test]
fn detect_style_handles_missing_and_marker_dirs() {
    let tmp = TempDir::new("detect");
    let absent = tmp.path().join("does-not-exist");
    let (code, out, err) = call(
        "detect-project-style",
        &[absent.to_str().expect("utf8")],
        tmp.path(),
    );
    assert_eq!(code, 0, "detect missing failed: out={out}\nerr={err}");
    assert_eq!(out.trim(), "general");

    let slurm_dir = tmp.path().join("slurm-proj");
    fs::create_dir_all(&slurm_dir).expect("mkdir slurm");
    fs::write(
        slurm_dir.join("run.sbatch"),
        "#!/bin/bash\n#SBATCH --time=01:00:00\n",
    )
    .expect("write sbatch");
    let (s_code, s_out, s_err) = call(
        "apply-project-template",
        &["auto", slurm_dir.to_str().expect("utf8"), "--no-private"],
        tmp.path(),
    );
    assert_eq!(
        s_code, 0,
        "apply auto slurm failed: out={s_out}\nerr={s_err}"
    );
    let spec = fs::read_to_string(slurm_dir.join("PROJECT.md")).expect("read slurm PROJECT.md");
    assert!(spec.contains("## Slurm"), "expected Slurm section:\n{spec}");

    let ml_dir = tmp.path().join("ml-proj");
    fs::create_dir_all(&ml_dir).expect("mkdir ml");
    fs::write(
        ml_dir.join("pyproject.toml"),
        "[project]\nname = \"demo\"\ndependencies = [\"torch\"]\n",
    )
    .expect("write pyproject");
    let (m_code, m_out, m_err) = call(
        "detect-project-style",
        &[ml_dir.to_str().expect("utf8")],
        tmp.path(),
    );
    assert_eq!(m_code, 0, "detect ml failed: err={m_err}");
    assert_eq!(m_out.trim(), "ml");
}
