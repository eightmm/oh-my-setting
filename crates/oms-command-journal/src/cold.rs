#![forbid(unsafe_code)]

use crate::json::{JsonExt, Map, MapExt, Value};
use crate::sanitize::{
    COLD_SCHEMA_VERSION, JOURNAL_FILE_COUNT_LIMIT, LIVE_EVENT_LINE_LIMIT, LIVE_FILE_BYTE_LIMIT,
    canonical_json_bytes, sha256_hex,
};
use crate::schema::{atomic_write_bytes, ensure_oms_gitignore, validate_canonical_event};
use crate::tz::{TimeZoneSpec, naive_date_to_string};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct StorageHealth {
    pub healthy: bool,
    pub present: bool,
    pub total_bytes: u64,
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
        crate::json::json!({"present":self.present,"healthy":self.healthy,"event_lines":self.event_lines,
            "events_bytes":self.events_bytes,"file_count":self.file_count,"total_bytes":self.total_bytes,
            "thresholds":{"event_lines":LIVE_EVENT_LINE_LIMIT,"events_bytes":LIVE_FILE_BYTE_LIMIT,"file_count":JOURNAL_FILE_COUNT_LIMIT},
            "threshold_exceeded":self.threshold_exceeded,"cold_partition":self.has_cold_segments,
            "threshold_note":self.threshold_note})
    }
}

pub struct ColdSegmentEntry {
    pub file: String,
    pub month: String,
    pub count: usize,
    pub first_event_id: Value,
    pub last_event_id: Value,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingSeal {
    pub before: String,
    pub after: String,
    pub cutoff: String,
}

#[derive(Clone, PartialEq)]
pub struct ColdManifest {
    pub schema_version: u64,
    pub segments: Vec<ColdSegmentEntry>,
    pub pending: Option<PendingSeal>,
    pending_null: bool,
}

impl Clone for ColdSegmentEntry {
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            month: self.month.clone(),
            count: self.count,
            first_event_id: self.first_event_id.copy(),
            last_event_id: self.last_event_id.copy(),
            sha256: self.sha256.clone(),
        }
    }
}
impl PartialEq for ColdSegmentEntry {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file
            && self.month == other.month
            && self.count == other.count
            && self.sha256 == other.sha256
            && oms_json::ordered_values_equal(&self.first_event_id, &other.first_event_id)
            && oms_json::ordered_values_equal(&self.last_event_id, &other.last_event_id)
    }
}

fn checked_fields<'a>(
    value: &'a Value,
    allowed: &[&str],
    required: &[&str],
) -> Result<&'a Map, String> {
    let obj = value.as_object().ok_or("invalid cold journal manifest")?;
    if obj.iter().any(|(key, _)| {
        !allowed
            .iter()
            .any(|name| key == &oms_json::PyString::from(*name))
    }) || required.iter().any(|name| obj.get(name).is_none())
    {
        return Err("invalid cold journal manifest".into());
    }
    Ok(obj)
}
fn manifest_text(obj: &Map, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| "invalid cold journal manifest".into())
}
fn parse_manifest(original: &Value) -> Result<ColdManifest, String> {
    let obj = checked_fields(
        original,
        &["schema_version", "segments", "pending"],
        &["schema_version", "segments"],
    )?;
    let schema_version = obj
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or("invalid cold journal manifest")?;
    let mut segments = Vec::new();
    for row in obj
        .get("segments")
        .and_then(Value::as_array)
        .ok_or("invalid cold journal manifest")?
    {
        let fields = &[
            "file",
            "month",
            "count",
            "first_event_id",
            "last_event_id",
            "sha256",
        ];
        let row = checked_fields(row, fields, fields)?;
        segments.push(ColdSegmentEntry {
            file: manifest_text(row, "file")?,
            month: manifest_text(row, "month")?,
            count: row
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("invalid cold journal manifest")?,
            first_event_id: row.at("first_event_id").copy(),
            last_event_id: row.at("last_event_id").copy(),
            sha256: manifest_text(row, "sha256")?,
        });
    }
    let pending = match obj.get("pending") {
        None | Some(Value::Null) => None,
        Some(row) => {
            let fields = &["before", "after", "cutoff"];
            let row = checked_fields(row, fields, fields)?;
            Some(PendingSeal {
                before: manifest_text(row, "before")?,
                after: manifest_text(row, "after")?,
                cutoff: manifest_text(row, "cutoff")?,
            })
        }
    };
    Ok(ColdManifest {
        schema_version,
        segments,
        pending,
        pending_null: obj.get("pending").is_some_and(Value::is_null),
    })
}

