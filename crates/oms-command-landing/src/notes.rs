//! Deterministic, scanned release-note skeletons. Never creates Git objects.
use crate::{Context, Output, Result, V, cli::Options, git::Git, require, safe, storage, string};
use std::{collections::BTreeMap, path::Path};
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(o.words == ["notes"], "unexpected land notes argument")?;
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    let start = git.commit(o.get("from"))?;
    let end = git.commit(o.get("to"))?;
    require(
        git.succeeds(&["merge-base", "--is-ancestor", &start, &end])?,
        "--from is not an ancestor of --to; a notes range runs forward along one history",
    )?;
    let raw = git.text(&[
        "log",
        "--first-parent",
        "--reverse",
        "--format=%H%x09%s",
        &format!("{start}..{end}"),
    ])?;
    let commits: Vec<_> = raw.lines().filter_map(|l| l.split_once('\t')).collect();
    let mut states = BTreeMap::new();
    if let Ok(dir) = crate::delivery::state_directory(&git)
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for e in entries.flatten() {
            if e.path().extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            if let Ok(raw) = storage::read_external(&e.path(), false)
                && let Ok(row) = V::parse(&raw)
            {
                let sha = string(&row, "sha");
                if states.get(&sha).is_none_or(|s| s != "passed") {
                    states.insert(sha, string(&row, "state"));
                }
            }
        }
    }
    let conventional = regex::Regex::new(r"^([a-z]+)(\([A-Za-z0-9._/-]+\))?(!)?: (.+)$").unwrap();
    let order = [
        "feat", "fix", "perf", "refactor", "docs", "test", "build", "ci", "chore", "revert",
        "other",
    ];
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut breaking = vec![];
    for (sha, subject) in commits.iter().take(500) {
        let cap = conventional.captures(subject);
        let kind = cap
            .as_ref()
            .and_then(|m| m.get(1))
            .map(|m| m.as_str())
            .filter(|s| order.contains(s))
            .unwrap_or("other");
        let text = if kind != "other" {
            cap.as_ref().unwrap().get(4).unwrap().as_str()
        } else {
            subject
        };
        let text = oms_contract_markdown::bounded_line(text, 200);
        let scope = cap
            .as_ref()
            .and_then(|m| m.get(2))
            .map(|m| format!("{}: ", &m.as_str()[1..m.len() - 1]))
            .unwrap_or_default();
        let mark = states
            .get(*sha)
            .map(|s| format!(" [land: {}]", oms_contract_markdown::bounded_line(s, 20)))
            .unwrap_or_default();
        let entry = format!("- {scope}{text} ({}){mark}", &sha[..sha.len().min(7)]);
        groups.entry(kind.into()).or_default().push(entry.clone());
        if cap.as_ref().is_some_and(|m| m.get(3).is_some()) {
            breaking.push(entry);
        }
    }
    let mut out = format!(
        "# Notes for {}..{}\n\n<!-- Skeleton from commit subjects and land receipts; a person writes the notes. Nothing was tagged, released or committed. -->\n\n{} commit(s) on the first-parent line{}.\nLand receipts on this machine: {} of these commits were the head of a passed land; a commit with no mark was not landed from here or landed inside a later one.\n",
        &start[..7],
        &end[..7],
        commits.len(),
        if commits.len() > 500 {
            "; only the first 500 are listed"
        } else {
            ""
        },
        commits
            .iter()
            .filter(|(sha, _)| states.get(*sha).is_some_and(|s| s == "passed"))
            .count()
    );
    if !breaking.is_empty() {
        out += &format!("\n## Breaking (marked `!`)\n{}\n", breaking.join("\n"));
    }
    for kind in order {
        if let Some(entries) = groups.get(kind) {
            out += &format!("\n## {kind}\n{}\n", entries.join("\n"));
        }
    }
    safe(out.as_bytes())?;
    if let Some(path) = o.path("output", c.cwd) {
        let parent = std::fs::canonicalize(
            path.parent()
                .ok_or_else(|| crate::Error::new(2, "output has no parent"))?,
        )?;
        let _lock = storage::lock(&path, c.interrupted)?;
        storage::create_exclusive(
            &parent,
            Path::new(
                path.file_name()
                    .ok_or_else(|| crate::Error::new(2, "output has no name"))?,
            ),
            out.as_bytes(),
            0o644,
            false,
        )?;
        Ok(Output::text(format!(
            "land notes: wrote {}\n",
            path.display()
        )))
    } else {
        Ok(Output::text(out))
    }
}
pub fn receipt_bytes(path: &Path) -> Result<Vec<u8>> {
    let raw = storage::read_external(path, false)?;
    require(
        raw.len() <= 1024 * 1024,
        "handoff receipt exceeds size limit",
    )?;
    Ok(raw)
}

