use oms_command_slurm_lint::{owns, run_with_source};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn canonical_static_warnings_and_resource_preview_never_execute_the_script() {
    let snapshot = b"- QOS default: high\n## Associations (current user)\nCluster|Partition|GrpTRES|MaxTRES|MaxTRESPJ\nc|gpu|gres/gpu=8||gres/gpu=4\n## QOS\nName|PreemptMode|MaxTRESPJ|MaxTRESPU|GrpTRES|MaxWall\nhigh|REQUEUE|gres/gpu=2|gres/gpu=6||2-00:00:00\n";
    let fixture = Scratch::case(b"#!/bin/bash\n#SBATCH -o run.log\n#SBATCH --array=0-9\n#SBATCH -t 3-00:00:00\n: > executed\n#SBATCH --requeue\n", Some(snapshot));
    assert_result(
        fixture.lint(),
        1,
        &lint_golden(
            "job.sh",
            5,
            "warn: log: --output run.log is not job-specific: add %j so runs do not overwrite each other\nwarn: array: --array 0-9 has no concurrency limit: add %N (for example 0-9%8)\nwarn: requeue: QOS high preempts by REQUEUE but the script does not ask for --requeue: whether it returns to the queue then depends on the cluster's JobRequeue, which the snapshot does not record\nwarn: signal: QOS high requeues, and the script has neither --signal nor a trap: the run gets no warning to write a checkpoint\nwarn: maxwall: --time 3-00:00:00 is longer than QOS high MaxWall 2-00:00:00\n",
        ),
        b"",
    );
    assert!(!fixture.0.join("executed").exists());
    fs::write(fixture.0.join("job.sh"), b"#!/bin/bash\n#SBATCH --gres=gpu:a100:2\n#SBATCH -t 1-00:00:00\n#SBATCH --array=0-9%4\n: > executed\n").unwrap();
    let preview = fixture.preview();
    assert_eq!(preview.0, 0);
    assert!(preview.2.is_empty());
    let preview = String::from_utf8(preview.1).unwrap();
    for line in [
        "GPU-hours, upper bound       480.0",
        "GPUs held at once, peak      8",
        "QOS high MaxTRESPJ                           gres/gpu=2    per job asks 2    within",
        "OVER by 2",
        "wall asks 1-00:00:00  within",
    ] {
        assert!(preview.contains(line), "{line}");
    }
    assert!(!fixture.0.join("executed").exists());
}

#[test]
fn arguments_template_roots_and_interrupt_have_exact_results() {
    let fixture = Scratch::case(b"#SBATCH --time=10\n", None);
    assert!(owns(&[]));
    assert!(owns(&args(&["--nonsense"])));
    let help = fixture.call(&["--help"]);
    assert_eq!(help.0, 0);
    assert!(help.1.starts_with(b"Usage: slurm-lint.sh"));
    assert!(help.2.is_empty());
    assert_result(fixture.call(&[]), 2, b"", &help.1);
    for (values, error) in [
        (vec!["--snapshot"], "--snapshot requires a file"),
        (vec!["--unknown", "--help"], "unknown argument: --unknown"),
        (vec!["absent.sh"], "cannot read script: absent.sh"),
        (vec!["job.sh", "other.sh"], "one script at a time"),
        (
            vec!["job.sh", "--template"],
            "--template takes no other argument",
        ),
    ] {
        assert_result(
            fixture.call(&values),
            2,
            b"",
            format!("error: {error}\n").as_bytes(),
        );
    }
    assert_eq!(fixture.call(&["--help", "--unknown"]), help);
    assert_result(
        fixture.call(&["--template"]),
        0,
        b"#!/bin/bash\n# synthetic template\n",
        b"",
    );
    assert_result(
        fixture.call(&["--preview", "--template"]),
        0,
        b"#!/bin/bash\n# synthetic template\n",
        b"",
    );
    let missing_root = fixture.0.join("missing-source");
    let mut missing = b"error: template missing: ".to_vec();
    missing.extend_from_slice(
        missing_root
            .join("templates/slurm-job.sh")
            .as_os_str()
            .as_encoded_bytes(),
    );
    missing.push(b'\n');
    assert_result(
        run_with_source(
            &args(&["--template"]),
            &fixture.0,
            &missing_root,
            &AtomicBool::new(false),
        ),
        2,
        b"",
        &missing,
    );
    assert_result(
        run_with_source(
            &args(&["job.sh", "--snapshot", "snapshot.md"]),
            &fixture.0,
            &fixture.0.join("source"),
            &AtomicBool::new(true),
        ),
        130,
        b"",
        b"error: interrupted\n",
    );
}

