use super::*;
use crate::value::*;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let n = SERIAL.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "oms-runs-{}-{}-{n}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        Self(p.canonicalize().unwrap())
    }
    fn write(&self, name: &str, body: impl AsRef<[u8]>) {
        let p = self.0.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn executable(&self, name: &str, body: &str) {
        self.write(name, body);
        fs::set_permissions(self.0.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn context<'a>(&self, flag: &'a AtomicBool) -> Context<'a> {
        let mut c = Context::new(&self.0, flag).unwrap();
        self.executable("bin/git", "#!/bin/sh\ncase \"$*\" in *--show-toplevel) pwd -P;; 'rev-parse --git-dir') echo .git;; 'rev-parse --verify --quiet HEAD') exit 1;; 'status --porcelain --untracked-files=no') exit 0;; *) exit 128;; esac\n");
        let path = std::env::var_os("PATH").unwrap_or_default();
        c.env.insert(
            "PATH".into(),
            std::env::join_paths(
                std::iter::once(self.0.join("bin")).chain(std::env::split_paths(&path)),
            )
            .unwrap(),
        );
        c.lock_settings.lock_dir = Some(self.0.join("locks"));
        c.lock_settings.mode = oms_lock::Mode::Mkdir;
        c.lock_settings.timeout = Duration::from_secs(oms_lock::legacy_seconds(
            std::env::var_os("OMS_TEST_CALL_TIMEOUT").as_deref(),
            30,
        ));
        for (k, v) in [
            ("OMS_WORK_JOURNAL", "0"),
            ("OMS_RUN_ID", ""),
            ("OMS_RUN_LEDGER_GATE", "1"),
            ("OMS_RUN_LEDGER_GATE_REASON", ""),
            ("OMS_RUN_LEDGER_CHECK_MODE", ""),
            ("OMS_RUN_LEDGER_STATUS_OVERRIDE", ""),
            ("OMS_RESEARCH_METADATA_FILE", ""),
            ("OMS_OPERATION_ID", ""),
            ("OMS_METRICS_MAX_BYTES", "65536"),
            ("SLURM_JOB_ID", ""),
        ] {
            c.env.insert(k.into(), v.into());
        }
        c
    }
    fn invoke(
        &self,
        command: &str,
        args: &[&str],
        ctx: &Context<'_>,
        svc: &mut dyn Services,
    ) -> (i32, Vec<u8>, Vec<u8>) {
        let mut out = vec![];
        let mut err = vec![];
        let code = run(
            command,
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            ctx,
            svc,
            &mut out,
            &mut err,
        );
        (code, out, err)
    }
    fn rows(&self, name: &str) -> Vec<Json> {
        let mut out = vec![];
        store::rows(&self.0.join(name), false, |r| {
            out.push(r);
            Ok(())
        })
        .unwrap();
        out
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Recorder {
    events: Vec<String>,
    notes: Vec<String>,
    digests: usize,
    fail_digest: bool,
}
impl Recorder {
    fn new() -> Self {
        Self {
            events: vec![],
            notes: vec![],
            digests: 0,
            fail_digest: false,
        }
    }
}
impl Services for Recorder {
    fn available(&self, _: Service) -> bool {
        true
    }
    fn observe(&mut self, o: Observation<'_>) -> Result<()> {
        self.events
            .push(format!("{}:{}", o.source_type, s(o.row, "id")));
        Ok(())
    }
    fn memory(&mut self, n: MemoryNote<'_>) -> Result<()> {
        self.notes.push(n.text.into());
        Ok(())
    }
    fn link(&mut self, _: RunLink<'_>) -> Result<()> {
        Ok(())
    }
    fn digest(&mut self, r: DigestRequest<'_>) -> Result<Vec<u8>> {
        assert!(!r.accounting.stdout.is_empty());
        assert_eq!(
            r.accounting.format,
            AccountingFormat::SacctAllocationParsable2
        );
        assert!(
            r.accounting
                .columns
                .starts_with("JobID,State,ExitCode,Elapsed,")
        );
        self.digests += 1;
        if self.fail_digest {
            Err(Error::io("fake digest failed"))
        } else {
            Ok(format!("# job {}\n", s(r.job, "job_id")).into_bytes())
        }
    }
}
#[test]
fn scalar_metrics_boundaries_and_raw_json() {
    assert!(metrics::sanitize(b"{}").unwrap().is_none());
    assert!(metrics::sanitize(b"[]").is_err());
    assert!(metrics::sanitize(b"{").is_err());
    let v=metrics::sanitize(br#"{"yes":true,"good":1.5,"large":123456789012345678901234567890,"bad":NaN,"inf":Infinity,"label":"a\nb\tc","nested":{}}"#).unwrap().unwrap();
    assert_eq!(s(&v, "label"), "abc");
    assert_eq!(s(&v, "large"), "123456789012345678901234567890");
    assert!(get(&v, "bad").is_none());
    assert!(get(&v, "inf").is_none());
    assert!(get(&v, "nested").is_none());
    let raw = format!(
        "{{{}}}",
        (0..51)
            .map(|i| format!("\"k{i}\":\"{}\"", "é".repeat(201)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let v = metrics::sanitize(raw.as_bytes()).unwrap().unwrap();
    let Json::Object(o) = &v else { panic!() };
    assert_eq!(o.len(), 50);
    assert_eq!(s(&v, "k0").chars().count(), 200);
    let raw = OsString::from_vec(vec![b'a', 0xff, b'z']);
    let v = os(&raw);
    let raw = ordered(&v).unwrap();
    assert_eq!(raw, br#""a\udcffz""#);
    assert_eq!(parse(&raw).unwrap(), v);
}
#[test]
fn seeds_contracts_and_group_reruns() {
    let cmd = Json::Array(vec![
        string("train"),
        string("--seed"),
        string("1"),
        string("seed=2"),
    ]);
    let (seed, g) = metrics::seed_and_group(&cmd, "sha", "diff", "", "").unwrap();
    assert_eq!(s(&object(vec![("seed", seed.unwrap())]), "seed"), "2");
    let other = Json::Array(vec![string("train"), string("--seed=9")]);
    assert_eq!(
        g,
        metrics::seed_and_group(&other, "sha", "diff", "", "")
            .unwrap()
            .1
    );
    assert!(metrics::seed_and_group(&cmd, "s", "d", "-", "").is_err());
    for n in [1, 4300, 4301] {
        assert_eq!(
            metrics::seed_and_group(&cmd, "s", "d", &"1".repeat(n), "").is_ok(),
            n <= 4300
        );
    }
    let rows = vec![
        (1.0, object(vec![("group", string("g")), ("seed", num(0))])),
        (2.0, object(vec![("group", string("g")), ("seed", num(1))])),
        (3.0, object(vec![("group", string("g")), ("seed", num(0))])),
        (1.0, object(vec![])),
    ];
    let (groups, loose) = metrics::groups(&rows, "m").unwrap();
    assert_eq!(loose, 1);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].mean, 2.5);
    assert_eq!(groups[0].seeds, vec!["0", "1"]);
    assert_eq!(groups[0].ci, Some([2.0, 3.0]));
    let (v, left) = metrics::comparable(
        vec![(1.0, object(vec![]))],
        "x",
        Some(&object(vec![("digest", string("a"))])),
    );
    assert!(v.is_empty());
    assert_eq!(left["no contract recorded"], 1);
}
#[test]
fn contract_limits_hashes_and_input_capture() {
    let f = Fixture::new();
    let ledger = f.0.join("docs/EXPERIMENTS.jsonl");
    f.write(
        "docs/metric-contract.json",
        br#"{"schema":1,"metrics":{"m":{"direction":"min","eval_code":["eval.rs"]}}}"#,
    );
    f.write("eval.rs", "one");
    let a = metrics::row_contract(&ledger, &f.0).unwrap().unwrap();
    f.write("eval.rs", "two");
    let b = metrics::row_contract(&ledger, &f.0).unwrap().unwrap();
    assert_ne!(
        s(get(&a, "m").unwrap(), "digest"),
        s(get(&b, "m").unwrap(), "digest")
    );
    for raw in [
        br#"{"schema":1,"metrics":{"m":{"direction":"wat"}}}"#.as_slice(),
        br#"{"schema":1,"metrics":{"m":{"direction":"min","eval_code":["../x"]}}}"#,
        b"{",
        b"",
    ] {
        f.write("docs/metric-contract.json", raw);
        assert!(metrics::load(&ledger).is_err());
    }
    let mut identity = object(vec![
        ("schema", num(1)),
        ("capture", string("pre-command")),
        (
            "git",
            object(vec![
                ("commit", string("a".repeat(40))),
                ("dirty", num(0)),
                ("diff_sha256", string("")),
            ]),
        ),
        ("config_status", string("unrecorded")),
        ("data_status", string("unrecorded")),
        ("config_sha256", Json::Null),
        ("data_sha256", Json::Null),
        ("recorded_configs", num(0)),
        ("recorded_data", num(0)),
    ]);
    let d = digest(&identity).unwrap();
    put(&mut identity, "digest", string(d));
    metrics::validate_identity(&identity).unwrap();
    let capture = object(vec![
        ("schema", num(1)),
        ("capsule_id", string("capsule-a")),
        ("inputs", identity),
        ("metric_contract", Json::Null),
        ("seeds", Json::Array(vec![string("3")])),
    ]);
    let bytes = ordered(&capture).unwrap();
    let capture = metrics::decode_capture(&bytes).unwrap();
    let mut row = object(vec![
        ("cmd", Json::Array(vec![string("train")])),
        ("seed", num(5)),
        ("group", string("old")),
    ]);
    metrics::attach_capture(&mut row, &capture).unwrap();
    assert_eq!(s(&row, "seed"), "3");
    assert_eq!(s(&row, "seed_status"), "declared");
    assert_eq!(s(&row, "git_sha"), "a".repeat(40));
    assert!(metrics::decode_capture(b"{}").is_err());
    assert!(metrics::decode_capture(&vec![b' '; 65537]).is_err());
    assert!(metrics::decode_capture(br#"{"schema":1,"schema":1}"#).is_err());
    put(
        &mut row,
        "cmd",
        Json::Array(vec![string("train"), string("--seed=4")]),
    );
    metrics::attach_capture(&mut row, &capture).unwrap();
    assert_eq!(s(&row, "seed_status"), "conflicting-declarations");
    assert!(get(&row, "seed").is_none());
}
#[test]
fn execute_gate_metrics_records_and_list() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.write("metrics.json", br#"{"m":0.5,"bool":true,"bad":NaN}"#);
    let r = f.invoke(
        "run-ledger",
        &[
            "--note",
            "trial",
            "--metrics",
            "metrics.json",
            "--seed",
            "0",
            "--",
            "sh",
            "-c",
            "printf live; printf diagnostic >&2; exit 7",
        ],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 7, "{}", String::from_utf8_lossy(&r.2));
    assert_eq!(r.1, b"live");
    assert!(String::from_utf8_lossy(&r.2).contains("diagnostic"));
    let rows = f.rows("docs/EXPERIMENTS.jsonl");
    assert_eq!(s(&rows[0], "gate"), "none");
    assert_eq!(s(&rows[0], "exit"), "7");
    assert_eq!(
        get(&rows[0], "metrics")
            .and_then(|m| get(m, "m"))
            .and_then(number),
        Some(0.5)
    );
    let r = f.invoke("run-ledger", &["list", "--json", "1"], &c, &mut svc);
    assert_eq!(r.0, 0);
    assert!(String::from_utf8_lossy(&r.1).contains("trial"));
    f.executable(
        "scripts/check.sh",
        "#!/bin/sh\nprintf first >&2\nprintf second\nexit 1\n",
    );
    let gate = f.invoke("run-ledger", &["--", "true"], &c, &mut svc);
    assert_eq!(gate.0, 3);
    assert!(String::from_utf8_lossy(&gate.2).contains("firstsecond"));
    assert_eq!(
        f.invoke("run-ledger", &["--no-gate", "--", "true"], &c, &mut svc)
            .0,
        2
    );
    let r = f.invoke(
        "run-ledger",
        &["--no-gate", "--reason", "fixture override", "--", "true"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
    assert_eq!(s(&f.rows("docs/EXPERIMENTS.jsonl")[1], "gate"), "skipped");
    f.executable("scripts/check.sh","#!/bin/sh\nprintf '%s\\n' \"$1\" >> gate-mode\n# alternative dispatch\ncase \"$1\" in ml-smoke) exit 0;; *) exit 0;; esac\n");
    c.env.remove(OsStr::new("OMS_RUN_LEDGER_CHECK_MODE"));
    assert_eq!(f.invoke("run-ledger", &["--", "true"], &c, &mut svc).0, 0);
    assert_eq!(
        fs::read_to_string(f.0.join("gate-mode")).unwrap(),
        "ml-smoke\n"
    );
    for args in [
        vec!["list", "0"],
        vec!["list", "bad"],
        vec!["--seed", "-", "--", "true"],
        vec!["--group", "", "--", "true"],
        vec!["--group", "bad/group", "--", "true"],
        vec!["--note"],
        vec!["--"],
        vec!["top"],
        vec!["table", "--format", "html"],
        vec!["top", "--metric", "m", "0"],
    ] {
        assert_eq!(f.invoke("run-ledger", &args, &c, &mut svc).0, 2, "{args:?}");
    }
}
#[test]
fn started_journal_duplicate_and_service_absence() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    c.env
        .insert("OMS_RESEARCH_METADATA_FILE".into(), "research.json".into());
    f.write("research.json", br#"{"question":"Q","change":"C"}"#);
    c.env.insert("OMS_WORK_JOURNAL".into(), "1".into());
    let r = f.invoke(
        "run-ledger",
        &["--", "sh", "-c", "test -s docs/EXPERIMENTS.jsonl"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
    let rows = f.rows("docs/EXPERIMENTS.jsonl");
    assert_eq!(rows.len(), 2);
    assert_eq!(s(&rows[0], "status"), "started");
    assert_eq!(s(&rows[0], "id"), s(&rows[1], "id"));
    assert_eq!(svc.events.len(), 1);
    let r = f.invoke(
        "run-ledger",
        &["--", "sh", "-c", "test -s docs/EXPERIMENTS.jsonl"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0);
    assert!(String::from_utf8_lossy(&r.2).contains("identical run already in ledger"));
    let r = f.invoke(
        "run-ledger",
        &["--", "sh", "-c", "touch must-not-run"],
        &c,
        &mut NoServices,
    );
    assert_eq!(r.0, 2);
    assert!(!f.0.join("must-not-run").exists());
    c.env
        .insert("OMS_RUN_LEDGER_STATUS_OVERRIDE".into(), "x".into());
    assert_eq!(f.invoke("run-ledger", &["--", "true"], &c, &mut svc).0, 2);
    for (status, code) in [
        ("0", 0),
        ("300", 44),
        ("9223372036854775807", 255),
        ("9223372036854775808", 2),
    ] {
        c.env
            .insert("OMS_RUN_LEDGER_STATUS_OVERRIDE".into(), status.into());
        let r = f.invoke(
            "run-ledger",
            &["--", "sh", "-c", "touch must-not-run"],
            &c,
            &mut svc,
        );
        assert_eq!(r.0, code);
        assert_eq!(
            s(f.rows("docs/EXPERIMENTS.jsonl").last().unwrap(), "exit"),
            status
        );
        assert!(!f.0.join("must-not-run").exists());
    }
}
#[test]
fn ranking_tables_and_comparability() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let c = f.context(&flag);
    let mut svc = Recorder::new();
    for (group, seed, v, exit) in [
        ("a", 0, 0.2, 0),
        ("a", 1, 0.4, 0),
        ("b", 0, 0.5, 0),
        ("b", 1, 0.7, 0),
        ("single", 1, 0.9, 0),
        ("failed", 0, 99.0, 3),
    ] {
        let row = object(vec![
            ("schema", num(1)),
            ("group", string(group)),
            ("seed", num(seed)),
            ("exit", num(exit)),
            ("metrics", object(vec![("m", num(v))])),
        ]);
        store::append(&c, &f.0.join("docs/EXPERIMENTS.jsonl"), &row).unwrap();
    }
    let r = f.invoke(
        "run-ledger",
        &["top", "--metric", "m", "--by-group"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0);
    let out = String::from_utf8_lossy(&r.1);
    assert!(out.contains("group=b"));
    assert!(out.contains("single seed (no claim"));
    assert!(!out.contains("99"));
    for format in ["md", "tsv", "tex"] {
        let r = f.invoke(
            "run-ledger",
            &["table", "--metric", "m", "--format", format],
            &c,
            &mut svc,
        );
        assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
        let out = String::from_utf8_lossy(&r.1);
        assert!(out.contains("4 significant digits"));
        assert!(out.contains("single seed"));
    }
    assert_eq!(
        f.invoke("run-ledger", &["top", "--metric", "missing"], &c, &mut svc)
            .0,
        3
    );
    f.write(
        "docs/metric-contract.json",
        br#"{"schema":1,"metrics":{"m":{"direction":"max"}}}"#,
    );
    assert_eq!(
        f.invoke("run-ledger", &["top", "--metric", "m"], &c, &mut svc)
            .0,
        4
    );
    assert_eq!(
        f.invoke("run-ledger", &["table", "--mixed"], &c, &mut svc)
            .0,
        0
    );
}
#[test]
fn scheduler_aggregation_limits_and_usage() {
    let legacy = reconcile::parse_accounting("1", b"COMPLETED|0:0|00:10:00\n").unwrap();
    assert_eq!(s(&legacy, "state"), "COMPLETED");
    let partial =
        reconcile::parse_accounting("1", b"RUNNING|0:0|00:00:01\nFAILED|1:0|00:01:00\n").unwrap();
    assert_eq!(s(&partial, "state"), "RUNNING");
    let row = |i: usize, state: &str| {
        format!(
            "1_{i}|{state}|0:0|00:00:30|c|2026-01-02T03:00:00|2026-01-02T03:01:00|2026-01-02T03:02:00|gres/gpu=1|n|00:01:00|2|00:00:30|name|0\n"
        )
    };
    for count in [0, 1, 200, 201] {
        let raw = (0..count)
            .map(|i| row(i, if i == 1 { "FAILED" } else { "COMPLETED" }))
            .collect::<String>();
        let job = reconcile::parse_accounting("1", raw.as_bytes()).unwrap();
        if count == 0 {
            assert_eq!(s(&job, "state"), "UNKNOWN");
        }
        if count > 1 {
            let Some(Json::Array(tasks)) = get(&job, "tasks") else {
                panic!()
            };
            assert_eq!(tasks.len(), count.min(200));
        }
        if count == 201 {
            assert_eq!(s(&job, "tasks_omitted"), "1");
        }
        if count == 200 {
            assert!(get(&job, "tasks_omitted").is_none());
        }
    }
    let raw = format!(
        "{}{}",
        row(0, "PREEMPTED").replace("|0\n", "|1\n"),
        row(0, "COMPLETED").replace("|0\n", "|2\n")
    );
    let job = reconcile::parse_accounting("1", raw.as_bytes()).unwrap();
    assert_eq!(s(&job, "attempts"), "2");
    assert_eq!(
        get(&job, "usage").map(|v| s(v, "elapsed_seconds")).unwrap(),
        "60"
    );
    let raw = format!("{}{}", row(0, "FAILED"), row(0, "COMPLETED"));
    let job = reconcile::parse_accounting("1", raw.as_bytes()).unwrap();
    assert!(get(&job, "attempts").is_none());
    assert_eq!(s(&job, "state"), "COMPLETED");
    assert_eq!(usage::seconds("1-02:03:04.99"), Some(93784));
    assert_eq!(usage::seconds("00:00"), Some(0));
    assert_eq!(usage::seconds(""), None);
    assert_eq!(usage::seconds("garbage"), None);
    assert_eq!(
        usage::tres_count("gres/gpu:a=2,gres/gpu:b=3", "gres/gpu"),
        Some(5)
    );
    assert_eq!(usage::tres_count("cpu=4", "gres/gpu"), Some(0));
    assert_eq!(usage::tres_count("", "gres/gpu"), None);
}
#[test]
fn reconcile_fake_queries_one_pass_idempotence_and_placeholder() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.executable(
        "sacct",
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> queries\ncat accounting\n",
    );
    f.executable("squeue", "#!/bin/sh\ncat queued\n");
    f.write("queued", b"");
    f.write("accounting",b"11|COMPLETED|0:0|00:30:00|c|2026-01-02T03:00:00|2026-01-02T03:10:00|2026-01-02T03:40:00|gres/gpu=2|n|01:00:00|4|01:00:00|name|0\n");
    c.env
        .insert("OMS_SACCT_CMD".into(), f.0.join("sacct").into_os_string());
    c.env
        .insert("OMS_SQUEUE_CMD".into(), f.0.join("squeue").into_os_string());
    c.env.insert("OMS_WORK_JOURNAL".into(), "1".into());
    let r = f.invoke(
        "run-reconcile",
        &["apply", "--job", "11", "--memory", "--agent", "fixture"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
    assert_eq!(svc.digests, 1);
    assert_eq!(svc.notes.len(), 1);
    assert_eq!(svc.events.len(), 1);
    let rows = f.rows(".oms/runs/reconcile.jsonl");
    assert_eq!(
        get(&rows[0], "usage").map(|v| s(v, "gpu_hours")).unwrap(),
        "1.0"
    );
    assert_eq!(
        fs::read_to_string(f.0.join("queries")).unwrap(),
        format!(
            "-X -D -j 11 --format={} -n -P\n",
            "JobID,State,ExitCode,Elapsed,Cluster,Submit,Start,End,AllocTRES,NodeList,Timelimit,AllocCPUS,TotalCPU,JobName,Restarts"
        )
    );
    assert_eq!(
        f.invoke("run-reconcile", &["apply", "--job", "11"], &c, &mut svc)
            .0,
        0
    );
    assert_eq!(svc.digests, 1);
    assert_eq!(f.rows(".oms/runs/reconcile.jsonl").len(), 1);
    let r = f.invoke(
        "run-reconcile",
        &["usage", "--job", "11", "--json"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0);
    assert_eq!(
        get(&parse(&r.1).unwrap(), "gpu_hours").and_then(number),
        Some(1.0)
    );
    svc.fail_digest = true;
    f.write("accounting", b"FAILED|1:0|00:00:01\n");
    assert_eq!(
        f.invoke("run-reconcile", &["apply", "--job", "12"], &c, &mut svc)
            .0,
        0
    );
    assert_eq!(
        fs::read_to_string(f.0.join(".oms/runs/reconcile/12.md")).unwrap(),
        "# job 12 digest unavailable\n"
    );
    assert_eq!(
        f.invoke("run-reconcile", &["list", "0"], &c, &mut svc).1,
        b""
    );
    assert_eq!(
        f.invoke("run-reconcile", &["list", "malformed"], &c, &mut svc)
            .0,
        2
    );
    assert_eq!(
        f.invoke("run-reconcile", &["scan", "--job", "../bad"], &c, &mut svc)
            .0,
        2
    );
}
#[test]
fn requeue_unknown_queries_deadlines_and_cancellation() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.executable("sacct", "#!/bin/sh\ncat accounting\n");
    f.executable("squeue", "#!/bin/sh\ncat queued\n");
    f.write("accounting", b"PREEMPTED|0:15|00:01:00\n");
    f.write("queued", b"PENDING\n");
    c.env
        .insert("OMS_SACCT_CMD".into(), f.0.join("sacct").into_os_string());
    c.env
        .insert("OMS_SQUEUE_CMD".into(), f.0.join("squeue").into_os_string());
    let r = f.invoke("run-reconcile", &["scan", "--job", "15"], &c, &mut svc);
    assert_eq!(r.0, 0);
    assert!(
        String::from_utf8_lossy(&r.1).contains("REQUEUED\trequeued after PREEMPTED, attempt 2")
    );
    assert_eq!(
        f.invoke("run-reconcile", &["apply", "--job", "15"], &c, &mut svc)
            .0,
        0
    );
    assert!(!f.0.join(".oms/runs/reconcile.jsonl").exists());
    f.executable("squeue", "#!/bin/sh\nexit 7\n");
    let r = f.invoke("run-reconcile", &["scan", "--job", "15"], &c, &mut svc);
    assert!(String::from_utf8_lossy(&r.1).contains("UNKNOWN"));
    f.executable("sacct", "#!/bin/sh\nsleep 5\n");
    c.query_timeout = Duration::from_millis(40);
    let start = Instant::now();
    assert_eq!(
        f.invoke("run-reconcile", &["scan", "--job", "15"], &c, &mut svc)
            .0,
        124
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    c.command_timeout = Some(Duration::from_millis(40));
    let r = f.invoke("run-ledger", &["--", "sh", "-c", "sleep 5"], &c, &mut svc);
    assert_eq!(r.0, 124, "{}", String::from_utf8_lossy(&r.2));
    assert_eq!(
        s(f.rows("docs/EXPERIMENTS.jsonl").last().unwrap(), "exit"),
        "124"
    );
    flag.store(true, Ordering::Relaxed);
    assert_eq!(f.invoke("run-ledger", &["--", "true"], &c, &mut svc).0, 130);
}
#[test]
fn safe_storage_concurrent_append_links_and_torn_tail() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let c = f.context(&flag);
    let path = f.0.join("rows.jsonl");
    std::thread::scope(|scope| {
        for i in 0..6 {
            let c = &c;
            let path = &path;
            scope.spawn(move || {
                for j in 0..4 {
                    store::append(c, path, &object(vec![("i", num(i)), ("j", num(j))])).unwrap();
                }
            });
        }
    });
    assert_eq!(f.rows("rows.jsonl").len(), 24);
    f.write("broken", b"{torn");
    store::append(&c, &f.0.join("broken"), &object(vec![("ok", num(1))])).unwrap();
    assert_eq!(f.rows("broken").len(), 1);
    symlink(&path, f.0.join("alias")).unwrap();
    assert!(store::append(&c, &f.0.join("alias"), &object(vec![])).is_err());
    fs::hard_link(&path, f.0.join("hard")).unwrap();
    assert!(store::append(&c, &path, &object(vec![])).is_err());
    assert!(store::open(&path, false).is_err());
    f.write("large", vec![b'x'; store::ROW_BYTES + 1]);
    assert!(store::lines(&f.0.join("large"), false, |_| Ok(())).is_err());
}

#[test]
fn canonical_metric_hash_bootstrap_and_identity_producer() {
    let spec = object(vec![
        ("unit", string("α")),
        ("direction", string("max")),
        ("split", string("val")),
        ("aggregation", string("mean")),
        ("eval_command", string("eval")),
        ("eval_code", Json::Array(vec![])),
    ]);
    let row = metrics::record(&spec, Path::new(".")).unwrap();
    assert_eq!(
        s(&row, "digest"),
        "9849f7af1ae7b97e082938cde6b1535dfe91252345b253f579fd93735cfc4b57"
    );
    let (_, group) = metrics::seed_and_group(
        &Json::Array(vec![
            string("train"),
            string("--seed"),
            string("1"),
            string("seed=2"),
        ]),
        "sha",
        "diff",
        "",
        "",
    )
    .unwrap();
    assert_eq!(group.as_deref(), Some("64094cb0de01"));
    let ci = metrics::bootstrap(&[0.1, 0.25, 0.8, 1.2], "fixture").unwrap();
    assert!((ci[0] - 0.175).abs() < 1e-14);
    assert!((ci[1] - 1.1).abs() < 1e-14);
    assert!(metrics::bootstrap(&[], "x").is_none());
    assert_eq!(metrics::bootstrap(&[0.0], "x"), Some([0.0, 0.0]));
    let git = object(vec![
        ("commit_full", string("c".repeat(40))),
        ("dirty", num(0)),
        ("diff_sha256", string("")),
    ]);
    let configs = Json::Array(vec![object(vec![
        ("path", string("a")),
        ("sha256", string("b".repeat(32))),
    ])]);
    let data = Json::Array(vec![]);
    let expected = Json::Array(vec![string("a")]);
    let identity = metrics::make_input_identity(&git, &configs, &data, &expected).unwrap();
    metrics::validate_identity(&identity).unwrap();
    assert_eq!(s(&identity, "config_status"), "recorded");
    let incomplete =
        metrics::make_input_identity(&git, &Json::Array(vec![]), &data, &expected).unwrap();
    assert_eq!(s(&incomplete, "config_status"), "incomplete");
    metrics::validate_identity(&incomplete).unwrap();
    for n in [0, 128, 129] {
        let rows = Json::Array((0..n).map(|_| string("a")).collect());
        assert_eq!(
            metrics::make_input_identity(&git, &configs, &data, &rows).is_ok(),
            n <= 128
        );
    }
    let invalid = Json::Array(vec![object(vec![
        ("path", string("a\u{200b}")),
        ("sha256", string("b".repeat(32))),
    ])]);
    assert!(metrics::make_input_identity(&git, &invalid, &data, &expected).is_err());
    let capture = metrics::capture_for_ledger(
        &git,
        &configs,
        &data,
        &expected,
        &Json::Null,
        &Json::Array(vec![string("0")]),
        "cap-a",
    )
    .unwrap();
    assert_eq!(s(&capture, "capsule_id"), "cap-a");
}
#[test]
fn submission_stream_exact_argv_and_git_fingerprints() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.executable("bin/git","#!/bin/sh\ncase \"$*\" in *--show-toplevel) pwd -P;; 'rev-parse --git-dir') echo .git;; 'rev-parse --verify --quiet HEAD') echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa;; 'rev-parse --short HEAD') echo aaaaaaa;; 'status --porcelain --untracked-files=no') printf 'M  staged\\n';; 'diff HEAD') printf staged-diff;; *) exit 128;; esac\n");
    f.executable("bin/sbatch","#!/bin/sh\nprintf '%s\\n' 'Submitted batch job 123'\nprintf '%s\\n' \"$@\" > submitted-argv\n");
    c.env.insert("SLURM_JOB_ID".into(), "9000".into());
    let raw = OsString::from_vec(vec![b'x', 0xed, 0xa0, 0x80, 0xff]);
    let args = vec![
        "--seed".into(),
        "-1".into(),
        "--".into(),
        "sbatch".into(),
        raw.clone(),
        "hello world".into(),
    ];
    let mut out = vec![];
    let mut err = vec![];
    let recorded = execute(&args, &c, &mut svc, &mut out, &mut err).unwrap();
    assert_eq!(recorded.code, 0);
    assert!(out.starts_with(b"Submitted batch job 123"));
    assert_eq!(s(&recorded.row, "slurm_job_id"), "123");
    assert_eq!(s(&recorded.row, "git_sha"), "aaaaaaa");
    assert_eq!(s(&recorded.row, "dirty_hash"), hash(b"staged-diff")[..16]);
    assert!(
        ordered(&recorded.row)
            .unwrap()
            .windows(6)
            .any(|w| w == b"\\udced")
    );
    let Some(Json::Array(cmd)) = get(&recorded.row, "cmd") else {
        panic!()
    };
    assert_eq!(cmd[1], os(&raw));
    assert_eq!(cmd[2], string("hello world"));
    f.executable("bin/sbatch", "#!/bin/sh\necho '123;other'\n");
    assert_eq!(f.invoke("run-ledger", &["--", "sbatch"], &c, &mut svc).0, 0);
    assert_eq!(
        s(
            f.rows("docs/EXPERIMENTS.jsonl").last().unwrap(),
            "slurm_job_id"
        ),
        ""
    );
    f.executable("bin/git","#!/bin/sh\ncase \"$*\" in *--show-toplevel) pwd -P;; 'rev-parse --git-dir') echo .git;; 'rev-parse --verify --quiet HEAD') exit 1;; 'status --porcelain --untracked-files=no') printf 'A  staged\\n';; 'diff --cached') printf index;; 'diff') printf worktree;; *) exit 128;; esac\n");
    assert_eq!(f.invoke("run-ledger", &["--", "true"], &c, &mut svc).0, 0);
    let rows = f.rows("docs/EXPERIMENTS.jsonl");
    let row = rows.last().unwrap();
    assert_eq!(s(row, "git_sha"), "no-commit");
    assert_eq!(s(row, "dirty_hash"), hash(b"indexworktree")[..16]);
}
#[test]
fn metrics_file_limits_scrub_and_preflight_errors() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.write("metrics.json", b"{\"m\":1}");
    for (limit, kept) in [(0, false), (6, false), (7, true)] {
        c.env
            .insert("OMS_METRICS_MAX_BYTES".into(), limit.to_string().into());
        let r = f.invoke(
            "run-ledger",
            &["--metrics", "metrics.json", "--", "true"],
            &c,
            &mut svc,
        );
        assert_eq!(r.0, 0);
        let rows = f.rows("docs/EXPERIMENTS.jsonl");
        assert_eq!(get(rows.last().unwrap(), "metrics").is_some(), kept);
    }
    c.env.insert("OMS_METRICS_MAX_BYTES".into(), "bad".into());
    assert_eq!(
        f.invoke(
            "run-ledger",
            &[
                "--metrics",
                "metrics.json",
                "--",
                "sh",
                "-c",
                "touch unsafe"
            ],
            &c,
            &mut svc
        )
        .0,
        2
    );
    assert!(!f.0.join("unsafe").exists());
    c.env.insert("OMS_METRICS_MAX_BYTES".into(), "65536".into());
    let human = format!("{}{}{}", "pass", "word", ["=", "example-value"].concat());
    f.executable("scripts/check.sh", "#!/bin/sh\nexit 0\n");
    let r = f.invoke(
        "run-ledger",
        &["--no-gate", "--reason", &human, "--", "true"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 2);
    assert!(!String::from_utf8_lossy(&r.2).contains("example-value"));
    f.write("metrics.json", format!("{{\"m\":\"{human}\"}}"));
    let r = f.invoke(
        "run-ledger",
        &["--metrics", "metrics.json", "--", "true"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0);
    assert!(String::from_utf8_lossy(&r.2).contains("sensitive-looking"));
    assert!(get(f.rows("docs/EXPERIMENTS.jsonl").last().unwrap(), "metrics").is_none());
    for group in ["a".repeat(60), "a".repeat(61)] {
        let r = f.invoke(
            "run-ledger",
            &["--group", &group, "--", "true"],
            &c,
            &mut svc,
        );
        assert_eq!(r.0, if group.len() == 60 { 0 } else { 2 });
    }
}
#[test]
fn signal_reaps_children_and_records_final_outcome() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let signal = AtomicI32::new(0);
    let mut c = f.context(&flag);
    c.signal = Some(&signal);
    let mut svc = Recorder::new();
    let ready = f.0.join("ready");
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let limit = Instant::now() + Duration::from_secs(3);
            while !ready.exists() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(ready.exists());
            signal.store(15, Ordering::Relaxed);
        });
        let r = f.invoke(
            "run-ledger",
            &[
                "--",
                "sh",
                "-c",
                "trap '' TERM; sleep 10 & touch ready; wait",
            ],
            &c,
            &mut svc,
        );
        assert_eq!(r.0, 143, "{}", String::from_utf8_lossy(&r.2));
    });
    assert_eq!(
        s(f.rows("docs/EXPERIMENTS.jsonl").last().unwrap(), "exit"),
        "143"
    );
}
#[test]
fn concurrent_reconcile_has_one_receipt_and_digest() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    f.executable("sacct", "#!/bin/sh\necho 'COMPLETED|0:0|00:01:00'\n");
    c.env
        .insert("OMS_SACCT_CMD".into(), f.0.join("sacct").into_os_string());
    std::thread::scope(|scope| {
        let mut handles = vec![];
        for _ in 0..4 {
            let c = &c;
            let f = &f;
            handles.push(scope.spawn(move || {
                let mut svc = Recorder::new();
                let r = f.invoke("run-reconcile", &["apply", "--job", "7"], c, &mut svc);
                assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
                svc.digests
            }));
        }
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .sum::<usize>(),
            1
        );
    });
    assert_eq!(f.rows(".oms/runs/reconcile.jsonl").len(), 1);
}

#[test]
fn parser_help_positions_and_read_only_unknown_fields() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let c = f.context(&flag);
    let mut svc = Recorder::new();
    assert_eq!(f.invoke("run-ledger", &["--help"], &c, &mut svc).0, 0);
    assert_eq!(
        f.invoke("run-ledger", &["list", "--help"], &c, &mut svc).0,
        2
    );
    assert_eq!(
        f.invoke("run-ledger", &["--seed", "bad", "--help"], &c, &mut svc)
            .0,
        2
    );
    let r = f.invoke(
        "run-ledger",
        &["--note", "--help", "--", "true"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0);
    assert_eq!(
        s(f.rows("docs/EXPERIMENTS.jsonl").last().unwrap(), "note"),
        "--help"
    );
    f.write("docs/EXPERIMENTS.jsonl",b"{\"exit\":0,\"metrics\":{\"m\":1},\"extra\":NaN,\"alien\":{\"value\":12345678901234567890}}\n[1]\n{torn\n");
    let r = f.invoke("run-ledger", &["list", "--json", "3"], &c, &mut svc);
    assert_eq!(r.0, 0);
    assert!(String::from_utf8_lossy(&r.1).contains("12345678901234567890"));
    assert!(String::from_utf8_lossy(&r.1).contains("NaN"));
    assert!(String::from_utf8_lossy(&r.2).contains("2 invalid row(s)"));
    assert_eq!(
        f.invoke("run-ledger", &["top", "--metric", "m"], &c, &mut svc)
            .0,
        0
    );
}
#[test]
fn metadata_contract_and_capture_limit_edges() {
    let f = Fixture::new();
    let ledger = f.0.join("docs/EXPERIMENTS.jsonl");
    assert!(matches!(metrics::load(&ledger).unwrap(),Json::Object(ref o) if o.is_empty()));
    for n in [0, 50, 51] {
        let fields = (0..n)
            .map(|i| format!("\"m{i}\":{{\"direction\":\"min\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        f.write(
            "docs/metric-contract.json",
            format!("{{\"schema\":1,\"metrics\":{{{fields}}}}}"),
        );
        assert_eq!(metrics::load(&ledger).is_ok(), n <= 50);
    }
    for n in [0, 20, 21] {
        let files = (0..n)
            .map(|i| format!("\"file{i}\""))
            .collect::<Vec<_>>()
            .join(",");
        f.write("docs/metric-contract.json",format!("{{\"schema\":1,\"metrics\":{{\"m\":{{\"direction\":\"min\",\"eval_code\":[{files}]}}}}}}"));
        assert_eq!(metrics::load(&ledger).is_ok(), n <= 20);
    }
    for n in [0, 200, 201] {
        let label = "x".repeat(n);
        f.write(
            "docs/metric-contract.json",
            format!("{{\"schema\":1,\"metrics\":{{\"{label}\":{{\"direction\":\"min\"}}}}}}"),
        );
        assert_eq!(metrics::load(&ledger).is_ok(), n <= 200);
    }
    let body = b"{\"schema\":1,\"metrics\":{}}";
    for n in [65536, 65537] {
        let mut bytes = body.to_vec();
        bytes.resize(n, b' ');
        f.write("docs/metric-contract.json", bytes);
        assert_eq!(metrics::load(&ledger).is_ok(), n == 65536);
    }
    let git = object(vec![
        ("commit_full", string("c".repeat(40))),
        ("dirty", num(0)),
        ("diff_sha256", string("")),
    ]);
    let empty = Json::Array(vec![]);
    let capture =
        metrics::capture_for_ledger(&git, &empty, &empty, &empty, &Json::Null, &empty, "c")
            .unwrap();
    let mut bytes = ordered(&capture).unwrap();
    bytes.resize(65536, b' ');
    assert!(metrics::decode_capture(&bytes).is_ok());
    bytes.push(b' ');
    assert!(metrics::decode_capture(&bytes).is_err());
    f.write("code", b"");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(f.0.join("code"))
        .unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    f.write(
        "docs/metric-contract.json",
        b"{\"schema\":1,\"metrics\":{\"m\":{\"direction\":\"min\",\"eval_code\":[\"code\"]}}}",
    );
    let a = metrics::row_contract(&ledger, &f.0).unwrap().unwrap();
    file.set_len(8 * 1024 * 1024 + 1).unwrap();
    let b = metrics::row_contract(&ledger, &f.0).unwrap().unwrap();
    assert_ne!(
        s(get(&a, "m").unwrap(), "digest"),
        s(get(&b, "m").unwrap(), "digest")
    );
}
/// A parallel test thread can fork while this script's write fd is open, making
/// the immediate exec fail with ETXTBSY; `process` reports that as `Ok(126)`.
fn query_script(c: &Context<'_>, f: &Fixture) -> Result<process::Output> {
    for _ in 0..100 {
        let r = process::query(c, &[f.0.join("query").into_os_string()], 7)?;
        if r.code != 126 || !String::from_utf8_lossy(&r.stderr).contains("busy") {
            return Ok(r);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("query script stayed busy");
}

#[test]
fn query_output_byte_limit_edges() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let c = f.context(&flag);
    for (body, ok) in [("", true), ("1234567", true), ("12345678", false)] {
        f.executable("query", &format!("#!/bin/sh\nprintf '%s' '{body}'\n"));
        let r = query_script(&c, &f);
        assert_eq!(r.is_ok(), ok);
        if let Ok(r) = r {
            assert_eq!(r.code, 0);
            assert_eq!(r.bytes, body.as_bytes());
        }
    }
    for (size, ok) in [(0, true), (65536, true), (65537, false)] {
        f.write("diagnostic", vec![b'x'; size]);
        f.executable("query", "#!/bin/sh\ncat diagnostic >&2\n");
        let r = query_script(&c, &f);
        assert_eq!(r.is_ok(), ok);
        if let Ok(r) = r {
            assert_eq!(r.code, 0);
            assert_eq!(r.stderr.len(), size);
        }
    }
}

#[test]
fn parent_relative_override_empty_scan_and_final_split_advisory() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.write("nested/keep", b"");
    c.cwd = f.0.join("nested");
    let r = f.invoke(
        "run-ledger",
        &["--file", "../custom.jsonl", "--", "true"],
        &c,
        &mut svc,
    );
    assert_eq!(r.0, 0, "{}", String::from_utf8_lossy(&r.2));
    assert_eq!(f.rows("custom.jsonl").len(), 1);
    let r = f.invoke(
        "run-reconcile",
        &["apply", "--ledger", "missing"],
        &c,
        &mut NoServices,
    );
    assert_eq!(r.0, 0);
    assert!(String::from_utf8_lossy(&r.2).contains("0 reconciled, 0 still running/pending"));
    f.write(
        "docs/metric-contract.json",
        br#"{"schema":1,"metrics":{},"splits":{"test":{"role":"final"}}}"#,
    );
    f.write("docs/EXPERIMENTS.jsonl",b"{\"metrics\":{\"m\":1},\"metric_contract\":{\"m\":{\"split\":\"test\"}},\"group\":\"a\"}\n{\"metrics\":{\"m\":2},\"metric_contract\":{\"m\":{\"split\":\"test\"}},\"group\":\"b\"}\n");
    let notes = metrics::final_split_report(&f.0.join("docs/EXPERIMENTS.jsonl")).unwrap();
    assert!(
        notes
            .iter()
            .any(|s| s.contains("test split evaluated 2 times across 2 variants"))
    );
}

#[test]
fn observed_in_place_change_is_not_a_complete_read() {
    let f = Fixture::new();
    f.write("changing", b"{}\n");
    let path = f.0.join("changing");
    let mut changed = false;
    let result = store::lines(&path, false, |_| {
        if !changed {
            changed = true;
            use std::io::Write;
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"{}\n").unwrap();
        }
        Ok(())
    });
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("changed during read"));
}

#[test]
fn reconcile_digest_cannot_replace_the_receipt_file() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let mut c = f.context(&flag);
    let mut svc = Recorder::new();
    f.executable("sacct", "#!/bin/sh\necho 'COMPLETED|0:0|00:00:01'\n");
    c.env
        .insert("OMS_SACCT_CMD".into(), f.0.join("sacct").into_os_string());
    c.env
        .insert("OMS_RECONCILE_FILE".into(), "out/../out/7.md".into());
    c.env
        .insert("OMS_RECONCILE_DIGEST_DIR".into(), "out".into());
    let r = f.invoke("run-reconcile", &["apply", "--job", "7"], &c, &mut svc);
    assert_eq!(r.0, 2);
    assert!(String::from_utf8_lossy(&r.2).contains("must be distinct"));
    assert!(!f.0.join("out/7.md").exists());
    assert_eq!(svc.digests, 0);
}

#[test]
fn execute_with_hooks_delivers_live_child_pid_immediately_after_spawn() {
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let c = f.context(&flag);
    let gate = f.0.join("spawned.gate");
    f.executable(
        "pid_wait.sh",
        "#!/bin/sh\necho \"$$\"\nwhile [ ! -e \"$1\" ]; do sleep 0.01; done\n",
    );
    let argv = [
        f.0.join("pid_wait.sh").into_os_string(),
        gate.clone().into_os_string(),
    ];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut observed_pid = None;
    let mut hook = |pid: u32| {
        observed_pid = Some(pid);
        assert!(pid > 0);
        fs::write(&gate, b"ok").unwrap();
    };
    let output = crate::process::execute_with_hooks(
        &c,
        &argv,
        &crate::process::ProcessPolicy::default(),
        false,
        1024,
        Some(Duration::from_secs(5)),
        &mut out,
        &mut err,
        Some(&mut hook),
    )
    .unwrap();
    assert_eq!(output.code, 0);
    let stdout_pid: u32 = String::from_utf8_lossy(&output.bytes)
        .trim()
        .parse()
        .expect("child prints numeric pid");
    assert_eq!(observed_pid, Some(stdout_pid));
}

#[test]
fn multi_metric_table_uses_one_ledger_snapshot() {
    struct MutateOnOutput {
        path: PathBuf,
        bytes: Vec<u8>,
        replaced: bool,
    }
    impl std::io::Write for MutateOnOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.replaced {
                self.replaced = true;
                fs::write(&self.path, b"{\"exit\":0,\"group\":\"later\",\"seed\":0,\"metrics\":{\"a\":99,\"b\":99}}\n")?;
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    let flag = AtomicBool::new(false);
    let c = f.context(&flag);
    f.write(
        "docs/metric-contract.json",
        br#"{"schema":1,"metrics":{"a":{"direction":"max"},"b":{"direction":"max"}}}"#,
    );
    f.write(
        "docs/EXPERIMENTS.jsonl",
        b"invalid\n{\"exit\":0,\"group\":\"original\",\"seed\":0,\"metrics\":{\"a\":1,\"b\":2}}\n",
    );
    let mut out = MutateOnOutput {
        path: f.0.join("docs/EXPERIMENTS.jsonl"),
        bytes: vec![],
        replaced: false,
    };
    let mut err = vec![];
    let code = run(
        "run-ledger",
        &["table".into(), "--mixed".into()],
        &c,
        &mut NoServices,
        &mut out,
        &mut err,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let text = String::from_utf8(out.bytes).unwrap();
    assert_eq!(text.matches("original").count(), 2);
    assert!(!text.contains("later"));
}
