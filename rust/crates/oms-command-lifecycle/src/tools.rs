use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};
fn strings(v: &Value) -> Result<Vec<String>> {
    v.as_array()
        .ok_or_else(|| r::Error(2, "capability command list must be an array".into()))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or_else(|| r::Error(2, "capability command must be a string".into()))
        })
        .collect()
}
fn expand(
    name: &str,
    table: &Value,
    active: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> Result<()> {
    if seen.contains(name) {
        return Ok(());
    }
    if !active.insert(name.into()) {
        return r::fail("capability inheritance cycle");
    }
    let spec = table
        .get(name)
        .filter(|s| s.is_object())
        .ok_or_else(|| r::Error(2, format!("unknown capability profile: {name}")))?;
    if let Some(parents) = spec.get("inherits") {
        for parent in strings(parents)? {
            expand(&parent, table, active, seen, ordered)?;
        }
    }
    active.remove(name);
    seen.insert(name.into());
    ordered.push(name.into());
    Ok(())
}
pub(crate) fn plan(paths: &Paths, options: &Options) -> Result<Value> {
    let table = r::value(&paths.source.join("config/capability-profiles.json"))?;
    if table["schema"] != 1 {
        return r::fail("unsupported capability profile schema");
    }
    let receipt = options
        .path("--receipt", &paths.cwd)?
        .unwrap_or(paths.config.join("capabilities.json"));
    let previous = if options.has("--reapply") || options.has("--add") {
        Some(r::value(&receipt)?)
    } else {
        None
    };
    let mut profiles = options.all("--profile").to_vec();
    if let Some(previous) = previous {
        for name in strings(&previous["profiles"])? {
            if !profiles.contains(&name) {
                profiles.push(name);
            }
        }
    }
    if profiles.is_empty() {
        profiles.push("core".into());
    }
    let mut ordered = Vec::new();
    let mut seen = BTreeSet::new();
    for name in profiles {
        expand(
            &name,
            &table["profiles"],
            &mut BTreeSet::new(),
            &mut seen,
            &mut ordered,
        )?;
    }
    let primary = options.get("--primary-provider").unwrap_or("auto");
    if !matches!(primary, "auto" | "codex" | "claude" | "agy") {
        return r::fail("primary provider must be auto, codex, claude, or agy");
    }
    let provider = if primary == "auto" {
        ["codex", "claude", "agy"]
            .into_iter()
            .find(|p| r::find(p, &paths.cwd).is_some())
            .unwrap_or("codex")
    } else {
        primary
    };
    let mut results = Vec::new();
    let mut missing = BTreeSet::new();
    for name in &ordered {
        let profile = &table["profiles"][name];
        let mut absent = Vec::new();
        for command in strings(&profile["required"])? {
            if command == "python3" {
                continue;
            }
            if r::find(&command, &paths.cwd).is_none() {
                absent.push(command);
            }
        }
        for alternatives in profile["any_of"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid any_of capabilities".into()))?
        {
            let choices = strings(alternatives)?;
            if !choices.iter().any(|s| r::find(s, &paths.cwd).is_some()) {
                absent.push(choices.join("|"));
            }
        }
        if let Some(count) = profile["min_present"]["count"].as_u64() {
            let commands = strings(&profile["min_present"]["commands"])?;
            if commands
                .iter()
                .filter(|s| r::find(s, &paths.cwd).is_some())
                .count()
                < count as usize
            {
                absent.push(format!("at least {count} provider commands"));
            }
        }
        if let Some(vars) = profile.get("environment") {
            for var in strings(vars)? {
                if std::env::var_os(&var).is_none_or(|v| v.is_empty()) {
                    absent.push(format!("environment {var}"));
                }
            }
        }
        missing.extend(absent.iter().cloned());
        results.push(json!({"profile":name,"available":absent.is_empty(),"missing":absent}));
    }
    Ok(
        json!({"schema":1,"profiles":ordered,"primary_provider":provider,"receipt":receipt,"checks":results,"missing":missing,"native_runtime":true}),
    )
}
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.has("--only") || options.has("--upgrade") || options.has("--full") {
        return r::fail("native tool selection/download/upgrade adapters are not yet available");
    }
    let plan = plan(paths, options)?;
    if options.preview() {
        return r::json(&plan);
    }
    let missing = !plan["missing"].as_array().unwrap().is_empty();
    if options.has("--check") {
        let mut out = r::json(&plan)?;
        out.0 = i32::from(missing);
        return Ok(out);
    }
    r::child_guard()?;
    paths.require_owner()?;
    if missing && !options.has("--allow-missing") {
        return r::fail(
            "selected tools are missing; native verified-download/provider installer adapters are required before apply",
        );
    }
    let receipt = options
        .path("--receipt", &paths.cwd)?
        .unwrap_or(paths.config.join("capabilities.json"));
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    let mut tx = Transaction::begin(paths)?;
    let mut row = plan;
    row["installed_at"] = json!(r::now());
    row["status"] = json!(if missing { "degraded" } else { "available" });
    r::check(flag)?;
    tx.write(&receipt, &r::bytes(&row)?, 0o600)?;
    tx.commit()?;
    r::json(&row)
}