#[test]
fn malformed_tres_and_large_integer_requests_preserve_unknown_and_exact_counts() {
    let fixture = Scratch::case(
        b"#SBATCH --gpus=18446744073709551617\n#SBATCH --time=1\n#SBATCH --array=0-1%2\n",
        Some(b"- QOS default: high\n## QOS\nName|PreemptMode|MaxTRESPJ\nhigh|CANCEL|gres/gpu:=4\n"),
    );
    let result = fixture.preview();
    assert_eq!(result.0, 0);
    assert!(result.2.is_empty());
    let output = String::from_utf8(result.1).unwrap();
    assert!(output.contains("GPUs held at once, peak      36893488147419103234"));
    assert!(!output.contains("QOS high MaxTRESPJ"));
}

fn assert_result(actual: (i32, Vec<u8>, Vec<u8>), code: i32, out: &[u8], err: &[u8]) {
    assert_eq!(actual.0, code);
    assert!(
        actual.1 == out,
        "stdout actual={:?} expected={:?}",
        String::from_utf8_lossy(&actual.1),
        String::from_utf8_lossy(out)
    );
    assert!(
        actual.2 == err,
        "stderr actual={:?} expected={:?}",
        String::from_utf8_lossy(&actual.2),
        String::from_utf8_lossy(err)
    );
}

impl Scratch {
    fn case(script: &[u8], snapshot: Option<&[u8]>) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "oms-slurm-case-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("source/templates")).unwrap();
        fs::write(
            root.join("source/templates/slurm-job.sh"),
            b"#!/bin/bash\n# synthetic template\n",
        )
        .unwrap();
        fs::write(root.join("job.sh"), script).unwrap();
        if let Some(snapshot) = snapshot {
            fs::write(root.join("snapshot.md"), snapshot).unwrap();
        }
        Self(root)
    }

    fn call(&self, values: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        run_with_source(
            &args(values),
            &self.0,
            &self.0.join("source"),
            &AtomicBool::new(false),
        )
    }

    fn lint(&self) -> (i32, Vec<u8>, Vec<u8>) {
        self.call(&["job.sh", "--snapshot", "snapshot.md"])
    }

    fn preview(&self) -> (i32, Vec<u8>, Vec<u8>) {
        self.call(&["--preview", "job.sh", "--snapshot", "snapshot.md"])
    }
}

fn lint_golden(label: &str, count: usize, lines: &str) -> Vec<u8> {
    format!("slurm-lint: {label}: {count} warning(s); static check, nothing was submitted or run\n{lines}").into_bytes()
}

fn preview_golden(snapshot_line: &str, fields: [&str; 5], limits: &str) -> Vec<u8> {
    let [gpus, wall, tasks, hours, peak] = fields;
    format!("slurm-preview: job.sh (read from #SBATCH lines only; nothing was submitted, Slurm --test-only was not used)\n{snapshot_line}\n\nrequest\n  GPUs per array task          {gpus}\n  wall time                    {wall}\n  array tasks                  {tasks}\n  GPU-hours, upper bound       {hours}\n  GPUs held at once, peak      {peak}\n\nlimits recorded in the snapshot (a limit not listed here is not recorded, which is not the same as no limit)\n{limits}  GPU-hour budgets (GrpTRESMins) are not in the snapshot, so the GPU-hours above have no limit to compare with.\n").into_bytes()
}

const NO_SNAPSHOT: &str = "snapshot: none readable at snapshot.md; run: oms snapshot --cluster";
const NO_QOS: &str = "  QOS: none named and no snapshot default\n";
const UNKNOWN_PREEMPTION: &str =
    "note: preemption: unknown (no cluster snapshot; run: oms snapshot --cluster)\n";

#[test]
fn multibyte_short_options_and_crlf_never_panic() {
    for script in [
        "#SBATCH -évalue\n#SBATCH --time=10\n",
        "#SBATCH --time=10\r\ntrue\r\n",
    ] {
        let fixture = Scratch::case(script.as_bytes(), None);
        assert_eq!(
            fixture.lint(),
            (0, lint_golden("job.sh", 0, UNKNOWN_PREEMPTION), Vec::new())
        );
    }
}

#[test]
fn warning_status_is_independent_of_the_script_label() {
    let fixture = Scratch::case(b"true\n", None);
    let label = "x: 0 warning(s); y.sh";
    fs::rename(fixture.0.join("job.sh"), fixture.0.join(label)).unwrap();
    assert_eq!(
        fixture.call(&[label, "--snapshot", "snapshot.md"]),
        (
            1,
            lint_golden(
                label,
                1,
                &format!(
                    "warn: time: no --time: the queue's default or maximum decides how long this runs\n{UNKNOWN_PREEMPTION}"
                )
            ),
            Vec::new()
        )
    );
}

