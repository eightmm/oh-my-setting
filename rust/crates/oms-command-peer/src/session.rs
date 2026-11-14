use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, process, providers,
    safety, storage, text,
};
use std::os::unix::fs::MetadataExt;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
struct Source {
    provider: String,
    ident: String,
    path: PathBuf,
    root: PathBuf,
    repo: PathBuf,
    modified: u128,
}
#[derive(Clone)]
struct Message {
    line: usize,
    part: usize,
    pointer: String,
    role: String,
    text: String,
    hash: String,
}
fn plain(path: &Path, directory: bool) -> Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() {
            return fail("session path traverses a symlink");
        }
    }
    let meta = std::fs::symlink_metadata(path)?;
    if if directory {
        !meta.is_dir()
    } else {
        !meta.is_file() || meta.nlink() != 1 || meta.uid() != rustix::process::getuid().as_raw()
    } {
        return fail("unsafe or unavailable local session path");
    }
    Ok(())
}
fn roots(args: &Args) -> Result<Vec<(String, PathBuf, usize)>> {
    let selected = args.value("provider", "all");
    if !["all", "claude", "codex", "pi"].contains(&selected) {
        return fail("invalid native session provider");
    }
    let supplied = args.all("root");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(2, "native session home unavailable"))?;
    if !supplied.is_empty() {
        let mut values = Vec::new();
        for value in supplied {
            let (provider, raw) = if let Some((provider, path)) = value.split_once('=') {
                (provider.to_owned(), path.to_owned())
            } else if selected != "all" {
                (selected.into(), value)
            } else {
                return fail("with --provider all, use --root PROVIDER=PATH");
            };
            if !["claude", "codex", "pi"].contains(&provider.as_str())
                || selected != "all" && provider != selected
            {
                return fail("root provider conflicts with --provider");
            }
            let path = Path::new(&raw).canonicalize()?;
            if path.parent().is_none() || path == home.canonicalize()? {
                return fail("choose a bounded transcript directory");
            }
            plain(&path, true)?;
            values.push((provider, path, 4));
        }
        return Ok(values);
    }
    let codex = std::env::var_os("CODEX_HOME")
        .or_else(|| std::env::var_os("OMS_CODEX_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let pi = std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".pi/agent/sessions"));
    Ok(vec![
        ("claude".into(), claude.join("projects"), 1),
        ("codex".into(), codex.join("sessions"), 3),
        ("codex".into(), codex.join("archived_sessions"), 3),
        ("pi".into(), pi, 1),
    ]
    .into_iter()
    .filter(|(p, _, _)| selected == "all" || p == selected)
    .collect())
}
fn metadata(raw: &[u8], provider: &str) -> Option<(String, PathBuf)> {
    for line in raw.split(|b| *b == b'\n').take(32) {
        if line.len() > 65536 {
            break;
        }
        let Ok(row) = V::parse(line) else { continue };
        let meta = match provider {
            "codex" if text(&row, "type") == "session_meta" => row.get("payload"),
            "pi" if text(&row, "type") == "session" => &row,
            "claude" => &row,
            _ => continue,
        };
        let ident = text(
            meta,
            if provider == "claude" {
                "sessionId"
            } else {
                "id"
            },
        );
        let cwd = text(meta, "cwd");
        if !storage::identifier(ident)
            || cwd.len() > 4096
            || cwd.chars().any(char::is_control)
            || !Path::new(cwd).is_absolute()
        {
            continue;
        }
        if let Ok(path) = Path::new(cwd).canonicalize() {
            return Some((ident.into(), path));
        }
    }
    None
}
fn discover(repo: &Path, args: &Args, context: &Context<'_>) -> Result<(Vec<Source>, bool, usize)> {
    let mut found = Vec::new();
    let mut entries = 0usize;
    let mut files = 0usize;
    let mut seen = BTreeSet::new();
    for (provider, root, max_depth) in roots(args)? {
        if plain(&root, true).is_err() {
            continue;
        }
        let mut pending = vec![(root.clone(), 0)];
        while let Some((folder, depth)) = pending.pop() {
            crate::check(context.interrupted)?;
            for entry in std::fs::read_dir(folder)? {
                entries += 1;
                if entries > 8192 || files >= 4096 {
                    return Ok((found, true, entries));
                }
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() && depth < max_depth {
                    pending.push((entry.path(), depth + 1));
                } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl")
                {
                    files += 1;
                    let path = entry.path();
                    if !seen.insert((provider.clone(), path.clone()))
                        || plain(&path, false).is_err()
                    {
                        continue;
                    }
                    let meta = entry.metadata()?;
                    if meta.len() > 32 * 1024 * 1024 {
                        continue;
                    }
                    let bytes = storage::file(&path)?;
                    if let Some((ident, owner)) =
                        metadata(&bytes[..bytes.len().min(65536)], &provider)
                        && owner == repo
                    {
                        let modified = meta
                            .modified()?
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos();
                        found.push(Source {
                            provider: provider.clone(),
                            ident,
                            path,
                            root: root.clone(),
                            repo: repo.into(),
                            modified,
                        });
                    }
                }
            }
        }
    }
    found.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok((found, false, entries))
}
fn read(source: &Source) -> Result<Vec<u8>> {
    plain(&source.path, false)?;
    plain(&source.root, true)?;
    if !source.path.starts_with(&source.root) {
        return fail("transcript escaped its recorded root");
    }
    let bytes = storage::file(&source.path)?;
    if bytes.len() > 32 * 1024 * 1024
        || metadata(&bytes[..bytes.len().min(65536)], &source.provider)
            != Some((source.ident.clone(), source.repo.clone()))
    {
        return fail("transcript identity or repository binding changed");
    }
    Ok(bytes)
}
fn messages(source: &Source, raw: &[u8]) -> Vec<Message> {
    let mut result = Vec::new();
    let mut cwd = source.repo.clone();
    for (line, bytes) in raw.split(|b| *b == b'\n').enumerate() {
        if bytes.len() > 1024 * 1024 {
            continue;
        }
        let Ok(row) = V::parse(bytes) else { continue };
        if source.provider == "codex"
            && text(&row, "type") == "turn_context"
            && row.get("payload").has("cwd")
        {
            cwd = PathBuf::from(text(row.get("payload"), "cwd"));
        }
        let path = row
            .get("cwd")
            .text()
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone());
        if path.canonicalize().ok().as_deref() != Some(source.repo.as_path()) {
            continue;
        }
        let (message, prefix) = if source.provider == "codex" {
            if text(&row, "type") != "response_item"
                || text(row.get("payload"), "type") != "message"
            {
                continue;
            }
            (row.get("payload"), "/payload")
        } else {
            if !["user", "assistant", "message"].contains(&text(&row, "type")) {
                continue;
            }
            (row.get("message"), "/message")
        };
        let role = text(message, "role");
        if !["user", "assistant"].contains(&role) {
            continue;
        }
        if source.provider == "claude"
            && row
                .get("sessionId")
                .text()
                .is_some_and(|id| id != source.ident)
        {
            continue;
        }
        let content = message.get("content");
        if let Some(text) = content.text() {
            result.push(Message {
                line: line + 1,
                part: 0,
                pointer: format!("{prefix}/content"),
                role: role.into(),
                text: text.into(),
                hash: storage::hash(bytes),
            });
        } else {
            for (part, item) in content.array().iter().enumerate() {
                if ["text", "input_text", "output_text"].contains(&text(item, "type"))
                    && item.get("text").text().is_some()
                {
                    result.push(Message {
                        line: line + 1,
                        part,
                        pointer: format!("{prefix}/content/{part}/text"),
                        role: role.into(),
                        text: text(item, "text").into(),
                        hash: storage::hash(bytes),
                    });
                }
            }
        }
    }
    result
}
fn origin(source: &Source, message: &Message, start: usize, end: Option<usize>) -> V {
    let excerpt = message
        .text
        .chars()
        .skip(start)
        .take(400)
        .collect::<String>();
    let shown = safety::redact(excerpt.as_bytes())
        .unwrap_or_else(|_| b"[sensitive-content-omitted]".to_vec());
    let redacted = shown != excerpt.as_bytes();
    obj([
        ("provider", source.provider.clone().into()),
        ("session_id", source.ident.clone().into()),
        ("path", source.path.to_string_lossy().into_owned().into()),
        ("line", V::Num(message.line.to_string())),
        ("part", V::Num(message.part.to_string())),
        ("json_pointer", message.pointer.clone().into()),
        ("role", message.role.clone().into()),
        (
            "excerpt",
            String::from_utf8_lossy(&shown).into_owned().into(),
        ),
        ("excerpt_start", V::Num(start.to_string())),
        (
            "excerpt_end",
            V::Num(message.text.chars().count().min(start + 400).to_string()),
        ),
        ("match_end", end.map_or(V::Null, |n| V::Num(n.to_string()))),
        ("redacted", redacted.into()),
        ("line_sha256", message.hash.clone().into()),
    ])
}
pub(crate) fn advice_digest(repo: &Path, ident: &str, context: &Context<'_>) -> Result<String> {
    let caller = std::env::var("OMS_AGENT").unwrap_or_else(|_| "claude".into());
    let args = Args::parse(&["--provider".into(), caller.into()], &["provider"], &[])?;
    let (sources, truncated, _) = discover(repo, &args, context)?;
    if truncated {
        return fail("session discovery incomplete; pin a bounded source before advising");
    }
    let source = sources
        .into_iter()
        .find(|s| ident.is_empty() || s.ident == ident)
        .ok_or_else(|| Error::new(2, "calling session transcript absent"))?;
    let items = messages(&source, &read(&source)?);
    let mut selected = items
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .take(6)
        .collect::<Vec<_>>();
    selected.reverse();
    let mut body = String::new();
    for message in selected {
        body.push_str(&format!(
            "User:\n{}\n",
            safety::truncate(&message.text, 2000)
        ));
    }
    if let Some(last) = items.iter().rev().find(|m| m.role == "assistant") {
        body.push_str(&format!(
            "Last assistant summary:\n{}\n",
            safety::truncate(&last.text, 6000)
        ));
    }
    Ok(body)
}
fn select(repo: &Path, args: &Args, context: &Context<'_>) -> Result<Source> {
    let ident = args.required("session")?;
    let provider = args.required("provider")?;
    let (items, truncated, _) = discover(repo, args, context)?;
    if truncated {
        return fail("session discovery is incomplete; choose a narrower --root");
    }
    let matches = items
        .into_iter()
        .filter(|s| s.ident == ident && s.provider == provider)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return fail("native session is missing or ambiguous in this repository scope");
    }
    let source = matches.into_iter().next().unwrap();
    read(&source)?;
    Ok(source)
}
pub(crate) fn run(tool: &str, argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    if tool == "session-handoff" {
        return handoff(argv, context);
    }
    let args = Args::parse(
        argv,
        &[
            "repo",
            "provider",
            "session",
            "root",
            "limit",
            "line",
            "part",
            "name",
            "owner-pid",
            "label",
        ],
        &["json", "apply", "no-launch", "patch", "untracked-only"],
    )?;
    if args.has("help") {
        return Ok(out(b"Usage: oms session search QUERY | open | resume | fork | forks | launch ID | compare LEFT RIGHT | bookmark add|list|open|resume | restore files|conversation\nUse --provider claude|codex|pi --session ID --root PATH. Native launches and file restore require --apply.\n".to_vec()));
    }
    safety::parent()?;
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let limit = args.limit("limit", 20, 1, 100)?;
    let line = args.limit("line", 1, 1, usize::MAX)?;
    if args.action() == "search" {
        let query = args.operand(1);
        if query.is_empty() || query.chars().count() > 200 {
            return fail("query must contain 1..200 characters");
        }
        let (sources, mut truncated, entries) = discover(&repo, &args, context)?;
        let mut matches = Vec::new();
        let mut consumed = 0usize;
        for source in &sources {
            let raw = read(source)?;
            if consumed + raw.len() > 64 * 1024 * 1024 {
                truncated = true;
                break;
            }
            consumed += raw.len();
            for message in messages(source, &raw) {
                if let Some(at) = message.text.find(query) {
                    if matches.len() >= limit {
                        truncated = true;
                        break;
                    }
                    let at = message.text[..at].chars().count();
                    matches.push(origin(
                        source,
                        &message,
                        at,
                        Some(at + query.chars().count()),
                    ));
                }
            }
            if matches.len() >= limit {
                truncated = true;
                break;
            }
        }
        return Ok(out(json(&obj([
            ("schema", 1usize.into()),
            ("kind", "oms-session-search".into()),
            ("scope", repo.to_string_lossy().into_owned().into()),
            ("matches", V::Arr(matches)),
            (
                "scan",
                obj([
                    ("entries", V::Num(entries.to_string())),
                    ("matched_sessions", V::Num(sources.len().to_string())),
                    ("truncated", truncated.into()),
                    ("bytes", V::Num(consumed.to_string())),
                ]),
            ),
        ]))?));
    }
    if args.action() == "forks" {
        let mut rows = Vec::new();
        for name in storage::names(&repo, Path::new(".oms/sessions/forks"), ".json")? {
            rows.push(V::parse(&storage::read(
                &repo,
                &Path::new(".oms/sessions/forks").join(name),
                false,
            )?)?);
        }
        return Ok(out(json(&obj([
            ("schema", 1usize.into()),
            ("kind", "oms-session-forks".into()),
            ("forks", V::Arr(rows)),
        ]))?));
    }
    if args.action() == "bookmark" && args.operand(1) == "list" {
        let mut rows = Vec::new();
        for name in storage::names(&repo, Path::new(".oms/sessions/bookmarks"), ".json")? {
            rows.push(V::parse(&storage::read(
                &repo,
                &Path::new(".oms/sessions/bookmarks").join(name),
                false,
            )?)?);
        }
        return Ok(out(json(&obj([
            ("schema", 1usize.into()),
            ("kind", "oms-session-bookmarks".into()),
            ("bookmarks", V::Arr(rows)),
        ]))?));
    }
    if matches!(args.action(), "launch" | "compare")
        || args.action() == "restore" && args.operand(1) == "files"
    {
        return fail(
            "native checkpoint/fork ownership integration is required; state was preserved",
        );
    }
    let source = select(&repo, &args, context)?;
    let raw = read(&source)?;
    let items = messages(&source, &raw);
    if args.action() == "open" {
        let selected = items.iter().filter(|m| m.line >= line).collect::<Vec<_>>();
        return Ok(out(json(&obj([
            ("schema", 1usize.into()),
            ("kind", "oms-session-open".into()),
            ("provider", source.provider.clone().into()),
            ("session_id", source.ident.clone().into()),
            (
                "messages",
                V::Arr(
                    selected
                        .iter()
                        .take(limit)
                        .map(|m| origin(&source, m, 0, None))
                        .collect(),
                ),
            ),
            ("truncated", (selected.len() > limit).into()),
        ]))?));
    }
    if args.action() == "bookmark" {
        if args.operand(1) != "add" {
            return fail("bookmark reopen requires a bound stored source");
        }
        let part = args.limit("part", 0, 0, usize::MAX)?;
        let message = items
            .iter()
            .find(|m| m.line == line && m.part == part)
            .ok_or_else(|| Error::new(2, "no native text at the exact line/part"))?;
        let ident = crate::id("bookmark-")?;
        let mut row = origin(&source, message, 0, None);
        row.set("id", ident.clone());
        row.set("source_sha256", storage::hash(&raw));
        row.set("label", args.get("label"));
        row.set("created_at", now());
        storage::write(
            &repo,
            &Path::new(".oms/sessions/bookmarks").join(format!("{ident}.json")),
            json(&row)?,
            false,
            context.interrupted,
        )?;
        return Ok(out(json(&row)?));
    }
    let fork =
        args.action() == "fork" || args.action() == "restore" && args.operand(1) == "conversation";
    if args.action() != "resume" && !fork {
        return fail("unknown session action");
    }
    let binary = providers::binary(&source.provider);
    let help = process::invoke(
        context,
        &repo,
        vec![binary.clone().into(), "--help".into()],
        None,
        5,
        false,
    )?;
    if help.code != 0 || !help.complete {
        return fail("native provider help probe failed");
    }
    let help = String::from_utf8_lossy(&help.stdout);
    let mut native = vec![OsString::from(&binary)];
    match source.provider.as_str() {
        "codex" => {
            let action = if fork { "fork" } else { "resume" };
            if !help
                .lines()
                .any(|l| l.trim_start().starts_with(&format!("{action} ")))
            {
                return fail("native Codex does not advertise the requested action");
            }
            let supported = process::invoke(
                context,
                &repo,
                vec![binary.into(), action.into(), "--help".into()],
                None,
                5,
                false,
            )?;
            if !String::from_utf8_lossy(&supported.stdout).contains("--cd") {
                return fail("native Codex does not advertise worktree-bound --cd");
            }
            native.extend([
                action.into(),
                source.ident.clone().into(),
                "--cd".into(),
                repo.as_os_str().into(),
            ]);
        }
        "claude" => {
            if !help.contains("--resume") || fork && !help.contains("--fork-session") {
                return fail("native Claude does not advertise required resume/fork flags");
            }
            native.extend(["--resume".into(), source.ident.clone().into()]);
            if fork {
                native.push("--fork-session".into());
            }
        }
        "pi" => {
            let flag = if fork { "--fork" } else { "--session" };
            if !help.contains(flag) {
                return fail("native Pi does not advertise required flag");
            }
            native.extend([flag.into(), source.path.as_os_str().into()]);
        }
        _ => return fail("unsupported native provider"),
    }
    if args.action() == "fork" {
        return fail(
            "file-state checkpoint and fork ownership registration are not integrated; no launch occurred",
        );
    }
    let mut report = obj([
        ("schema", 1usize.into()),
        ("kind", "oms-session-native".into()),
        (
            "argv",
            V::Arr(
                native
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned().into())
                    .collect(),
            ),
        ),
        ("cwd", repo.to_string_lossy().into_owned().into()),
        ("provider", source.provider.into()),
        ("source_session", source.ident.into()),
        ("action", if fork { "fork" } else { "resume" }.into()),
        ("capability", "installed CLI --help".into()),
        ("dry_run", (!args.has("apply")).into()),
    ]);
    if args.has("apply") {
        let remove_environment = std::env::vars_os()
            .filter_map(|(k, _)| {
                k.to_str()
                    .is_some_and(|s| s.starts_with("OMS_") || s.starts_with("GIT_"))
                    .then_some(k)
            })
            .collect();
        let result = context.executor.execute(
            process::Invocation {
                argv: native,
                cwd: repo,
                input: None,
                environment: Vec::new(),
                remove_environment,
                timeout: Duration::from_secs(86400),
                max_bytes: 1024 * 1024,
                merge: false,
            },
            context.interrupted,
        )?;
        report.set("native_returncode", V::Num(result.code.to_string()));
        return Ok((result.code, json(&report)?, Vec::new()));
    }
    Ok(out(json(&report)?))
}
fn handoff(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "agent",
            "session",
            "cwd",
            "out",
            "note",
            "min-user-turns",
            "missing-session-policy",
            "limit",
            "max-bytes",
            "output",
        ],
        &["json", "status", "allow-sensitive"],
    )?;
    if args.has("help") {
        return Ok(out(include_bytes!("help/session-handoff.txt").to_vec()));
    }
    let repo = storage::repository(context, &args.path("cwd", context.cwd))?;
    let directory = Path::new(".oms/handoffs");
    match args.action() {
        "show" => {
            let raw = storage::file(&context.cwd.join(args.operand(1)))?;
            Ok(out(raw))
        }
        "list" => {
            let mut names = storage::names(&repo, directory, ".md")?;
            names.sort_by_key(|name| {
                std::cmp::Reverse(
                    std::fs::symlink_metadata(repo.join(directory).join(name))
                        .and_then(|m| m.modified())
                        .ok(),
                )
            });
            let total = names.len();
            let limit = args.limit("limit", usize::MAX, 0, usize::MAX)?;
            names.truncate(limit);
            let ledger =
                storage::read(&repo, &directory.join("consumed.jsonl"), true).unwrap_or_default();
            let start = ledger.len().saturating_sub(1024 * 1024);
            let mut pickups =
                std::collections::BTreeMap::<String, (String, String, BTreeSet<String>)>::new();
            for (i, line) in ledger[start..].split(|b| *b == b'\n').enumerate() {
                if start > 0 && i == 0 {
                    continue;
                }
                let Ok(row) = V::parse(line) else {
                    continue;
                };
                let (Some(name), Some(session), Some(stamp)) = (
                    row.get("handoff").text(),
                    row.get("session").text(),
                    row.get("ts").text(),
                ) else {
                    continue;
                };
                let entry = pickups
                    .entry(name.into())
                    .or_insert_with(|| (session.into(), stamp.into(), BTreeSet::new()));
                entry.2.insert(session.into());
            }
            let mut values = Vec::new();
            let mut listing = String::new();
            for name in &names {
                let info = std::fs::symlink_metadata(repo.join(directory).join(name))?;
                if !info.is_file() {
                    return fail("handoff catalog contains a non-regular file");
                }
                let first = pickups.get(name);
                let path = repo
                    .join(directory)
                    .join(name)
                    .to_string_lossy()
                    .into_owned();
                values.push(obj([
                    ("path", path.clone().into()),
                    ("bytes", V::Num(info.len().to_string())),
                    (
                        "mtime",
                        V::Num(
                            info.modified()?
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                .to_string(),
                        ),
                    ),
                    ("picked_up", first.is_some().into()),
                    (
                        "picked_up_by",
                        first.map_or(V::Null, |f| f.0.clone().into()),
                    ),
                    (
                        "picked_up_at",
                        first.map_or(V::Null, |f| f.1.clone().into()),
                    ),
                    ("pickups", first.map_or(0usize, |f| f.2.len()).into()),
                ]));
                let prefix = if args.has("status") {
                    first.map_or_else(
                        || "unclaimed ".into(),
                        |f| format!("picked-up {} {} ", f.0, f.1),
                    )
                } else {
                    String::new()
                };
                listing.push_str(&format!("{prefix}{path}\n"));
            }
            let mut report = obj([("schema", 1usize.into()), ("handoffs", V::Arr(values))]);
            if args.has("limit") {
                report.set("omitted", total - names.len());
            }
            Ok(out(if args.has("json") {
                json(&report)?
            } else {
                if listing.is_empty() {
                    "no handoffs captured\n".into()
                } else {
                    listing.into_bytes()
                }
            }))
        }
        "prompt" => {
            let budget = args.limit("max-bytes", 4096, 512, 65536)?;
            let task = storage::read(&repo, Path::new(".oms/task/current.md"), true)?;
            let plan = storage::read(&repo, Path::new(".oms/plan/current.json"), true)?;
            let mut body = format!(
                "# Next-session prompt\n\nContinue the recorded project goal. Inspect the repository contracts and current state before editing.\n\n{}\n{}\n\nVerification: only recorded mechanical receipts prove completion.\n",
                String::from_utf8_lossy(&task),
                String::from_utf8_lossy(&plan)
            );
            if body.len() > budget {
                body = format!(
                    "{}\ntruncated: remaining OMS context omitted\n",
                    safety::truncate(&body, budget - 50)
                );
            }
            safety::outbound(body.as_bytes())?;
            if args.has("output") {
                let path = Path::new(args.get("output"));
                storage::write(&repo, path, body.into_bytes(), false, context.interrupted)?;
                Ok(out(format!("{}\n", repo.join(path).display()).into_bytes()))
            } else {
                Ok(out(body.into_bytes()))
            }
        }
        "capture" => {
            let provider = args.value("agent", "claude");
            if !["claude", "codex"].contains(&provider) {
                return fail("native Antigravity prompt-only handoff is not integrated");
            }
            let roots_args = Args::parse(
                &[OsString::from("--provider"), provider.into()],
                &["provider"],
                &[],
            )?;
            let (sources, truncated, _) = discover(&repo, &roots_args, context)?;
            if truncated {
                return fail("session discovery incomplete");
            }
            let source = sources.into_iter().find(|s| {
                !args.has("session")
                    || s.ident == args.get("session")
                    || s.path == context.cwd.join(args.get("session"))
            });
            let Some(source) = source else {
                if args.value("missing-session-policy", "fail") == "skip" {
                    return Ok(out(Vec::new()));
                }
                return fail("native session transcript is absent");
            };
            let raw = read(&source)?;
            let messages = messages(&source, &raw);
            let users = messages
                .iter()
                .filter(|m| m.role == "user")
                .collect::<Vec<_>>();
            if users.len() < args.limit("min-user-turns", 2, 0, usize::MAX)? {
                return Ok((
                    0,
                    Vec::new(),
                    b"session-handoff: too few user turns; skipped\n".to_vec(),
                ));
            }
            let turns = std::env::var("OMS_HANDOFF_TURNS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(6)
                .min(50);
            let mut body = format!(
                "# Session handoff\n\n- agent: {provider}\n- session: {}\n- captured: {}\n\n{}\n\n## Goal\n{}\n\n## Recent user turns\n",
                source.ident,
                now(),
                args.get("note"),
                users.first().map_or("", |m| m.text.as_str())
            );
            for message in users.iter().rev().take(turns).rev() {
                body.push_str(&format!("\n{}\n", safety::truncate(&message.text, 2000)));
            }
            body.push_str("\n## Last assistant summary\n");
            if let Some(last) = messages.iter().rev().find(|m| m.role == "assistant") {
                body.push_str(safety::truncate(&last.text, 6000));
            }
            body.push('\n');
            if !args.has("allow-sensitive") {
                safety::outbound(body.as_bytes())?;
            }
            let path = if args.has("out") {
                PathBuf::from(args.get("out"))
            } else {
                directory.join(format!("{provider}-{}.md", crate::peer::run_id()?))
            };
            storage::write(&repo, &path, body.into_bytes(), false, context.interrupted)?;
            Ok(out(format!("{}\n", repo.join(path).display()).into_bytes()))
        }
        _ => fail("session-handoff expects capture, list, show or prompt"),
    }
}
