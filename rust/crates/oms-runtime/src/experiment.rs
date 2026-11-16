//! Experiment contracts reuse native normalization; runtime owns linkage and budgets.
use crate::parser::Request;
use crate::services::LedgerRequest;
use crate::statistics::{mean, stdev};
use crate::storage::{Runtime, random_id};
use crate::value::*;
use crate::{Result, error, require};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
const RUNS: &str = ".oms/experiments/runs.jsonl";
const RESERVATIONS: &str = ".oms/experiments/budget-reservations.jsonl";
pub fn validate(raw: &Value) -> Result<Value> {
    let native = to_native(raw);
    let report = oms_experiment::validate(&native).map_err(error)?;
    Ok(from_native(&report.normalized))
}
fn id(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 160
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid experiment id",
    )
}
pub fn load(rt: &Runtime<'_>, name: &str) -> Result<Value> {
    id(name)?;
    let raw = rt.json(
        format!(".oms/experiments/contracts/{name}.json"),
        Value::Null,
    )?;
    require(
        raw.object().is_ok(),
        format!("experiment contract not found: {name}"),
    )?;
    validate(&raw)
}
pub fn register(rt: &Runtime<'_>, raw: &Value, name: &str) -> Result<Value> {
    let mut contract = validate(raw)?;
    let default = format!("experiment-{}", &contract.text("contract_digest")[..16]);
    let name = if name.is_empty() { &default } else { name };
    id(name)?;
    let path = format!(".oms/experiments/contracts/{name}.json");
    let _guard = rt.lock(Path::new(&path))?;
    rt.invalidate(Path::new(&path));
    let old = rt.json(&path, Value::Null)?;
    require(
        old.object().is_err() || old.get("contract_digest") == contract.get("contract_digest"),
        "experiment id already names a different contract",
    )?;
    contract.set("experiment_id", string(name))?;
    contract.set("registered_at", string(&rt.now))?;
    let mut body = sorted_pretty(&contract)?;
    body.push(b'\n');
    rt.write_locked(Path::new(&path), &body, body.len(), &_guard)?;
    Ok(object([
        ("schema", number(1)),
        ("experiment_id", string(name)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("path", string(path)),
    ]))
}
fn numeric(n: Option<f64>) -> Value {
    n.map(number).unwrap_or(Value::Null)
}
fn integer_zero(value: &Value) -> Option<bool> {
    let Value::Number(raw) = value else {
        return None;
    };
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.bytes().all(|b| b == b'0'))
}
fn result_values(results: &Value, arm: &str, metric: &str, primary: &str) -> Vec<f64> {
    let arm = results.get(arm);
    let v = if metric == primary && arm.array().is_ok() {
        arm
    } else {
        arm.get(metric)
    };
    rows(v).iter().filter_map(finite).collect()
}
fn noise(contract: &Value, baseline: &[f64]) -> (Option<Value>, Option<f64>) {
    let band = contract.get("success").get("noise_band");
    if !band.truth() {
        return (None, Some(0.0));
    }
    let delta = if band.text("mode") == "fixed" {
        finite(band.get("delta"))
    } else if baseline.len() >= 2 {
        stdev(baseline).map(|s| {
            finite(band.get("z")).unwrap_or(0.0) * s * (2.0 / baseline.len() as f64).sqrt()
        })
    } else {
        None
    };
    let mut report = band.clone();
    let _ = report.set("delta", numeric(delta));
    (Some(report), delta)
}
pub fn evaluate(raw: &Value, results: &Value) -> Result<Value> {
    let contract = validate(raw)?;
    require(
        results.object().is_ok(),
        "experiment results must be a JSON object",
    )?;
    let primary = contract.text("primary_metric");
    let baseline = result_values(results, "baseline", primary, primary);
    let treatment = result_values(results, "treatment", primary, primary);
    require(
        !baseline.is_empty() && !treatment.is_empty(),
        "results must contain finite numeric baseline and treatment values",
    )?;
    let bm = mean(&baseline).unwrap();
    let tm = mean(&treatment).unwrap();
    let delta = tm - bm;
    let improvement = if contract.get("metrics").get("primary").text("direction") == "minimize" {
        -delta
    } else {
        delta
    };
    let expected = rows(contract.get("seeds")).len();
    let mut complete = baseline.len() == expected && treatment.len() == expected;
    let mut passed = true;
    let mut checks = empty();
    for (k, rule) in contract.get("success").get("no_regression").object()? {
        let name = std::str::from_utf8(k).map_err(error)?;
        let l = result_values(results, "baseline", name, primary);
        let r = result_values(results, "treatment", name, primary);
        let ready = l.len() == expected && r.len() == expected;
        let regression = if ready {
            mean(&l).zip(mean(&r)).map(|(l, r)| {
                if rule.text("direction") == "minimize" {
                    r - l
                } else {
                    l - r
                }
            })
        } else {
            None
        };
        let ok = regression.map(|v| v <= finite(rule.get("max_regression")).unwrap_or(0.0));
        complete &= ready;
        passed &= ok == Some(true);
        checks.set(
            name,
            object([
                ("regression", numeric(regression)),
                ("max_regression", rule.get("max_regression").clone()),
                ("direction", rule.get("direction").clone()),
                ("passed", ok.map(b).unwrap_or(Value::Null)),
            ]),
        )?;
    }
    let mut required = vec![];
    let mut invariant_status = empty();
    let mut invariant_complete = true;
    let mut invariant_pass = true;
    for item in rows(contract.get("invariant_pack"))
        .iter()
        .filter(|v| v.get("required") != &b(false))
    {
        let name = item.text("name");
        required.push(name.to_owned());
        let raw = results.get("invariants").get(name);
        let ok = match raw {
            Value::Bool(v) => Some(*v),
            _ => match raw.get("passed") {
                Value::Bool(v) => Some(*v),
                _ => match raw.get("exit") {
                    Value::Bool(v) => Some(!*v),
                    v => integer_zero(v),
                },
            },
        };
        invariant_complete &= ok.is_some();
        invariant_pass &= ok == Some(true);
        invariant_status.set(name, ok.map(b).unwrap_or(Value::Null))?;
    }
    let (noise, band_delta) = noise(&contract, &baseline);
    let threshold = finite(contract.get("success").get("min_improvement")).unwrap_or(0.0);
    let verdict = match band_delta {
        Some(band) if complete && invariant_complete => {
            if improvement >= threshold.max(band) && passed && invariant_pass {
                "supported"
            } else {
                "not_supported"
            }
        }
        _ => "inconclusive",
    };
    let mut out = object([
        ("schema", number(1)),
        ("metric", string(primary)),
        ("baseline_mean", number(bm)),
        ("treatment_mean", number(tm)),
        ("delta", number(delta)),
        ("improvement", number(improvement)),
        ("expected_n", number(expected)),
        ("baseline_n", number(baseline.len())),
        ("treatment_n", number(treatment.len())),
        ("no_regression", checks),
        (
            "invariants",
            object([
                ("required", list(required.iter().map(String::as_str))),
                ("status", invariant_status),
                ("complete", b(invariant_complete)),
                ("passed", b(invariant_pass)),
            ]),
        ),
        ("verdict", string(verdict)),
    ]);
    if let Some(noise) = noise {
        out.set("noise_band", noise)?;
    }
    Ok(out)
}
pub fn budget_status(rt: &Runtime<'_>, contract: &Value, experiment_id: &str) -> Result<Value> {
    let budget = contract.get("budget");
    if budget.object().is_err() {
        return Ok(Value::Null);
    }
    let mut out = empty();
    if let Some(limit) = budget.opt("runs") {
        let digest = contract.get("contract_digest");
        let mut used = BTreeSet::new();
        for (index, row) in rt.rows(RUNS)?.into_iter().enumerate() {
            if row.get("contract_digest") == digest {
                used.insert(if row.text("run_id").is_empty() {
                    format!("row-{index}")
                } else {
                    row.text("run_id").into()
                });
            }
        }
        let mut held = BTreeMap::new();
        let mut settled = BTreeMap::new();
        for row in rt.rows(RESERVATIONS)? {
            if row.get("contract_digest") != digest || row.text("run_id").is_empty() {
                continue;
            }
            if row.text("kind") == "reserved" {
                held.insert(row.text("run_id").to_owned(), row);
            } else if row.text("kind") == "settled" {
                settled.insert(
                    row.text("run_id").to_owned(),
                    row.text("outcome").to_owned(),
                );
            }
        }
        for (run_id, outcome) in &settled {
            if outcome == "used" {
                used.insert(run_id.clone());
            }
        }
        let mut live = vec![];
        let mut stale = vec![];
        for (run_id, row) in held {
            if used.contains(&run_id) || settled.contains_key(&run_id) {
                continue;
            }
            let age = chrono::DateTime::parse_from_rfc3339(row.text("created_at"))
                .ok()
                .and_then(|created| {
                    chrono::DateTime::parse_from_rfc3339(&rt.now)
                        .ok()
                        .map(|now| (now.timestamp() - created.timestamp()).max(0))
                });
            let mut item = object([
                ("run_id", string(&run_id)),
                ("pid", row.get("pid").clone()),
                ("host", row.get("host").clone()),
                ("age_seconds", age.map(number).unwrap_or(Value::Null)),
            ]);
            let gone = if age.is_some_and(|v| v > 3600) {
                if let Ok(pid) = row.get("pid").int() {
                    if let Ok(pid) = u32::try_from(pid) {
                        pid > 0
                            && rt
                                .services
                                .reservation_process_gone(row.text("host"), pid)?
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if gone {
                item.set("message",string(format!("stale reservation: run {run_id} (pid {} on {} gone, {}s old) is not counted against the budget; its row stays in .oms/experiments/budget-reservations.jsonl",row.get("pid").int().unwrap_or(0),row.text("host"),age.unwrap_or(0))))?;
                stale.push(item)
            } else {
                live.push(item)
            }
        }
        let used = used.len();
        let count = live.len();
        let mut runs = object([
            ("limit", limit.clone()),
            ("used", number(used)),
            ("reserved", number(count)),
            ("remaining", subtract_count(limit, used + count)?),
            (
                "exceeded",
                b(subtract_count(limit, used + count)?.is_int(0)),
            ),
            ("stale", a(stale)),
        ]);
        if count > 0 {
            runs.set("reserved_runs", a(live))?;
        }
        out.set("runs", runs)?;
    }
    if let Some(limit) = budget.opt("gpu_hours") {
        let mut usage = rt.services.experiment_gpu_usage(&rt.repo, experiment_id)?;
        usage.set("limit", limit.clone())?;
        usage.set(
            "exceeded",
            finite(usage.get("used"))
                .zip(finite(limit))
                .map(|(a, c)| b(a >= c))
                .unwrap_or(Value::Null),
        )?;
        out.set("gpu_hours", usage)?;
    }
    if let Some(limit) = budget.opt("final_evals") {
        out.set("final_evals",object([("limit",limit.clone()),("used",Value::Null),("note",string("not counted: this branch records no evaluation on a final split; the run-ledger test-split-count change (branch oms/run-ledger-test-split-count) counts them"))]))?;
    }
    Ok(out)
}
fn budget_refusal(runs: &Value) -> Result<String> {
    let held = runs.get("reserved").int()?;
    let count = runs.get("used").int()? + held;
    let limit = runs.get("limit").int()?;
    let detail = if held == 0 {
        "recorded for this contract".to_owned()
    } else {
        format!("recorded or reserved for this contract ({held} reserved by a run in progress)")
    };
    Ok(format!(
        "run budget spent: {count} of {limit} run(s) {detail}; pass --reason to run past it"
    ))
}
fn reserve(
    rt: &Runtime<'_>,
    contract: &Value,
    experiment_id: &str,
    run_id: &str,
    reason: &str,
    source: &str,
) -> Result<(bool, String)> {
    if contract.get("budget").opt("runs").is_none() {
        return Ok((false, String::new()));
    }
    let _guard = rt.lock(Path::new(RESERVATIONS))?;
    rt.invalidate(Path::new(RESERVATIONS));
    rt.invalidate(Path::new(RUNS));
    let budget = budget_status(rt, contract, experiment_id)?;
    let runs = budget.get("runs");
    let mut override_reason = String::new();
    if runs.get("exceeded").truth() {
        require(!reason.trim().is_empty(), budget_refusal(runs)?)?;
        override_reason = bounded(reason, 300);
    }
    let mut row = object([
        ("schema", number(1)),
        ("kind", string("reserved")),
        ("run_id", string(run_id)),
        ("experiment_id", string(experiment_id)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("source", string(source)),
        ("created_at", string(&rt.now)),
        ("pid", number(std::process::id())),
        ("host", string(rt.services.host_name()?)),
    ]);
    if !override_reason.is_empty() {
        row.set("budget_override_reason", string(&override_reason))?;
    }
    rt.append_locked(Path::new(RESERVATIONS), &row, &_guard)?;
    Ok((true, override_reason))
}
fn settle(
    rt: &Runtime<'_>,
    contract: &Value,
    run_id: &str,
    outcome: &str,
    reason: &str,
) -> Result<()> {
    let mut row = object([
        ("schema", number(1)),
        ("kind", string("settled")),
        ("run_id", string(run_id)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("outcome", string(outcome)),
        ("created_at", string(&rt.now)),
    ]);
    if !reason.is_empty() {
        row.set("reason", string(bounded(reason, 300)))?;
    }
    rt.append(Path::new(RESERVATIONS), &row)
}
pub fn record_run(rt: &Runtime<'_>, r: &Request) -> Result<(Value, i32)> {
    let name = r.text("id", "")?;
    let contract = load(rt, name)?;
    let arm = r.text("arm", "")?;
    let seed_value = r.integer_value("seed", 0)?;
    let seed = String::from_utf8(canonical(&seed_value)?).map_err(error)?;
    require(
        ["baseline", "treatment"].contains(&arm),
        "experiment arm must be baseline or treatment",
    )?;
    require(
        rows(contract.get("seeds"))
            .iter()
            .map(canonical)
            .collect::<Result<Vec<_>>>()?
            .iter()
            .any(|v| v.as_slice() == seed.as_bytes()),
        format!("seed {seed} was not pre-registered"),
    )?;
    let metrics = rt.user_path(
        r.os("metrics")
            .ok_or_else(|| error("metrics path is required"))?,
    )?;
    let before = rt.read_optional_user(metrics.as_os_str(), crate::storage::MAX_STATE)?;
    require(
        before.is_none() || r.has("allow-existing-metrics"),
        "metrics file already exists; refuse stale reuse unless --allow-existing-metrics is explicit",
    )?;
    let run_id = random_id("experiment-run-")?;
    let profile = r.text("profile", "trusted-local")?;
    let backend = crate::backend::request(rt, r, profile, 86400)?;
    let (reserved, override_reason) =
        reserve(rt, &contract, name, &run_id, r.text("reason", "")?, "run")?;
    let execution = (|| {
        if profile == "trusted-local" {
            let result = rt.services.run_ledger(
                &rt.repo,
                &LedgerRequest {
                    operation_id: run_id.clone(),
                    argv: r.tail.clone(),
                    metrics: before.as_ref().map(|_| metrics.clone()),
                    file: Some("docs/EXPERIMENTS.jsonl".into()),
                    note: format!("experiment={name} arm={arm} seed={seed} contract-run"),
                    research: empty(),
                    no_gate: false,
                    reason: String::new(),
                    wall: backend.wall,
                },
            )?;
            require(
                !result.ledger_id.is_empty(),
                "the run ledger did not record this run; refusing an unrecorded experiment result",
            )?;
            Ok((
                object([("receipt", Value::Null), ("operation_id", string(&run_id))]),
                result.output.exit,
                result.ledger_id,
            ))
        } else {
            let mut backend = backend.clone();
            backend.log = Some(format!(".oms/experiments/logs/{run_id}.log").into());
            let result = crate::backend::execute(rt, &backend)?;
            Ok((result.receipt, result.exit, String::new()))
        }
    })();
    let (receipt, rc, ledger_id) = match execution {
        Ok(v) => v,
        Err(e) => {
            if reserved {
                settle(
                    rt,
                    &contract,
                    &run_id,
                    "released",
                    &format!("run ended without a record: {e}"),
                )?;
            }
            return Err(e);
        }
    };
    let after = rt.read_optional_user(metrics.as_os_str(), crate::storage::MAX_STATE)?;
    let stale = before
        .as_ref()
        .zip(after.as_ref())
        .is_some_and(|(l, r)| l.len() == r.len() && sha(l) == sha(r));
    let data = if stale {
        Value::Null
    } else {
        after
            .as_ref()
            .map(|v| parse(v))
            .transpose()?
            .unwrap_or(Value::Null)
    };
    let mut numeric_metrics = empty();
    for (k, _) in contract.get("metric_specs").object()? {
        let name = std::str::from_utf8(k).map_err(error)?;
        if let Some(value) = finite(data.get(name)) {
            numeric_metrics.set(name, number(value))?;
        }
    }
    let primary = contract.text("primary_metric");
    let metric_value = finite(data.get(primary));
    let mut required = BTreeSet::from([primary.to_owned()]);
    required.extend(
        contract
            .get("success")
            .get("no_regression")
            .object()?
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned()),
    );
    let missing = required
        .into_iter()
        .filter(|name| numeric_metrics.opt(name).is_none())
        .collect::<Vec<_>>();
    let metric_error = if stale {
        "metrics file content was not changed by this run".into()
    } else if data.object().is_err() {
        "metrics file is missing or invalid".into()
    } else if metric_value.is_none() {
        format!("metrics file must contain finite numeric {primary}")
    } else if !missing.is_empty() {
        format!(
            "required metrics are missing or non-finite: {}",
            missing.join(", ")
        )
    } else {
        String::new()
    };
    let valid = rc == 0 && !stale && metric_value.is_some() && missing.is_empty();
    let effective = if rc != 0 {
        rc
    } else if valid {
        0
    } else {
        3
    };
    let mut row = object([
        ("schema", number(1)),
        ("run_id", string(&run_id)),
        ("created_at", string(&rt.now)),
        ("experiment_id", string(name)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("arm", string(arm)),
        ("seed", seed_value.clone()),
        ("execution_receipt", receipt.get("receipt").clone()),
        (
            "execution_operation_id",
            receipt.get("operation_id").clone(),
        ),
        ("ledger_id", optional(&ledger_id)),
        ("exit", number(effective)),
        ("command_exit", number(rc)),
        ("metric", string(primary)),
        ("metric_value", numeric(metric_value)),
        ("metrics", numeric_metrics),
        (
            "metrics_digest",
            if !stale {
                after.map(|v| string(sha(v))).unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        ),
        ("metric_error", string(metric_error)),
        (
            "missing_required_metrics",
            list(missing.iter().map(String::as_str)),
        ),
        ("metrics_valid", b(valid)),
        ("stale_metrics", b(stale)),
    ]);
    if !override_reason.is_empty() {
        row.set("budget_override_reason", string(override_reason))?;
    }
    if let Err(e) = rt.append(Path::new(RUNS), &row) {
        if reserved {
            settle(
                rt,
                &contract,
                &run_id,
                "released",
                &format!("run row not written: {e}"),
            )?;
        }
        return Err(e);
    }
    if reserved {
        settle(rt, &contract, &run_id, "used", "")?;
    }
    Ok((row, effective))
}
fn arm_summary(
    latest: &BTreeMap<String, Value>,
    seeds: &[Value],
    metric: &str,
    digest: &str,
) -> Result<Value> {
    let mut values = vec![];
    let mut missing = vec![];
    for seed in seeds {
        let key = String::from_utf8(canonical(seed)?).map_err(error)?;
        let candidate = latest
            .get(&key)
            .filter(|r| r.get("exit") == &b(false) || finite(r.get("exit")) == Some(0.0))
            .map(|r| r.get("metrics").get(metric));
        let found = candidate.and_then(finite);
        require(
            found.is_some() || !matches!(candidate, Some(Value::Number(_))),
            "experiment metric cannot be represented as a finite float",
        )?;
        if let Some(v) = found {
            values.push(v)
        } else {
            missing.push(seed.clone())
        }
    }
    let ci = oms_command_runs::metrics::bootstrap(&values, &format!("{digest}{metric}"));
    Ok(object([
        ("values", a(values.iter().copied().map(number))),
        ("n", number(values.len())),
        ("mean", numeric(mean(&values))),
        ("stdev", numeric(stdev(&values))),
        (
            "bootstrap_ci95",
            ci.map(|v| a(v.map(number))).unwrap_or(Value::Null),
        ),
        ("missing_seeds", a(missing)),
    ]))
}
pub fn summarize(rt: &Runtime<'_>, name: &str) -> Result<Value> {
    let c = load(rt, name)?;
    let digest = c.text("contract_digest");
    let mut latest = [BTreeMap::new(), BTreeMap::new()];
    for row in rt
        .rows(RUNS)?
        .into_iter()
        .filter(|r| r.text("experiment_id") == name && r.text("contract_digest") == digest)
    {
        let index = match row.text("arm") {
            "baseline" => 0,
            "treatment" => 1,
            _ => continue,
        };
        let key = match row.get("seed") {
            Value::Bool(v) => Some(u8::from(*v).to_string()),
            Value::Number(s) if !s.contains(['.', 'e', 'E']) => {
                Some(String::from_utf8(canonical(row.get("seed"))?).map_err(error)?)
            }
            _ => None,
        };
        if let Some(key) = key {
            latest[index].insert(key, row);
        }
    }
    let seeds = rows(c.get("seeds"));
    let primary = c.text("primary_metric");
    let baseline = arm_summary(&latest[0], seeds, primary, &format!("{digest}baseline"))?;
    let treatment = arm_summary(&latest[1], seeds, primary, &format!("{digest}treatment"))?;
    let data_complete =
        !baseline.get("missing_seeds").truth() && !treatment.get("missing_seeds").truth();
    let direction = c.get("metrics").get("primary").text("direction");
    let improvement = finite(baseline.get("mean"))
        .zip(finite(treatment.get("mean")))
        .map(|(b, t)| {
            if direction == "minimize" {
                b - t
            } else {
                t - b
            }
        });
    let values = rows(baseline.get("values"))
        .iter()
        .filter_map(finite)
        .collect::<Vec<_>>();
    let (noise, band) = noise(&c, &values);
    let threshold = finite(c.get("success").get("min_improvement")).unwrap_or(0.0);
    let primary_pass = if data_complete {
        improvement.zip(band).map(|(v, b)| v >= threshold.max(b))
    } else {
        None
    };
    let mut no_regression = empty();
    let mut regression_complete = true;
    let mut regression_pass = true;
    for (k, rule) in c.get("success").get("no_regression").object()? {
        let metric = std::str::from_utf8(k).map_err(error)?;
        let l = arm_summary(&latest[0], seeds, metric, &format!("{digest}baseline"))?;
        let r = arm_summary(&latest[1], seeds, metric, &format!("{digest}treatment"))?;
        let ready = !l.get("missing_seeds").truth() && !r.get("missing_seeds").truth();
        let delta = if ready {
            finite(l.get("mean"))
                .zip(finite(r.get("mean")))
                .map(|(b, t)| {
                    if rule.text("direction") == "minimize" {
                        t - b
                    } else {
                        b - t
                    }
                })
        } else {
            None
        };
        let pass = delta.map(|d| d <= finite(rule.get("max_regression")).unwrap_or(0.0));
        regression_complete &= ready;
        regression_pass &= pass == Some(true);
        no_regression.set(
            metric,
            object([
                ("direction", rule.get("direction").clone()),
                ("max_regression", rule.get("max_regression").clone()),
                ("baseline", l),
                ("treatment", r),
                ("regression", numeric(delta)),
                ("passed", pass.map(b).unwrap_or(Value::Null)),
            ]),
        )?;
    }
    let invariant_result = rt
        .rows(".oms/experiments/invariants.jsonl")?
        .into_iter()
        .rfind(|r| r.text("contract_digest") == digest);
    let invariant_required = c.get("invariant_pack").truth();
    let invariant_complete = !invariant_required || invariant_result.is_some();
    let invariant_pass = !invariant_required
        || invariant_result
            .as_ref()
            .is_some_and(|r| r.get("passed").truth());
    let complete = data_complete && regression_complete && invariant_complete;
    let verdict = if !complete || primary_pass.is_none() {
        "inconclusive"
    } else if primary_pass == Some(true) && regression_pass && invariant_pass {
        "supported"
    } else {
        "not_supported"
    };
    let mut out = object([
        ("schema", number(1)),
        ("experiment_id", string(name)),
        ("contract_digest", string(digest)),
        ("metric", string(primary)),
        ("direction", string(direction)),
        (
            "arms",
            object([("baseline", baseline), ("treatment", treatment)]),
        ),
        ("complete", b(complete)),
        ("data_complete", b(data_complete)),
        ("improvement", numeric(improvement)),
        ("min_improvement", number(threshold)),
        ("primary_pass", primary_pass.map(b).unwrap_or(Value::Null)),
        ("no_regression", no_regression),
        (
            "invariants",
            object([
                ("required", b(invariant_required)),
                ("complete", b(invariant_complete)),
                ("passed", b(invariant_pass)),
                ("result", invariant_result.unwrap_or(Value::Null)),
            ]),
        ),
        ("verdict", string(verdict)),
    ]);
    if c.get("success").opt("noise_band").is_some() {
        out.set("noise_band", noise.unwrap_or(Value::Null))?;
    }
    let budget = budget_status(rt, &c, name)?;
    if !matches!(budget, Value::Null) {
        out.set("budget", budget)?;
    }
    Ok(out)
}
pub fn invariants(rt: &Runtime<'_>, r: &Request, raw: &Value) -> Result<Value> {
    let c = validate(raw)?;
    let operation = random_id("invariant-run-")?;
    let mut results = vec![];
    for item in rows(c.get("invariant_pack")) {
        let mut run = r.clone();
        run.tail = item
            .get("command")
            .strings()?
            .iter()
            .map(std::ffi::OsString::from)
            .collect();
        let mut req = crate::backend::request(rt, &run, r.text("profile", "trusted-local")?, 1200)?;
        req.log = Some(
            format!(
                ".oms/experiments/invariants/{operation}/{}.log",
                item.text("name")
            )
            .into(),
        );
        let result = crate::backend::execute(rt, &req)?;
        results.push(object([
            ("name", item.get("name").clone()),
            ("required", item.get("required").clone()),
            ("exit", number(result.exit)),
            ("passed", b(result.exit == 0)),
            ("receipt", result.receipt.get("receipt").clone()),
            ("operation_id", result.receipt.get("operation_id").clone()),
        ]));
    }
    let passed = results
        .iter()
        .filter(|r| r.get("required").truth())
        .all(|r| r.get("passed").truth());
    let row = object([
        ("schema", number(1)),
        ("invariant_run_id", string(operation)),
        ("created_at", string(&rt.now)),
        ("contract_digest", c.get("contract_digest").clone()),
        ("passed", b(passed)),
        ("results", a(results)),
    ]);
    rt.append(Path::new(".oms/experiments/invariants.jsonl"), &row)?;
    Ok(row)
}
fn scalar(v: &Value, label: &str) -> Result<()> {
    require(
        match v {
            Value::Null | Value::Bool(_) => true,
            Value::String(s) => std::str::from_utf8(s)
                .is_ok_and(|s| s.chars().count() <= 200 && !s.contains(['\r', '\n', '\t'])),
            Value::Number(s) => !s.contains(['.', 'e', 'E']) || finite(v).is_some(),
            _ => false,
        },
        format!("{label} must be a string, number, boolean or null on one line"),
    )
}
fn scalar_text(v: &Value) -> Result<String> {
    match v {
        Value::String(_) => Ok(v.str()?.into()),
        _ => String::from_utf8(canonical(v)?).map_err(error),
    }
}
pub fn sweep(raw: &Value) -> Result<Value> {
    keys(raw, &["grid"], &["seeds", "fixed"])?;
    let mut grid = raw.get("grid").object()?.to_vec();
    grid.sort_by(|(a, _), (b, _)| a.cmp(b));
    require(
        !grid.is_empty() && grid.len() <= 16,
        "sweep grid must have 1..16 parameters",
    )?;
    let fixed = raw.opt("fixed").cloned().unwrap_or_else(empty);
    fixed.object()?;
    let name_re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]{0,79}$").unwrap();
    for (k, _) in grid.iter().chain(fixed.object()?.iter()) {
        let name = std::str::from_utf8(k).map_err(error)?;
        require(
            name != "seed" && name_re.is_match(name),
            "sweep parameter name is not usable",
        )?;
    }
    for (k, v) in fixed.object()? {
        require(
            !grid.iter().any(|(name, _)| name == k),
            "parameter is both swept and fixed",
        )?;
        scalar(v, "fixed value")?;
    }
    let seeds = raw.opt("seeds").cloned().unwrap_or_else(|| a([number(0)]));
    let seeds = seeds.array()?;
    require(
        !seeds.is_empty(),
        "sweep seeds must be a nonempty list of integers",
    )?;
    let mut seen = BTreeSet::new();
    for seed in seeds {
        require(
            matches!(seed,Value::Number(s)if !s.contains(['.','e','E'])),
            "sweep seeds must contain integers",
        )?;
        require(seen.insert(canonical(seed)?), "sweep seeds must be unique")?;
    }
    let mut repeated = empty();
    let mut axes = vec![];
    let mut values = vec![];
    let mut count = 1usize;
    for (k, listed) in &grid {
        let name = std::str::from_utf8(k).map_err(error)?;
        let listed = listed.array()?;
        require(
            !listed.is_empty() && listed.len() <= 200,
            format!("sweep grid.{name} must be a list of 1-200 values"),
        )?;
        let mut kept = vec![];
        let mut repeats = vec![];
        let mut seen = BTreeSet::new();
        for v in listed {
            scalar(v, name)?;
            if seen.insert(canonical(v)?) {
                kept.push(v.clone())
            } else {
                repeats.push(v.clone())
            }
        }
        if !repeats.is_empty() {
            repeated.set(name, a(repeats))?;
        }
        count = count
            .checked_mul(kept.len())
            .ok_or_else(|| error("sweep cell count overflow"))?;
        require(count <= 5000, "sweep has more than 5000 cells; split it")?;
        axes.push(name.to_owned());
        values.push(kept);
    }
    require(
        count.checked_mul(seeds.len()).is_some_and(|n| n <= 20000),
        "sweep has more than 20000 runs; split it",
    )?;
    let mut params = vec![fixed.clone()];
    for (name, values) in axes.iter().zip(values) {
        let mut next = vec![];
        for p in params {
            for value in &values {
                let mut row = p.clone();
                row.set(name, value.clone())?;
                next.push(row)
            }
        }
        params = next;
    }
    let cells = params
        .into_iter()
        .map(|p| {
            Ok(object([
                ("cell_id", string(format!("cell-{}", &hash(&p)?[..12]))),
                ("params", p),
            ]))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut notes = vec![];
    if repeated.truth() {
        let mut labels = vec![];
        for (k, v) in repeated.object()? {
            labels.push(format!(
                "{}={}",
                std::str::from_utf8(k).map_err(error)?,
                rows(v)
                    .iter()
                    .map(scalar_text)
                    .collect::<Result<Vec<_>>>()?
                    .join(",")
            ));
        }
        notes.push(format!(
            "repeated grid values were listed once: {}",
            labels.join("; ")
        ));
    }
    if cells.len() > 1 {
        notes.push(format!("{} configurations are compared: the best of {} is expected to look better than it is; confirm the chosen cell on data the selection did not use",cells.len(),cells.len()))
    }
    if seeds.len() < 3 {
        notes.push(format!(
            "{} seed(s) per cell: a difference between cells may be seed noise",
            seeds.len()
        ))
    }
    Ok(object([
        ("schema", number(1)),
        ("kind", string("oms-experiment-sweep-plan")),
        ("parameters", list(axes.iter().map(String::as_str))),
        ("fixed", fixed),
        ("seeds", a(seeds.iter().cloned())),
        ("cell_count", number(cells.len())),
        ("run_count", number(cells.len() * seeds.len())),
        ("cells", a(cells)),
        ("repeated_values", repeated),
        ("notes", list(notes.iter().map(String::as_str))),
    ]))
}
pub fn emit_sweep(value: &Value, format: &str) -> Result<String> {
    require(
        ["tsv", "hydra-overrides", "sbatch-array"].contains(&format),
        "--emit must be one of: tsv, hydra-overrides, sbatch-array",
    )?;
    let mut names = rows(value.get("parameters"))
        .iter()
        .map(|v| text(v).to_owned())
        .collect::<BTreeSet<_>>();
    names.extend(
        value
            .get("fixed")
            .object()?
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned()),
    );
    let mut lines = vec![];
    if format == "tsv" {
        lines.push(format!(
            "index\tcell_id\tseed\t{}",
            names.iter().cloned().collect::<Vec<_>>().join("\t")
        ));
    } else if format == "sbatch-array" {
        lines.push(format!(
            "# {} run(s): {} cell(s) x {} seed(s). Printed only; nothing was submitted.",
            value.get("run_count").int()?,
            value.get("cell_count").int()?,
            rows(value.get("seeds")).len()
        ));
        lines.push(format!(
            "#SBATCH --array=0-{}",
            value.get("run_count").int()? - 1
        ));
        lines.push(
            "# One line per array index: INDEX<TAB>CELL_ID<TAB>OVERRIDES. In the job script:"
                .into(),
        );
        lines.push(
            "#   line=\"$(awk -F'\\t' -v i=\"$SLURM_ARRAY_TASK_ID\" '$1 == i' sweep.tsv)\"".into(),
        );
    }
    let safe = regex::Regex::new(r"^[A-Za-z0-9_.+:/@%-]+$").unwrap();
    let mut index = 0;
    for cell in rows(value.get("cells")) {
        for seed in rows(value.get("seeds")) {
            let seed = scalar_text(seed)?;
            if format == "tsv" {
                let values = names
                    .iter()
                    .map(|n| scalar_text(cell.get("params").get(n)))
                    .collect::<Result<Vec<_>>>()?;
                lines.push(format!(
                    "{index}\t{}\t{seed}\t{}",
                    cell.text("cell_id"),
                    values.join("\t")
                ));
            } else {
                let mut parts = vec![];
                for name in &names {
                    let s = scalar_text(cell.get("params").get(name))?;
                    require(
                        safe.is_match(&s),
                        format!("value of {name} cannot be written as a bare override"),
                    )?;
                    parts.push(format!("{name}={s}"));
                }
                parts.push(format!("seed={seed}"));
                let overrides = parts.join(" ");
                lines.push(if format == "hydra-overrides" {
                    overrides
                } else {
                    format!("{index}\t{}\t{overrides}", cell.text("cell_id"))
                });
            }
            index += 1;
        }
    }
    Ok(lines.join("\n"))
}
pub fn launch(rt: &Runtime<'_>, r: &Request) -> Result<crate::router::Response> {
    let names = [
        "question",
        "hypothesis",
        "prediction",
        "baseline",
        "metric",
        "success",
        "change",
    ];
    let mut fields = empty();
    for name in names {
        fields.set(name, string(r.text(name, "")?))?;
    }
    let contract = if let Some(raw) = r.os("contract") {
        let raw = parse(&rt.read_user(raw, crate::storage::MAX_STATE)?)?;
        let c = validate(&raw)?;
        let success = c.get("success");
        let mut threshold = format!(
            "min_improvement={}",
            scalar_text(success.get("min_improvement"))?
        );
        let regressions = success
            .get("no_regression")
            .object()?
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
            .collect::<BTreeSet<_>>();
        if !regressions.is_empty() {
            threshold.push_str(&format!(
                "; no_regression={}",
                regressions.into_iter().collect::<Vec<_>>().join(",")
            ));
        }
        let baseline = String::from_utf8(default_bytes(c.get("baseline"), true)?)
            .map_err(error)?
            .chars()
            .take(300)
            .collect::<String>();
        let derived = [
            c.text("question").to_owned(),
            c.text("hypothesis").to_owned(),
            c.text("prediction").to_owned(),
            baseline,
            format!(
                "{} ({})",
                c.text("primary_metric"),
                c.get("metrics").get("primary").text("direction")
            ),
            threshold,
            c.get("treatment").text("independent_change").into(),
        ];
        for (name, value) in names.iter().zip(derived) {
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            require(
                fields.text(name).is_empty() || fields.text(name) == value,
                format!("--{name} conflicts with --contract"),
            )?;
            fields.set(name, string(value))?;
        }
        fields.set("contract_digest", c.get("contract_digest").clone())?;
        Some(c)
    } else {
        None
    };
    for name in names {
        require(
            !fields.text(name).trim().is_empty(),
            format!("--{name} is required"),
        )?;
    }
    require(!r.tail.is_empty(), "command after -- is required")?;
    require(
        !r.has("no-gate") || !r.text("reason", "")?.trim().is_empty(),
        "--reason (required with --no-gate) is required",
    )?;
    let note = names
        .iter()
        .zip(["Q", "H", "P", "B", "M", "S", "C"])
        .map(|(name, label)| format!("{label}: {}", fields.text(name)))
        .collect::<Vec<_>>()
        .join(" | ");
    require(
        note.len() <= 1600,
        "pre-registration note exceeds 1600 bytes; shorten the fields",
    )?;
    require(
        !crate::capsule::sensitive(&note),
        "pre-registration contains sensitive-looking content or scan failed",
    )?;
    let operation = random_id("experiment-run-")?;
    let request = LedgerRequest {
        operation_id: operation.clone(),
        argv: r.tail.clone(),
        metrics: r.os("metrics").map(|p| rt.user_path(p)).transpose()?,
        file: r.os("file").map(|p| rt.user_path(p)).transpose()?,
        note: note.clone(),
        research: fields,
        no_gate: r.has("no-gate"),
        reason: r.text("reason", "")?.into(),
        wall: std::time::Duration::from_secs(86400),
    };
    let mut warning = String::new();
    if let Some(c) = &contract {
        let state = budget_status(rt, c, "")?;
        if state.get("runs").get("exceeded").truth() {
            require(
                !r.text("reason", "")?.trim().is_empty(),
                budget_refusal(state.get("runs"))?,
            )?;
            warning = format!(
                "experiment launch: past the contract run budget ({})\n",
                bounded(r.text("reason", "")?, 300)
            );
        }
    }
    if r.has("dry-run") {
        let mut argv = vec![
            std::ffi::OsString::from("oms"),
            "run-ledger".into(),
            "--note".into(),
            note.clone().into(),
        ];
        for key in ["file", "metrics"] {
            if let Some(v) = r.os(key).filter(|v| !v.is_empty()) {
                argv.extend([format!("--{key}").into(), v.to_owned()]);
            }
        }
        if r.has("no-gate") {
            argv.extend([
                "--no-gate".into(),
                "--reason".into(),
                r.text("reason", "")?.into(),
            ]);
        }
        argv.push("--".into());
        argv.extend(r.tail.iter().cloned());
        let safe = regex::Regex::new(r"^[A-Za-z0-9_@%+=:,./-]+$").unwrap();
        let printed = argv
            .iter()
            .map(|v| {
                let s = v
                    .to_str()
                    .ok_or_else(|| error("launch command cannot be encoded as UTF-8"))?;
                Ok(if safe.is_match(s) {
                    s.to_owned()
                } else {
                    format!("'{}'", s.replace('\'', "'\"'\"'"))
                })
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let mut response = crate::router::Response::text(format!(
            "experiment launch: dry-run\nnote: {note}\ncommand: {printed}"
        ));
        response.stderr = warning.into_bytes();
        return Ok(response);
    }
    rt.parent_write()?;
    let (reserved, _) = if let Some(c) = &contract {
        reserve(rt, c, "", &operation, r.text("reason", "")?, "launch")?
    } else {
        (false, String::new())
    };
    let result = rt.services.run_ledger(&rt.repo, &request);
    match result {
        Ok(result) => {
            if reserved {
                settle(
                    rt,
                    contract.as_ref().unwrap(),
                    &operation,
                    if result.ledger_id.is_empty() {
                        "released"
                    } else {
                        "used"
                    },
                    if result.ledger_id.is_empty() {
                        "the run ledger did not record this run"
                    } else {
                        ""
                    },
                )?;
            }
            let mut stderr = warning.into_bytes();
            stderr.extend(b"experiment launch: launching registered experiment\n");
            stderr.extend(result.output.stderr);
            Ok(crate::router::Response {
                code: result.output.exit,
                stdout: result.output.stdout,
                stderr,
            })
        }
        Err(e) => {
            if reserved {
                settle(
                    rt,
                    contract.as_ref().unwrap(),
                    &operation,
                    "released",
                    &format!("launch ended without a record: {e}"),
                )?;
            }
            Err(e)
        }
    }
}
