//! Optional capability profiles and minimal installation plans.
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, require, status};
use std::collections::BTreeSet;
use std::path::Path;
const NAMES: &[&str] = &[
    "core",
    "council",
    "github",
    "notion",
    "research",
    "hpc",
    "container",
    "remote",
    "full",
];
pub fn catalog(rt: &Runtime<'_>) -> Result<Value> {
    let store = oms_command_plan::storage::Store::open(&rt.install_root)?
        .with_group_resolver(rt.services.group_resolver());
    let raw = match store.optional("config/capability-profiles.json", crate::storage::MAX_STATE)? {
        Some(raw) => {
            let value = parse(&raw)?;
            if matches!(value, Value::Null) {
                parse(include_bytes!(
                    "../../../../config/capability-profiles.json"
                ))?
            } else {
                value
            }
        }
        None => parse(include_bytes!(
            "../../../../config/capability-profiles.json"
        ))?,
    };
    require(
        raw.get("schema").is_int(1),
        "invalid capability profile catalog",
    )?;
    let profiles = raw.get("profiles");
    let fields = profiles.object()?;
    require(
        fields.len() == NAMES.len() && NAMES.iter().all(|s| profiles.get(s).object().is_ok()),
        "capability profile catalog is missing or has unsupported profiles",
    )?;
    Ok(profiles.clone())
}
fn expand(catalog: &Value, names: &[String]) -> Result<Vec<String>> {
    fn visit(
        catalog: &Value,
        name: &str,
        out: &mut Vec<String>,
        active: &mut BTreeSet<String>,
    ) -> Result<()> {
        require(
            NAMES.contains(&name),
            format!("unknown capability profile: {name}"),
        )?;
        require(
            active.insert(name.into()),
            "capability profile inheritance cycle",
        )?;
        for p in rows(catalog.get(name).get("inherits")) {
            visit(catalog, text(p), out, active)?
        }
        active.remove(name);
        if !out.iter().any(|s| s == name) {
            out.push(name.into())
        }
        Ok(())
    }
    let mut out = vec![];
    for name in names {
        visit(catalog, name, &mut out, &mut BTreeSet::new())?
    }
    Ok(out)
}
pub fn present(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|s| s.is_file() && s.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        return executable(Path::new(name));
    }
    if std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|dir| executable(&dir.join(name))))
    {
        return true;
    }
    std::env::var_os("HOME").is_some_and(|p| Path::new(&p).join(".local/bin").join(name).is_file())
}
pub fn check(rt: &Runtime<'_>, names: &[String]) -> Result<Value> {
    let catalog = catalog(rt)?;
    let expanded = expand(&catalog, names)?;
    let mut required = BTreeSet::new();
    let mut optional = BTreeSet::new();
    let mut env = BTreeSet::new();
    let mut groups = vec![];
    let mut minimum = vec![];
    for name in &expanded {
        let spec = catalog.get(name);
        for (key, set) in [
            ("required", &mut required),
            ("optional", &mut optional),
            ("environment", &mut env),
        ] {
            set.extend(rows(spec.get(key)).iter().map(|v| text(v).to_owned()))
        }
        groups.extend(rows(spec.get("any_of")).iter().cloned());
        if spec.get("min_present").truth() {
            minimum.push((name, spec.get("min_present")))
        }
    }
    let mut missing = BTreeSet::new();
    let mut required_status = empty();
    for name in &required {
        let exists = present(name);
        required_status.set(name, b(exists))?;
        if !exists {
            missing.insert(name.clone());
        }
    }
    let mut optional_status = empty();
    for name in optional.difference(&required) {
        optional_status.set(name, b(present(name)))?;
    }
    let mut env_status = empty();
    for name in env {
        let exists = std::env::var_os(&name).is_some_and(|v| !v.is_empty());
        env_status.set(&name, b(exists))?;
        if !exists {
            missing.insert(format!("env:{name}"));
        }
    }
    let mut any_status = vec![];
    for g in groups {
        let commands = g.strings()?;
        let found = commands
            .iter()
            .filter(|n| present(n))
            .copied()
            .collect::<Vec<_>>();
        if found.is_empty() {
            missing.insert(format!("one-of:{}", commands.join("|")));
        }
        any_status.push(object([
            ("commands", g.clone()),
            ("present", list(found.iter().copied())),
            ("satisfied", b(!found.is_empty())),
        ]));
    }
    let mut minimum_status = vec![];
    for (name, g) in minimum {
        let commands = g.get("commands").strings()?;
        let count = g.opt("count").map(|v| v.int()).transpose()?.unwrap_or(1);
        let found = commands
            .iter()
            .filter(|n| present(n))
            .copied()
            .collect::<Vec<_>>();
        let satisfied = found.len() as i128 >= count;
        if !satisfied {
            missing.insert(format!("at-least-{count}:{}", commands.join("|")));
        }
        minimum_status.push(object([
            ("profile", string(name)),
            ("commands", g.get("commands").clone()),
            ("required_count", number(count)),
            ("present", list(found)),
            ("satisfied", b(satisfied)),
        ]));
    }
    Ok(object([
        ("schema", number(1)),
        ("requested", list(names.iter().map(String::as_str))),
        ("expanded", list(expanded.iter().map(String::as_str))),
        ("ready", b(missing.is_empty())),
        ("required", required_status),
        ("any_of", a(any_status)),
        ("minimum", a(minimum_status)),
        ("environment", env_status),
        ("optional", optional_status),
        ("missing", list(missing.iter().map(String::as_str))),
        (
            "installation_policy",
            string(
                "missing optional adapters disable only their capability; local state remains usable",
            ),
        ),
    ]))
}
pub fn current(rt: &Runtime<'_>) -> Result<Value> {
    let mut row = rt.json(".oms/runtime/profiles.json", Value::Null)?;
    if row.object().is_err() {
        return Ok(object([
            ("schema", number(1)),
            ("requested", list(["core"])),
            ("configured", b(false)),
            ("check", check(rt, &["core".into()])?),
        ]));
    }
    let mut names = rows(row.get("requested"))
        .iter()
        .map(|v| text(v).to_owned())
        .filter(|s| NAMES.contains(&s.as_str()))
        .collect::<Vec<_>>();
    if names.is_empty() {
        names.push("core".into())
    }
    row.set("requested", list(names.iter().map(String::as_str)))?;
    row.set("configured", b(true))?;
    row.set("check", check(rt, &names)?)?;
    Ok(row)
}
pub fn apply(rt: &Runtime<'_>, names: &[String], allow_missing: bool) -> Result<Value> {
    let check = check(rt, names)?;
    if !check.get("ready").truth() && !allow_missing {
        return Err(status(
            3,
            format!(
                "capability profile is not ready: {}",
                check.get("missing").strings()?.join(", ")
            ),
        ));
    }
    let mut row = object([
        ("schema", number(1)),
        ("updated_at", string(&rt.now)),
        ("requested", list(names.iter().map(String::as_str))),
        ("expanded", check.get("expanded").clone()),
        ("ready_at_write", check.get("ready").clone()),
        ("missing_at_write", check.get("missing").clone()),
    ]);
    rt.write_json(Path::new(".oms/runtime/profiles.json"), &row)?;
    row.set("check", check)?;
    Ok(row)
}
pub fn install_plan(rt: &Runtime<'_>, names: &[String], primary: &str) -> Result<Value> {
    require(
        ["codex", "claude", "agy"].contains(&primary),
        "primary provider must be codex, claude, or agy",
    )?;
    let mut requested = vec![];
    for n in names {
        if !requested.contains(n) {
            requested.push(n.clone())
        }
    }
    let catalog = catalog(rt)?;
    let expanded = expand(&catalog, &requested)?;
    let order = ["codex", "claude", "agy"];
    let mut managed = Vec::<String>::new();
    for name in &expanded {
        for t in rows(catalog.get(name).get("managed_tools")) {
            let selected = match text(t) {
                "primary-provider" => vec![primary],
                "secondary-provider" => {
                    let candidates = order
                        .iter()
                        .filter(|n| **n != primary)
                        .copied()
                        .collect::<Vec<_>>();
                    vec![
                        candidates
                            .iter()
                            .find(|n| present(n))
                            .copied()
                            .unwrap_or(candidates[0]),
                    ]
                }
                "all-providers" => order.to_vec(),
                other => vec![other],
            };
            for value in selected {
                if !managed.iter().any(|s| s == value) {
                    managed.push(value.into())
                }
            }
        }
    }
    if managed
        .iter()
        .any(|s| ["codex", "claude", "ntn"].contains(&s.as_str()))
    {
        managed.insert(0, "node".into());
    }
    let mut command = vec![string("oms"), string("install-profile")];
    for name in &requested {
        command.extend([string("--profile"), string(name)]);
    }
    command.extend([string("--primary-provider"), string(primary)]);
    Ok(object([
        ("schema", number(1)),
        ("requested", list(requested.iter().map(String::as_str))),
        ("expanded", list(expanded.iter().map(String::as_str))),
        ("primary_provider", string(primary)),
        ("managed_tools", list(managed.iter().map(String::as_str))),
        ("notion_optional", b(!expanded.contains(&"notion".into()))),
        ("github_optional", b(!expanded.contains(&"github".into()))),
        // Wave 4 will migrate this legacy compatibility label.
        ("selective_installer", string("scripts/install-profile.sh")),
        ("command", a(command)),
    ]))
}
