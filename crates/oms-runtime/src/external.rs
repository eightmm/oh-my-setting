//! Exact external-result relations. There is no operation or consumption store
//! here: CI, Slurm, SSH and experiments retain their own records; the existing
//! autopilot receipt owns whether one terminal result has been consumed.
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, error, require};
use std::path::Path;

const MAX_OBSERVATION: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 4096;
const CI: &str = ".oms/ci.jsonl";
const SLURM: &str = ".oms/slurm-submits.jsonl";
const RUNS: &str = ".oms/experiments/runs.jsonl";

fn digest(v: &Value) -> Result<String> {
    Ok(sha(canonical(v)?))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !s.contains("..")
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn exact(row: &Value, id: &Value, names: &[&str]) -> bool {
    names.iter().all(|name| row.get(name) == id.get(name))
}

/// Validate the entire binding before a refresh can select a native command.
pub fn validate_binding(binding: &Value) -> Result<()> {
    keys(binding, &["schema", "source", "identity"], &[])?;
    require(
        binding.get("schema").is_int(1) && canonical(binding)?.len() <= 8192,
        "unsupported external wait schema or size",
    )?;
    let id = binding.get("identity");
    match binding.text("source") {
        "ci" => {
            keys(
                id,
                &[
                    "repository",
                    "head_sha",
                    "workflow_id",
                    "run_id",
                    "run_attempt",
                ],
                &[],
            )?;
            let parts = id.text("repository").split('/').collect::<Vec<_>>();
            require(
                parts.len() == 2 && parts.iter().all(|v| identifier(v)),
                "invalid CI repository",
            )?;
            require(
                [40, 64].contains(&id.text("head_sha").len())
                    && id
                        .text("head_sha")
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid CI head",
            )?;
            for name in ["workflow_id", "run_id", "run_attempt"] {
                require(
                    id.get(name)
                        .int()
                        .is_ok_and(|n| n > 0 && n <= u64::MAX as i128),
                    "invalid CI run identity",
                )?;
            }
        }
        "slurm" => {
            keys(id, &["key", "attempt", "job_id", "cluster_name"], &[])?;
            require(
                identifier(id.text("key")) && identifier(id.text("attempt")),
                "invalid Slurm submission identity",
            )?;
            require(
                id.text("job_id").len() <= 20
                    && id.text("job_id").bytes().all(|b| b.is_ascii_digit()),
                "invalid Slurm job identity",
            )?;
            require(
                identifier(id.text("cluster_name")),
                "Slurm wait requires an exact recorded cluster",
            )?;
        }
        "ssh" => {
            keys(id, &["op_id", "op_tag", "command_sha256", "snapshot"], &[])?;
            require(
                identifier(id.text("op_id"))
                    && identifier(id.text("op_tag"))
                    && hash(id.text("command_sha256"))
                    && (id.text("snapshot").is_empty() || hash(id.text("snapshot"))),
                "invalid SSH operation identity",
            )?;
        }
        "experiment" => {
            keys(
                id,
                &[
                    "experiment_id",
                    "run_id",
                    "contract_digest",
                    "request_sha256",
                ],
                &[],
            )?;
            require(
                identifier(id.text("experiment_id"))
                    && identifier(id.text("run_id"))
                    && hash(id.text("contract_digest"))
                    && (id.text("request_sha256").is_empty() || hash(id.text("request_sha256"))),
                "invalid experiment identity",
            )?;
        }
        _ => return Err(error("unsupported external result source")),
    }
    Ok(())
}

fn checked_json(raw: &[u8]) -> Result<Value> {
    // Typed waits reject duplicate keys and non-finite values even where a
    // legacy runtime reader remains permissive for compatibility.
    oms_command_plan::json::parse_bounded(raw, MAX_OBSERVATION)
}
fn records(rt: &Runtime<'_>, path: &str) -> Result<Vec<Value>> {
    rt.invalidate(Path::new(path));
    let Some(raw) = rt.bytes(path, MAX_OBSERVATION)? else {
        return Ok(vec![]);
    };
    require(
        raw.is_empty() || raw.last() == Some(&b'\n'),
        "external producer has a torn record tail",
    )?;
    let mut out = vec![];
    for line in raw.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        require(
            out.len() < MAX_ROWS,
            "external observation row bound exceeded",
        )?;
        out.push(checked_json(line)?);
    }
    Ok(out)
}
fn receipt(rt: &Runtime<'_>, path: &str) -> Result<Option<Value>> {
    rt.invalidate(Path::new(path));
    rt.bytes(path, 1024 * 1024)?
        .map(|raw| checked_json(&raw))
        .transpose()
}
fn projection(
    binding: &Value,
    row: Option<&Value>,
    path: &str,
    status: &str,
    outcome: &str,
    progress: bool,
) -> Result<Value> {
    let identity = digest(binding)?;
    let terminal = status == "terminal";
    let result = if terminal {
        // Ignore observation time and transport presentation. Identity and
        // terminal payload remain bound; repeated reads have the same digest.
        let fact = without(
            row.ok_or_else(|| error("terminal result missing"))?,
            &[
                "ts",
                "updated_at",
                "action",
                "deduplicated",
                "observed_at",
                "transport_exit",
            ],
        );
        string(digest(&object([
            ("binding", binding.clone()),
            ("result", fact),
        ]))?)
    } else {
        Value::Null
    };
    Ok(object([
        ("schema", number(1)),
        ("source", binding.get("source").clone()),
        ("identity_sha256", string(&identity)),
        ("binding_sha256", string(identity)),
        ("identity_valid", b(row.is_some())),
        ("status", string(status)),
        ("result_sha256", result),
        ("outcome", optional(outcome)),
        ("progress", b(terminal && progress)),
        (
            "evidence_refs",
            a(row
                .map(|row| {
                    Ok(object([
                        ("path", string(path)),
                        ("sha256", string(digest(row)?)),
                    ]))
                })
                .transpose()?),
        ),
    ]))
}

