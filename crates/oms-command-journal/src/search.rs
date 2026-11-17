#![forbid(unsafe_code)]
use crate::json::{JsonExt, MapExt, Value, json};
use crate::sanitize::os_str_to_py_string;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const SEARCH_KINDS: &[&str] = &["journal", "handoff", "thread", "dispatch", "result"];
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_FILES_PER_KIND: usize = 300;
pub const MAX_COLLECTED: usize = 2000;
pub const TIME_BUDGET_SECS: f64 = 5.0;
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 200;
pub const SNIPPET_LEN: usize = 160;
pub const QUERY_MAX_LEN: usize = 200;
const WITHHELD: &str = "[withheld: sensitive-looking content]";
const UNSCANNED: &str = concat!("[withheld: the sec", "ret scanner could not run]");
const SCOPE: &str = "scope: .oms records only (journal, handoff, thread, dispatch, result); sealed journal segments and provider-native transcripts are not read. A hit says where to look, not that anything was verified.";

struct Candidate {
    kind: &'static str,
    path: PathBuf,
    seconds: i64,
    nanos: i64,
}
struct Hit {
    kind: &'static str,
    when: String,
    id: OsString,
    path: PathBuf,
    line: usize,
    open: OsString,
    excerpt: String,
    withheld: bool,
}
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if crate::unicode::printable(c) { c } else { ' ' })
        .collect()
}
fn collect(repo: &Path, kind: &'static str) -> Vec<Candidate> {
    let base = repo.join(".oms");
    let folders: Vec<(PathBuf, &[&str])> = match kind {
        "journal" => vec![
            (base.join("work-journal/daily"), &["md", "json"]),
            (base.join("work-journal/weekly"), &["md", "json"]),
        ],
        "handoff" => vec![(base.join("handoffs"), &["md"])],
        "thread" => vec![(base.join("threads"), &["jsonl"])],
        "dispatch" => vec![(base.join("panel-state/dispatch-logs"), &["log"])],
        "result" => vec![(base.join("artifacts/panel-results"), &["md", "json"])],
        _ => vec![],
    };
    let mut paths = Vec::new();
    if kind == "journal" {
        paths.push(base.join("work-journal/events.jsonl"));
    }
    for (folder, extensions) in folders {
        if !fs::symlink_metadata(&folder).is_ok_and(|s| s.is_dir() && !s.file_type().is_symlink()) {
            continue;
        }
        if let Ok(entries) = fs::read_dir(folder) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|s| extensions.contains(&s))
                {
                    paths.push(path);
                }
            }
        }
    }
    paths
        .into_iter()
        .filter_map(|path| {
            let s = fs::symlink_metadata(&path).ok()?;
            if !s.is_file() || s.file_type().is_symlink() {
                return None;
            }
            Some(Candidate {
                kind,
                path,
                seconds: s.mtime(),
                nanos: s.mtime_nsec(),
            })
        })
        .collect()
}
fn tail(path: &Path) -> Option<(String, usize, bool)> {
    let parent = crate::files::Parent::open(path).ok()?;
    let before = parent.stat().ok()??;
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let opened = rustix::fs::fstat(&descriptor).ok()?;
    if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
        return None;
    }
    let size = u64::try_from(opened.st_size).ok()?;
    let cut = size > MAX_FILE_BYTES;
    let mut file = File::from(descriptor);
    let start = size.saturating_sub(MAX_FILE_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut data = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES)
        .read_to_end(&mut data)
        .ok()?;
    let mut first = 1;
    if cut {
        file.seek(SeekFrom::Start(0)).ok()?;
        let mut remaining = start;
        let mut skipped = 0;
        let mut buffer = [0; 65536];
        while remaining > 0 {
            let bound = remaining.min(buffer.len() as u64) as usize;
            let n = file.read(&mut buffer[..bound]).ok()?;
            if n == 0 {
                return None;
            }
            remaining -= n as u64;
            skipped += buffer[..n].iter().filter(|b| **b == b'\n').count();
        }
        let newline = data.iter().position(|b| *b == b'\n');
        if let Some(n) = newline {
            data.drain(..=n);
        } else {
            data.clear();
        }
        first = skipped + 2;
    }
    let after = rustix::fs::fstat(&file).ok()?;
    let named = parent.stat().ok()??;
    if after.st_size != opened.st_size
        || after.st_mtime != opened.st_mtime
        || after.st_mtime_nsec != opened.st_mtime_nsec
        || after.st_ctime != opened.st_ctime
        || after.st_ctime_nsec != opened.st_ctime_nsec
        || named.st_dev != after.st_dev
        || named.st_ino != after.st_ino
    {
        return None;
    }
    Some((String::from_utf8_lossy(&data).into_owned(), first, cut))
}
fn strings(value: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 12 {
        return;
    }
    match value {
        Value::String(s) => out.push(crate::sanitize::display_py_string(s)),
        Value::Object(o) => {
            for v in o.values() {
                strings(v, out, depth + 1);
            }
        }
        Value::Array(a) => {
            for v in a {
                strings(v, out, depth + 1);
            }
        }
        _ => {}
    }
}
fn excerpt(text: &str, at: usize, length: usize) -> String {
    let start = at.saturating_sub(SNIPPET_LEN.saturating_sub(length) / 2);
    let chars: Vec<_> = text.chars().collect();
    let piece: String = chars.iter().skip(start).take(SNIPPET_LEN).collect();
    let piece = clean(&piece)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{}{piece}{}",
        if start > 0 { "…" } else { "" },
        if start + SNIPPET_LEN < chars.len() {
            "…"
        } else {
            ""
        }
    )
}
fn match_position(text: &str, folded_needle: &str) -> Option<usize> {
    if text.is_ascii() && folded_needle.is_ascii() {
        if folded_needle.is_empty() {
            return Some(0);
        }
        text.as_bytes()
            .windows(folded_needle.len())
            .position(|part| part.eq_ignore_ascii_case(folded_needle.as_bytes()))
    } else {
        let folded = crate::unicode::casefold(text);
        folded
            .find(folded_needle)
            .map(|byte| folded[..byte].chars().count())
    }
}

