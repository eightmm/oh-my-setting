use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};
pub(crate) fn strings(v: &Value) -> Result<Vec<String>> {
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
        if r::exists(&receipt) {
            Some(r::value(&receipt)?)
        } else if options.has("--reapply") {
            return r::fail("reapply requires a capability receipt");
        } else {
            None
        }
    } else {
        None
    };
    if options.has("--reapply") && (options.has("--add") || options.has("--profile")) {
        return r::fail("reapply cannot be combined with add or profile");
    }
    if options.has("--add") && !options.has("--profile") {
        return r::fail("add needs at least one profile");
    }
    if options.has("--full") && options.has("--only") {
        return r::fail("full and only are exclusive");
    }
    let mut profiles = options.all("--profile").to_vec();
    if options.has("--full") {
        profiles = vec!["full".into()];
    }
    let previous_provider = previous
        .as_ref()
        .and_then(|v| v["primary_provider"].as_str())
        .map(str::to_owned);
    let allow_missing = options.has("--allow-missing")
        || previous
            .as_ref()
            .is_some_and(|v| v["allow_missing"] == true);
    if let Some(previous) = &previous {
        let requested = previous
            .get("requested")
            .or_else(|| previous["plan"].get("requested"))
            .or_else(|| previous.get("profiles"))
            .ok_or_else(|| r::Error(2, "invalid capability receipt".into()))?;
        for name in strings(requested)? {
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
    for name in &profiles {
        expand(
            name,
            &table["profiles"],
            &mut BTreeSet::new(),
            &mut seen,
            &mut ordered,
        )?;
    }
    let primary = options
        .get("--primary-provider")
        .or(previous_provider.as_deref())
        .unwrap_or("auto");
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
            if !available(paths, &command) {
                absent.push(command);
            }
        }
        for alternatives in profile["any_of"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid any_of capabilities".into()))?
        {
            let choices = strings(alternatives)?;
            if !choices.iter().any(|s| available(paths, s)) {
                absent.push(choices.join("|"));
            }
        }
        if profile["min_present"]
            .get("count")
            .is_some_and(|v| v.as_u64().is_none())
        {
            return r::fail("minimum capability count must be unsigned");
        }
        if let Some(count) = profile["min_present"]["count"].as_u64() {
            let commands = strings(&profile["min_present"]["commands"])?;
            if commands.iter().filter(|s| available(paths, s)).count() < count as usize {
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
        let optional = strings(&profile["optional"])?
            .into_iter()
            .map(|s| {
                let present = available(paths, &s);
                (s, json!(present))
            })
            .collect::<serde_json::Map<_, _>>();
        results.push(json!({"profile":name,"available":absent.is_empty(),"missing":absent,"optional":optional}));
    }
    let mut managed = Vec::<String>::new();
    for name in &ordered {
        for tool in strings(&table["profiles"][name]["managed_tools"])? {
            let selected = match tool.as_str() {
                "primary-provider" => vec![provider.to_owned()],
                "secondary-provider" => vec![
                    ["codex", "claude", "agy"]
                        .into_iter()
                        .filter(|s| *s != provider)
                        .find(|s| available(paths, s))
                        .unwrap_or(if provider == "codex" {
                            "claude"
                        } else {
                            "codex"
                        })
                        .to_owned(),
                ],
                "all-providers" => vec!["codex".into(), "claude".into(), "agy".into()],
                _ => vec![tool],
            };
            for tool in selected {
                if !managed.contains(&tool) {
                    managed.push(tool);
                }
            }
        }
    }
    if options.tool == "install-tools" && !options.has("--profile") && !options.has("--only") {
        managed = ["node", "uv", "claude", "codex", "ntn", "agy", "gh"]
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    if let Some(only) = options.get("--only") {
        managed.clear();
        missing.clear();
        results.clear();
        for tool in only
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|s| !s.is_empty())
        {
            if !["node", "uv", "claude", "codex", "ntn", "agy", "gh"].contains(&tool) {
                return r::fail(format!("unknown tool selection: {tool}"));
            }
            if !managed.iter().any(|s| s == tool) {
                managed.push(tool.into());
            }
            if !available(paths, tool) {
                missing.insert(tool.into());
            }
            results.push(json!({"tool":tool,"available":available(paths,tool)}));
        }
        if managed.is_empty() {
            return r::fail("only requires at least one tool");
        }
    }
    if options.has("--providers") {
        if options.has("--only") || options.has("--profile") || options.has("--full") {
            return r::fail(
                "providers refresh cannot be combined with a fresh tool/profile selection",
            );
        }
        managed = ["codex", "claude", "agy"]
            .into_iter()
            .filter(|s| available(paths, s))
            .map(str::to_owned)
            .collect();
        missing.clear();
        results.clear();
    }
    if !options.has("--providers")
        && managed
            .iter()
            .any(|s| ["codex", "claude", "ntn"].contains(&s.as_str()))
        && !managed.iter().any(|s| s == "node")
    {
        managed.insert(0, "node".into());
    }
    Ok(
        json!({"schema":1,"requested":profiles,"profiles":ordered,"expanded":ordered,"primary_provider":provider,"managed_tools":managed,"receipt":receipt,"checks":results,"missing":missing,"allow_missing":allow_missing,"native_runtime":true}),
    )
}
fn available(paths: &Paths, name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    r::find(name, &paths.cwd).is_some()
        || std::fs::metadata(paths.home.join(".local/bin").join(name))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
pub(crate) fn apply(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<Value> {
    let initial = plan(paths, options)?;
    crate::supply::install(
        paths,
        &strings(&initial["managed_tools"])?,
        options.has("--upgrade") || options.has("--providers"),
        !options.has("--providers"),
        tx,
        flag,
    )?;
    if options.has("--providers") {
        crate::supply::refresh_optional(paths, tx, flag)?;
    }
    let mut result = plan(paths, options)?;
    let missing = !result["missing"].as_array().unwrap().is_empty();
    if missing && result["allow_missing"] != true {
        return r::fail(
            "selected capabilities remain unavailable; external adapters must be supplied",
        );
    }
    result["installed_at"] = json!(r::now());
    result["updated_at"] = json!(r::now());
    result["status"] = json!(if missing { "degraded" } else { "available" });
    result["plan"] = initial;
    Ok(result)
}

pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
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
    let receipt = options
        .path("--receipt", &paths.cwd)?
        .unwrap_or(paths.config.join("capabilities.json"));
    let _guard = r::receipt_lock(paths, flag)?;
    let mut tx = Transaction::begin(paths)?;
    let row = apply(paths, options, &mut tx, flag)?;
    r::check(flag)?;
    tx.write(&receipt, &r::bytes(&row)?, 0o600)?;
    tx.commit()?;
    r::json(&row)
}
