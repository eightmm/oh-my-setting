use crate::engine::{clean_paths, split_list, valid_id};
use crate::json::{self, Value, empty, list, number, object, string};
use crate::storage::{MAX_JSON, Store};
use crate::{Action, Plan, Request, Response, Result, Snapshot, error, require};
use std::path::{Component, Path};

const PATH: &str = ".oms/plan/contracts.json";

pub(crate) fn load(store: &Store) -> Result<(Value, Option<Vec<u8>>)> {
    let snapshot = store.snapshot(PATH, MAX_JSON, true)?;
    if let Some(file) = &snapshot {
        require(file.mode == 0o600, "contract registry requires mode 0600")?;
    }
    let raw = snapshot.map(|file| file.raw);
    let registry = match &raw {
        Some(bytes) => json::parse(bytes)?,
        None => object([("schema", number(1)), ("contracts", empty())]),
    };
    require(
        registry.get("schema").is_int(1),
        "invalid contract registry schema",
    )?;
    for (key, contract) in registry.get("contracts").object()? {
        let id = std::str::from_utf8(key).map_err(|_| error("invalid contract id"))?;
        require(
            valid_id(id) && contract.text("id") == id,
            "invalid contract id",
        )?;
        let accepted = contract.get("accepted_version").int()?;
        require(accepted >= 0, "invalid accepted version")?;
        let versions = contract.get("versions");
        for (key, version) in versions.object()? {
            let name = std::str::from_utf8(key).map_err(|_| error("invalid contract version"))?;
            let n = name
                .parse::<u64>()
                .map_err(|_| error("invalid contract version"))?;
            require(
                n > 0 && name == n.to_string() && version.get("version").int()? == i128::from(n),
                "invalid contract version",
            )?;
            require(
                matches!(version.text("status"), "proposed" | "accepted"),
                "invalid contract status",
            )?;
            let mut payload = version.clone();
            payload.remove("status");
            payload.remove("sha256");
            require(
                json::hash(&payload)? == version.text("sha256"),
                "contract version digest mismatch",
            )?;
        }
        if accepted == 0 {
            require(
                contract.text("accepted_sha256").is_empty(),
                "invalid accepted digest",
            )?;
        } else {
            let version = versions
                .opt(&accepted.to_string())
                .ok_or_else(|| error("accepted contract version missing"))?;
            require(
                version.text("status") == "accepted"
                    && version.text("sha256") == contract.text("accepted_sha256"),
                "invalid accepted contract",
            )?;
        }
    }
    Ok((registry, raw))
}

pub(crate) fn stale(registry: &Value, task: &Value) -> Result<Vec<String>> {
    let refs = task
        .opt("contracts")
        .map(Value::strings)
        .transpose()?
        .unwrap_or_default();
    let mut stale = Vec::new();
    for reference in refs {
        let (id, version) = match reference.split_once('@') {
            Some((id, v)) => (id, v.strip_prefix('v').and_then(|n| n.parse::<i128>().ok())),
            None => (reference, Some(0)),
        };
        let accepted = registry
            .get("contracts")
            .get(id)
            .get("accepted_version")
            .int()
            .unwrap_or(0);
        if (accepted <= 0 || Some(accepted) != version) && !stale.iter().any(|v| v == id) {
            stale.push(id.to_owned());
        }
    }
    Ok(stale)
}