pub fn run_recorded_search(
    repo: &Path,
    query: &str,
    kinds: &[String],
    limit: usize,
    json_out: bool,
) -> Result<(i32, Vec<u8>), String> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > QUERY_MAX_LEN || query.contains('\n') {
        return Err(format!(
            "--grep needs 1 to {QUERY_MAX_LEN} characters of plain text on one line"
        ));
    }
    if kinds.iter().any(|k| !SEARCH_KINDS.contains(&k.as_str())) {
        return Err(format!("--kind is one of: {}", SEARCH_KINDS.join(", ")));
    }
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(format!("--limit is 1 to {MAX_LIMIT}"));
    }
    let wanted: Vec<_> = SEARCH_KINDS
        .iter()
        .copied()
        .filter(|k| kinds.is_empty() || kinds.iter().any(|v| v == k))
        .collect();
    let started = Instant::now();
    let mut files = Vec::new();
    let mut caps = Vec::new();
    let mut skipped = BTreeMap::new();
    for kind in &wanted {
        let mut selected = collect(repo, kind);
        selected.sort_by(|a, b| (b.seconds, b.nanos, &b.path).cmp(&(a.seconds, a.nanos, &a.path)));
        if selected.len() > MAX_FILES_PER_KIND {
            skipped.insert(*kind, selected.len() - MAX_FILES_PER_KIND);
            selected.truncate(MAX_FILES_PER_KIND);
        }
        files.extend(selected);
    }
    let mut hits = Vec::new();
    let mut scanned = 0;
    let mut truncated = 0;
    let mut unread = 0;
    let mut out_of_time = false;
    let mut overflow = false;
    let needle = crate::unicode::casefold(query);
    for (index, file) in files.iter().enumerate() {
        if started.elapsed().as_secs_f64() > TIME_BUDGET_SECS {
            out_of_time = true;
            unread = files.len() - index;
            break;
        }
        let Some((content, first, cut)) = tail(&file.path) else {
            continue;
        };
        scanned += 1;
        truncated += usize::from(cut);
        let relative = file.path.strip_prefix(repo).unwrap_or(&file.path);
        let rows = file.path.extension().is_some_and(|e| e == "jsonl");
        for (offset, line) in content.split('\n').enumerate() {
            if match_position(line, &needle).is_none() && !(rows && line.contains('\\')) {
                continue;
            }
            let parsed = if rows {
                crate::json::from_str(line).ok()
            } else {
                None
            };
            let haystack = if let Some(value) = parsed.as_ref().filter(|v| !v.is_null()) {
                let mut leaves = Vec::new();
                strings(value, &mut leaves, 0);
                leaves.join(" | ")
            } else {
                line.into()
            };
            let Some(position) = match_position(&haystack, &needle) else {
                continue;
            };
            if hits.len() >= MAX_COLLECTED {
                overflow = true;
                break;
            }
            let mut id = file.path.file_stem().unwrap_or_default().to_os_string();
            if file.kind == "journal"
                && let Some(value) = parsed
                    .as_ref()
                    .and_then(|r| r.get("event_id"))
                    .and_then(Value::as_str)
            {
                id = clean(&value).chars().take(80).collect::<String>().into();
            }
            let when = parsed
                .as_ref()
                .and_then(|row| {
                    ["occurred_at", "ts", "time", "created_at", "updated_at"]
                        .iter()
                        .filter_map(|k| row.get(k).and_then(Value::as_str))
                        .find(|s| {
                            s.chars().count() >= 10 && s.chars().take(4).all(|c| c.is_numeric())
                        })
                })
                .map(|s| s.chars().take(20).collect::<String>())
                .unwrap_or_else(|| {
                    chrono::DateTime::from_timestamp(file.seconds, 0)
                        .map(crate::tz::utc_rfc3339)
                        .unwrap_or_default()
                });
            let mut open = OsString::new();
            if file.kind == "thread" {
                open.push("oms thread show --id ");
                open.push(&id);
                open.push("  (");
            }
            open.push(relative);
            open.push(format!(":{}", first + offset));
            if file.kind == "thread" {
                open.push(")");
            }
            hits.push(Hit {
                kind: file.kind,
                when,
                id,
                path: relative.into(),
                line: first + offset,
                open,
                excerpt: excerpt(&haystack, position, needle.chars().count()),
                withheld: false,
            });
        }
        if overflow {
            unread = files.len() - index - 1;
            break;
        }
    }
    hits.sort_by(|a, b| (&b.when, &b.path, b.line).cmp(&(&a.when, &a.path, a.line)));
    let matches = hits.len();
    hits.truncate(limit);
    for hit in &mut hits {
        match oms_scrub::scan_secret(format!("{}\n", hit.excerpt).as_bytes(), &[]) {
            Ok(found) if found.is_empty() => {}
            Ok(_) => {
                hit.excerpt = WITHHELD.into();
                hit.withheld = true;
            }
            Err(_) => {
                hit.excerpt = UNSCANNED.into();
                hit.withheld = true;
            }
        }
    }
    for (kind, count) in skipped {
        caps.push(format!(
            "{kind}: {count} older file(s) not read (newest {MAX_FILES_PER_KIND} only)"
        ));
    }
    if truncated > 0 {
        caps.push(format!(
            "{truncated} file(s) longer than {MAX_FILE_BYTES} bytes were read from their end only"
        ));
    }
    if out_of_time {
        caps.push(format!(
            "time budget of 5s reached: {unread} file(s) not read"
        ));
    }
    if overflow {
        caps.push(format!(
            "more than {MAX_COLLECTED} matches: stopped collecting, {unread} file(s) not read"
        ));
    }
    if json_out {
        let report = json!({"schema":1,"kind":"oms-recorded-search","query":query,"kinds":wanted,"files_scanned":scanned,"matches":matches,"shown":hits.len(),"complete":caps.is_empty(),"caps":caps,"hits":[]});
        let mut report = crate::sanitize::to_oms_json(&report)?;
        let mut values = Vec::new();
        for hit in &hits {
            let value =
                json!({"kind":hit.kind,"when":hit.when,"line":hit.line,"excerpt":hit.excerpt});
            let mut value = crate::sanitize::to_oms_json(&value)?;
            if let oms_json::Value::Object(o) = &mut value {
                o.insert(
                    "id".into(),
                    oms_json::Value::String(os_str_to_py_string(&hit.id)),
                );
                o.insert(
                    "path".into(),
                    oms_json::Value::String(os_str_to_py_string(hit.path.as_os_str())),
                );
                o.insert(
                    "open".into(),
                    oms_json::Value::String(os_str_to_py_string(&hit.open)),
                );
                if hit.withheld {
                    o.insert("withheld".into(), oms_json::Value::Bool(true));
                }
            }
            values.push(value);
        }
        if let oms_json::Value::Object(o) = &mut report {
            o.insert("hits".into(), oms_json::Value::Array(values));
        }
        return Ok((
            i32::from(matches == 0),
            crate::sanitize::default_json_bytes(&report)?,
        ));
    }
    let query = String::from_utf8(crate::sanitize::canonical_json_bytes(&json!(query))?)
        .map_err(|e| e.to_string())?;
    let mut output=format!("recorded-work search for {query}: {matches} match(es) in {scanned} file(s) of {}; showing {}, newest first\n",if wanted.is_empty(){"nothing".into()}else{wanted.join(", ")},hits.len()).into_bytes();
    use std::os::unix::ffi::OsStrExt;
    for hit in &hits {
        output.extend_from_slice(format!("{:<8} {}  ", hit.kind, hit.when).as_bytes());
        output.extend_from_slice(hit.id.as_bytes());
        output.extend_from_slice(format!("\n    {}\n    open: ", hit.excerpt).as_bytes());
        output.extend_from_slice(hit.open.as_bytes());
        output.push(b'\n');
    }
    if matches > hits.len() {
        output.extend_from_slice(
            format!(
                "{} more match(es) not shown; raise --limit or narrow with --kind\n",
                matches - hits.len()
            )
            .as_bytes(),
        );
    }
    for cap in caps {
        output.extend_from_slice(format!("incomplete: {cap}\n").as_bytes());
    }
    output.extend_from_slice(SCOPE.as_bytes());
    output.push(b'\n');
    Ok((i32::from(matches == 0), output))
}
