#![forbid(unsafe_code)]

use crate::sanitize::{
    COLD_SCHEMA_VERSION, JOURNAL_FILE_COUNT_LIMIT, LIVE_EVENT_LINE_LIMIT, LIVE_FILE_BYTE_LIMIT,
    canonical_json_bytes, sha256_hex,
};
use crate::schema::{atomic_write_bytes, ensure_oms_gitignore, validate_canonical_event};
use crate::tz::{TimeZoneSpec, naive_date_to_string};
use chrono::Datelike as _;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct StorageHealth {
    pub healthy: bool,
    pub has_cold_segments: bool,
    pub event_lines: usize,
    pub events_bytes: u64,
    pub file_count: usize,
    pub cold_events: usize,
    pub cold_segments: usize,
    pub threshold_exceeded: Vec<String>,
    pub threshold_note: String,
    pub error: Option<String>,
}

impl StorageHealth {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("healthy".into(), Value::Bool(self.healthy));
        m.insert("cold_partition".into(), Value::Bool(self.has_cold_segments));
        m.insert(
            "event_lines".into(),
            Value::Number((self.event_lines as u64).into()),
        );
        m.insert(
            "events_bytes".into(),
            Value::Number(self.events_bytes.into()),
        );
        m.insert(
            "file_count".into(),
            Value::Number((self.file_count as u64).into()),
        );
        m.insert(
            "cold_events".into(),
            Value::Number((self.cold_events as u64).into()),
        );
        m.insert(
            "cold_segments".into(),
            Value::Number((self.cold_segments as u64).into()),
        );
        m.insert(
            "threshold_exceeded".into(),
            Value::Array(
                self.threshold_exceeded
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        m.insert(
            "threshold_note".into(),
            Value::String(self.threshold_note.clone()),
        );
        if let Some(err) = &self.error {
            m.insert("error".into(), Value::String(err.clone()));
        }
        Value::Object(m)
    }
}

#[derive(Clone, Debug)]
pub struct ColdSegmentEntry {
    pub rel_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ColdManifest {
    pub segments: Vec<ColdSegmentEntry>,
    pub pending_live_prune: BTreeSet<String>,
}

pub fn read_regular_no_symlink_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("failed to inspect {}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("refusing non-regular file {}", path.display()));
    }
    let mut f = File::open(path).map_err(|e| format!("failed reading {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .map_err(|e| format!("failed reading {}: {e}", path.display()))?;
    Ok(Some(buf))
}

pub fn ensure_regular_dir_no_symlink(dir: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(dir) {
        Ok(m) => {
            if m.file_type().is_symlink() || !m.is_dir() {
                return Err(format!("refusing non-directory {}", dir.display()));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("failed to inspect {}: {e}", dir.display())),
    }
}

pub fn cold_dir(journal_dir: &Path) -> PathBuf {
    journal_dir.join("cold")
}

pub fn cold_manifest_path(journal_dir: &Path) -> PathBuf {
    cold_dir(journal_dir).join("manifest.json")
}

pub fn load_cold_manifest(journal_dir: &Path) -> Result<ColdManifest, String> {
    let cdir = cold_dir(journal_dir);
    if !ensure_regular_dir_no_symlink(&cdir)? {
        return Ok(ColdManifest::default());
    }
    let mpath = cold_manifest_path(journal_dir);
    let Some(bytes) = read_regular_no_symlink_bytes(&mpath)? else {
        // Check if orphan .jsonl segment files exist without manifest
        let has_segments = fs::read_dir(&cdir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|e| {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt as _;
                    e.file_name().as_bytes().ends_with(b".jsonl")
                }
                #[cfg(not(unix))]
                {
                    e.file_name()
                        .to_str()
                        .is_some_and(|s| s.ends_with(".jsonl"))
                }
            });
        if has_segments {
            return Err("cold journal segment digest mismatch: missing cold manifest".to_string());
        }
        return Ok(ColdManifest::default());
    };
    let parsed: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("cold journal segment digest mismatch: invalid manifest ({e})"))?;
    let Some(obj) = parsed.as_object() else {
        return Err("cold journal segment digest mismatch: manifest is not an object".to_string());
    };
    if obj.get("schema_version").and_then(Value::as_str) != Some(COLD_SCHEMA_VERSION) {
        return Err(
            "cold journal segment digest mismatch: unsupported cold schema_version".to_string(),
        );
    }
    let mut segments = Vec::new();
    if let Some(arr) = obj.get("segments").and_then(Value::as_array) {
        for item in arr {
            let Some(seg) = item.as_object() else {
                return Err(
                    "cold journal segment digest mismatch: invalid segment entry".to_string(),
                );
            };
            let rel_path = seg
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "cold journal segment digest mismatch: missing path".to_string())?;
            let sha256 = seg.get("sha256").and_then(Value::as_str).ok_or_else(|| {
                "cold journal segment digest mismatch: missing sha256".to_string()
            })?;
            let bytes = seg
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| "cold journal segment digest mismatch: missing bytes".to_string())?;
            let count =
                seg.get("count").and_then(Value::as_u64).ok_or_else(|| {
                    "cold journal segment digest mismatch: missing count".to_string()
                })? as usize;
            segments.push(ColdSegmentEntry {
                rel_path: rel_path.to_string(),
                sha256: sha256.to_string(),
                bytes,
                count,
            });
        }
    }
    let mut pending = BTreeSet::new();
    if let Some(arr) = obj.get("pending_live_prune").and_then(Value::as_array) {
        for item in arr {
            if let Some(id) = item.as_str() {
                pending.insert(id.to_string());
            }
        }
    }
    Ok(ColdManifest {
        segments,
        pending_live_prune: pending,
    })
}

