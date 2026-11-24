use oms_command_providers_skills::{run_with_source, validate_bundled_skill};
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

const GOOD_SKILL: &str = "---\nname: demo\ndescription: Demo skill used only by the parity tests for reference checks.\n---\n[a](references/a.md)\n";

/// Run one tool in a child process (this test binary re-entered) with a scrubbed environment,
/// because the crate forbids unsafe code and so cannot set process-wide variables in-process.
fn isolated(
    tool: &str,
    args: &[&str],
    cwd: &Path,
    source: &Path,
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let spec = serde_json::json!({"tool":tool,"args":args,"cwd":cwd,"source":source});
    let mut child = Command::new(std::env::current_exe().expect("exe"));
    child
        .args([
            "--exact",
            "isolated_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
        .env_remove("XDG_CACHE_HOME")
        .env("OMS_PARITY_CALL", spec.to_string());
    for (k, v) in env {
        // An empty value means "leave this variable unset".
        if v.is_empty() {
            child.env_remove(k);
        } else {
            child.env(k, v);
        }
    }
    let out = child.output().expect("spawn child");
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text
        .lines()
        .find_map(|l| l.split_once("PARITY-RESULT:").map(|(_, r)| r))
        .unwrap_or_else(|| {
            panic!(
                "no result: {text}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let v: serde_json::Value = serde_json::from_str(row).expect("json");
    (
        v["code"].as_i64().expect("code") as i32,
        v["out"].as_str().unwrap_or("").to_owned(),
        v["err"].as_str().unwrap_or("").to_owned(),
    )
}

#[test]
fn isolated_child() {
    let Ok(raw) = std::env::var("OMS_PARITY_CALL") else {
        return;
    };
    let v: serde_json::Value = serde_json::from_str(&raw).expect("spec");
    let args: Vec<OsString> = v["args"]
        .as_array()
        .expect("args")
        .iter()
        .map(|a| OsString::from(a.as_str().expect("str")))
        .collect();
    let stop = AtomicBool::new(false);
    let (code, out, err) = run_with_source(
        v["tool"].as_str().expect("tool"),
        &args,
        Path::new(v["cwd"].as_str().expect("cwd")),
        Path::new(v["source"].as_str().expect("source")),
        &stop,
    );
    println!(
        "PARITY-RESULT:{}",
        serde_json::json!({"code":code,"out":String::from_utf8_lossy(&out),"err":String::from_utf8_lossy(&err)})
    );
}

#[test]
fn models_preview_preserves_full_catalog_routability() {
    let tmp = TempDir::new("models-catalog");
    let home = tmp.path().join("home");
    let cache = home.join("capabilities");
    fs::create_dir_all(&cache).expect("cache");
    let preview = (0..19)
        .map(|minor| format!("gpt-5.{minor}-sol"))
        .chain(std::iter::once("gpt-6.1-sol".into()))
        .collect::<Vec<_>>();
    let mut catalog = preview.clone();
    catalog.extend(["gpt-6.2-sol".into(), "claude-sonnet-5-5".into()]);
    fs::write(
        cache.join("codex.models"),
        format!("{}\n", catalog.join("\n")),
    )
    .expect("catalog");
    let (code, out, err) = isolated(
        "models",
        &["--providers", "codex", "--json"],
        tmp.path(),
        &oms_source(),
        &[
            ("HOME", utf8(&home)),
            ("OMS_CAPABILITY_DIR", utf8(&cache)),
            ("PATH", ""),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).expect("report");
    let row = &report["providers"][0];
    assert_eq!(row["models"], serde_json::json!(preview));
    assert_eq!(row["catalog_models"], serde_json::json!(catalog));
    assert_eq!(row["routable"], serde_json::json!(["gpt-6.2-sol"]));
    let mut not_routed = preview;
    not_routed.push("claude-sonnet-5-5".into());
    assert_eq!(row["not_routed"], serde_json::json!(not_routed));
    assert_eq!(row["usable"], serde_json::Value::Null);
    assert_eq!(row["exact_model_override"], true);
    assert!(!cache.join("codex.env").exists());

    let (code, out, err) = isolated(
        "models",
        &["--providers", "deepseek,vibe,openhands", "--json"],
        tmp.path(),
        &oms_source(),
        &[
            ("HOME", utf8(&home)),
            ("OMS_CAPABILITY_DIR", utf8(&cache)),
            ("PATH", ""),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).expect("report");
    for row in report["providers"].as_array().unwrap() {
        assert_eq!(row["exact_model_override"], false, "{row}");
        assert_eq!(row["usable"], serde_json::Value::Null);
    }

    let mut rows = (0..21)
        .map(|minor| format!("gemini-3.{minor}-flash-low\tGemini 3 Flash"))
        .collect::<Vec<_>>();
    let preview = rows[..20].to_vec();
    rows.extend([
        "gemini-4-carbon-high\tCarbon High".into(),
        "gemini-4-neon-high\tNeon High".into(),
        "gemini-4-neon-high\tAlternate label".into(),
        "gemini-4-oxygen-low".into(),
        "gemini-pro-orchestrator\tOrchestrator".into(),
        "gemini-pro-orchestrator-argon\tOrchestrator Argon".into(),
        "gemini-4-bogus-high\twrong\tlabel".into(),
        "gemini-4-malformed-high\t".into(),
    ]);
    let contents = format!("{}\n", rows.join("\n"));
    fs::write(cache.join("antigravity.models"), &contents).unwrap();
    let (code, out, err) = isolated(
        "models",
        &["--providers", "antigravity", "--json"],
        tmp.path(),
        &oms_source(),
        &[
            ("HOME", utf8(&home)),
            ("OMS_CAPABILITY_DIR", utf8(&cache)),
            ("PATH", ""),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = &report["providers"][0];
    assert_eq!(row["models"], serde_json::json!(preview));
    let selectors = (0..21)
        .map(|minor| format!("gemini-3.{minor}-flash-low"))
        .chain(
            [
                "gemini-4-carbon-high",
                "gemini-4-neon-high",
                "gemini-4-oxygen-low",
                "gemini-pro-orchestrator",
                "gemini-pro-orchestrator-argon",
            ]
            .map(str::to_owned),
        )
        .collect::<Vec<_>>();
    assert_eq!(row["catalog_models"], serde_json::json!(selectors));
    assert_eq!(
        row["routable"],
        serde_json::json!([
            "gemini-4-carbon-high",
            "gemini-4-neon-high",
            "gemini-4-oxygen-low",
            "gemini-pro-orchestrator",
            "gemini-pro-orchestrator-argon"
        ])
    );
    assert_eq!(
        fs::read_to_string(cache.join("antigravity.models")).unwrap(),
        contents
    );
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("utf8")
}

fn write_all(base: &Path, files: &[(&str, &str)]) {
    for (name, body) in files {
        let path = base.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }
}

#[test]
fn prompt_audit_includes_references_and_reports_unchecked_surfaces() {
    let tmp = TempDir::new("prompt-audit");
    let source = tmp.path();
    write_all(
        source,
        &[
            ("config/prompt-audit-patterns.txt", "double[- ]check\n"),
            ("rules/global-AGENTS.md", "Use scoped evidence.\n"),
            ("templates/project-general-AGENTS.md", "Project rules.\n"),
            (
                "custom-skills/demo/SKILL.md",
                "Load references on demand.\n",
            ),
            (
                "custom-skills/demo/references/routing.md",
                "Double-check.\n",
            ),
            (
                "custom-skills/demo/references/nested/source.md",
                "Double-check.\n",
            ),
            ("output-styles/oms-demo.md", "Double-check.\n"),
            (
                "templates/project-skills/demo/references/workflow.md",
                "Double-check.\n",
            ),
            ("custom-skills/demo/assets/example.md", "Double-check.\n"),
            ("custom-skills/demo/notes.md", "Double-check.\n"),
            ("templates/README.md", "Double-check.\n"),
            ("crates/example/src/prompt.rs", "Double-check.\n"),
        ],
    );
    std::os::unix::fs::symlink(
        source.join("custom-skills/demo/notes.md"),
        source.join("custom-skills/demo/references/linked.md"),
    )
    .expect("link");
    let outside = TempDir::new("outside-audit");
    write_all(
        outside.path(),
        &[
            ("outside.md", "Double-check. Outside fixture marker.\n"),
            ("project-foreign-AGENTS.md", "Outside fixture marker.\n"),
            ("prompt-audit-patterns.txt", "double[- ]check\n"),
            (
                "project-skills/demo/references/outside.md",
                "Outside fixture marker.\n",
            ),
        ],
    );
    std::os::unix::fs::symlink(outside.path(), source.join("prompts"))
        .expect("link source directory");

    let (code, out, err) = call("project-doctor", &["--prompts", "--json", "."], source);
    assert_eq!(code, 0, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).expect("report");
    let files = report["files"].as_array().expect("files");
    let paths: std::collections::BTreeSet<_> = files
        .iter()
        .map(|file| file["path"].as_str().expect("path"))
        .collect();
    assert_eq!(
        paths,
        std::collections::BTreeSet::from([
            "rules/global-AGENTS.md",
            "templates/project-general-AGENTS.md",
            "custom-skills/demo/SKILL.md",
            "custom-skills/demo/references/routing.md",
            "custom-skills/demo/references/nested/source.md",
            "output-styles/oms-demo.md",
            "templates/project-skills/demo/references/workflow.md",
        ])
    );
    assert_eq!(report["findings"], 4);
    assert_eq!(report["schema"], 1);
    let source_present = |report: &serde_json::Value, path: &str| {
        report["coverage"]["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .find(|source| source["path"] == path)
            .expect("source coverage")["present"]
            .as_bool()
            .expect("present flag")
    };
    assert!(!source_present(&report, "prompts"));
    assert!(source_present(&report, "templates/project-skills"));
    assert_eq!(report["coverage"]["symlink_entries"], "not_followed");
    assert_eq!(report["coverage"]["duplicate_analysis"], "not_run");
    assert_eq!(report["coverage"]["semantic_conflict_analysis"], "not_run");
    assert_eq!(
        report["coverage"]["runtime_generated_prompts"],
        "not_scanned"
    );
    assert_eq!(report["coverage"]["runtime_delivery"], "not_verified");
    assert!(
        files
            .iter()
            .filter(|file| {
                file["path"].as_str().is_some_and(|path| {
                    path.contains("references/") || path.starts_with("output-styles/")
                })
            })
            .all(|file| file["injection"] == "on-demand")
    );

    let (code, out, err) = call("project-doctor", &["--prompts", "--strict", "."], source);
    assert_eq!(code, 1, "{err}");
    assert!(out.contains("7 files, 4 findings"), "{out}");
    assert!(out.contains("not checked"), "{out}");
    fs::write(
        source.join("config/prompt-audit-patterns.txt"),
        "Outside fixture marker\n",
    )
    .expect("patterns");
    fs::rename(source.join("templates"), source.join("saved-templates")).expect("move templates");
    std::os::unix::fs::symlink(outside.path(), source.join("templates"))
        .expect("link intermediate source directory");
    let (code, out, err) = call(
        "project-doctor",
        &["--prompts", "--strict", "--json", "."],
        source,
    );
    assert_eq!(code, 0, "{out} {err}");
    let report: serde_json::Value = serde_json::from_str(&out).expect("report");
    assert_eq!(report["findings"], 0);
    assert_eq!(report["files"].as_array().expect("files").len(), 5);
    assert!(!source_present(&report, "templates"));
    assert!(!source_present(&report, "templates/project-skills"));

    fs::rename(source.join("config"), source.join("saved-config")).expect("move config");
    let (code, out, err) = call("project-doctor", &["--prompts", "--strict", "."], source);
    assert_eq!(code, 0, "{out} {err}");
    std::os::unix::fs::symlink(outside.path(), source.join("config"))
        .expect("link config directory");
    let (code, out, err) = call("project-doctor", &["--prompts", "--strict", "."], source);
    assert_eq!(code, 2, "{out} {err}");
    assert!(err.contains("unsafe"), "{err}");
}

#[test]
fn prompt_audit_counts_only_relevant_files_toward_the_file_limit() {
    let tmp = TempDir::new("prompt-audit-budget");
    let source = tmp.path();
    write_all(
        source,
        &[(
            "custom-skills/demo/references/workflow.md",
            "Review evidence.\n",
        )],
    );
    let skill = source.join("custom-skills/demo");
    let assets = skill.join("assets");
    fs::create_dir(&assets).expect("assets");
    for n in 0..=10_000 {
        fs::write(assets.join(format!("{n}.txt")), "").expect("asset");
    }

    let assert_reference = || {
        let (code, out, err) = call("project-doctor", &["--prompts", "--json", "."], source);
        assert_eq!(code, 0, "{out} {err}");
        let report: serde_json::Value = serde_json::from_str(&out).expect("report");
        let files = report["files"].as_array().expect("files");
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0]["path"],
            "custom-skills/demo/references/workflow.md"
        );
    };
    assert_reference();
    let reference_assets = skill.join("references/assets");
    fs::rename(assets, &reference_assets).expect("move non-Markdown files");
    assert_reference();

    for n in 0..=10_000 {
        fs::rename(
            reference_assets.join(format!("{n}.txt")),
            reference_assets.join(format!("{n}.md")),
        )
        .expect("rename scanned reference");
    }
    let (code, out, err) = call("project-doctor", &["--prompts", "--json", "."], source);
    assert_eq!(code, 2, "{out} {err}");
    assert!(err.contains("Markdown file budget"), "{err}");
}

fn doctor(src: &Path) -> (i32, String) {
    let home = src.join("home");
    fs::create_dir_all(&home).expect("home");
    let (code, out, err) = isolated("skill-doctor", &[], src, src, &[("HOME", utf8(&home))]);
    (code, format!("{out}{err}"))
}

#[test]
fn skill_doctor_recursive_references() {
    let tmp = TempDir::new("skillrefs");
    let src = tmp.path();
    let dir = src.join("custom-skills/demo");
    write_all(
        src,
        &[
            ("custom-skills/demo/SKILL.md", GOOD_SKILL),
            (
                "custom-skills/demo/references/a.md",
                "[b](b.md#x)\n[gen](x.generated.md)\n",
            ),
            (
                "custom-skills/demo/references/b.md",
                "[a](a.md)\n[web](https://e.com/a.md)\n",
            ),
            (
                "custom-skills/demo/references/z.generated.md",
                "generated, unlinked\n",
            ),
        ],
    );
    let (code, text) = doctor(src);
    assert_eq!(code, 0, "{text}");

    fs::write(dir.join("references/b.md"), "[m](missing.md)\n").expect("w");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("fail: missing local reference: custom-skills/demo/references/missing.md")
    );

    fs::write(dir.join("references/b.md"), "ok\n").expect("w");
    write_all(
        src,
        &[("custom-skills/demo/references/deep/orphan.md", "x\n")],
    );
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("orphan local reference: custom-skills/demo/references/deep/orphan.md"));
    assert!(!text.contains("z.generated.md"), "{text}");
    fs::remove_dir_all(dir.join("references/deep")).expect("rm");

    fs::write(
        dir.join("SKILL.md"),
        format!("{GOOD_SKILL}[e](references/../../evil.md)\n"),
    )
    .expect("w");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("unsafe local reference: custom-skills/demo/references/../../evil.md"));

    // A symlinked references/ root is refused, never walked.
    fs::write(dir.join("SKILL.md"), GOOD_SKILL).expect("w");
    fs::create_dir_all(src.join("elsewhere")).expect("mkdir");
    fs::write(src.join("elsewhere/a.md"), "ok\n").expect("w");
    let saved = src.join("saved-references");
    fs::rename(dir.join("references"), &saved).expect("mv");
    std::os::unix::fs::symlink(src.join("elsewhere"), dir.join("references")).expect("ln");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("symlink local reference: custom-skills/demo/references"),
        "{text}"
    );
    fs::remove_file(dir.join("references")).expect("rm");
    fs::rename(&saved, dir.join("references")).expect("mv");

    // An unreadable references/ subdirectory is reported, not skipped.
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.join("references/locked");
        fs::create_dir_all(&locked).expect("mkdir");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        let readable = fs::read_dir(&locked).is_ok(); // root ignores modes
        let (code, text) = doctor(src);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
        if !readable {
            assert_eq!(code, 1, "{text}");
            assert!(text.contains("unreadable skill reference"), "{text}");
        }
        fs::remove_dir_all(&locked).expect("rm");
    }

    // Unlinked symlink entries under references/ are refused too: a .generated.md file link
    // and a directory link, neither mentioned by any markdown.
    fs::write(src.join("outside.md"), "x\n").expect("w");
    fs::create_dir_all(src.join("elsewhere")).expect("mkdir");
    fs::write(dir.join("references/a.md"), "[b](b.md)\n").expect("w");
    fs::write(dir.join("references/b.md"), "ok\n").expect("w");
    let (code, text) = doctor(src);
    assert_eq!(code, 0, "baseline before symlinks: {text}");
    std::os::unix::fs::symlink(
        src.join("outside.md"),
        dir.join("references/x.generated.md"),
    )
    .expect("ln");
    std::os::unix::fs::symlink(src.join("elsewhere"), dir.join("references/sub")).expect("ln");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("symlink local reference: custom-skills/demo/references/x.generated.md"),
        "{text}"
    );
    assert!(
        text.contains("symlink local reference: custom-skills/demo/references/sub"),
        "{text}"
    );
    fs::remove_file(dir.join("references/x.generated.md")).expect("rm");
    fs::remove_file(dir.join("references/sub")).expect("rm");
    fs::write(dir.join("references/a.md"), "[b](b.md#x)\n").expect("w");

    fs::write(src.join("outside.md"), "x\n").expect("w");
    fs::write(dir.join("references/a.md"), "[l](link.md)\n").expect("w");
    std::os::unix::fs::symlink(src.join("outside.md"), dir.join("references/link.md")).expect("ln");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("symlink local reference: custom-skills/demo/references/link.md"));
}

#[test]
fn skill_doctor_checks_templates_and_marketplace() {
    let tmp = TempDir::new("skilltpl");
    let src = tmp.path();
    let market = |source: &str, skills: &str, strict: &str| {
        format!(
            "{{\"name\":\"m\",\"owner\":{{\"name\":\"o\"}},\"plugins\":[{{\"name\":\"p\",\"source\":\"{source}\",\"strict\":{strict},\"description\":\"needs the oms command\",\"skills\":{skills}}}]}}"
        )
    };
    write_all(
        src,
        &[
            ("custom-skills/demo/SKILL.md", GOOD_SKILL),
            ("custom-skills/demo/references/a.md", "ok\n"),
            (
                "skills.manifest.json",
                "{\"skills\":[{\"name\":\"demo\",\"source\":\"custom-skills/demo\"}]}",
            ),
            (
                ".claude-plugin/marketplace.json",
                &market("./", "[\"./custom-skills/demo\"]", "false"),
            ),
            (
                "templates/project-skills/tpl/SKILL.md",
                &GOOD_SKILL.replace("demo", "tpl"),
            ),
            ("templates/project-skills/tpl/references/a.md", "ok\n"),
        ],
    );
    let (code, text) = doctor(src);
    assert_eq!(code, 0, "{text}");

    write_all(
        src,
        &[("templates/project-skills/tpl/references/orphan.md", "x\n")],
    );
    fs::write(
        src.join("templates/project-skills/tpl/references/a.md"),
        "[m](gone.md)\n",
    )
    .expect("w");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("orphan local reference: templates/project-skills/tpl/references/orphan.md")
    );
    assert!(
        text.contains("missing local reference: templates/project-skills/tpl/references/gone.md")
    );
    fs::remove_dir_all(src.join("templates")).expect("rm");

    for (body, needle) in [
        (
            market(
                "./",
                "[\"./custom-skills/demo\",\"./custom-skills/extra\"]",
                "false",
            ),
            "no SKILL.md: custom-skills/extra",
        ),
        (
            market("./", "[\"./custom-skills/demo\"]", "true"),
            "needs \"strict\": false",
        ),
        (market("../out", "[]", "false"), "relative ./ source"),
        (
            market("./", "[]", "false"),
            "enabled skill is not offered by any plugin: custom-skills/demo",
        ),
    ] {
        fs::write(src.join(".claude-plugin/marketplace.json"), body).expect("w");
        let (code, text) = doctor(src);
        assert_eq!(code, 1, "{text}");
        assert!(text.contains(needle), "{needle}: {text}");
    }
    // With a marketplace present, an unreadable or malformed manifest fails closed.
    fs::write(
        src.join(".claude-plugin/marketplace.json"),
        market("./", "[\"./custom-skills/demo\"]", "false"),
    )
    .expect("w");
    for body in ["{", ""] {
        fs::write(src.join("skills.manifest.json"), body).expect("w");
        let (code, text) = doctor(src);
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("invalid skills.manifest.json"), "{text}");
    }
    fs::remove_file(src.join("skills.manifest.json")).expect("rm");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("invalid skills.manifest.json"), "{text}");
    fs::write(src.join(".claude-plugin/marketplace.json"), "{").expect("w");
    let (code, text) = doctor(src);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("invalid .claude-plugin/marketplace.json"),
        "{text}"
    );
}

