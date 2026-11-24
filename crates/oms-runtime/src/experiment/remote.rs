//! Remote experiments reuse SSH operation custody and the existing run stream.
//! A terminal command is not a valid metric until its bounded result bundle
//! matches the exact experiment, seed, checkout and physical operation.
use super::*;
use std::ffi::OsString;

const MAX_METRICS: usize = 64 * 1024;
const IDENTITY_FIELDS: &[&str] = &[
    "run_id",
    "experiment_id",
    "contract_digest",
    "snapshot",
    "arm",
    "seed",
];

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn run_identity(value: &str) -> Result<()> {
    id(value)?;
    require(
        value.starts_with("experiment-run-") && value.len() <= 64 && !value.contains(".."),
        "remote --run-id must start with experiment-run- and contain at most 64 characters",
    )
}

fn latest(rt: &Runtime<'_>, run_id: &str) -> Result<Option<Value>> {
    rt.invalidate(Path::new(RUNS));
    Ok(rt
        .rows(RUNS)?
        .into_iter()
        .rfind(|row| row.text("run_id") == run_id))
}

fn terminal(row: &Value) -> bool {
    matches!(
        row.text("state"),
        "succeeded" | "failed" | "cancelled" | "lost" | "invalid_result"
    )
}

fn same_run(actual: &Value, expected: &Value) -> bool {
    IDENTITY_FIELDS
        .iter()
        .copied()
        .chain([
            "schema",
            "profile",
            "target",
            "target_fingerprint",
            "host",
            "remote_dir",
            "request_sha256",
            "command_sha256",
            "execution_operation_id",
            "execution_receipt",
        ])
        .all(|name| actual.get(name) == expected.get(name))
}

fn outcome(row: Value) -> (Value, i32) {
    let code = row.get("exit").int().ok().unwrap_or(75) as i32;
    (row, code)
}

fn packet_identity(row: &Value) -> Result<Value> {
    let mut identity = object([("schema", number(1))]);
    for name in IDENTITY_FIELDS {
        identity.set(name, row.get(name).clone())?;
    }
    Ok(identity)
}

fn wrapped_command(row: &Value, command: &[OsString]) -> Result<Vec<OsString>> {
    let identity = packet_identity(row)?;
    let mut prefix = String::from_utf8(canonical(&identity)?).map_err(error)?;
    prefix.pop();
    prefix.push_str(",\"metrics\":");
    let script = format!(
        r#"set -eu
test -n "${{OMS_REMOTE_OP_DIR:-}}"
OMS_EXPERIMENT_METRICS="$OMS_REMOTE_OP_DIR/experiment-metrics.json"
test ! -e "$OMS_EXPERIMENT_METRICS" && test ! -L "$OMS_EXPERIMENT_METRICS"
export OMS_EXPERIMENT_METRICS
export OMS_EXPERIMENT_RUN_ID={run_id}
export OMS_EXPERIMENT_ID={experiment_id}
export OMS_EXPERIMENT_ARM={arm}
export OMS_EXPERIMENT_SEED={seed}
export OMS_EXEC_SNAPSHOT_SHA256={snapshot}
# Keep the result channel free of arbitrary training output. The operation's
# existing bounded stderr log retains the command's diagnostics.
"$@" >&2
test -f "$OMS_EXPERIMENT_METRICS" && test ! -L "$OMS_EXPERIMENT_METRICS"
size=$(wc -c < "$OMS_EXPERIMENT_METRICS" | tr -d ' \t\r\n')
test "$size" -le {max_metrics}
printf '%s' {prefix}
cat -- "$OMS_EXPERIMENT_METRICS"
printf '}}\n'
"#,
        run_id = quote(row.text("run_id")),
        experiment_id = quote(row.text("experiment_id")),
        arm = quote(row.text("arm")),
        seed = quote(&String::from_utf8(canonical(row.get("seed"))?).map_err(error)?),
        snapshot = quote(row.text("snapshot")),
        max_metrics = MAX_METRICS,
        prefix = quote(&prefix),
    );
    let mut argv = vec![
        "sh".into(),
        "-c".into(),
        script.into(),
        "oms-experiment".into(),
    ];
    argv.extend(command.iter().cloned());
    Ok(argv)
}