impl Default for ColdManifest {
    fn default() -> Self {
        Self {
            schema_version: COLD_SCHEMA_VERSION,
            segments: Vec::new(),
            pending: None,
            pending_null: false,
        }
    }
}

pub fn read_regular_no_symlink_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let (root, relative) = crate::schema::storage_root(path)?;
    use std::os::unix::fs::MetadataExt;
    let before = match fs::symlink_metadata(path) {
        Ok(info) => Some(info),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("journal file unavailable: {e}")),
    };
    let bound = before
        .as_ref()
        .map(|info| usize::try_from(info.len()).map_err(|_| "journal file size is not addressable"))
        .transpose()?
        .unwrap_or(0);
    let bytes = match root.read_no_follow(
        &relative,
        oms_safe_fs::ByteReadOptions {
            missing_ok: false,
            max_bytes: bound,
        },
    ) {
        Ok(bytes) => bytes,
        Err(oms_safe_fs::ByteError::Missing) if before.is_none() => return Ok(None),
        Err(error) => return Err(format!("journal file unavailable or changed: {error:?}")),
    };
    let after = fs::symlink_metadata(path).map_err(|_| "journal file changed after read")?;
    if bytes.len() != bound
        || before.as_ref().is_some_and(|before| {
            (
                before.dev(),
                before.ino(),
                before.len(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec(),
                before.nlink(),
                before.mode(),
                before.uid(),
            ) != (
                after.dev(),
                after.ino(),
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec(),
                after.nlink(),
                after.mode(),
                after.uid(),
            )
        })
    {
        return Err("journal file changed during read".into());
    }
    Ok(Some(bytes))
}

pub fn ensure_regular_dir_no_symlink(dir: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("cold journal requires a real directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("cold journal directory unavailable".into()),
    }
}

pub fn cold_dir(journal_dir: &Path) -> PathBuf {
    journal_dir.join("cold")
}
pub fn cold_manifest_path(journal_dir: &Path) -> PathBuf {
    cold_dir(journal_dir).join("manifest.json")
}

fn valid_month(month: &str) -> bool {
    let b = month.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && matches!(
            &b[5..],
            b"01"
                | b"02"
                | b"03"
                | b"04"
                | b"05"
                | b"06"
                | b"07"
                | b"08"
                | b"09"
                | b"10"
                | b"11"
                | b"12"
        )
}
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn load_cold_manifest(journal_dir: &Path) -> Result<ColdManifest, String> {
    let cdir = cold_dir(journal_dir);
    if !ensure_regular_dir_no_symlink(&cdir)? {
        return Ok(ColdManifest::default());
    }
    for dir in [journal_dir.parent(), Some(journal_dir)]
        .into_iter()
        .flatten()
    {
        if !ensure_regular_dir_no_symlink(dir)? {
            return Err("cold journal requires a real directory".into());
        }
    }
    let Some(bytes) = read_regular_no_symlink_bytes(&cold_manifest_path(journal_dir))? else {
        for entry in fs::read_dir(cdir).map_err(|_| "cold journal directory unavailable")? {
            let entry = entry.map_err(|_| "cold journal directory unavailable")?;
            if entry.path().extension().is_some_and(|v| v == "jsonl") {
                return Err("cold journal file unavailable".into());
            }
        }
        return Ok(ColdManifest::default());
    };
    let original: Value =
        crate::json::from_slice(&bytes).map_err(|_| "invalid cold journal manifest")?;
    let manifest = parse_manifest(&original)?;
    if manifest.schema_version != COLD_SCHEMA_VERSION {
        return Err("invalid cold journal manifest".into());
    }
    let mut names = BTreeSet::new();
    for entry in &manifest.segments {
        if !valid_month(&entry.month)
            || !valid_digest(&entry.sha256)
            || entry.count == 0
            || entry.file != format!("{}-{}.jsonl", entry.month, entry.sha256)
            || !names.insert(&entry.file)
        {
            return Err("invalid cold journal manifest".into());
        }
    }
    if let Some(pending) = &manifest.pending
        && (!valid_digest(&pending.before)
            || !valid_digest(&pending.after)
            || !valid_month(&pending.cutoff))
    {
        return Err("invalid cold journal manifest".into());
    }
    Ok(manifest)
}

