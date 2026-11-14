#![forbid(unsafe_code)]

use crate::cold::{
    ensure_regular_dir_no_symlink, journal_storage, read_regular_no_symlink_bytes,
    read_verified_cold_events, seal_events,
};
use crate::config::{journal_language, notion_repo_excluded, notion_settings};
use crate::render::{compute_period_aggregates, has_blocker, render_daily, render_weekly};
use crate::sanitize::{
    DIGEST_MAX_BYTES, INDEX_SCHEMA_VERSION, RENDERER_VERSION, canonical_json_bytes,
    pretty_json_bytes, sanitize_text, sha256_hex, truncate_utf8_bytes,
};
use crate::schema::{
    ProjectIdentity, atomic_write_bytes, ensure_oms_gitignore, normalize_event,
    resolve_project_identity, validate_canonical_event, work_journal_dir,
};
use crate::tz::{
    TimeZoneSpec, iso_week_range, naive_date_to_string, parse_date_ymd, parse_rfc3339_utc,
    resolve_timezone, utc_rfc3339,
};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
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
    RE.get_or_init(|| Regex::new(r"^wj_[0-9a-f]{16,64}$").expect("valid regex"))
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
}

impl JournalStore {
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
        let quarantine_path = root.join("quarantine.jsonl");
        let index_json_path = root.join("index.json");
        let db_path = root.join("index.sqlite3");
        let daily_dir = root.join("daily");
        let weekly_dir = root.join("weekly");
        let tz = resolve_timezone(timezone_name);

