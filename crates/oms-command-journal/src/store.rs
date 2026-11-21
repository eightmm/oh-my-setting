#![forbid(unsafe_code)]

use crate::cold::{ensure_regular_dir_no_symlink, read_regular_no_symlink_bytes};
use crate::config::{journal_language, notion_repo_excluded, notion_settings};
use crate::json::{JsonExt, Map, MapExt, Value};
use crate::render::{compute_period_aggregates, has_blocker, render_daily, render_weekly};
use crate::sanitize::{
    INDEX_SCHEMA_VERSION, RENDERER_VERSION, canonical_json_bytes, pretty_json_bytes, sanitize_text,
    sha256_hex,
};
use crate::schema::{
    ProjectIdentity, atomic_write_bytes, ensure_oms_gitignore, normalize_event,
    resolve_project_identity, validate_canonical_event, work_journal_dir,
};
use crate::tz::{
    TimeZoneSpec, iso_week_range, naive_date_to_string, parse_date_ymd, parse_rfc3339_utc,
    utc_rfc3339,
};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension as _, params};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryAppendOutcome {
    Appended,
    Refused(String),
}

pub type MemoryAppendFn<'a> = &'a dyn Fn(&Path, &str, &str) -> Result<MemoryAppendOutcome, String>;

fn event_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^wj_[0-9a-f]{32}$").expect("valid regex"))
}

struct SourceSnapshot(crate::cold::ColdManifest, Vec<Value>, Option<Vec<u8>>);
impl Clone for SourceSnapshot {
    fn clone(&self) -> Self {
        Self(
            self.0.clone(),
            self.1.iter().map(JsonExt::copy).collect(),
            self.2.clone(),
        )
    }
}

pub struct JournalStore {
    pub repo_root: PathBuf,
    pub root: PathBuf,
    pub events_path: PathBuf,
    pub quarantine_path: PathBuf,
    pub index_json_path: PathBuf,
    pub db_path: PathBuf,
    pub daily_dir: PathBuf,
    pub weekly_dir: PathBuf,
    pub tz: TimeZoneSpec,
    pub project: ProjectIdentity,
    last_head: std::cell::RefCell<String>,
    repo_anchor: oms_safe_fs::Root,
    rendering: std::cell::Cell<bool>,
    storage_checked: std::cell::Cell<bool>,
    snapshot: std::cell::RefCell<Option<std::rc::Rc<SourceSnapshot>>>,
    cold_cached: std::cell::Cell<bool>,
    snapshot_stamp: std::cell::RefCell<String>,
}

impl JournalStore {
    /// Open without creating storage; stale indexes are replayed in memory.
    /// Pinned identities may reuse stat-validated cold verification from the index.
    pub fn open_for_read(repo_root: &Path, timezone_name: Option<&str>) -> Result<Self, String> {
        Self::open_internal(repo_root, timezone_name, false)
    }

    pub fn open_for_write(repo_root: &Path, timezone_name: Option<&str>) -> Result<Self, String> {
        Self::open_internal(repo_root, timezone_name, true)
    }