pub(crate) fn manifest_value(manifest: &ColdManifest) -> Result<Value, String> {
    let segments: Vec<_> = manifest.segments.iter().map(|row| crate::json::json!({
        "file": row.file, "month": row.month, "count": row.count, "first_event_id": row.first_event_id,
        "last_event_id": row.last_event_id, "sha256": row.sha256
    })).collect();
    let mut value =
        crate::json::json!({"schema_version": manifest.schema_version, "segments": segments});
    if let Some(pending) = &manifest.pending {
        *value.at_mut("pending") = crate::json::json!({"before": pending.before, "after": pending.after, "cutoff": pending.cutoff});
    } else if manifest.pending_null {
        *value.at_mut("pending") = Value::Null;
    }
    Ok(value)
}
fn manifest_bytes(manifest: &ColdManifest) -> Result<Vec<u8>, String> {
    let mut bytes = canonical_json_bytes(&manifest_value(manifest)?)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn save_cold_manifest(journal_dir: &Path, manifest: &ColdManifest) -> Result<(), String> {
    atomic_write_bytes(&cold_manifest_path(journal_dir), &manifest_bytes(manifest)?)
}

fn cold_event(raw: &[u8]) -> Result<Value, String> {
    let row: Value = crate::json::from_slice(raw).map_err(|_| "invalid cold journal segment")?;
    validate_canonical_event(&row)?;
    let date = row
        .get("local_date")
        .and_then(Value::as_str)
        .ok_or("invalid canonical date")?;
    let parsed = crate::tz::parse_date_ymd(&date).ok_or("invalid canonical date")?;
    if naive_date_to_string(parsed) != date {
        return Err("invalid canonical date".into());
    }
    Ok(row)
}

fn segment_events(journal_dir: &Path, entry: &ColdSegmentEntry) -> Result<Vec<Value>, String> {
    let content = read_regular_no_symlink_bytes(&cold_dir(journal_dir).join(&entry.file))?
        .ok_or("cold journal file unavailable")?;
    if sha256_hex(&content) != entry.sha256 || !content.ends_with(b"\n") {
        return Err("cold journal segment digest mismatch".into());
    }
    let rows: Vec<Value> = byte_lines(&content)
        .map(cold_event)
        .collect::<Result<_, _>>()
        .map_err(|_| "invalid cold journal segment")?;
    if rows.len() != entry.count
        || !rows
            .first()
            .and_then(|r| r.get("event_id"))
            .is_some_and(|id| oms_json::ordered_values_equal(id, &entry.first_event_id))
        || !rows
            .last()
            .and_then(|r| r.get("event_id"))
            .is_some_and(|id| oms_json::ordered_values_equal(id, &entry.last_event_id))
        || rows.iter().any(|r| {
            r.at("local_date")
                .as_str()
                .map(|d| d.chars().take(7).collect::<String>())
                .as_deref()
                != Some(entry.month.as_str())
        })
    {
        return Err("invalid cold journal segment".into());
    }
    Ok(rows)
}

// bytes.splitlines(), excluding record separators, retains empty records.
pub(crate) fn byte_lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    content
        .split_inclusive(|b| matches!(b, b'\n' | b'\r'))
        .scan(false, |after_cr, raw| {
            let skip = *after_cr && raw == b"\n";
            *after_cr = raw.ends_with(b"\r");
            Some((!skip).then(|| {
                raw.strip_suffix(b"\n")
                    .or_else(|| raw.strip_suffix(b"\r"))
                    .unwrap_or(raw)
            }))
        })
        .flatten()
}

