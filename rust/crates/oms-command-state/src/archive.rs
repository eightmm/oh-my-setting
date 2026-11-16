//! Cold artifact store: `.oms/archive.sqlite3` holds compacted peer, review, prompt, handoff and
//! task-archive files as SHA-256 content-addressed blobs with FTS5 search. Originals are unlinked
//! only after the SQLite commit and a complete WAL checkpoint, and never when referenced.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, error, helpers, json_output, require, storage, success,
};
use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(crate) const HELP: &str = "Usage: oms state compact [--repo PATH] [--keep-recent N] [--dry-run] [--json]\n       oms state archive list [--repo PATH] [--kind K] [--limit N] [--json]\n       oms state archive show PATH [--repo PATH] [--json]\n       oms state archive search QUERY [--repo PATH] [--limit N] [--json]\n       oms state archive stats [--repo PATH] [--json]\nCold artifacts move into .oms/archive.sqlite3 only after commit; referenced files stay on disk.\n";

const DB: &str = ".oms/archive.sqlite3";
const SCAN_LIMIT: usize = 50000;
const DEFAULT_KEEP: usize = 20;

/// (kind, directory, recursive, accepted extensions; empty accepts all).
const FAMILIES: &[(&str, &str, bool, &[&str])] = &[
    ("peer", ".oms/peer", false, &["md", "patch", "json"]),
    ("review", ".oms/reviews", true, &[]),
    ("prompt", ".oms/prompts", true, &[]),
    ("handoff", ".oms/handoffs", false, &["md"]),
    ("task-archive", ".oms/task/archive", false, &["md"]),
];

fn db<T>(result: rusqlite::Result<T>) -> Result<T> {
    result.map_err(|e| error(1, format!("archive store: {e}")))
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn db_path(repo: &Path) -> PathBuf {
    repo.join(DB)
}

fn real_oms_dir(repo: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(repo.join(".oms")).map_err(|e| error(2, e))?;
    require(
        meta.is_dir() && !meta.is_symlink(),
        ".oms must be a non-symlink directory",
    )
}

fn private_files(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        match std::fs::set_permissions(PathBuf::from(name), std::fs::Permissions::from_mode(0o600))
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(2, e)),
        }
    }
    Ok(())
}

fn open_rw(repo: &Path) -> Result<Connection> {
    real_oms_dir(repo)?;
    let path = db_path(repo);
    match std::fs::symlink_metadata(&path) {
        Ok(m) => require(
            m.is_file() && !m.is_symlink(),
            "archive store must be a regular file",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(error(2, e)),
            }
        }
        Err(e) => return Err(error(2, e)),
    }
    let conn = db(Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ))?;
    db(conn.busy_timeout(std::time::Duration::from_secs(5)))?;
    db(conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)))?;
    db(conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS blobs(sha256 TEXT PRIMARY KEY, bytes INTEGER NOT NULL, content BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS entries(path TEXT PRIMARY KEY, kind TEXT NOT NULL, sha256 TEXT NOT NULL, mtime_sec INTEGER NOT NULL, bytes INTEGER NOT NULL, archived_at TEXT NOT NULL);
         CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(path, kind, body, tokenize='unicode61');",
    ))?;
    private_files(&path)?;
    Ok(conn)
}

/// Read-only commands never create the store; `None` means nothing was ever archived.
fn open_existing(repo: &Path) -> Result<Option<Connection>> {
    let path = db_path(repo);
    match std::fs::symlink_metadata(&path) {
        Ok(m) => require(
            m.is_file() && !m.is_symlink(),
            "archive store must be a regular file",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(2, e)),
    }
    let conn = db(Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ))?;
    db(conn.busy_timeout(std::time::Duration::from_secs(5)))?;
    Ok(Some(conn))
}

struct Candidate {
    rel: String,
    abs: PathBuf,
    kind: &'static str,
    mtime: i64,
    meta: std::fs::Metadata,
}