#[test]
fn empty_snapshot_and_default_reset_remain_distinct_from_missing() {
    let empty = Scratch::case(b"#SBATCH --time=10\n", Some(b""));
    assert_eq!(
        empty.lint(),
        (
            0,
            lint_golden(
                "job.sh",
                0,
                "note: preemption: unknown (the snapshot has no QOS table)\n"
            ),
            Vec::new()
        )
    );
    let reset = Scratch::case(b"#SBATCH --time=10\n", Some(b"- QOS default: high\n- QOS default: (none)\n## QOS\nName|PreemptMode|MaxWall\nhigh|REQUEUE|01:00:00\n"));
    assert_eq!(
        reset.lint(),
        (
            0,
            lint_golden(
                "job.sh",
                0,
                "note: preemption: unknown (the script names no --qos and the snapshot has no QOS default)\n"
            ),
            Vec::new()
        )
    );
    let empty_key = Scratch::case(
        b"#SBATCH --time=10\n",
        Some(b"## QOS\nName|MaxTRESPJ|MaxWall\n|gres/gpu=1|00:01:00\n"),
    );
    assert_eq!(
        empty_key.preview(),
        (
            0,
            preview_golden(
                "snapshot: snapshot.md (no Generated line)",
                [
                    "0  (no GPU option in the #SBATCH lines)",
                    "10 (0.17 h)",
                    "1  (no --array: one job)",
                    "0.0  (GPUs x wall x tasks, as if every task ran to its limit)",
                    "0",
                ],
                NO_QOS,
            ),
            Vec::new(),
        )
    );
}

#[test]
fn python_shlex_quoted_escapes_comments_and_last_option_win() {
    let script = format!(
        "#SBATCH --time=1\n#SBATCH -t 10\n#SBATCH --output=ignored\n#SBATCH -o \"a\\$b\\{}c # d\" # --time=100\n#SBATCH --error=word# --array=0-4\n",
        char::from(96)
    );
    let fixture = Scratch::case(script.as_bytes(), None);
    let expected = format!(
        "warn: log: --output a\\$b\\{}c # d is not job-specific: add %j so runs do not overwrite each other\nwarn: log: --error word is not job-specific: add %j so runs do not overwrite each other\n{UNKNOWN_PREEMPTION}",
        char::from(96)
    );
    assert_eq!(
        fixture.lint(),
        (1, lint_golden("job.sh", 2, &expected), Vec::new())
    );
}

#[test]
fn leading_dash_time_and_strict_array_ranges_match_reference() {
    let cases = [
        (
            "-30",
            "0-0:1",
            "-30 (0.50 h)",
            "1  (--array=0-0:1)",
            "0.5",
            "1",
        ),
        (
            "-0",
            "5:2",
            "-0 (0.00 h)",
            "unknown  (--array=5:2 not understood)",
            "unknown",
            "unknown",
        ),
        (
            "10",
            " 1-5:2 , 8 %0",
            "10 (0.17 h)",
            "4  (--array= 1-5:2 , 8 %0)",
            "0.7",
            "4  (no %N limit: every task may run together)",
        ),
        (
            "bad",
            "0-4:",
            "unknown (no --time, or not understood)",
            "unknown  (--array=0-4: not understood)",
            "unknown",
            "unknown",
        ),
        (
            "0",
            "0-0:0",
            "0 (0.00 h)",
            "unknown  (--array=0-0:0 not understood)",
            "unknown",
            "unknown",
        ),
        (
            "1-2:03:04",
            "0-1%2",
            "1-2:03:04 (26.05 h)",
            "2  (--array=0-1%2)",
            "52.1",
            "2",
        ),
    ];
    for (wall, array, shown_wall, tasks, hours, peak) in cases {
        let fixture = Scratch::case(
            format!("#SBATCH --gpus=1\n#SBATCH --time={wall}\n#SBATCH --array='{array}'\n")
                .as_bytes(),
            None,
        );
        let hours = if hours == "unknown" {
            hours.to_owned()
        } else {
            format!("{hours}  (GPUs x wall x tasks, as if every task ran to its limit)")
        };
        assert_eq!(
            fixture.preview(),
            (
                0,
                preview_golden(
                    NO_SNAPSHOT,
                    ["1  (--gpus=1)", shown_wall, tasks, &hours, peak],
                    NO_QOS
                ),
                Vec::new()
            ),
            "wall={wall:?} array={array:?}"
        );
    }
}