fn partition_live(content: &[u8], cutoff: &str) -> (BTreeMap<String, Vec<u8>>, Vec<u8>) {
    let mut months: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut recent = Vec::new();
    // CR and CRLF are record separators in Python bytes.splitlines(keepends=True).
    let mut start = 0;
    while start < content.len() {
        let end = content[start..]
            .iter()
            .position(|b| matches!(b, b'\n' | b'\r'))
            .map(|n| {
                let end = start + n + 1;
                if content[end - 1] == b'\r' && content.get(end) == Some(&b'\n') {
                    end + 1
                } else {
                    end
                }
            })
            .unwrap_or(content.len());
        let raw = &content[start..end];
        let month = raw
            .ends_with(b"\n")
            .then(|| cold_event(raw).ok())
            .flatten()
            .and_then(|row| {
                row.at("local_date")
                    .as_str()
                    .and_then(|d| d.get(..7).map(str::to_owned))
            });
        if let Some(month) = month.filter(|m| m.as_str() < cutoff) {
            months.entry(month).or_default().extend_from_slice(raw);
        } else {
            recent.extend_from_slice(raw);
        }
        start = end;
    }
    (months, recent)
}

pub fn journal_live(journal_dir: &Path, manifest: &ColdManifest) -> Result<Vec<u8>, String> {
    let mut content =
        read_regular_no_symlink_bytes(&journal_dir.join("events.jsonl"))?.unwrap_or_default();
    if let Some(pending) = &manifest.pending {
        let digest = sha256_hex(&content);
        if digest == pending.before {
            let (months, retained) = partition_live(&content, &pending.cutoff);
            for (month, raw) in months {
                let name = format!("{month}-{}.jsonl", sha256_hex(&raw));
                if !manifest.segments.iter().any(|s| s.file == name) {
                    return Err("incomplete cold journal transition".into());
                }
            }
            content = retained;
        } else if digest != pending.after {
            return Err("cold journal transition live digest mismatch".into());
        }
        if sha256_hex(&content) != pending.after {
            return Err("cold journal transition replay mismatch".into());
        }
    }
    Ok(content)
}

pub fn journal_snapshot<T>(
    journal_dir: &Path,
    mut read: impl FnMut(&ColdManifest) -> Result<T, String>,
) -> Result<(ColdManifest, T), String> {
    for _ in 0..8 {
        let manifest = load_cold_manifest(journal_dir)?;
        let result = read(&manifest);
        if manifest == load_cold_manifest(journal_dir)? {
            return result.map(|value| (manifest, value));
        }
    }
    Err("journal layout kept changing during read".into())
}

pub fn read_verified_cold_events(journal_dir: &Path) -> Result<(ColdManifest, Vec<Value>), String> {
    journal_snapshot(journal_dir, |manifest| {
        let mut events = Vec::new();
        for entry in &manifest.segments {
            events.extend(segment_events(journal_dir, entry)?);
        }
        if manifest.pending.is_some() {
            journal_live(journal_dir, manifest)?;
        }
        Ok(events)
    })
}

pub fn replay_snapshot(journal_dir: &Path) -> Result<(ColdManifest, Vec<Value>, Vec<u8>), String> {
    let (manifest, (events, live)) = journal_snapshot(journal_dir, |manifest| {
        let mut events = Vec::new();
        for entry in &manifest.segments {
            events.extend(segment_events(journal_dir, entry)?);
        }
        Ok((events, journal_live(journal_dir, manifest)?))
    })?;
    Ok((manifest, events, live))
}

pub fn journal_storage(repo_root: &Path) -> StorageHealth {
    storage_health(repo_root, false, None)
}

