use crate::time::timestamp;
use crate::value::{V, equal, obj, parse, rounded};
use regex::Regex;
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const EDGE: usize = 64 * 1024;
const USAGE: [&str; 3] = ["input_tokens", "cache_read_tokens", "output_tokens"];
static TOTAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^tokens used[ \t]*\r?\n[ \t]*([0-9][0-9,]*)[ \t]*$").unwrap()
});
static SERVED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)^served model[ \t]*\r?\n[ \t]*([A-Za-z0-9İı][A-Za-z0-9İı._:+/-]{0,199})[ \t]*$",
    )
    .unwrap()
});
static CONFIGURED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)^configured model[ \t]*\r?\n[ \t]*([A-Za-z0-9İı][A-Za-z0-9İı._:+/-]{0,199})[ \t]*$",
    )
    .unwrap()
});
static COST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^cost usd[ \t]*\r?\n[ \t]*([0-9]+(?:\.[0-9]+)?)[ \t]*$").unwrap()
});
static START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^- started:\s*(\S+)\s*$").unwrap());
static REPAIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^## Repair [0-9]+$").unwrap());
static VERIFY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^## Verify(?: \(repair [0-9]+\))?$").unwrap());
static DETAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^usage detail: (\{[^\r\n]+\})$").unwrap());

// os.fsencode accepts surrogateescape bytes, but refuses other lone surrogates.
pub(crate) fn os_path(raw: &[u8]) -> Option<PathBuf> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == 0xed && i + 2 < raw.len() && raw[i + 1] >= 0xa0 {
            let point = ((raw[i] & 15) as u32) << 12
                | ((raw[i + 1] & 63) as u32) << 6
                | (raw[i + 2] & 63) as u32;
            if !(0xdc80..=0xdcff).contains(&point) {
                return None;
            }
            result.push((point - 0xdc00) as u8);
            i += 3;
        } else {
            result.push(raw[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&result)))
}
fn artifact_text(repo: &Path, row: &V) -> Option<String> {
    let name = row["artifact"].bytes().filter(|s| !s.is_empty())?;
    let name = os_path(name)?;
    if name.is_absolute() {
        return None;
    }
    let candidate = std::fs::canonicalize(repo.join(name)).ok()?;
    if !candidate.starts_with(repo) {
        return None;
    }
    // Anchor every component after resolving benign aliases; a raced symlink
    // must never turn a repo-local metric into an external-file read.
    use rustix::fs::{Mode, OFlags, open, openat};
    let directories = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = open(repo, directories, Mode::empty()).ok()?;
    let relative = candidate.strip_prefix(repo).ok()?;
    let mut parts = relative.components().peekable();
    let file = loop {
        let part = parts.next()?;
        if parts.peek().is_none() {
            break openat(
                &parent,
                part.as_os_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .ok()?;
        }
        parent = openat(&parent, part.as_os_str(), directories, Mode::empty()).ok()?;
    };
    let mut file = std::fs::File::from(file);
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut raw = Vec::new();
    if meta.len() <= (EDGE * 2) as u64 {
        (&mut file)
            .take((EDGE * 2 + 1) as u64)
            .read_to_end(&mut raw)
            .ok()?;
    } else {
        (&mut file).take(EDGE as u64).read_to_end(&mut raw).ok()?;
        file.seek(SeekFrom::End(-(EDGE as i64))).ok()?;
        raw.push(b'\n');
        (&mut file).take(EDGE as u64).read_to_end(&mut raw).ok()?;
    }
    Some(String::from_utf8_lossy(&raw).into_owned())
}
fn output(text: &str, row: &V) -> String {
    let mut lines = Vec::new();
    let mut active = false;
    let mut repair = false;
    let synthesis = format!(
        "## Synthesis ({})",
        row["provider"].text().unwrap_or("None")
    );
    let normalized = text.replace("\r\n", "\n");
    for line in normalized.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ]) {
        if line == "## Output"
            || (row["kind"].text() == Some("review-synthesis")
                && row["provider"].text() != Some("local")
                && line == synthesis)
        {
            lines.clear();
            active = true;
            repair = false;
        } else if REPAIR.is_match(line) {
            active = false;
            repair = true;
        } else if repair && line == "### Output" {
            active = true;
        } else if ["## Prompt", "### Prompt", "## Exit"].contains(&line) || VERIFY.is_match(line) {
            active = false;
        } else if active {
            lines.push(line);
        }
    }
    lines.join("\n")
}
#[derive(Default)]
pub(crate) struct Metrics {
    pub present: bool,
    pub duration: Option<f64>,
    pub tokens: Option<V>,
    pub detail: Option<V>,
    pub model: Option<V>,
    pub cost: Option<f64>,
    pub ambiguous: bool,
    pub served_model: Option<V>,
    pub configured_model: Option<V>,
}
pub(crate) fn complete(detail: &V) -> bool {
    USAGE.iter().all(|k| detail[k].int().is_some())
}
pub(crate) fn cumulative(detail: &V) -> bool {
    detail["session_cumulative"] == V::Bool(true)
}

