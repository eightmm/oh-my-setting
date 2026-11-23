//! Read-only continuation and checkpoint provenance over the experiment owners.
//! A recommendation never spends a budget, submits work, or changes a contract.
use super::*;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, PathBuf};
use std::time::{Duration, Instant};

const MAX_RECORDS: usize = 4096;

fn checked_runs(rt: &Runtime<'_>) -> Result<(Vec<Value>, String)> {
    rt.invalidate(Path::new(RUNS));
    let raw = rt
        .bytes(RUNS, crate::storage::MAX_STATE)?
        .unwrap_or_default();
    require(
        raw.is_empty() || raw.last() == Some(&b'\n'),
        "experiment stream has an incomplete tail",
    )?;
    let mut rows = Vec::new();
    for line in raw.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        require(
            rows.len() < MAX_RECORDS,
            "experiment continuation row bound exceeded",
        )?;
        rows.push(oms_command_plan::json::parse_bounded(
            line,
            crate::storage::MAX_ROW,
        )?);
    }
    Ok((rows, sha(raw.as_slice())))
}

pub(crate) fn valid_measurement(contract: &Value, row: &Value) -> bool {
    let mut metrics = vec![contract.text("primary_metric")];
    if let Ok(rules) = contract.get("success").get("no_regression").object() {
        for (name, _) in rules {
            if let Ok(name) = std::str::from_utf8(name) {
                metrics.push(name);
            }
        }
    } else {
        return false;
    }
    row.get("metrics_valid").truth()
        && !row.get("stale_metrics").truth()
        && row.get("exit").is_int(0)
        && hash(row.text("metrics_digest"))
        && metrics
            .iter()
            .all(|name| finite(row.get("metrics").get(name)).is_some())
}
fn terminal(row: &Value) -> bool {
    matches!(
        row.text("state"),
        "succeeded" | "failed" | "cancelled" | "lost" | "invalid_result"
    ) || row.text("state").is_empty() && row.get("exit").int().is_ok()
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn relation(row: &Value) -> Value {
    object([
        ("schema", number(1)),
        ("source", string("experiment")),
        (
            "identity",
            object([
                ("experiment_id", row.get("experiment_id").clone()),
                ("run_id", row.get("run_id").clone()),
                ("contract_digest", row.get("contract_digest").clone()),
                ("request_sha256", string(row.text("request_sha256"))),
            ]),
        ),
    ])
}

pub fn next(rt: &Runtime<'_>, name: &str) -> Result<Value> {
    let contract = load(rt, name)?;
    let (records, source_sha256) = checked_runs(rt)?;
    let mut runs: BTreeMap<String, Value> = BTreeMap::new();
    let mut cells: BTreeMap<(String, Vec<u8>), Value> = BTreeMap::new();
    for row in records.into_iter().filter(|row| {
        row.text("experiment_id") == name
            && row.get("contract_digest") == contract.get("contract_digest")
    }) {
        id(row.text("run_id"))?;
        require(
            ["baseline", "treatment"].contains(&row.text("arm"))
                && rows(contract.get("seeds")).contains(row.get("seed")),
            "experiment record uses an unregistered arm or seed",
        )?;
        if let Some(previous) = runs.get(row.text("run_id")) {
            require(
                [
                    "experiment_id",
                    "contract_digest",
                    "arm",
                    "seed",
                    "request_sha256",
                    "snapshot",
                ]
                .iter()
                .all(|key| previous.get(key) == row.get(key)),
                "experiment run identity changed in its source stream",
            )?;
            require(
                !terminal(previous) || terminal(&row),
                "experiment terminal result regressed to pending",
            )?;
        }
        cells.insert(
            (row.text("arm").into(), canonical(row.get("seed"))?),
            row.clone(),
        );
        runs.insert(row.text("run_id").into(), row);
    }
    let summary = summarize(rt, name)?;
    let budget = budget_status(rt, &contract, name)?;
    let planning = object([
        (
            "contract_path",
            string(format!(".oms/experiments/contracts/{name}.json")),
        ),
        ("contract_digest", contract.get("contract_digest").clone()),
        (
            "controlled_variables",
            contract.get("controlled_variables").clone(),
        ),
        ("baseline", contract.get("baseline").clone()),
        ("treatment", contract.get("treatment").clone()),
        ("metrics", contract.get("metrics").clone()),
        ("success", contract.get("success").clone()),
        ("seeds", contract.get("seeds").clone()),
        ("invariant_pack", contract.get("invariant_pack").clone()),
        ("source_path", string(RUNS)),
        ("source_sha256", string(&source_sha256)),
        ("verdict", summary.get("verdict").clone()),
    ]);
    let mut action = object([
        ("schema", number(1)),
        ("experiment_id", string(name)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("budget", budget.clone()),
        ("planning_context", planning),
        (
            "commands",
            a([
                list(["oms", "runtime", "experiment", "show", name]),
                list(["oms", "autopilot", "status"]),
            ]),
        ),
    ]);
    let mut selected = None;
    for row in runs.values() {
        if !terminal(row) {
            selected = Some((
                "wait",
                "An exact existing run is unresolved; reconcile it without submitting again.",
            ));
            let wait = relation(row);
            crate::external::validate_binding(&wait)?;
            action.set("wait_binding", wait)?;
            action.set(
                "commands",
                a([list([
                    "oms",
                    "runtime",
                    "experiment",
                    "reconcile",
                    "--run-id",
                    row.text("run_id"),
                ])]),
            )?;
            break;
        }
    }
    if selected.is_none() {
        for row in cells.values() {
            if !valid_measurement(&contract, row) {
                selected = Some((
                    "inspect",
                    "The latest completed cell has missing, non-finite, stale or failed evidence; inspect before revising the experiment.",
                ));
                action.set("run_id", row.get("run_id").clone())?;
                break;
            }
        }
    }
    if selected.is_none() && summary.get("complete").truth() {
        selected = Some((
            "review-hypothesis",
            "The registered hypothesis has a complete result, including valid negative evidence. Main may propose the next hypothesis through the existing reviewed Plan; this contract remains immutable.",
        ));
    }
    if selected.is_none()
        && (budget.get("runs").get("exceeded").truth()
            || budget.get("gpu_hours").get("exceeded").truth())
    {
        selected = Some((
            "stop",
            "The original experiment budget is exhausted; no new run is authorized by this projection.",
        ));
    }
    if selected.is_none() {
        for arm in ["baseline", "treatment"] {
            for seed in rows(contract.get("seeds")) {
                if cells
                    .get(&(arm.to_owned(), canonical(seed)?))
                    .is_some_and(|row| valid_measurement(&contract, row))
                {
                    continue;
                }
                action.set("arm", string(arm))?;
                action.set("seed", seed.clone())?;
                action.set(
                    "required_inputs",
                    list([
                        "command for this exact arm and seed",
                        "execution route and frozen code/data/config/evaluation inputs",
                    ]),
                )?;
                action.set(
                    "run_request",
                    object([
                        ("id", string(name)),
                        ("arm", string(arm)),
                        ("seed", seed.clone()),
                        ("contract_digest", contract.get("contract_digest").clone()),
                    ]),
                )?;
                // Older run argv are not templates: a command can embed a
                // different seed or configuration. Main plans this exact cell.
                action.set(
                    "commands",
                    a([
                        list(["oms", "autopilot", "status"]),
                        list(["oms", "plan-from-spec", "--collaboration", "auto"]),
                    ]),
                )?;
                selected = Some((
                    "input-required",
                    "Main must plan the missing preregistered cell and have Lead supply its exact execution inputs through the reviewed proposal path. Existing run admission enforces seed, target, contract and budget; do not replay another cell's argv.",
                ));
                break;
            }
            if selected.is_some() {
                break;
            }
        }
    }
    let (kind, reason) = selected.unwrap_or(("inspect", "Metrics are complete but invariant or evaluation evidence is missing; inspect the registered invariant pack before continuing."));
    if kind == "inspect" && !summary.get("invariants").get("complete").truth() {
        action.set(
            "commands",
            a([list([
                "oms",
                "runtime",
                "experiment",
                "invariants",
                &format!(".oms/experiments/contracts/{name}.json"),
            ])]),
        )?;
    }
    action.set("kind", string(kind))?;
    action.set("reason", string(reason))?;
    action.set(
        "requires_parent_plan",
        b(matches!(kind, "input-required" | "review-hypothesis")),
    )?;
    let mut out = object([
        ("schema", number(1)),
        ("kind", string("oms-experiment-next")),
        ("experiment_id", string(name)),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("source_sha256", string(source_sha256)),
        ("summary", summary),
        ("budget", budget),
        ("next_action", action),
        (
            "authority",
            string("read-only; existing Plan review, run admission and budgets retain authority"),
        ),
    ]);
    out.set("projection_sha256", string(sha(canonical(&out)?)))?;
    Ok(out)
}

// Open each component relative to a held directory, without symlinks; a
// checkpoint is byte data and is never deserialized by OMS.
fn open_checkpoint(repo: &Path, relative: &Path) -> Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    require(
        !relative.as_os_str().is_empty()
            && !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "checkpoint path must be relative and confined to the repository",
    )?;
    let mut dir = open(
        repo,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let last = index + 1 == components.len();
        let flags = OFlags::RDONLY
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | if last {
                OFlags::empty()
            } else {
                OFlags::DIRECTORY
            };
        let next = openat(&dir, component.as_os_str(), flags, Mode::empty()).map_err(error)?;
        if last {
            return Ok(next.into());
        }
        dir = next;
    }
    Err(error("checkpoint path missing"))
}
fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

pub fn checkpoint(rt: &Runtime<'_>, r: &Request) -> Result<Value> {
    let seconds = r.integer("timeout-seconds", 30)?;
    require(
        (1..=300).contains(&seconds),
        "checkpoint timeout must be 1..300 seconds",
    )?;
    let deadline = Instant::now() + Duration::from_secs(seconds as u64);
    let max = u64::try_from(r.integer("max-bytes", 512 * 1024 * 1024)?).map_err(error)?;
    require(
        max > 0 && max <= 64 * 1024 * 1024 * 1024,
        "checkpoint byte bound must be 1..64 GiB",
    )?;
    let name = r.text("id", "")?;
    let run_id = r.text("run-id", "")?;
    let contract = load(rt, name)?;
    let raw = rt.read_user(
        r.os("manifest")
            .ok_or_else(|| error("--manifest is required"))?,
        64 * 1024,
    )?;
    let manifest = oms_command_plan::json::parse_bounded(&raw, 64 * 1024)?;
    keys(
        &manifest,
        &[
            "schema",
            "experiment_id",
            "run_id",
            "contract_digest",
            "arm",
            "seed",
            "snapshot",
            "code_base",
            "dataset_split",
            "config_digest",
            "evaluation_sha256",
            "step",
            "rng_state_sha256",
            "checkpoint",
        ],
        &[],
    )?;
    require(
        manifest.get("schema").is_int(1),
        "unsupported checkpoint provenance schema",
    )?;
    let (records, _) = checked_runs(rt)?;
    let row = records
        .iter()
        .rev()
        .find(|row| row.text("run_id") == run_id)
        .ok_or_else(|| error("checkpoint has no recorded experiment run"))?;
    require(
        row.text("experiment_id") == name
            && manifest.text("experiment_id") == name
            && manifest.text("run_id") == run_id
            && row.get("contract_digest") == contract.get("contract_digest")
            && manifest.get("contract_digest") == contract.get("contract_digest"),
        "checkpoint belongs to another experiment, run or contract",
    )?;
    require(
        ["baseline", "treatment"].contains(&row.text("arm"))
            && rows(contract.get("seeds")).contains(row.get("seed"))
            && manifest.get("arm") == row.get("arm")
            && manifest.get("seed") == row.get("seed"),
        "checkpoint arm or seed mismatch",
    )?;
    let controlled = contract.get("controlled_variables");
    let config = contract.get(row.text("arm")).text("config_digest");
    require(
        !controlled.text("code_base").is_empty()
            && !controlled.text("dataset_split").is_empty()
            && hash(config)
            && manifest.get("code_base") == controlled.get("code_base")
            && manifest.get("dataset_split") == controlled.get("dataset_split")
            && manifest.text("config_digest") == config,
        "checkpoint requires matching frozen code, dataset and configuration provenance",
    )?;
    let evaluation = object([
        ("metrics", contract.get("metrics").clone()),
        ("success", contract.get("success").clone()),
        ("invariant_pack", contract.get("invariant_pack").clone()),
    ]);
    require(
        manifest.text("evaluation_sha256") == sha(canonical(&evaluation)?),
        "checkpoint evaluation identity changed",
    )?;
    require(
        manifest.text("snapshot") == row.text("snapshot")
            && (hash(row.text("snapshot"))
                || row.text("snapshot").is_empty()
                    && controlled.text("code_base") == rt.repository.head),
        "checkpoint execution snapshot or local HEAD mismatch",
    )?;
    require(
        manifest.get("step").int().is_ok_and(|n| n >= 0) && hash(manifest.text("rng_state_sha256")),
        "checkpoint step and RNG state identity are required",
    )?;
    let checkpoint = manifest.get("checkpoint");
    keys(checkpoint, &["path", "size", "sha256"], &[])?;
    require(
        hash(checkpoint.text("sha256")) && !checkpoint.text("path").chars().any(char::is_control),
        "invalid checkpoint artifact identity",
    )?;
    let size = u64::try_from(checkpoint.get("size").int()?).map_err(error)?;
    require(
        size > 0 && size <= max,
        "checkpoint exceeds the explicit byte bound",
    )?;
    let relative = PathBuf::from(checkpoint.text("path"));
    let mut file = open_checkpoint(&rt.repo, &relative)?;
    let before = file.metadata().map_err(error)?;
    require(
        before.is_file() && before.len() == size,
        "checkpoint is not the expected regular file",
    )?;
    let mut hasher = PrefixSha256::default();
    let mut buffer = [0u8; 64 * 1024];
    let mut count = 0u64;
    loop {
        rt.check()?;
        if Instant::now() >= deadline {
            return Err(crate::status(
                124,
                "checkpoint verification deadline exceeded",
            ));
        }
        let read = file.read(&mut buffer).map_err(error)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or_else(|| error("checkpoint size overflow"))?;
        require(count <= size, "checkpoint grew during verification")?;
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata().map_err(error)?;
    let current = open_checkpoint(&rt.repo, &relative)?
        .metadata()
        .map_err(error)?;
    require(
        count == size
            && stamp(&before) == stamp(&after)
            && stamp(&after) == stamp(&current)
            && hasher.digest() == checkpoint.text("sha256"),
        "checkpoint bytes changed or digest mismatched",
    )?;
    Ok(object([
        ("schema", number(1)),
        ("kind", string("oms-experiment-checkpoint")),
        ("verified", b(true)),
        ("experiment_id", string(name)),
        ("run_id", string(run_id)),
        ("manifest_sha256", string(sha(raw))),
        ("contract_digest", contract.get("contract_digest").clone()),
        ("path", string(relative.to_string_lossy())),
        ("checkpoint_sha256", checkpoint.get("sha256").clone()),
        ("size", number(size)),
        ("step", manifest.get("step").clone()),
        ("rng_state_sha256", manifest.get("rng_state_sha256").clone()),
        (
            "application_state",
            string(
                "not inspected; the application must safely restore model, optimizer, scheduler and RNG state and recheck this exact file before loading",
            ),
        ),
    ]))
}