/// Import exact producer bytes, publishing its recorded range boundary first.
/// This changes only remote-tracking refs and host receipts in the destination.
pub fn handoff(
    source: &Path,
    target: &Path,
    path: &Path,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let source = Git::root(source, interrupted)?;
    let target = Git::root(target, interrupted)?;
    let from = Git::new(&source, interrupted);
    let to = Git::new(&target, interrupted);
    from.transport_safe()?;
    to.transport_safe()?;
    let raw = receipt_bytes(path)?;
    let row = V::parse(&raw)?;
    require(
        string(&row, "state") == "passed" && row.get("schema") == &1usize.into(),
        "handoff requires a passed producer receipt",
    )?;
    require(
        row.get("gate").get("rc") == &0usize.into()
            && row.get("push").get("rc") == &0usize.into()
            && row.get("push").get("moved") != &0usize.into()
            && string(row.get("ci"), "conclusion") == "success",
        "handoff requires successful gate, moved push and exact-SHA CI",
    )?;
    let sha = string(&row, "sha");
    let remote = string(&row, "remote");
    let branch = string(&row, "target");
    let command = string(row.get("gate"), "command");
    require(
        crate::hex(&sha, 40) || crate::hex(&sha, 64),
        "invalid producer SHA",
    )?;
    require(
        !remote.is_empty() && !remote.starts_with('-'),
        "invalid producer remote",
    )?;
    let destination = from.text(&["remote", "get-url", "--push", &remote])?;
    let request = oms_command_plan::json::list([
        destination.as_str(),
        remote.as_str(),
        branch.as_str(),
        command.as_str(),
    ]);
    let stamp = format!(
        "{sha}-{}",
        &crate::digest(&oms_command_plan::json::default_bytes(&request, true)?)[..24]
    );
    require(
        string(&row, "request") == stamp
            && path.file_name().and_then(|s| s.to_str()) == Some(&format!("{stamp}.json")),
        "producer receipt destination or request changed",
    )?;
    let target_destination = to.text(&["remote", "get-url", &remote])?;
    let canonical = |repo: &Path, url: &str| -> Result<String> {
        if url.contains(':') {
            Ok(url.into())
        } else {
            Ok(std::fs::canonicalize(repo.join(url))?
                .to_string_lossy()
                .into_owned())
        }
    };
    require(
        canonical(&source, &destination)? == canonical(&target, &target_destination)?,
        "receipt handoff destination differs",
    )?;
    let reference = format!("refs/remotes/{remote}/{branch}");
    to.run(&["check-ref-format", &reference])?;
    to.run(&["check-ref-format", &format!("refs/heads/{branch}")])?;
    to.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        &remote,
        &format!("+refs/heads/{branch}:{reference}"),
    ])?;
    require(
        to.succeeds(&["merge-base", "--is-ancestor", &sha, &reference])?,
        "target remote-tracking ref does not contain the producer SHA",
    )?;
    let base = crate::helpers::recorded_base(&row)?
        .ok_or_else(|| crate::Error::new(2, "handoff requires an explicit recorded push range"))?;
    require(
        from.succeeds(&["merge-base", "--is-ancestor", base, &sha])?
            && to.succeeds(&["merge-base", "--is-ancestor", base, &sha])?,
        "handoff first-parent range cannot be proved",
    )?;
    let directory = crate::delivery::state_directory(&to)?;
    let mut transfers = vec![];
    for entry in std::fs::read_dir(
        path.parent()
            .ok_or_else(|| crate::Error::new(2, "receipt has no parent"))?,
    )?
    .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&format!("{base}-")) || !name.ends_with(".json") {
            continue;
        }
        let previous = receipt_bytes(&entry.path())?;
        let proof = V::parse(&previous)?;
        if string(&proof, "sha") == base && proof.get("push").get("rc") == &0usize.into() {
            transfers.push((name, previous));
        }
    }
    transfers.sort_by(|a, b| a.0.cmp(&b.0));
    transfers.push((format!("{stamp}.json"), raw));
    let _lock = storage::lock(&directory.join("handoff"), interrupted)?;
    for (name, bytes) in &transfers {
        let target = directory.join(name);
        let old = storage::read_external(&target, true)?;
        require(
            !target.try_exists()? || old == *bytes,
            "conflicting existing handoff receipt",
        )?;
    }
    for (name, bytes) in transfers {
        let target = directory.join(name);
        if target.try_exists()? {
            continue;
        }
        let mut ancestor = directory.as_path();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| crate::Error::new(2, "receipt directory has no ancestor"))?;
        }
        let physical = std::fs::canonicalize(ancestor)?;
        storage::create_exclusive(
            &physical,
            target
                .strip_prefix(ancestor)
                .map_err(|_| crate::Error::new(2, "invalid handoff path"))?,
            &bytes,
            0o600,
            true,
        )?;
    }
    Ok(())
}