pub fn save_cold_manifest(journal_dir: &Path, manifest: &ColdManifest) -> Result<(), String> {
    let segs: Vec<Value> = manifest
        .segments
        .iter()
        .map(|s| {
            let mut m = Map::new();
            m.insert("path".into(), Value::String(s.rel_path.clone()));
            m.insert("sha256".into(), Value::String(s.sha256.clone()));
            m.insert("bytes".into(), Value::Number(s.bytes.into()));
            m.insert("count".into(), Value::Number((s.count as u64).into()));
            Value::Object(m)
        })
        .collect();
    let mut root = Map::new();
    root.insert(
        "schema_version".into(),
        Value::String(COLD_SCHEMA_VERSION.to_string()),
    );
    root.insert("segments".into(), Value::Array(segs));
    if !manifest.pending_live_prune.is_empty() {
        root.insert(
            "pending_live_prune".into(),
            Value::Array(
                manifest
                    .pending_live_prune
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    let bytes = crate::sanitize::pretty_json_bytes(&Value::Object(root))?;
    atomic_write_bytes(&cold_manifest_path(journal_dir), &bytes)
}

fn resolve_segment_path(journal_dir: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel_path);
    if p.is_absolute() || rel_path.contains("..") {
        return Err("cold journal segment digest mismatch: invalid segment path".to_string());
    }
    let resolved = if rel_path.starts_with("cold/") {
        journal_dir.join(rel_path)
    } else {
        cold_dir(journal_dir).join(rel_path)
    };
    Ok(resolved)
}

pub fn read_verified_cold_events(journal_dir: &Path) -> Result<(ColdManifest, Vec<Value>), String> {
    let manifest = load_cold_manifest(journal_dir)?;
    let mut events = Vec::new();
    for seg in &manifest.segments {
        let seg_path = resolve_segment_path(journal_dir, &seg.rel_path)?;
        let Some(bytes) = read_regular_no_symlink_bytes(&seg_path)
            .map_err(|e| format!("cold journal segment digest mismatch: {e}"))?
        else {
            return Err(format!(
                "cold journal segment digest mismatch: missing {}",
                seg.rel_path
            ));
        };
        if bytes.len() as u64 != seg.bytes || sha256_hex(&bytes) != seg.sha256 {
            return Err(format!(
                "cold journal segment digest mismatch for {}",
                seg.rel_path
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            format!(
                "cold journal segment digest mismatch: non-UTF8 segment {}",
                seg.rel_path
            )
        })?;
        let mut parsed_count = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let val: Value = serde_json::from_str(line).map_err(|_| {
                format!(
                    "cold journal segment digest mismatch: malformed JSON in {}",
                    seg.rel_path
                )
            })?;
            validate_canonical_event(&val).map_err(|_| {
                format!(
                    "cold journal segment digest mismatch: invalid event in {}",
                    seg.rel_path
                )
            })?;
            events.push(val);
            parsed_count += 1;
        }
        if parsed_count != seg.count {
            return Err(format!(
                "cold journal segment digest mismatch: count mismatch in {}",
                seg.rel_path
            ));
        }
    }
    Ok((manifest, events))
}

pub fn journal_storage(repo_root: &Path) -> StorageHealth {
    let journal_dir = repo_root.join(".oms").join("work-journal");
    let mut health = StorageHealth {
        healthy: true,
        ..Default::default()
    };

    if let Ok(meta) = fs::symlink_metadata(&journal_dir) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            health.healthy = false;
            health.error = Some("journal directory is not a regular directory".into());
            return health;
        }
    } else {
        return health;
    }

    health.file_count = count_files_recursive(&journal_dir);

    let events_path = journal_dir.join("events.jsonl");
    match fs::symlink_metadata(&events_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                health.healthy = false;
                health.error = Some("events.jsonl is not a regular file".into());
                return health;
            }
            match fs::read(&events_path) {
                Ok(bytes) => {
                    health.events_bytes = bytes.len() as u64;
                    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                        health.healthy = false;
                        health.error = Some("events.jsonl ends with a partial line".into());
                    }
                    health.event_lines = bytes
                        .split(|&b| b == b'\n')
                        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
                        .count();
                }
                Err(e) => {
                    health.healthy = false;
                    health.error = Some(format!("failed reading events.jsonl: {e}"));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            health.healthy = false;
            health.error = Some(format!("failed inspecting events.jsonl: {e}"));
        }
    }

    match read_verified_cold_events(&journal_dir) {
        Ok((manifest, _)) => {
            health.cold_segments = manifest.segments.len();
            health.cold_events = manifest.segments.iter().map(|s| s.count).sum();
            health.has_cold_segments = !manifest.segments.is_empty();
        }
        Err(e) => {
            health.healthy = false;
            health.error = Some(e);
        }
    }

    if health.event_lines > LIVE_EVENT_LINE_LIMIT {
        health.threshold_exceeded.push("event_lines".into());
    }
    if health.events_bytes >= LIVE_FILE_BYTE_LIMIT {
        health.threshold_exceeded.push("events_bytes".into());
    }
    if health.file_count > JOURNAL_FILE_COUNT_LIMIT {
        health.threshold_exceeded.push("file_count".into());
    }
    if !health.threshold_exceeded.is_empty() {
        health.threshold_note = format!(
            "live journal exceeds threshold ({}); run `oms journal seal --apply` to partition closed months",
            health.threshold_exceeded.join(", ")
        );
    }

    health
}

