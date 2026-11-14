use crate::{
    db::Query,
    store::{self, Store},
    *,
};
use std::collections::{BTreeMap, BTreeSet};
use std::{
    ffi::OsString,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
const SHELL_MODES: &[&str] = &[
    "path",
    "db-path",
    "show",
    "context",
    "init",
    "append",
    "pin",
    "supersede",
    "retire",
    "redact",
    "sources",
    "compact",
    "search",
    "recall",
    "health",
    "rebuild",
];
#[derive(Default)]
pub(crate) struct Options {
    pub mode: String,
    pub values: BTreeMap<String, OsString>,
    pub flags: BTreeSet<String>,
    pub kinds: Vec<String>,
    pub words: Vec<String>,
}
impl Options {
    pub fn value(&self, k: &str) -> String {
        self.values
            .get(k)
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    pub fn path(&self, k: &str) -> Option<PathBuf> {
        self.values.get(k).map(PathBuf::from)
    }
    pub fn has(&self, k: &str) -> bool {
        self.flags.contains(k)
    }
    pub fn number(&self, k: &str, default: usize) -> Result<usize> {
        if !self.values.contains_key(k) {
            return Ok(default);
        }
        let s = self.value(k);
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return err(format!("{k} requires a positive integer"));
        }
        let n = s
            .parse::<usize>()
            .map_err(|_| Error(2, format!("{k} requires a positive integer")))?;
        if n == 0 {
            return err(format!("{k} requires a positive integer"));
        }
        Ok(n)
    }
}
fn parse(args: &[OsString]) -> Result<Options> {
    let takes = &[
        "--repo",
        "--file",
        "--agent",
        "--text",
        "--limit",
        "--source-file",
        "--source-line",
        "--id",
        "--reason",
        "--paths",
        "--target",
        "--from",
        "--select",
        "--kind",
        "--accept",
        "--retire",
        "--reject",
        "--to",
        "--cold-days",
    ];
    let mut first = String::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        let s = a.to_string_lossy();
        if takes.contains(&s.as_ref()) {
            skip = true;
            continue;
        }
        if s.starts_with('-') {
            continue;
        }
        first = s.into_owned();
        break;
    }
    let special = [
        "import", "review", "capture", "promote", "graduate", "hygiene",
    ]
    .contains(&first.as_str())
        || (first == "recall" && args.iter().any(|a| a == "--all-repos"));
    let expanded: Vec<OsString> = if special {
        let mut expanded = Vec::new();
        for arg in args {
            use std::os::unix::ffi::OsStringExt;
            let raw = arg.as_encoded_bytes();
            if raw.starts_with(b"--") {
                if let Some(at) = raw.iter().position(|b| *b == b'=') {
                    expanded.push(OsString::from_vec(raw[..at].to_vec()));
                    expanded.push(OsString::from_vec(raw[at + 1..].to_vec()));
                    continue;
                }
            }
            expanded.push(arg.clone());
        }
        expanded
    } else {
        args.to_vec()
    };
    let args = expanded.as_slice();
    let mut o = Options::default();
    let mut at = 0;
    while at < args.len() {
        let s = args[at].to_string_lossy().into_owned();
        if takes.contains(&s.as_str())
            && (special
                || ![
                    "--from",
                    "--select",
                    "--kind",
                    "--accept",
                    "--retire",
                    "--reject",
                    "--to",
                    "--cold-days",
                ]
                .contains(&s.as_str()))
        {
            let Some(value) = args.get(at + 1) else {
                return err(format!(
                    "{s} requires {}",
                    match s.as_str() {
                        "--repo" | "--file" => "path",
                        "--agent" => "name",
                        "--source-file" => "a path",
                        "--text" | "--reason" => "text",
                        _ => "a value",
                    }
                ));
            };
            if !special && ["--limit", "--source-line"].contains(&s.as_str()) {
                let number = value.to_string_lossy();
                if number.is_empty()
                    || !number.bytes().all(|b| b.is_ascii_digit())
                    || number.bytes().all(|b| b == b'0')
                {
                    return err(format!("{s} requires a positive integer"));
                }
            }
            if s == "--kind" {
                o.kinds.push(value.to_string_lossy().into_owned());
            }
            o.values.insert(s, value.clone());
            at += 2;
            continue;
        }
        if [
            "--global",
            "--stdin",
            "--full",
            "--json",
            "--apply",
            "--include-stale",
            "--include-superseded",
            "--all-repos",
            "--dry-run",
            "--all",
            "--help",
            "-h",
        ]
        .contains(&s.as_str())
            && (special || !["--all-repos", "--dry-run", "--all"].contains(&s.as_str()))
        {
            let help = !special && ["--help", "-h"].contains(&s.as_str());
            o.flags.insert(s);
            if help {
                break;
            }
            at += 1;
            continue;
        }
        if special {
            if o.mode.is_empty() && s == first {
                o.mode = s;
            } else {
                o.words.push(s);
            }
        } else if SHELL_MODES.contains(&s.as_str()) || o.mode.is_empty() {
            o.mode = s;
        } else {
            let mut note = o.values.get("--text").cloned().unwrap_or_default();
            if !note.is_empty() {
                note.push(" ");
            }
            note.push(&args[at]);
            o.values.insert("--text".into(), note);
        }
        at += 1;
    }
    if o.mode.is_empty() {
        o.mode = "show".into();
    }
    if special {
        let allowed_values: &[&str] = match first.as_str() {
            "hygiene" => &["--repo", "--cold-days", "--limit"],
            "import" | "review" | "capture" => &[
                "--repo", "--agent", "--from", "--select", "--kind", "--accept", "--retire",
                "--reject", "--reason",
            ],
            _ => &["--repo", "--agent", "--id", "--to", "--text", "--limit"],
        };
        let allowed_flags: &[&str] = match first.as_str() {
            "hygiene" => &["--json"],
            "import" | "review" | "capture" => &["--apply", "--dry-run", "--all", "--json"],
            _ => &["--global", "--all-repos", "--apply", "--json"],
        };
        for key in o.values.keys() {
            if !allowed_values.contains(&key.as_str()) {
                return err(format!(
                    "{} does not take {key} (project memory only; --global and --file are not supported)",
                    o.mode
                ));
            }
        }
        for key in &o.flags {
            if !allowed_flags.contains(&key.as_str()) {
                return err(format!("{} does not take {key}", o.mode));
            }
        }
        if ["hygiene", "import", "review", "capture"].contains(&o.mode.as_str())
            && !o.words.is_empty()
        {
            return err(format!("{} does not take {}", o.mode, o.words[0]));
        }
        if o.words.iter().any(|s| s.starts_with('-')) {
            return err(format!(
                "{} does not take {}",
                o.mode,
                o.words.iter().find(|s| s.starts_with('-')).unwrap()
            ));
        }
    }
    Ok(o)
}
fn path_list(raw: &str, kind: &str) -> Result<String> {
    let mut out = Vec::new();
    for s in raw.split(',') {
        let s = s.trim();
        let s = s.strip_prefix("./").unwrap_or(s);
        if s.is_empty() {
            continue;
        }
        if s.starts_with('/')
            || s.contains("..")
            || s.len() > 120
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._*?/-".contains(&b))
        {
            return err(format!(
                "{kind} takes up to 8 repository-relative {} of [A-Za-z0-9._*?/-], no leading / and no ..",
                if kind == "--paths" { "globs" } else { "paths" }
            ));
        }
        out.push(s);
    }
    if out.is_empty() || out.len() > 8 {
        return err(format!(
            "{kind} takes up to 8 repository-relative paths of [A-Za-z0-9._*?/-], no leading / and no .."
        ));
    }
    Ok(out.join(","))
}
pub(crate) fn child() -> bool {
    std::env::var("OMS_HARNESS_CHILD").as_deref() == Ok("1")
}
pub(crate) fn parent_only(what: &str) -> Result<()> {
    if child() {
        err(format!(
            "a harness child cannot {what}; return the request to the parent agent"
        ))
    } else {
        Ok(())
    }
}
fn agent() -> String {
    if let Ok(s) = std::env::var("OMS_AGENT") {
        if !s.is_empty() {
            return s;
        }
    }
    if std::env::var_os("CLAUDECODE").is_some_and(|v| !v.is_empty())
        || std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some_and(|v| !v.is_empty())
    {
        "claude"
    } else if std::env::var_os("CODEX_SANDBOX").is_some_and(|v| !v.is_empty()) {
        "codex"
    } else {
        "agent"
    }
    .into()
}
pub(crate) fn execute(
    args: &[OsString],
    cwd: &Path,
    _source: &Path,
    stdin: Option<&[u8]>,
    flag: &AtomicBool,
) -> Result<Output> {
    execute_inner(args, cwd, _source, stdin, flag, false)
}
pub(crate) fn execute_review(
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    stdin: Option<&[u8]>,
    flag: &AtomicBool,
) -> Result<Output> {
    execute_inner(args, cwd, source, stdin, flag, true)
}
fn execute_inner(
    args: &[OsString],
    cwd: &Path,
    _source: &Path,
    stdin: Option<&[u8]>,
    flag: &AtomicBool,
    review_adapter: bool,
) -> Result<Output> {
    check(flag)?;
    let o = parse(args)?;
    if review_adapter && !["import", "review", "capture"].contains(&o.mode.as_str()) {
        return err(format!("unknown memory review command: {}", o.mode));
    }
    if o.mode == "capture" && !review_adapter {
        return err(format!(
            "unknown command: capture\n{}",
            include_str!("usage.txt")
        ));
    }
    let special = [
        "import", "review", "capture", "promote", "graduate", "hygiene",
    ]
    .contains(&o.mode.as_str())
        || (o.mode == "recall" && o.has("--all-repos"));
    if !special && (o.has("--help") || o.has("-h")) {
        return Ok((0, include_bytes!("usage.txt").to_vec(), Vec::new()));
    }
    let default_repo = std::env::var_os("OMS_STATE_REPO")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_owned());
    let repo_path = o.path("--repo").unwrap_or(default_repo);
    if o.mode == "recall" && o.has("--all-repos") {
        return crate::carry::recall_all(&o, cwd, flag);
    }
    let repo = if !special && (o.has("--global") || o.path("--file").is_some()) {
        std::fs::canonicalize(&repo_path).unwrap_or_else(|_| store::absolute(cwd, &repo_path))
    } else {
        store::repo_root(cwd, &repo_path, flag)?
    };
    let file = o
        .path("--file")
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| store::absolute(cwd, &p));
    let explicit = o.values.contains_key("--file");
    let global = o.has("--global");
    let s = Store::new(
        repo,
        file,
        global && !matches!(o.mode.as_str(), "promote" | "graduate"),
    );
    let author = if o.value("--agent").is_empty() {
        agent()
    } else {
        o.value("--agent")
    };
    if special {
        return s.with_warnings(workflows::execute(&s, &o, &author, cwd, stdin, flag));
    }
    let mut note = o.value("--text");
    for w in &o.words {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(w);
    }
    if !["append", "pin", "supersede", "search", "recall"].contains(&o.mode.as_str())
        && !note.is_empty()
    {
        return err(format!("unknown argument: {note}"));
    }
    if o.values.contains_key("--limit") && !["search", "recall"].contains(&o.mode.as_str()) {
        return err("--limit applies only to search or recall");
    }
    let limit = o.number("--limit", if o.mode == "recall" { 20 } else { 0 })?;
    let source = o
        .path("--source-file")
        .filter(|p| !p.as_os_str().is_empty());
    let source_line = o.number("--source-line", 0)?;
    if source.is_some() || source_line > 0 {
        if o.mode != "append" {
            return err("--source-file/--source-line apply only to append");
        }
        if source.is_none() || source_line == 0 {
            return err("--source-file and --source-line must be used together");
        }
        if global {
            return err("source citations require project memory");
        }
    }
    let paths = o.value("--paths");
    let paths = if paths.is_empty() {
        paths
    } else {
        if o.mode != "append" || global || explicit {
            return err("--paths applies only to append on project memory");
        }
        path_list(&paths, "--paths")?
    };
    let target = o.value("--target");
    let target = if target.is_empty() {
        target
    } else {
        if o.mode != "recall" {
            return err("--target applies only to recall");
        }
        path_list(&target, "--target")?
    };
    let id = o.value("--id");
    let reason = o.value("--reason");
    if ["supersede", "retire", "redact"].contains(&o.mode.as_str()) {
        if id.is_empty() {
            return err(format!("{} requires --id", o.mode));
        }
    } else if !id.is_empty() || !reason.is_empty() {
        return err("--id/--reason apply only to supersede, retire or redact");
    }
    if o.mode != "retire" && !reason.is_empty() {
        return err("--reason applies only to retire");
    }
    if o.has("--apply") && o.mode != "redact" {
        return err("--apply applies only to redact");
    }
    for option in ["--include-stale", "--include-superseded"] {
        if o.has(option) && !["search", "recall"].contains(&o.mode.as_str()) {
            return err(format!("{option} applies only to search or recall"));
        }
    }
    if o.has("--json") && !["search", "recall", "health", "sources"].contains(&o.mode.as_str()) {
        return err("--json applies only to search, recall, health, or sources");
    }
    if child()
        && (global || explicit)
        && ([
            "init",
            "append",
            "pin",
            "compact",
            "search",
            "recall",
            "rebuild",
            "supersede",
            "retire",
        ]
        .contains(&o.mode.as_str())
            || (o.mode == "context" && !o.has("--full")))
    {
        return err(
            "a harness child cannot mutate parent-owned host or global state; return the request to the parent agent",
        );
    }
    let mut raw_note = o
        .values
        .get("--text")
        .map(|s| s.as_encoded_bytes().to_vec())
        .unwrap_or_default();
    if ["append", "pin", "supersede"].contains(&o.mode.as_str()) {
        if o.has("--stdin") {
            let mut data = Vec::new();
            if let Some(bytes) = stdin {
                data.extend_from_slice(bytes);
            } else {
                std::io::stdin().read_to_end(&mut data)?;
            }
            note = String::from_utf8_lossy(&data).into_owned();
            raw_note = data;
        } else if note.is_empty() {
            return err(format!("{} requires --text or --stdin", o.mode));
        } else {
            note.push('\n');
            raw_note.push(b'\n');
        }
    }
    let result = match o.mode.as_str() {
        "path" => Ok(os::line(&[s.shared.as_os_str().as_bytes()])),
        "db-path" => Ok(os::line(&[s.database.as_os_str().as_bytes()])),
        "show" => {
            let raw = store::read(&s.shared)?;
            Ok(if raw.is_empty() {
                os::line(&[b"memory: empty (", s.shared.as_os_str().as_bytes(), b")"])
            } else {
                (0, raw, Vec::new())
            })
        }
        "init" => {
            s.init(flag)?;
            db::sync(&s, flag)?;
            Ok(os::line(&[
                b"memory: initialized ",
                s.shared.as_os_str().as_bytes(),
                b"\nmemory: indexed ",
                s.database.as_os_str().as_bytes(),
            ]))
        }
        "append" => {
            let warning = workflows::contradictions(
                &s,
                &note,
                &paths,
                source.as_deref().unwrap_or(Path::new("")),
            )
            .unwrap_or_default();
            s.append_bytes(
                &author,
                &raw_note,
                "note",
                source.as_deref().map(|p| (p, source_line)),
                &paths,
                None,
                flag,
            )?;
            Ok((
                0,
                os::line(&[
                    b"memory: appended ",
                    s.shared.as_os_str().as_bytes(),
                    b"\nmemory: refreshed ",
                    s.summary.as_os_str().as_bytes(),
                ])
                .1,
                warning.into_bytes(),
            ))
        }
        "pin" => {
            s.pin_bytes(&author, &raw_note, flag)?;
            Ok(os::line(&[
                b"memory: pinned ",
                s.pins.as_os_str().as_bytes(),
            ]))
        }
        "supersede" | "retire" => {
            if o.mode == "retire" && reason.is_empty() {
                return err("retire requires --reason");
            }
            let mut retired = o
                .values
                .get("--reason")
                .map(|s| s.as_encoded_bytes().to_vec())
                .unwrap_or_default();
            retired.push(b'\n');
            let body = if o.mode == "retire" {
                &retired
            } else {
                &raw_note
            };
            s.append_bytes(
                &author,
                body,
                if o.mode == "retire" {
                    "retirement"
                } else {
                    "note"
                },
                None,
                "",
                Some((
                    &id,
                    if o.mode == "retire" {
                        "retires"
                    } else {
                        "supersedes"
                    },
                )),
                flag,
            )?;
            Ok(os::line(&[
                b"memory: ",
                o.mode.as_bytes(),
                b" recorded in ",
                s.shared.as_os_str().as_bytes(),
            ]))
        }
        "redact" => {
            if std::env::var("OMS_HARNESS_CHILD").unwrap_or_else(|_| "0".into()) != "0"
                || std::env::var("OMS_HARNESS_DELEGATE_DEPTH").unwrap_or_else(|_| "0".into()) != "0"
            {
                return err(
                    "only the parent session may redact memory; return the request to the parent agent",
                );
            }
            if explicit {
                return err("redact does not accept --file");
            }
            workflows::redact(&s, &id, &author, o.has("--apply"), flag)
        }
        "compact" => {
            s.compact(flag)?;
            db::sync(&s, flag)?;
            Ok(os::line(&[
                b"memory: refreshed ",
                s.summary.as_os_str().as_bytes(),
            ]))
        }
        "context" => match s.context(o.has("--full"), flag) {
            Ok(body) => Ok((0, body.into_bytes(), Vec::new())),
            Err(Error(_, message)) => {
                s.warning(&format!("warning: {message}"));
                Ok((0, Vec::new(), Vec::new()))
            }
        },
        "search" | "recall" => {
            if note.is_empty() {
                return err(format!(
                    "{} requires --text {} (or a positional {})",
                    o.mode,
                    if o.mode == "search" {
                        "PATTERN"
                    } else {
                        "QUERY"
                    },
                    if o.mode == "search" {
                        "pattern"
                    } else {
                        "query"
                    }
                ));
            }
            db::query(
                &s,
                Query {
                    query: &note,
                    agent: &o.value("--agent"),
                    limit,
                    recall: o.mode == "recall",
                    json: o.has("--json"),
                    include_stale: o.has("--include-stale"),
                    include_superseded: o.has("--include-superseded"),
                    targets: &target,
                    drop_scoped: false,
                    handoffs: s
                        .canonical_repo()
                        .map(|r| r.join(".oms/handoffs"))
                        .as_deref(),
                },
                flag,
            )
        }
        "health" => diagnostics::health(&s, o.has("--json")),
        "sources" => {
            if global || explicit {
                return err("sources reports one repository; use --repo, not --global or --file");
            }
            diagnostics::sources(&s.repo, o.has("--json"))
        }
        "rebuild" => {
            db::rebuild(&s, flag)?;
            Ok(os::line(&[
                b"memory: rebuilt ",
                s.database.as_os_str().as_bytes(),
            ]))
        }
        _ => err(format!(
            "unknown command: {}\n{}",
            o.mode,
            include_str!("usage.txt")
        )),
    };
    s.with_warnings(result)
}