#[derive(Default)]
pub(crate) struct Report {
    pub kept: usize,
    pub archived: usize,
    pub bytes_freed: u64,
    pub dedup_hits: usize,
    pub skipped_referenced: usize,
    pub skipped_other: usize,
    pub unlink_failed: usize,
    /// (repo-relative path, family kind, sha256, bytes)
    pub entries: Vec<(String, &'static str, String, u64)>,
}

/// Names of every artifact mentioned by live state. Basenames are matched, which over-retains
/// rather than ever unlinking something a task, delegation, operation or receipt still names.
fn referenced_text(ctx: &Context<'_>) -> Result<String> {
    let repo = &ctx.repo;
    let mut paths = vec![
        repo.join(".oms/plan/tasks.json"),
        repo.join(".oms/lifecycle/events.jsonl"),
        repo.join(".oms/artifacts/index.jsonl"),
        repo.join(".oms/landings.jsonl"),
    ];
    for (dir, suffix) in [
        (".oms/delegations", ".json"),
        (".oms/plan/completion-prefixes", ".json"),
    ] {
        paths.extend(helpers::files(ctx, &repo.join(dir), suffix, SCAN_LIMIT)?);
    }
    if let Ok(entries) = std::fs::read_dir(repo.join(".oms")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("operations") && name.ends_with(".jsonl") {
                paths.push(entry.path());
            }
        }
    }
    let mut text = String::new();
    for path in paths {
        crate::checkpoint(ctx.interrupted)?;
        let raw = storage::read(repo, &path, storage::MAX_STREAM, true)?;
        text.push_str(&String::from_utf8_lossy(&raw));
        text.push('\n');
    }
    Ok(text)
}

fn candidates(ctx: &Context<'_>) -> Result<BTreeMap<&'static str, Vec<Candidate>>> {
    let mut families = BTreeMap::new();
    for (kind, dir, recursive, exts) in FAMILIES {
        let root = ctx.repo.join(dir);
        let mut found = Vec::new();
        for abs in helpers::files(ctx, &root, "", SCAN_LIMIT)? {
            if !recursive && abs.parent() != Some(root.as_path()) {
                continue;
            }
            let name = abs.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let stem = abs.file_stem().and_then(|n| n.to_str()).unwrap_or("");
            let ext = abs.extension().and_then(|n| n.to_str()).unwrap_or("");
            if name.is_empty()
                || name.starts_with('.')
                || name.contains(".tmp")
                || (!exts.is_empty() && !exts.contains(&ext))
                || (*kind == "handoff" && ["latest", "current", "active"].contains(&stem))
            {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&abs) else {
                continue;
            };
            if !meta.is_file() || meta.is_symlink() {
                continue;
            }
            let Ok(rel) = abs.strip_prefix(&ctx.repo) else {
                continue;
            };
            found.push(Candidate {
                rel: rel.to_string_lossy().into_owned(),
                abs,
                kind,
                mtime: meta.mtime(),
                meta,
            });
        }
        found.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.rel.cmp(&b.rel)));
        families.insert(*kind, found);
    }
    Ok(families)
}

