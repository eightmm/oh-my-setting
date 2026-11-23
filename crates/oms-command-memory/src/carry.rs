use crate::{
    cli::{Options, parent_only},
    store::{self, Store},
    workflows::*,
    *,
};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
fn find(s: &Store, id: &str) -> Result<records::Entry> {
    if !review_id(id) {
        return err("--id needs a memory event ID (see: oms agent-memory search)");
    }
    let mut matches: Vec<_> = s
        .active()?
        .into_iter()
        .filter(|e| e.event_id == id)
        .collect();
    if matches.len() != 1 {
        return err(format!(
            "no active shared note has the ID {id} (retired and superseded notes cannot be carried over)"
        ));
    }
    let e = matches.remove(0);
    if e.body.trim().is_empty() {
        return err(format!("the note {id} has no text"));
    }
    if e.body.trim().chars().count() > 2000 {
        return err(format!(
            "the note {id} is longer than 2000 characters; supersede it with a shorter one first"
        ));
    }
    Ok(e)
}
pub(crate) fn carry(s: &Store, o: &Options, author: &str, flag: &AtomicBool) -> Result<Output> {
    if !o.value("--text").is_empty() || !o.words.is_empty() || o.has("--all-repos") {
        return err(format!("{} takes --id, not a query", o.mode));
    }
    if o.mode == "promote" {
        if !o.has("--global") || !o.value("--to").is_empty() {
            return err(
                "promote copies a project note to global memory: promote --id EVENT --global [--apply]",
            );
        }
        parent_only("write global memory")?;
    } else {
        if o.has("--global") || o.has("--apply") {
            return err(
                "graduate only previews a diff: graduate --id EVENT --to AGENTS.md|PROJECT.md",
            );
        }
        if !["AGENTS.md", "PROJECT.md"].contains(&o.value("--to").as_str()) {
            return err("--to takes AGENTS.md or PROJECT.md");
        }
    }
    let e = find(s, &o.value("--id"))?;
    let body = store::normalize_paths(e.body.trim())
        .trim_end_matches('\n')
        .to_owned();
    if !store::clean(&body, true) {
        return Err(Error(3,if o.mode=="promote"{"the note still looks sensitive or machine-specific after its paths were normalized; supersede it with a portable wording first"}else{"the note looks sensitive or machine-specific; a checked-in document must not carry it"}.into()));
    }
    if o.mode == "promote" {
        let target = Store::new(s.repo.clone(), None, true);
        let marker = format!("(promoted from project note {})", e.event_id);
        let mentions = s
            .repo
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.chars().count() >= 3 && fold(&body).contains(&fold(n)));
        // The check and append share the canonical source lock, including retry.
        let _g = if o.has("--apply") {
            Some(store::lock(&target.shared, flag)?)
        } else {
            None
        };
        let already = String::from_utf8_lossy(&store::read(&target.shared)?).contains(&marker);
        let applied = o.has("--apply") && !already;
        if applied {
            target.append_held(author, &format!("{body}\n{marker}\n"), "promoted", flag)?;
        }
        drop(_g);
        if applied {
            target.after_write(flag);
        }
        let result = json!({"schema":1,"kind":"oms-memory-promote","id":e.event_id,"applied":applied,"already_promoted":already,"mentions_repository_name":mentions,"bytes":body.len()});
        if o.has("--json") {
            return Ok((0, json_line(&result)?, target.take_warnings()));
        }
        let mut out = String::new();
        if !already {
            out.push_str(&format!(
                "memory: {} {} ({} bytes):\n",
                if o.has("--apply") {
                    "promoting"
                } else {
                    "would promote"
                },
                e.event_id,
                body.len()
            ));
            for l in body.lines() {
                out.push_str(&format!("    {l}\n"));
            }
            if mentions {
                out.push_str("note: the text names this repository; global memory is read in every project\n");
            }
        }
        out.push_str(&if already{format!("memory: note {} is already in global memory; nothing to do\n",e.event_id)}else if !applied{"memory: dry run; nothing changed. Re-run with --apply to copy the note to global memory.\n".into()}else{format!("memory: promoted {} to global memory; the project note is unchanged\n",e.event_id)});
        return Ok((0, out.into_bytes(), target.take_warnings()));
    }
    let name = o.value("--to");
    const LONG: &str =
        "the note is longer than 400 characters on one line; supersede it with a shorter one first";
    let line = flat(&body);
    // In GitHub project mode the reviewed pull request owns the decision
    // record, so no local PROJECT.md is needed (the declaration may be tracked).
    if name == "PROJECT.md"
        && let Some(slug) = crate::github::mode(s, flag)
    {
        if line.chars().count() > 400 {
            return err(LONG);
        }
        let section = crate::github::decision_section(&line, &e.event_id);
        if o.has("--json") {
            return Ok((
                0,
                json_line(
                    &json!({"schema":1,"kind":"oms-memory-graduate","id":e.event_id,"to":name,"line":format!("- {line}"),"diff":[],"applied":false,"github_project":slug.to_string(),"pull_request_section":section}),
                )?,
                Vec::new(),
            ));
        }
        return Ok(output(format!(
            "{section}\nmemory: preview only; nothing was written. Put this section in the pull request that lands the change, then:\n    oms agent-memory retire --id {} --reason graduated",
            e.event_id
        )));
    }
    let path = s.repo.join(&name);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
        return err(format!(
            "{name} is not a regular file at the repository root"
        ));
    }
    let raw = store::read(&path)?;
    let raw = std::str::from_utf8(&raw)
        .map_err(|_| Error(2, format!("{name} could not be read as UTF-8")))?;
    let mut before: Vec<String> = raw.split('\n').map(str::to_owned).collect();
    if before.last().is_some_and(String::is_empty) {
        before.pop();
    }
    if line.chars().count() > 400 {
        return err(LONG);
    }
    let item = format!("- {line}");
    if before.contains(&item) {
        return Ok(output(format!(
            "memory: {name} already carries this line; retire the note with: oms agent-memory retire --id {} --reason graduated",
            e.event_id
        )));
    }
    let mut after = before.clone();
    if name == "PROJECT.md" {
        let re = regex::Regex::new(r"(?i)^##\s+(decisions|결정|의사결정)\s*$").unwrap();
        if let Some(start) = after.iter().position(|l| re.is_match(l.trim())) {
            let mut end = (start + 1..after.len())
                .find(|i| after[*i].starts_with("## "))
                .unwrap_or(after.len());
            while end > start + 1 && after[end - 1].trim().is_empty() {
                end -= 1;
            }
            after.insert(end, item.clone());
        } else {
            while after.last().is_some_and(|l| l.trim().is_empty()) {
                after.pop();
            }
            if !after.is_empty() {
                after.push(String::new());
            }
            after.extend(["## Decisions".into(), String::new(), item.clone()]);
        }
    } else {
        while after.last().is_some_and(|l| l.trim().is_empty()) {
            after.pop();
        }
        after.push(item.clone());
    }
    let diff = unified_diff(&before, &after, &name);
    if o.has("--json") {
        return Ok((
            0,
            json_line(
                &json!({"schema":1,"kind":"oms-memory-graduate","id":e.event_id,"to":name,"line":item,"diff":diff,"applied":false}),
            )?,
            Vec::new(),
        ));
    }
    Ok(output(format!(
        "{}\nmemory: preview only; {name} was not changed. Make the edit through your usual review path, then:\n    oms agent-memory retire --id {} --reason graduated",
        diff.join("\n"),
        e.event_id
    )))
}
// Graduation inserts one unique item and can trim trailing blanks. Its edit
// region fits one unified hunk, without invoking an external diff.
fn unified_diff(before: &[String], after: &[String], name: &str) -> Vec<String> {
    let prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let aend = before.len() - suffix;
    let bend = after.len() - suffix;
    let start = prefix.saturating_sub(2);
    let stop = (aend + 2).min(before.len());
    let astop = stop;
    let bstop = (bend + 2).min(after.len());
    fn range(start: usize, len: usize) -> String {
        match len {
            0 => format!("{start},0"),
            1 => format!("{}", start + 1),
            _ => format!("{},{len}", start + 1),
        }
    }
    let mut out = vec![
        format!("--- a/{name}"),
        format!("+++ b/{name}"),
        format!(
            "@@ -{} +{} @@",
            range(start, astop - start),
            range(start, bstop - start)
        ),
    ];
    out.extend(before[start..prefix].iter().map(|l| format!(" {l}")));
    out.extend(before[prefix..aend].iter().map(|l| format!("-{l}")));
    out.extend(after[prefix..bend].iter().map(|l| format!("+{l}")));
    out.extend(before[aend..astop].iter().map(|l| format!(" {l}")));
    out
}
pub(crate) fn recall_all(o: &Options, cwd: &Path, flag: &AtomicBool) -> Result<Output> {
    let limit = o.number("--limit", 5).map_err(|_| {
        Error(
            2,
            "--limit is 1 to 20 notes per repository with --all-repos".into(),
        )
    })?;
    if limit > 20 {
        return err("--limit is 1 to 20 notes per repository with --all-repos");
    }
    if !o.value("--id").is_empty() || !o.value("--to").is_empty() || o.has("--apply") {
        return err("--id/--to/--apply do not apply to recall --all-repos");
    }
    parent_only("read other repositories' memory")?;
    let query = if o.value("--text").is_empty() {
        o.words.join(" ")
    } else {
        o.value("--text")
    };
    if query.is_empty() {
        return err("recall --all-repos requires --text QUERY (or a positional query)");
    }
    let registry = std::env::var_os("OMS_TICK_REGISTRY")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| store::home().join(".config"))
                .join("oh-my-setting/tick-repos.txt")
        });
    if !registry.exists() {
        return err("no repository registry (register a repository with: oms tick register)");
    }
    let raw = store::read(&store::absolute(cwd, &registry))?;
    let mut seen = HashSet::new();
    let repos: Vec<_> = String::from_utf8_lossy(&raw)
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.trim().to_owned())
        .filter(|l| seen.insert(l.clone()))
        .collect();
    let (mut hits, mut skipped, mut unread) = (Vec::new(), 0, Vec::new());
    for repo in repos.iter().take(30) {
        check(flag)?;
        let s = Store::new(store::absolute(cwd, Path::new(repo)), None, false);
        if !std::fs::symlink_metadata(&s.database)
            .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        {
            skipped += 1;
            continue;
        }
        let result = (|| -> Result<Vec<records::Entry>> {
            let (n, p) = s.logs()?;
            let status = records::states(&n, &p);
            let db = db::connect(&s.database, true)?;
            let mut rows = db::ranked(&db, &query, "")?;
            rows.retain(|e| e.kind != "retirement" && !status.contains_key(&e.event_id));
            rows.truncate(limit);
            Ok(rows)
        })();
        let label = tilde(repo);
        let Ok(rows) = result else {
            unread.push(label);
            continue;
        };
        for e in rows {
            let mut row = json!({"repo":label,"event_id":e.event_id,"kind":e.kind,"occurred_at":e.occurred_at,"agent":e.agent,"body":e.body.trim()});
            hits.push(row.take());
        }
    }
    hits.truncate(50);
    for hit in &mut hits {
        if !store::clean(&text(hit, "body"), true) {
            hit["body"] = "".into();
            hit["withheld"] = true.into();
        }
    }
    let code = if hits.is_empty() { 1 } else { 0 };
    if o.has("--json") {
        return Ok((
            code,
            json_line(
                &json!({"schema":1,"kind":"oms-memory-recall-all","query":query,"hits":hits,"repositories":{"registered":repos.len(),"read":repos.len().min(30)-skipped-unread.len(),"without_index":skipped,"unread":unread,"not_reached":repos.len().saturating_sub(30)}}),
            )?,
            Vec::new(),
        ));
    }
    let mut out = String::new();
    let mut current = String::new();
    for h in &hits {
        let repo = text(h, "repo");
        if repo != current {
            out.push_str(&format!("{repo}\n"));
            current = repo;
        }
        out.push_str(&format!(
            "  {}  {}  {}\n",
            text(h, "occurred_at"),
            text(h, "event_id"),
            if h["withheld"] == true {
                "[withheld: sensitive-looking content]".into()
            } else {
                clip(text(h, "body").lines().next().unwrap_or(""), 200)
            }
        ));
    }
    let mut diagnostic = format!(
        "memory: {} recalled entr{} for \"{query}\" across {} registered repositor{} ({skipped} without an index{}); each index is read as last synchronized and nothing was written\n",
        hits.len(),
        if hits.len() == 1 { "y" } else { "ies" },
        repos.len(),
        if repos.len() == 1 { "y" } else { "ies" },
        if unread.is_empty() {
            String::new()
        } else {
            format!("; unread: {}", unread.join(", "))
        }
    );
    if repos.len() > 30 {
        diagnostic
            .push_str("memory: incomplete: only the first 30 registered repositories were read\n");
    }
    Ok((code, out.into_bytes(), diagnostic.into_bytes()))
}
fn tilde(path: &str) -> String {
    let home = store::home();
    Path::new(path)
        .strip_prefix(&home)
        .map(|p| {
            if p.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", os::diagnostic(p.as_os_str()))
            }
        })
        .unwrap_or_else(|_| path.into())
}
