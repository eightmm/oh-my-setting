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
    // A legacy source tree (extension + Python helper) keeps its own pi-oms extension until
    // `oms harness-hook` is wired; the codex extension is always the embedded copy.
    let legacy = oms_source().join("scripts/lib/hook_state.py").is_file();
    assert_eq!(
        out.contains("scripts/lib/pi-oms-extension.mjs"),
        legacy,
        "{out}"
    );
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
