//! Fake sbatch/squeue/sacct scenarios. Each call re-executes this test binary
//! with a scrubbed environment because `set_var` is unsafe and the crate forbids it.
#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);
// Writing an executable while another thread forks yields ETXTBSY (126).
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());
const SEP: char = '\u{1f}';

/// Entry point for re-executed children; a no-op in the normal test run.
#[test]
fn child_entry() {
    let Some(cwd) = std::env::var_os("OMS_ST_CWD") else {
        return;
    };
    let raw = std::env::var("OMS_ST_ARGS").unwrap_or_default();
    let args: Vec<OsString> = raw.split(SEP).map(OsString::from).collect();
    let flag = AtomicBool::new(false);
    let marker = std::env::var_os("OMS_ST_INTERRUPT_ON").map(PathBuf::from);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let cwd = PathBuf::from(cwd);
    let done = AtomicBool::new(false);
    let code = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if marker.as_ref().is_some_and(|m| m.exists()) {
                    flag.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let code = oms_command_slurm_submit::run_with_source_streaming(
            &args, &cwd, &cwd, &flag, &mut out, &mut err,
        );
        done.store(true, Ordering::Release);
        code
    });
    let esc = |b: Vec<u8>| {
        String::from_utf8_lossy(&b)
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
    };
    println!("ST_CODE={code}");
    println!("ST_OUT={}", esc(out));
    println!("ST_ERR={}", esc(err));
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
struct Outcome {
    code: i32,
    out: String,
    err: String,
}
impl Fixture {
    fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("oms-slurm-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "T"],
            vec!["config", "user.email", "t@example.invalid"],
        ] {
            assert!(
                Command::new("git")
                    .args(&args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(repo.join("job.sh"), "#!/bin/sh\necho hi\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-q", "-m", "init"]] {
            assert!(
                Command::new("git")
                    .args(&args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::create_dir_all(root.join("fake")).unwrap();
        let fixture = Self { root, repo };
        fixture.set("squeue", "");
        fixture.set("sacct", "");
        fixture.script("sbatch", "echo \"Submitted batch job 4242\"\n");
        fixture.script("squeue", "cat \"$FAKE_DIR/squeue.out\"\n");
        fixture.script("sacct", "cat \"$FAKE_DIR/sacct.out\"\n");
        fixture
    }
    fn fake(&self) -> PathBuf {
        self.root.join("fake")
    }
    fn script(&self, name: &str, body: &str) {
        let _serial = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.fake().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn set(&self, name: &str, content: &str) {
        fs::write(self.fake().join(format!("{name}.out")), content).unwrap();
    }
    fn ledger(&self) -> String {
        fs::read_to_string(self.repo.join(".oms/slurm-submits.jsonl")).unwrap_or_default()
    }
    fn run(&self, args: &[&str], extra: &[(&str, String)]) -> Outcome {
        let exe = std::env::current_exe().unwrap();
        let mut command = Command::new(exe);
        command
            .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("LC_ALL", "C")
            .env("FAKE_DIR", self.fake())
            .env("OMS_ST_CWD", &self.repo)
            .env("OMS_ST_ARGS", args.join(&SEP.to_string()))
            .env("OMS_SLURM_SBATCH_BIN", self.fake().join("sbatch"))
            .env("OMS_SLURM_SQUEUE_BIN", self.fake().join("squeue"))
            .env("OMS_SLURM_SACCT_BIN", self.fake().join("sacct"))
            .current_dir(&self.repo);
        for (k, v) in extra {
            command.env(k, v);
        }
        let _serial = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let output = command.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.find(k).map(|i| l[i + k.len()..].to_owned()))
                .unwrap_or_else(|| {
                    panic!(
                        "missing {k}: {text}\n{}",
                        String::from_utf8_lossy(&output.stderr)
                    )
                })
        };
        let unescape = |s: String| {
            let (mut out, mut it) = (String::new(), s.chars().peekable());
            while let Some(c) = it.next() {
                match (c, it.peek().copied()) {
                    ('\\', Some('n')) => {
                        out.push('\n');
                        it.next();
                    }
                    ('\\', Some('\\')) => {
                        out.push('\\');
                        it.next();
                    }
                    _ => out.push(c),
                }
            }
            out
        };
        Outcome {
            code: field("ST_CODE=").parse().unwrap(),
            out: unescape(field("ST_OUT=")),
            err: unescape(field("ST_ERR=")),
        }
    }
    fn submit(&self, key: &str, extra: &[&str]) -> Outcome {
        let mut args = vec!["--key", key];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--", "sbatch", "job.sh"]);
        self.run(&args, &[])
    }
    /// Scenario 5: the scheduler queues the job, then OMS is cut off before
    /// sbatch answers. The fake records the job and blocks until interrupted.
    fn lost_reply(&self, key: &str) -> Outcome {
        let marker = self.root.join("queued");
        self.script(
            "sbatch",
            &format!("touch '{}'\nexec sleep 30\n", marker.display()),
        );
        let out = self.run(
            &["--key", key, "--", "sbatch", "job.sh"],
            &[("OMS_ST_INTERRUPT_ON", marker.display().to_string())],
        );
        assert_eq!(out.code, 130, "{}", out.err);
        out
    }
}
fn json_field(text: &str, name: &str) -> String {
    let needle = format!("\"{name}\": \"");
    text.find(&needle).map_or_else(String::new, |at| {
        let rest = &text[at + needle.len()..];
        rest[..rest.find('"').unwrap()].to_owned()
    })
}
fn short_of(key: &str) -> String {
    // sha256(key)[..12], matching the --comment oms-key=<short> that sbatch received.
    format!("{:x}", Sha256::digest(key.as_bytes()))[..12].to_owned()
}

#[test]
fn lost_sbatch_reply_is_recovered_from_the_scheduler_and_blocks_a_resubmit() {
    let f = Fixture::new();
    f.lost_reply("cell-5");
    assert!(f.ledger().contains("\"type\": \"intent\""));
    assert!(!f.ledger().contains("\"type\": \"submitted\""));

    // slurmdbd has not caught up and squeue is empty: unknown, never absent.
    let r = f.run(&["reconcile", "--key", "cell-5", "--json"], &[]);
    assert_eq!(r.code, 75, "{}{}", r.out, r.err);
    assert_eq!(json_field(&r.out, "state"), "unknown");
    assert!(!f.ledger().contains("\"type\": \"submitted\""));

    // The queued job becomes visible under the comment OMS attached.
    let short = short_of("cell-5");
    f.set("squeue", &format!("4242|RUNNING|job.sh|oms-key={short}\n"));
    let r = f.run(&["reconcile", "--key", "cell-5", "--json"], &[]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(json_field(&r.out, "state"), "active");
    assert_eq!(json_field(&r.out, "job_id"), "4242");
    assert!(r.out.contains("\"recovered\": true"), "{}", r.out);
    let ledger = f.ledger();
    assert!(ledger.contains("\"type\": \"submitted\""));
    assert!(ledger.contains("\"reconciled\": true"));

    // A second submit with the same key is a duplicate, and sbatch is not run.
    f.script("sbatch", "echo ran >> \"$FAKE_DIR/second-sbatch\"\n");
    let again = f.submit("cell-5", &[]);
    assert_eq!(again.code, 3, "{}", again.err);
    assert!(again.err.contains("already submitted as job 4242"));
    assert!(!f.fake().join("second-sbatch").exists());
}

#[test]
fn terminal_states_never_map_to_success_and_lag_or_ambiguity_is_unknown() {
    let f = Fixture::new();
    assert_eq!(f.submit("cell-7", &["--task-id", "t-1"]).code, 0);
    let table = [
        ("COMPLETED|0:0", "succeeded", 0),
        ("RUNNING|0:0", "active", 0),
        ("PENDING|0:0", "active", 0),
        ("COMPLETED|1:0", "failed", 1),
        ("OUT_OF_MEMORY|0:125", "oom", 1),
        ("PREEMPTED|0:0", "preempted", 1),
        ("TIMEOUT|0:0", "timeout", 1),
        ("CANCELLED by 1000|0:0", "cancelled", 1),
        ("NODE_FAIL|1:0", "failed", 1),
        ("FUTURE_STATE|0:0", "unknown", 75),
    ];
    for (state, class, code) in table {
        f.set("sacct", &format!("4242|job.sh|{state}|oms-key=x\n"));
        let r = f.run(&["reconcile", "--job-id", "4242", "--json"], &[]);
        assert_eq!(
            (r.code, json_field(&r.out, "state")),
            (code, class.to_owned()),
            "{state}: {}{}",
            r.out,
            r.err
        );
    }
    // Empty squeue and sacct (accounting lag) is unknown too.
    f.set("sacct", "");
    let r = f.run(&["reconcile", "--job-id", "4242", "--json"], &[]);
    assert_eq!(r.code, 75, "{}", r.out);

    // A failing sacct is no evidence: unknown, never absent or success.
    f.script("sacct", "exit 1\n");
    let r = f.run(&["reconcile", "--key", "cell-7", "--json"], &[]);
    assert_eq!(r.code, 75, "{}", r.out);
    assert!(json_field(&r.out, "sacct").starts_with("failed"));

    // Two distinct jobs carrying one key tag are ambiguous and write nothing.
    let lost = Fixture::new();
    lost.lost_reply("cell-amb");
    let short = short_of("cell-amb");
    lost.set(
        "sacct",
        &format!("10|j|COMPLETED|0:0|oms-key={short}\n11|j|COMPLETED|0:0|oms-key={short}\n"),
    );
    let r = lost.run(&["reconcile", "--key", "cell-amb", "--json"], &[]);
    assert_eq!(r.code, 75, "{}", r.out);
    assert!(json_field(&r.out, "reason").starts_with("ambiguous"));
    assert!(!lost.ledger().contains("\"type\": \"submitted\""));
}

#[test]
fn terminal_reconcile_is_recorded_once_and_provenance_is_kept() {
    let f = Fixture::new();
    let out = f.run(
        &[
            "--key",
            "cell-p",
            "--attempt-id",
            "a-9",
            "--exec-snapshot",
            &"ab".repeat(32),
            "--",
            "sbatch",
            "job.sh",
        ],
        &[("OMS_TASK_ID", "task-env".into())],
    );
    assert_eq!(out.code, 0, "{}", out.err);
    let intent = f.ledger();
    assert!(intent.contains("\"task_id\": \"task-env\""));
    assert!(intent.contains("\"attempt_id\": \"a-9\""));
    assert!(intent.contains(&format!(
        "\"exec_snapshot_sha256\": \"{}\"",
        "ab".repeat(32)
    )));

    f.set("sacct", "4242|job.sh|OUT_OF_MEMORY|0:125|x\n");
    for _ in 0..2 {
        let r = f.run(&["reconcile", "--key", "cell-p"], &[]);
        assert_eq!(r.code, 1, "{}{}", r.out, r.err);
        assert!(r.out.starts_with("reconcile: oom job 4242"));
    }
    let ledger = f.ledger();
    assert_eq!(ledger.matches("\"type\": \"reconciled\"").count(), 1);
    assert!(ledger.contains("\"state\": \"oom\""));
    // Reconciled rows carry the intent's provenance and the key stays blocked.
    assert_eq!(ledger.matches("\"task_id\": \"task-env\"").count(), 2);
    assert_eq!(f.submit("cell-p", &[]).code, 3);
}

#[test]
fn key_aliases_and_provenance_flags_are_validated() {
    let f = Fixture::new();
    let bad = f.run(
        &[
            "--key",
            "a",
            "--idempotency-key",
            "b",
            "--",
            "sbatch",
            "job.sh",
        ],
        &[],
    );
    assert_eq!(bad.code, 2);
    assert!(bad.err.contains("disagree"));
    let bad = f.run(
        &[
            "--task-id",
            "has space",
            "--key",
            "a",
            "--",
            "sbatch",
            "job.sh",
        ],
        &[],
    );
    assert_eq!(bad.code, 2);
    let ok = f.run(
        &[
            "--idempotency-key",
            "same",
            "--key",
            "same",
            "--",
            "sbatch",
            "job.sh",
        ],
        &[("OMS_TASK_ID", "not safe!".into())],
    );
    assert_eq!(ok.code, 0, "{}", ok.err);
    assert!(!f.ledger().contains("task_id"));
    assert_eq!(f.run(&["reconcile"], &[]).code, 2);
    assert_eq!(
        f.run(&["reconcile", "--key", "never-submitted"], &[]).code,
        2
    );
    assert!(Path::new(&f.repo).join(".oms").exists());
}

#[test]
fn forced_resubmit_never_adopts_earlier_attempt_and_job_id_keeps_its_attempt() {
    let f = Fixture::new();
    // Attempt 1 submits job 101 cleanly.
    f.script("sbatch", "printf 'Submitted batch job 101\\n'\n");
    let first = f.submit("cell-f", &[]);
    assert_eq!(first.code, 0, "{}", first.err);

    // Forced resubmit Attempt 2 loses its sbatch reply (intent only, no submitted row).
    let marker = f.root.join("queued-f2");
    f.script(
        "sbatch",
        &format!("touch '{}'\nexec sleep 30\n", marker.display()),
    );
    let second = f.run(
        &[
            "--key",
            "cell-f",
            "--force-resubmit",
            "--reason",
            "retry",
            "--",
            "sbatch",
            "job.sh",
        ],
        &[("OMS_ST_INTERRUPT_ON", marker.display().to_string())],
    );
    assert_eq!(second.code, 130);

    // Historical sacct contains Attempt 1's completed job 101 with the same key tag.
    let short = short_of("cell-f");
    f.set(
        "sacct",
        &format!("101|job.sh|COMPLETED|0:0|oms-key={short}\n"),
    );
    // reconcile --key targets Attempt 2 and must NOT steal Attempt 1's job 101.
    let rec2 = f.run(&["reconcile", "--key", "cell-f", "--json"], &[]);
    assert_eq!(rec2.code, 75, "{}{}", rec2.out, rec2.err);
    assert_eq!(json_field(&rec2.out, "state"), "unknown");

    // reconcile --job-id 101 targets Attempt 1 and preserves Attempt 2 unresolved.
    let attempt1 = f
        .ledger()
        .lines()
        .find(|l| l.contains("\"job_id\": \"101\""))
        .map(|l| json_field(l, "attempt"))
        .unwrap();
    let rec1 = f.run(&["reconcile", "--job-id", "101", "--json"], &[]);
    assert_eq!(rec1.code, 0, "{}{}", rec1.out, rec1.err);
    assert_eq!(json_field(&rec1.out, "attempt"), attempt1);
    assert_eq!(json_field(&rec1.out, "state"), "succeeded");
    assert_eq!(f.submit("cell-f", &[]).code, 3);
}

#[test]
fn cluster_flag_is_recorded_and_forwarded_to_scheduler_queries() {
    let f = Fixture::new();
    let squeue_log = f.root.join("squeue.args");
    let sacct_log = f.root.join("sacct.args");
    f.script(
        "squeue",
        &format!("printf '%s\\n' \"$@\" > '{}'\n", squeue_log.display()),
    );
    f.script(
        "sacct",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf '505|job.sh|COMPLETED|0:0|oms-key=%s\\n' \"{}\"\n",
            sacct_log.display(),
            short_of("cell-c")
        ),
    );
    let marker = f.root.join("queued-c");
    f.script(
        "sbatch",
        &format!("touch '{}'\nexec sleep 30\n", marker.display()),
    );
    let lost = f.run(
        &[
            "--key",
            "cell-c",
            "--",
            "sbatch",
            "-M",
            "h100-clus",
            "job.sh",
        ],
        &[("OMS_ST_INTERRUPT_ON", marker.display().to_string())],
    );
    assert_eq!(lost.code, 130);
    assert!(f.ledger().contains("\"cluster_name\": \"h100-clus\""));

    let rec = f.run(&["reconcile", "--key", "cell-c", "--json"], &[]);
    assert_eq!(rec.code, 0, "{}{}", rec.out, rec.err);
    assert!(
        fs::read_to_string(&squeue_log)
            .unwrap()
            .contains("-M\nh100-clus\n")
    );
    assert!(
        fs::read_to_string(&sacct_log)
            .unwrap()
            .contains("-M\nh100-clus\n")
    );
    let submitted_line = f
        .ledger()
        .lines()
        .find(|l| l.contains("\"type\": \"submitted\""))
        .unwrap()
        .to_owned();
    assert!(submitted_line.contains("\"cluster_name\": \"h100-clus\""));
}
