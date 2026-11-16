use crate::{
    cli::Options,
    payload,
    receipt::{self, Paths},
    runtime::{self as r, Result},
    transaction::{self, Transaction},
};
use serde_json::json;
use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) fn marker(target: &Path) -> PathBuf {
    target.with_file_name(format!(
        ".{}.oh-my-setting-managed.json",
        target.file_name().unwrap_or_default().to_string_lossy()
    ))
}
pub(crate) fn owned(source: &Path, target: &Path) -> Result<bool> {
    if !r::exists(target) {
        return Ok(false);
    }
    if fs::symlink_metadata(target)?.file_type().is_symlink() {
        let value = fs::read_link(target)?;
        let actual = if value.is_absolute() {
            value
        } else {
            target.parent().unwrap().join(value)
        };
        return Ok(r::physical(&actual)? == r::physical(source)?);
    }
    let record = match r::value(&marker(target)) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    if record["schema"] != 1 || !matches!(record["kind"].as_str(), Some("file" | "directory")) {
        return Ok(false);
    }
    let Some(recorded) = record["source"].as_str() else {
        return Ok(false);
    };
    Ok(r::physical(Path::new(recorded))? == r::physical(source)?
        && transaction::identity(target)?.as_deref() == record["sha256"].as_str())
}
fn copy_backup(tx: &mut Transaction, path: &Path, backup: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        tx.link(backup, &fs::read_link(path)?)
    } else {
        tx.copy(backup, path)
    }
}
pub(crate) fn backup(paths: &Paths, tx: &mut Transaction, target: &Path) -> Result<()> {
    if !r::exists(target) {
        return Ok(());
    }
    let stamp = r::now().replace(['-', ':'], "").replace(['T', 'Z'], "");
    let mut candidate = target.with_file_name(format!(
        "{}.backup.{}",
        target.file_name().unwrap().to_string_lossy(),
        stamp
    ));
    let mut n = 0u32;
    while r::exists(&candidate) {
        n = n
            .checked_add(1)
            .ok_or_else(|| r::Error(2, "backup namespace exhausted".into()))?;
        candidate = target.with_file_name(format!(
            "{}.backup.{}.{}",
            target.file_name().unwrap().to_string_lossy(),
            stamp,
            n
        ));
    }
    if !r::physical(target.parent().unwrap())?.starts_with(&paths.home) {
        return r::fail("refusing unmanaged takeover outside the user home");
    }
    copy_backup(tx, target, &candidate)
}
pub(crate) fn materialize(
    paths: &Paths,
    tx: &mut Transaction,
    source: &Path,
    target: &Path,
    previous: Option<&Path>,
    mode: &str,
) -> Result<()> {
    let ours = owned(source, target)?;
    let prior = if let Some(previous) = previous {
        owned(previous, target)?
    } else {
        false
    };
    if r::exists(target) && !ours && !prior {
        backup(paths, tx, target)?;
    }
    if mode == "copy" {
        tx.copy(target, source)?;
        let digest = transaction::identity(target)?
            .ok_or_else(|| r::Error(2, "managed copy disappeared".into()))?;
        let row = json!({"schema":1,"source":r::physical(source)?,"kind":if source.is_dir() {"directory"} else {"file"},"sha256":digest});
        tx.write(&marker(target), &r::bytes(&row)?, 0o600)?;
    } else {
        tx.link(target, source)?;
        if r::exists(&marker(target)) {
            if !ours && !prior {
                return r::fail("foreign managed marker blocks target installation");
            }
            tx.remove(&marker(target))?;
        }
    }
    Ok(())
}
pub(crate) fn skill_sources(paths: &Paths) -> Result<Vec<PathBuf>> {
    let manifest = r::value(&paths.source.join("skills.manifest.json"))?;
    let rows = manifest["skills"]
        .as_array()
        .ok_or_else(|| r::Error(2, "skills manifest has no skills array".into()))?;
    let mut sources = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        if row["enabled"] != true {
            continue;
        }
        let source = row["source"]
            .as_str()
            .ok_or_else(|| r::Error(2, "skill source is missing".into()))?;
        let relative = Path::new(source);
        if !source.starts_with("custom-skills/")
            || relative.components().count() != 2
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return r::fail("skill source is outside custom-skills");
        }
        let name = relative.file_name().unwrap().to_str().unwrap_or("");
        if !r::safe_name(name) || !seen.insert(name.to_owned()) {
            return r::fail("invalid or repeated skill source");
        }
        let path = paths.source.join(relative);
        if !r::physical(&path)?.starts_with(paths.source.join("custom-skills"))
            || !path.join("SKILL.md").is_file()
        {
            return r::fail("skill source is missing or escapes its owner");
        }
        if let Some(required) = row.get("requires") {
            let required = required
                .as_array()
                .ok_or_else(|| r::Error(2, "skill requires must be an array".into()))?;
            if required
                .iter()
                .any(|x| x.as_str().is_none_or(|s| r::find(s, &paths.cwd).is_none()))
            {
                continue;
            }
        }
        sources.push(path);
    }
    Ok(sources)
}
pub(crate) fn targets(paths: &Paths, executable: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut pairs = vec![
        (
            paths.source.join("rules/global-AGENTS.md"),
            paths.home.join(".codex/AGENTS.md"),
        ),
        (
            paths.source.join("rules/global-AGENTS.md"),
            paths.home.join(".claude/CLAUDE.md"),
        ),
        (
            paths.source.join("rules/global-AGENTS.md"),
            paths.home.join(".gemini/AGENTS.md"),
        ),
        (
            paths.source.join("output-styles/oms-korean.md"),
            paths.home.join(".claude/output-styles/oms-korean.md"),
        ),
        (executable.to_path_buf(), paths.home.join(".local/bin/oms")),
    ];
    for source in skill_sources(paths)? {
        for root in [".codex/skills", ".claude/skills", ".gemini/config/skills"] {
            pairs.push((
                source.clone(),
                paths.home.join(root).join(source.file_name().unwrap()),
            ));
        }
    }
    Ok(pairs)
}
pub(crate) fn link(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    paths.load()?;
    let payload = payload::verify(paths, flag)?;
    let pairs = targets(paths, &payload.frontdoor(paths))?;
    let mode = receipt::link_mode()?;
    if options.preview() {
        return Ok(r::text(
            pairs
                .iter()
                .map(|(s, t)| format!("would {mode} {} -> {}", t.display(), s.display()))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    r::child_guard()?;
    let _guard = r::receipt_lock(paths, flag)?;
    let old = paths.load()?;
    let mut tx = Transaction::begin(paths)?;
    let row = apply(paths, old.as_ref(), &payload, &mut tx, flag)?;
    tx.write(&paths.receipt, &r::bytes(&row)?, 0o600)?;
    tx.commit()?;
    Ok(r::text(format!(
        "install receipt: {}",
        paths.receipt.display()
    )))
}
pub(crate) fn apply(
    paths: &Paths,
    old: Option<&serde_json::Value>,
    payload: &payload::Payload,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<serde_json::Value> {
    let pairs = targets(paths, &payload.frontdoor(paths))?;
    let mode = receipt::link_mode()?;
    let previous = old.map(receipt::owner).transpose()?;
    if previous.as_ref().is_some_and(|p| p != &paths.source)
        && !previous
            .as_ref()
            .unwrap()
            .join("rules/global-AGENTS.md")
            .is_file()
    {
        return r::fail("previous canonical owner is unavailable; refusing ownership transfer");
    }
    let mut row = receipt::build(paths, old, flag)?;
    payload::install(paths, payload, tx)?;
    retire_interpreter_launcher(paths, tx)?;
    for (source, target) in &pairs {
        r::check(flag)?;
        let prior = if target == &paths.home.join(".local/bin/oms") {
            old.and_then(|r| r["native"]["frontdoor"].as_str())
                .map(PathBuf::from)
                .or_else(|| previous.as_ref().map(|p| p.join("scripts/oms")))
        } else {
            previous.as_ref().and_then(|p| {
                source
                    .strip_prefix(&paths.source)
                    .ok()
                    .map(|rel| p.join(rel))
            })
        };
        materialize(paths, tx, source, target, prior.as_deref(), &mode)?;
    }
    let enabled = pairs
        .iter()
        .map(|(_, t)| t.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for root in [
        ".codex/skills",
        ".claude/skills",
        ".gemini/config/skills",
        ".gemini/antigravity/skills",
    ] {
        let dir = paths.home.join(root);
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if enabled.contains(&path) {
                continue;
            }
            if let Some(source) = owned_skill_source(paths, &path)?
                && owned(&source, &path)?
            {
                restore(paths, tx, &source, &path)?;
            }
        }
    }
    row["native"] = json!({"revision":payload.revision,"libexec":paths.data.join("libexec"),"frontdoor":payload.frontdoor(paths),"helper":payload.helper.is_some()});
    r::check(flag)?;
    payload.recheck()?;
    Ok(row)
}
fn owned_skill_source(paths: &Paths, target: &Path) -> Result<Option<PathBuf>> {
    let source = if fs::symlink_metadata(target)?.file_type().is_symlink() {
        let p = fs::read_link(target)?;
        if p.is_absolute() {
            p
        } else {
            target.parent().unwrap().join(p)
        }
    } else {
        let Ok(row) = r::value(&marker(target)) else {
            return Ok(None);
        };
        let Some(s) = row["source"].as_str() else {
            return Ok(None);
        };
        PathBuf::from(s)
    };
    let source = r::physical(&source)?;
    Ok(source
        .starts_with(paths.source.join("custom-skills"))
        .then_some(source))
}
fn latest(target: &Path) -> Result<Option<PathBuf>> {
    let prefix = format!("{}.backup.", target.file_name().unwrap().to_string_lossy());
    let Some(parent) = target.parent().filter(|p| p.is_dir()) else {
        return Ok(None);
    };
    let mut entries = fs::read_dir(parent)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.retain(|e| {
        e.file_name().to_str().is_some_and(|s| {
            s.strip_prefix(&prefix).is_some_and(|tail| {
                !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            })
        })
    });
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.pop().map(|e| e.path()))
}
fn restore(_paths: &Paths, tx: &mut Transaction, source: &Path, target: &Path) -> Result<()> {
    if !owned(source, target)? {
        return Ok(());
    }
    tx.remove(target)?;
    if r::exists(&marker(target)) {
        tx.remove(&marker(target))?;
    }
    if let Some(backup) = latest(target)? {
        copy_backup(tx, &backup, target)?;
        tx.remove(&backup)?;
    }
    Ok(())
}
fn apply_unlink(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    tx: &mut Transaction,
) -> Result<crate::Output> {
    let row = paths.require_owner()?;
    let executable = row
        .as_ref()
        .and_then(|r| r["native"]["frontdoor"].as_str())
        .map(PathBuf::from)
        .or_else(|| payload::verify(paths, flag).ok().map(|p| p.executable))
        .unwrap_or_else(|| paths.source.join("scripts/oms"));
    let mut pairs = targets(paths, &executable)?;
    for target in [".codex/AGENTS.md", ".claude/CLAUDE.md", ".gemini/AGENTS.md"] {
        pairs.push((paths.source.join("AGENTS.md"), paths.home.join(target)));
    }
    pairs.push((
        paths.source.join("scripts/oms"),
        paths.home.join(".local/bin/oms"),
    ));
    pairs.push((
        paths.source.join("prompts"),
        paths.home.join(".oh-my-setting-prompts"),
    ));
    for root in [
        ".codex/skills",
        ".claude/skills",
        ".gemini/config/skills",
        ".gemini/antigravity/skills",
        ".pi/agent/skills",
    ] {
        let dir = paths.home.join(root);
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(source) = owned_skill_source(paths, &path)? {
                pairs.push((source, path));
            }
        }
    }
    if options.preview() {
        return Ok(r::text(
            pairs
                .iter()
                .map(|(_, t)| format!("would inspect and remove owned {}", t.display()))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    for (source, target) in pairs {
        r::check(flag)?;
        restore(paths, tx, &source, &target)?;
    }
    retire_interpreter_launcher(paths, tx)?;
    if r::exists(&paths.receipt) {
        tx.remove(&paths.receipt)?;
    }
    Ok(r::text("managed links removed; user backups restored"))
}
fn retire_interpreter_launcher(paths: &Paths, tx: &mut Transaction) -> Result<()> {
    // Remove only the interpreter link owned by the retired OMS runtime.
    let shim = paths.home.join(".local/bin/python3");
    if launcher_owned(&shim, &paths.data.join("python-runtime")) {
        tx.remove(&shim)?;
    }
    Ok(())
}
/// Ownership is judged on the launcher's resolved parent directory, never on
/// the final component: a dangling link inside the runtime is still the
/// runtime's, while a symlinked directory leading out of it is foreign. The
/// launcher must live inside the runtime directory, not be the runtime itself.
/// Anything that cannot be resolved (dangling parent, loop, permissions) is
/// not provably owned and is left alone.
fn launcher_owned(shim: &Path, runtime: &Path) -> bool {
    let Ok(to) = fs::read_link(shim) else {
        return false;
    };
    let to = if to.is_absolute() {
        to
    } else {
        match shim.parent() {
            Some(parent) => parent.join(to),
            None => return false,
        }
    };
    let Some(parent) = to.parent() else {
        return false;
    };
    // A runtime that is itself a symlink points at a tree this installation
    // does not own (for example the system prefix): never judge by its target.
    let Ok(runtime) = r::leaf(runtime) else {
        return false;
    };
    if fs::symlink_metadata(&runtime).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    r::physical(parent).is_ok_and(|parent| parent.starts_with(&runtime))
}
pub(crate) fn unlink(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    paths.require_owner()?;
    if options.preview() {
        return Ok(r::text(
            "would remove owned targets and restore their user backups",
        ));
    }
    r::child_guard()?;
    let _guard = r::receipt_lock(paths, flag)?;
    paths.require_owner()?;
    let mut tx = Transaction::begin(paths)?;
    let result = apply_unlink(paths, options, flag, &mut tx)?;
    r::check(flag)?;
    tx.commit()?;
    Ok(result)
}
pub(crate) fn run(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if options.tool == "install-skills" {
        return Ok(r::text(format!(
            "skills: {} enabled sources validated",
            skill_sources(paths)?.len()
        )));
    }
    if options.tool == "link" {
        return link(paths, options, flag);
    }
    if options.tool == "unlink" {
        return unlink(paths, options, flag);
    }
    if options.tool == "install" {
        return install(paths, options, flag, services);
    }
    if options.tool == "uninstall" {
        return uninstall(paths, options, flag);
    }
    r::fail("unsupported installer action")
}

fn component_options(options: &Options, tool: &str, remove: bool) -> Options {
    let mut options = options.clone();
    options.tool = tool.into();
    options.action.clear();
    if remove {
        options.flags.insert("--remove".into());
    } else {
        options.flags.remove("--remove");
    }
    options
}
fn install(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if !options.preview() {
        r::child_guard()?;
    }
    let old = paths.load()?;
    let mut row = receipt::build(paths, old.as_ref(), flag)?;
    for (option, key) in [
        ("--tools", "tools"),
        ("--auto-update", "auto_update"),
        ("--machine-snapshot", "machine_snapshot"),
        ("--slurm-snapshot", "slurm_snapshot"),
    ] {
        if options.has(option) || options.has("--full") {
            row["components"][key] = json!(true);
        }
    }
    if options.has("--no-auto-update") {
        row["components"]["auto_update"] = json!(false);
    }
    let extras = [
        "--connect-services",
        "--peer-permissions",
        "--star",
        "--notion-data-source",
    ]
    .iter()
    .any(|s| options.has(s));
    if extras && !options.preview() {
        let out = services.install_extras(
            crate::IncomingPhase::Preflight,
            &paths.source,
            &options.raw,
            flag,
        );
        if out.0 != 0 {
            return Err(r::Error(
                out.0,
                "native install extra preflight failed; installation unchanged".into(),
            ));
        }
    }
    for (key, tool) in [
        ("claude_hooks", "install-claude-hooks"),
        ("mcp", "install-mcp"),
        ("codex_plugin", "install-codex-plugin"),
        ("agy_plugin", "install-agy-plugin"),
    ] {
        let desired = row["components"][key] == true;
        if !options.preview()
            && (desired || old.as_ref().is_some_and(|r| r["components"][key] == true))
        {
            crate::integrations::preflight(
                paths,
                &component_options(options, tool, !desired),
                flag,
            )?;
        }
    }
    if let Some(reference) = options.get("--ref") {
        row["ref"] = json!(reference);
    }
    if let Some(profile) = options.get("--profile") {
        row["profile"] = json!(profile);
    }
    receipt::validate(&row)?;
    let plan = if row["components"]["tools"] == true {
        Some(crate::tools::plan(paths, options)?)
    } else {
        None
    };
    if options.preview() {
        return r::json(
            &json!({"schema":1,"install":row,"capabilities":plan,"actions":["stage native payload","converge owned targets and components","publish install receipt"]}),
        );
    }
    r::child_guard()?;
    let prepared = if row["components"]["claude_hooks"] == true {
        crate::integrations::prepare_hooks(paths, options, flag)?
    } else {
        options.clone()
    };
    let options = &prepared;
    let _guard = r::receipt_lock(paths, flag)?;
    let fresh = paths.load()?;
    if fresh != old {
        return Err(r::Error(
            75,
            "install receipt changed during planning; retry installation".into(),
        ));
    }
    let mut tx = Transaction::begin(paths)?;
    payload::stage(paths, &mut tx, flag)?;
    let payload = payload::verify(paths, flag)?;
    let native = apply(paths, fresh.as_ref(), &payload, &mut tx, flag)?;
    row["native"] = native["native"].clone();
    row["hooks_schema"] = crate::integrations::expected(paths)?["hooks_schema"].clone();
    for (key, tool) in [
        ("claude_hooks", "install-claude-hooks"),
        ("mcp", "install-mcp"),
        ("codex_plugin", "install-codex-plugin"),
        ("agy_plugin", "install-agy-plugin"),
    ] {
        let desired = row["components"][key] == true;
        if desired || old.as_ref().is_some_and(|r| r["components"][key] == true) {
            crate::integrations::apply(
                paths,
                &component_options(options, tool, !desired),
                &mut tx,
                flag,
            )?;
        }
    }
    crate::integrations::huds(
        paths,
        &mut tx,
        false,
        row["components"]["codex_plugin"] == true,
    )?;
    if row["components"]["auto_update"] == true {
        let mut scheduler = component_options(options, "install-autoupdate", false);
        if row["component_modes"]["auto_update"] == "check" {
            scheduler.flags.insert("--check-only".into());
        }
        crate::scheduler::apply(paths, &scheduler, &mut tx, flag)?;
    } else if old
        .as_ref()
        .is_some_and(|r| r["components"]["auto_update"] == true)
    {
        crate::scheduler::apply(
            paths,
            &component_options(options, "uninstall-autoupdate", true),
            &mut tx,
            flag,
        )?;
    }
    if row["components"]["machine_snapshot"] == true {
        tx.write(
            &crate::snapshot::output_path(paths)?,
            crate::snapshot::render(paths, flag)?.as_bytes(),
            0o600,
        )?;
    }
    if row["components"]["slurm_snapshot"] == true {
        crate::snapshot::slurm(paths, &mut tx, flag)?;
    }
    if plan.is_some() {
        let plan = crate::tools::apply(paths, options, &mut tx, flag)?;
        tx.write(
            &paths.config.join("capabilities.json"),
            &r::bytes(&plan)?,
            0o600,
        )?;
    }
    payload.recheck()?;
    r::check(flag)?;
    tx.write(&paths.receipt, &r::bytes(&row)?, 0o600)?;
    let diagnostics = tx.diagnostics();
    tx.commit()?;
    if extras {
        let out = services.install_extras(
            crate::IncomingPhase::Reconcile,
            &paths.source,
            &options.raw,
            flag,
        );
        if out.0 != 0 {
            return Err(r::Error(out.0,"core install succeeded; native extra producer failed and retains its recovery state".into()));
        }
    }
    let mut output = r::text(format!(
        "installed native OMS {}; receipt: {}",
        payload.revision,
        paths.receipt.display()
    ));
    output.2 = diagnostics;
    Ok(output)
}
fn uninstall(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if !options.preview() {
        r::child_guard()?;
    }
    paths.require_owner()?;
    if !options.preview()
        && !options.has("--yes")
        && !options.has("-y")
        && !std::env::var_os("OH_MY_SETTING_ASSUME_YES").is_some_and(|s| s == "1")
    {
        return Err(r::Error(
            1,
            "uninstall requires --yes in a noninteractive lifecycle invocation".into(),
        ));
    }
    let purge =
        options.has("--purge") || std::env::var_os("OH_MY_SETTING_PURGE").is_some_and(|s| s == "1");
    let dirty = options.has("--purge-dirty")
        || std::env::var_os("OH_MY_SETTING_PURGE_DIRTY").is_some_and(|s| s == "1");
    if dirty && !purge {
        return r::fail("purge-dirty requires purge");
    }
    if purge {
        if paths.source == Path::new("/")
            || paths.home.starts_with(&paths.source)
            || [&paths.config, &paths.state, &paths.data]
                .iter()
                .any(|p| p.starts_with(&paths.source))
        {
            return r::fail("refusing unsafe checkout purge root");
        }
        if !dirty
            && !r::git(
                &paths.source,
                &["status", "--porcelain", "--untracked-files=all"],
                flag,
            )?
            .is_empty()
        {
            return r::fail(
                "refusing to purge a dirty checkout; preserve changes or explicitly use purge-dirty",
            );
        }
    }
    if options.preview() {
        crate::account_cleanup::plan(paths)?;
        return Ok(r::text(if purge {
            "would withdraw owned integrations and purge the checkout"
        } else {
            "would withdraw owned integrations and keep the checkout"
        }));
    }
    let old = paths.require_owner()?;
    for (key, tool) in [
        ("codex_plugin", "install-codex-plugin"),
        ("agy_plugin", "install-agy-plugin"),
    ] {
        if old.as_ref().is_some_and(|r| r["components"][key] == true) {
            crate::plugins::preflight(paths, &component_options(options, tool, true), flag)?;
        }
    }
    {
        r::child_guard()?;
        let _operation = crate::lifecycle_lock::acquire(paths, flag)?;
        let _accounts = r::lock(&paths.config.join("provider-accounts.json"), &paths.cwd)?;
        let _guard = r::receipt_only(paths)?;
        let old = paths.require_owner()?;
        let mut tx = Transaction::begin(paths)?;
        let accounts = crate::account_cleanup::plan(paths)?;
        crate::account_cleanup::apply(accounts, &mut tx)?;

        for (key, tool) in [
            ("claude_hooks", "install-claude-hooks"),
            ("mcp", "install-mcp"),
            ("codex_plugin", "install-codex-plugin"),
            ("agy_plugin", "install-agy-plugin"),
        ] {
            if old.as_ref().is_some_and(|v| v["components"][key] == true) {
                crate::integrations::apply(
                    paths,
                    &component_options(options, tool, true),
                    &mut tx,
                    flag,
                )?;
            }
        }
        crate::integrations::huds(paths, &mut tx, true, false)?;
        if paths.source.join(".git").exists() {
            crate::integrations::apply(
                paths,
                &component_options(options, "install-hooks", true),
                &mut tx,
                flag,
            )?;
        }
        if old
            .as_ref()
            .is_some_and(|v| v["components"]["auto_update"] == true)
        {
            crate::scheduler::apply(
                paths,
                &component_options(options, "uninstall-autoupdate", true),
                &mut tx,
                flag,
            )?;
        }
        crate::scheduler::apply(
            paths,
            &component_options(options, "tick", true),
            &mut tx,
            flag,
        )?;
        crate::account_cleanup::permissions(paths, &mut tx)?;
        if purge {
            crate::account_cleanup::journal(paths, &mut tx)?;
        }
        let result = apply_unlink(paths, options, flag, &mut tx)?;
        let runtime = std::env::var_os("OMS_PYTHON_RUNTIME_ROOT")
            .map(PathBuf::from)
            .unwrap_or(paths.data.join("python-runtime"));
        if r::exists(&runtime) {
            let proof = runtime.join(".oh-my-setting-python-runtime");
            let expected = ["schema", "1", "owner", "oh-my-setting"];
            let lines = String::from_utf8(r::read(&proof, 4096)?)
                .map_err(|_| r::Error(2, "invalid interpreter ownership".into()))?;
            if !runtime.is_symlink()
                && !paths.home.starts_with(&runtime)
                && lines.lines().take(2).eq([
                    format!("{}{}{}", expected[0], '=', expected[1]),
                    format!("{}{}{}", expected[2], '=', expected[3]),
                ]
                .iter()
                .map(String::as_str))
            {
                tx.remove_external(&runtime)?;
            } else {
                return r::fail("interpreter ownership is ambiguous; directory retained");
            }
        }
        if purge {
            tx.remove_external(&paths.source)?;
        }
        r::check(flag)?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::launcher_owned;
    use std::{fs, os::unix::fs::symlink, path::PathBuf};

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn retired_launcher_ownership_is_judged_on_the_resolved_parent() {
        let dir = Dir(fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("oms-launcher-owned-{}", std::process::id())));
        let _ = fs::remove_dir_all(&dir.0);
        let (bin, runtime) = (
            dir.0.join("home/.local/bin"),
            dir.0.join("data/python-runtime"),
        );
        let (shim, foreign) = (bin.join("python3"), dir.0.join("foreign"));
        for d in [&bin, &runtime.join("bin"), &foreign] {
            fs::create_dir_all(d).unwrap();
        }
        let owned = |to: &str| {
            let _ = fs::remove_file(&shim);
            symlink(to, &shim).unwrap();
            launcher_owned(&shim, &runtime)
        };
        // Missing, dangling-in-runtime and relative launchers are the runtime's.
        assert!(owned(runtime.join("bin/python3").to_str().unwrap()));
        symlink("missing", runtime.join("bin/dangling")).unwrap();
        assert!(owned(runtime.join("bin/dangling").to_str().unwrap()));
        assert!(owned("../../../data/python-runtime/bin/python3"));
        // The runtime tree itself may be absent without failing the decision.
        fs::remove_dir_all(&runtime).unwrap();
        assert!(owned(runtime.join("bin/python3").to_str().unwrap()));
        // A symlinked parent directory leaving the runtime is foreign.
        fs::create_dir(&runtime).unwrap();
        symlink(&foreign, runtime.join("out")).unwrap();
        assert!(!owned(runtime.join("out/python3").to_str().unwrap()));
        // A dangling directory link in the path or runtime root is unprovable, not fatal.
        symlink("nowhere", runtime.join("gone")).unwrap();
        assert!(!owned(runtime.join("gone/python3").to_str().unwrap()));
        assert!(!owned(foreign.join("python3").to_str().unwrap()));
        assert!(!owned("foreign-runtime"));
        // The runtime itself is not a launcher inside it.
        assert!(!owned(runtime.to_str().unwrap()));
        assert!(!owned(".."));
        assert!(!owned("/"));
        // Not a symlink: never owned.
        fs::remove_file(&shim).unwrap();
        fs::write(&shim, b"x").unwrap();
        assert!(!launcher_owned(&shim, &runtime));
        // A live symlink runtime to a foreign tree owns nothing, even via its own path.
        fs::remove_file(&shim).unwrap();
        fs::remove_dir_all(&runtime).unwrap();
        fs::create_dir_all(foreign.join("bin")).unwrap();
        symlink(&foreign, &runtime).unwrap();
        for to in [
            foreign.join("python3"),
            foreign.join("bin/python3"),
            runtime.join("bin/python3"),
            runtime.clone(),
        ] {
            let _ = fs::remove_file(&shim);
            symlink(&to, &shim).unwrap();
            assert!(!launcher_owned(&shim, &runtime), "{}", to.display());
        }
        // Dangling runtime root.
        fs::remove_file(&runtime).unwrap();
        symlink("nowhere", &runtime).unwrap();
        assert!(!launcher_owned(&shim, &runtime));
    }
}