#[test]
fn skill_forge_validate_enforces_references() {
    let tmp = TempDir::new("forgerefs");
    let proj = tmp.path().join("proj");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).expect("home");
    write_all(
        &proj,
        &[
            (".oms/skills/demo/SKILL.md", GOOD_SKILL),
            (".oms/skills/demo/references/a.md", "ok\n"),
        ],
    );
    let run = || {
        isolated(
            "skill-forge",
            &["validate", "demo"],
            &proj,
            &oms_source(),
            &[("HOME", utf8(&home))],
        )
    };
    let (code, out, err) = run();
    assert_eq!(code, 0, "{out}{err}");
    write_all(&proj, &[(".oms/skills/demo/references/orphan.md", "x\n")]);
    let (code, out, err) = run();
    assert_eq!(code, 1, "{out}{err}");
    fs::remove_file(proj.join(".oms/skills/demo/references/orphan.md")).expect("rm");
    fs::write(
        proj.join(".oms/skills/demo/references/a.md"),
        "[m](nope.md)\n",
    )
    .expect("w");
    let (code, out, err) = run();
    assert_eq!(code, 1, "{out}{err}");
}

fn forge(proj: &Path, home: &Path, args: &[&str]) -> (i32, String) {
    let (code, out, err) = isolated(
        "skill-forge",
        args,
        proj,
        &oms_source(),
        &[("HOME", utf8(home))],
    );
    (code, format!("{out}{err}"))
}

