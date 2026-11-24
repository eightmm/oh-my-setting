#![forbid(unsafe_code)]

use serde_json::{Value, json};
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        for _ in 0..64 {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{prefix}-{pid}-{nonce:x}-{seq}"));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("create exclusive temp dir {}: {err}", path.display()),
            }
        }
        panic!("exhausted exclusive temp dir attempts for {prefix}");
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

fn mkdir_private(path: &Path) {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("create private dir");
}

fn configure_private_env(cmd: &mut Command, root: &Path) {
    let base_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned());
    let dirs = ["home", "config", "data", "state", "cache", "locks"].map(|d| root.join(d));
    for dir in &dirs {
        mkdir_private(dir);
    }
    cmd.env_clear()
        .env("PATH", base_path)
        .env("HOME", &dirs[0])
        .env("XDG_CONFIG_HOME", &dirs[1])
        .env("XDG_DATA_HOME", &dirs[2])
        .env("XDG_STATE_HOME", &dirs[3])
        .env("XDG_CACHE_HOME", &dirs[4])
        .env("OMS_LOCK_DIR", &dirs[5])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "oms-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "oms-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com");
}

fn run_git(repo: &Path, root: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    configure_private_env(&mut cmd, root);
    let out = cmd.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn oms_json(root: &Path, args: &[&str]) -> Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_oms"));
    cmd.args(args);
    configure_private_env(&mut cmd, root);
    let out = cmd.output().expect("spawn oms");
    assert!(
        out.status.success(),
        "oms {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("parse oms JSON")
}

fn oms_cmd(root: &Path, spec: &str, repo: &str) -> Value {
    let mut args: Vec<&str> = spec.split_whitespace().collect();
    args.extend(["--repo", repo, "--json"]);
    oms_json(root, &args)
}

fn input_add(root: &Path, repo: &str, mode: &str, text: &str) -> Value {
    oms_json(
        root,
        &[
            "input", "add", "--repo", repo, "--room", "r1", "--from", "main", "--to", "w1",
            "--mode", mode, "--text", text, "--json",
        ],
    )
}

fn init_fixture_repo(root: &Path, name: &str) -> String {
    let repo = root.join(name);
    mkdir_private(&repo);
    run_git(&repo, root, &["init", "-q"]);
    run_git(&repo, root, &["checkout", "-qb", "main"]);
    fs::write(repo.join("README.md"), "verify-smoke\n").expect("write README");
    run_git(&repo, root, &["add", "README.md"]);
    run_git(&repo, root, &["commit", "-qm", "init"]);

    let repo_s = repo.to_str().expect("utf8 repo").to_owned();
    let r = oms_cmd(root, "room new --id r1 --topic smoke", &repo_s);
    assert_eq!(r["id"].as_str(), Some("r1"));
    let _ = oms_cmd(
        root,
        "room join --id r1 --participant main --provider codex --role main",
        &repo_s,
    );
    let _ = oms_cmd(
        root,
        "room join --id r1 --participant w1 --provider claude --role worker --parent main",
        &repo_s,
    );
    repo_s
}

fn active_ids(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter(|i| i["status"].as_str() == Some("queued"))
        .map(|i| i["id"].as_str().expect("id").to_owned())
        .collect()
}

fn run_offline_payload(root: &Path) -> Value {
    let repo = init_fixture_repo(root, "fx1");
    let a1 = input_add(root, &repo, "steer", "offline-1");
    let a2 = input_add(root, &repo, "next", "offline-2");
    let (id1, id2) = (
        a1["id"].as_str().expect("id1"),
        a2["id"].as_str().expect("id2"),
    );
    assert!(id1.starts_with("input-") && id2.starts_with("input-"));
    assert_eq!(
        (a1["mode"].as_str(), a2["mode"].as_str()),
        (Some("steer"), Some("next"))
    );

    let list = oms_cmd(root, "input list", &repo);
    assert_eq!(list["schema"].as_i64(), Some(1));
    let (rev, ids) = (
        list["revision"].as_i64().expect("revision"),
        active_ids(&list),
    );
    assert_eq!(
        (rev, ids.as_slice()),
        (2, &[id1.to_owned(), id2.to_owned()][..])
    );

    let pv = oms_json(root, &["input", "preview", "--repo", &repo, "--json", id1]);
    assert_eq!(
        (pv["id"].as_str(), pv["room"].as_str()),
        (Some(id1), Some("r1"))
    );
    assert_eq!(pv["executes_model"].as_bool(), Some(false));
    assert!(pv["bytes"].as_i64().unwrap_or(0) > 0);
    let sha = pv["payload_sha256"].as_str().expect("sha256");
    assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()));

    let cnt = ids.len() as i64;
    json!({
        "fixture_id": "offline-cli-input-queue-v1", "ok": true, "stage": "core_e2e",
        "expected": { "item_count": 2, "revision": 2 },
        "actual": { "item_count": cnt, "revision": rev },
        "deltas": { "item_count": cnt - 2, "revision": rev - 2 },
        "error": null
    })
}