#[test]
fn gpu_request_labels_parentheses_and_task_defaults_match_reference() {
    let cases = [
        (
            "--gpus-per-task=bad --ntasks=3",
            "unknown  (--gpus-per-task=bad, task count not stated)",
            "unknown",
            "unknown",
        ),
        (
            "--gres=gpu:a100(S:0,1):2",
            "2  (--gres=gpu:a100(S:0,1):2 x 1 node(s))",
            "2.0",
            "2",
        ),
        (
            "--gres=gpu:a100(S:0",
            "0  (--gres=gpu:a100(S:0 x 1 node(s))",
            "0.0",
            "0",
        ),
        (
            "--gpus-per-task=2 --nodes=1-3 --ntasks-per-node=4",
            "24  (--gpus-per-task=2 x 12 task(s))",
            "24.0",
            "24",
        ),
        (
            "--gpus-per-node=2 --nodes=0",
            "2  (--gpus-per-node=2 x 1 node(s))",
            "2.0",
            "2",
        ),
        (
            "--gpus=3 --gpus-per-node=8 --gpus-per-task=9 --gres=gpu:20",
            "3  (--gpus=3)",
            "3.0",
            "3",
        ),
        (
            "--gpus-per-task=2 --ntasks=0",
            "0  (--gpus-per-task=2 x 0 task(s))",
            "0.0",
            "0",
        ),
    ];
    for (options, gpus, hours, peak) in cases {
        let fixture = Scratch::case(format!("#SBATCH --time=60 {options}\n").as_bytes(), None);
        assert_eq!(
            fixture.preview(),
            (
                0,
                preview_golden(
                    NO_SNAPSHOT,
                    [
                        gpus,
                        "60 (1.00 h)",
                        "1  (no --array: one job)",
                        &if hours == "unknown" {
                            hours.to_owned()
                        } else {
                            format!(
                                "{hours}  (GPUs x wall x tasks, as if every task ran to its limit)"
                            )
                        },
                        peak
                    ],
                    NO_QOS
                ),
                Vec::new()
            ),
            "{options}"
        );
    }
}

#[test]
fn empty_partition_and_typed_tres_use_all_relevant_associations() {
    let fixture = Scratch::case(b"#SBATCH --time=60 --gpus=3 -p ''\n", Some(b"## Associations (current user)\nPartition|MaxTRESPJ|MaxTRES|GrpTRES\ngpu|gres/gpu:a=2,gres/gpu:b=2||gres/gpu=8\nother|gres/gpu:a=9,gres/gpu=1|gres/gpu=2|gres/gpu=4\n"));
    let limits = "  QOS: none named and no snapshot default\n  association MaxTRESPJ (2 row(s), lowest)     gres/gpu=1    per job asks 3    OVER by 2\n  association MaxTRES (2 row(s), lowest)       gres/gpu=2    per job asks 3    OVER by 1\n  association GrpTRES (2 row(s), lowest)       gres/gpu=4    at once asks 3    within\n";
    assert_eq!(
        fixture.preview(),
        (
            0,
            preview_golden(
                "snapshot: snapshot.md (no Generated line)",
                [
                    "3  (--gpus=3)",
                    "60 (1.00 h)",
                    "1  (no --array: one job)",
                    "3.0  (GPUs x wall x tasks, as if every task ran to its limit)",
                    "3"
                ],
                limits
            ),
            Vec::new()
        )
    );
}

#[test]
fn log_width_tokens_only_accept_ascii_digits() {
    let fixture = Scratch::case(
        "#SBATCH --time=10 --output=log.%٣j --error=log.%03j\n".as_bytes(),
        None,
    );
    assert_eq!(
        fixture.lint(),
        (
            1,
            lint_golden(
                "job.sh",
                1,
                &format!(
                    "warn: log: --output log.%٣j is not job-specific: add %j so runs do not overwrite each other\n{UNKNOWN_PREEMPTION}"
                )
            ),
            Vec::new()
        )
    );
}

#[test]
fn timestamp_grammar_accepts_unpadded_fields_and_rejects_signs() {
    for (stamp, shown) in [
        ("9999-1-2T3:4:5Z", "9999-1-2T3:4:5Z, 0d 0h old"),
        ("9999-01-01t00:00:61z", "9999-01-01t00:00:61z, 0d 0h old"),
        ("٩٩٩٩-01- 1T00:00:00Z", "٩٩٩٩-01- 1T00:00:00Z, 0d 0h old"),
        ("9999-01-٣T00:00:00Z", "generation time unreadable"),
        ("9999-01-0٣T00:00:00Z", "generation time unreadable"),
        ("9999-01-1٣T00:00:00Z", "9999-01-1٣T00:00:00Z, 0d 0h old"),
        ("+020-01-01T00:00:00Z", "generation time unreadable"),
    ] {
        let fixture = Scratch::case(
            b"#SBATCH --time=60 --gpus=1\n",
            Some(format!("Generated: {stamp}\n").as_bytes()),
        );
        assert_eq!(
            fixture.preview(),
            (
                0,
                preview_golden(
                    &format!("snapshot: snapshot.md ({shown})"),
                    [
                        "1  (--gpus=1)",
                        "60 (1.00 h)",
                        "1  (no --array: one job)",
                        "1.0  (GPUs x wall x tasks, as if every task ran to its limit)",
                        "1"
                    ],
                    NO_QOS
                ),
                Vec::new()
            ),
            "{stamp}"
        );
    }
}

