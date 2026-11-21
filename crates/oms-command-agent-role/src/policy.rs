use crate::native_model;
use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path};

const KINDS: &[&str] = &[
    "worker",
    "builder",
    "refactorer",
    "documenter",
    "integrator",
    "performance-engineer",
    "release-engineer",
    "researcher",
    "scholar",
    "advisor",
    "architect",
    "critic",
    "reviewer",
    "verifier",
    "security-auditor",
];
const TIERS: &[&str] = &["light", "routine", "deep", "frontier"];
fn keys(value: &Value, names: &[&str]) -> bool {
    value.as_object().is_some_and(|m| {
        m.keys().map(String::as_str).collect::<BTreeSet<_>>() == names.iter().copied().collect()
    })
}
fn allowed(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|m| {
        required.iter().all(|k| m.contains_key(*k))
            && m.keys()
                .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    })
}
fn route(value: &Value) -> bool {
    if !allowed(
        value,
        &["provider", "model"],
        &["effort", "public_only", "access"],
    ) {
        return false;
    }
    if value.get("public_only").is_some_and(|v| !v.is_boolean())
        || value
            .get("access")
            .is_some_and(|v| !matches!(v.as_str(), Some("read" | "write")))
    {
        return false;
    }
    let (Some(provider), Some(model), Some(effort)) = (
        value["provider"].as_str(),
        value["model"].as_str(),
        value.get("effort").map_or(Some(""), Value::as_str),
    ) else {
        return false;
    };
    native_model::valid_role_model(provider, model)
        && native_model::validate_effort(provider, model, effort).is_ok()
}

pub(crate) fn load(root: &Path) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(
        &fs::read(root.join("config/panel-routing.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if !allowed(
        &value,
        &["schema", "main", "tiers", "roles"],
        &["tie_band", "auto_review", "council"],
    ) || value["schema"].as_f64() != Some(2.0)
    {
        return Err("unsupported panel routing policy".into());
    }
    if value.get("auto_review").is_some_and(|v| !v.is_boolean()) {
        return Err("panel auto-review setting must be boolean".into());
    }
    if let Some(band) = value.get("tie_band")
        && (!allowed(band, &[], &["gap"])
            || band
                .as_object()
                .unwrap()
                .values()
                .any(|v| v.as_u64().is_none_or(|n| n > 100)))
    {
        return Err("panel tie band limits must be integer percentages".into());
    }
    if let Some(council) = value.get("council") {
        let extra = council["extra"].as_array();
        if !keys(council, &["extra"]) || extra.is_none() || extra.unwrap().iter().any(|r| !route(r))
        {
            return Err("invalid panel council extra routes".into());
        }
        let extra = extra.unwrap();
        if extra
            .iter()
            .map(|r| r["provider"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len()
            != extra.len()
        {
            return Err("invalid panel council extra routes".into());
        }
    }
    if !keys(&value["main"], &["codex", "claude"]) {
        return Err("invalid panel main routes".into());
    }
    for provider in ["codex", "claude"] {
        let r = &value["main"][provider];
        if !route(r) || r["provider"] != provider {
            return Err(format!("invalid panel route: main/{provider}"));
        }
    }
    if !keys(&value["tiers"], TIERS) {
        return Err("invalid panel tiers".into());
    }
    for tier in TIERS {
        let Some(routes) = value["tiers"][tier].as_array() else {
            return Err(format!("invalid panel tier: {tier}"));
        };
        if routes.is_empty()
            || routes
                .iter()
                .any(|r| !route(r) || (r["provider"] == "antigravity" && r["access"] != "read"))
        {
            return Err(format!("invalid panel tier: {tier}"));
        }
        if routes
            .iter()
            .map(|r| r["provider"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len()
            != routes.len()
        {
            return Err(format!("duplicate provider in panel tier: {tier}"));
        }
    }
    if !keys(&value["roles"], KINDS) {
        return Err("panel roles must map every role to a tier".into());
    }
    for (role, row) in value["roles"].as_object().unwrap() {
        if !keys(row, &["tier"])
            || row["tier"]
                .as_str()
                .is_none_or(|tier| !TIERS.contains(&tier))
        {
            return Err(format!("invalid panel role: {role}"));
        }
    }
    Ok(value)
}