fn run_seeded_trial(root: &Path, name: &str, seed: u64) -> (Vec<String>, i64, i64) {
    let repo = init_fixture_repo(root, name);
    let (mut state, mut rev) = (seed, 0_i64);
    let mut next = |bound: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 32) as usize) % bound
    };
    let (mut model, mut plan): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());

    for step in 1..=6 {
        match if model.is_empty() { 0 } else { next(5) } {
            0 | 1 => {
                let mode = if next(2) == 0 { "steer" } else { "next" };
                let row = input_add(root, &repo, mode, &format!("s{step}-{seed}"));
                model.push(row["id"].as_str().expect("id").to_owned());
                rev += 1;
                plan.push(format!("add:{mode}"));
            }
            2 => {
                let _ = oms_cmd(root, "input list", &repo);
                plan.push("list".to_owned());
            }
            3 => {
                let idx = next(model.len());
                let id = model.remove(idx);
                let _ = oms_json(root, &["input", "cancel", "--repo", &repo, "--json", &id]);
                rev += 1;
                plan.push(format!("cancel:{idx}"));
            }
            _ => {
                let (src, dst) = (next(model.len()), next(model.len()));
                if src == dst {
                    let id = model.remove(src);
                    let _ = oms_json(root, &["input", "move", "--repo", &repo, "--json", &id]);
                    model.push(id);
                    plan.push(format!("move:{src}->end"));
                } else {
                    let (src_id, dst_id) = (model[src].clone(), model[dst].clone());
                    let _ = oms_json(
                        root,
                        &[
                            "input", "move", "--repo", &repo, "--json", "--before", &dst_id,
                            &src_id,
                        ],
                    );
                    let moved = model.remove(src);
                    let pos = model.iter().position(|x| x == &dst_id).expect("dst");
                    model.insert(pos, moved);
                    plan.push(format!("move:{src}->{dst}"));
                }
                rev += 1;
            }
        }
    }

    let list = oms_cmd(root, "input list", &repo);
    assert_eq!(active_ids(&list), model, "ordered active IDs mismatch");
    assert_eq!(list["revision"].as_i64(), Some(rev), "revision mismatch");
    (plan, model.len() as i64, rev)
}