/// Read one relation. `refresh` invokes only the existing owner's bounded
/// query/reconciliation; an observation failure never falls back to old green.
pub fn observe(rt: &Runtime<'_>, binding: &Value, refresh: bool) -> Result<Value> {
    validate_binding(binding)?;
    rt.check()?;
    let id = binding.get("identity");
    let source = binding.text("source");
    // Validate local intent/custody before invoking a reconciliation command.
    if refresh && source != "ci" {
        let initial = observe(rt, binding, false)?;
        require(
            initial.get("identity_valid").truth(),
            "external wait has no matching canonical intent",
        )?;
        if initial.text("status") == "terminal" {
            return Ok(initial);
        }
    }
    if refresh {
        match source {
            "ssh" => {
                let path = format!(".oms/runtime/remote-ops/{}.json", id.text("op_id"));
                let row = receipt(rt, &path)?.ok_or_else(|| error("SSH intent disappeared"))?;
                let mut args = vec![
                    "reconcile".into(),
                    "--repo".into(),
                    rt.state_repo.as_os_str().to_owned(),
                    "--host".into(),
                    row.text("host").into(),
                    "--remote-dir".into(),
                    row.text("remote_dir").into(),
                    "--op-id".into(),
                    id.text("op_id").into(),
                    "--timeout-seconds".into(),
                    "10".into(),
                ];
                if row.get("target").truth() {
                    args.extend(["--target".into(), row.text("target").into()]);
                }
                if let Ok(port) = row.get("port").int() {
                    args.extend(["--port".into(), port.to_string().into()]);
                }
                let response = crate::ssh_adapter::run(&args, &rt.repo, rt.interrupted);
                if response.code == 130 {
                    return Err(crate::status(130, "SSH reconciliation interrupted"));
                }
                require(
                    [0, 1, 75].contains(&response.code),
                    "SSH reconciliation unavailable or timed out",
                )?;
            }
            _ => rt
                .services
                .refresh_external(&rt.repo, &rt.state_repo, binding)?,
        }
    }
    match source {
        "ci" => {
            require(
                rt.repository.head == id.text("head_sha"),
                "CI wait HEAD moved",
            )?;
            let rows = records(rt, CI)?;
            let row = rows.iter().rev().find(|row| {
                row.text("kind") == "exact-run-attempt"
                    && row
                        .get("identity")
                        .text("repository")
                        .eq_ignore_ascii_case(id.text("repository"))
                    && row.get("identity").get("run_id") == id.get("run_id")
            });
            let Some(row) = row else {
                return projection(binding, None, CI, "unknown", "", false);
            };
            require(row.get("identity") == id, "CI wait run attempt changed")?;
            let status = match row.text("status") {
                "queued" | "waiting" | "requested" | "pending" => "pending",
                "in_progress" => "running",
                "completed" => "terminal",
                _ => "unknown",
            };
            let outcome = row.text("conclusion");
            let known = [
                "success",
                "failure",
                "cancelled",
                "timed_out",
                "neutral",
                "skipped",
                "action_required",
                "startup_failure",
                "stale",
            ]
            .contains(&outcome);
            projection(
                binding,
                Some(row),
                CI,
                if status == "terminal" && !known {
                    "unknown"
                } else {
                    status
                },
                outcome,
                false,
            )
        }
        "ssh" => {
            let path = format!(".oms/runtime/remote-ops/{}.json", id.text("op_id"));
            let Some(row) = receipt(rt, &path)? else {
                return projection(binding, None, &path, "unknown", "", false);
            };
            require(
                row.get("schema").is_int(1)
                    && exact(&row, id, &["op_id", "op_tag", "command_sha256"])
                    && row.text("snapshot") == id.text("snapshot"),
                "SSH wait operation identity changed",
            )?;
            let state = row.text("state");
            let status = match state {
                "intent" => "pending",
                "running" => "running",
                "succeeded" | "failed" | "cancelled" | "lost" => "terminal",
                _ => "unknown",
            };
            if matches!(state, "succeeded" | "failed")
                && row.text("failure_kind") != "snapshot_drift"
            {
                require(
                    exact(row.get("remote_meta"), id, &["op_tag", "command_sha256"])
                        && row.get("remote_meta").text("snapshot") == id.text("snapshot")
                        && row.get("exit_code").int().is_ok(),
                    "SSH terminal record lacks exact start/result evidence",
                )?;
            }
            projection(binding, Some(&row), &path, status, state, false)
        }
        "slurm" => {
            let rows = records(rt, SLURM)?;
            let relevant = rows
                .iter()
                .filter(|row| row.get("key") == id.get("key"))
                .collect::<Vec<_>>();
            let Some(intent) = relevant
                .iter()
                .rev()
                .find(|row| row.text("type") == "intent")
            else {
                return projection(binding, None, SLURM, "unknown", "", false);
            };
            require(
                intent.get("attempt") == id.get("attempt"),
                "Slurm wait submission attempt changed",
            )?;
            let mine = relevant
                .iter()
                .copied()
                .filter(|row| row.get("attempt") == id.get("attempt"))
                .collect::<Vec<_>>();
            let submitted = mine
                .iter()
                .rev()
                .find(|row| row.text("type") == "submitted");
            let cluster_name = submitted.map_or_else(
                || intent.text("cluster_name"),
                |row| row.text("cluster_name"),
            );
            require(
                cluster_name == id.text("cluster_name")
                    && (intent.text("cluster_name").is_empty()
                        || intent.get("cluster_name") == id.get("cluster_name")),
                "Slurm wait cluster changed",
            )?;
            if let Some(submitted) = submitted {
                require(
                    submitted.get("cluster_name") == id.get("cluster_name")
                        && (id.text("job_id").is_empty()
                            || submitted.get("job_id") == id.get("job_id")),
                    "Slurm job or cluster mismatch",
                )?;
            }
            let row = mine
                .iter()
                .rev()
                .find(|row| matches!(row.text("type"), "reconciled" | "failed"))
                .copied();
            let Some(row) = row else {
                return projection(
                    binding,
                    Some(intent),
                    SLURM,
                    if submitted.is_some() {
                        "running"
                    } else {
                        "unknown"
                    },
                    "",
                    false,
                );
            };
            if row.text("type") == "failed" {
                require(
                    id.text("job_id").is_empty() && submitted.is_none(),
                    "Slurm allocated job cannot resolve from a submission failure",
                )?;
                return projection(
                    binding,
                    Some(row),
                    SLURM,
                    "terminal",
                    "submission_failed",
                    false,
                );
            }
            require(
                submitted.is_some_and(|r| r.get("job_id") == row.get("job_id")),
                "Slurm result lacks exact submitted job",
            )?;
            let state = row.text("state");
            let status = match state {
                "active" => "running",
                "succeeded"
                    if row.text("scheduler_exit") == "0:0" && row.text("source") == "sacct" =>
                {
                    "terminal"
                }
                "failed" | "oom" | "preempted" | "timeout" | "cancelled"
                    if row.text("source") == "sacct" =>
                {
                    "terminal"
                }
                _ => "unknown",
            };
            projection(binding, Some(row), SLURM, status, state, false)
        }
        "experiment" => {
            let contract = crate::experiment::load(rt, id.text("experiment_id"))?;
            require(
                contract.get("contract_digest") == id.get("contract_digest"),
                "experiment wait contract changed",
            )?;
            let records = records(rt, RUNS)?;
            let Some(row) = records
                .iter()
                .rev()
                .find(|row| row.get("run_id") == id.get("run_id"))
            else {
                return projection(binding, None, RUNS, "unknown", "", false);
            };
            require(
                exact(row, id, &["experiment_id", "run_id", "contract_digest"])
                    && row.text("request_sha256") == id.text("request_sha256"),
                "experiment result identity changed",
            )?;
            let state = row.text("state");
            let terminal = matches!(
                state,
                "succeeded" | "failed" | "cancelled" | "lost" | "invalid_result"
            ) || state.is_empty() && row.get("exit").int().is_ok();
            require(
                ["baseline", "treatment"].contains(&row.text("arm"))
                    && rows(contract.get("seeds")).contains(row.get("seed")),
                "experiment arm or seed is outside the frozen contract",
            )?;
            let valid = crate::experiment::valid_measurement(&contract, row);
            let mut projected = projection(
                binding,
                Some(row),
                RUNS,
                if terminal {
                    "terminal"
                } else if state == "running" {
                    "running"
                } else if state == "launching" {
                    "pending"
                } else {
                    "unknown"
                },
                if terminal && !valid {
                    "inconclusive"
                } else if valid {
                    "valid_measurement"
                } else {
                    ""
                },
                valid,
            )?;
            if terminal {
                if valid {
                    projected.set(
                        "progress_sha256",
                        string(digest(&object([
                            ("experiment_id", row.get("experiment_id").clone()),
                            ("contract_digest", row.get("contract_digest").clone()),
                            ("arm", row.get("arm").clone()),
                            ("seed", row.get("seed").clone()),
                        ]))?),
                    )?;
                }
                // Summarization keeps missing seeds/metrics/invariants
                // inconclusive. A valid negative hypothesis still advances
                // knowledge; it is not an infrastructure or acceptance pass.
                projected.set(
                    "experiment",
                    crate::experiment::next(rt, id.text("experiment_id"))?,
                )?;
            }
            Ok(projected)
        }
        _ => unreachable!(),
    }
}