/// State also reports older cold rows. The storage measurement may fail before
/// replay, so preserve its independent fallback without repeating a successful replay.
pub fn journal_storage_with_cold_count(
    repo_root: &Path,
    before_month: &str,
) -> (StorageHealth, usize) {
    let (health, manifest) = measure_storage_health(repo_root, false, None);
    let manifest = manifest.or_else(|| {
        replay_snapshot(&repo_root.join(".oms/work-journal"))
            .ok()
            .map(|(manifest, _, _)| manifest)
    });
    let count = manifest.map_or(0, |manifest| {
        // Replay verifies that each segment contains exactly this count and month.
        manifest
            .segments
            .into_iter()
            .filter(|entry| entry.month.as_str() < before_month)
            .map(|entry| entry.count)
            .sum()
    });
    (health, count)
}

pub(crate) fn storage_health(
    repo_root: &Path,
    allow_incomplete: bool,
    verified: Option<&ColdManifest>,
) -> StorageHealth {
    measure_storage_health(repo_root, allow_incomplete, verified).0
}

fn measure_storage_health(
    repo_root: &Path,
    allow_incomplete: bool,
    verified: Option<&ColdManifest>,
) -> (StorageHealth, Option<ColdManifest>) {
    let root = repo_root.join(".oms/work-journal");
    let mut health=StorageHealth{healthy:true,threshold_note:"Review prior-month sealing with `oms journal seal` (dry-run); run `oms journal seal --apply` to seal.".into(),..Default::default()};
    let result = (|| -> Result<Option<ColdManifest>, String> {
        health.present = match fs::symlink_metadata(&root) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.to_string()),
        };
        if !health.present {
            return Ok(None);
        }
        if !ensure_regular_dir_no_symlink(&root)?
            || !ensure_regular_dir_no_symlink(root.parent().ok_or("invalid journal root")?)?
        {
            return Err("journal storage is not a real directory".into());
        }
        let mut directories = vec![root.clone()];
        let mut scanned = 0;
        while let Some(dir) = directories.pop() {
            for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                scanned += 1;
                if scanned > 10000 {
                    return Err("journal storage measurement entry limit".into());
                }
                let path = entry.path();
                let info = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                if info.file_type().is_symlink() {
                    return Err("journal storage contains a symlink".into());
                }
                if info.is_dir() {
                    directories.push(path);
                    continue;
                }
                if !info.is_file() {
                    if path == root.join("events.jsonl") {
                        return Err("canonical journal is not a regular file".into());
                    }
                    continue;
                }
                health.file_count += 1;
                health.total_bytes += info.len();
                if path != root.join("events.jsonl") {
                    continue;
                }
                health.events_bytes = info.len();
                let parent = crate::files::Parent::open(&path)?;
                let before = parent.stat()?.ok_or("canonical journal disappeared")?;
                use std::io::Read;
                let descriptor = rustix::fs::open(
                    &path,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::NONBLOCK
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|e| e.to_string())?;
                let opened = rustix::fs::fstat(&descriptor).map_err(|e| e.to_string())?;
                if before.st_dev != opened.st_dev || before.st_ino != opened.st_ino {
                    return Err("canonical journal changed during open".into());
                }
                let mut file = std::fs::File::from(descriptor);
                let mut remaining = info.len();
                let mut last = None;
                let mut chunk = [0; 65536];
                while remaining > 0 {
                    let limit = remaining.min(chunk.len() as u64) as usize;
                    let n = file.read(&mut chunk[..limit]).map_err(|e| e.to_string())?;
                    if n == 0 {
                        return Err("canonical journal shortened during measurement".into());
                    }
                    remaining -= n as u64;
                    health.event_lines += chunk[..n].iter().filter(|b| **b == b'\n').count();
                    last = Some(chunk[n - 1]);
                }
                let after = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
                let named = parent.stat()?.ok_or("canonical journal disappeared")?;
                if after.st_size != before.st_size
                    || after.st_mtime != before.st_mtime
                    || after.st_mtime_nsec != before.st_mtime_nsec
                    || after.st_ctime != before.st_ctime
                    || after.st_ctime_nsec != before.st_ctime_nsec
                    || after.st_dev != named.st_dev
                    || after.st_ino != named.st_ino
                    || (!allow_incomplete && last.is_some_and(|b| b != b'\n'))
                {
                    return Err("canonical journal changed or has an incomplete event".into());
                }
            }
        }
        let manifest = if let Some(verified) = verified {
            verified.clone()
        } else {
            let (manifest, _, _) = replay_snapshot(&root)?;
            manifest
        };
        health.cold_segments = manifest.segments.len();
        health.cold_events = manifest.segments.iter().map(|s| s.count).sum();
        health.has_cold_segments = !manifest.segments.is_empty();
        Ok(Some(manifest))
    })();
    let observed = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            health.healthy = false;
            health.error = Some(error);
            None
        }
    };
    for (name, exceeded) in [
        ("event_lines", health.event_lines >= LIVE_EVENT_LINE_LIMIT),
        ("events_bytes", health.events_bytes >= LIVE_FILE_BYTE_LIMIT),
        ("file_count", health.file_count >= JOURNAL_FILE_COUNT_LIMIT),
    ] {
        if exceeded {
            health.threshold_exceeded.push(name.into());
        }
    }
    (health, observed)
}