#[test]
fn verify_core_e2e() {
    let seed: u64 = std::env::var("OMS_VERIFY_SEED")
        .ok()
        .filter(|s| !s.is_empty())
        .map_or(20_261_123, |s| s.parse().expect("valid OMS_VERIFY_SEED"));
    let report_path = std::env::var_os("OMS_VERIFY_REPORT").filter(|s| !s.is_empty());

    let work = TempDir::new("oms-verify-e2e");
    let fx1 = run_offline_payload(work.path());
    let (plan_a, cnt_a, rev_a) = run_seeded_trial(work.path(), "fx2-a", seed);
    let (plan_b, cnt_b, rev_b) = run_seeded_trial(work.path(), "fx2-b", seed);
    assert_eq!((&plan_a, cnt_a, rev_a), (&plan_b, cnt_b, rev_b));

    let fx2 = json!({
        "fixture_id": "seeded-queue-invariant-v1", "ok": true, "stage": "core_e2e", "seed": seed,
        "expected": { "item_count": cnt_a, "revision": rev_a, "repeat_match": 1 },
        "actual": { "item_count": cnt_b, "revision": rev_b, "repeat_match": 1 },
        "deltas": { "item_count": cnt_b - cnt_a, "revision": rev_b - rev_a, "repeat_match": 0 },
        "error": null
    });
    let report = json!({
        "schema": 1, "ok": true, "stage": "core_e2e", "seed": seed,
        "checks_executed": 2, "fixtures": [fx1, fx2]
    });

    if let Some(dest) = report_path {
        let dest = PathBuf::from(dest);
        let tmp = dest.with_extension("json.tmp");
        let text = format!("{}\n", serde_json::to_string_pretty(&report).expect("json"));
        fs::write(&tmp, text).expect("write report");
        fs::rename(&tmp, &dest).expect("rename report");
    }
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn verify_fail_fast_and_zero_test_negative_controls() {
    let verify_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../verify");
    let work = TempDir::new("oms-verify-neg");
    let (repo, bin) = (work.path().join("repo"), work.path().join("bin"));
    mkdir_private(&repo.join("scripts"));
    mkdir_private(&bin);
    let verify = repo.join("verify");
    fs::copy(&verify_src, &verify).expect("copy verify");
    fs::set_permissions(&verify, fs::Permissions::from_mode(0o755)).expect("chmod");

    let run = |args: &[&str]| {
        let base_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned());
        let mut cmd = Command::new("bash");
        cmd.arg(&verify).args(args);
        configure_private_env(&mut cmd, work.path());
        cmd.env("PATH", format!("{}:{base_path}", bin.display()))
            .env("TMPDIR", work.path());
        cmd.output().expect("run verify")
    };

    assert_eq!(run(&["--help"]).status.code(), Some(0));
    assert_eq!(run(&["--bogus"]).status.code(), Some(2));

    let m1 = repo.join("lint-ran");
    write_exec(
        &bin.join("cargo"),
        "#!/usr/bin/env bash\ncase \"${2:-}\" in check) exit 42 ;; *) exit 0 ;; esac\n",
    );
    write_exec(
        &repo.join("scripts/check.sh"),
        &format!("#!/usr/bin/env bash\ntouch '{}'\nexit 0\n", m1.display()),
    );
    let o1 = run(&[]);
    assert_eq!(o1.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&o1.stderr).contains("stage=type") && !m1.exists());

    let m2 = repo.join("test-ran");
    write_exec(
        &bin.join("cargo"),
        &format!(
            "#!/usr/bin/env bash\ncase \"${{2:-}}\" in check) exit 0 ;; test) touch '{}'; exit 0 ;; esac\n",
            m2.display()
        ),
    );
    write_exec(
        &repo.join("scripts/check.sh"),
        "#!/usr/bin/env bash\nexit 17\n",
    );
    let o2 = run(&[]);
    assert_eq!(o2.status.code(), Some(17));
    assert!(
        String::from_utf8_lossy(&o2.stderr).contains("stage=architecture_lint") && !m2.exists()
    );

    write_exec(&bin.join("cargo"), "#!/usr/bin/env bash\nexit 0\n");
    write_exec(
        &repo.join("scripts/check.sh"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
    let o3 = run(&[]);
    let err3 = String::from_utf8_lossy(&o3.stderr);
    assert_eq!(o3.status.code(), Some(1));
    assert!(err3.contains("stage=core_e2e") && err3.contains("empty_or_zero_test_smoke_report"));
}
