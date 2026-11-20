use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, process, providers,
    safety, storage, text,
};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

const SESSION_BYTES: usize = 32 * 1024 * 1024;
const SESSION_HEADER_BYTES: usize = 65536;
const HOOK_TRANSCRIPT_ERROR: &str = "unsafe or unavailable Codex hook transcript";

pub struct CodexHookTranscript<'a> {
    pub session: &'a str,
    pub path: &'a Path,
}

#[derive(Clone)]
struct Source {
    provider: String,
    ident: String,
    path: PathBuf,
    root: PathBuf,
    repo: PathBuf,
    modified: u128,
    account_home: Option<PathBuf>,
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
type Root = (String, PathBuf, usize, Option<PathBuf>);

fn roots(args: &Args) -> Result<Vec<Root>> {
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
            values.push((provider, path, 4, None));
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
        .unwrap_or_else(|| {
            std::env::var_os("PI_CODING_AGENT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".pi/agent"))
                .join("sessions")
        });
    let mut found = vec![
        ("claude".into(), claude.join("projects"), 1, None),
        ("codex".into(), codex.join("sessions"), 3, None),
        ("codex".into(), codex.join("archived_sessions"), 3, None),
        ("pi".into(), pi, 1, None),
    ];
    // Registered account homes keep their transcripts beside their own credentials.
    for entry in providers::configured_homes()? {
        let (provider, home) = entry;
        match provider.as_str() {
            "claude" => found.insert(0, (provider, home.join("projects"), 1, Some(home))),
            "codex" => {
                found.insert(
                    0,
                    (
                        provider.clone(),
                        home.join("sessions"),
                        3,
                        Some(home.clone()),
                    ),
                );
                found.insert(0, (provider, home.join("archived_sessions"), 3, Some(home)));
            }
            _ => found.insert(0, (provider, home.join("sessions"), 1, Some(home))),
        }
    }
    let mut seen = BTreeSet::new();
    Ok(found
        .into_iter()
        .filter(|(p, path, _, _)| {
            (selected == "all" || p == selected) && seen.insert((p.clone(), path.clone()))
        })
        .collect())
}
fn metadata(raw: &[u8], provider: &str) -> Option<(String, PathBuf)> {
    for line in raw.split(|b| *b == b'\n').take(32) {
        if line.len() > SESSION_HEADER_BYTES {
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
    for (provider, root, max_depth, account_home) in roots(args)? {
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
                    if meta.len() > SESSION_BYTES as u64 {
                        continue;
                    }
                    let bytes = storage::file(&path)?;
                    if let Some((ident, owner)) =
                        metadata(&bytes[..bytes.len().min(SESSION_HEADER_BYTES)], &provider)
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
                            account_home: account_home.clone(),
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
    if bytes.len() > SESSION_BYTES
        || metadata(
            &bytes[..bytes.len().min(SESSION_HEADER_BYTES)],
            &source.provider,
        ) != Some((source.ident.clone(), source.repo.clone()))
    {
        return fail("transcript identity or repository binding changed");
    }
    Ok(bytes)
}

fn absolute_label(path: &Path) -> Option<&OsStr> {
    let raw = path.as_os_str().as_bytes();
    let label = raw.strip_prefix(b"/")?;
    if raw.len() > 4096
        || raw.iter().any(u8::is_ascii_control)
        || label
            .split(|&byte| byte == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return None;
    }
    Some(OsStr::from_bytes(label))
}

fn read_hook_transcript(hint: CodexHookTranscript<'_>, repo: &Path) -> Option<Vec<u8>> {
    let codex_home = match std::env::var_os("CODEX_HOME") {
        Some(value) => PathBuf::from(value),
        None => {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            absolute_label(&home)?;
            home.join(".codex")
        }
    };
    absolute_label(&codex_home)?;
    let root = codex_home.join("sessions");
    let label = absolute_label(hint.path)?;
    if !hint.path.starts_with(&root) || hint.path.extension() != Some(OsStr::new("jsonl")) {
        return None;
    }
    // The filesystem root keeps every component of the account path in the
    // descriptor-relative no-follow chain, including the sessions directory.
    let bytes = oms_safe_fs::Root::open("/")
        .ok()?
        .read_os(label, SESSION_BYTES, oms_safe_fs::Policy::manifest())
        .ok()?;
    let header = &bytes[..bytes.len().min(SESSION_HEADER_BYTES + 1)];
    let end = header.iter().position(|&byte| byte == b'\n')?;
    let header = &header[..end];
    oms_json::parse_strict(header).ok()?;
    let (id, cwd) = metadata(header, "codex")?;
    (id == hint.session && cwd == repo).then_some(bytes)
}

fn messages(source: &Source, raw: &[u8]) -> Vec<Message> {
    parse_messages(&source.provider, &source.ident, &source.repo, raw)
}

fn parse_messages(provider: &str, ident: &str, repo: &Path, raw: &[u8]) -> Vec<Message> {
    let mut result = Vec::new();
    let mut cwd = repo.to_owned();
    for (line, bytes) in raw.split(|b| *b == b'\n').enumerate() {
        if bytes.len() > 1024 * 1024 {
            continue;
        }
        let Ok(row) = V::parse(bytes) else { continue };
        if provider == "codex"
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
        if path.canonicalize().ok().as_deref() != Some(repo) {
            continue;
        }
        let (message, prefix) = if provider == "codex" {
            if text(&row, "type") == "event_msg"
                && text(row.get("payload"), "type") == "user_message"
                && let Some(text) = row.get("payload").get("message").text()
            {
                result.push(Message {
                    line: line + 1,
                    part: 0,
                    pointer: "/payload/message".into(),
                    role: "user".into(),
                    text: text.into(),
                    hash: storage::hash(bytes),
                });
                continue;
            }
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
        if provider == "claude" && row.get("sessionId").text().is_some_and(|id| id != ident) {
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
pub(crate) fn advice_digest(
    repo: &Path,
    ident: &str,
    allow_sensitive: bool,
    context: &Context<'_>,
) -> Result<String> {
    let caller = crate::peer::detect_caller()?.filter(|p| matches!(p.as_str(), "claude" | "codex" | "pi")).ok_or_else(|| {
        Error::new(
            2,
            "cannot identify calling agent with native session history for --session; set OMS_AGENT (e.g. codex or claude) or pass --session-id",
        )
    })?;
    let pinned = if !ident.is_empty() {
        ident.to_owned()
    } else if caller == "codex" {
        std::env::var("CODEX_THREAD_ID").unwrap_or_default()
    } else {
        String::new()
    };
    let args = Args::parse(&["--provider".into(), caller.into()], &["provider"], &[])?;
    let (sources, truncated, _) = discover(repo, &args, context)?;
    if truncated {
        return fail("session discovery incomplete; pin a bounded source before advising");
    }
    let source = sources
        .into_iter()
        .find(|s| pinned.is_empty() || s.ident == pinned)
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
    if !allow_sensitive && safety::outbound(body.as_bytes()).is_err() {
        return Err(Error::new(
            3,
            "calling session digest contains sensitive content; pass --allow-sensitive to override the session check",
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
        return handoff(argv, context, None);
    }
    let mut args = Args::parse(
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
    let (source, bookmark) =
        if args.action() == "bookmark" && matches!(args.operand(1), "open" | "resume") {
            let ident = args.operand(2);
            if !storage::identifier(ident) {
                return fail("invalid bookmark id");
            }
            let record = V::parse(&storage::read(
                &repo,
                &Path::new(".oms/sessions/bookmarks").join(format!("{ident}.json")),
                false,
            )?)?;
            if record.get("schema") != &1usize.into()
                || text(&record, "id") != ident
                || Path::new(text(&record, "repo")) != repo
            {
                return fail("bookmark record is missing or belongs to a different repository");
            }
            let value = record.get("source");
            if Path::new(text(value, "repo")) != repo
                || !["claude", "codex", "pi"].contains(&text(value, "provider"))
                || !storage::identifier(text(value, "session_id"))
            {
                return fail("bookmark source has no valid repository binding");
            }
            let source = Source {
                provider: text(value, "provider").into(),
                ident: text(value, "session_id").into(),
                path: PathBuf::from(text(value, "path")),
                root: PathBuf::from(text(value, "root")),
                repo: repo.clone(),
                modified: 0,
                account_home: None,
            };
            if !source.path.is_absolute() || !source.root.is_absolute() {
                return fail("bookmark source paths must be absolute");
            }
            (source, Some(record))
        } else {
            (select(&repo, &args, context)?, None)
        };
    let raw = read(&source)?;
    let items = messages(&source, &raw);
    if let Some(bookmark) = bookmark {
        let position = |key| {
            bookmark
                .get(key)
                .integer(false)
                .then(|| bookmark.get(key).py().parse::<usize>().ok())
                .flatten()
        };
        let message = items
            .iter()
            .find(|message| {
                Some(message.line) == position("line") && Some(message.part) == position("part")
            })
            .filter(|message| text(&bookmark, "line_sha256") == message.hash)
            .ok_or_else(|| {
                Error::new(
                    2,
                    "bookmark is stale: its original line changed; select a new origin",
                )
            })?;
        if args.operand(1) == "open" {
            return Ok(out(json(&obj([
                ("schema", 1usize.into()),
                ("kind", "oms-session-bookmark-open".into()),
                ("origin", origin(&source, message, 0, None)),
            ]))?));
        }
        args.positional = vec!["resume".into()];
    }
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
        let label = args.get("label");
        safety::outbound(label.as_bytes())?;
        let absolute =
            regex::Regex::new(r"(?:^|[^A-Za-z0-9_.:/-])(?:/[^/\s]+/[^\s]+|[A-Za-z]:[\\/][^\s]+)")
                .unwrap();
        if label.len() > 120 || label.chars().any(char::is_control) || absolute.is_match(label) {
            return fail(
                "label must be a short title without sensitive content or control characters",
            );
        }
        let source_record = obj([
            ("provider", source.provider.clone().into()),
            ("session_id", source.ident.clone().into()),
            ("path", source.path.to_string_lossy().into_owned().into()),
            ("root", source.root.to_string_lossy().into_owned().into()),
            ("repo", repo.to_string_lossy().into_owned().into()),
            ("bytes", raw.len().into()),
            ("mtime_ns", V::Num(source.modified.to_string())),
            ("sha256", storage::hash(&raw).into()),
        ]);
        let row = obj([
            ("schema", 1usize.into()),
            ("id", ident.clone().into()),
            ("repo", repo.to_string_lossy().into_owned().into()),
            ("created_at", now().into()),
            ("label", label.into()),
            ("source", source_record),
            ("line", message.line.into()),
            ("part", message.part.into()),
            ("line_sha256", message.hash.clone().into()),
        ]);
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
    let configured = Args::parse(
        &["--provider".into(), source.provider.clone().into()],
        &["provider"],
        &[],
    )?;
    if source.provider != "pi"
        && !roots(&configured)?
            .iter()
            .any(|(_, root, _, _)| source.path.starts_with(root))
    {
        return fail(
            "selected transcript is outside the configured native store; custom roots are read-only for native launches",
        );
    }
    refuse_managed_resume(&repo, &source, fork, context)?;
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
        ("provider", source.provider.clone().into()),
        ("source_session", source.ident.clone().into()),
        ("action", if fork { "fork" } else { "resume" }.into()),
        ("capability", "installed CLI --help".into()),
        ("dry_run", (!args.has("apply")).into()),
    ]);
    if args.has("apply") {
        let owner = source.account_home.clone().or(roots(&configured)?
            .into_iter()
            .find(|(provider, root, _, _)| {
                *provider == source.provider && source.path.starts_with(root)
            })
            .and_then(|(_, _, _, home)| home));
        let (mut environment, account_remove, account) = match owner {
            Some(home) => providers::session_environment(&source.provider, &home)?,
            None => (Vec::new(), Vec::new(), None),
        };
        let mut remove_environment: Vec<OsString> = std::env::vars_os()
            .filter_map(|(k, _)| {
                k.to_str()
                    .is_some_and(|s| s.starts_with("OMS_") || s.starts_with("GIT_"))
                    .then_some(k)
            })
            .collect();
        remove_environment.extend(account_remove);
        let usage_repo = crate::usage::state_repository(&repo, &[])?;
        let pending = crate::admission::begin_interactive_with_executor(
            crate::admission::InteractiveRequest {
                state_root: &usage_repo,
                provider: &source.provider,
                selected_model: None,
                account: account.as_deref(),
                attempt: None,
                resume_session: Some(&source.ident),
            },
            crate::admission::InteractiveContext {
                contract_repo: &repo,
                execution_repo: &repo,
                root: None,
                team: None,
                task_id: None,
            },
            context.executor,
            context.interrupted,
        )?;
        let call_id = pending.call_id().to_owned();
        environment.push(("OMS_PROVIDER_CALL_ID".into(), call_id.clone().into()));
        remove_environment.retain(|key| key != "OMS_PROVIDER_CALL_ID");
        pending.fence_with_executor(context.executor, context.interrupted)?;
        let result = context.executor.execute(
            process::Invocation {
                argv: native,
                cwd: repo,
                input: None,
                environment,
                remove_environment,
                timeout: Duration::from_secs(86400),
                max_bytes: 1024 * 1024,
                merge: false,
                launch_fence: Some(pending.launch_fence()),
                physical_call: Some(
                    oms_supervisor::process::PhysicalCall::new(&call_id)
                        .map_err(|e| Error::new(2, e))?,
                ),
            },
            context.interrupted,
        );
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let _ = pending.finish(None, None);
                return Err(error);
            }
        };
        pending
            .finish_with_cleanup(
                Some(&result.stdout),
                result.cleanup_complete,
                result.owned_cleanup.as_ref(),
            )
            .map_err(|error| {
                Error::new(
                    error.code,
                    format!(
                        "native resume usage accounting failed; reconcile before retrying: {error}"
                    ),
                )
            })?;
        report.set("provider_call_id", call_id);
        report.set("native_returncode", V::Num(result.code.to_string()));
        return Ok((result.code, json(&report)?, Vec::new()));
    }
    Ok(out(json(&report)?))
}
pub(crate) fn handoff(
    argv: &[OsString],
    context: &Context<'_>,
    transcript: Option<CodexHookTranscript<'_>>,
) -> Result<Output> {
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
    let repo = storage::repository(context, &args.path("cwd", context.cwd));
    let repo = if let Some(hint) = &transcript {
        let repo = repo.map_err(|_| Error::new(2, HOOK_TRANSCRIPT_ERROR))?;
        if args.action() != "capture"
            || args.get("agent") != "codex"
            || args.get("session") != hint.session
            || !storage::identifier(hint.session)
            || repo != context.cwd
        {
            return fail(HOOK_TRANSCRIPT_ERROR);
        }
        repo
    } else {
        repo?
    };
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
            let services = crate::native::Services(context);
            let body = oms_command_autonomy::handoff::render_checked(
                &repo,
                &repo,
                budget,
                &services,
                context.interrupted,
                &mut |raw| {
                    safety::outbound(raw)
                        .map_err(|e| oms_command_autonomy::Error::new(e.code, e.message))
                },
            )
            .map_err(|e| Error::new(e.code, e.message))?;
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
            if matches!(provider, "antigravity" | "agy") {
                return antigravity_handoff(&repo, &args, context);
            }
            if !["claude", "codex"].contains(&provider) {
                return fail("unknown handoff agent; use claude, codex or antigravity");
            }
            let (ident, raw) = if let Some(hint) = transcript {
                let ident = hint.session.to_owned();
                let raw = read_hook_transcript(hint, &repo)
                    .ok_or_else(|| Error::new(2, HOOK_TRANSCRIPT_ERROR))?;
                (ident, raw)
            } else {
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
                (source.ident, raw)
            };
            let messages = parse_messages(provider, &ident, &repo, &raw);
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
                "# Session handoff\n\n- agent: {provider}\n- session: {ident}\n- captured: {}\n\n{}\n\n## Goal\n{}\n\n## Recent user turns\n",
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

fn refuse_managed_resume(
    repo: &Path,
    source: &Source,
    fork: bool,
    context: &Context<'_>,
) -> Result<()> {
    if fork {
        return Ok(());
    }
    let digest = storage::hash(source.ident.as_bytes());
    let room = oms_command_room::Room::open(
        repo,
        oms_command_room::Session::from_env(),
        &oms_command_room::LocalServices,
        context.interrupted,
        false,
        false,
    )
    .map_err(|_| {
        Error::new(
            2,
            "native owner discovery is incomplete; resume through the existing panel",
        )
    })?;
    for name in storage::names(repo, Path::new(".oms/threads"), ".jsonl")? {
        let rows = room.records(name.trim_end_matches(".jsonl")).map_err(|_| {
            Error::new(
                2,
                "native owner discovery is incomplete; resume through the existing panel",
            )
        })?;
        if !rows
            .iter()
            .any(|row| row["room_event"].text("kind") == "created")
        {
            continue;
        }
        let state = oms_command_room::project(&rows).map_err(|_| {
            Error::new(
                2,
                "native owner discovery is incomplete; resume through the existing panel",
            )
        })?;
        if state.list("participants").iter().any(|member| {
            (member.text("provider") == source.provider
                || source.provider == "pi" && member.text("provider") == "codex")
                && [member.text("consumer"), member.text("initial_consumer")]
                    .contains(&&digest[..32])
        }) {
            return fail(
                "managed native sessions must resume through the existing panel to retain owner and lease checks",
            );
        }
    }
    Ok(())
}

fn antigravity_handoff(repo: &Path, args: &Args, context: &Context<'_>) -> Result<Output> {
    let home = std::env::var_os("OMS_GEMINI_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".gemini")))
        .ok_or_else(|| Error::new(2, "native history home unavailable"))?;
    let path = home.join("antigravity-cli/history.jsonl");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if args.value("missing-session-policy", "fail") == "skip" {
                return Ok(out(Vec::new()));
            }
            return Err(Error::new(3, "native Antigravity history is absent"));
        }
        Err(_) => return fail("native Antigravity history is unavailable"),
        _ => {}
    }
    plain(&path, false)?;
    let bytes = storage::file(&path)?;
    let mut rows = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        crate::check(context.interrupted)?;
        if line.len() > 1024 * 1024 {
            return fail("native history row exceeds the read bound");
        }
        let Ok(row) = V::parse(line) else {
            continue;
        };
        if Path::new(text(&row, "workspace")) == repo
            && !text(&row, "display").trim().is_empty()
            && (!args.has("session") || text(&row, "conversationId") == args.get("session"))
        {
            rows.push(row);
        }
    }
    let conversation = if args.has("session") {
        args.get("session").to_owned()
    } else {
        rows.last()
            .map_or("", |row| text(row, "conversationId"))
            .to_owned()
    };
    let users = rows
        .iter()
        .filter(|row| text(row, "conversationId") == conversation)
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
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(6)
        .min(50);
    let mut body = format!(
        "# Session handoff\n\n- agent: antigravity\n- session: {conversation}\n- captured: {}\n- source_kind: prompts-only\n\n{}\n\n## Goal\n{}\n\n## Recent user turns\n",
        now(),
        args.get("note"),
        users
            .first()
            .map(|row| safety::truncate(text(row, "display"), 600))
            .unwrap_or("(no antigravity prompts matched)")
    );
    for row in users.iter().rev().take(turns).rev() {
        body.push_str(&format!(
            "\n{}\n",
            safety::truncate(text(row, "display"), 400)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    body.push_str("\n## Last assistant summary\n(antigravity assistant output not available from history.jsonl)\n");
    if !args.has("allow-sensitive") {
        safety::outbound(body.as_bytes())?;
    }
    let output = if args.has("out") {
        PathBuf::from(args.get("out"))
    } else {
        Path::new(".oms/handoffs").join(format!("antigravity-{}.md", crate::peer::run_id()?))
    };
    storage::write(repo, &output, body.into_bytes(), false, context.interrupted)?;
    Ok(out(
        format!("{}\n", repo.join(output).display()).into_bytes()
    ))
}