    fn open_internal(
        repo_root: &Path,
        timezone_name: Option<&str>,
        persist_project: bool,
    ) -> Result<Self, String> {
        let root = work_journal_dir(repo_root);
        let events_path = root.join("events.jsonl");
        let quarantine_path = root.join("quarantine/index.json");
        let index_json_path = root.join("index.json");
        let db_path = root.join("index.sqlite3");
        let daily_dir = root.join("daily");
        let weekly_dir = root.join("weekly");
        let tz = crate::tz::checked_timezone(timezone_name)?;

        let pinned = read_regular_no_symlink_bytes(&crate::schema::project_id_path(repo_root))?
            .and_then(|bytes| crate::json::from_slice(&bytes).ok())
            .is_some_and(|row| {
                row.at("schema_version").as_u64() == Some(1)
                    && row.at("project_id").is_string()
                    && row.at("project_name").is_string()
            });
        let manifest = crate::cold::load_cold_manifest(&root)?;
        let stamp = cold_stamp(&root, &manifest)?;
        let cold_cached = pinned
            && !persist_project
            && manifest.pending.is_none()
            && open_index(&db_path, false)
                .ok()
                .and_then(|conn| load_db_metadata(&conn).ok())
                .is_some_and(|meta| {
                    meta.get("schema_version").map(String::as_str) == Some(INDEX_SCHEMA_VERSION)
                        && meta.get("cold_stamp") == Some(&stamp)
                });
        let (manifest, cold, live, stamp) = if cold_cached {
            (manifest, Vec::new(), None, stamp)
        } else {
            verified_source(&root, !pinned)?
        };
        let mut seed_events = cold.iter().map(JsonExt::copy).collect::<Vec<_>>();
        if let Some(live) = &live {
            for raw in crate::cold::byte_lines(live) {
                if let Ok(row) = crate::json::from_slice(raw) {
                    seed_events.push(row);
                }
            }
        }
        let snapshot = (manifest, cold, live);
        let project = resolve_project_identity(repo_root, &seed_events, false, None, None)?;

        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            root,
            events_path,
            quarantine_path,
            index_json_path,
            db_path,
            daily_dir,
            weekly_dir,
            tz,
            project,
            last_head: std::cell::RefCell::new(String::new()),
            rendering: std::cell::Cell::new(false),
            storage_checked: std::cell::Cell::new(false),
            repo_anchor: oms_safe_fs::Root::open(repo_root)
                .map_err(|e| format!("journal repository unavailable: {e:?}"))?,
            cold_cached: std::cell::Cell::new(cold_cached),
            snapshot_stamp: std::cell::RefCell::new(stamp),
            snapshot: std::cell::RefCell::new(Some(std::rc::Rc::new(SourceSnapshot(
                snapshot.0, snapshot.1, snapshot.2,
            )))),
        })
    }

    pub(crate) fn pinned_identity(&self) -> Result<Option<(String, String)>, String> {
        if let Some(bytes) =
            read_regular_no_symlink_bytes(&crate::schema::project_id_path(&self.repo_root))?
            && let Ok(row) = crate::json::from_slice(&bytes)
            && row.at("schema_version").as_u64() == Some(1)
            && let (Some(id), Some(name)) = (
                row.at("project_id").as_str(),
                row.at("project_name").as_str(),
            )
        {
            return Ok(Some((id, name)));
        }
        let snapshot = self.replay_source_snapshot()?;
        for event in snapshot.1.iter().map(JsonExt::copy).chain(
            crate::cold::byte_lines(snapshot.2.as_ref().expect("replayed live snapshot"))
                .filter_map(|r| crate::json::from_slice(r).ok()),
        ) {
            if validate_canonical_event(&event).is_ok()
                && let (Some(id), Some(name)) = (
                    event.at("project_id").as_str(),
                    event.at("project_name").as_str(),
                )
            {
                return Ok(Some((id, name)));
            }
        }
        Ok(None)
    }

    fn verify_repo(&self) -> Result<(), String> {
        self.repo_anchor
            .read_no_follow(
                Path::new(".oms/work-journal/project.json"),
                oms_safe_fs::ByteReadOptions {
                    missing_ok: true,
                    max_bytes: usize::MAX,
                },
            )
            .map(|_| ())
            .map_err(|e| format!("journal repository namespace changed: {e:?}"))
    }

    fn source_snapshot(&self) -> Result<std::rc::Rc<SourceSnapshot>, String> {
        self.verify_repo()?;
        let current = crate::cold::load_cold_manifest(&self.root)?;
        let stamp = cold_stamp(&self.root, &current)?;
        if let Some(snapshot) = self.snapshot.borrow().as_ref()
            && snapshot.0 == current
            && *self.snapshot_stamp.borrow() == stamp
        {
            return Ok(snapshot.clone());
        }
        let (manifest, events, live, stamp) = verified_source(&self.root, false)?;
        self.snapshot_stamp.replace(stamp);
        self.cold_cached.set(false);
        let snapshot = std::rc::Rc::new(SourceSnapshot(manifest, events, live));
        self.snapshot.replace(Some(snapshot.clone()));
        Ok(snapshot)
    }
    fn replay_source_snapshot(&self) -> Result<std::rc::Rc<SourceSnapshot>, String> {
        if self.cold_cached.get() {
            let (manifest, events, live, stamp) = verified_source(&self.root, true)?;
            self.snapshot_stamp.replace(stamp);
            self.snapshot.replace(Some(std::rc::Rc::new(SourceSnapshot(
                manifest, events, live,
            ))));
            self.cold_cached.set(false);
        }
        for _ in 0..8 {
            let snapshot = self.source_snapshot()?;
            if snapshot.2.is_some() {
                if snapshot.0 == crate::cold::load_cold_manifest(&self.root)? {
                    return Ok(snapshot);
                }
                continue;
            }
            let live = crate::cold::journal_live(&self.root, &snapshot.0);
            if snapshot.0 != crate::cold::load_cold_manifest(&self.root)? {
                continue;
            }
            let snapshot = std::rc::Rc::new(SourceSnapshot(
                snapshot.0.clone(),
                snapshot.1.iter().map(JsonExt::copy).collect(),
                Some(live?),
            ));
            self.snapshot.replace(Some(snapshot.clone()));
            return Ok(snapshot);
        }
        Err("journal layout kept changing during read".into())
    }

    pub fn load_all_events_verified(&self, record_quarantine: bool) -> Result<Vec<Value>, String> {
        let snapshot = self.replay_source_snapshot()?;
        let mut ordered = snapshot.1.iter().map(JsonExt::copy).collect::<Vec<_>>();
        let live = snapshot.2.as_ref().expect("replayed live snapshot");
        let mut quarantine = Vec::new();
        let offset = ordered.len();
        for (index, raw) in crate::cold::byte_lines(live).enumerate() {
            if raw.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let parsed = crate::json::from_slice(raw);
            let reason = match parsed {
                Ok(value) => match validate_canonical_event(&value) {
                    Ok(()) => {
                        ordered.push(value);
                        continue;
                    }
                    Err(error) if error.contains("unsupported schema") => "unsupported_schema",
                    Err(_) => "invalid_schema",
                },
                Err(_) => "malformed",
            };
            quarantine.push(crate::json::json!({"record": offset+index+1, "sha256": sha256_hex(raw), "reason": reason}));
        }
        if record_quarantine
            && (!quarantine.is_empty()
                || read_regular_no_symlink_bytes(&self.quarantine_path)?.is_some())
        {
            let value = crate::json::json!({"schema_version": 1, "entries": quarantine});
            atomic_write_bytes(&self.quarantine_path, &pretty_json_bytes(&value)?)?;
        }

        ordered.sort_by(|a, b| {
            let ka = (
                a.get("occurred_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                a.get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            let kb = (
                b.get("occurred_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                b.get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            ka.cmp(&kb)
        });
        Ok(ordered)
    }

    fn open_db_connection(&self, writable: bool) -> Result<Connection, String> {
        open_index(&self.db_path, writable)
    }

    fn current_read_index(&self) -> Result<Option<Connection>, String> {
        if !self.db_path.try_exists().map_err(|e| e.to_string())? {
            return Ok(None);
        }
        let conn = match self.open_db_connection(false) {
            Ok(conn) => conn,
            Err(error) if error.starts_with("failed opening sqlite index:") => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(meta) = load_db_metadata(&conn) else {
            return Ok(None);
        };
        let (size, modified) = self.events_fingerprint()?;
        if meta.get("schema_version").map(String::as_str) != Some(INDEX_SCHEMA_VERSION)
            || meta.get("events_size") != Some(&size.to_string())
            || meta.get("events_mtime_ns") != Some(&modified.to_string())
            || meta.get("cold_stamp") != Some(&cold_stamp(&self.root, &self.source_snapshot()?.0)?)
        {
            return Ok(None);
        }
        Ok(Some(conn))
    }

    fn init_db_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                event_id TEXT PRIMARY KEY,
                occurred_at TEXT NOT NULL,
                local_date TEXT NOT NULL,
                iso_week TEXT NOT NULL,
                event_type TEXT NOT NULL,
                source_type TEXT NOT NULL,
                source_id TEXT NOT NULL,
                supersedes_event_id TEXT,
                active INTEGER NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_active_order ON events(active, occurred_at, event_id);
            CREATE INDEX IF NOT EXISTS events_day_active ON events(local_date, active, occurred_at, event_id);
            CREATE INDEX IF NOT EXISTS events_week_active ON events(iso_week, active, occurred_at, event_id);
            CREATE TABLE IF NOT EXISTS dirty_periods (
                kind TEXT NOT NULL,
                period TEXT NOT NULL,
                PRIMARY KEY(kind, period)
            );
            CREATE TABLE IF NOT EXISTS summaries (
                kind TEXT NOT NULL,
                period TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                content TEXT NOT NULL,
                has_blocker INTEGER NOT NULL,
                PRIMARY KEY(kind, period)
            );
            ",
        )
        .map_err(|e| format!("sqlite schema init failed: {e}"))
    }

    fn live_stat(&self) -> Result<(u64, i128), String> {
        if fs::symlink_metadata(&self.events_path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            self.verify_repo()?;
            return Ok((0, 0));
        }
        let parent = crate::files::Parent::open(&self.events_path)?;
        match parent.stat()? {
            Some(stat) => Ok((
                u64::try_from(stat.st_size).map_err(|_| "invalid journal size")?,
                i128::from(stat.st_mtime) * 1_000_000_000 + i128::from(stat.st_mtime_nsec),
            )),
            None => Ok((0, 0)),
        }
    }

    fn events_fingerprint(&self) -> Result<(u64, i128), String> {
        for _ in 0..8 {
            let snapshot = self.source_snapshot()?;
            let manifest = &snapshot.0;
            let stat = self.live_stat();
            if *manifest != crate::cold::load_cold_manifest(&self.root)? {
                continue;
            }
            let (size, modified) = stat?;
            if manifest.segments.is_empty() {
                return Ok((size, modified));
            }
            let mut bytes = canonical_json_bytes(&crate::cold::manifest_value(manifest)?)?;
            bytes.extend_from_slice(format!("{size}:{modified}").as_bytes());
            let digest = sha256_hex(&bytes);
            return Ok((
                size,
                i128::from(u64::from_str_radix(&digest[..16], 16).map_err(|e| e.to_string())?),
            ));
        }
        Err("journal layout kept changing during read".into())
    }

    fn ensure_layout(&self) -> Result<(), String> {
        ensure_oms_gitignore(&self.root)?;
        let project = crate::schema::project_id_path(&self.repo_root);
        if read_regular_no_symlink_bytes(&project)?.is_none() {
            atomic_write_bytes(
                &project,
                &pretty_json_bytes(
                    &crate::json::json!({"schema_version":1,"project_id":self.project.id,"project_name":self.project.name}),
                )?,
            )?;
        }
        Ok(())
    }

    fn ensure_index_current(&self, allow_create: bool) -> Result<Connection, String> {
        if !allow_create {
            return match self.current_read_index()? {
                Some(conn) => Ok(conn),
                None => self.replay_index(false),
            };
        }
        ensure_oms_gitignore(&self.root)?;
        self.ensure_layout()?;
        if !self.db_path.exists() {
            return self.rebuild_with_views();
        }
        let conn = self.open_db_connection(true)?;
        if Self::init_db_schema(&conn).is_err() {
            drop(conn);
            return self.rebuild_with_views();
        }
        let meta = load_db_metadata(&conn)?;
        let (size, mtime_ns) = self.events_fingerprint()?;
        let needs_rebuild = meta.get("schema_version").map(String::as_str)
            != Some(INDEX_SCHEMA_VERSION)
            || meta.get("events_size").map(String::as_str) != Some(&size.to_string())
            || meta.get("events_mtime_ns").map(String::as_str) != Some(&mtime_ns.to_string())
            || meta.get("cold_stamp") != Some(&cold_stamp(&self.root, &self.source_snapshot()?.0)?);
        if needs_rebuild {
            let old_rv = meta.get("renderer_version").cloned();
            let old_periods = list_all_summary_periods(&conn)?;
            drop(conn);
            let rebuilt = self.rebuild_index_into_conn()?;
            if old_rv.as_deref() != Some(RENDERER_VERSION) {
                for (k, p) in old_periods {
                    rebuilt
                        .execute(
                            "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES (?1, ?2)",
                            params![k, p],
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
            return self.finish_rebuild_views(rebuilt);
        }
        if meta.get("renderer_version").map(String::as_str) != Some(RENDERER_VERSION) {
            mark_all_periods_dirty(&conn)?;
        }
        Ok(conn)
    }

    fn rebuild_with_views(&self) -> Result<Connection, String> {
        let connection = self.rebuild_index_into_conn()?;
        self.finish_rebuild_views(connection)
    }
    fn finish_rebuild_views(&self, connection: Connection) -> Result<Connection, String> {
        if !self.source_snapshot()?.0.segments.is_empty() && !self.rendering.get() {
            drop(connection);
            self.rendering.set(true);
            let result = self.materialize_indexed();
            self.rendering.set(false);
            result?;
            return self.open_db_connection(true);
        }
        Ok(connection)
    }
    fn check_storage(&self) -> Result<(), String> {
        if self.storage_checked.get() {
            return Ok(());
        }
        let snapshot = self.source_snapshot()?;
        if !crate::cold::storage_health(&self.repo_root, true, Some(&snapshot.0)).healthy {
            return Err("journal storage is unhealthy".into());
        }
        self.storage_checked.set(true);
        Ok(())
    }

    fn rebuild_index_into_conn(&self) -> Result<Connection, String> {
        self.replay_index(true)
    }

    fn replay_index(&self, persist: bool) -> Result<Connection, String> {
        let before = self.events_fingerprint()?;
        if let Some(snapshot) = self.snapshot.borrow_mut().as_mut() {
            std::rc::Rc::make_mut(snapshot).2 = None;
        }
        let stamp = cold_stamp(&self.root, &self.source_snapshot()?.0)?;
        let events = self.load_all_events_verified(persist)?;
        let (size, mtime_ns) = self.events_fingerprint()?;
        if before != (size, mtime_ns) {
            return Err("journal changed during index replay".into());
        }
        let private = if persist {
            self.ensure_layout()?;
            Some(crate::files::PrivateIndex::new(&self.db_path)?)
        } else {
            None
        };
        let mut conn = if let Some(private) = &private {
            let conn = Connection::open_with_flags(
                private.path(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|e| format!("failed opening private index: {e}"))?;
            private.validate()?;
            conn
        } else {
            Connection::open_in_memory().map_err(|e| e.to_string())?
        };
        Self::init_db_schema(&conn)?;

        let mut superseded_ids: HashSet<String> = HashSet::new();
        for ev in &events {
            if let Some(sup) = ev.get("supersedes_event_id").and_then(Value::as_str) {
                superseded_ids.insert(sup.to_string());
            }
        }

        let mut all_periods: BTreeSet<(String, String)> = BTreeSet::new();
        {
            let tx = conn
                .transaction()
                .map_err(|e| format!("sqlite transaction failed: {e}"))?;
            for ev in &events {
                let eid = ev
                    .get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let occ = ev
                    .get("occurred_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let ld = ev
                    .get("local_date")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let iw = ev
                    .get("iso_week")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let et = ev
                    .get("event_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let st = ev
                    .pointer("/source/type")
                    .map(crate::schema::python_str)
                    .unwrap_or_default();
                let sid = ev
                    .pointer("/source/id")
                    .map(crate::schema::python_str)
                    .unwrap_or_default();
                let sup = ev.get("supersedes_event_id").and_then(Value::as_str);
                let active = if superseded_ids.contains(&eid) {
                    0i64
                } else {
                    1i64
                };
                let payload = String::from_utf8(canonical_json_bytes(ev)?)
                    .map_err(|e| format!("UTF-8 error: {e}"))?;
                tx.execute(
                    "INSERT INTO events
                    (event_id, occurred_at, local_date, iso_week, event_type, source_type, source_id, supersedes_event_id, active, payload)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![eid, occ, ld, iw, et, st, sid, sup, active, payload],
                )
                .map_err(|e| format!("sqlite insert failed: {e}"))?;
                if !ld.is_empty() {
                    all_periods.insert(("daily".to_string(), ld.to_string()));
                }
                if !iw.is_empty() {
                    all_periods.insert(("weekly".to_string(), iw.to_string()));
                }
            }
            for (kind, period) in &all_periods {
                tx.execute(
                    "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES (?1, ?2)",
                    params![kind, period],
                )
                .map_err(|e| format!("sqlite dirty insert failed: {e}"))?;
            }
            for (k, v) in [
                ("schema_version", INDEX_SCHEMA_VERSION.to_string()),
                ("renderer_version", RENDERER_VERSION.to_string()),
                ("events_size", size.to_string()),
                ("events_mtime_ns", mtime_ns.to_string()),
                ("cold_stamp", stamp.clone()),
            ] {
                tx.execute(
                    "INSERT OR REPLACE INTO metadata(key, value) VALUES (?1, ?2)",
                    params![k, v],
                )
                .map_err(|e| format!("sqlite metadata insert failed: {e}"))?;
            }
            tx.commit()
                .map_err(|e| format!("sqlite commit failed: {e}"))?;
        }
        if before != self.events_fingerprint()?
            || stamp != cold_stamp(&self.root, &self.source_snapshot()?.0)?
        {
            return Err("journal changed before index publication".into());
        }
        let Some(private) = private else {
            if !self.source_snapshot()?.0.segments.is_empty() {
                let lang = journal_language(Some(&self.tz));
                for (kind, period) in all_periods {
                    let events = load_active_events_for_period(&conn, &kind, &period)?;
                    if events.is_empty() {
                        continue;
                    }
                    let rendered = if kind == "daily" {
                        render_daily(
                            &period,
                            &crate::adapters::enrich_reviews(
                                &self.repo_root,
                                events.iter().map(JsonExt::copy).collect(),
                            ),
                            &self.tz,
                            &lang,
                        )
                    } else {
                        render_weekly(&period, &events, &self.tz, &lang)
                    };
                    let content = crate::render::bounded_export(&rendered, &lang);
                    conn.execute("INSERT INTO summaries(kind, period, content_hash, content, has_blocker) VALUES (?1, ?2, ?3, ?4, ?5)", params![kind, period, sha256_hex(content.as_bytes()), content, i64::from(has_blocker(&events))]).map_err(|e| e.to_string())?;
                }
                conn.execute("DELETE FROM dirty_periods", [])
                    .map_err(|e| e.to_string())?;
            }
            return Ok(conn);
        };
        drop(conn);
        self.verify_repo()?;
        private.install()?;
        if before != self.events_fingerprint()? {
            return Err(
                "journal changed after index publication; index requires reconciliation".into(),
            );
        }
        self.open_db_connection(true)
    }

    pub fn record_event(&self, raw: &Value, now: DateTime<Utc>) -> Result<(Value, bool), String> {
        let normalized = normalize_event(raw, &self.project, &self.tz, now)?;
        let eid = normalized
            .get("event_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing normalized event_id".to_string())?
            .to_string();

        if crate::cold::load_cold_manifest(&self.root)?
            .pending
            .is_some()
        {
            return Err("pending journal seal requires oms journal seal --apply".into());
        }
        let mut conn = self.ensure_index_current(true)?;
        if let Some(existing_payload) = conn
            .query_row(
                "SELECT payload FROM events WHERE event_id = ?1",
                params![&eid],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| format!("sqlite lookup failed: {e}"))?
        {
            if let Ok(val) = crate::json::from_str(&existing_payload) {
                validate_canonical_event(&val)?;
                return Ok((val, false));
            }
            return Err("indexed duplicate is unavailable".into());
        }

        ensure_oms_gitignore(&self.root)?;
        fs::create_dir_all(&self.root)
            .map_err(|e| format!("failed creating {}: {e}", self.root.display()))?;
        if crate::cold::load_cold_manifest(&self.root)?
            .pending
            .is_some()
        {
            return Err("pending journal seal requires oms journal seal --apply".into());
        }
        let line_bytes = canonical_json_bytes(&normalized)?;
        let line_str = String::from_utf8(line_bytes.clone()).map_err(|e| e.to_string())?;
        self.verify_repo()?;
        crate::files::append_event(&self.events_path, &line_bytes)?;
        let result = (|| -> Result<(Value, bool), String> {
            self.storage_checked.set(false);
            if let Some(snapshot) = self.snapshot.borrow_mut().as_mut()
                && let Some(live) = std::rc::Rc::make_mut(snapshot).2.as_mut()
            {
                if !live.is_empty() && !live.ends_with(b"\n") {
                    live.push(b'\n');
                }
                live.extend_from_slice(&line_bytes);
                live.push(b'\n');
            }

            let (new_size, new_mtime_ns) = self.events_fingerprint()?;
            let occ = normalized
                .get("occurred_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let ld = normalized
                .get("local_date")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let iw = normalized
                .get("iso_week")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let et = normalized
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let st = normalized
                .pointer("/source/type")
                .map(crate::schema::python_str)
                .unwrap_or_default();
            let sid = normalized
                .pointer("/source/id")
                .map(crate::schema::python_str)
                .unwrap_or_default();
            let sup = normalized
                .get("supersedes_event_id")
                .and_then(Value::as_str);

            let tx = conn
                .transaction()
                .map_err(|e| format!("sqlite transaction failed: {e}"))?;
            tx.execute(
            "INSERT OR IGNORE INTO events
            (event_id, occurred_at, local_date, iso_week, event_type, source_type, source_id, supersedes_event_id, active, payload)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)",
            params![eid, occ, ld, iw, et, st, sid, sup, line_str],
        )
        .map_err(|e| format!("sqlite insert failed: {e}"))?;

            if let Some(target_id) = sup
                && let Some((t_ld, t_iw)) = tx
                    .query_row(
                        "SELECT local_date, iso_week FROM events WHERE event_id = ?1",
                        params![target_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()
                    .map_err(|e| format!("sqlite superseded lookup failed: {e}"))?
            {
                tx.execute(
                    "UPDATE events SET active = 0 WHERE event_id = ?1",
                    params![target_id],
                )
                .map_err(|e| format!("sqlite deactivate failed: {e}"))?;
                tx.execute(
                    "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES ('daily', ?1)",
                    params![t_ld],
                )
                .map_err(|e| {
                    format!("canonical append completed; derived index update failed: {e}")
                })?;
                tx.execute(
                    "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES ('weekly', ?1)",
                    params![t_iw],
                )
                .map_err(|e| {
                    format!("canonical append completed; derived index update failed: {e}")
                })?;
            }

            if !ld.is_empty() {
                tx.execute(
                    "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES ('daily', ?1)",
                    params![ld],
                )
                .map_err(|e| format!("sqlite dirty insert failed: {e}"))?;
            }
            if !iw.is_empty() {
                tx.execute(
                    "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES ('weekly', ?1)",
                    params![iw],
                )
                .map_err(|e| format!("sqlite dirty insert failed: {e}"))?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO metadata(key, value) VALUES ('events_size', ?1)",
                params![new_size.to_string()],
            )
            .map_err(|e| format!("canonical append completed; derived index update failed: {e}"))?;
            tx.execute(
                "INSERT OR REPLACE INTO metadata(key, value) VALUES ('events_mtime_ns', ?1)",
                params![new_mtime_ns.to_string()],
            )
            .map_err(|e| format!("canonical append completed; derived index update failed: {e}"))?;
            tx.commit()
                .map_err(|e| format!("sqlite commit failed: {e}"))?;

            Ok((normalized, true))
        })();
        result.map_err(|error|format!("canonical append completed; derived state requires reconciliation before retry: {error}"))
    }

    pub fn capture_head_commit(&self, now: DateTime<Utc>) -> Result<Option<Value>, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["show", "-s", "--format=%H%x00%cI%x00%s", "HEAD"])
            .output();
        let output = match output {
            Ok(value) if value.status.success() => value,
            _ => return Ok(None),
        };
        let decoded = String::from_utf8_lossy(&output.stdout);
        let fields: Vec<_> = decoded.trim().splitn(3, '\0').collect();
        if fields.len() != 3 || fields[0].is_empty() || fields[1].is_empty() {
            return Ok(None);
        }
        let sha = fields[0];
        self.last_head.replace(sha.into());
        let payload = crate::json::json!({"event_type":"commit","occurred_at":utc_rfc3339(parse_rfc3339_utc(fields[1])?),"source":{"type":"git-commit","id":sha},
            "outcome":{"summary":format!("Commit {}: {}",sha.chars().take(12).collect::<String>(),sanitize_text(fields[2],500)),"status":"recorded"},"verification_status":"not_verified",
            "refs":[{"type":"commit","id":sha}],"evidence":[{"type":"git-commit","ref":sha}],"provenance":{"code_commit":sha}});
        let (event, created) = self.record_event(&payload, now)?;
        Ok(created.then_some(event))
    }

    pub fn materialize(&self) -> Result<MaterializeReport, String> {
        self.check_storage()?;
        self.materialize_indexed()
    }
    fn materialize_indexed(&self) -> Result<MaterializeReport, String> {
        if let Some(conn) = self.current_read_index()? {
            let meta = load_db_metadata(&conn)?;
            let dirty: i64 = conn
                .query_row("SELECT COUNT(*) FROM dirty_periods", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if dirty == 0
                && meta.get("renderer_version").map(String::as_str) == Some(RENDERER_VERSION)
            {
                return write_index_json(&conn, &self.index_json_path, &self.project.id);
            }
        }
        let mut conn = self.ensure_index_current(true)?;
        crate::files::ensure_directory(&self.daily_dir)?;
        crate::files::ensure_directory(&self.weekly_dir)?;
        let meta = load_db_metadata(&conn)?;
        let renderer_changed =
            meta.get("renderer_version").map(String::as_str) != Some(RENDERER_VERSION);
        if renderer_changed {
            mark_all_periods_dirty(&conn)?;
        }

        let mut dirty = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT kind, period FROM dirty_periods ORDER BY kind, period")
                .map_err(|e| format!("sqlite dirty read failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("sqlite dirty query failed: {e}"))?;
            for r in rows {
                dirty.push(r.map_err(|e| format!("sqlite dirty row failed: {e}"))?);
            }
        }

        let lang = journal_language(Some(&self.tz));
        let mut written = 0usize;
        let mut removed = 0usize;

        for (kind, period) in &dirty {
            let events = load_active_events_for_period(&conn, kind, period)?;
            let events = if kind == "daily" {
                crate::adapters::enrich_reviews(&self.repo_root, events)
            } else {
                events
            };
            let md_path = if kind == "daily" {
                self.daily_dir.join(format!("{period}.md"))
            } else {
                self.weekly_dir.join(format!("{period}.md"))
            };
            if events.is_empty() {
                if md_path.exists() {
                    crate::files::remove_owned_file(&md_path)?;
                }
                conn.execute(
                    "DELETE FROM summaries WHERE kind = ?1 AND period = ?2",
                    params![kind, period],
                )
                .map_err(|e| format!("derived index update failed: {e}"))?;
                removed += 1;
                continue;
            }
            let rendered = if kind == "daily" {
                render_daily(period, &events, &self.tz, &lang)
            } else {
                render_weekly(period, &events, &self.tz, &lang)
            };
            let exported = crate::render::bounded_export(&rendered, &lang);
            let content_hash = sha256_hex(exported.as_bytes());
            let blocker_flag = i64::from(has_blocker(&events));

            let needs_disk_write =
                read_regular_no_symlink_bytes(&md_path)?.as_deref() != Some(rendered.as_bytes());
            if needs_disk_write {
                atomic_write_bytes(&md_path, rendered.as_bytes())?;
                written += 1;
            }
            conn.execute(
                "INSERT OR REPLACE INTO summaries(kind, period, content_hash, content, has_blocker)
                VALUES (?1, ?2, ?3, ?4, ?5)",
                params![kind, period, content_hash, exported, blocker_flag],
            )
            .map_err(|e| format!("sqlite summary update failed: {e}"))?;
        }

        let tx = conn
            .transaction()
            .map_err(|e| format!("sqlite transaction failed: {e}"))?;
        tx.execute("DELETE FROM dirty_periods", [])
            .map_err(|e| format!("sqlite clear dirty failed: {e}"))?;
        tx.execute(
            "INSERT OR REPLACE INTO metadata(key, value) VALUES ('renderer_version', ?1)",
            params![RENDERER_VERSION],
        )
        .map_err(|e| format!("derived index metadata update failed: {e}"))?;
        tx.commit()
            .map_err(|e| format!("sqlite commit failed: {e}"))?;

        let report = write_index_json(&conn, &self.index_json_path, &self.project.id)?;
        Ok(MaterializeReport {
            event_count: report.event_count,
            written_periods: written,
            removed_periods: removed,
            daily_periods: report.daily_periods,
            weekly_periods: report.weekly_periods,
        })
    }

    pub fn rebuild(&self) -> Result<MaterializeReport, String> {
        self.check_storage()?;
        self.ensure_layout()?;
        let _conn = self.rebuild_index_into_conn()?;
        // Remove stale markdown summaries that do not belong to any active period
        let report = self.materialize()?;
        prune_stale_markdown(&self.daily_dir, &report.daily_periods)?;
        prune_stale_markdown(&self.weekly_dir, &report.weekly_periods)?;
        Ok(report)
    }

    pub fn summary_text(&self, kind: &str, period: &str) -> Result<Option<String>, String> {
        let conn = self.ensure_index_current(false)?;
        let content: Option<String> = conn
            .query_row(
                "SELECT content FROM summaries WHERE kind = ?1 AND period = ?2",
                params![kind, period],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("sqlite summary lookup failed: {e}"))?;
        Ok(content)
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<Value>, String> {
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare(
                "SELECT payload FROM events WHERE active = 1 ORDER BY occurred_at DESC, event_id DESC LIMIT ?1",
            )
            .map_err(|e| format!("sqlite recent query failed: {e}"))?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 256) as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| format!("sqlite recent query map failed: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(|e| format!("sqlite recent row failed: {e}"))?;
            let value = decode_indexed(&s)?;
            out.push(value);
        }
        Ok(out)
    }

    pub fn load_events_for_period(&self, kind: &str, period: &str) -> Result<Vec<Value>, String> {
        let conn = self.ensure_index_current(false)?;
        let events = load_active_events_for_period(&conn, kind, period)?;
        Ok(if kind == "daily" {
            crate::adapters::enrich_reviews(&self.repo_root, events)
        } else {
            events
        })
    }

    pub fn open_annotations(&self) -> Result<OpenAnnotations, String> {
        self.open_annotations_at(crate::tz::current_utc_now())
    }
    fn open_annotations_at(&self, now: DateTime<Utc>) -> Result<OpenAnnotations, String> {
        let (today, _, _) = self.tz.local_date_and_week(now);
        let since = naive_date_to_string(today - Duration::days(6));
        let conn = self.ensure_index_current(false)?;
        let mut statement=conn.prepare("SELECT payload FROM events WHERE active=1 AND local_date>=?1 ORDER BY occurred_at DESC,event_id DESC").map_err(|e|e.to_string())?;
        let rows = statement
            .query_map(params![since], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut blockers = Vec::new();
        let mut next_actions = Vec::new();
        let mut seen = [HashSet::new(), HashSet::new()];
        for row in rows {
            let event = decode_indexed(&row.map_err(|e| e.to_string())?)?;
            for (index, (field, bucket)) in [
                ("blocker", &mut blockers),
                ("next_action", &mut next_actions),
            ]
            .into_iter()
            .enumerate()
            {
                if let Some(text) = event.at(field).as_str().filter(|t| !t.is_empty())
                    && seen[index].insert(text.to_owned())
                {
                    bucket.push(AnnotationRow {
                        event_id: event.at("event_id").as_str().unwrap_or_default(),
                        occurred_at: event.at("occurred_at").as_str().unwrap_or_default(),
                        local_date: event.at("local_date").as_str().unwrap_or_default(),
                        text,
                    });
                }
            }
        }
        Ok(OpenAnnotations {
            since,
            blockers,
            next_actions,
        })
    }

    pub fn decisions(&self, since: Option<&str>, limit: usize) -> Result<DecisionsResult, String> {
        if let Some(s) = since
            && (s.len() != 10
                || parse_date_ymd(s).is_none()
                || parse_date_ymd(s).map(naive_date_to_string).as_deref() != Some(s))
        {
            return Err(format!("invalid --since date '{s}'; expected YYYY-MM-DD"));
        }
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE active = 1 AND (?1 IS NULL OR local_date >= ?1) AND json_type(payload, '$.decision') = 'text' AND json_extract(payload, '$.decision') != '' ORDER BY occurred_at DESC, event_id DESC")
            .map_err(|e| format!("sqlite decisions query failed: {e}"))?;
        let rows = stmt
            .query_map(params![since], |row| row.get::<_, String>(0))
            .map_err(|e| format!("sqlite decisions map failed: {e}"))?;

        let mut seen_text: HashSet<String> = HashSet::new();
        let mut items = Vec::new();
        let mut omitted = 0usize;

        for r in rows {
            let s = r.map_err(|e| format!("sqlite decisions row failed: {e}"))?;
            let ev = decode_indexed(&s)?;
            let ld = ev
                .get("local_date")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(since_date) = since
                && ld.as_str() < since_date
            {
                continue;
            }
            let Some(dec) = ev
                .get("decision")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            else {
                continue;
            };
            if !seen_text.insert(dec.to_string()) {
                continue;
            }
            if items.len() >= limit {
                omitted += 1;
                continue;
            }
            items.push(DecisionRow {
                event_id: ev
                    .get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                occurred_at: ev
                    .get("occurred_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                local_date: ld.to_string(),
                decision: dec.to_string(),
                event_type: ev.at("event_type").as_str().unwrap_or_default(),
                task_id: ev
                    .at("correlation")
                    .at("task_id")
                    .as_str()
                    .unwrap_or_default(),
                source_type: ev
                    .pointer("/source/type")
                    .map(crate::schema::python_str)
                    .unwrap_or_default()
                    .to_string(),
                source_id: ev
                    .pointer("/source/id")
                    .map(crate::schema::python_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Ok(DecisionsResult { items, omitted })
    }

    pub fn resolve(
        &self,
        event_id: &str,
        how: &str,
        now: DateTime<Utc>,
    ) -> Result<ResolveResult, String> {
        let eid = event_id;
        let how_clean = how.split_whitespace().collect::<Vec<_>>().join(" ");
        if how_clean.is_empty() {
            return Err("resolve requires --how TEXT".into());
        }
        if !event_id_re().is_match(eid) {
            return Err("no active journal event with that ID carries a blocker (see: oms journal show --blockers)".into());
        }
        let conn = self.ensure_index_current(true)?;
        let target_payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM events WHERE event_id=?1 AND active=1",
                params![eid],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let target=decode_indexed(&target_payload.ok_or("no active journal event with that ID carries a blocker (see: oms journal show --blockers)")?)?;
        let target_blocker=target.at("blocker").as_str().filter(|b|!b.is_empty()).ok_or("no active journal event with that ID carries a blocker (see: oms journal show --blockers)")?.to_owned();
        let short: String = target_blocker.chars().take(200).collect();
        let mut correction_payload = crate::json::json!({"event_type":"correction","occurred_at":utc_rfc3339(now),"source":{"type":"journal-resolve","id":format!("resolve:{eid}")},"supersedes_event_id":eid,
            "outcome":{"summary":format!("Blocker resolved: {short}"),"status":"resolved"},"decision":format!("Resolved blocker \"{short}\": {}",how_clean.chars().take(400).collect::<String>()),"verification_status":"not_verified"});
        if crate::schema::json_truthy(target.at("correlation").at("task_id")) {
            *correction_payload.at_mut("correlation") =
                crate::json::json!({"task_id":target.at("correlation").at("task_id")});
        }
        drop(conn);

        let (correction, created) = self.record_event(&correction_payload, now)?;
        self.materialize()?;

        // Check for other remaining active events with the exact same blocker text
        let mut remaining_matching = Vec::new();
        if !target_blocker.is_empty() {
            let conn2 = self.ensure_index_current(true)?;
            let mut stmt = conn2
                .prepare("SELECT event_id, payload FROM events WHERE active = 1 AND json_type(payload, '$.blocker') = 'text' AND json_extract(payload, '$.blocker') = ?1 ORDER BY occurred_at DESC, event_id DESC")
                .map_err(|e| format!("sqlite remaining blocker query failed: {e}"))?;
            let rows = stmt
                .query_map(params![target_blocker], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("sqlite remaining map failed: {e}"))?;
            for row in rows {
                let (other_id, payload) = row.map_err(|e| e.to_string())?;
                let value = decode_indexed(&payload)?;
                if value.at("blocker").as_str().as_deref() == Some(&target_blocker) {
                    remaining_matching.push(other_id);
                }
            }
        }

        Ok(ResolveResult {
            blocker: target_blocker,
            created,
            superseded_event_id: eid.to_string(),
            correction_event_id: correction
                .get("event_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            remaining_matching_blocker_ids: remaining_matching,
        })
    }

    pub fn summary_rows(&self) -> Result<Vec<SummaryRow>, String> {
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare(
                "SELECT kind, period, content_hash, content, has_blocker FROM summaries ORDER BY kind, period",
            )
            .map_err(|e| format!("sqlite summary_rows failed: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SummaryRow {
                    kind: row.get(0)?,
                    period: row.get(1)?,
                    content_hash: row.get(2)?,
                    content: row.get(3)?,
                    has_blocker: row.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(|e| format!("sqlite summary_rows map failed: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("sqlite summary row failed: {e}"))?);
        }
        Ok(out)
    }

    pub fn period_aggregates(&self, kind: &str, period: &str) -> Result<Value, String> {
        if kind != "daily" {
            return Ok(Value::Object(Map::new()));
        }
        let events = self.load_events_for_period(kind, period)?;
        if events.is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        let agg = compute_period_aggregates(&events);
        Ok(crate::json::json!({
            "sessions": agg.sessions,
            "commits": agg.commits,
            "verified": agg.verified,
        }))
    }

    pub fn write_prompt_stamp(
        &self,
        signature: &str,
        autodistill: bool,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let path = self.root.join("prompt-tick");
        let head = self.last_head.borrow().clone();
        if head.is_empty() || (autodistill && self.distill_due_today(now)?) {
            return crate::files::remove_owned_file(&path);
        }
        let mut lines = vec![self.tz.next_local_midnight_epoch(now).to_string(), head];
        for name in ["events.jsonl", "digest.json", "distill.json"] {
            let parent = crate::files::Parent::open(&self.root.join(name))?;
            lines.push(
                parent
                    .stat()?
                    .map(|s| s.st_size.to_string())
                    .unwrap_or_default(),
            );
        }
        lines.push(signature.into());
        atomic_write_bytes(&path, format!("{}\n", lines.join("\n")).as_bytes())
    }

    pub fn prompt_digest(&self, now: DateTime<Utc>) -> Result<Option<String>, String> {
        let (_, today, _) = self.tz.local_date_and_week(now);
        let path = self.root.join("digest.json");
        if let Some(bytes) = read_regular_no_symlink_bytes(&path)?
            && let Ok(row) = crate::json::from_slice(&bytes)
            && row.at("schema_version").as_u64() == Some(1)
            && row.at("last_date").as_str().as_deref() == Some(&today)
        {
            return Ok(None);
        }
        let recent = self.recent_events(256)?;
        if recent.is_empty() {
            return Ok(None);
        }
        let annotations = self.open_annotations_at(now)?;
        let previous: BTreeSet<_> = recent
            .iter()
            .filter_map(|e| e.at("local_date").as_str())
            .filter(|d| d.as_str() < today.as_str())
            .collect();
        if annotations.blockers.is_empty()
            && annotations.next_actions.is_empty()
            && previous.is_empty()
        {
            return Ok(None);
        }
        let mut lines = vec![format!("[work-journal] {} — {today}", self.project.name)];
        lines.push("Historical observations; check oms inbox for current task state.".into());
        for (heading, rows) in [
            ("Recently observed blockers", &annotations.blockers),
            ("Recorded next actions", &annotations.next_actions),
        ] {
            if !rows.is_empty() {
                lines.push(format!("{heading} (last 7d):"));
                lines.extend(
                    rows.iter()
                        .take(3)
                        .map(|r| format!("- {} [{}]", r.text, r.event_id)),
                );
            }
        }
        if let Some(day) = previous.last() {
            let rows: Vec<_> = recent
                .iter()
                .filter(|e| e.at("local_date").as_str().as_deref() == Some(day))
                .collect();
            let verified = rows
                .iter()
                .filter(|e| e.at("verification_status").as_str().as_deref() == Some("passed"))
                .count();
            lines.push(format!(
                "Last journal day {day}: {} events, {verified} verified.",
                rows.len()
            ));
            let all = self.load_events_for_period("daily", day)?;
            if !all
                .iter()
                .any(|e| crate::schema::json_truthy(e.at("decision")))
            {
                lines.push("That day recorded no decision: 'oms agent-task update --decision TEXT [--next TEXT]' is what keeps the summary readable.".into());
            }
        }
        let handoffs = self.repo_root.join(".oms/handoffs");
        let mut newest = None;
        if let Ok(entries) = fs::read_dir(handoffs) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|v| v != "md") {
                    continue;
                }
                let Ok(info) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if !info.is_file() || info.file_type().is_symlink() {
                    continue;
                }
                if let Ok(modified) = info.modified()
                    && newest.as_ref().is_none_or(|(time, _)| modified > *time)
                {
                    newest = Some((modified, entry.file_name()));
                }
            }
        }
        if let Some((modified, name)) = newest
            && let Ok(duration) = std::time::SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(now.timestamp().max(0) as u64))
                .ok_or("invalid clock")?
                .duration_since(modified)
            && duration.as_secs_f64() <= 48.0 * 3600.0
        {
            lines.push(format!(
                "Newest handoff ({}h old): oms session-handoff show {}",
                duration.as_secs() / 3600,
                sanitize_text(&crate::sanitize::diagnostic_os_str(&name), 200)
            ));
        }
        lines.push("Details: oms journal show --today | --blockers | --recent 20".into());
        atomic_write_bytes(
            &path,
            &pretty_json_bytes(&crate::json::json!({"schema_version":1,"last_date":today}))?,
        )?;
        Ok(Some(lines.join("\n")))
    }

    pub fn distill(
        &self,
        dry_run: bool,
        memory_append: Option<MemoryAppendFn<'_>>,
        now: DateTime<Utc>,
    ) -> Result<DistillReport, String> {
        let state_path = self.root.join("distill.json");
        let state = read_regular_no_symlink_bytes(&state_path)?
            .map(|bytes| {
                if bytes.len() > 512 * 1024 {
                    return Err("distill receipt exceeds its bounded state limit".to_string());
                }
                crate::json::from_slice(&bytes).map_err(|e| format!("invalid distill receipt: {e}"))
            })
            .transpose()?;
        let mut completed = BTreeSet::new();
        let last_attempted_candidate = if let Some(state) = &state {
            if state.at("schema_version").as_u64() != Some(1) {
                return Err("unsupported distill receipt schema".into());
            }
            if let Some(keys) = state.get("completed_candidates") {
                let keys = keys
                    .as_array()
                    .ok_or("invalid distill candidate receipts")?;
                if keys.len() > MAX_DISTILL_RECEIPTS {
                    return Err(
                        "distill candidate receipt limit exceeded; no input consumed".into(),
                    );
                }
                for key in keys {
                    completed.insert(distill_receipt_key(key)?);
                }
            }
            state
                .get("last_attempted_candidate")
                .map(distill_receipt_key)
                .transpose()?
        } else {
            None
        };

        // Occurrence time is not ingestion order: late events and daily blocker
        // observations remain eligible. Only annotation rows are revisited, with
        // an explicit bound; completed fingerprints live in this existing receipt.
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare(
                "SELECT payload FROM events WHERE active = 1 AND ((json_type(payload, '$.decision') = 'text' AND json_extract(payload, '$.decision') != '') OR (json_type(payload, '$.blocker') = 'text' AND json_extract(payload, '$.blocker') != '')) ORDER BY occurred_at ASC, event_id ASC LIMIT ?1",
            )
            .map_err(|e| format!("sqlite distill query failed: {e}"))?;
        let rows = stmt
            .query_map([MAX_DISTILL_INPUTS as i64 + 1], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| format!("sqlite distill map failed: {e}"))?;
        let mut pending = Vec::new();
        for row in rows {
            if pending.len() == MAX_DISTILL_INPUTS {
                return Err("distill annotation limit exceeded; no input consumed".into());
            }
            let payload = row.map_err(|e| format!("sqlite distill row failed: {e}"))?;
            pending.push(decode_indexed(&payload)?);
        }
        let latest: Option<(String, String)> = conn.query_row(
            "SELECT occurred_at, event_id FROM events WHERE active = 1 ORDER BY occurred_at DESC, event_id DESC LIMIT 1",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|e| e.to_string())?;

        let shared_norm = read_regular_no_symlink_bytes(
            &self.repo_root.join(".oms").join("memory").join("shared.md"),
        )?
        .map(|b| distill_normalized_text(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
        let mut candidates = distill_candidates(&pending);
        // Rotate before deduplication so a completed candidate can still mark the
        // prior batch boundary. A refusal advances the scan, not completion.
        if let Some(key) = last_attempted_candidate
            && let Some(position) = candidates.iter().position(|candidate| candidate.key == key)
        {
            candidates.rotate_left(position + 1);
        }
        let mut fresh = Vec::new();
        let mut deduped = 0;
        for candidate in candidates {
            if completed.contains(&candidate.key) {
                continue;
            }
            if !shared_norm.is_empty()
                && !candidate.needle.is_empty()
                && shared_norm.contains(&candidate.needle)
            {
                completed.insert(candidate.key);
                deduped += 1;
            } else {
                fresh.push(candidate);
            }
        }
        let dropped = fresh.len().saturating_sub(MAX_DISTILL_ATTEMPTS);
        fresh.truncate(MAX_DISTILL_ATTEMPTS);
        if completed.len() + fresh.len() > MAX_DISTILL_RECEIPTS {
            return Err("distill candidate receipt limit exceeded; no input consumed".into());
        }
        if dry_run {
            return Ok(DistillReport {
                promoted: fresh.len(),
                dropped,
                deduped,
                skipped_refused: Vec::new(),
                dry_run: true,
                lessons: fresh.into_iter().map(|c| c.lesson).collect(),
            });
        }

        let next_candidate_cursor = fresh
            .last()
            .filter(|_| dropped > 0)
            .map(|candidate| candidate.key.clone());
        let mut promoted_lessons = Vec::new();
        let mut skipped_refused = Vec::new();
        if !fresh.is_empty() {
            let append_fn = memory_append.ok_or_else(|| {
                "native memory append adapter is not wired; parent CLI must supply MemoryAppendFn via run_with_services".to_string()
            })?;
            for candidate in fresh {
                match append_fn(&self.repo_root, "journal", &candidate.lesson) {
                    Ok(MemoryAppendOutcome::Appended) => {
                        completed.insert(candidate.key);
                        promoted_lessons.push(candidate.lesson);
                    }
                    Ok(MemoryAppendOutcome::Refused(_)) | Err(_) => {
                        skipped_refused.push(candidate.lesson);
                    }
                }
            }
        }

        let through_val = if let Some((occ, eid)) = latest {
            crate::json::json!({
                "occurred_at": occ,
                "event_id": eid,
            })
        } else {
            Value::Null
        };

        let (_, today_str, _) = self.tz.local_date_and_week(now);
        let mut state_obj = Map::new();
        state_obj.insert("schema_version".into(), crate::json::json!(1));
        state_obj.insert("through".into(), through_val);
        state_obj.insert(
            "completed_candidates".into(),
            crate::json::json!(completed.into_iter().collect::<Vec<_>>()),
        );
        state_obj.insert(
            "pending_candidates".into(),
            crate::json::json!(dropped + skipped_refused.len()),
        );
        if let Some(key) = next_candidate_cursor {
            state_obj.insert("last_attempted_candidate".into(), crate::json::json!(key));
        }
        state_obj.insert(
            "last_run_date".into(),
            Value::String(today_str.as_str().into()),
        );
        let bytes = pretty_json_bytes(&Value::Object(state_obj))?;
        if read_regular_no_symlink_bytes(&state_path)?.as_deref() != Some(&bytes) {
            atomic_write_bytes(&state_path, &bytes)?;
        }

        Ok(DistillReport {
            promoted: promoted_lessons.len(),
            dropped,
            deduped,
            skipped_refused,
            dry_run: false,
            lessons: promoted_lessons,
        })
    }

    pub fn distill_due_today(&self, now: DateTime<Utc>) -> Result<bool, String> {
        let state_path = self.root.join("distill.json");
        let (_, today, _) = self.tz.local_date_and_week(now);
        if let Some(bytes) = read_regular_no_symlink_bytes(&state_path)?
            && let Ok(v) = crate::json::from_slice(&bytes)
            && v.get("schema_version").and_then(Value::as_i64) == Some(1)
            && v.get("last_run_date").and_then(Value::as_str).as_deref() == Some(today.as_str())
        {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn status_report(&self) -> Result<Value, String> {
        let conn = self.ensure_index_current(false)?;
        let summary = index_summary(&conn, &self.project.id)?;
        let snapshot = self.source_snapshot()?;
        let storage = crate::cold::storage_health(&self.repo_root, false, Some(&snapshot.0));
        let dirty: i64 = conn
            .query_row("SELECT COUNT(*) FROM dirty_periods", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut synced = 0;
        let mut pending = 0;
        let mut failed = 0;
        if let Some(bytes) = read_regular_no_symlink_bytes(&self.root.join("sync/notion.json"))? {
            match crate::json::from_slice(&bytes) {
                Ok(state) => {
                    if let Some(rows) = state.at("summaries").as_object() {
                        for row in rows.values() {
                            match row.at("status").as_str().as_deref() {
                                Some("synced") => synced += 1,
                                Some("pending") => pending += 1,
                                Some("failed") => failed += 1,
                                _ => {}
                            }
                        }
                    }
                }
                Err(_) => failed += 1,
            }
        }
        let (detected_id, detected_name) = crate::schema::detected_identity(&self.repo_root);
        let settings = notion_settings()?;
        Ok(
            crate::json::json!({"repo":crate::sanitize::diagnostic_os_str(self.repo_root.as_os_str()),"enabled":true,
            "project":{"id":self.project.id,"name":self.project.name,"detected_id":detected_id,"detected_name":detected_name,"drift":self.project.id!=detected_id||self.project.name!=detected_name},
            "event_count":summary.at("event_count"),"active_event_count":summary.at("active_event_count"),"events_bytes":self.live_stat()?.0,"storage":storage.to_json(),
            "daily_count":summary.at("daily").as_array().map(<[_]>::len).unwrap_or(0),"weekly_count":summary.at("weekly").as_array().map(<[_]>::len).unwrap_or(0),"dirty_period_count":dirty,"index_ready":true,
            "notion":{"configured":!settings.target_id().is_empty(),"excluded":notion_repo_excluded(&self.repo_root),"credential_present":crate::config::notion_auth_available(&settings),"auth_mode":settings.auth_mode,
                "target_kind":if !settings.data_source_id.is_empty(){"data_source"}else if !settings.database_id.is_empty(){"database"}else{""},"data_source_id":settings.data_source_id,"sync_mode":"finalized","synced":synced,"pending":pending,"failed":failed}}),
        )
    }

    pub fn seal(&self, apply: bool, now: DateTime<Utc>) -> Result<usize, String> {
        self.check_storage()?;
        let manifest = self.source_snapshot()?.0.clone();
        if apply {
            self.ensure_layout()?;
        }
        let count = crate::cold::seal_verified(&self.repo_root, &self.tz, now, apply, manifest)?;
        if apply {
            self.snapshot.replace(None);
            if count > 0 {
                self.ensure_index_current(true)?;
            }
        }
        Ok(count)
    }
}

#[derive(Clone, Debug, Default)]
pub struct MaterializeReport {
    pub event_count: usize,
    pub written_periods: usize,
    pub removed_periods: usize,
    pub daily_periods: Vec<String>,
    pub weekly_periods: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AnnotationRow {
    pub event_id: String,
    pub local_date: String,
    pub text: String,
    pub occurred_at: String,
}

#[derive(Clone, Debug, Default)]
pub struct OpenAnnotations {
    pub since: String,
    pub blockers: Vec<AnnotationRow>,
    pub next_actions: Vec<AnnotationRow>,
}

#[derive(Clone, Debug)]
pub struct DecisionRow {
    pub event_id: String,
    pub occurred_at: String,
    pub local_date: String,
    pub decision: String,
    pub event_type: String,
    pub task_id: String,
    pub source_type: String,
    pub source_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct DecisionsResult {
    pub items: Vec<DecisionRow>,
    pub omitted: usize,
}

#[derive(Clone, Debug)]
pub struct ResolveResult {
    pub blocker: String,
    pub created: bool,
    pub superseded_event_id: String,
    pub correction_event_id: String,
    pub remaining_matching_blocker_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SummaryRow {
    pub kind: String,
    pub period: String,
    pub content_hash: String,
    pub content: String,
    pub has_blocker: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DistillReport {
    pub promoted: usize,
    pub dropped: usize,
    pub deduped: usize,
    pub skipped_refused: Vec<String>,
    pub dry_run: bool,
    pub lessons: Vec<String>,
}

type VerifiedSource = (
    crate::cold::ColdManifest,
    Vec<Value>,
    Option<Vec<u8>>,
    String,
);

fn verified_source(root: &Path, live: bool) -> Result<VerifiedSource, String> {
    for _ in 0..8 {
        let before = crate::cold::load_cold_manifest(root)?;
        let stamp = cold_stamp(root, &before)?;
        let (manifest, events, bytes) = if live {
            let (manifest, events, bytes) = crate::cold::replay_snapshot(root)?;
            (manifest, events, Some(bytes))
        } else {
            let (manifest, events) = crate::cold::read_verified_cold_events(root)?;
            let bytes = if manifest.pending.is_some() {
                Some(crate::cold::journal_live(root, &manifest)?)
            } else {
                None
            };
            (manifest, events, bytes)
        };
        if manifest == before && stamp == cold_stamp(root, &manifest)? {
            return Ok((manifest, events, bytes, stamp));
        }
    }
    Err("journal layout kept changing during read".into())
}

fn open_index(path: &Path, writable: bool) -> Result<Connection, String> {
    let parent = crate::files::Parent::open(path)?;
    let before = parent.stat()?.ok_or("sqlite index does not exist")?;
    let flags = if writable {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
    } | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(path, flags)
        .map_err(|e| format!("failed opening sqlite index: {e}"))?;
    if !writable {
        // Set before the first read: even WAL indexes must not create a shared-memory sidecar.
        conn.execute_batch("PRAGMA locking_mode = EXCLUSIVE; PRAGMA temp_store = MEMORY;")
            .map_err(|e| format!("failed opening sqlite index: {e}"))?;
    }
    let after = parent.stat()?.ok_or("sqlite index disappeared")?;
    if before.st_dev != after.st_dev || before.st_ino != after.st_ino {
        return Err("sqlite index replaced during open".into());
    }
    conn.busy_timeout(std::time::Duration::from_secs(if writable { 5 } else { 0 }))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

// Include archive identity and ctime so changes behind an unchanged manifest
// invalidate the cache without reading archive bodies on the steady-state path.
fn cold_stamp(root: &Path, manifest: &crate::cold::ColdManifest) -> Result<String, String> {
    let path = crate::cold::cold_manifest_path(root);
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok("missing".into());
    }
    let mut stamp = String::new();
    for path in std::iter::once(path).chain(
        manifest
            .segments
            .iter()
            .map(|s| crate::cold::cold_dir(root).join(&s.file)),
    ) {
        let stat = crate::files::Parent::open(&path)?
            .stat()?
            .ok_or("cold journal file unavailable")?;
        use std::fmt::Write;
        write!(
            stamp,
            "{}:{}:{}:{}:{}:{}:{};",
            stat.st_dev,
            stat.st_ino,
            stat.st_size,
            stat.st_mtime,
            stat.st_mtime_nsec,
            stat.st_ctime,
            stat.st_ctime_nsec
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(sha256_hex(stamp.as_bytes()))
}

fn load_db_metadata(conn: &Connection) -> Result<BTreeMap<String, String>, String> {
    let mut statement = conn
        .prepare("SELECT key,value FROM metadata")
        .map_err(|e| e.to_string())?;
    statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}
fn list_all_summary_periods(conn: &Connection) -> Result<Vec<(String, String)>, String> {
    let mut statement = conn
        .prepare("SELECT kind,period FROM summaries ORDER BY kind,period")
        .map_err(|e| e.to_string())?;
    statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}

fn mark_all_periods_dirty(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        INSERT OR IGNORE INTO dirty_periods(kind, period)
        SELECT DISTINCT 'daily', local_date FROM events WHERE local_date != '';
        INSERT OR IGNORE INTO dirty_periods(kind, period)
        SELECT DISTINCT 'weekly', iso_week FROM events WHERE iso_week != '';
        INSERT OR IGNORE INTO dirty_periods(kind, period)
        SELECT kind, period FROM summaries;
        ",
    )
    .map_err(|e| format!("sqlite mark_all_periods_dirty failed: {e}"))
}

fn load_active_events_for_period(
    conn: &Connection,
    kind: &str,
    period: &str,
) -> Result<Vec<Value>, String> {
    let query = if kind == "daily" {
        "SELECT payload FROM events WHERE local_date = ?1 AND active = 1 ORDER BY occurred_at ASC, event_id ASC"
    } else if kind == "weekly" {
        "SELECT payload FROM events WHERE iso_week = ?1 AND active = 1 ORDER BY occurred_at ASC, event_id ASC"
    } else {
        return Err(format!("unsupported summary kind '{kind}'"));
    };
    let mut stmt = conn
        .prepare(query)
        .map_err(|e| format!("sqlite period query failed: {e}"))?;
    let rows = stmt
        .query_map(params![period], |row| row.get::<_, String>(0))
        .map_err(|e| format!("sqlite period map failed: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        let s = r.map_err(|e| format!("sqlite period row failed: {e}"))?;
        let value = decode_indexed(&s)?;
        out.push(value);
    }
    Ok(out)
}

fn decode_indexed(payload: &str) -> Result<Value, String> {
    let value: Value = crate::json::from_str(payload).map_err(|e| e.to_string())?;
    validate_canonical_event(&value)?;
    Ok(value)
}
fn index_summary(conn: &Connection, project_id: &str) -> Result<Value, String> {
    let event_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let active: i64 = conn
        .query_row("SELECT COUNT(*) FROM events WHERE active=1", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let mut periods = Vec::new();
    for field in ["local_date", "iso_week"] {
        let mut statement = conn
            .prepare(&format!(
                "SELECT DISTINCT {field} FROM events WHERE active=1 ORDER BY {field}"
            ))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        periods.push(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
        );
    }
    let mut statement=conn.prepare("SELECT payload FROM events WHERE active=1 ORDER BY occurred_at DESC,event_id DESC LIMIT 256").map_err(|e|e.to_string())?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    for row in rows {
        let value = decode_indexed(&row.map_err(|e| e.to_string())?)?;
        let descriptor: Map = crate::json::object(
            [
                "event_id",
                "occurred_at",
                "local_date",
                "iso_week",
                "event_type",
                "source",
            ]
            .into_iter()
            .map(|k| (k.into(), value.at(k).copy())),
        );
        events.push(Value::Object(descriptor));
    }
    events.reverse();
    Ok(
        crate::json::json!({"schema_version":1,"renderer_version":3,"project_id":project_id,"event_count":event_count,"active_event_count":active,"events_truncated":active>events.len() as i64,"events":events,"daily":periods[0],"weekly":periods[1]}),
    )
}
fn write_index_json(
    conn: &Connection,
    path: &Path,
    project_id: &str,
) -> Result<MaterializeReport, String> {
    let value = index_summary(conn, project_id)?;
    let bytes = pretty_json_bytes(&value)?;
    if read_regular_no_symlink_bytes(path)?.as_deref() != Some(bytes.as_slice()) {
        atomic_write_bytes(path, &bytes)?;
    }
    Ok(MaterializeReport {
        event_count: value.at("event_count").as_u64().unwrap_or(0) as usize,
        daily_periods: value
            .at("daily")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect(),
        weekly_periods: value
            .at("weekly")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect(),
        ..Default::default()
    })
}

fn prune_stale_markdown(dir: &Path, keep_periods: &[String]) -> Result<(), String> {
    if !ensure_regular_dir_no_symlink(dir)? {
        return Ok(());
    }
    let keep: HashSet<_> = keep_periods
        .iter()
        .map(|p| std::ffi::OsString::from(format!("{p}.md")))
        .collect();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") && !keep.contains(&entry.file_name()) {
            let info = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if info.is_file() || info.file_type().is_symlink() {
                crate::files::remove_owned_file(&path)?;
            }
        }
    }
    Ok(())
}

fn distill_normalized_text(text: &str) -> String {
    crate::unicode::casefold(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

const MAX_DISTILL_INPUTS: usize = 16_384;
const MAX_DISTILL_RECEIPTS: usize = 4_096;
const MAX_DISTILL_ATTEMPTS: usize = 5;

fn distill_receipt_key(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .filter(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "invalid distill candidate identity".into())
}

struct DistillCandidate {
    key: String,
    lesson: String,
    needle: String,
}

fn distill_candidate(
    kind: &str,
    claim: &str,
    lesson: String,
    events: &[&Value],
) -> DistillCandidate {
    let key = sha256_hex(
        format!(
            "journal-distill-v2\0{kind}\0{}",
            distill_normalized_text(claim)
        )
        .as_bytes(),
    );
    let sources: Vec<_> = events
        .iter()
        .take(8)
        .filter_map(|e| e.at("event_id").as_str())
        .collect();
    let omitted = events.len().saturating_sub(sources.len());
    let suffix = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} more source events")
    };
    DistillCandidate {
        key,
        needle: distill_normalized_text(&sanitize_text(claim, 600)),
        lesson: format!(
            "{lesson}\nsource events: {}{suffix}; recorded evidence; not independently verified",
            sources.join(", ")
        ),
    }
}

fn distill_candidates(events: &[Value]) -> Vec<DistillCandidate> {
    let mut blockers: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut candidates = Vec::new();
    let mut seen_decisions = HashSet::new();
    for ev in events {
        let occ = ev.at("occurred_at").as_str().unwrap_or_default();
        let eid = ev.at("event_id").as_str().unwrap_or_default();
        let ld = ev.at("local_date").as_str().unwrap_or_default();
        if let Some(dec) = ev.at("decision").as_str() {
            let key = distill_normalized_text(&dec);
            if !key.is_empty() && seen_decisions.insert(key) {
                let clean = sanitize_text(&dec, 600);
                candidates.push((
                    (occ.clone(), eid.clone()),
                    distill_candidate(
                        "decision",
                        &dec,
                        format!("journal-distill: decision {ld}: {clean}"),
                        &[ev],
                    ),
                ));
            }
        }
        if let Some(blocker) = ev.at("blocker").as_str() {
            let key = distill_normalized_text(&blocker);
            if !key.is_empty() {
                blockers.entry(key).or_default().push(ev);
            }
        }
    }
    for group in blockers.into_values() {
        let days: BTreeSet<_> = group
            .iter()
            .filter_map(|e| e.at("local_date").as_str())
            .filter(|s| !s.is_empty())
            .collect();
        if days.len() < 2 {
            continue;
        }
        let latest = group.last().expect("nonempty blocker observations");
        let position = (
            latest.at("occurred_at").as_str().unwrap_or_default(),
            latest.at("event_id").as_str().unwrap_or_default(),
        );
        let blocker = group[0].at("blocker").as_str().unwrap_or_default();
        let clean = sanitize_text(&blocker, 600);
        let lesson = format!(
            "journal-distill: blocker seen {}d {}..{}: {clean}",
            days.len(),
            days.first().unwrap(),
            days.last().unwrap()
        );
        candidates.push((
            position,
            distill_candidate("blocker", &blocker, lesson, &group),
        ));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

pub fn period_range_hint(kind: &str, period: &str) -> String {
    if kind == "weekly"
        && let Some((start, end)) = iso_week_range(period)
    {
        return format!("{start}..{end}");
    }
    period.to_string()
}