fn transport_args(rt: &Runtime<'_>, row: &Value, action: &str) -> Vec<OsString> {
    let mut args = vec![
        action.into(),
        "--repo".into(),
        rt.repo.as_os_str().to_owned(),
        "--target".into(),
        row.text("target").into(),
        "--host".into(),
        row.text("host").into(),
        "--remote-dir".into(),
        row.text("remote_dir").into(),
        "--op-id".into(),
        row.text("run_id").into(),
    ];
    if action == "reconcile" {
        args.extend(["--timeout-seconds".into(), "10".into()]);
    }
    args
}

fn finish(rt: &Runtime<'_>, initial: &Value, transport_exit: Option<i32>) -> Result<(Value, i32)> {
    let guard = rt.lock(Path::new(RUNS))?;
    let current = latest(rt, initial.text("run_id"))?
        .ok_or_else(|| error("remote experiment launch intent disappeared"))?;
    require(
        same_run(&current, initial),
        "remote experiment identity changed during reconciliation",
    )?;
    if terminal(&current) {
        return Ok(outcome(current));
    }
    let observation = crate::ssh_adapter::experiment_receipt(&rt.repo, &current)?;
    let mut row = current.clone();
    row.set("state", string("unknown"))?;
    row.set("exit", number(75))?;
    row.set("metrics_valid", b(false))?;
    row.set("reconcile_required", b(true))?;
    if let Some(code) = transport_exit {
        row.set("transport_exit", number(code))?;
    }
    if let Some(observation) = observation {
        let state = observation.text("state");
        require(
            [
                "intent",
                "unknown",
                "running",
                "succeeded",
                "failed",
                "cancelled",
                "lost",
            ]
            .contains(&state),
            "invalid remote operation receipt state",
        )?;
        row.set("remote_state", string(state))?;
        match observation.text("state") {
            "succeeded" => {
                let meta = observation.get("remote_meta");
                let validation = (|| -> Result<Value> {
                    require(
                        meta.text("command_sha256") == row.text("command_sha256")
                            && meta.text("snapshot") == row.text("snapshot"),
                        "remote completion has no matching execution metadata",
                    )?;
                    let raw = observation.text("stdout");
                    require(
                        raw.len() <= MAX_METRICS + 4096,
                        "remote experiment result exceeds the byte limit",
                    )?;
                    let packet = parse(raw.as_bytes())?;
                    let mut required = vec!["schema", "metrics"];
                    required.extend(IDENTITY_FIELDS);
                    keys(&packet, &required, &[])?;
                    require(
                        packet.get("schema").is_int(1)
                            && IDENTITY_FIELDS
                                .iter()
                                .all(|name| packet.get(name) == row.get(name)),
                        "remote result belongs to another experiment, seed or snapshot",
                    )?;
                    packet.get("metrics").object()?;
                    Ok(packet)
                })();
                row.set("command_exit", number(0))?;
                row.set("reconcile_required", b(false))?;
                match validation {
                    Ok(packet) => {
                        let contract = load(rt, row.text("experiment_id"))?;
                        require(
                            contract.get("contract_digest") == row.get("contract_digest"),
                            "registered contract changed since the remote run started",
                        )?;
                        let data = packet.get("metrics");
                        let mut metrics = empty();
                        for (name, _) in contract.get("metric_specs").object()? {
                            let name = std::str::from_utf8(name).map_err(error)?;
                            if let Some(value) = finite(data.get(name)) {
                                metrics.set(name, number(value))?;
                            }
                        }
                        let primary = contract.text("primary_metric");
                        let mut required = BTreeSet::from([primary.to_owned()]);
                        for (name, _) in contract.get("success").get("no_regression").object()? {
                            required.insert(String::from_utf8_lossy(name).into_owned());
                        }
                        let missing: Vec<_> = required
                            .into_iter()
                            .filter(|name| metrics.opt(name).is_none())
                            .collect();
                        let valid = missing.is_empty();
                        row.set("metric", string(primary))?;
                        row.set("metric_value", metrics.get(primary).clone())?;
                        row.set("metrics", metrics)?;
                        row.set("metrics_digest", string(sha(canonical(data)?)))?;
                        row.set("result_bundle_sha256", string(sha(canonical(&packet)?)))?;
                        row.set(
                            "missing_required_metrics",
                            list(missing.iter().map(String::as_str)),
                        )?;
                        row.set("metrics_valid", b(valid))?;
                        row.set("stale_metrics", b(false))?;
                        row.set(
                            "state",
                            string(if valid { "succeeded" } else { "invalid_result" }),
                        )?;
                        row.set("exit", number(if valid { 0 } else { 3 }))?;
                        if !valid {
                            row.set(
                                "metric_error",
                                string("required metrics are missing or non-finite"),
                            )?;
                        }
                    }
                    Err(_) => {
                        row.set("state", string("invalid_result"))?;
                        row.set("exit", number(3))?;
                        row.set("metric_error", string("remote result bundle is malformed or does not match the run identity"))?;
                    }
                }
            }
            state @ ("failed" | "cancelled" | "lost") => {
                row.set("state", string(state))?;
                let exit = observation.get("exit_code").int().unwrap_or(1);
                if observation.text("failure_kind") == "snapshot_drift" {
                    require(
                        state == "failed" && exit == 2,
                        "invalid snapshot drift receipt",
                    )?;
                    row.set("failure_kind", string("snapshot_drift"))?;
                } else {
                    row.set("command_exit", number(exit))?;
                }
                row.set("exit", number(if exit == 0 { 1 } else { exit }))?;
                row.set("reconcile_required", b(false))?;
            }
            "running" => row.set("state", string("running"))?,
            _ => {}
        }
    } else {
        row.set("reconcile_error", string("missing_operation_receipt"))?;
        return Ok(outcome(row));
    }
    if row == current {
        return Ok(outcome(current));
    }
    row.set("updated_at", string(&rt.now))?;
    rt.append_locked(Path::new(RUNS), &row, &guard)?;
    Ok(outcome(row))
}