/// Archive each family's files beyond the `keep_recent` newest (and, with `older_than`, only those
/// whose mtime is before it). `apply == false` plans without touching SQLite or the originals.
pub(crate) fn compact(
    ctx: &Context<'_>,
    keep_recent: usize,
    older_than: Option<i64>,
    apply: bool,
) -> Result<Report> {
    let mut report = Report::default();
    if !ctx.repo.join(".oms").is_dir() {
        return Ok(report);
    }
    let refs = referenced_text(ctx)?;
    let mut selected = Vec::new();
    for (_, files) in candidates(ctx)? {
        for (index, file) in files.into_iter().enumerate() {
            if index < keep_recent || older_than.is_some_and(|cutoff| file.mtime >= cutoff) {
                report.kept += 1;
                continue;
            }
            let name = file.abs.file_name().unwrap_or_default().to_string_lossy();
            if refs.contains(name.as_ref()) {
                report.skipped_referenced += 1;
                continue;
            }
            selected.push(file);
        }
    }
    if selected.is_empty() {
        return Ok(report);
    }
    let _guard = storage::lock(&db_path(&ctx.repo))?;
    let conn = if apply {
        Some(open_rw(&ctx.repo)?)
    } else {
        open_existing(&ctx.repo)?
    };
    let mut loaded = Vec::new();
    let mut seen = BTreeSet::new();
    for file in selected {
        crate::checkpoint(ctx.interrupted)?;
        let Ok(bytes) = storage::input(&file.abs, storage::MAX_FILE) else {
            report.skipped_other += 1;
            continue;
        };
        let sha = hex(&bytes);
        let stored = conn.as_ref().is_some_and(|c| {
            c.query_row("SELECT 1 FROM blobs WHERE sha256=?1", [&sha], |_| Ok(()))
                .is_ok()
        });
        if stored || !seen.insert(sha.clone()) {
            report.dedup_hits += 1;
        }
        report
            .entries
            .push((file.rel.clone(), file.kind, sha.clone(), bytes.len() as u64));
        loaded.push((file, sha, bytes));
    }
    let Some(mut conn) = conn.filter(|_| apply) else {
        report.archived = loaded.len();
        report.bytes_freed = loaded.iter().map(|(_, _, b)| b.len() as u64).sum();
        return Ok(report);
    };
    let at = crate::timestamp(crate::epoch())?;
    let tx = db(conn.transaction())?;
    let mut replaced = false;
    for (file, sha, bytes) in &loaded {
        db(tx.execute(
            "INSERT OR IGNORE INTO blobs(sha256, bytes, content) VALUES(?1, ?2, ?3)",
            params![sha, bytes.len() as i64, bytes],
        ))?;
        let existed = tx
            .query_row("SELECT 1 FROM entries WHERE path=?1", [&file.rel], |_| {
                Ok(())
            })
            .is_ok();
        if existed {
            replaced = true;
            db(tx.execute("DELETE FROM entries_fts WHERE path=?1", [&file.rel]))?;
        }
        db(tx.execute(
            "INSERT OR REPLACE INTO entries(path, kind, sha256, mtime_sec, bytes, archived_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![file.rel, file.kind, sha, file.mtime, bytes.len() as i64, at],
        ))?;
        db(tx.execute(
            "INSERT INTO entries_fts(path, kind, body) VALUES(?1, ?2, ?3)",
            params![file.rel, file.kind, String::from_utf8_lossy(bytes)],
        ))?;
    }
    if replaced {
        db(tx.execute(
            "DELETE FROM blobs WHERE sha256 NOT IN (SELECT sha256 FROM entries)",
            [],
        ))?;
    }
    db(tx.commit())?;
    let (busy, log, done) = db(conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    }))?;
    private_files(&db_path(&ctx.repo))?;
    report.archived = loaded.len();
    if busy != 0 || log != done {
        // Committed but not fully checkpointed: retain every original rather than trust the WAL.
        report.unlink_failed = loaded.len();
        return Ok(report);
    }
    for (file, _, bytes) in &loaded {
        match storage::remove(&ctx.repo, &file.abs, &file.meta) {
            Ok(()) => report.bytes_freed += bytes.len() as u64,
            Err(_) => report.unlink_failed += 1,
        }
    }
    Ok(report)
}

fn entry_rows(report: &Report) -> Value {
    Value::Array(
        report
            .entries
            .iter()
            .map(|(path, kind, sha, bytes)| {
                object([
                    ("path", string(path)),
                    ("kind", string(kind)),
                    ("sha256", string(sha)),
                    ("bytes", number(bytes)),
                ])
            })
            .collect(),
    )
}

fn run_compact(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let opts = Options::parse(args, &["repo", "keep-recent"], &["dry-run", "json"])?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(opts.pos.is_empty(), "unexpected positional argument")?;
    let ctx = scoped(ctx, &opts)?;
    let dry = opts.flag("dry-run");
    let report = compact(&ctx, opts.number("keep-recent", DEFAULT_KEEP)?, None, !dry)?;
    if opts.flag("json") {
        return json_output(
            0,
            &object([
                ("schema", number(1)),
                ("dry_run", Value::Bool(dry)),
                ("kept", number(report.kept)),
                ("archived", number(report.archived)),
                ("bytes_freed", number(report.bytes_freed)),
                ("dedup_hits", number(report.dedup_hits)),
                ("skipped_referenced", number(report.skipped_referenced)),
                ("skipped_other", number(report.skipped_other)),
                ("unlink_failed", number(report.unlink_failed)),
                ("db_path", string(DB)),
                ("entries", entry_rows(&report)),
            ]),
        );
    }
    let mut text = format!(
        "compact{}: {} archived, {} kept, {} referenced skipped, {} dedup hits, {} bytes freed -> {DB}\n",
        if dry { " (dry run)" } else { "" },
        report.archived,
        report.kept,
        report.skipped_referenced,
        report.dedup_hits,
        report.bytes_freed
    );
    for (path, kind, _, bytes) in &report.entries {
        text.push_str(&format!("  {kind} {path} ({bytes} bytes)\n"));
    }
    Ok(success(text))
}

fn scoped<'a>(ctx: &Context<'a>, opts: &Options) -> Result<Context<'a>> {
    let repo = opts.repo(ctx)?;
    Ok(Context {
        runtime: ctx.runtime,
        cwd: repo.clone(),
        repo: repo.clone(),
        state_repo: repo,
        interrupted: ctx.interrupted,
    })
}