#[cfg(unix)]
#[test]
fn raw_unix_path_and_error_labels_keep_original_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Scratch::case(b"#SBATCH --time=10\n", Some(b""));
    let script = OsString::from_vec(b"job-\xff.sh".to_vec());
    let snapshot = OsString::from_vec(b"snapshot-\xfe.md".to_vec());
    fs::rename(fixture.0.join("job.sh"), fixture.0.join(&script)).unwrap();
    fs::rename(fixture.0.join("snapshot.md"), fixture.0.join(&snapshot)).unwrap();
    let result = run_with_source(
        &[script.clone(), "--snapshot".into(), snapshot.clone()],
        &fixture.0,
        &fixture.0.join("source"),
        &AtomicBool::new(false),
    );
    assert_eq!(result, (0, b"slurm-lint: job-\xff.sh: 0 warning(s); static check, nothing was submitted or run\nnote: preemption: unknown (the snapshot has no QOS table)\n".to_vec(), Vec::new()));
    let preview = run_with_source(
        &[
            "--preview".into(),
            script.clone(),
            "--snapshot".into(),
            snapshot.clone(),
        ],
        &fixture.0,
        &fixture.0.join("source"),
        &AtomicBool::new(false),
    );
    let mut expected = preview_golden(
        NO_SNAPSHOT,
        [
            "0  (no GPU option in the #SBATCH lines)",
            "10 (0.17 h)",
            "1  (no --array: one job)",
            "0.0  (GPUs x wall x tasks, as if every task ran to its limit)",
            "0",
        ],
        NO_QOS,
    );
    for (needle, replacement) in [
        (b"job.sh".as_slice(), script.as_encoded_bytes()),
        (b"snapshot.md".as_slice(), snapshot.as_encoded_bytes()),
    ] {
        let start = expected
            .windows(needle.len())
            .position(|part| part == needle)
            .unwrap();
        expected.splice(start..start + needle.len(), replacement.iter().copied());
    }
    assert_result(preview, 0, &expected, b"");
    let missing = OsString::from_vec(b"missing-\xfd.sh".to_vec());
    assert_eq!(
        run_with_source(
            &[missing],
            &fixture.0,
            &fixture.0.join("source"),
            &AtomicBool::new(false)
        ),
        (
            2,
            Vec::new(),
            b"error: cannot read script: missing-\xfd.sh\n".to_vec()
        )
    );
    let unknown = OsString::from_vec(b"--bad-\xfc".to_vec());
    assert_eq!(
        run_with_source(
            &[unknown],
            &fixture.0,
            &fixture.0.join("source"),
            &AtomicBool::new(false)
        ),
        (
            2,
            Vec::new(),
            b"error: unknown argument: --bad-\xfc\n".to_vec()
        )
    );
    let raw_source = fixture.0.join(OsString::from_vec(b"source-\xfb".to_vec()));
    fs::rename(fixture.0.join("source"), &raw_source).unwrap();
    assert_eq!(
        run_with_source(
            &args(&["--template"]),
            &fixture.0,
            &raw_source,
            &AtomicBool::new(false)
        ),
        (
            0,
            b"#!/bin/bash\n# synthetic template\n".to_vec(),
            Vec::new()
        )
    );
}

#[cfg(unix)]
#[test]
fn environment_snapshot_bytes_and_source_default_are_resolved_without_global_mutation() {
    use std::os::unix::ffi::OsStringExt;
    const CHILD_ROOT: &str = "OMS_SLURM_LINT_NATIVE_TEST_ROOT";
    const CHILD_MODE: &str = "OMS_SLURM_LINT_NATIVE_TEST_MODE";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert_eq!(
            fs::read(root.join("job.sh")).unwrap(),
            b"#SBATCH --time=10\n"
        );
        let mode = std::env::var(CHILD_MODE).unwrap();
        if mode == "raw" {
            assert_eq!(
                std::env::var_os("OH_MY_SETTING_SLURM_REF").unwrap(),
                OsString::from_vec(b"snapshot-\xfe.md".to_vec())
            );
        } else {
            assert!(
                std::env::var_os("OH_MY_SETTING_SLURM_REF").is_none_or(|value| value.is_empty())
            );
        }
        let result = run_with_source(
            &args(&["job.sh"]),
            &root,
            &root.join("source"),
            &AtomicBool::new(false),
        );
        assert_eq!(
            result,
            (
                0,
                lint_golden(
                    "job.sh",
                    0,
                    "note: preemption: QOS normal PreemptMode is CANCEL; no requeue check applies\n"
                ),
                Vec::new()
            )
        );
        return;
    }
    let fixture = Scratch::case(b"#SBATCH --time=10\n", None);
    let snapshot =
        b"- QOS default: normal\n## QOS\nName|PreemptMode|MaxWall\nnormal|CANCEL|01:00:00\n";
    let raw_name = OsString::from_vec(b"snapshot-\xfe.md".to_vec());
    fs::write(fixture.0.join(&raw_name), snapshot).unwrap();
    fs::create_dir_all(fixture.0.join("source/local")).unwrap();
    fs::write(fixture.0.join("source/local/slurm.md"), snapshot).unwrap();
    for mode in ["raw", "default", "empty"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "environment_snapshot_bytes_and_source_default_are_resolved_without_global_mutation", "--nocapture"])
            .env(CHILD_ROOT, &fixture.0).env(CHILD_MODE, mode);
        match mode {
            "raw" => {
                command.env("OH_MY_SETTING_SLURM_REF", &raw_name);
            }
            "empty" => {
                command.env("OH_MY_SETTING_SLURM_REF", "");
            }
            _ => {
                command.env_remove("OH_MY_SETTING_SLURM_REF");
            }
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "native child {mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn maxwall_equal_zero_unlimited_and_malformed_values_match_reference() {
    let snapshot =
        b"- QOS default: normal\n## QOS\nName|PreemptMode|MaxWall\nnormal|CANCEL|01:00:00\n";
    for (wall, warns) in [
        ("59", false),
        ("60", false),
        ("61", true),
        ("0", true),
        ("UNLIMITED", true),
        ("INFINITE", true),
        ("-1", true),
        ("", false),
        ("bad", false),
    ] {
        let fixture = Scratch::case(
            format!("#SBATCH --time='{wall}'\n").as_bytes(),
            Some(snapshot),
        );
        let warning = if warns {
            format!("warn: maxwall: --time {wall} is longer than QOS normal MaxWall 01:00:00\n")
        } else {
            String::new()
        };
        assert_result(
            fixture.lint(),
            i32::from(warns),
            &lint_golden(
                "job.sh",
                usize::from(warns),
                &format!(
                    "{warning}note: preemption: QOS normal PreemptMode is CANCEL; no requeue check applies\n"
                ),
            ),
            b"",
        );
    }
}