fn count_files_recursive(dir: &Path) -> usize {
    let mut total = 0usize;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if let Ok(meta) = fs::symlink_metadata(&p) {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                total += count_files_recursive(&p);
            } else {
                total += 1;
            }
        }
    }
    total
}

pub fn seal_report_text(dry_run: bool, count: usize) -> String {
    if dry_run {
        format!("dry-run: {count} events would be sealed\n")
    } else {
        format!("sealed: {count} events\n")
    }
}

pub fn seal_events(
    repo_root: &Path,
    tz: &TimeZoneSpec,
    now: chrono::DateTime<chrono::Utc>,
    apply: bool,
) -> Result<usize, String> {
    let journal_dir = repo_root.join(".oms").join("work-journal");
    if !journal_dir.exists() {
        return Ok(0);
    }
    let live_path = journal_dir.join("events.jsonl");
    let Some(live_bytes) = read_regular_no_symlink_bytes(&live_path)? else {
        return Ok(0);
    };

    let (mut manifest, existing_cold) = read_verified_cold_events(&journal_dir)?;
    let (now_local_date, _, _) = tz.local_date_and_week(now);
    let current_month_prefix =
        format!("{:04}-{:02}", now_local_date.year(), now_local_date.month());
    let today_ymd = naive_date_to_string(now_local_date);

    // Partition live lines into: sealable valid events, remaining live valid events, and preserved raw/quarantine lines
    let live_text = std::str::from_utf8(&live_bytes)
        .map_err(|_| "events.jsonl contains non-UTF8 bytes".to_string())?;
    let mut existing_cold_ids: HashSet<String> = existing_cold
        .iter()
        .filter_map(|e| {
            e.get("event_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect();

    let mut seal_buckets: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut keep_lines: Vec<String> = Vec::new();
    let mut sealed_ids: Vec<String> = Vec::new();

    for raw_line in live_text.lines() {
        if raw_line.trim().is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(raw_line) else {
            keep_lines.push(raw_line.to_string());
            continue;
        };
        if validate_canonical_event(&val).is_err() {
            keep_lines.push(raw_line.to_string());
            continue;
        }
        let eid = val
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if existing_cold_ids.contains(&eid) {
            // Replay from interrupted prior seal: already safely committed in cold
            continue;
        }
        let local_date = val
            .get("local_date")
            .and_then(Value::as_str)
            .unwrap_or(&today_ymd);
        let month_key = local_date.get(..7).unwrap_or(&current_month_prefix);
        if month_key < current_month_prefix.as_str() {
            seal_buckets
                .entry(month_key.to_string())
                .or_default()
                .push(val);
            sealed_ids.push(eid);
        } else {
            keep_lines.push(raw_line.to_string());
        }
    }

    let seal_count = sealed_ids.len();
    if !apply || seal_count == 0 {
        return Ok(seal_count);
    }

    ensure_oms_gitignore(&journal_dir)?;
    let cdir = cold_dir(&journal_dir);
    fs::create_dir_all(&cdir).map_err(|e| format!("failed creating {}: {e}", cdir.display()))?;

    // Merge with any existing cold events per month segment
    let mut by_month: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for ev in existing_cold {
        let m = ev
            .get("local_date")
            .and_then(Value::as_str)
            .and_then(|d| d.get(..7))
            .unwrap_or("unknown")
            .to_string();
        by_month.entry(m).or_default().push(ev);
    }
    for (month, new_evs) in seal_buckets {
        let entry = by_month.entry(month).or_default();
        for ev in new_evs {
            if let Some(eid) = ev.get("event_id").and_then(Value::as_str) {
                if existing_cold_ids.insert(eid.to_string()) {
                    entry.push(ev);
                }
            }
        }
    }

    let mut new_segments = Vec::new();
    for (month, mut evs) in by_month {
        evs.sort_by(|a, b| {
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
        let mut buf = Vec::new();
        for ev in &evs {
            let line = canonical_json_bytes(ev)?;
            buf.extend_from_slice(&line);
            buf.push(b'\n');
        }
        let rel_path = format!("cold/{month}.jsonl");
        let seg_path = resolve_segment_path(&journal_dir, &rel_path)?;
        atomic_write_bytes(&seg_path, &buf)?;
        new_segments.push(ColdSegmentEntry {
            rel_path,
            sha256: sha256_hex(&buf),
            bytes: buf.len() as u64,
            count: evs.len(),
        });
    }

    // Publication boundary 1: persist manifest with pending_live_prune marker
    manifest.segments = new_segments;
    for eid in &sealed_ids {
        manifest.pending_live_prune.insert(eid.clone());
    }
    save_cold_manifest(&journal_dir, &manifest)?;

    // Publication boundary 2: replace live events.jsonl atomically with only unsealed/quarantined rows
    let mut new_live = Vec::new();
    for line in keep_lines {
        new_live.extend_from_slice(line.as_bytes());
        new_live.push(b'\n');
    }
    atomic_write_bytes(&live_path, &new_live)?;

    // Publication boundary 3: clear pending_live_prune marker
    manifest.pending_live_prune.clear();
    save_cold_manifest(&journal_dir, &manifest)?;

    Ok(seal_count)
}