/// FTS5 syntax is never exposed: each whitespace token becomes a quoted phrase, so `peer-review`
/// or a dotted path matches its tokens instead of raising a syntax error.
fn fts_query(query: &str) -> Option<String> {
    let phrases: Vec<String> = query
        .split_whitespace()
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!phrases.is_empty()).then(|| phrases.join(" "))
}

fn table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='entries_fts'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

fn lines(rows: &[Value], fields: &[&str]) -> String {
    rows.iter()
        .map(|row| {
            let mut line = fields
                .iter()
                .map(|f| match row.get(f) {
                    Value::Number(n) => n.clone(),
                    _ => row.text(f).to_owned(),
                })
                .collect::<Vec<_>>()
                .join("  ");
            line.push('\n');
            line
        })
        .collect()
}

fn run_archive(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let Some(sub) = args.first().and_then(|v| v.to_str()) else {
        return Ok(success(HELP));
    };
    let (values, flags): (&[&str], &[&str]) = match sub {
        "list" => (&["repo", "kind", "limit"], &["json"]),
        "search" => (&["repo", "limit"], &["json"]),
        "show" | "stats" => (&["repo"], &["json"]),
        "-h" | "--help" | "help" => return Ok(success(HELP)),
        _ => return Err(error(2, "unknown archive action")),
    };
    let opts = Options::parse(&args[1..], values, flags)?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    let ctx = scoped(ctx, &opts)?;
    let json = opts.flag("json");
    let conn = open_existing(&ctx.repo)?.filter(table_exists);
    match sub {
        "list" => {
            require(opts.pos.is_empty(), "unexpected positional argument")?;
            let limit = opts.number("limit", 50)?;
            let mut rows = Vec::new();
            if let Some(conn) = &conn {
                let mut stmt = db(conn.prepare(
                    "SELECT path, kind, sha256, mtime_sec, bytes, archived_at FROM entries WHERE (?1 = '' OR kind = ?1) ORDER BY mtime_sec DESC, path ASC LIMIT ?2",
                ))?;
                let mapped = db(
                    stmt.query_map(params![opts.get("kind"), limit as i64], |r| {
                        Ok(object([
                            ("path", string(r.get::<_, String>(0)?)),
                            ("kind", string(r.get::<_, String>(1)?)),
                            ("sha256", string(r.get::<_, String>(2)?)),
                            ("mtime_sec", number(r.get::<_, i64>(3)?)),
                            ("bytes", number(r.get::<_, i64>(4)?)),
                            ("archived_at", string(r.get::<_, String>(5)?)),
                        ]))
                    }),
                )?;
                for row in mapped {
                    rows.push(db(row)?);
                }
            }
            if json {
                return json_output(
                    0,
                    &object([("schema", number(1)), ("entries", Value::Array(rows))]),
                );
            }
            Ok(success(lines(&rows, &["kind", "bytes", "path"])))
        }
        "show" => {
            require(opts.pos.len() == 1, "show requires one PATH")?;
            let given = Path::new(&opts.pos[0]);
            let rel = given.strip_prefix(&ctx.repo).unwrap_or(given);
            let rel = rel.to_string_lossy();
            let Some(conn) = &conn else {
                return Err(error(1, "archive is empty"));
            };
            let row = conn.query_row(
                "SELECT e.kind, e.sha256, e.mtime_sec, e.bytes, e.archived_at, b.content FROM entries e JOIN blobs b USING(sha256) WHERE e.path = ?1",
                [rel.as_ref()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                    ))
                },
            );
            let (kind, sha, mtime, bytes, at, content) = match row {
                Ok(row) => row,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(error(1, format!("not archived: {rel}")));
                }
                Err(e) => return Err(error(1, format!("archive store: {e}"))),
            };
            require(
                hex(&content) == sha && content.len() as i64 == bytes,
                "archived blob failed its integrity check",
            )?;
            if !json {
                return Ok((0, content, Vec::new()));
            }
            let text = std::str::from_utf8(&content).ok();
            json_output(
                0,
                &object([
                    ("schema", number(1)),
                    ("path", string(rel)),
                    ("kind", string(kind)),
                    ("sha256", string(sha)),
                    ("mtime_sec", number(mtime)),
                    ("bytes", number(bytes)),
                    ("archived_at", string(at)),
                    ("content", text.map_or(Value::Null, string)),
                ]),
            )
        }
        "search" => {
            require(!opts.pos.is_empty(), "search requires a QUERY")?;
            let limit = opts.number("limit", 20)?;
            let mut rows = Vec::new();
            if let (Some(conn), Some(query)) = (&conn, fts_query(&opts.pos.join(" "))) {
                let mut stmt = db(conn.prepare(
                    "SELECT path, kind, snippet(entries_fts, 2, '[', ']', '...', 16) FROM entries_fts WHERE entries_fts MATCH ?1 ORDER BY rank LIMIT ?2",
                ))?;
                let mapped = db(stmt.query_map(params![query, limit as i64], |r| {
                    Ok(object([
                        ("path", string(r.get::<_, String>(0)?)),
                        ("kind", string(r.get::<_, String>(1)?)),
                        ("excerpt", string(r.get::<_, String>(2)?.replace('\n', " "))),
                    ]))
                }))?;
                for row in mapped {
                    rows.push(db(row)?);
                }
            }
            if json {
                return json_output(
                    0,
                    &object([("schema", number(1)), ("results", Value::Array(rows))]),
                );
            }
            Ok(success(lines(&rows, &["kind", "path", "excerpt"])))
        }
        _ => {
            require(opts.pos.is_empty(), "unexpected positional argument")?;
            let mut by_kind = crate::json::empty();
            let (mut entries, mut blobs, mut raw, mut dedup) = (0i64, 0i64, 0i64, 0i64);
            if let Some(conn) = &conn {
                (entries, raw) = db(conn.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM entries",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ))?;
                (blobs, dedup) = db(conn.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM blobs",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ))?;
                let mut stmt = db(
                    conn.prepare("SELECT kind, COUNT(*) FROM entries GROUP BY kind ORDER BY kind")
                )?;
                let mapped =
                    db(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))))?;
                for row in mapped {
                    let (kind, count) = db(row)?;
                    by_kind.set(&kind, number(count))?;
                }
            }
            let db_bytes: u64 = ["", "-wal"]
                .iter()
                .filter_map(|s| {
                    let mut name = db_path(&ctx.repo).into_os_string();
                    name.push(s);
                    std::fs::metadata(name).ok().map(|m| m.len())
                })
                .sum();
            if json {
                return json_output(
                    0,
                    &object([
                        ("schema", number(1)),
                        ("entries", number(entries)),
                        ("blobs", number(blobs)),
                        ("raw_bytes", number(raw)),
                        ("dedup_bytes", number(dedup)),
                        ("entries_by_kind", by_kind),
                        ("db_bytes", number(db_bytes)),
                    ]),
                );
            }
            Ok(success(format!(
                "entries: {entries}\nblobs: {blobs}\nraw_bytes: {raw}\ndedup_bytes: {dedup}\ndb_bytes: {db_bytes}\n"
            )))
        }
    }
}