fn named_skill(name: &str, body_tail: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: Demo skill used only by the parity tests for reference checks.\n---\n{body_tail}"
    )
}

#[test]
fn desktop_marketplaces_package_the_same_project_workflow() {
    let root = oms_source();
    let load = |path: PathBuf| -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).expect("read plugin JSON")).expect("plugin JSON")
    };
    let mut workflows = Vec::new();
    for (catalog, command) in [(root.clone(), "oms"), (root.join("plugins/oms-ssh"), "ssh")] {
        let market = load(catalog.join(".agents/plugins/marketplace.json"));
        let entry = market["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "oms")
            .unwrap();
        let package = catalog
            .join(entry["source"]["path"].as_str().unwrap())
            .canonicalize()
            .unwrap();
        assert!(package.starts_with(&catalog));
        let manifest = load(package.join("plugin.json"));
        assert_eq!(manifest["name"], "oms");
        let setup = &manifest["extensions"]["com.openai"]["onboardingSkill"];
        assert!(package.join(setup.as_str().unwrap()).is_file());
        let servers = load(package.join("mcp.json"));
        assert_eq!(servers["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(servers["mcpServers"]["oms-panel"]["command"], command);
        workflows.push(fs::read(package.join("skills/oms-project/SKILL.md")).unwrap());
    }
    // Each transport is a self-contained import, including on Windows.
    assert_eq!(workflows[0], workflows[1]);
}

#[test]
fn bundled_skill_validation_covers_enabled_source_catalog() {
    let source = oms_source();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("skills.manifest.json")).expect("manifest"))
            .expect("manifest JSON");
    let enabled: Vec<_> = manifest["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .filter(|row| row["enabled"] == true)
        .collect();
    assert!(!enabled.is_empty(), "bundled catalog must be exercised");
    for row in enabled {
        let name = row["name"].as_str().expect("skill name");
        assert_eq!(row["source"], format!("custom-skills/{name}"));
        validate_bundled_skill(&source, name).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn bundled_skill_validation_reuses_contract_and_reference_checks() {
    let tmp = TempDir::new("bundled-contract");
    let source = tmp.path();
    let directory = source.join("custom-skills/oms-demo");
    let valid = named_skill(
        "oms-demo",
        "[a](references/a.md)\n[repository](../../docs/guide.md)\n",
    );
    write_all(
        source,
        &[
            ("custom-skills/oms-demo/SKILL.md", &valid),
            (
                "custom-skills/oms-demo/references/a.md",
                "[b](b.md#anchor)\n[generated](future.generated.md)\n",
            ),
            ("custom-skills/oms-demo/references/b.md", "[a](a.md)\n"),
        ],
    );
    // Existing non-managed repository links, cycles and generated references stay valid.
    validate_bundled_skill(source, "oms-demo").expect("valid bundled references");
    for (body, expected) in [
        ("no frontmatter\n".into(), "missing frontmatter"),
        (named_skill("oms-other", ""), "name must match directory"),
        (
            "---\nname: oms-demo\ndescription: Short.\n---\n".into(),
            "description too thin",
        ),
        (
            named_skill("oms-demo", &"line\n".repeat(501)),
            "500-line body budget",
        ),
        (
            valid.replace("name: oms-demo", "name: oms-demo\nverify: true"),
            "non-portable frontmatter",
        ),
        (
            named_skill("oms-demo", "[escape](references/../../outside.md)\n"),
            "unsafe local reference",
        ),
    ] {
        fs::write(directory.join("SKILL.md"), body).expect("invalid skill");
        let error = validate_bundled_skill(source, "oms-demo").expect_err("must refuse");
        assert!(error.contains(expected), "{error}");
    }
    fs::write(directory.join("SKILL.md"), &valid).expect("restore skill");
    fs::write(directory.join("references/b.md"), "[missing](missing.md)\n")
        .expect("broken recursive link");
    let error = validate_bundled_skill(source, "oms-demo").expect_err("missing reference");
    assert!(error.contains("missing local reference"), "{error}");
    fs::write(directory.join("references/b.md"), "[a](a.md)\n").expect("restore reference");
    fs::write(directory.join("references/orphan.md"), "unlinked\n").expect("orphan");
    let error = validate_bundled_skill(source, "oms-demo").expect_err("orphan reference");
    assert!(error.contains("orphan local reference"), "{error}");
    fs::remove_file(directory.join("references/orphan.md")).expect("remove orphan");
    validate_bundled_skill(source, "oms-demo").expect("restored bundle");
    assert!(
        !source.join(".oms").exists(),
        "static validation writes no state"
    );
}

#[test]
fn bundled_skill_validation_stays_under_its_source_owner() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new("bundled-owner");
    let source = tmp.path();
    let directory = source.join("custom-skills/oms-demo");
    write_all(
        source,
        &[(
            "custom-skills/oms-demo/SKILL.md",
            &named_skill("oms-demo", "plain body\n"),
        )],
    );
    for invalid in ["demo", "../oms-demo", "oms-demo/other", "/oms-demo"] {
        assert!(validate_bundled_skill(source, invalid).is_err());
    }
    // Parent and bundle aliases may not redirect the managed tree.
    let saved = source.join("saved-skills");
    fs::rename(source.join("custom-skills"), &saved).expect("save parent");
    symlink(&saved, source.join("custom-skills")).expect("parent symlink");
    assert!(validate_bundled_skill(source, "oms-demo").is_err());
    fs::remove_file(source.join("custom-skills")).expect("remove parent symlink");
    fs::rename(&saved, source.join("custom-skills")).expect("restore parent");
    let saved = source.join("saved-skill");
    fs::rename(&directory, &saved).expect("save bundle");
    symlink(&saved, &directory).expect("bundle symlink");
    assert!(validate_bundled_skill(source, "oms-demo").is_err());
    fs::remove_file(&directory).expect("remove bundle symlink");
    fs::rename(&saved, &directory).expect("restore bundle");

    // Even an unlinked child cannot introduce a symlink or a hardlink alias.
    let outside = source.join("outside.md");
    fs::write(&outside, "outside\n").expect("outside file");
    let alias = directory.join("unlinked.md");
    symlink(&outside, &alias).expect("child symlink");
    assert!(validate_bundled_skill(source, "oms-demo").is_err());
    fs::remove_file(&alias).expect("remove child symlink");
    fs::hard_link(&outside, &alias).expect("child hardlink");
    assert!(validate_bundled_skill(source, "oms-demo").is_err());
    fs::remove_file(&alias).expect("remove child hardlink");
    validate_bundled_skill(source, "oms-demo").expect("restored owner");
}

#[test]
fn skill_forge_refuses_bad_references_before_mutation_and_in_lifecycle() {
    let tmp = TempDir::new("forgelife");
    let proj = tmp.path().join("proj");
    let home = tmp.path().join("home");
    fs::create_dir_all(&proj).expect("proj");
    fs::create_dir_all(&home).expect("home");

    // add: bad or traversal links are refused before any durable write.
    for tail in ["[m](references/nope.md)\n", "[t](references/../../x.md)\n"] {
        let file = tmp.path().join("bad.md");
        fs::write(&file, named_skill("oms-demo", tail)).expect("w");
        let (code, text) = forge(
            &proj,
            &home,
            &["add", "--name", "oms-demo", "--file", utf8(&file)],
        );
        assert_eq!(code, 2, "{text}");
        assert!(!proj.join(".oms/skills").exists(), "add mutated state");
        assert!(!proj.join(".oms/skill-admissions.jsonl").exists());
    }
    // add with no references succeeds and links.
    let file = tmp.path().join("ok.md");
    fs::write(&file, named_skill("oms-demo", "plain body\n")).expect("w");
    let (code, text) = forge(
        &proj,
        &home,
        &["add", "--name", "oms-demo", "--file", utf8(&file)],
    );
    assert_eq!(code, 0, "{text}");
    let link = proj.join(".agents/skills/oms-demo");
    assert!(fs::symlink_metadata(&link).is_ok());

    // link withdraws the skill once its references are invalid (orphan, broken, traversal).
    let skill = proj.join(".oms/skills/oms-demo");
    for (tail, extra) in [
        ("plain body\n", Some("references/orphan.md")),
        ("[m](references/nope.md)\n", None),
        ("[t](references/../../x.md)\n", None),
    ] {
        fs::write(skill.join("SKILL.md"), named_skill("oms-demo", tail)).expect("w");
        if let Some(extra) = extra {
            write_all(&skill, &[(extra, "x\n")]);
        }
        let (code, text) = forge(&proj, &home, &["link"]);
        assert_eq!(code, 0, "{text}");
        assert!(fs::symlink_metadata(&link).is_err(), "link kept for {tail}");
        assert!(fs::symlink_metadata(proj.join(".claude/skills/oms-demo")).is_err());
        let (code, text) = forge(&proj, &home, &["status"]);
        assert_eq!(code, 1, "{text}");
        let _ = fs::remove_dir_all(skill.join("references"));
    }
    fs::write(
        skill.join("SKILL.md"),
        named_skill("oms-demo", "plain body\n"),
    )
    .expect("w");
    let (code, text) = forge(&proj, &home, &["link"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        fs::symlink_metadata(&link).is_ok(),
        "valid skill is relinked"
    );

    // eval --candidate rejects a candidate with a broken reference.
    let suite = tmp.path().join("suite.json");
    fs::write(
        &suite,
        r#"{"schema":1,"skill":"oms-demo","router":["true"]}"#,
    )
    .expect("w");
    let cand = tmp.path().join("cand");
    write_all(
        &cand,
        &[(
            "SKILL.md",
            &named_skill("oms-demo", "[m](references/nope.md)\n"),
        )],
    );
    let (code, text) = forge(
        &proj,
        &home,
        &[
            "eval",
            "oms-demo",
            "--suite",
            utf8(&suite),
            "--candidate",
            utf8(&cand),
        ],
    );
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("missing local reference"), "{text}");

    // preview / import / update reject; a clean bundle imports and validate labels the store path.
    let bundle = tmp.path().join("bundle");
    write_all(
        &bundle,
        &[
            (
                "SKILL.md",
                &named_skill("oms-imp", "[a](references/a.md)\n"),
            ),
            ("references/a.md", "ok\n"),
            ("references/orphan.md", "x\n"),
        ],
    );
    for command in ["preview", "import", "update"] {
        let (code, text) = forge(&proj, &home, &[command, "--source", utf8(&bundle)]);
        assert_ne!(code, 0, "{command}: {text}");
        assert!(text.contains("orphan local reference"), "{command}: {text}");
    }
    fs::remove_file(bundle.join("references/orphan.md")).expect("rm");
    let (code, text) = forge(
        &proj,
        &home,
        &["preview", "--source", utf8(&bundle), "--json"],
    );
    assert_eq!(code, 0, "{text}");
    let sha =
        serde_json::from_str::<serde_json::Value>(text.trim()).expect("json")["bundle_sha256"]
            .as_str()
            .expect("sha")
            .to_owned();
    let (code, text) = forge(
        &proj,
        &home,
        &[
            "import",
            "--source",
            utf8(&bundle),
            "--apply",
            "--expected-bundle-sha256",
            &sha,
        ],
    );
    assert_eq!(code, 0, "{text}");
    let (code, text) = forge(&proj, &home, &["validate", "oms-imp"]);
    assert_eq!(code, 0, "{text}");
    let lock = fs::read_to_string(proj.join(".oms/skill-store/oms-imp/lock.json")).expect("lock");
    let revision = serde_json::from_str::<serde_json::Value>(&lock).expect("json")["revision_path"]
        .as_str()
        .expect("revision")
        .to_owned();
    // The immutable revision cannot drift: tampering fails validation outright.
    fs::write(proj.join(&revision).join("references/late.md"), "x\n").expect("w");
    let (code, text) = forge(&proj, &home, &["validate", "oms-imp"]);
    assert_ne!(code, 0, "{text}");
}

const PI_ARGS: [&str; 4] = ["pi", "--model", "gpt-5.5", "--dry-run"];

fn tree_is_empty(path: &Path) -> bool {
    fs::read_dir(path).expect("read").next().is_none()
}

#[test]
fn harness_dry_run_is_pure_and_refused_in_workers() {
    let tmp = TempDir::new("harness");
    let (home, cache) = (tmp.path().join("home"), tmp.path().join("cache"));
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&cache).expect("cache");
    let env = [("HOME", utf8(&home)), ("XDG_CACHE_HOME", utf8(&cache))];
    for host in ["codex", "claude"] {
        let (code, out, err) = isolated(
            "harness",
            &[host, "--model", "gpt-5.5", "--dry-run"],
            tmp.path(),
            &oms_source(),
            &env,
        );
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains("--extension"), "{out}");
    }
    let (code, out, err) = isolated("harness", &PI_ARGS, tmp.path(), &oms_source(), &env);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("pi-codex-extension.mjs") && out.contains("oh-my-setting/harness/"),
        "{out}"
    );
    assert!(!out.contains("scripts/lib/pi-oms-extension.mjs"), "{out}");
    assert!(!out.contains("scripts/lib/pi-codex-extension.mjs"), "{out}");
    let (code, out, err) = isolated("harness", &PI_ARGS, tmp.path(), tmp.path(), &env);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("scripts/lib"), "{out}");
    assert_eq!(out.matches("oh-my-setting/harness/").count(), 2, "{out}");
    assert!(
        tree_is_empty(&cache) && tree_is_empty(&home),
        "dry-run wrote files"
    );
    let (code, _, err) = isolated(
        "harness",
        &["pi", "--model", "latest", "--dry-run"],
        tmp.path(),
        &oms_source(),
        &env,
    );
    assert_ne!(code, 0);
    assert!(err.contains("exact model ID"), "{err}");

    // codex/claude never need HOME; pi needs it only when XDG_CACHE_HOME is not absolute.
    let no_home = [("HOME", ""), ("XDG_CACHE_HOME", utf8(&cache))];
    for host in ["codex", "claude", "pi"] {
        let (code, out, err) = isolated(
            "harness",
            &[host, "--model", "gpt-5.5", "--dry-run"],
            tmp.path(),
            &oms_source(),
            &no_home,
        );
        assert_eq!(code, 0, "{host}: {err}");
        assert_eq!(out.contains("--extension"), host == "pi", "{host}: {out}");
    }
    let bare = [("HOME", ""), ("XDG_CACHE_HOME", "")];
    for host in ["codex", "claude"] {
        let (code, _, err) = isolated(
            "harness",
            &[host, "--model", "gpt-5.5", "--dry-run"],
            tmp.path(),
            &oms_source(),
            &bare,
        );
        assert_eq!(code, 0, "{host}: {err}");
    }
    let (code, _, err) = isolated("harness", &PI_ARGS, tmp.path(), &oms_source(), &bare);
    assert_ne!(code, 0);
    assert!(err.contains("HOME"), "{err}");
    for name in ["OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH"] {
        let mut worker = env.to_vec();
        worker.push((name, "1"));
        let (code, _, err) = isolated("harness", &PI_ARGS, tmp.path(), &oms_source(), &worker);
        assert_eq!(code, 2, "{name}: {err}");
    }
    assert!(tree_is_empty(&cache));
}