pub fn seal_report_text(dry_run: bool, count: usize) -> String {
    if dry_run {
        format!("dry-run: {count} events would be sealed; run oms journal seal --apply\n")
    } else {
        format!("sealed: {count} events\n")
    }
}

fn finish_seal(journal_dir: &Path, manifest: &mut ColdManifest) -> Result<(), String> {
    if manifest.pending.is_some() {
        let content = journal_live(journal_dir, manifest)?;
        atomic_write_bytes(&journal_dir.join("events.jsonl"), &content)?;
        manifest.pending = None;
        manifest.pending_null = false;
        save_cold_manifest(journal_dir, manifest)?;
    }
    Ok(())
}

pub fn seal_events(
    repo_root: &Path,
    tz: &TimeZoneSpec,
    now: chrono::DateTime<chrono::Utc>,
    apply: bool,
) -> Result<usize, String> {
    let journal_dir = repo_root.join(".oms/work-journal");
    let (manifest, _) = read_verified_cold_events(&journal_dir)?;
    seal_verified(repo_root, tz, now, apply, manifest)
}
pub(crate) fn seal_verified(
    repo_root: &Path,
    tz: &TimeZoneSpec,
    now: chrono::DateTime<chrono::Utc>,
    apply: bool,
    mut manifest: ColdManifest,
) -> Result<usize, String> {
    let journal_dir = repo_root.join(".oms/work-journal");
    if apply {
        finish_seal(&journal_dir, &mut manifest)?;
    }
    let content = journal_live(&journal_dir, &manifest)?;
    let (_, today, _) = tz.local_date_and_week(now);
    let cutoff = &today[..7];
    let (months, recent) = partition_live(&content, cutoff);
    let count = months.values().map(|raw| byte_lines(raw).count()).sum();
    if !apply || months.is_empty() {
        return Ok(count);
    }
    ensure_oms_gitignore(&journal_dir)?;
    if read_regular_no_symlink_bytes(&cold_manifest_path(&journal_dir))?.is_none() {
        save_cold_manifest(&journal_dir, &manifest)?;
    }
    for (month, raw) in months {
        let digest = sha256_hex(&raw);
        let file = format!("{month}-{digest}.jsonl");
        let target = cold_dir(&journal_dir).join(&file);
        match read_regular_no_symlink_bytes(&target)? {
            Some(existing) if existing != raw => {
                return Err("cold journal segment collision".into());
            }
            Some(_) => {}
            None => atomic_write_bytes(&target, &raw)?,
        }
        let rows: Vec<Value> = byte_lines(&raw).map(cold_event).collect::<Result<_, _>>()?;
        manifest.segments.push(ColdSegmentEntry {
            file,
            month,
            sha256: digest,
            count: rows.len(),
            first_event_id: rows[0].at("event_id").copy(),
            last_event_id: rows[rows.len() - 1].at("event_id").copy(),
        });
    }
    manifest.pending = Some(PendingSeal {
        before: sha256_hex(&content),
        after: sha256_hex(&recent),
        cutoff: cutoff.into(),
    });
    save_cold_manifest(&journal_dir, &manifest)?;
    finish_seal(&journal_dir, &mut manifest)?;
    Ok(count)
}
