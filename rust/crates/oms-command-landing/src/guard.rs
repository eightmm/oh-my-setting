//! Advisory edits guard. Scope matching is the shared bounded Bash-compatible
//! matcher, and commits since `begin` remain part of the changed path set.
use crate::{Context, Error, Output, Result, cli::Options, digest, git::Git, require, storage};
use oms_contract_markdown::scope;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
fn normalize(p: &str) -> Result<String> {
    scope::normalize(p).map_err(|e| Error::new(2, e.to_string()))
}
pub(crate) fn in_scope(path: &str, allowed: &[String], denied: &[String]) -> Result<bool> {
    let matches = |patterns: &[String]| {
        scope::matches_any(
            path,
            &patterns.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .map_err(|e| Error::new(2, e.to_string()))
    };
    Ok(!matches(denied)? && (allowed.is_empty() || matches(allowed)?))
}
fn fingerprint(repo: &Path, path: &str) -> Result<String> {
    match std::fs::symlink_metadata(repo.join(path)) {
        Ok(m) if m.is_file() => Ok(digest(&storage::read(repo, Path::new(path), false)?)),
        Ok(_) => Ok("<non-regular>".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("<missing>".into()),
        Err(e) => Err(e.into()),
    }
}
fn paths(raw: &[u8]) -> Result<BTreeSet<String>> {
    raw.split(|b| *b == 0)
        .filter(|p| !p.is_empty() && !p.starts_with(b".oms/"))
        .map(|p| {
            std::str::from_utf8(p)
                .map(String::from)
                .map_err(|_| Error::new(2, "changed path cannot be scope-checked (unquotable)"))
        })
        .collect()
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(
        o.words.len() == 1,
        "change-guard requires begin, check, end or status",
    )?;
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    let file = o
        .path("file", c.cwd)
        .unwrap_or_else(|| repo.join(".oms/guards/change-guard.tsv"));
    let _guard = storage::lock(&file, c.interrupted)?;
    let raw = storage::read_external(&file, true)?;
    let mut state: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for line in std::str::from_utf8(&raw)
        .map_err(|_| Error::new(2, "invalid guard state"))?
        .lines()
    {
        let mut fields = line.splitn(2, '\t');
        state
            .entry(fields.next().unwrap())
            .or_default()
            .push(fields.next().unwrap_or(""));
    }
    let first = |key| {
        state
            .get(key)
            .and_then(|v| v.first())
            .copied()
            .unwrap_or("")
    };
    match o.action("") {
        "begin" => {
            storage::ensure_ignore(&repo, c.interrupted)?;
            let mut allowed = o.all("allow");
            let mut denied = o.all("deny");
            if o.has("from-task") {
                let state_repo = c.state(&repo)?;
                let raw = storage::read(&state_repo, Path::new(".oms/agent-task.md"), true)?;
                let section = oms_command_task::packet::section(&raw, "Constraints");
                let parsed =
                    oms_contract_markdown::parse_scope(String::from_utf8_lossy(&section).lines());
                allowed.extend(parsed.allowed);
                denied.extend(parsed.forbidden);
            }
            let mut body = format!(
                "repo\t{}\nhead\t{}\nstarted\t{}\n",
                repo.display(),
                git.commit("HEAD").unwrap_or_else(|_| "no-head".into()),
                crate::timestamp()
            );
            if let Ok(pid) = std::env::var("OMS_GUARD_PID") {
                require(
                    pid.bytes().all(|b| b.is_ascii_digit())
                        && pid.parse::<u32>().is_ok_and(|v| v > 0),
                    "invalid guard owner pid",
                )?;
                body += &format!("pid\t{pid}\n");
            }
            for (kind, items) in [("allow", allowed), ("deny", denied)] {
                for p in items {
                    body += &format!("{kind}\t{}\n", normalize(&p)?);
                }
            }
            let mut dirty = paths(
                &git.run(&["diff", "--name-only", "-z", "HEAD", "--"])
                    .unwrap_or_default(),
            )?;
            dirty.extend(paths(&git.run(&[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ])?)?);
            for path in dirty {
                body += &format!("dirty\t{path}\t{}\n", fingerprint(&repo, &path)?);
            }
            let mut ancestor = file.parent().unwrap();
            while !ancestor.exists() {
                ancestor = ancestor.parent().unwrap();
            }
            storage::replace(
                &std::fs::canonicalize(ancestor)?,
                file.strip_prefix(ancestor).unwrap(),
                if file.exists() { Some(&raw) } else { None },
                body.as_bytes(),
            )?;
            Ok(Output::text(format!(
                "change-guard: snapshot {}\n",
                file.display()
            )))
        }
        "status" => {
            let mut tag = String::new();
            if !raw.is_empty() {
                if let Ok(pid) = first("pid").parse::<i64>() {
                    if !oms_command_operations::lock::pid_alive(pid) {
                        tag = format!(" STALE (owner pid {pid} dead; run: change-guard.sh end)");
                    }
                } else if let Ok(started) = first("started").parse::<u64>() {
                    let ttl = std::env::var("OMS_GUARD_TTL")
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(86400);
                    let age = crate::timestamp().saturating_sub(started);
                    if age > ttl {
                        tag = format!(
                            " STALE (began {}h ago > ttl {ttl}s; run: change-guard.sh end)",
                            age / 3600
                        );
                    }
                }
            }
            Ok(Output::text(format!(
                "change-guard: {} ({}){tag}\n",
                if raw.is_empty() { "inactive" } else { "active" },
                file.display()
            )))
        }
        "end" => {
            if file.exists() {
                require(
                    !std::fs::symlink_metadata(&file)?.file_type().is_symlink(),
                    "unsafe guard state",
                )?;
                std::fs::remove_file(&file)?;
            }
            Ok(Output::text("change-guard: ended\n"))
        }
        "check" => {
            require(
                !raw.is_empty(),
                format!("state file not found: {}", file.display()),
            )?;
            require(
                std::fs::canonicalize(first("repo"))? == repo,
                "guard belongs to another repository",
            )?;
            let allowed: Vec<String> = state
                .get("allow")
                .into_iter()
                .flatten()
                .map(|s| normalize(s))
                .collect::<Result<_>>()?;
            let denied: Vec<String> = state
                .get("deny")
                .into_iter()
                .flatten()
                .map(|s| normalize(s))
                .collect::<Result<_>>()?;
            let mut warnings = 0;
            let mut out = String::new();
            let begin = first("head");
            let head = git.commit("HEAD")?;
            if !begin.is_empty() && begin != "no-head" && begin != head {
                out += &format!(
                    "note: HEAD moved since guard begin ({begin} -> {head}); committed changes are included in the scope check\n"
                );
            }
            for row in state.get("dirty").into_iter().flatten() {
                let (p, old) = row
                    .split_once('\t')
                    .ok_or_else(|| Error::new(2, "invalid guard dirty record"))?;
                normalize(p)?;
                if fingerprint(&repo, p)? != old {
                    warnings += 1;
                    out += &format!(
                        "warning: pre-existing dirty file changed since guard begin: {p}\n"
                    );
                }
            }
            let mut changed = paths(&git.run(&["diff", "--name-only", "-z", "HEAD", "--"])?)?;
            changed.extend(paths(&git.run(&[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ])?)?);
            if !begin.is_empty() && begin != "no-head" {
                changed.extend(paths(&git.run(&[
                    "diff",
                    "--name-only",
                    "-z",
                    begin,
                    "HEAD",
                    "--",
                ])?)?);
            }
            for p in changed {
                if !in_scope(&p, &allowed, &denied)? {
                    warnings += 1;
                    let forbidden = scope::matches_any(
                        &p,
                        &denied.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                    .map_err(|e| Error::new(2, e.to_string()))?;
                    out += &format!(
                        "warning: changed path {}: {p}\n",
                        if forbidden {
                            "in forbidden scope"
                        } else {
                            "outside declared scope"
                        }
                    );
                }
            }
            out += &if warnings == 0 {
                "change-guard: ok\n".into()
            } else {
                format!("change-guard: {warnings} warning(s)\n")
            };
            let mut result = Output::text(out);
            result.code = i32::from(o.has("strict") && warnings > 0);
            Ok(result)
        }
        _ => Err(Error::new(2, "unknown change-guard action")),
    }
}