#[test]
fn python_layout_and_doctor() {
    if let Some(target) = std::env::var_os("OMS_TEST_PYTHON_DRY") {
        let target = PathBuf::from(target);
        let (code, out, err) = call(
            "apply-project-template",
            &[
                "general",
                target.to_str().unwrap(),
                "--python-layout",
                "--no-private",
            ],
            target.parent().unwrap(),
        );
        assert_eq!(code, 0, "{out} {err}");
        assert!(out.contains("would ensure"));
        assert!(!target.exists());
        return;
    }
    let dry = TempDir::new("python-dry");
    let target = dry.path().join("missing");
    let result = Command::new(std::env::current_exe().unwrap())
        .env("OMS_TEST_PYTHON_DRY", &target)
        .env("OH_MY_SETTING_DRY_RUN", "1")
        .args(["--exact", "python_layout_and_doctor"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!target.exists());
    let edges = TempDir::new("python-edges");
    let root = edges.path();
    let long = format!("a{}", "b".repeat(63));
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--package", &long, "--no-private"],
            root
        )
        .0,
        2
    );
    assert_eq!(
        call(
            "apply-project-template",
            &[
                "general",
                ".",
                "--python-layout",
                "--package",
                &format!("{long}b"),
                "--no-private"
            ],
            root
        )
        .0,
        2
    );
    write_all(root, &[("tests/existing_test.py", "existing")]);
    assert_eq!(
        call(
            "apply-project-template",
            &[
                "general",
                ".",
                "--python-layout",
                "--package",
                &long,
                "--no-private"
            ],
            root
        )
        .0,
        0
    );
    assert!(!root.join("tests/test_smoke.py").exists());
    assert!(
        fs::read_to_string(root.join("PROJECT.md"))
            .unwrap()
            .contains("bash scripts/check.sh fast tests/existing_test.py")
    );
    let blocked = TempDir::new("python-link");
    std::os::unix::fs::symlink(root, blocked.path().join("src")).unwrap();
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            blocked.path()
        )
        .0,
        2
    );
    assert!(!blocked.path().join("PROJECT.md").exists());
    let tmp = TempDir::new("python-layout");
    let repo = tmp.path();
    let (code, out, err) = call(
        "apply-project-template",
        &[
            "general",
            ".",
            "--python-layout",
            "--package",
            "my_ai",
            "--full-docs",
            "--no-private",
        ],
        repo,
    );
    assert_eq!(code, 0, "{out} {err}");
    for path in [
        "pyproject.toml",
        "src/my_ai/__init__.py",
        "src/my_ai/py.typed",
        "tests/test_smoke.py",
        "scripts/check.sh",
        ".gitignore",
        "PROJECT.md",
        "docs/ARCHITECTURE.md",
        "docs/DECISIONS.md",
        ".oms/skills/oms-python-ai/SKILL.md",
    ] {
        assert!(repo.join(path).is_file(), "missing {path}");
    }
    assert!(repo.join(".agents/skills/oms-python-ai").exists());
    let pyproj = fs::read_to_string(repo.join("pyproject.toml")).unwrap();
    assert!(
        pyproj.contains("[dependency-groups]")
            && pyproj.contains("\"pytest\"")
            && pyproj.contains("\"ruff\""),
        "{pyproj}"
    );
    let spec_body = fs::read_to_string(repo.join("PROJECT.md")).unwrap();
    assert!(
        spec_body.contains("uv sync")
            && spec_body.contains("bash scripts/check.sh fast tests/test_smoke.py"),
        "{spec_body}"
    );
    assert!(
        spec_body.contains("docs/ARCHITECTURE.md, docs/DECISIONS.md"),
        "{spec_body}"
    );
    let ml_tmp = TempDir::new("python-ml-full-docs");
    let ml_root = ml_tmp.path();
    let (code, out, err) = call(
        "apply-project-template",
        &[
            "ml",
            ".",
            "--python-layout",
            "--package",
            "ml_pkg",
            "--full-docs",
            "--no-private",
        ],
        ml_root,
    );
    assert_eq!(code, 0, "{out} {err}");
    let ml_spec = fs::read_to_string(ml_root.join("PROJECT.md")).unwrap();
    assert!(!ml_spec.contains("docs/ARCHITECTURE.md"), "{ml_spec}");
    fs::write(repo.join("pyproject.toml"), "keep").unwrap();
    assert_eq!(
        call(
            "apply-project-template",
            &[
                "general",
                ".",
                "--python-layout",
                "--package",
                "my_ai",
                "--no-private"
            ],
            repo
        )
        .0,
        0
    );
    assert_eq!(
        fs::read_to_string(repo.join("pyproject.toml")).unwrap(),
        "keep"
    );
    for name in [
        "",
        "123bad",
        "bad-name",
        "trailing_",
        "annotationlib",
        "base64",
        "bisect",
        "class",
        "compileall",
        "for",
        "if",
        "json",
        "lambda",
        "match",
        "src",
    ] {
        assert_eq!(
            call(
                "apply-project-template",
                &[
                    "general",
                    ".",
                    "--python-layout",
                    "--package",
                    name,
                    "--no-private"
                ],
                repo
            )
            .0,
            2,
            "{name}"
        );
    }
    let (_, out, _) = call("project-doctor", &["."], repo);
    assert!(
        out.contains("ok: Python package sources live under src/")
            && out.contains("ok: docs/ hierarchy is shallow"),
        "{out}"
    );
    fs::remove_dir_all(repo.join("src")).unwrap();
    write_all(
        repo,
        &[
            ("docs/nested/spec.md", "x"),
            ("stray.py", "x"),
            ("stray.md", "x"),
        ],
    );
    let (_, out, _) = call("project-doctor", &["."], repo);
    for text in [
        "warn: nested markdown under docs/",
        "warn: pyproject.toml present without src/",
        "warn: top-level python files",
        "warn: markdown outside docs/",
    ] {
        assert!(out.contains(text), "missing {text}: {out}");
    }
    let non_ascii = repo.join("프로젝트");
    fs::create_dir_all(&non_ascii).unwrap();
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            &non_ascii
        )
        .0,
        0
    );
    assert!(non_ascii.join("src/pkg/__init__.py").is_file());
    let bad_gitignore = repo.join("bad_gitignore");
    fs::create_dir_all(&bad_gitignore).unwrap();
    fs::write(bad_gitignore.join(".gitignore"), [0xff, 0xfe]).unwrap();
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            &bad_gitignore
        )
        .0,
        2
    );
    assert!(!bad_gitignore.join("PROJECT.md").exists());
    assert!(!bad_gitignore.join("pyproject.toml").exists());

    let uni_tests = repo.join("uni_tests");
    write_all(
        &uni_tests,
        &[("tests/test_모델.py", "def test_u(): pass\n")],
    );
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            &uni_tests
        )
        .0,
        0
    );
    assert!(!uni_tests.join("tests/test_smoke.py").exists());
    assert!(
        fs::read_to_string(uni_tests.join("PROJECT.md"))
            .unwrap()
            .contains("bash scripts/check.sh fast 'tests/test_모델.py'")
    );

    let foreign_skill = repo.join("foreign_skill");
    write_all(
        &foreign_skill,
        &[(".agents/skills/oms-python-ai", "foreign")],
    );
    let foreign = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &foreign_skill,
    );
    assert_eq!(foreign.0, 2);
    assert!(
        foreign
            .2
            .contains("foreign project skill entry blocks link")
            && foreign.2.contains("nothing was changed"),
        "{}",
        foreign.2
    );
    assert!(!foreign_skill.join("PROJECT.md").exists());
    assert!(!foreign_skill.join("pyproject.toml").exists());
    assert!(!foreign_skill.join("AGENTS.md").exists());

    let gemini_dir = repo.join("gemini_ok");
    write_all(&gemini_dir, &[("GEMINI.md", "")]);
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            &gemini_dir
        )
        .0,
        0
    );
    let (_, gout, _) = call("project-doctor", &["."], &gemini_dir);
    assert!(
        gout.contains("ok: markdown files live under docs/")
            && !gout.contains("warn: markdown outside docs/"),
        "{gout}"
    );

    let deep_docs = repo.join("deep_docs");
    write_all(&deep_docs, &[("docs/a/b/c/d/e/spec.md", "deep")]);
    let (_, dout, _) = call("project-doctor", &["."], &deep_docs);
    assert!(dout.contains("warn: nested markdown under docs/"), "{dout}");

    let ci_layout = repo.join("ci_layout");
    write_all(
        &ci_layout,
        &[
            (
                "pyproject.toml",
                "[project]\nname = \"ci_layout\"\nversion = \"0.1.0\"\n",
            ),
            ("uv.lock", "version = 1\n"),
            ("tests/test_ci.py", "def test_ci(): pass\n"),
        ],
    );
    let ci_res = call(
        "apply-project-template",
        &[
            "general",
            ".",
            "--python-layout",
            "--python-ci",
            "--no-private",
        ],
        &ci_layout,
    );
    assert_eq!(ci_res.0, 0, "{} {}", ci_res.1, ci_res.2);
    assert!(ci_layout.join(".github/workflows/python-ci.yml").is_file());
    assert!(ci_layout.join("scripts/check.sh").is_file());

    let imported = TempDir::new("python-imported-skill");
    let imp_root = imported.path();
    let imp_home = TempDir::new("python-imported-home");
    let imp_bundle = imp_home.path().join("bundle");
    write_all(
        &imp_bundle,
        &[(
            "SKILL.md",
            &named_skill("oms-python-ai", "Validate imported Python AI layout.\n"),
        )],
    );
    let (code, text) = forge(
        imp_root,
        imp_home.path(),
        &["preview", "--source", utf8(&imp_bundle), "--json"],
    );
    assert_eq!(code, 0, "{text}");
    let sha = serde_json::from_str::<serde_json::Value>(text.trim()).unwrap()["bundle_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let (code, text) = forge(
        imp_root,
        imp_home.path(),
        &[
            "import",
            "--source",
            utf8(&imp_bundle),
            "--apply",
            "--expected-bundle-sha256",
            &sha,
        ],
    );
    assert_eq!(code, 0, "{text}");
    let imp_apply = call(
        "apply-project-template",
        &[
            "general",
            ".",
            "--python-layout",
            "--package",
            "imp_pkg",
            "--no-private",
        ],
        imp_root,
    );
    assert_eq!(imp_apply.0, 0, "{} {}", imp_apply.1, imp_apply.2);
    assert!(!imp_root.join(".oms/skills/oms-python-ai").exists());
    assert!(
        imp_root
            .join(".oms/skill-store/oms-python-ai/lock.json")
            .is_file()
    );
    assert!(imp_root.join(".agents/skills/oms-python-ai").exists());

    let anno_dir = repo.join("annotationlib");
    fs::create_dir_all(&anno_dir).unwrap();
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            &anno_dir
        )
        .0,
        0
    );
    assert!(anno_dir.join("src/pkg_annotationlib/__init__.py").is_file());

    let bad_skill_dir = repo.join("bad_skill_dir");
    write_all(&bad_skill_dir, &[(".agents/skills", "file")]);
    let bad_res = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &bad_skill_dir,
    );
    assert_eq!(bad_res.0, 2);
    assert!(bad_res.2.contains("nothing was changed"), "{}", bad_res.2);
    assert!(!bad_skill_dir.join("PROJECT.md").exists());
    assert!(!bad_skill_dir.join("pyproject.toml").exists());
    assert!(!bad_skill_dir.join("AGENTS.md").exists());

    let bad_oms_git = repo.join("bad_oms_git");
    fs::create_dir_all(bad_oms_git.join(".oms/.gitignore")).unwrap();
    let bad_git_res = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &bad_oms_git,
    );
    assert_eq!(bad_git_res.0, 2);
    assert!(
        bad_git_res.2.contains("nothing was changed"),
        "{}",
        bad_git_res.2
    );
    assert!(!bad_oms_git.join("PROJECT.md").exists());
    assert!(!bad_oms_git.join("pyproject.toml").exists());
    assert!(!bad_oms_git.join("AGENTS.md").exists());

    let bad_local_skill = repo.join("bad_local_skill");
    write_all(&bad_local_skill, &[(".oms/skills/oms-python-ai", "file")]);
    let bad_local_res = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &bad_local_skill,
    );
    assert_eq!(bad_local_res.0, 2);
    assert!(
        bad_local_res.2.contains("nothing was changed"),
        "{}",
        bad_local_res.2
    );
    assert!(!bad_local_skill.join("PROJECT.md").exists());
    assert!(!bad_local_skill.join("pyproject.toml").exists());

    let mono = repo.join("monorepo");
    fs::create_dir_all(&mono).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&mono)
            .status()
            .unwrap()
            .success()
    );
    let nested = mono.join("packages/sub_ai");
    let first_nested = call(
        "apply-project-template",
        &[
            "general",
            "packages/sub_ai",
            "--python-layout",
            "--no-private",
        ],
        &mono,
    );
    assert_eq!(first_nested.0, 0, "{} {}", first_nested.1, first_nested.2);
    assert!(nested.join("src/sub_ai/__init__.py").is_file());
    assert!(nested.join(".oms/skills/oms-python-ai/SKILL.md").is_file());
    assert!(nested.join(".agents/skills/oms-python-ai").exists());
    assert!(nested.join(".claude/skills/oms-python-ai").exists());
    assert!(!mono.join(".oms").exists());
    assert!(!mono.join(".agents").exists());
    assert!(!mono.join(".claude").exists());
    assert!(!mono.join("PROJECT.md").exists());
    assert!(!mono.join("pyproject.toml").exists());
    for link_rel in [
        "packages/sub_ai/.agents/skills/oms-python-ai",
        "packages/sub_ai/.claude/skills/oms-python-ai",
    ] {
        assert!(
            Command::new("git")
                .args(["-C", utf8(&mono), "check-ignore", "-q", link_rel])
                .status()
                .unwrap()
                .success(),
            "expected {link_rel} to be ignored by enclosing git repo"
        );
    }
    let rerun_nested = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &nested,
    );
    assert_eq!(rerun_nested.0, 0, "{} {}", rerun_nested.1, rerun_nested.2);
    assert!(!mono.join(".oms").exists());
    assert!(!mono.join(".agents").exists());

    // Whitespace, Git pattern metacharacters, link repair on rerun, and prefix-matching subdirs.
    for sub in ["packages/my ai", "packages/[demo]"] {
        let sub_dir = mono.join(sub);
        let res = call(
            "apply-project-template",
            &["general", sub, "--python-layout"],
            &mono,
        );
        assert_eq!(res.0, 0, "{sub}: {} {}", res.1, res.2);
        assert!(sub_dir.join(".agents/skills/oms-python-ai").exists());
        assert!(sub_dir.join(".claude/skills/oms-python-ai").exists());
        for rel in [
            format!("{sub}/AGENTS.md"),
            format!("{sub}/.agents/skills/oms-python-ai"),
            format!("{sub}/.claude/skills/oms-python-ai"),
        ] {
            assert!(
                Command::new("git")
                    .args(["-C", utf8(&mono), "check-ignore", "-q", &rel])
                    .status()
                    .unwrap()
                    .success(),
                "expected {rel} to be ignored by enclosing git repo"
            );
        }
        let st = call("project-private", &["status", "--check"], &sub_dir);
        assert_eq!(st.0, 0, "{sub}: {} {}", st.1, st.2);
    }
    let space_dir = mono.join("packages/my ai");
    fs::remove_file(space_dir.join(".claude/skills/oms-python-ai")).unwrap();
    let repair = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &space_dir,
    );
    assert_eq!(repair.0, 0, "{} {}", repair.1, repair.2);
    assert!(space_dir.join(".claude/skills/oms-python-ai").exists());

    let pkg_dir = mono.join("packages");
    write_all(&mono, &[("packages/packages/x", "local\n")]);
    assert_eq!(
        call(
            "project-private",
            &["apply", "--path", "packages/x"],
            &pkg_dir
        )
        .0,
        0
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                utf8(&mono),
                "check-ignore",
                "-q",
                "packages/packages/x"
            ])
            .status()
            .unwrap()
            .success()
    );

    // Literal pathspecs keep --untrack scoped to packages/[demo] without untracking packages/d.
    write_all(
        &mono,
        &[
            ("packages/d/AGENTS.md", "keep\n"),
            ("packages/[demo]/AGENTS.md", "drop\n"),
        ],
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                utf8(&mono),
                "--literal-pathspecs",
                "add",
                "-f",
                "--",
                "packages/d/AGENTS.md",
                "packages/[demo]/AGENTS.md",
            ])
            .status()
            .unwrap()
            .success()
    );
    let untrack = call(
        "project-private",
        &["apply", "--untrack"],
        &mono.join("packages/[demo]"),
    );
    assert_eq!(untrack.0, 0, "{} {}", untrack.1, untrack.2);
    let tracked_sibling = Command::new("git")
        .args([
            "-C",
            utf8(&mono),
            "--literal-pathspecs",
            "ls-files",
            "--",
            "packages/d/AGENTS.md",
        ])
        .output()
        .unwrap();
    assert!(!tracked_sibling.stdout.is_empty());
    let tracked_target = Command::new("git")
        .args([
            "-C",
            utf8(&mono),
            "--literal-pathspecs",
            "ls-files",
            "--",
            "packages/[demo]/AGENTS.md",
        ])
        .output()
        .unwrap();
    assert!(tracked_target.stdout.is_empty());

    // Scoped privacy removal unignores only that subdirectory (or --path) and preserves siblings.
    let scoped_rm = call(
        "project-private",
        &["remove"],
        &mono.join("packages/[demo]"),
    );
    assert_eq!(scoped_rm.0, 0, "{} {}", scoped_rm.1, scoped_rm.2);
    for removed_rel in [
        "packages/[demo]/AGENTS.md",
        "packages/[demo]/.agents/skills/oms-python-ai",
    ] {
        assert!(
            !Command::new("git")
                .args(["-C", utf8(&mono), "check-ignore", "-q", removed_rel])
                .status()
                .unwrap()
                .success(),
            "expected {removed_rel} to be unignored after scoped remove"
        );
    }
    for kept_rel in [
        "packages/my ai/AGENTS.md",
        "packages/my ai/.agents/skills/oms-python-ai",
        "packages/packages/x",
    ] {
        assert!(
            Command::new("git")
                .args(["-C", utf8(&mono), "check-ignore", "-q", kept_rel])
                .status()
                .unwrap()
                .success(),
            "expected sibling {kept_rel} to stay ignored"
        );
    }
    assert_eq!(
        call("project-private", &["status", "--check"], &space_dir).0,
        0
    );
    let path_rm = call(
        "project-private",
        &["remove", "--path", "packages/x"],
        &pkg_dir,
    );
    assert_eq!(path_rm.0, 0, "{} {}", path_rm.1, path_rm.2);
    assert!(
        !Command::new("git")
            .args([
                "-C",
                utf8(&mono),
                "check-ignore",
                "-q",
                "packages/packages/x"
            ])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        call("project-private", &["status", "--check"], &space_dir).0,
        0
    );

    // Trailing spaces in a subdirectory name must escape the same way on scoped remove as apply.
    let trail_sub = "packages/model ";
    let trail_dir = mono.join(trail_sub);
    let trail_apply = call(
        "apply-project-template",
        &["general", trail_sub, "--python-layout"],
        &mono,
    );
    assert_eq!(trail_apply.0, 0, "{} {}", trail_apply.1, trail_apply.2);
    write_all(&trail_dir, &[("custom.txt", "keep ignored else\n")]);
    assert_eq!(
        call(
            "project-private",
            &["apply", "--path", "custom.txt"],
            &trail_dir
        )
        .0,
        0
    );
    for rel in [
        "packages/model /AGENTS.md",
        "packages/model /.agents/skills/oms-python-ai",
        "packages/model /custom.txt",
    ] {
        assert!(
            Command::new("git")
                .args(["-C", utf8(&mono), "check-ignore", "-q", rel])
                .status()
                .unwrap()
                .success(),
            "expected {rel} to be ignored before trailing-space remove"
        );
    }
    let trail_rm = call("project-private", &["remove"], &trail_dir);
    assert_eq!(trail_rm.0, 0, "{} {}", trail_rm.1, trail_rm.2);
    for rel in [
        "packages/model /AGENTS.md",
        "packages/model /.agents/skills/oms-python-ai",
        "packages/model /custom.txt",
    ] {
        assert!(
            !Command::new("git")
                .args(["-C", utf8(&mono), "check-ignore", "-q", rel])
                .status()
                .unwrap()
                .success(),
            "expected {rel} to be unignored after trailing-space remove"
        );
    }
    assert_eq!(
        call("project-private", &["status", "--check"], &space_dir).0,
        0
    );

    // Repair-only reruns preflight broken provider links before mutating template files.
    let repair_refuse = repo.join("repair_refuse");
    fs::create_dir_all(&repair_refuse).unwrap();
    assert_eq!(
        call(
            "apply-project-template",
            &["general", ".", "--python-layout", "--no-private"],
            &repair_refuse,
        )
        .0,
        0
    );
    fs::remove_file(repair_refuse.join("PROJECT.md")).unwrap();
    fs::remove_file(repair_refuse.join(".agents/skills/oms-python-ai")).unwrap();
    fs::remove_file(repair_refuse.join(".claude/skills/oms-python-ai")).unwrap();
    fs::write(
        repair_refuse.join(".agents/skills/oms-python-ai"),
        "foreign",
    )
    .unwrap();
    let refused_repair = call(
        "apply-project-template",
        &["general", ".", "--python-layout", "--no-private"],
        &repair_refuse,
    );
    assert_eq!(refused_repair.0, 2);
    assert!(
        refused_repair.2.contains("nothing was changed"),
        "{}",
        refused_repair.2
    );
    assert!(!repair_refuse.join("PROJECT.md").exists());
}
