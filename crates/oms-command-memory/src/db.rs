use crate::{
    records::{self, Entry},
    store::{self, Store},
    *,
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::atomic::AtomicBool,
};
pub(crate) const FIELDS: &str = "event_id, source, ordinal, occurred_at, agent, kind, task_id, session_hash, git_sha, git_dirty, git_state, body, rendered";
pub(crate) fn connect(path: &Path, readonly: bool) -> Result<Connection> {
    store::validate_owned(path)?;
    if !readonly && !path.exists() {
        return err("memory database: index file has not been initialized safely");
    }
    let flags = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let db = Connection::open_with_flags(
        path,
        flags | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    db.execute_batch(if readonly {
        "pragma query_only=on;"
    } else {
        "pragma foreign_keys=on; pragma synchronous=FULL;"
    })?;
    Ok(db)
}
fn schema(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row("pragma user_version", [], |r| r.get(0))?;
    if ![0, 1, 2, 3].contains(&version) {
        return err(format!(
            "memory database: unsupported memory database schema {version} (expected 3)"
        ));
    }
    if version == 1 || version == 2 {
        db.execute_batch("begin immediate; drop trigger if exists memory_entries_ai; drop trigger if exists memory_entries_ad; drop trigger if exists memory_entries_au; drop table if exists memory_fts; drop table if exists memory_entries; drop table if exists memory_sources; commit;")?;
    }
    db.execute_batch(include_str!("schema.sql"))?;
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub path: std::path::PathBuf,
    pub size: i64,
    pub mtime: i64,
    pub digest: String,
    pub bytes: Vec<u8>,
}
pub(crate) fn snapshot(path: &Path) -> Result<Snapshot> {
    if path.as_os_str().is_empty() {
        return Ok(Snapshot {
            path: std::path::PathBuf::new(),
            size: -1,
            mtime: 0,
            digest: String::new(),
            bytes: Vec::new(),
        });
    }
    for _ in 0..4 {
        store::validate_owned(path)?;
        let before = fs::symlink_metadata(path);
        let bytes = store::read(path)?;
        let after = fs::symlink_metadata(path);
        match (before, after) {
            (Err(a), Err(b))
                if a.kind() == std::io::ErrorKind::NotFound
                    && b.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(Snapshot {
                    path: path.to_owned(),
                    size: -1,
                    mtime: 0,
                    digest: String::new(),
                    bytes: Vec::new(),
                });
            }
            (Ok(a), Ok(b))
                if a.dev() == b.dev()
                    && a.ino() == b.ino()
                    && a.len() == b.len()
                    && a.len() == bytes.len() as u64
                    && a.mtime() == b.mtime()
                    && a.mtime_nsec() == b.mtime_nsec() =>
            {
                return Ok(Snapshot {
                    path: fs::canonicalize(path)?,
                    size: b.len() as i64,
                    mtime: b
                        .mtime()
                        .saturating_mul(1_000_000_000)
                        .saturating_add(b.mtime_nsec()),
                    digest: hash(&bytes),
                    bytes,
                });
            }
            _ => {}
        }
    }
    err("memory source kept changing while it was being indexed")
}
pub(crate) fn snapshots(s: &Store) -> Result<Vec<(&'static str, Snapshot, Vec<Entry>)>> {
    let mut out = Vec::new();
    for (source, path) in [
        ("shared", &s.shared),
        ("pins", &s.pins),
        ("failures", &s.failures),
    ] {
        let image = snapshot(path)?;
        let entries = if source == "failures" {
            records::failures(&image.bytes)
        } else {
            records::parse(&image.bytes, source)
        };
        out.push((source, image, entries));
    }
    Ok(out)
}
fn sync_db(db: &mut Connection, s: &Store, flag: &AtomicBool) -> Result<()> {
    schema(db)?;
    let sources = snapshots(s)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (source, image, entries) in sources {
        if source == "failures" && s.failures.as_os_str().is_empty() {
            continue;
        }
        check(flag)?;
        let current = tx
            .query_row(
                "select path,size,mtime_ns,digest from memory_sources where source=?",
                [source],
                |r| {
                    Ok((
                        r.get_ref(0)?.as_bytes()?.to_vec(),
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .ok();
        if current
            == Some((
                image.path.as_os_str().as_encoded_bytes().to_vec(),
                image.size,
                image.mtime,
                image.digest.clone(),
            ))
        {
            // Parser changes can invalidate derived identities without changing source bytes.
            let indexed = tx
                .prepare("select event_id from memory_entries where source=? order by ordinal")?
                .query_map([source], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if indexed
                .iter()
                .map(String::as_str)
                .eq(entries.iter().map(|e| e.event_id.as_str()))
            {
                continue;
            }
        }
        tx.execute("delete from memory_entries where source=?", [source])?;
        for e in entries {
            tx.execute("insert into memory_entries (event_id,source,ordinal,occurred_at,agent,kind,task_id,session_hash,git_sha,git_dirty,git_state,body,rendered) values (?,?,?,?,?,?,?,?,?,?,?,?,?)",params![e.event_id,e.source,e.ordinal as i64,e.occurred_at,e.agent,e.kind,e.task_id,e.session_hash,e.git_sha,e.git_dirty,e.git_state,e.body,e.rendered])?;
        }
        tx.execute("insert into memory_sources(source,path,size,mtime_ns,digest) values (?,cast(? as text),?,?,?) on conflict(source) do update set path=excluded.path,size=excluded.size,mtime_ns=excluded.mtime_ns,digest=excluded.digest",params![source,image.path.as_os_str().as_encoded_bytes(),image.size,image.mtime,image.digest])?;
    }
    tx.commit()?;
    Ok(())
}
fn initialize(s: &Store, flag: &AtomicBool) -> Result<()> {
    s.ignore(flag)?;
    if !s.database.exists() {
        store::mutate(&s.database, flag, |old| Ok(old.to_vec()))?;
    }
    Ok(())
}
pub(crate) fn sync(s: &Store, flag: &AtomicBool) -> Result<()> {
    let _g = store::lock(&s.database, flag)?;
    initialize(s, flag)?;
    let mut db = connect(&s.database, false)?;
    sync_db(&mut db, s, flag)
}
pub(crate) fn rebuild(s: &Store, flag: &AtomicBool) -> Result<()> {
    let _g = store::lock(&s.database, flag)?;
    rebuild_locked(s, flag)
}
pub(crate) fn rebuild_locked(s: &Store, flag: &AtomicBool) -> Result<()> {
    s.ignore(flag)?;
    let name = format!(
        ".oms-rebuild.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = s.database.parent().unwrap().join(name);
    store::mutate(&path, flag, |_| Ok(Vec::new()))?;
    let result = (|| {
        let mut db = connect(&path, false)?;
        sync_db(&mut db, s, flag)?;
        let integrity: String = db.query_row("pragma integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return err("rebuilt memory database failed its integrity check");
        }
        drop(db);
        let data = store::read(&path)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut sidecar = s.database.as_os_str().to_owned();
            sidecar.push(suffix);
            store::remove(Path::new(&sidecar))?;
        }
        store::mutate(&s.database, flag, |_| Ok(data))?;
        Ok(())
    })();
    let _ = fs::remove_file(&path);
    result
}
pub(crate) fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        event_id: r.get(0)?,
        source: r.get(1)?,
        ordinal: r.get::<_, i64>(2)? as usize,
        occurred_at: r.get(3)?,
        agent: r.get(4)?,
        kind: r.get(5)?,
        task_id: r.get(6)?,
        session_hash: r.get(7)?,
        git_sha: r.get(8)?,
        git_dirty: r.get(9)?,
        git_state: r.get(10)?,
        body: r.get(11)?,
        rendered: r.get(12)?,
        ..Default::default()
    })
}
pub(crate) fn all(db: &Connection, agent: &str) -> Result<Vec<Entry>> {
    let mut query=db.prepare(&format!("select {FIELDS} from memory_entries where (?='' or agent=?) order by case source when 'shared' then 0 else 1 end,ordinal"))?;
    Ok(query
        .query_map(params![agent, agent], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
pub(crate) fn receipts(db: &Connection) -> Result<HashMap<String, (String, i64)>> {
    let mut q = db.prepare("select event_id,last_accessed,hit_count from memory_access")?;
    Ok(
        q.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?,
    )
}
pub(crate) fn tokens(s: &str) -> Vec<String> {
    let re = regex::Regex::new(r"[^\W_]+").unwrap();
    let mut out = Vec::new();
    for m in re.find_iter(s) {
        let word = fold(m.as_str());
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out
}
pub(crate) fn ranked(db: &Connection, query: &str, agent: &str) -> Result<Vec<Entry>> {
    let tokens = tokens(query);
    let fts: i64 = db.query_row(
        "select count(*) from sqlite_master where type='table' and name='memory_fts'",
        [],
        |r| r.get(0),
    )?;
    let mut found = if !tokens.is_empty() && fts > 0 {
        let fields = FIELDS
            .split(", ")
            .map(|s| format!("entries.{s}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut q=db.prepare(&format!("select {fields} from memory_fts join memory_entries entries on entries.id=memory_fts.rowid where memory_fts match ? and (?='' or entries.agent=?) order by bm25(memory_fts),entries.occurred_at desc,entries.id desc"))?;
        let query = tokens
            .iter()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        q.query_map(params![query, agent, agent], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let repeated: Vec<_> = regex::Regex::new(r"[^\W_]+")
            .unwrap()
            .find_iter(query)
            .map(|m| fold(m.as_str()))
            .collect();
        let mut found = Vec::new();
        for e in all(db, agent)? {
            let body = fold(&e.rendered);
            let score: usize = repeated.iter().map(|t| body.matches(t).count()).sum();
            if score > 0 {
                found.push((score, e));
            }
        }
        found.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.occurred_at.cmp(&a.1.occurred_at))
                .then(b.1.ordinal.cmp(&a.1.ordinal))
        });
        found.into_iter().map(|(_, e)| e).collect()
    };
    let access = receipts(db).unwrap_or_default();
    let span = found.len() as f64;
    let nudge = (span / 4.0).max(1.0);
    let clock = timestamp(&now()).unwrap_or(0);
    let mut adjusted = Vec::new();
    for (pos, e) in found.drain(..).enumerate() {
        let mut score = span - pos as f64;
        let stats = access.get(&e.event_id);
        if stats
            .and_then(|(s, _)| timestamp(s))
            .is_some_and(|t| clock - t <= 7 * 86400)
        {
            score += nudge;
        }
        if stats.is_none() && timestamp(&e.occurred_at).is_some_and(|t| clock - t >= 45 * 86400) {
            score -= nudge;
        }
        adjusted.push((score, pos, e));
    }
    adjusted.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(a.2.occurred_at.cmp(&b.2.occurred_at))
            .then(a.1.cmp(&b.1))
    });
    Ok(adjusted.into_iter().map(|(_, _, e)| e).collect())
}
pub(crate) fn touch(db: &Connection, entries: &[Entry]) {
    let time = now();
    let _ = db.execute_batch("begin immediate");
    let result = (|| -> rusqlite::Result<()> {
        for e in entries {
            db.execute("insert into memory_access(event_id,last_accessed,hit_count) values (?,?,1) on conflict(event_id) do update set last_accessed=excluded.last_accessed,hit_count=memory_access.hit_count+1",params![e.event_id,time])?;
        }
        Ok(())
    })();
    let _ = db.execute_batch(if result.is_ok() { "commit" } else { "rollback" });
}
pub(crate) struct Query<'a> {
    pub query: &'a str,
    pub agent: &'a str,
    pub limit: usize,
    pub recall: bool,
    pub json: bool,
    pub include_stale: bool,
    pub include_superseded: bool,
    pub targets: &'a str,
    pub drop_scoped: bool,
    pub context_only: bool,
    pub handoffs: Option<&'a Path>,
    /// GitHub project mode sources; injected provider context keeps it off.
    pub github: bool,
}
pub(crate) fn query(s: &Store, opt: Query<'_>, flag: &AtomicBool) -> Result<Output> {
    let _g = store::lock(&s.database, flag)?;
    initialize(s, flag)?;
    let mut db = connect(&s.database, false)?;
    sync_db(&mut db, s, flag)?;
    let (notes, pins) = s.logs()?;
    let status = records::states(&notes, &pins);
    let eligible = if opt.context_only {
        Some(crate::review::context_filter(s)?)
    } else {
        None
    };
    let by_id: HashMap<_, _> = notes
        .iter()
        .chain(&pins)
        .map(|e| (e.event_id.as_str(), e))
        .collect();
    let mut entries = if opt.recall {
        ranked(&db, opt.query, opt.agent)?
    } else {
        all(&db, opt.agent)?
            .into_iter()
            .filter(|e| fold(&e.rendered).contains(&fold(opt.query)))
            .collect()
    };
    entries.retain(|e| {
        e.kind != "retirement" && (opt.include_superseded || !status.contains_key(&e.event_id))
    });
    let mut cache = BTreeMap::new();
    let mut validation = HashMap::new();
    let mut chosen = Vec::new();
    let mut rest = Vec::new();
    for e in entries {
        check(flag)?;
        let provenance = by_id.get(e.event_id.as_str()).copied();
        let memory_source = matches!(e.source.as_str(), "shared" | "pins");
        if opt.context_only && memory_source && provenance.is_none() {
            continue;
        }
        let provenance = provenance.unwrap_or(&e);
        if memory_source
            && eligible
                .as_ref()
                .is_some_and(|eligible| !eligible(provenance))
        {
            continue;
        }
        if let Some(citation) = store::validate_citation(provenance, &s.repo, &mut cache, flag) {
            let stale = citation["status"] == "stale";
            validation.insert(e.event_id.clone(), citation);
            if stale && !opt.include_stale {
                continue;
            }
        }
        let scopes = provenance.field("paths");
        if opt.recall && !scopes.is_empty() {
            let targets: Vec<_> = opt.targets.split(',').filter(|s| !s.is_empty()).collect();
            if !targets.is_empty() {
                if scopes.split(',').any(|glob| {
                    targets
                        .iter()
                        .any(|target| records::scope_match(glob, target.trim_start_matches("./")))
                }) {
                    chosen.push(e);
                }
                continue;
            }
            if opt.drop_scoped {
                continue;
            }
        }
        rest.push(e);
    }
    chosen.extend(rest);
    if opt.limit > 0 {
        chosen.truncate(opt.limit);
    }
    if opt.recall {
        touch(&db, &chosen);
    }
    let mut out = Vec::new();
    for e in &chosen {
        let reason = if opt.recall {
            "query relevance with access decay; citation status reported when available"
        } else {
            ""
        };
        if opt.json {
            let mut row = e.json();
            row["schema"] = 3.into();
            if !reason.is_empty() {
                row["selection_reason"] = reason.into();
            }
            if let Some(c) = validation.get(&e.event_id) {
                row["citation"] = c.clone();
            }
            if let Some(st) = status.get(&e.event_id) {
                row["supersession"] = st.clone();
            }
            let paths = by_id
                .get(e.event_id.as_str())
                .and_then(|p| p.raw_source_path.as_ref())
                .map(|p| vec![("/citation/path".into(), p.clone())])
                .unwrap_or_default();
            out.extend(os::json_line(&row, &paths)?);
        } else {
            out.extend(format!("{}\n\n", e.rendered.trim_end()).as_bytes());
            if let Some(st) = status.get(&e.event_id) {
                out.extend(format!("[memory: {}]\n\n", text(st, "status")).as_bytes());
            }
            if !reason.is_empty() {
                out.extend(
                    format!(
                        "[selection: {reason}; source={}; event={}; recorded={}]\n\n",
                        e.source, e.event_id, e.occurred_at
                    )
                    .as_bytes(),
                );
            }
            if let Some(c) = validation.get(&e.event_id) {
                out.extend(b"[source: ");
                if let Some(entry) = by_id.get(e.event_id.as_str()) {
                    out.extend(entry.source_path().as_encoded_bytes());
                } else {
                    out.extend(text(c, "path").as_bytes());
                }
                out.extend(
                    format!(
                        ":{}; {}]\n\n",
                        c.get("current_line").unwrap_or(&c["line"]),
                        text(c, "status")
                    )
                    .as_bytes(),
                );
            }
        }
    }
    let mut count = chosen.len();
    if let Some(dir) = opt.handoffs.filter(|_| opt.recall) {
        let (extra, n) = handoffs(dir, opt.query, opt.json)?;
        out.extend(extra);
        count += n;
    }
    if opt.github && opt.recall {
        drop(_g);
        let (extra, n) = crate::github::recall(s, opt.query, opt.json, flag)?;
        out.extend(extra);
        count += n;
    }
    let diagnostic = if opt.recall {
        format!(
            "memory: {count} recalled entr{} for \"{}\"\n",
            if count == 1 { "y" } else { "ies" },
            opt.query
        )
    } else {
        format!("memory: {count} match(es) for \"{}\"\n", opt.query)
    };
    Ok((if count == 0 { 1 } else { 0 }, out, diagnostic.into_bytes()))
}
pub(crate) fn handoffs(dir: &Path, query: &str, as_json: bool) -> Result<(Vec<u8>, usize)> {
    let file_re = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,200}\.md$").unwrap();
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for f in entries.flatten() {
            let filename = f.file_name();
            let Some(name) = filename.to_str().map(str::to_owned) else {
                continue;
            };
            if !file_re.is_match(&name) {
                continue;
            }
            if let Ok(m) = f.path().symlink_metadata()
                && m.is_file()
                && !m.file_type().is_symlink()
            {
                files.push((m.mtime(), name, f.path()));
            }
        }
    }
    files.sort_by(|a, b| b.cmp(a));
    files.truncate(200);
    let words = tokens(query);
    let mut ranked = Vec::new();
    for (mtime, name, path) in files {
        let Ok(raw) = store::read_prefix(&path, 8192) else {
            continue;
        };
        let head = String::from_utf8_lossy(&raw[..raw.len().min(8192)]);
        let mut captured = String::new();
        let mut agent = String::new();
        let mut request = String::new();
        let mut in_request = false;
        let mut kept = Vec::new();
        for line in head.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("- captured:") {
                captured = clip(v.trim(), 40);
            }
            if let Some(v) = line.strip_prefix("- agent:") {
                agent = clip(v.trim(), 40);
            }
            if [
                "- cwd:",
                "- source:",
                "- verification_observed_at:",
                "- assistant_summary:",
            ]
            .iter()
            .any(|p| line.starts_with(p))
            {
                continue;
            }
            if line.starts_with("## ") {
                in_request = line.starts_with("## Original request");
            } else if in_request && !line.is_empty() && request.is_empty() {
                request = clip(&flat(line), 160);
            }
            kept.push(line);
        }
        let folded = fold(&kept.join("\n"));
        let score: usize = words.iter().map(|w| folded.matches(w).count()).sum();
        if score > 0 {
            ranked.push((score, mtime, name, captured, agent, request));
        }
    }
    ranked.sort_by(|a, b| b.cmp(a));
    ranked.truncate(3);
    let mut out = Vec::new();
    for (_, _, name, when, agent, request) in &ranked {
        let withheld = !request.is_empty() && !store::clean(request, false);
        let body = if withheld { "" } else { request };
        let id = format!("handoff:{name}");
        let open = format!("oms session-handoff show {name}");
        if as_json {
            out.extend(json_line(&json!({"schema":3,"event_id":id,"source":"handoffs","kind":"handoff","occurred_at":when,"agent":agent,"body":body,"withheld":withheld,"open":open}))?);
        } else {
            out.extend(format!("## {} {}\n\nhandoff digest: {}\nopen: {open}\n\n[selection: session handoff whose head matches the query; a pointer, not the digest; event={id}]\n\n",if when.is_empty(){"unknown-time"}else{when},if agent.is_empty(){"agent"}else{agent},if withheld{"[withheld: sensitive-looking content]"}else if body.is_empty(){"(no request recorded)"}else{body}).as_bytes());
        }
    }
    Ok((out, ranked.len()))
}
