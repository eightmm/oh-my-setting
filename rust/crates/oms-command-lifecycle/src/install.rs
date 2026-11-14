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
    let stamp = r::now()
        .replace(['-', ':'], "")
        .replace('T', "")
        .replace('Z', "");
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
fn targets(paths: &Paths, executable: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
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
    let old = paths.load()?;
    let payload = payload::verify(paths, flag)?;
    let pairs = targets(paths, &payload.executable)?;
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
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    let old = paths.load()?.or(old);
    let previous = old.as_ref().map(receipt::owner).transpose()?;
    if previous.as_ref().is_some_and(|p| p != &paths.source)
        && !previous
            .as_ref()
            .unwrap()
            .join("rules/global-AGENTS.md")
            .is_file()
    {
        return r::fail("previous canonical owner is unavailable; refusing ownership transfer");
    }
    let mut row = receipt::build(paths, old.as_ref(), flag)?;
    let mut tx = Transaction::begin(paths)?;
    payload::install(paths, &payload, &mut tx)?;
    for (source, target) in &pairs {
        r::check(flag)?;
        let prior = previous.as_ref().and_then(|p| {
            source
                .strip_prefix(&paths.source)
                .ok()
                .map(|rel| p.join(rel))
        });
        materialize(paths, &mut tx, source, target, prior.as_deref(), &mode)?;
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
            if let Some(source) = owned_skill_source(paths, &path)? {
                if owned(&source, &path)? {
                    restore(paths, &mut tx, &source, &path)?;
                }
            }
        }
    }
    row["native"] = json!({"revision":payload.revision,"libexec":paths.data.join("libexec"),"helper":payload.helper.is_some()});
    r::check(flag)?;
    tx.write(&paths.receipt, &r::bytes(&row)?, 0o600)?;
    tx.commit()?;
    Ok(r::text(format!(
        "install receipt: {}",
        paths.receipt.display()
    )))
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
pub(crate) fn unlink(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    paths.require_owner()?;
    let executable = payload::verify(paths, flag)
        .map(|p| p.executable)
        .unwrap_or_else(|_| paths.source.join("scripts/oms"));
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
    r::child_guard()?;
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    paths.require_owner()?;
    let mut tx = Transaction::begin(paths)?;
    for (source, target) in pairs {
        r::check(flag)?;
        restore(paths, &mut tx, &source, &target)?;
    }
    // Legacy interpreter trees contain symlinks; only remove a proven-owned
    // launcher here. Runtime directory cleanup is a separate explicit action.
    let shim = paths.home.join(".local/bin/python3");
    if shim.is_symlink() {
        let to = fs::read_link(&shim)?;
        if to.starts_with(paths.data.join("python-runtime")) {
            tx.remove(&shim)?;
        }
    }
    if r::exists(&paths.receipt) {
        tx.remove(&paths.receipt)?;
    }
    tx.commit()?;
    Ok(r::text("managed links removed; user backups restored"))
}
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.has("--purge") || options.has("--purge-dirty") {
        return r::fail(
            "checkout purge is outside the reversible native installer; unlink first and remove the checkout explicitly",
        );
    }
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
    if options.tool == "uninstall" || options.tool == "install" {
        return r::fail(
            "full native install/uninstall orchestration is not yet available; use the bounded link/unlink operations only after review; no installation state changed",
        );
    }
    r::fail("unsupported installer action")
}