        let seed_events = read_live_seed_events(&events_path);
        let project =
            resolve_project_identity(repo_root, &seed_events, persist_project, None, None)?;

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
        })
    }

    pub fn load_all_events_verified(&self, record_quarantine: bool) -> Result<Vec<Value>, String> {
        let (_manifest, cold_events) = read_verified_cold_events(&self.root)?;
        let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
        let mut ordered = Vec::new();
        for ev in cold_events {
            if let Some(eid) = ev.get("event_id").and_then(Value::as_str) {
                if !by_id.contains_key(eid) {
                    by_id.insert(eid.to_string(), ev.clone());
                    ordered.push(ev);
                }
            }
        }

        let mut bad_lines = Vec::new();
        if let Some(bytes) = read_regular_no_symlink_bytes(&self.events_path)? {
            let text = String::from_utf8_lossy(&bytes);
            for raw_line in text.lines() {
                let t = raw_line.trim();
                if t.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<Value>(t) else {
                    bad_lines.push(t.to_string());
                    continue;
                };
                if validate_canonical_event(&val).is_err() {
                    bad_lines.push(t.to_string());
                    continue;
                }
                if let Some(eid) = val.get("event_id").and_then(Value::as_str) {
                    if !by_id.contains_key(eid) {
                        by_id.insert(eid.to_string(), val.clone());
                        ordered.push(val);
                    }
                }
            }
        }

        if record_quarantine && !bad_lines.is_empty() {
            ensure_oms_gitignore(&self.root)?;
            fs::create_dir_all(&self.root)
                .map_err(|e| format!("failed creating {}: {e}", self.root.display()))?;
            let existing_q = read_regular_no_symlink_bytes(&self.quarantine_path)?
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let existing_set: HashSet<&str> = existing_q.lines().collect();
            let mut to_add = Vec::new();
            for line in &bad_lines {
                if !existing_set.contains(line.as_str()) {
                    to_add.push(line.as_str());
                }
            }
            if !to_add.is_empty() {
                let mut f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.quarantine_path)
                    .map_err(|e| {
                        format!("failed opening {}: {e}", self.quarantine_path.display())
                    })?;
                for line in to_add {
                    writeln!(f, "{line}").map_err(|e| format!("failed writing quarantine: {e}"))?;
                }
                let _ = f.sync_all();
            }
        }

        ordered.sort_by(|a, b| {
            let ka = (
                a.get("occurred_at").and_then(Value::as_str).unwrap_or(""),
                a.get("event_id").and_then(Value::as_str).unwrap_or(""),
            );
            let kb = (
                b.get("occurred_at").and_then(Value::as_str).unwrap_or(""),
                b.get("event_id").and_then(Value::as_str).unwrap_or(""),
            );
            ka.cmp(&kb)
        });
        Ok(ordered)
    }

    fn open_db_connection(&self, create: bool) -> Result<Connection, String> {
        if !create && !self.db_path.exists() {
            return Err("sqlite index does not exist".to_string());
        }
        if let Ok(meta) = fs::symlink_metadata(&self.db_path) {
            if meta.file_type().is_symlink() {
                return Err(format!("refusing symlink index {}", self.db_path.display()));
            }
        }
        if create {
            ensure_oms_gitignore(&self.root)?;
            fs::create_dir_all(&self.root)
                .map_err(|e| format!("failed creating {}: {e}", self.root.display()))?;
        }
        let conn = Connection::open(&self.db_path).map_err(|e| {
            format!(
                "failed opening sqlite index {}: {e}",
                self.db_path.display()
            )
        })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("sqlite busy_timeout failed: {e}"))?;
        Ok(conn)
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
            CREATE INDEX IF NOT EXISTS events_day_active ON events(local_date, active, occurred_at);
            CREATE INDEX IF NOT EXISTS events_week_active ON events(iso_week, active, occurred_at);
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

    fn live_stat(&self) -> (u64, i128) {
        match fs::symlink_metadata(&self.events_path) {
            Ok(m) if !m.file_type().is_symlink() && m.is_file() => {
                let mtime_ns = i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec());
                (m.len(), mtime_ns)
            }
            _ => (0, 0),
        }
    }

    fn cold_digest(&self) -> Result<String, String> {
        let (manifest, _) = read_verified_cold_events(&self.root)?;
        let parts: Vec<String> = manifest
            .segments
            .iter()
            .map(|s| format!("{}:{}", s.rel_path, s.sha256))
            .collect();
        Ok(sha256_hex(parts.join("|").as_bytes()))
    }

    fn ensure_index_current(&self, allow_create: bool) -> Result<Connection, String> {
        let cold_digest = self.cold_digest()?;
        if !self.db_path.exists() {
            if !allow_create {
                return self.rebuild_index_into_conn();
            }
            return self.rebuild_index_into_conn();
        }
        let conn = self.open_db_connection(true)?;
        if Self::init_db_schema(&conn).is_err() {
            drop(conn);
            return self.rebuild_index_into_conn();
        }
        let meta = load_db_metadata(&conn);
        let (size, mtime_ns) = self.live_stat();
        let needs_rebuild = meta.get("schema_version").map(String::as_str)
            != Some(INDEX_SCHEMA_VERSION)
            || meta.get("events_size").map(String::as_str) != Some(&size.to_string())
            || meta.get("events_mtime_ns").map(String::as_str) != Some(&mtime_ns.to_string())
            || meta.get("cold_digest").map(String::as_str) != Some(&cold_digest);
        if needs_rebuild {
            let old_rv = meta.get("renderer_version").cloned();
            let old_periods = list_all_summary_periods(&conn);
            drop(conn);
            let rebuilt = self.rebuild_index_into_conn()?;
            if old_rv.as_deref() != Some(RENDERER_VERSION) {
                for (k, p) in old_periods {
                    let _ = rebuilt.execute(
                        "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES (?1, ?2)",
                        params![k, p],
                    );
                }
            }
            return Ok(rebuilt);
        }
        if meta.get("renderer_version").map(String::as_str) != Some(RENDERER_VERSION) {
            mark_all_periods_dirty(&conn)?;
        }
        Ok(conn)
    }

    fn rebuild_index_into_conn(&self) -> Result<Connection, String> {
        let events = self.load_all_events_verified(true)?;
        let (size, mtime_ns) = self.live_stat();
        let cold_digest = self.cold_digest()?;
        ensure_oms_gitignore(&self.root)?;
        fs::create_dir_all(&self.root)
            .map_err(|e| format!("failed creating {}: {e}", self.root.display()))?;

        let tmp_db = self
            .root
            .join(format!(".index.sqlite3.tmp.{}", std::process::id()));
        if tmp_db.exists() {
            let _ = fs::remove_file(&tmp_db);
        }
        let mut conn = Connection::open(&tmp_db)
            .map_err(|e| format!("failed opening temp index {}: {e}", tmp_db.display()))?;
        Self::init_db_schema(&conn)?;

        let mut superseded_ids: HashSet<String> = HashSet::new();
        for ev in &events {
            if ev.get("event_type").and_then(Value::as_str) == Some("correction") {
                if let Some(sup) = ev.get("supersedes_event_id").and_then(Value::as_str) {
                    superseded_ids.insert(sup.to_string());
                }
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
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let sid = ev
                    .pointer("/source/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let sup = ev.get("supersedes_event_id").and_then(Value::as_str);
                let active = if superseded_ids.contains(eid) {
                    0i64
                } else {
                    1i64
                };
                let payload = String::from_utf8(canonical_json_bytes(ev)?)
                    .map_err(|e| format!("UTF-8 error: {e}"))?;
                tx.execute(
                    "INSERT OR REPLACE INTO events
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
                ("cold_digest", cold_digest),
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
        drop(conn);

        if let Ok(meta) = fs::symlink_metadata(&self.db_path) {
            if meta.file_type().is_symlink() {
                let _ = fs::remove_file(&tmp_db);
                return Err(format!(
                    "refusing to replace symlink {}",
                    self.db_path.display()
                ));
            }
        }
        fs::rename(&tmp_db, &self.db_path)
            .map_err(|e| format!("failed installing {}: {e}", self.db_path.display()))?;
        self.open_db_connection(true)
    }

    pub fn record_event(&self, raw: &Value, now: DateTime<Utc>) -> Result<(Value, bool), String> {
        let normalized = normalize_event(raw, &self.project, &self.tz, now)?;
        let eid = normalized
            .get("event_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing normalized event_id".to_string())?
            .to_string();

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
            if let Ok(val) = serde_json::from_str::<Value>(&existing_payload) {
                return Ok((val, false));
            }
            return Ok((normalized, false));
        }

        ensure_oms_gitignore(&self.root)?;
        fs::create_dir_all(&self.root)
            .map_err(|e| format!("failed creating {}: {e}", self.root.display()))?;
        if let Ok(meta) = fs::symlink_metadata(&self.events_path) {
            if meta.file_type().is_symlink() || (!meta.is_file()) {
                return Err(format!(
                    "refusing non-regular log {}",
                    self.events_path.display()
                ));
            }
        }

        let line_bytes = canonical_json_bytes(&normalized)?;
        let line_str =
            String::from_utf8(line_bytes.clone()).map_err(|e| format!("UTF-8 error: {e}"))?;
        {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.events_path)
                .map_err(|e| format!("failed opening {}: {e}", self.events_path.display()))?;
            let mut write_buf = line_bytes;
            write_buf.push(b'\n');
            file.write_all(&write_buf)
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("failed appending {}: {e}", self.events_path.display()))?;
        }

        let (new_size, new_mtime_ns) = self.live_stat();
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
            .and_then(Value::as_str)
            .unwrap_or_default();
        let sid = normalized
            .pointer("/source/id")
            .and_then(Value::as_str)
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

        if let Some(target_id) = sup {
            if let Some((t_ld, t_iw)) = tx
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
                .ok();
                tx.execute(
                    "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES ('weekly', ?1)",
                    params![t_iw],
                )
                .ok();
            }
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
        .ok();
        tx.execute(
            "INSERT OR REPLACE INTO metadata(key, value) VALUES ('events_mtime_ns', ?1)",
            params![new_mtime_ns.to_string()],
        )
        .ok();
        tx.commit()
            .map_err(|e| format!("sqlite commit failed: {e}"))?;

        Ok((normalized, true))
    }

    pub fn capture_head_commit(&self, now: DateTime<Utc>) -> Result<Option<Value>, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["log", "-1", "--format=%H%n%cI%n%s"])
            .output();
        let Ok(out) = out else {
            return Ok(None);
        };
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let Some(sha) = lines.next().map(str::trim).filter(|s| s.len() >= 7) else {
            return Ok(None);
        };
        let committed_at = lines.next().map(str::trim).unwrap_or("");
        let subject = lines.next().map(str::trim).unwrap_or("commit");

        let last_head_path = self.root.join("last-head");
        if let Some(prev) = read_regular_no_symlink_bytes(&last_head_path)? {
            if String::from_utf8_lossy(&prev).trim() == sha {
                return Ok(None);
            }
        }

        let short: String = sha.chars().take(12).collect();
        let occurred = if let Ok(dt) = parse_rfc3339_utc(committed_at) {
            utc_rfc3339(dt)
        } else {
            utc_rfc3339(now)
        };
        let payload = serde_json::json!({
            "event_type": "commit",
            "occurred_at": occurred,
            "source": {"type": "git-commit", "id": sha},
            "correlation": {"commit": sha},
            "outcome": {"summary": format!("Commit {short}: {subject}"), "status": "recorded"},
            "verification_status": "not_verified",
            "evidence": [{"type": "git-commit", "ref": sha}],
        });
        let (event, _created) = self.record_event(&payload, now)?;
        atomic_write_bytes(&last_head_path, format!("{sha}\n").as_bytes())?;
        Ok(Some(event))
    }

    pub fn materialize(&self) -> Result<MaterializeReport, String> {
        let mut conn = self.ensure_index_current(true)?;
        let meta = load_db_metadata(&conn);
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
            let md_path = if kind == "daily" {
                self.daily_dir.join(format!("{period}.md"))
            } else {
                self.weekly_dir.join(format!("{period}.md"))
            };
            if events.is_empty() {
                if md_path.exists() {
                    let _ = fs::remove_file(&md_path);
                }
                conn.execute(
                    "DELETE FROM summaries WHERE kind = ?1 AND period = ?2",
                    params![kind, period],
                )
                .ok();
                removed += 1;
                continue;
            }
            let rendered = if kind == "daily" {
                render_daily(period, &events, &self.tz, &lang)
            } else {
                render_weekly(period, &events, &self.tz, &lang)
            };
            let content_hash = sha256_hex(rendered.as_bytes());
            let blocker_flag = i64::from(has_blocker(&events));

            let needs_disk_write = match read_regular_no_symlink_bytes(&md_path) {
                Ok(Some(existing)) => existing != rendered.as_bytes(),
                _ => true,
            };
            if needs_disk_write {
                atomic_write_bytes(&md_path, rendered.as_bytes())?;
                written += 1;
            }
            conn.execute(
                "INSERT OR REPLACE INTO summaries(kind, period, content_hash, content, has_blocker)
                VALUES (?1, ?2, ?3, ?4, ?5)",
                params![kind, period, content_hash, rendered, blocker_flag],
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
        .ok();
        tx.commit()
            .map_err(|e| format!("sqlite commit failed: {e}"))?;

        let report = write_index_json(&conn, &self.index_json_path, &self.root)?;
        Ok(MaterializeReport {
            event_count: report.event_count,
            written_periods: written,
            removed_periods: removed,
            daily_periods: report.daily_periods,
            weekly_periods: report.weekly_periods,
        })
    }

    pub fn rebuild(&self) -> Result<MaterializeReport, String> {
        let _conn = self.rebuild_index_into_conn()?;
        // Remove stale markdown summaries that do not belong to any active period
        let report = self.materialize()?;
        prune_stale_markdown(&self.daily_dir, &report.daily_periods)?;
        prune_stale_markdown(&self.weekly_dir, &report.weekly_periods)?;
        Ok(report)
    }

    pub fn summary_text(&self, kind: &str, period: &str) -> Result<Option<String>, String> {
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(None);
        }
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
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(Vec::new());
        }
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare(
                "SELECT payload FROM events WHERE active = 1 ORDER BY occurred_at DESC, event_id DESC LIMIT ?1",
            )
            .map_err(|e| format!("sqlite recent query failed: {e}"))?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))
            .map_err(|e| format!("sqlite recent query map failed: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(|e| format!("sqlite recent row failed: {e}"))?;
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                out.push(v);
            }
        }
        Ok(out)
    }

    pub fn load_events_for_period(&self, kind: &str, period: &str) -> Result<Vec<Value>, String> {
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(Vec::new());
        }
        let conn = self.ensure_index_current(false)?;
        load_active_events_for_period(&conn, kind, period)
    }

    pub fn open_annotations(&self) -> Result<OpenAnnotations, String> {
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(OpenAnnotations::default());
        }
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE active = 1 ORDER BY occurred_at DESC, event_id DESC")
            .map_err(|e| format!("sqlite annotations query failed: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("sqlite annotations map failed: {e}"))?;

        let mut blockers = Vec::new();
        let mut next_actions = Vec::new();
        for r in rows {
            let s = r.map_err(|e| format!("sqlite annotations row failed: {e}"))?;
            let Ok(ev) = serde_json::from_str::<Value>(&s) else {
                continue;
            };
            let eid = ev
                .get("event_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let date = ev
                .get("local_date")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(blk) = ev
                .get("blocker")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                blockers.push(AnnotationRow {
                    event_id: eid.clone(),
                    local_date: date.clone(),
                    text: blk.to_string(),
                });
            }
            if let Some(na) = ev
                .get("next_action")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                next_actions.push(AnnotationRow {
                    event_id: eid,
                    local_date: date,
                    text: na.to_string(),
                });
            }
        }
        Ok(OpenAnnotations {
            blockers,
            next_actions,
        })
    }

    pub fn decisions(&self, since: Option<&str>, limit: usize) -> Result<DecisionsResult, String> {
        if let Some(s) = since {
            if parse_date_ymd(s).is_none() {
                return Err(format!("invalid --since date '{s}'; expected YYYY-MM-DD"));
            }
        }
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(DecisionsResult::default());
        }
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE active = 1 ORDER BY occurred_at DESC, event_id DESC")
            .map_err(|e| format!("sqlite decisions query failed: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("sqlite decisions map failed: {e}"))?;

        let mut seen_text: HashSet<String> = HashSet::new();
        let mut items = Vec::new();
        let mut omitted = 0usize;

        for r in rows {
            let s = r.map_err(|e| format!("sqlite decisions row failed: {e}"))?;
            let Ok(ev) = serde_json::from_str::<Value>(&s) else {
                continue;
            };
            let ld = ev.get("local_date").and_then(Value::as_str).unwrap_or("");
            if let Some(since_date) = since {
                if ld < since_date {
                    continue;
                }
            }
            let Some(dec) = ev
                .get("decision")
                .and_then(Value::as_str)
                .map(str::trim)
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
                    .unwrap_or("")
                    .to_string(),
                occurred_at: ev
                    .get("occurred_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                local_date: ld.to_string(),
                decision: dec.to_string(),
                source_type: ev
                    .pointer("/source/type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                source_id: ev
                    .pointer("/source/id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
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
        let eid = event_id.trim();
        if !event_id_re().is_match(eid) {
            return Err(format!("invalid event ID '{eid}'; expected wj_<hex>"));
        }
        let how_clean = how.trim();
        if how_clean.is_empty() {
            return Err("--how must be non-empty".to_string());
        }

        let conn = self.ensure_index_current(true)?;
        let target_payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM events WHERE event_id = ?1 AND active = 1",
                params![eid],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("sqlite lookup failed: {e}"))?;
        let Some(raw_target) = target_payload else {
            return Err(format!("no active event found with ID '{eid}'"));
        };
        let target: Value = serde_json::from_str(&raw_target)
            .map_err(|e| format!("corrupt target event JSON: {e}"))?;
        if target.get("event_type").and_then(Value::as_str) == Some("correction") {
            return Err(format!("cannot resolve correction event '{eid}'"));
        }
        let target_blocker = target
            .get("blocker")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();

        let correction_payload = serde_json::json!({
            "event_type": "correction",
            "occurred_at": utc_rfc3339(now),
            "source": {"type": "resolve", "id": format!("resolve-{eid}")},
            "supersedes_event_id": eid,
            "outcome": {"summary": format!("Resolved {eid}: {how_clean}"), "status": "resolved"},
            "verification_status": "not_verified",
            "decision": how_clean,
            "evidence": [{"type": "journal-event", "ref": eid}],
        });
        drop(conn);

        let (correction, _) = self.record_event(&correction_payload, now)?;
        self.materialize()?;

        // Check for other remaining active events with the exact same blocker text
        let mut remaining_matching = Vec::new();
        if !target_blocker.is_empty() {
            let conn2 = self.ensure_index_current(true)?;
            let mut stmt = conn2
                .prepare("SELECT event_id, payload FROM events WHERE active = 1")
                .map_err(|e| format!("sqlite remaining blocker query failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("sqlite remaining map failed: {e}"))?;
            for r in rows {
                if let Ok((other_id, payload_s)) = r {
                    if let Ok(v) = serde_json::from_str::<Value>(&payload_s) {
                        if v.get("blocker").and_then(Value::as_str).map(str::trim)
                            == Some(target_blocker.as_str())
                        {
                            remaining_matching.push(other_id);
                        }
                    }
                }
            }
        }

        Ok(ResolveResult {
            superseded_event_id: eid.to_string(),
            correction_event_id: correction
                .get("event_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            remaining_matching_blocker_ids: remaining_matching,
        })
    }

    pub fn summary_rows(&self) -> Result<Vec<SummaryRow>, String> {
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(Vec::new());
        }
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare(
                "SELECT kind, period, content_hash, content, has_blocker FROM summaries ORDER BY period DESC, kind ASC",
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
        Ok(serde_json::json!({
            "sessions": agg.sessions,
            "commits": agg.commits,
            "verified": agg.verified,
            "events": agg.event_count,
        }))
    }

    pub fn write_prompt_stamp(
        &self,
        kind: &str,
        has_digest: bool,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        ensure_oms_gitignore(&self.root)?;
        fs::create_dir_all(&self.root)
            .map_err(|e| format!("failed creating {}: {e}", self.root.display()))?;
        let until_epoch = self.tz.next_local_midnight_epoch(now);
        let flag = if has_digest { "1" } else { "0" };
        let content = format!("{until_epoch}\n{flag}\n");
        let name = if kind == "tick" {
            "prompt-tick"
        } else {
            "prompt-digest"
        };
        atomic_write_bytes(&self.root.join(name), content.as_bytes())
    }

    pub fn prompt_digest(&self, now: DateTime<Utc>) -> Result<Option<String>, String> {
        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(None);
        }
        let conn = self.ensure_index_current(false)?;
        let (today_date, _, _) = self.tz.local_date_and_week(now);
        let cutoff = naive_date_to_string(today_date - Duration::days(7));
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE active = 1 AND local_date >= ?1 ORDER BY occurred_at DESC")
            .map_err(|e| format!("sqlite digest query failed: {e}"))?;
        let rows = stmt
            .query_map(params![cutoff], |row| row.get::<_, String>(0))
            .map_err(|e| format!("sqlite digest map failed: {e}"))?;

        let mut newest_handoff: Option<(String, String)> = None;
        let mut blockers: Vec<String> = Vec::new();
        let mut nexts: Vec<String> = Vec::new();
        let mut event_count = 0usize;
        for r in rows {
            let s = r.map_err(|e| format!("sqlite digest row failed: {e}"))?;
            let Ok(ev) = serde_json::from_str::<Value>(&s) else {
                continue;
            };
            event_count += 1;
            if newest_handoff.is_none()
                && ev.get("event_type").and_then(Value::as_str) == Some("handoff")
            {
                let summary = ev
                    .pointer("/outcome/summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let date = ev
                    .get("local_date")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !summary.is_empty() {
                    newest_handoff = Some((date, sanitize_text(&summary, 180)));
                }
            }
            if blockers.len() < 3 {
                if let Some(b) = ev
                    .get("blocker")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                {
                    let clean = sanitize_text(b, 140);
                    if !blockers.contains(&clean) {
                        blockers.push(clean);
                    }
                }
            }
            if nexts.len() < 3 {
                if let Some(n) = ev
                    .get("next_action")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                {
                    let clean = sanitize_text(n, 140);
                    if !nexts.contains(&clean) {
                        nexts.push(clean);
                    }
                }
            }
        }
        if event_count == 0 {
            return Ok(None);
        }
        let mut lines = vec![format!("Work Journal ({event_count} recent events):")];
        if let Some((d, h)) = newest_handoff {
            lines.push(format!("- Latest handoff ({d}): {h}"));
        }
        for b in blockers {
            lines.push(format!("- Blocker: {b}"));
        }
        for n in nexts {
            lines.push(format!("- Next: {n}"));
        }
        let joined = lines.join("\n");
        Ok(Some(truncate_utf8_bytes(&joined, DIGEST_MAX_BYTES)))
    }

    pub fn distill(
        &self,
        dry_run: bool,
        memory_append: Option<MemoryAppendFn<'_>>,
        now: DateTime<Utc>,
    ) -> Result<DistillReport, String> {
        let state_path = self.root.join("distill.json");
        let marker: Option<(String, String)> = read_regular_no_symlink_bytes(&state_path)?
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|row| {
                if row.get("schema_version").and_then(Value::as_i64) != Some(1) {
                    return None;
                }
                let through = row.get("through")?.as_object()?;
                let occ = through.get("occurred_at")?.as_str()?.to_string();
                let eid = through.get("event_id")?.as_str()?.to_string();
                Some((occ, eid))
            });

        if !self.events_path.exists() && !ensure_regular_dir_no_symlink(&self.root.join("cold"))? {
            return Ok(DistillReport::default());
        }
        let conn = self.ensure_index_current(false)?;
        let mut stmt = conn
            .prepare(
                "SELECT event_id, occurred_at, payload FROM events WHERE active = 1 ORDER BY occurred_at ASC, event_id ASC",
            )
            .map_err(|e| format!("sqlite distill query failed: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("sqlite distill map failed: {e}"))?;

        let mut all_events = Vec::new();
        let mut pending = Vec::new();
        for r in rows {
            let (eid, occ, payload_s) = r.map_err(|e| format!("sqlite distill row failed: {e}"))?;
            let Ok(ev) = serde_json::from_str::<Value>(&payload_s) else {
                continue;
            };
            let key = (occ, eid);
            let is_pending = match &marker {
                None => true,
                Some(m) => key > *m,
            };
            all_events.push((key, ev.clone()));
            if is_pending {
                pending.push(ev);
            }
        }

        let shared_norm = read_regular_no_symlink_bytes(
            &self.repo_root.join(".oms").join("memory").join("shared.md"),
        )
        .ok()
        .flatten()
        .map(|b| distill_normalized_text(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();

        let (lessons, dropped, deduped) = distill_candidates(&pending, &shared_norm);
        if dry_run {
            return Ok(DistillReport {
                promoted: lessons.len(),
                dropped,
                deduped,
                skipped_refused: Vec::new(),
                dry_run: true,
                lessons,
            });
        }

        let mut promoted_lessons = Vec::new();
        let mut skipped_refused = Vec::new();
        if !lessons.is_empty() {
            let append_fn = memory_append.ok_or_else(|| {
                "native memory append adapter is not wired; parent CLI must supply MemoryAppendFn via run_with_services".to_string()
            })?;
            for lesson in &lessons {
                if lesson.contains("<!-- oms-memory")
                    || oms_scrub::scan(lesson.as_bytes(), &[])
                        .map(|hits| !hits.is_empty())
                        .unwrap_or(true)
                {
                    skipped_refused.push(lesson.clone());
                    continue;
                }
                match append_fn(&self.repo_root, "journal", lesson) {
                    Ok(MemoryAppendOutcome::Appended) => {
                        promoted_lessons.push(lesson.clone());
                    }
                    Ok(MemoryAppendOutcome::Refused(_)) | Err(_) => {
                        skipped_refused.push(lesson.clone());
                    }
                }
            }
        }

        let through_val =
            if let Some(((occ, eid), _)) = all_events.iter().max_by(|a, b| a.0.cmp(&b.0)) {
                serde_json::json!({
                    "occurred_at": occ,
                    "event_id": eid,
                })
            } else {
                Value::Null
            };

        let (_, today_str, _) = self.tz.local_date_and_week(now);
        let mut state_obj = Map::new();
        state_obj.insert("schema_version".into(), Value::Number(1.into()));
        state_obj.insert("through".into(), through_val);
        state_obj.insert("last_run_date".into(), Value::String(today_str));
        let bytes = pretty_json_bytes(&Value::Object(state_obj))?;
        atomic_write_bytes(&state_path, &bytes)?;

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
        if let Some(bytes) = read_regular_no_symlink_bytes(&state_path)? {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if v.get("schema_version").and_then(Value::as_i64) == Some(1)
                    && v.get("last_run_date").and_then(Value::as_str) == Some(today.as_str())
                {
                    return Ok(false);
                }
            }
        }
        let report = self.distill(true, None, now)?;
        Ok(report.promoted > 0 || report.dropped > 0 || report.deduped > 0)
    }

    pub fn status_report(&self) -> Result<Value, String> {
        let storage = journal_storage(&self.repo_root);
        let mut active_events = 0usize;
        let mut daily_periods = Vec::new();
        let mut weekly_periods = Vec::new();
        if self.db_path.exists() {
            if let Ok(conn) = self.ensure_index_current(false) {
                active_events = conn
                    .query_row("SELECT COUNT(*) FROM events WHERE active = 1", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .unwrap_or(0) as usize;
                let all = list_all_summary_periods(&conn);
                for (k, p) in all {
                    if k == "daily" {
                        daily_periods.push(p);
                    } else if k == "weekly" {
                        weekly_periods.push(p);
                    }
                }
            }
        }
        let settings = notion_settings();
        let excluded = notion_repo_excluded(&self.repo_root);
        let sync_state = read_regular_no_symlink_bytes(&self.root.join("notion-sync-state.json"))?
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        Ok(serde_json::json!({
            "project": {
                "id": self.project.id,
                "name": self.project.name,
                "github_repository": self.project.github_repository,
                "origin_remote": self.project.origin_remote,
            },
            "timezone": self.tz.name(),
            "language": journal_language(Some(&self.tz)),
            "active_events": active_events,
            "daily_periods": daily_periods,
            "weekly_periods": weekly_periods,
            "storage": storage.to_json(),
            "notion": {
                "configured": settings.is_configured(),
                "excluded": excluded,
                "transport": settings.transport,
                "auth_mode": settings.auth_mode,
                "data_source_id": settings.target_id(),
                "sync_state": sync_state,
            }
        }))
    }

    pub fn seal(&self, apply: bool, now: DateTime<Utc>) -> Result<usize, String> {
        seal_events(&self.repo_root, &self.tz, now, apply)
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
}

#[derive(Clone, Debug, Default)]
pub struct OpenAnnotations {
    pub blockers: Vec<AnnotationRow>,
    pub next_actions: Vec<AnnotationRow>,
}

#[derive(Clone, Debug)]
pub struct DecisionRow {
    pub event_id: String,
    pub occurred_at: String,
    pub local_date: String,
    pub decision: String,
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

fn read_live_seed_events(events_path: &Path) -> Vec<Value> {
    let Ok(Some(bytes)) = read_regular_no_symlink_bytes(events_path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            if v.is_object() {
                out.push(v);
            }
        }
    }
    out
}

fn load_db_metadata(conn: &Connection) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT key, value FROM metadata") else {
        return map;
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return map;
    };
    for r in rows.flatten() {
        map.insert(r.0, r.1);
    }
    map
}

fn list_all_summary_periods(conn: &Connection) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare("SELECT kind, period FROM summaries ORDER BY kind, period")
    else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return out;
    };
    for r in rows.flatten() {
        out.push(r);
    }
    out
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
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            out.push(v);
        }
    }
    Ok(out)
}

fn write_index_json(
    conn: &Connection,
    index_json_path: &Path,
    journal_dir: &Path,
) -> Result<MaterializeReport, String> {
    let event_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events WHERE active = 1", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let mut daily_periods = Vec::new();
    let mut weekly_periods = Vec::new();
    for (k, p) in list_all_summary_periods(conn) {
        if k == "daily" {
            daily_periods.push(p);
        } else if k == "weekly" {
            weekly_periods.push(p);
        }
    }
    let cold_segments = crate::cold::load_cold_manifest(journal_dir)
        .map(|m| m.segments.len())
        .unwrap_or(0);
    let obj = serde_json::json!({
        "schema_version": INDEX_SCHEMA_VERSION,
        "renderer_version": RENDERER_VERSION,
        "event_count": event_count,
        "daily_periods": daily_periods,
        "weekly_periods": weekly_periods,
        "cold_segments": cold_segments,
    });
    let bytes = pretty_json_bytes(&obj)?;
    atomic_write_bytes(index_json_path, &bytes)?;
    Ok(MaterializeReport {
        event_count: event_count as usize,
        written_periods: 0,
        removed_periods: 0,
        daily_periods,
        weekly_periods,
    })
}

fn prune_stale_markdown(dir: &Path, keep_periods: &[String]) -> Result<(), String> {
    if !ensure_regular_dir_no_symlink(dir)? {
        return Ok(());
    }
    let keep: HashSet<String> = keep_periods.iter().map(|p| format!("{p}.md")).collect();
    let entries =
        fs::read_dir(dir).map_err(|e| format!("failed reading {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let fname_os = entry.file_name();
        let Some(fname) = fname_os.to_str() else {
            continue;
        };
        if fname.ends_with(".md") && !keep.contains(fname) {
            let p = entry.path();
            if fs::symlink_metadata(&p)
                .map(|m| m.is_file() && !m.file_type().is_symlink())
                .unwrap_or(false)
            {
                let _ = fs::remove_file(&p);
            }
        }
    }
    Ok(())
}

fn distill_normalized_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn distill_candidates(events: &[Value], promoted_norm: &str) -> (Vec<String>, usize, usize) {
    let mut blockers: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut candidates: Vec<((String, String), String, String)> = Vec::new();
    let mut seen_decisions: HashSet<String> = HashSet::new();

    for ev in events {
        let occ = ev
            .get("occurred_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let eid = ev
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let ld = ev.get("local_date").and_then(Value::as_str).unwrap_or("");

        if let Some(dec) = ev.get("decision").and_then(Value::as_str) {
            let dkey = distill_normalized_text(dec);
            if !dkey.is_empty() && seen_decisions.insert(dkey) {
                let clean = sanitize_text(dec, 600);
                let needle = distill_normalized_text(&clean);
                candidates.push((
                    (occ.clone(), eid.clone()),
                    format!("journal-distill: decision {ld}: {clean}"),
                    needle,
                ));
            }
        }
        if let Some(blk) = ev.get("blocker").and_then(Value::as_str) {
            let bkey = distill_normalized_text(blk);
            if !bkey.is_empty() {
                blockers.entry(bkey).or_default().push(ev);
            }
        }
    }

    for group in blockers.into_values() {
        let mut days: BTreeSet<&str> = BTreeSet::new();
        let mut latest: Option<(String, String)> = None;
        for ev in &group {
            if let Some(ld) = ev.get("local_date").and_then(Value::as_str) {
                if !ld.is_empty() {
                    days.insert(ld);
                }
            }
            let pos = (
                ev.get("occurred_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                ev.get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
            if latest.as_ref().is_none_or(|cur| pos > *cur) {
                latest = Some(pos);
            }
        }
        if days.len() < 2 {
            continue;
        }
        let first_day = days.iter().next().copied().unwrap_or("");
        let last_day = days.iter().next_back().copied().unwrap_or("");
        let raw_blk = group[0]
            .get("blocker")
            .and_then(Value::as_str)
            .unwrap_or("");
        let clean = sanitize_text(raw_blk, 600);
        let needle = distill_normalized_text(&clean);
        candidates.push((
            latest.unwrap_or_default(),
            format!(
                "journal-distill: blocker seen {}d {first_day}..{last_day}: {clean}",
                days.len()
            ),
            needle,
        ));
    }

    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let mut fresh = Vec::new();
    let mut deduped = 0usize;
    for (_pos, lesson, needle) in candidates {
        if !promoted_norm.is_empty() && !needle.is_empty() && promoted_norm.contains(&needle) {
            deduped += 1;
            continue;
        }
        fresh.push(lesson);
    }
    let dropped = fresh.len().saturating_sub(5);
    fresh.truncate(5);
    (fresh, dropped, deduped)
}

pub fn period_range_hint(kind: &str, period: &str) -> String {
    if kind == "weekly" {
        if let Some((start, end)) = iso_week_range(period) {
            return format!("{start}..{end}");
        }
    }
    period.to_string()
}