pub(crate) fn run(action: &str, args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    match action {
        "compact" => run_compact(args, ctx),
        _ => run_archive(args, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct Repo(PathBuf);
    impl Repo {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "oms-archive-test-{tag}-{}-{}",
                std::process::id(),
                crate::epoch()
            ));
            std::fs::create_dir_all(root.join(".oms")).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn write(&self, rel: &str, body: &[u8], age: u64) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_times(
                std::fs::FileTimes::new().set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(age),
                ),
            )
            .unwrap();
            path
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn call(repo: &Repo, action: &str, args: &[&str]) -> Output {
        let flag = AtomicBool::new(false);
        let ctx = Context {
            runtime: None,
            cwd: repo.0.clone(),
            repo: repo.0.clone(),
            state_repo: repo.0.clone(),
            interrupted: &flag,
        };
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        match run(action, &args, &ctx) {
            Ok(out) => out,
            Err(e) => panic!("{action} {args:?} failed: {}", e.message),
        }
    }

    fn json_of(out: &Output) -> Value {
        crate::json::parse(&out.1).unwrap()
    }

    #[test]
    fn dedups_blobs_searches_safely_and_restores_exact_bytes() {
        let repo = Repo::new("cas");
        let body = b"peer-review verdict for alpha.beta\nline two\xE2\x9C\x93\n";
        repo.write(".oms/peer/a.md", body, 300);
        repo.write(".oms/peer/b.md", body, 200);
        repo.write(".oms/peer/c.json", b"{\"other\":true}", 100);
        let out = call(&repo, "compact", &["--keep-recent", "0", "--json"]);
        let report = json_of(&out);
        assert_eq!(report.get("archived").int().unwrap(), 3);
        assert_eq!(report.get("dedup_hits").int().unwrap(), 1);
        assert!(!repo.0.join(".oms/peer/a.md").exists());
        let mode = std::fs::metadata(repo.0.join(DB))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let stats = json_of(&call(&repo, "archive", &["stats", "--json"]));
        assert_eq!(stats.get("entries").int().unwrap(), 3);
        assert_eq!(stats.get("blobs").int().unwrap(), 2);
        assert!(stats.get("raw_bytes").int().unwrap() > stats.get("dedup_bytes").int().unwrap());
        for query in [
            "peer-review",
            "alpha.beta",
            ".oms/peer/a.md",
            "say \"hi\" AND (",
            "!!!",
        ] {
            let found = json_of(&call(&repo, "archive", &["search", query, "--json"]));
            let n = found.get("results").array().unwrap().len();
            assert_eq!(
                n > 0,
                query != "say \"hi\" AND (" && query != "!!!",
                "{query}: {n}"
            );
        }
        let shown = call(&repo, "archive", &["show", ".oms/peer/b.md"]);
        assert_eq!(shown.1, body);
        let listed = json_of(&call(
            &repo,
            "archive",
            &["list", "--kind", "peer", "--json"],
        ));
        assert_eq!(
            listed.get("entries").array().unwrap()[0].text("path"),
            ".oms/peer/c.json"
        );
    }

    #[test]
    fn dry_run_is_inert_and_references_and_recent_files_stay() {
        let repo = Repo::new("safety");
        let newest = repo.write(".oms/reviews/2026/new.md", b"new", 10);
        let old = repo.write(".oms/reviews/2026/old.md", b"old", 500);
        let pinned = repo.write(".oms/prompts/pinned-prompt.md", b"pinned", 900);
        repo.write(".oms/prompts/fresh.md", b"fresh", 5);
        let prefix = repo.write(".oms/plan/completion-prefixes/x.json", b"{}", 900);
        let latest = repo.write(".oms/handoffs/latest.md", b"latest", 900);
        let temp = repo.write(".oms/peer/r.md.tmp", b"tmp", 900);
        std::fs::write(
            repo.0.join(".oms/plan/tasks.json"),
            b"{\"tasks\":{\"t\":{\"artifact\":\".oms/prompts/pinned-prompt.md\"}}}",
        )
        .unwrap();
        let outside = repo.write("outside.md", b"secret", 900);
        std::os::unix::fs::symlink(&outside, repo.0.join(".oms/peer/link.md")).unwrap();

        let dry = json_of(&call(
            &repo,
            "compact",
            &["--keep-recent", "1", "--dry-run", "--json"],
        ));
        assert!(dry.get("dry_run").truth());
        assert_eq!(dry.get("archived").int().unwrap(), 1);
        assert!(old.exists() && !repo.0.join(DB).exists());

        let real = json_of(&call(&repo, "compact", &["--keep-recent", "1", "--json"]));
        assert_eq!(real.get("archived").int().unwrap(), 1);
        assert_eq!(real.get("skipped_referenced").int().unwrap(), 1);
        assert!(!old.exists());
        for kept in [&newest, &pinned, &prefix, &latest, &temp, &outside] {
            assert!(kept.exists(), "{kept:?}");
        }
        assert!(std::fs::symlink_metadata(repo.0.join(".oms/peer/link.md")).is_ok());
    }

    #[test]
    fn gc_apply_archives_aged_cold_files_before_removal() {
        let repo = Repo::new("gc");
        let aged = repo.write(".oms/handoffs/h1.md", b"handoff body", 40 * 86400);
        let fresh = repo.write(".oms/handoffs/h2.md", b"fresh", 10);
        let flag = AtomicBool::new(false);
        let args: Vec<OsString> = ["--days", "30", "--apply", "--json"]
            .iter()
            .map(OsString::from)
            .collect();
        let out = crate::run_with_host(
            "gc",
            &args,
            &repo.0,
            &flag,
            &mut crate::NativeServices {
                source: &repo.0,
                interrupted: &flag,
            },
            &crate::NoHostServices,
        );
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        let report = crate::json::parse(&out.1).unwrap();
        assert!(
            report
                .get("items")
                .array()
                .unwrap()
                .iter()
                .any(|i| i.text("kind") == "archived-cold"
                    && i.text("path") == ".oms/handoffs/h1.md")
        );
        assert!(!aged.exists() && fresh.exists());
        let shown = call(&repo, "archive", &["show", ".oms/handoffs/h1.md"]);
        assert_eq!(shown.1, b"handoff body");
    }
}