#[test]
fn requeue_signals_and_python_trap_word_boundaries_match_reference() {
    let snapshot =
        b"- QOS default: high\n## QOS\nName|PreemptMode|MaxWall\nhigh|REQUEUE|01:00:00\n";
    for (flags, body, count, detail) in [
        (
            "--requeue",
            "trap 'true' USR1",
            0,
            "note: signal: the script traps but sets no --signal, so only the scheduler's final TERM arrives\n",
        ),
        (
            "--requeue",
            "trap\u{05b0}",
            0,
            "note: signal: the script traps but sets no --signal, so only the scheduler's final TERM arrives\n",
        ),
        (
            "--requeue --signal=USR1@60",
            "trap 'true' USR1",
            1,
            "warn: signal: --signal=USR1@60 reaches the job steps only; the trap in this script needs the B: prefix (--signal=B:USR1@60) to run\n",
        ),
        ("--requeue --signal=B:USR1@60", "trap 'true' USR1", 0, ""),
        (
            "--requeue --no-requeue --signal=B:USR1@60",
            "trap 'true' USR1",
            1,
            "warn: requeue: QOS high preempts by REQUEUE but the script says --no-requeue: a preempted job ends instead of returning to the queue\n",
        ),
    ] {
        let fixture = Scratch::case(
            format!("#SBATCH --time=10 {flags}\n{body}\n").as_bytes(),
            Some(snapshot),
        );
        assert_result(
            fixture.lint(),
            i32::from(count != 0),
            &lint_golden("job.sh", count, detail),
            b"",
        );
    }
}

fn preview_start(snapshot_line: &str) -> String {
    format!(
        "slurm-preview: job.sh (read from #SBATCH lines only; nothing was submitted, Slurm --test-only was not used)\n{snapshot_line}\n\nrequest\n"
    )
}