pub(crate) fn metrics(repo: &Path, row: &V) -> Result<Metrics, String> {
    for key in ["duration_s", "cost_usd"] {
        if matches!(&row[key], V::Int(_))
            && row[key]
                .number()
                .is_some_and(|n| !n.is_finite() && (key == "cost_usd" || n >= 0.0))
        {
            return Err("artifact telemetry: recorded number is out of range".into());
        }
    }
    let text = artifact_text(repo, row);
    let body = text.as_deref().map(|text| output(text, row));
    let mut result = Metrics {
        present: text.is_some(),
        ..Metrics::default()
    };
    result.duration = row["duration_s"]
        .number()
        .filter(|n| *n >= 0.0)
        .map(|n| rounded(n, 3));
    if row["tokens"].int().is_some() {
        result.tokens = Some(row["tokens"].clone());
    }
    if let Some(text) = &text
        && result.duration.is_none()
        && let Some(c) = START.captures(text)
        && let (Some(start), Some(end)) = (timestamp(&c[1]), row["ts"].text().and_then(timestamp))
        && end >= start
    {
        result.duration = Some(rounded((end - start) as f64 / 1_000_000.0, 3));
    }
    if result.tokens.is_none()
        && let Some(body) = &body
    {
        let mut total = obj!("value":0);
        let mut found = false;
        for c in TOTAL_RE.captures_iter(body) {
            let digits = c[1].replace(',', "");
            if digits.len() > 4300 {
                return Err("artifact telemetry: recorded integer exceeds the digit limit".into());
            }
            let digits = digits.trim_start_matches('0');
            total.add(
                "value",
                &V::Int(if digits.is_empty() {
                    "0".into()
                } else {
                    digits.into()
                }),
            );
            found = true;
        }
        if found {
            result.tokens = Some(total["value"].clone());
        }
    }
    result.detail = if matches!(row["provider_usage"], V::Object(_)) {
        Some(row["provider_usage"].clone())
    } else {
        body.as_deref().and_then(|body| usage(body, row))
    };
    let mut models = BTreeSet::new();
    let mut configured = BTreeSet::new();
    if let Some(body) = &body {
        for c in SERVED.captures_iter(body) {
            for part in c[1].split('+').filter(|s| !s.is_empty()) {
                models.insert(part.as_bytes().to_vec());
            }
        }
        for c in CONFIGURED.captures_iter(body) {
            configured.insert(c[1].as_bytes().to_vec());
        }
    }
    if let Some(model) = row["served_model"].bytes().filter(|s| !s.is_empty()) {
        for part in model.split(|b| *b == b'+').filter(|s| !s.is_empty()) {
            models.insert(part.to_vec());
        }
    }
    if let Some(model) = row["configured_model"].bytes().filter(|s| !s.is_empty()) {
        configured.insert(model.to_vec());
    }
    result.cost = row["cost_usd"].number().or_else(|| {
        body.as_deref().and_then(|body| {
            let values: Vec<f64> = COST
                .captures_iter(body)
                .filter_map(|c| c[1].parse().ok())
                .collect();
            (!values.is_empty()).then(|| rounded(values.iter().sum(), 6))
        })
    });
    result.ambiguous = row["fallback_used"].truth()
        || models.len() > 1
        || configured.len() > 1
        || (!models.is_empty() && !configured.is_empty() && models != configured)
        || row["model_attribution"].text() == Some("ambiguous");
    if !result.ambiguous {
        result.served_model = models.iter().next().cloned().map(V::Str);
        if result.served_model.is_none() {
            result.configured_model = configured.iter().next().cloned().map(V::Str);
        }
    }
    if !result.ambiguous {
        result.model = models
            .into_iter()
            .next()
            .or_else(|| configured.into_iter().next())
            .map(V::Str);
    }
    Ok(result)
}
fn usage(body: &str, row: &V) -> Option<V> {
    let reports: Vec<V> = DETAIL
        .captures_iter(body)
        .filter_map(|c| parse(c[1].as_bytes(), false).ok())
        .filter(|v| matches!(v, V::Object(_)) && equal(&v["provider"], &row["provider"]))
        .collect();
    if reports.is_empty() {
        return None;
    }
    let mut result = obj!("reports":reports.len(),"measurement":"provider-reported");
    let last = reports.iter().rfind(|v| cumulative(v));
    let measured: Vec<&V> = if let Some(last) = last {
        result["session_cumulative"] = true.into();
        vec![last]
    } else {
        reports.iter().collect()
    };
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "tool_calls",
    ] {
        result[key] = 0.into();
        for report in &measured {
            if report[key].int().is_none() {
                result[key] = V::Null;
                break;
            }
            result.add(key, &report[key]);
        }
    }
    let flag = &measured[0]["cache_in_input"];
    result["cache_in_input"] =
        if matches!(flag, V::Bool(_)) && measured.iter().all(|r| &r["cache_in_input"] == flag) {
            flag.clone()
        } else {
            V::Null
        };
    Some(result)
}

