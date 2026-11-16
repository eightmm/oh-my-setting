use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
fn identity(path: &Path) -> Result<Value> {
    let m = fs::symlink_metadata(path)?;
    Ok(json!({"st_uid":m.uid(),"st_dev":m.dev(),"st_ino":m.ino()}))
}
fn private(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o022 != 0
        || if directory {
            !m.is_dir()
        } else {
            !m.is_file() || m.nlink() != 1
        }
    {
        return r::fail("account ownership state is unsafe; installation retained");
    }
    Ok(())
}
fn load(path: &Path, default: Value) -> Result<Value> {
    if r::exists(path) {
        private(path, false)?;
        r::value(path)
    } else {
        Ok(default)
    }
}
fn home(row: &Value, expected: &Value) -> Result<PathBuf> {
    let path = PathBuf::from(
        row["home"]
            .as_str()
            .ok_or_else(|| r::Error(2, "invalid account home".into()))?,
    );
    if !path.is_absolute() || r::physical(&path)? != path {
        return r::fail("account home identity changed; ownership retained");
    }
    private(&path, true)?;
    let actual = identity(&path)?;
    for field in ["st_dev", "st_ino"] {
        if expected[field].as_u64().is_none() || expected[field] != actual[field] {
            return r::fail("account directory identity changed; installation retained");
        }
    }
    for ancestor in path.ancestors().skip(1) {
        let m = fs::metadata(ancestor)?;
        if ![0, rustix::process::geteuid().as_raw()].contains(&m.uid())
            || m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0
        {
            return r::fail("unsafe account ancestor; ownership retained");
        }
    }
    Ok(path)
}
fn exact_link(path: &Path, target: &Path) -> bool {
    path.is_symlink() && fs::read_link(path).is_ok_and(|p| p == target)
}
struct Settings {
    path: PathBuf,
    before: Vec<u8>,
    after: Value,
    side: PathBuf,
    ownership: Value,
    parent: Value,
    previous_identity: Option<Value>,
    side_before: Option<Vec<u8>>,
}
struct Link {
    path: PathBuf,
    target: PathBuf,
    parent: Value,
    entry: Value,
}
pub(crate) struct Plan {
    settings: Vec<Settings>,
    links: Vec<Link>,
    writes: Vec<(PathBuf, Value)>,
}
pub(crate) fn plan(paths: &Paths) -> Result<Plan> {
    let mut plan = Plan {
        settings: Vec::new(),
        links: Vec::new(),
        writes: Vec::new(),
    };
    let codex = paths.config.join("codex-account-links.json");
    let links = load(&codex, json!([]))?;
    let mut retained = Vec::new();
    for row in links
        .as_array()
        .ok_or_else(|| r::Error(2, "invalid Codex account ownership receipt".into()))?
    {
        let directory = home(row, row)?;
        let entries = row["links"]
            .as_object()
            .ok_or_else(|| r::Error(2, "invalid Codex account links".into()))?;
        let mut keep = row.clone();
        for (name, target) in entries {
            if ![
                "AGENTS.md",
                "skills",
                ".hook-trust",
                "config.toml",
                "rules",
                "hooks.json",
                "plugins",
            ]
            .contains(&name.as_str())
            {
                return r::fail("unrecognized account link name");
            }
            let target = PathBuf::from(
                target
                    .as_str()
                    .ok_or_else(|| r::Error(2, "invalid account link target".into()))?,
            );
            if !target.is_absolute() {
                return r::fail("invalid account link target");
            }
            let path = directory.join(name);
            if exact_link(&path, &target) {
                let main = paths.codex_home()?.join(name);
                if r::physical(&target)? == r::physical(&main)?
                    || r::physical(&target)?.starts_with(&paths.source)
                {
                    plan.links.push(Link {
                        path: path.clone(),
                        target,
                        parent: identity(&directory)?,
                        entry: identity(&path)?,
                    });
                    keep["links"].as_object_mut().unwrap().remove(name);
                }
            } else {
                keep["links"].as_object_mut().unwrap().remove(name);
            }
        }
        if !keep["links"].as_object().unwrap().is_empty() {
            retained.push(keep);
        }
    }
    if !retained.is_empty() {
        return r::fail("Codex account cleanup incomplete; outside targets retained");
    }
    if r::exists(&codex) {
        plan.writes.push((codex, json!([])));
    }
    let effects_file = paths.config.join("provider-account-effects.json");
    let registry_file = paths.config.join("provider-accounts.claude.json");
    let effects = load(&effects_file, json!({"schema":1,"accounts":[]}))?;
    let mut registry = load(
        &registry_file,
        json!({"schema":1,"provider":"claude","accounts":[]}),
    )?;
    if effects["schema"] != 1 || registry["schema"] != 1 || registry["provider"] != "claude" {
        return r::fail("invalid Claude account registry");
    }
    let rows = effects["accounts"]
        .as_array()
        .ok_or_else(|| r::Error(2, "invalid Claude account effects".into()))?;
    let legacy = load(
        &paths.config.join("provider-accounts.json"),
        json!({"schema":1,"providers":{}}),
    )?;
    if legacy["schema"] != 1 || !legacy["providers"].is_object() {
        return r::fail("invalid provider account registry");
    }
    if let Some(entries) = legacy["providers"]["claude"]["accounts"].as_array() {
        for entry in entries {
            if entry["id"] != "default" && !rows.iter().any(|row| row["home"] == entry["home"]) {
                return r::fail("legacy Claude account ownership missing; global cleanup refused");
            }
        }
    }
    for registered in registry["accounts"]
        .as_array()
        .ok_or_else(|| r::Error(2, "invalid Claude account registry".into()))?
    {
        if !rows.iter().any(|r| r["home"] == registered["home"]) {
            return r::fail("Claude account ownership missing; installation retained");
        }
    }
    for row in rows {
        if row["provider"] != "claude"
            || !matches!(
                row["status"].as_str(),
                Some("installed" | "pending" | "removing")
            )
        {
            return r::fail("invalid Claude account effect status");
        }
        let directory = PathBuf::from(
            row["home"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid Claude account home".into()))?,
        );
        if row["identity"].is_null()
            && !r::exists(&directory)
            && row["links"].as_object().is_some_and(|m| m.is_empty())
        {
            continue;
        }
        let directory = home(row, &row["identity"])?;
        let parent = PathBuf::from(
            row["parent"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid account reservation parent".into()))?,
        );
        if !directory.starts_with(&parent) || identity(&parent)? != row["parent_identity"] {
            return r::fail("Claude account reservation identity changed");
        }
        if let Some(settings) = settings_plan(paths, row, &directory)? {
            plan.settings.push(settings);
        }
        for (name, link) in row["links"]
            .as_object()
            .ok_or_else(|| r::Error(2, "invalid Claude account links".into()))?
        {
            if !["CLAUDE.md", "skills", "output-styles/oms-korean.md"].contains(&name.as_str()) {
                return r::fail("invalid Claude account link name");
            }
            let path = directory.join(name);
            let target = PathBuf::from(
                link["target"]
                    .as_str()
                    .ok_or_else(|| r::Error(2, "invalid account link target".into()))?,
            );
            if r::exists(&path) {
                if !exact_link(&path, &target) || identity(&path)? != link["identity"] {
                    return r::fail("changed Claude account link retained");
                }
                private(path.parent().unwrap(), true)?;
                plan.links.push(Link {
                    parent: identity(path.parent().unwrap())?,
                    entry: identity(&path)?,
                    path,
                    target,
                });
            }
        }
    }
    registry["accounts"] = json!([]);
    if r::exists(&registry_file) {
        plan.writes.push((registry_file, registry));
    }
    if r::exists(&effects_file) {
        plan.writes
            .push((effects_file, json!({"schema":1,"accounts":[]})));
    }
    Ok(plan)
}
pub(crate) fn apply(plan: Plan, tx: &mut Transaction) -> Result<()> {
    for mut settings in plan.settings {
        let current = if r::exists(&settings.path) {
            r::read(&settings.path, r::LIMIT)?
        } else {
            Vec::new()
        };
        if current != settings.before {
            return r::fail("account settings changed concurrently; cleanup refused");
        }
        let raw = r::bytes(&settings.after)?;
        tx.write_bound(
            &settings.path,
            &raw,
            0o600,
            &settings.parent,
            settings
                .previous_identity
                .as_ref()
                .map(|_| settings.before.as_slice()),
            settings.previous_identity.as_ref(),
        )?;
        settings.ownership["settings_identity"] = identity(&settings.path)?;
        settings.ownership["settings_digest"] = json!(r::hash(&raw));
        tx.write_bound(
            &settings.side,
            &r::bytes(&settings.ownership)?,
            0o600,
            &settings.parent,
            settings.side_before.as_deref(),
            None,
        )?;
    }
    for link in plan.links {
        if identity(link.path.parent().unwrap())? != link.parent
            || identity(&link.path)? != link.entry
            || !exact_link(&link.path, &link.target)
        {
            return r::fail("account link changed concurrently; ownership retained");
        }
        tx.remove_link_bound(&link.path, &link.parent, &link.entry, &link.target)?;
    }
    for (path, value) in plan.writes {
        tx.write(&path, &r::bytes(&value)?, 0o600)?;
    }
    Ok(())
}

pub(crate) fn permissions(paths: &Paths, tx: &mut Transaction) -> Result<()> {
    let settings = std::env::var_os("OMS_ANTIGRAVITY_SETTINGS")
        .map(PathBuf::from)
        .unwrap_or(paths.home.join(".gemini/antigravity-cli/settings.json"));
    let side = settings.with_file_name(format!(
        "{}.oh-my-setting-permissions.json",
        settings.file_name().unwrap_or_default().to_string_lossy()
    ));
    if !r::exists(&side) {
        return Ok(());
    }
    let state = load(&side, Value::Null)?;
    if state["schema"] != 1 {
        return r::fail("invalid managed permission ownership");
    }
    let rules = crate::tools::strings(&state["rules"])?;
    if !r::exists(&settings) {
        tx.remove(&side)?;
        return Ok(());
    }
    let raw = r::read(&settings, r::LIMIT)?;
    let mut current = r::parse(&raw)?;
    if !current.is_object() {
        return r::fail("invalid permission settings");
    }
    if let Some(removal) = state.get("removal") {
        let digest = r::hash(&raw);
        if removal["after_sha256"] == digest {
            tx.remove(&side)?;
            return Ok(());
        }
        if removal["before_sha256"] != digest {
            return r::fail("permission removal journal conflicts with current settings");
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    if let Some(allow) = current["permissions"]["allow"].as_array_mut() {
        for rule in rules {
            if seen.insert(rule.clone())
                && let Some(index) = allow.iter().position(|v| v == &json!(rule))
            {
                allow.remove(index);
            }
        }
    } else if !current["permissions"]["allow"].is_null() {
        return r::fail("permission allow list is malformed");
    }
    let after = r::bytes(&current)?;
    if let Some(removal) = state.get("removal")
        && removal["after_sha256"] != r::hash(&after)
    {
        return r::fail("permission removal recovery bytes do not match");
    }
    tx.write(&settings, &after, 0o600)?;
    tx.remove(&side)
}
pub(crate) fn journal(paths: &Paths, tx: &mut Transaction) -> Result<()> {
    let file = std::env::var_os("OMS_WORK_JOURNAL_CONFIG")
        .map(PathBuf::from)
        .unwrap_or(paths.config.join("work-journal.json"));
    if r::exists(&file) {
        let row = r::value(&file)?;
        if row["managed_by"] == "oh-my-setting" {
            tx.remove(&file)?;
        }
    }
    Ok(())
}

fn parent_mut<'a>(value: &'a mut Value, parts: &[String]) -> Option<&'a mut Value> {
    if parts.is_empty() {
        return None;
    }
    let mut cursor = value;
    for part in &parts[..parts.len() - 1] {
        cursor = cursor.as_object_mut()?.get_mut(part)?;
    }
    cursor.is_object().then_some(cursor)
}

fn settings_plan(paths: &Paths, row: &Value, directory: &Path) -> Result<Option<Settings>> {
    let settings = directory.join("settings.json");
    let side = directory.join(".oms-claude-hooks.json");
    if row["settings"] != json!(settings) || row["hooks_receipt"] != json!(side) {
        return r::fail("invalid Claude settings ownership path");
    }
    let source = PathBuf::from(
        row["source_root"]
            .as_str()
            .ok_or_else(|| r::Error(2, "invalid account source".into()))?,
    );
    if !source.join("rules/global-AGENTS.md").is_file()
        || !source.join("rust/Cargo.toml").is_file()
            && !source.join("scripts/install-claude-hooks.sh").is_file()
    {
        return r::fail("Claude account source unavailable; restore it for cleanup");
    }
    let mut ownership = load(&side, Value::Null)?;
    let source = if ownership.is_null() {
        source
    } else {
        let root = PathBuf::from(
            ownership["source_root"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid account hook source ownership".into()))?,
        );
        if !root.is_absolute()
            || !root.join("rules/global-AGENTS.md").is_file()
            || !root.join("rust/Cargo.toml").is_file()
                && !root.join("scripts/install-claude-hooks.sh").is_file()
        {
            return r::fail("unverified account hook source ownership");
        }
        root
    };

    if !ownership.is_null() {
        if ownership["schema"] != 1
            || ownership["settings"] != json!(settings)
            || ownership["home_identity"] != identity(directory)?
            || ownership["status"] == "pending"
        {
            return r::fail("ambiguous Claude settings ownership; sidecar retained");
        }
        let before = if r::exists(&settings) {
            r::read(&settings, r::LIMIT)?
        } else {
            Vec::new()
        };
        if !before.is_empty() && ownership["settings_identity"] != identity(&settings)? {
            return r::fail("Claude account settings were replaced; ownership retained");
        }
        let mut after = if before.is_empty() {
            json!({})
        } else {
            r::parse(&before)?
        };
        if !after.is_object() {
            return r::fail("invalid account settings");
        }
        if after.get("hooks").is_some_and(|v| !v.is_object())
            || after["hooks"]
                .as_object()
                .is_some_and(|m| m.values().any(|v| !v.is_array()))
        {
            return r::fail("account hook structure is malformed");
        }
        for leaf in ownership["leaves"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid account leaf ownership".into()))?
        {
            let parts = crate::tools::strings(&leaf["path"])?;
            let allowed = match parts
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["modelSettings", "claude-opus-5-5", "effortLevel"] => leaf["value"] == "high",
                ["attribution"] => leaf["value"] == json!({"commit":"","pr":"","sessionUrl":false}),
                ["statusLine"] | ["subagentStatusLine"] => {
                    let script = if parts[0] == "statusLine" {
                        "claude-statusline.py"
                    } else {
                        "claude-subagent-statusline.py"
                    };
                    leaf["value"]["type"] == "command"
                        && leaf["value"]["command"]
                            .as_str()
                            .and_then(crate::integrations::command_words)
                            .is_some_and(|words| match words.as_slice() {
                                [program, path] => {
                                    (Path::new(program) == paths.home.join(".local/bin/oms")
                                        && path == script.trim_end_matches(".py"))
                                        || program == "python3"
                                            && Path::new(path)
                                                == source.join("scripts").join(script)
                                }
                                [bash, wrapper, program, path] => {
                                    bash == "bash"
                                        && program == "python3"
                                        && Path::new(wrapper)
                                            == source.join("scripts/lib/python-entry.sh")
                                        && Path::new(path) == source.join("scripts").join(script)
                                }
                                _ => false,
                            })
                }
                _ => false,
            };
            if !allowed {
                return r::fail("unverified account setting ownership");
            }
            let Some(cursor) = parent_mut(&mut after, &parts) else {
                continue;
            };
            if cursor.get(parts.last().unwrap()) == Some(&leaf["value"]) {
                cursor
                    .as_object_mut()
                    .unwrap()
                    .remove(parts.last().unwrap());
            }
        }
        for hook in ownership["hooks"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid account hook ownership".into()))?
        {
            let event = hook["event"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid account hook event".into()))?;
            let wanted = &hook["entry"];
            let item = &wanted["hooks"][0];
            let command = item["command"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid account hook command".into()))?;
            let mut source_paths = paths.clone();
            source_paths.source = source.clone();
            if !crate::integrations::owned_hook(
                command,
                &source_paths,
                &crate::integrations::expected(&source_paths)?,
            )? {
                return r::fail("unverified account hook ownership");
            }
            if let Some(groups) = after["hooks"][event].as_array_mut() {
                let count = groups
                    .iter()
                    .filter(|g| g["matcher"] == wanted["matcher"])
                    .map(|g| {
                        g["hooks"]
                            .as_array()
                            .map(|a| a.iter().filter(|h| *h == item).count())
                            .unwrap_or(0)
                    })
                    .sum::<usize>();
                if count == 1 {
                    for group in groups
                        .iter_mut()
                        .filter(|g| g["matcher"] == wanted["matcher"])
                    {
                        if let Some(hooks) = group["hooks"].as_array_mut() {
                            hooks.retain(|h| h != item);
                        }
                    }
                    groups.retain(|g| {
                        !g["hooks"].as_array().is_some_and(|h| h.is_empty())
                            || g.as_object().is_some_and(|o| o.len() > 2)
                    });
                }
            }
        }
        if let Some(hooks) = after["hooks"].as_object_mut() {
            hooks.retain(|_, v| !v.as_array().is_some_and(|a| a.is_empty()));
        }
        for container in ownership["containers"]
            .as_array()
            .ok_or_else(|| r::Error(2, "invalid account container ownership".into()))?
            .iter()
            .rev()
        {
            let parts = crate::tools::strings(container)?;
            if !matches!(
                parts
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
                ["hooks"] | ["modelSettings"] | ["modelSettings", "claude-opus-5-5"]
            ) {
                return r::fail("unverified account container ownership");
            }
            let Some(cursor) = parent_mut(&mut after, &parts) else {
                continue;
            };
            if cursor
                .get(parts.last().unwrap())
                .and_then(Value::as_object)
                .is_some_and(|m| m.is_empty())
            {
                cursor
                    .as_object_mut()
                    .unwrap()
                    .remove(parts.last().unwrap());
            }
        }
        ownership["leaves"] = json!([]);
        ownership["hooks"] = json!([]);
        ownership["containers"] = json!([]);
        ownership["status"] = json!("removed");
        return Ok(Some(Settings {
            path: settings.clone(),
            before,
            after,
            side: side.clone(),
            ownership,
            parent: identity(directory)?,
            previous_identity: if r::exists(&settings) {
                Some(identity(&settings)?)
            } else {
                None
            },
            side_before: Some(r::read(&side, r::LIMIT)?),
        }));
    }
    Ok(None)
}

pub(crate) fn reservation(paths: &Paths, options: &crate::cli::Options) -> Result<Value> {
    let settings = options
        .path("--settings", &paths.cwd)?
        .unwrap_or(paths.claude_settings()?);
    let side = options
        .path("--ownership-receipt", &paths.cwd)?
        .ok_or_else(|| r::Error(2, "account hook ownership path required".into()))?;
    if settings.file_name().is_none_or(|s| s != "settings.json")
        || side != settings.parent().unwrap().join(".oms-claude-hooks.json")
    {
        return r::fail("account hook receipt must be beside its settings");
    }
    let effects = load(
        &paths.config.join("provider-account-effects.json"),
        json!({}),
    )?;
    if effects["schema"] != 1 {
        return r::fail("account hooks require a valid reservation registry");
    }
    let rows = effects["accounts"]
        .as_array()
        .ok_or_else(|| r::Error(2, "invalid reservation registry".into()))?;
    let matches = rows
        .iter()
        .filter(|row| {
            row["provider"] == "claude"
                && row["settings"] == json!(settings)
                && row["hooks_receipt"] == json!(side)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return r::fail("account hooks require exactly one validated Claude reservation");
    }
    let row = matches[0];
    home(row, &row["identity"])?;
    Ok(row.clone())
}
pub(crate) fn hooks(
    paths: &Paths,
    options: &crate::cli::Options,
    tx: &mut Transaction,
) -> Result<()> {
    let row = reservation(paths, options)?;
    let directory = home(&row, &row["identity"])?;
    let settings = directory.join("settings.json");
    let side = directory.join(".oms-claude-hooks.json");
    let prepared = settings_plan(paths, &row, &directory)?;
    if options.has("--remove") {
        if let Some(settings) = prepared {
            return apply(
                Plan {
                    settings: vec![settings],
                    links: Vec::new(),
                    writes: Vec::new(),
                },
                tx,
            );
        }
        return Ok(());
    }
    let before = if r::exists(&settings) {
        r::read(&settings, r::LIMIT)?
    } else {
        Vec::new()
    };
    let mut updated = if let Some(plan) = prepared {
        plan.after
    } else if before.is_empty() {
        json!({})
    } else {
        r::parse(&before)?
    };
    if !updated.is_object() {
        return r::fail("account settings must be an object");
    }
    let manifest = crate::integrations::expected(paths)?;
    let mut leaves = Vec::new();
    let mut owned_hooks = Vec::new();
    let mut containers = std::collections::BTreeSet::<Vec<String>>::new();
    let command = |verb: &str| crate::integrations::native_command(paths, verb);
    for (parts, value) in [
        (
            vec!["statusLine"],
            json!({"type":"command","command":command("claude-statusline")?}),
        ),
        (
            vec!["subagentStatusLine"],
            json!({"type":"command","command":command("claude-subagent-statusline")?}),
        ),
        (
            vec!["modelSettings", "claude-opus-5-5", "effortLevel"],
            json!("high"),
        ),
        (
            vec!["attribution"],
            json!({"commit":"","pr":"","sessionUrl":false}),
        ),
    ] {
        let parts = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let mut cursor = &mut updated;
        let mut prefix = Vec::new();
        let mut valid = true;
        for part in &parts[..parts.len() - 1] {
            let Some(map) = cursor.as_object_mut() else {
                valid = false;
                break;
            };
            prefix.push(part.clone());
            if !map.contains_key(part) {
                map.insert(part.clone(), json!({}));
                containers.insert(prefix.clone());
            }
            cursor = map.get_mut(part).unwrap();
        }
        if valid
            && let Some(parent) = parent_mut(&mut updated, &parts).and_then(Value::as_object_mut)
        {
            let last = parts.last().unwrap();
            if !parent.contains_key(last) {
                parent.insert(last.clone(), value.clone());
                leaves.push(json!({"path":parts,"value":value}));
            }
        }
    }
    if updated.get("hooks").is_none() {
        updated["hooks"] = json!({});
        containers.insert(vec!["hooks".into()]);
    }
    let hooks = updated["hooks"]
        .as_object_mut()
        .ok_or_else(|| r::Error(2, "account hooks must be an object".into()))?;
    for surface in manifest["surfaces"].as_array().unwrap() {
        let event = surface["event"].as_str().unwrap();
        let mut hook = json!({"type":"command","command":surface["command"]});
        if !surface["timeout"].is_null() {
            hook["timeout"] = surface["timeout"].clone();
        }
        let mut entry = json!({"hooks":[hook]});
        if !surface["matcher"].is_null() {
            entry["matcher"] = surface["matcher"].clone();
        }
        let entries = hooks
            .entry(event)
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or_else(|| r::Error(2, "account hook events must be lists".into()))?;
        if entries.iter().any(|e| {
            e["matcher"] == entry["matcher"]
                && e["hooks"]
                    .as_array()
                    .is_some_and(|a| a.contains(&entry["hooks"][0]))
        }) {
            continue;
        }
        entries.push(entry.clone());
        owned_hooks.push(json!({"event":event,"entry":entry}));
    }
    let ownership = json!({"schema":1,"settings":settings,"home_identity":identity(&directory)?,"source_root":paths.source,"hooks_schema":manifest["hooks_schema"],"status":"installed","settings_identity":Value::Null,"settings_digest":Value::Null,"leaves":leaves,"hooks":owned_hooks,"containers":containers});
    apply(
        Plan {
            settings: vec![Settings {
                path: settings.clone(),
                before,
                after: updated,
                side: side.clone(),
                ownership,
                parent: identity(&directory)?,
                previous_identity: if r::exists(&settings) {
                    Some(identity(&settings)?)
                } else {
                    None
                },
                side_before: if r::exists(&side) {
                    Some(r::read(&side, r::LIMIT)?)
                } else {
                    None
                },
            }],
            links: Vec::new(),
            writes: Vec::new(),
        },
        tx,
    )
}