#[test]
fn fatal_numeric_errors_preserve_exception_line_and_prior_stdout() {
    let invalid = Scratch::case("#SBATCH --time=²\n".as_bytes(), None);
    assert_result(
        invalid.preview(),
        1,
        b"",
        "ValueError: invalid literal for int() with base 10: '²'\n".as_bytes(),
    );

    let gpus = format!("1{}", "0".repeat(400));
    let overflow = Scratch::case(format!("#SBATCH --time=1 --gpus={gpus}\n").as_bytes(), None);
    let prefix = format!(
        "{}  GPUs per array task          {gpus}  (--gpus={gpus})\n  wall time                    1 (0.02 h)\n  array tasks                  1  (no --array: one job)\n",
        preview_start(NO_SNAPSHOT)
    );
    assert_result(
        overflow.preview(),
        1,
        prefix.as_bytes(),
        b"OverflowError: int too large to convert to float\n",
    );

    let wall = format!("1{}", "0".repeat(400));
    let wall_overflow = Scratch::case(format!("#SBATCH --time={wall} --gpus=0\n").as_bytes(), None);
    let prefix = format!(
        "{}  GPUs per array task          0  (--gpus=0)\n",
        preview_start(NO_SNAPSHOT)
    );
    assert_result(
        wall_overflow.preview(),
        1,
        prefix.as_bytes(),
        b"OverflowError: int too large to convert to float\n",
    );

    let high = format!("1{}", "0".repeat(400));
    let tasks = format!("1{}1", "0".repeat(399));
    let array_overflow = Scratch::case(
        format!("#SBATCH --time=1 --gpus=0 --array=0-{high}\n").as_bytes(),
        None,
    );
    let prefix = format!(
        "{}  GPUs per array task          0  (--gpus=0)\n  wall time                    1 (0.02 h)\n  array tasks                  {tasks}  (--array=0-{high})\n",
        preview_start(NO_SNAPSHOT)
    );
    assert_result(
        array_overflow.preview(),
        1,
        prefix.as_bytes(),
        b"OverflowError: int too large to convert to float\n",
    );

    let gpus = format!("1{}", "0".repeat(200));
    let tasks = format!("1{}1", "0".repeat(199));
    let peak = format!(
        "1{}1{}  (no %N limit: every task may run together)",
        "0".repeat(199),
        "0".repeat(200)
    );
    let float_infinity = Scratch::case(
        format!("#SBATCH --time=1 --gpus={gpus} --array=0-{gpus}\n").as_bytes(),
        None,
    );
    assert_result(
        float_infinity.preview(),
        0,
        &preview_golden(
            NO_SNAPSHOT,
            [
                &format!("{gpus}  (--gpus={gpus})"),
                "1 (0.02 h)",
                &format!("{tasks}  (--array=0-{gpus})"),
                "inf  (GPUs x wall x tasks, as if every task ran to its limit)",
                &peak,
            ],
            NO_QOS,
        ),
        b"",
    );
}

#[test]
fn nondecimal_digit_errors_follow_each_canonical_conversion_site() {
    for (options, digit) in [
        ("--time=፩", "፩"),
        ("--time=1 --array=0-1%²", "²"),
        ("--time=1 --gres=gpu:🄀", "🄀"),
    ] {
        let fixture = Scratch::case(format!("#SBATCH {options}\n").as_bytes(), None);
        assert_result(
            fixture.preview(),
            1,
            b"",
            format!("ValueError: invalid literal for int() with base 10: '{digit}'\n").as_bytes(),
        );
    }
    let lint = Scratch::case(
        "#SBATCH --time=²\n".as_bytes(),
        Some(b"- QOS default: normal\n## QOS\nName|PreemptMode|MaxWall\nnormal|CANCEL|01:00:00\n"),
    );
    assert_result(
        lint.lint(),
        1,
        b"",
        "ValueError: invalid literal for int() with base 10: '²'\n".as_bytes(),
    );
    let missing_snapshot = Scratch::case("#SBATCH --time=²\n".as_bytes(), None);
    assert_result(
        missing_snapshot.lint(),
        0,
        &lint_golden("job.sh", 0, UNKNOWN_PREEMPTION),
        b"",
    );

    let long_invalid = Scratch::case(
        format!("#SBATCH --time={}\n", "¹".repeat(201)).as_bytes(),
        None,
    );
    assert_result(
        long_invalid.preview(),
        1,
        b"",
        format!(
            "ValueError: invalid literal for int() with base 10: '{}\n",
            "¹".repeat(199)
        )
        .as_bytes(),
    );
    let fraction = Scratch::case("#SBATCH --time=½\n".as_bytes(), None);
    assert_result(
        fraction.preview(),
        0,
        &preview_golden(
            NO_SNAPSHOT,
            [
                "0  (no GPU option in the #SBATCH lines)",
                "unknown (no --time, or not understood)",
                "1  (no --array: one job)",
                "unknown",
                "0",
            ],
            NO_QOS,
        ),
        b"",
    );
}