pub(crate) fn native(root: &oms_safe_fs::Root) -> Result<V, String> {
    let bytes = match root.read(
        ".oms/hooks/events.jsonl",
        256 * 1024 * 1024,
        oms_safe_fs::Policy::source(),
    ) {
        Ok(bytes) => bytes,
        Err(oms_safe_fs::Error::Missing) => Vec::new(),
        Err(_) => return Err("artifact telemetry: cannot read native activity".into()),
    };
    let mut rows = Vec::new();
    let mut invalid = 0;
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    for raw in text.as_bytes().split_inclusive(|b| *b == b'\n') {
        // The hook reader decodes with replacement, unlike the strict index reader.
        match parse(String::from_utf8_lossy(raw).as_bytes(), false) {
            Ok(row) if matches!(row, V::Object(_)) && row["action"].text() == Some("telemetry") => {
                rows.push(row)
            }
            Ok(_) => {}
            Err(_) => invalid += 1,
        }
    }
    let keys = [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_creation_tokens",
        "reasoning_tokens",
    ];
    let mut sessions = std::collections::BTreeMap::<Vec<u8>, V>::new();
    let mut turns = BTreeSet::new();
    let mut tool = 0;
    let mut subagent = 0;
    for row in &rows {
        if row["tool_name"].truth() {
            tool += 1;
        }
        if row["hook"]
            .text()
            .unwrap_or("")
            .to_lowercase()
            .contains("subagent")
            || row["subagent_type"].truth()
        {
            subagent += 1;
        }
        let Some(session) = row["session"].bytes() else {
            continue;
        };
        if let Some(turn) = row["turn_id"].bytes().filter(|s| !s.is_empty()) {
            turns.insert((session.to_vec(), turn.to_vec()));
        }
        if session.is_empty() {
            continue;
        }
        let totals = sessions.entry(session.to_vec()).or_insert_with(|| obj!());
        for key in keys {
            if let Some(new) = row[key].int() {
                let old = totals[key].int().unwrap_or("0");
                if new.len() > old.len() || new.len() == old.len() && new > old {
                    totals[key] = row[key].clone();
                }
            }
        }
    }
    let mut usage = obj!();
    for key in keys {
        usage[key] = 0.into();
        for totals in sessions.values() {
            usage.add(key, &totals[key]);
        }
    }
    Ok(
        obj!("events":rows.len(),"invalid_lines":invalid,"sessions":sessions.len(),"turns":turns.len(),"tool_events":tool,
        "subagent_events":subagent,"usage_aggregation":"per-session-max","usage":usage),
    )
}
