use super::*;
use crate::value::V;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn retention_limits(
    keep: Option<i64>,
    high: Option<i64>,
) -> Result<(usize, usize), StoreError> {
    let keep = keep
        .or_else(|| {
            std::env::var("OMS_ARTIFACT_INDEX_KEEP")
                .ok()?
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(1000);
    let default_high = keep
        .checked_add(keep / 5)
        .ok_or(StoreError::Invalid("retention overflow"))?;
    let high = high
        .or_else(|| {
            std::env::var("OMS_ARTIFACT_INDEX_HIGH_WATER")
                .ok()?
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(default_high);
    if keep <= 0 || high < keep {
        return Err(StoreError::Invalid(
            "retention requires positive keep and high-water >= keep",
        ));
    }
    Ok((
        usize::try_from(keep).map_err(|_| StoreError::Invalid("retention overflow"))?,
        usize::try_from(high).map_err(|_| StoreError::Invalid("retention overflow"))?,
    ))
}
#[derive(Debug, Clone)]
pub struct RetentionOptions {
    pub keep: usize,
    pub max_bytes: usize,
    pub required_event_ids: Vec<String>,
    /// None means the receipt namespace was uncertain: protect all source rows.
    pub pinned_event_ids: Option<BTreeSet<String>>,
    pub include_decision_pins: bool,
    pub now: SystemTime,
}
impl Default for RetentionOptions {
    fn default() -> Self {
        Self {
            keep: 1000,
            max_bytes: MAX_INDEX_BYTES,
            required_event_ids: Vec::new(),
            pinned_event_ids: Some(BTreeSet::new()),
            include_decision_pins: true,
            now: SystemTime::now(),
        }
    }
}

pub fn decision_pin_indices(source: &[&[u8]], now: SystemTime, max_rows: usize) -> BTreeSet<usize> {
    let now = match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    source
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(index, line)| {
            let row = row(line).ok()?;
            if row["kind"].text() != Some("panel-result") {
                return None;
            }
            let ts = chrono::DateTime::parse_from_rfc3339(row["ts"].text()?).ok()?;
            let ts = ts.timestamp() as i128 * 1_000_000_000 + ts.timestamp_subsec_nanos() as i128;
            (ts >= now - 30 * 86400 * 1_000_000_000 && ts <= now + 300 * 1_000_000_000)
                .then_some(index)
        })
        .take(max_rows.min(3000))
        .collect()
}
fn complete(row: &V) -> bool {
    row["schema"] == V::from(1)
        && ["event_id", "operation_id", "artifact_id", "ts", "kind"]
            .iter()
            .all(|key| row[key].bytes().is_some_and(|s| !s.is_empty()))
        && row["provider"].bytes().is_some()
        && row["exit"].int().is_some()
}

/// Preserve complete backward resolution closures and the ordinary-history floor.
/// Required lineage must fit keep; hard pins and unresolved failures may exceed it.
pub fn retained_lines(
    source: &[&[u8]],
    options: &RetentionOptions,
) -> Result<Vec<Vec<u8>>, StoreError> {
    let RetentionOptions {
        keep, max_bytes, ..
    } = options;
    if *keep == 0 || *max_bytes == 0 || *max_bytes > MAX_INDEX_BYTES {
        return Err(StoreError::Invalid("invalid retention bounds"));
    }
    let parsed: Vec<_> = source
        .iter()
        .map(|line| row(line))
        .collect::<Result<_, _>>()?;
    let mut ids: BTreeMap<&[u8], Vec<usize>> = BTreeMap::new();
    for (i, row) in parsed.iter().enumerate() {
        if let Some(id) = row["event_id"].bytes() {
            ids.entry(id).or_default().push(i);
        }
    }
    let unique: BTreeMap<_, _> = ids
        .iter()
        .filter(|(_, v)| v.len() == 1)
        .map(|(k, v)| (*k, v[0]))
        .collect();
    let group_for = |index: usize| -> Vec<usize> {
        let mut group = Vec::new();
        let mut current = index;
        loop {
            group.push(current);
            if parsed[current]["kind"].text() != Some("artifact-resolution") {
                return group;
            }
            let target = parsed[current]["resolves_event_id"]
                .bytes()
                .and_then(|id| unique.get(id))
                .copied();
            match target {
                Some(target) if target < current => current = target,
                _ => return Vec::new(),
            }
        }
    };
    let size =
        |indices: &BTreeSet<usize>| -> usize { indices.iter().map(|i| source[*i].len()).sum() };
    let mut targets = BTreeSet::new();
    let mut settled = BTreeSet::new();
    for (i, resolution) in parsed.iter().enumerate() {
        if resolution["kind"].text() != Some("artifact-resolution") {
            continue;
        }
        let Some(&target) = resolution["resolves_event_id"]
            .bytes()
            .and_then(|id| unique.get(id))
        else {
            continue;
        };
        if target >= i {
            continue;
        }
        targets.insert(target);
        let failure = &parsed[target];
        if complete(resolution)
            && complete(failure)
            && resolution["event_id"].bytes().and_then(|id| unique.get(id)) == Some(&i)
            && resolution["parent_event_id"] == resolution["resolves_event_id"]
            && resolution["resolution"].text() == Some("resolved")
            && resolution["exit"] == V::from(0)
            && failure["kind"].text() != Some("artifact-resolution")
            && failure["exit"].int().is_some_and(|s| s != "0")
            && resolution["operation_id"] == failure["operation_id"]
            && resolution["artifact_id"] == failure["artifact_id"]
        {
            settled.insert(target);
        }
    }
    let mut selected = BTreeSet::new();
    for id in &options.required_event_ids {
        let &index = unique.get(id.as_bytes()).ok_or(StoreError::Invalid(
            "required artifact event missing or ambiguous",
        ))?;
        let group = group_for(index);
        if group.is_empty() {
            return Err(StoreError::Invalid(
                "required resolution has no earlier unique target",
            ));
        }
        selected.extend(group);
    }
    if selected.len() > *keep || size(&selected) > *max_bytes {
        return Err(StoreError::Invalid(
            "required artifact lineage exceeds retention",
        ));
    }
    let mut ordinary = selected.clone();
    if let Some(pins) = &options.pinned_event_ids {
        for id in pins {
            for index in ids.get(id.as_bytes()).into_iter().flatten() {
                let group = group_for(*index);
                if group.is_empty() {
                    selected.extend(0..source.len());
                } else {
                    selected.extend(group);
                }
            }
        }
    } else {
        selected.extend(0..source.len());
    }
    for (i, row) in parsed.iter().enumerate() {
        if row["kind"].text() != Some("artifact-resolution")
            && row["exit"].int().is_some_and(|s| s != "0")
            && !settled.contains(&i)
        {
            selected.insert(i);
        }
    }
    let mut used = size(&selected);
    if options.include_decision_pins {
        for i in decision_pin_indices(source, options.now, 3000).iter().rev() {
            if !selected.contains(i) && source[*i].len() <= max_bytes.saturating_sub(used) {
                selected.insert(*i);
                used += source[*i].len();
            }
        }
    }
    for index in (0..source.len()).rev() {
        if selected.contains(&index) || targets.contains(&index) {
            continue;
        }
        let group: Vec<_> = group_for(index)
            .into_iter()
            .filter(|i| !selected.contains(i))
            .collect();
        let bytes: usize = group.iter().map(|i| source[*i].len()).sum();
        if group.is_empty()
            || ordinary.len() + group.len() > *keep
            || used.checked_add(bytes).is_none_or(|n| n > *max_bytes)
        {
            continue;
        }
        used += bytes;
        selected.extend(&group);
        ordinary.extend(group);
        if ordinary.len() == *keep || used == *max_bytes {
            break;
        }
    }
    if used > *max_bytes {
        return Err(StoreError::Invalid(
            "protected artifact rows exceed byte retention",
        ));
    }
    Ok(source
        .iter()
        .enumerate()
        .filter(|(i, _)| selected.contains(i))
        .map(|(_, s)| s.to_vec())
        .collect())
}

pub type CompletionReferences = (BTreeSet<String>, BTreeSet<String>);
/// Any malformed, changing, unbounded or unreadable receipt namespace protects all.
pub fn completion_references(repo: &Path) -> Option<CompletionReferences> {
    fn scan(repo: &Path) -> Result<CompletionReferences, StoreError> {
        let root = Root::open(repo)?;
        let directory = root.physical_root().join(".oms/plan/completions");
        let dir = match paths::Directory::open(&root, &directory, false) {
            Err(error) if paths::missing(&error) => return Ok((BTreeSet::new(), BTreeSet::new())),
            other => other?,
        };
        let mut names = dir.names(4096)?;
        names.sort();
        let mut pins = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut total = 0usize;
        let mut snapshots = Vec::new();
        let before = rustix::fs::fstat(&dir.fd)?;
        for name in &names {
            let name = name
                .to_str()
                .ok_or(StoreError::Invalid("invalid completion name"))?;
            if name.len() != 69
                || !name.ends_with(".json")
                || !name.as_bytes()[..64]
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            {
                return Err(StoreError::Invalid("invalid completion name"));
            }
            let path = directory.join(name);
            paths::validate(&root, &path, false)?;
            let identity = dir.stat(std::ffi::OsStr::new(name))?;
            let raw = root.read_no_follow(
                &path,
                ByteReadOptions {
                    missing_ok: false,
                    max_bytes: 4 * 1024 * 1024,
                },
            )?;
            total += raw.len();
            if total > 64 * 1024 * 1024 || format!("{:x}", Sha256::digest(&raw)) != name[..64] {
                return Err(StoreError::Invalid(
                    "invalid completion digest or byte budget",
                ));
            }
            snapshots.push((name, identity));
            // The strict parser rejects duplicate keys even inside nested objects.
            oms_context::parse_json(&raw)
                .map_err(|_| StoreError::Invalid("invalid completion JSON"))?;
            let receipt =
                parse(&raw, true).map_err(|_| StoreError::Invalid("invalid completion JSON"))?;
            if !matches!(receipt, V::Object(_)) || !matches!(receipt["bundle"], V::Object(_)) {
                return Err(StoreError::Invalid("invalid completion bundle"));
            }
            for kind in ["research", "main_research"] {
                let info = &receipt["bundle"][kind];
                if matches!(info, V::Null) {
                    continue;
                }
                if info["classification"].text() != Some("research-only") {
                    return Err(StoreError::Invalid("invalid completion classification"));
                }
                let event = info["index_event_id"]
                    .text()
                    .filter(|s| !s.is_empty())
                    .ok_or(StoreError::Invalid("invalid completion event"))?;
                let artifact = info["artifact"]["path"]
                    .text()
                    .filter(|s| !s.is_empty())
                    .ok_or(StoreError::Invalid("invalid completion artifact"))?;
                pins.insert(event.to_string());
                paths.insert(artifact.to_string());
                let mut snapshot = Some(&receipt);
                for key in ["snapshot", "evidence", "index"] {
                    snapshot = match snapshot.and_then(|value| value.get(key)) {
                        Some(value @ V::Object(_)) => Some(value),
                        None => None,
                        Some(_) => return Err(StoreError::Invalid("invalid completion snapshot")),
                    };
                }
                for key in ["artifact", "patch", "source"] {
                    if let Some(path) = snapshot
                        .and_then(|row| row[key].text())
                        .filter(|s| !s.is_empty())
                    {
                        paths.insert(path.to_string());
                    }
                }
            }
        }
        dir.verify()?;
        for (name, before) in snapshots {
            if !paths::unchanged(&before, &dir.stat(std::ffi::OsStr::new(name))?) {
                return Err(StoreError::Changed);
            }
        }
        let mut after_names = dir.names(4096)?;
        after_names.sort();
        if names != after_names || !paths::unchanged(&before, &rustix::fs::fstat(&dir.fd)?) {
            return Err(StoreError::Changed);
        }
        Ok((pins, paths))
    }
    scan(repo).ok()
}
pub fn completion_pins(repo: &Path) -> Option<BTreeSet<String>> {
    completion_references(repo).map(|refs| refs.0)
}