#[test]
fn decimal_conversion_limits_are_local_to_python_conversion_sites() {
    const CHILD: &str = "OMS_SLURM_LINT_INTEGER_LIMIT_TEST";
    let known = [
        "0  (--gpus=0)",
        "1 (0.02 h)",
        "1  (no --array: one job)",
        "0.0  (GPUs x wall x tasks, as if every task ran to its limit)",
        "0",
    ];
    let mode = std::env::var(CHILD).unwrap_or_default();
    if mode == "disabled" || mode == "minimum" {
        let digits = if mode == "disabled" { 4301 } else { 641 };
        let fixture = Scratch::case(
            format!(
                "#SBATCH --nodes={} --ntasks=1 --gpus=0 --time=1\n",
                "1".repeat(digits)
            )
            .as_bytes(),
            None,
        );
        if mode == "disabled" {
            assert_result(
                fixture.preview(),
                0,
                &preview_golden(NO_SNAPSHOT, known, NO_QOS),
                b"",
            );
        } else {
            assert_result(fixture.preview(), 1, b"", b"ValueError: Exceeds the limit (640 digits) for integer string conversion: value has 641 digits; use sys.set_int_max_str_digits() to increase the limit\n");
        }
        return;
    }
    if mode != "child" {
        for (mode, limit) in [("child", "4300"), ("disabled", "0"), ("minimum", "640")] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "decimal_conversion_limits_are_local_to_python_conversion_sites",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env("PYTHONINTMAXSTRDIGITS", limit)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    assert_eq!(std::env::var("PYTHONINTMAXSTRDIGITS").unwrap(), "4300");
    for nodes in [
        String::new(),
        "0".into(),
        "bad".into(),
        "1".repeat(4300),
        format!("{}-1", "1".repeat(4301)),
    ] {
        let fixture = Scratch::case(
            format!("#SBATCH --nodes={nodes} --ntasks=1 --gpus=0 --time=1\n").as_bytes(),
            None,
        );
        assert_result(
            fixture.preview(),
            0,
            &preview_golden(NO_SNAPSHOT, known, NO_QOS),
            b"",
        );
    }
    let parse_error = b"ValueError: Exceeds the limit (4300 digits) for integer string conversion: value has 4301 digits; use sys.set_int_max_str_digits() to increase the limit\n";
    let format_error = b"ValueError: Exceeds the limit (4300 digits) for integer string conversion; use sys.set_int_max_str_digits() to increase the limit\n";
    for nodes in ["1".repeat(4301), "0".repeat(4301), "١".repeat(4301)] {
        let fixture = Scratch::case(
            format!("#SBATCH --nodes={nodes} --ntasks=1 --gpus=0 --time=1\n").as_bytes(),
            None,
        );
        assert_result(fixture.preview(), 1, b"", parse_error);
    }
    let equal_gpus = "1".repeat(4300);
    let equal = Scratch::case(
        format!("#SBATCH --time=1 --gpus={equal_gpus}\n").as_bytes(),
        None,
    );
    let prefix = format!(
        "{}  GPUs per array task          {equal_gpus}  (--gpus={equal_gpus})\n  wall time                    1 (0.02 h)\n  array tasks                  1  (no --array: one job)\n",
        preview_start(NO_SNAPSHOT)
    );
    assert_result(
        equal.preview(),
        1,
        prefix.as_bytes(),
        b"OverflowError: int too large to convert to float\n",
    );
    let factor = format!("1{}", "0".repeat(2150));
    let unused_product = Scratch::case(
        format!("#SBATCH --nodes={factor} --ntasks-per-node={factor} --gpus=0 --time=1\n")
            .as_bytes(),
        None,
    );
    assert_result(
        unused_product.preview(),
        0,
        &preview_golden(NO_SNAPSHOT, known, NO_QOS),
        b"",
    );
    let shown_product = Scratch::case(
        format!("#SBATCH --nodes={factor} --ntasks-per-node={factor} --gpus-per-task=0 --time=1\n")
            .as_bytes(),
        None,
    );
    assert_result(shown_product.preview(), 1, b"", format_error);
    let shown_gpus = Scratch::case(
        format!("#SBATCH --nodes={factor} --gpus-per-node={factor} --time=1\n").as_bytes(),
        None,
    );
    assert_result(
        shown_gpus.preview(),
        1,
        preview_start(NO_SNAPSHOT).as_bytes(),
        format_error,
    );
    let shown_array = Scratch::case(
        format!("#SBATCH --gpus=0 --time=1 --array=0-{}\n", "9".repeat(4300)).as_bytes(),
        None,
    );
    let prefix = format!(
        "{}  GPUs per array task          0  (--gpus=0)\n  wall time                    1 (0.02 h)\n",
        preview_start(NO_SNAPSHOT)
    );
    assert_result(shown_array.preview(), 1, prefix.as_bytes(), format_error);
    let snapshot = format!(
        "- QOS default: normal\n## QOS\nName|PreemptMode|MaxWall|MaxTRESPJ\nnormal|CANCEL|60|gres/gpu={}\n",
        "1".repeat(4301)
    );
    let limit = Scratch::case(b"#SBATCH --gpus=0 --time=1\n", Some(snapshot.as_bytes()));
    let before_limit = preview_golden("snapshot: snapshot.md (no Generated line)", known, "");
    let budget = b"  GPU-hour budgets (GrpTRESMins) are not in the snapshot, so the GPU-hours above have no limit to compare with.\n";
    assert_result(
        limit.preview(),
        1,
        before_limit.strip_suffix(budget).unwrap(),
        parse_error,
    );
    let snapshot = format!(
        "- QOS default: normal\n## QOS\nName|MaxTRESPJ\nnormal|gres/gpu:a={},gres/gpu:b={}\n",
        "9".repeat(4300),
        "9".repeat(4300)
    );
    let sum = Scratch::case(b"#SBATCH --gpus=0 --time=1\n", Some(snapshot.as_bytes()));
    assert_result(
        sum.preview(),
        1,
        before_limit.strip_suffix(budget).unwrap(),
        format_error,
    );
}