pub(crate) fn execute(plan: &Plan<'_>, req: &Request) -> Result<Response> {
    let (mut registry, raw) = load(&plan.store)?;
    let mut contracts = registry.get("contracts").clone();
    if req.action == Action::ContractList {
        return Response::json(&contracts);
    }
    let id = req.value("id")?;
    require(valid_id(id), "--id must match [A-Za-z0-9._-]+")?;
    if req.action == Action::ContractShow {
        return Response::json(contracts.opt(id).ok_or_else(|| error("no such contract"))?);
    }
    if req.action == Action::ContractAccept {
        plan.context.parent()?;
    }
    let mut contract = contracts.opt(id).cloned().unwrap_or_else(|| {
        object([
            ("id", string(id)),
            ("accepted_version", number(0)),
            ("accepted_sha256", string("")),
            ("versions", empty()),
        ])
    });
    let mut versions = contract.get("versions").clone();
    if req.action == Action::ContractPropose {
        let summary = req.value("summary")?;
        require(!summary.trim().is_empty(), "--summary must be non-empty")?;
        let paths = clean_paths(&split_list(req.text("scope-paths")?))?;
        let artifact = req.text("decision-artifact")?;
        let digest = if req.has("decision-artifact") {
            require(
                !artifact.is_empty()
                    && Path::new(artifact)
                        .components()
                        .all(|c| matches!(c, Component::Normal(_))),
                "decision artifact must be a safe repo-relative path",
            )?;
            json::sha(plan.source.read(artifact, 64 * 1024)?)
        } else {
            String::new()
        };
        let mut max = 0u64;
        for (key, _) in versions.object()? {
            let n = std::str::from_utf8(key)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| error("invalid contract version"))?;
            max = max.max(n);
        }
        let version = max
            .checked_add(1)
            .ok_or_else(|| error("contract version overflow"))?;
        let mut v = object([
            ("version", number(version)),
            ("summary", string(summary)),
            ("scope_paths", list(paths.iter().map(String::as_str))),
            ("decision_artifact", string(artifact)),
            ("decision_artifact_sha256", string(digest)),
            ("ts", string(&plan.context.ts)),
        ]);
        let hash = json::hash(&v)?;
        v.set("sha256", string(hash))?;
        v.set("status", string("proposed"))?;
        versions.set(&version.to_string(), v)?;
    } else {
        let version = req
            .value("version")?
            .parse::<u64>()
            .map_err(|_| error("--version must be a positive integer"))?;
        require(version > 0, "--version must be a positive integer")?;
        let key = version.to_string();
        let mut v = versions
            .opt(&key)
            .cloned()
            .ok_or_else(|| error("no such contract version"))?;
        v.set("status", string("accepted"))?;
        contract.set("accepted_version", number(version))?;
        contract.set("accepted_sha256", v.get("sha256").clone())?;
        versions.set(&key, v)?;
    }
    contract.set("versions", versions)?;
    contracts.set(id, contract.clone())?;
    registry.set("contracts", contracts)?;
    plan.checkpoint()?;
    plan.store.mutate(
        PATH,
        raw.as_deref().or(Some(&[])),
        &json::encoded(&registry)?,
        MAX_JSON,
        true,
    )?;
    Response::json(&contract)
}

pub(crate) fn refresh(plan: &Plan<'_>, req: &Request, snapshot: &mut Snapshot) -> Result<Response> {
    plan.context.parent()?;
    let id = req.value("task")?;
    require(valid_id(id), "invalid task id")?;
    let mut task = snapshot.task(id)?.clone();
    let state = task.text("state").to_owned();
    require(
        !matches!(state.as_str(), "done" | "cancelled" | "landing"),
        format!("cannot refresh contracts on {state} task"),
    )?;
    let refs = task
        .opt("contracts")
        .map(Value::strings)
        .transpose()?
        .unwrap_or_default();
    let mut pins = Vec::new();
    for reference in &refs {
        let cid = reference.split('@').next().unwrap_or("");
        require(valid_id(cid), "invalid contract reference")?;
        let accepted = snapshot
            .registry
            .get("contracts")
            .get(cid)
            .get("accepted_version")
            .int()
            .unwrap_or(0);
        require(
            accepted > 0,
            format!("contract {cid} has no accepted version"),
        )?;
        pins.push(format!("{cid}@v{accepted}"));
    }
    let changed = pins.len() != refs.len() || pins.iter().zip(&refs).any(|(p, r)| p != r);
    if changed
        && (matches!(state.as_str(), "claimed" | "running" | "review" | "blocked")
            || !task.text("lease_id").is_empty()
            || !task.text("review_lease_id").is_empty()
            || !task.text("artifact").is_empty()
            || !task.text("patch").is_empty()
            || task.opt("panel_accept").is_some())
    {
        let blocked_reason = (state == "blocked").then(|| task.text("reason").to_owned());
        crate::engine::clear_claim(snapshot, &mut task)?;
        if let Some(reason) = blocked_reason {
            task.set("state", string("blocked"))?;
            task.set("reason", string(&reason))?;
        }
        for field in [
            "review_lease_id",
            "artifact",
            "patch",
            "executor_id",
            "executor_soul_sha256",
            "repair_artifact",
        ] {
            task.set(field, string(""))?;
        }
        task.set("repair_count", number(0))?;
        task.remove("panel_accept");
    }
    task.set("updated", string(&plan.context.ts))?;
    task.set("contracts", list(pins.iter().map(String::as_str)))?;
    snapshot.put(id, task.clone())?;
    plan.save(snapshot)?;
    Response::json(&task)
}