pub(super) fn run(rt: &Runtime<'_>, r: &Request) -> Result<(Value, i32)> {
    rt.parent_write()?;
    require(
        rt.repo == rt.state_repo,
        "remote experiments currently require the project's common checkout",
    )?;
    require(!r.tail.is_empty(), "command after -- is required")?;
    require(
        [
            "metrics",
            "allow-existing-metrics",
            "image",
            "adapter",
            "worktree",
            "timeout-seconds",
        ]
        .iter()
        .all(|name| !r.has(name)),
        "remote experiments use OMS_EXPERIMENT_METRICS and the existing durable SSH operation; local metrics/backend options do not apply",
    )?;
    let name = r.text("id", "")?;
    let contract = load(rt, name)?;
    let arm = r.text("arm", "")?;
    require(
        ["baseline", "treatment"].contains(&arm),
        "invalid experiment arm",
    )?;
    let seed = r.integer_value("seed", 0)?;
    require(
        rows(contract.get("seeds")).contains(&seed),
        "seed was not pre-registered",
    )?;
    let target = r.text("target", "")?;
    let snapshot = r.text("snapshot", "")?;
    require(
        !target.is_empty() && !snapshot.is_empty(),
        "remote experiments require --target and --snapshot",
    )?;
    let registry = crate::targets::listing(rt)?;
    let route = registry.get("targets").get(target);
    require(
        crate::targets::approved(route)?,
        "target execution is not approved",
    )?;
    let retained_snapshot = crate::ssh_adapter::experiment_snapshot(
        &rt.repo,
        target,
        route.text("ssh_alias"),
        route.text("workdir"),
        snapshot,
    )?;
    let command = r
        .tail
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(string)
                .ok_or_else(|| error("remote command requires Unicode text"))
        })
        .collect::<Result<Vec<_>>>()?;
    let request = object([
        ("schema", number(1)),
        ("experiment_id", string(name)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("arm", string(arm)),
        ("seed", seed.clone()),
        ("target", string(target)),
        (
            "target_fingerprint",
            route.get("approved_fingerprint").clone(),
        ),
        ("snapshot", string(snapshot)),
        ("command", a(command)),
    ]);
    let request_digest = sha(canonical(&request)?);
    let default_run = format!("experiment-run-{}", &request_digest[..32]);
    let run_id = r.text("run-id", &default_run)?;
    run_identity(run_id)?;
    let mut row = object([
        ("schema", number(1)),
        ("profile", string("remote")),
        ("run_id", string(run_id)),
        ("experiment_id", string(name)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("arm", string(arm)),
        ("seed", seed),
        ("target", string(target)),
        ("host", route.get("ssh_alias").clone()),
        ("remote_dir", route.get("workdir").clone()),
        (
            "target_fingerprint",
            route.get("approved_fingerprint").clone(),
        ),
        ("snapshot", string(snapshot)),
        ("request_sha256", string(request_digest)),
        ("state", string("launching")),
        ("created_at", string(&rt.now)),
        ("metrics_valid", b(false)),
        ("exit", Value::Null),
        ("execution_operation_id", string(run_id)),
        (
            "execution_receipt",
            string(format!(".oms/runtime/remote-ops/{run_id}.json")),
        ),
    ]);
    let command = wrapped_command(&row, &r.tail)?;
    row.set(
        "command_sha256",
        string(crate::ssh_adapter::command_digest(&command)?),
    )?;
    let launch = {
        let guard = rt.lock(Path::new(RUNS))?;
        if let Some(existing) = latest(rt, run_id)? {
            require(
                same_run(&existing, &row),
                "conflicting reuse of remote experiment --run-id",
            )?;
            if terminal(&existing) {
                return Ok(outcome(existing));
            }
            row = existing;
            false
        } else {
            crate::ssh_adapter::verify_experiment_checkout(&rt.repo, &retained_snapshot)?;
            let (reserved, reason) =
                reserve(rt, &contract, name, run_id, r.text("reason", "")?, "remote")?;
            if !reason.is_empty() {
                row.set("budget_override_reason", string(reason))?;
            }
            // The durable intent counts against the run budget even when the
            // local caller dies. Only reconciliation can resolve a remote launch.
            rt.append_locked(Path::new(RUNS), &row, &guard)?;
            if reserved {
                settle(
                    rt,
                    &contract,
                    run_id,
                    "used",
                    "remote launch intent recorded",
                )?;
            }
            true
        }
    };
    if !launch {
        return finish(rt, &row, None);
    }
    let mut args = transport_args(rt, &row, "exec");
    args.extend(["--snapshot".into(), snapshot.into(), "--".into()]);
    args.extend(command);
    // Only the caller that recorded the initial experiment intent reaches exec.
    let response = crate::ssh_adapter::run(&args, &rt.repo, rt.interrupted);
    finish(rt, &row, Some(response.code))
}

pub(super) fn reconcile(rt: &Runtime<'_>, r: &Request) -> Result<(Value, i32)> {
    rt.parent_write()?;
    let run_id = r.text("run-id", "")?;
    run_identity(run_id)?;
    let row = latest(rt, run_id)?.ok_or_else(|| error("unknown remote experiment run"))?;
    require(
        row.text("profile") == "remote",
        "run is not a remote experiment",
    )?;
    if terminal(&row) {
        return Ok(outcome(row));
    }
    require(
        rt.repo == rt.state_repo,
        "remote experiments currently require the project's common checkout",
    )?;
    let registry = crate::targets::listing(rt)?;
    let route = registry.get("targets").get(row.text("target"));
    require(
        crate::targets::approved(route)?
            && route.get("approved_fingerprint") == row.get("target_fingerprint")
            && route.get("ssh_alias") == row.get("host")
            && route.get("workdir") == row.get("remote_dir"),
        "remote target identity or execution approval changed",
    )?;
    let receipt = crate::ssh_adapter::experiment_receipt(&rt.repo, &row)?;
    let transport_exit = receipt
        .is_some_and(|receipt| matches!(receipt.text("state"), "intent" | "running" | "unknown"))
        .then(|| {
            let args = transport_args(rt, &row, "reconcile");
            crate::ssh_adapter::run(&args, &rt.repo, rt.interrupted).code
        });
    finish(rt, &row, transport_exit)
}
