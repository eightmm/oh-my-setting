#![forbid(unsafe_code)]

use crate::sanitize::{os_str_to_py_string, pretty_json_with_surrogates};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const SEARCH_KINDS: &[&str] = &["journal", "handoff", "thread", "dispatch", "result"];
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_FILES_PER_KIND: usize = 300;
pub const MAX_COLLECTED: usize = 2000;
pub const TIME_BUDGET_SECS: f64 = 5.0;
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 200;
pub const SNIPPET_LEN: usize = 160;
pub const QUERY_MAX_LEN: usize = 200;

#[derive(Clone, Debug)]
struct CandidateFile {
    kind: &'static str,
    rel_path: PathBuf,
    abs_path: PathBuf,
    mtime_secs: i64,
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub kind: &'static str,
    pub path: PathBuf,
    pub line: usize,
    pub snippet: String,
    pub redacted: bool,
    pub mtime_secs: i64,
}

pub fn run_recorded_search(
    repo_root: &Path,
    query: &str,
    kinds: &[String],
    limit: usize,
    json_out: bool,
) -> Result<(i32, Vec<u8>), String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err("error: --grep query must not be empty".to_string());
    }
    if trimmed.chars().count() > QUERY_MAX_LEN {
        return Err(format!(
            "error: --grep query exceeds {QUERY_MAX_LEN} characters"
        ));
    }
    if limit == 0 || limit > MAX_LIMIT {
        return Err(format!("error: --limit must be between 1 and {MAX_LIMIT}"));
    }

    let selected_kinds: Vec<&'static str> = if kinds.is_empty() {
        SEARCH_KINDS.to_vec()
    } else {
        let mut out = Vec::new();
        for k in kinds {
            let found = SEARCH_KINDS.iter().copied().find(|&sk| sk == k.as_str());
            let Some(valid) = found else {
                return Err(format!(
                    "error: unsupported --kind '{k}'; allowed: {}",
                    SEARCH_KINDS.join(", ")
                ));
            };
            if !out.contains(&valid) {
                out.push(valid);
            }
        }
        out
    };

    let deadline = Instant::now() + Duration::from_secs_f64(TIME_BUDGET_SECS);
    let mut truncated = false;
    let mut files_scanned = 0usize;
    let mut hits: Vec<SearchHit> = Vec::new();
    let needle_lower = trimmed.to_ascii_lowercase();

    for kind in &selected_kinds {
        let mut candidates = collect_kind_files(repo_root, kind);
        candidates.sort_by(|a, b| {
            b.mtime_secs
                .cmp(&a.mtime_secs)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
        });
        if candidates.len() > MAX_FILES_PER_KIND {
            candidates.truncate(MAX_FILES_PER_KIND);
            truncated = true;
        }
        for cand in candidates {
            if Instant::now() >= deadline || hits.len() >= MAX_COLLECTED {
                truncated = true;
                break;
            }
            files_scanned += 1;
            let (text, line_offset) = match read_tail_regular_file(&cand.abs_path) {
                Some(pair) => pair,
                None => continue,
            };
            for (idx, line) in text.lines().enumerate() {
                if !line.to_ascii_lowercase().contains(&needle_lower) {
                    continue;
                }
                let raw_snippet = make_snippet(line, trimmed, SNIPPET_LEN);
                let (snippet, redacted) = scrub_snippet(&raw_snippet);
                hits.push(SearchHit {
                    kind: cand.kind,
                    path: cand.rel_path.clone(),
                    line: line_offset + idx + 1,
                    snippet,
                    redacted,
                    mtime_secs: cand.mtime_secs,
                });
                if hits.len() >= MAX_COLLECTED {
                    truncated = true;
                    break;
                }
            }
        }
    }

    hits.sort_by(|a, b| {
        b.mtime_secs
            .cmp(&a.mtime_secs)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    let total_matched = hits.len();
    if hits.len() > limit {
        hits.truncate(limit);
    }

    if json_out {
        let mut obj = BTreeMap::new();
        obj.insert(
            oms_json::PyString::from("schema"),
            oms_json::Value::integer("1").map_err(|e| format!("pretty JSON error: {e}"))?,
        );
        obj.insert(
            oms_json::PyString::from("query"),
            oms_json::Value::String(oms_json::PyString::from(trimmed)),
        );
        obj.insert(
            oms_json::PyString::from("kinds"),
            oms_json::Value::Array(
                selected_kinds
                    .iter()
                    .map(|s| oms_json::Value::String(oms_json::PyString::from(*s)))
                    .collect(),
            ),
        );
        obj.insert(
            oms_json::PyString::from("limit"),
            oms_json::Value::integer(&limit.to_string())
                .map_err(|e| format!("pretty JSON error: {e}"))?,
        );
        obj.insert(
            oms_json::PyString::from("files_scanned"),
            oms_json::Value::integer(&files_scanned.to_string())
                .map_err(|e| format!("pretty JSON error: {e}"))?,
        );
        obj.insert(
            oms_json::PyString::from("total_matched"),
            oms_json::Value::integer(&total_matched.to_string())
                .map_err(|e| format!("pretty JSON error: {e}"))?,
        );
        obj.insert(
            oms_json::PyString::from("truncated"),
            oms_json::Value::Bool(truncated || total_matched > hits.len()),
        );
        let json_hits: Result<Vec<oms_json::Value>, String> = hits
            .iter()
            .map(|h| {
                let mut m = BTreeMap::new();
                m.insert(
                    oms_json::PyString::from("kind"),
                    oms_json::Value::String(oms_json::PyString::from(h.kind)),
                );
                m.insert(
                    oms_json::PyString::from("path"),
                    oms_json::Value::String(os_str_to_py_string(h.path.as_os_str())),
                );
                m.insert(
                    oms_json::PyString::from("line"),
                    oms_json::Value::integer(&h.line.to_string())
                        .map_err(|e| format!("pretty JSON error: {e}"))?,
                );
                m.insert(
                    oms_json::PyString::from("snippet"),
                    oms_json::Value::String(oms_json::PyString::from(h.snippet.as_str())),
                );
                m.insert(
                    oms_json::PyString::from("redacted"),
                    oms_json::Value::Bool(h.redacted),
                );
                Ok(oms_json::Value::Object(m))
            })
            .collect();
        obj.insert(
            oms_json::PyString::from("hits"),
            oms_json::Value::Array(json_hits?),
        );
        let mut bytes = pretty_json_with_surrogates(&oms_json::Value::Object(obj))?;
        bytes.push(b'\n');
        let code = if hits.is_empty() { 1 } else { 0 };
        return Ok((code, bytes));
    }

    if hits.is_empty() {
        let out = format!("no recorded matches for '{trimmed}'\n");
        return Ok((1, out.into_bytes()));
    }

    let mut text_out = Vec::new();
    for h in &hits {
        text_out.extend_from_slice(format!("[{}] ", h.kind).as_bytes());
        #[cfg(unix)]
        {
            text_out.extend_from_slice(h.path.as_os_str().as_bytes());
        }
        #[cfg(not(unix))]
        {
            text_out.extend_from_slice(h.path.to_string_lossy().as_bytes());
        }
        text_out.extend_from_slice(format!(":{} — {}\n", h.line, h.snippet).as_bytes());
    }
    if truncated || total_matched > hits.len() {
        text_out.extend_from_slice(
            format!(
                "... showing {} of {} match(es) across {files_scanned} file(s)\n",
                hits.len(),
                total_matched
            )
            .as_bytes(),
        );
    }
    Ok((0, text_out))
}

fn collect_kind_files(repo_root: &Path, kind: &'static str) -> Vec<CandidateFile> {
    let oms_dir = repo_root.join(".oms");
    let mut dirs = Vec::new();
    match kind {
        "journal" => {
            dirs.push(oms_dir.join("work-journal/daily"));
            dirs.push(oms_dir.join("work-journal/weekly"));
            dirs.push(oms_dir.join("work-journal"));
        }
        "handoff" => {
            dirs.push(oms_dir.join("handoffs"));
            dirs.push(oms_dir.join("sessions"));
        }
        "thread" => {
            dirs.push(oms_dir.join("rooms"));
            dirs.push(oms_dir.join("threads"));
        }
        "dispatch" => {
            dirs.push(oms_dir.join("dispatch"));
            dirs.push(oms_dir.join("peers"));
        }
        "result" => {
            dirs.push(oms_dir.join("results"));
            dirs.push(oms_dir.join("artifacts"));
            dirs.push(oms_dir.join("runs"));
        }
        _ => {}
    }

    let mut out = Vec::new();
    for dir in dirs {
        walk_dir_no_symlink(repo_root, &dir, kind, &mut out, 0);
    }
    out
}

fn walk_dir_no_symlink(
    repo_root: &Path,
    dir: &Path,
    kind: &'static str,
    out: &mut Vec<CandidateFile>,
    depth: usize,
) {
    if depth > 4 || out.len() >= MAX_FILES_PER_KIND * 2 {
        return;
    }
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(em) = fs::symlink_metadata(&p) else {
            continue;
        };
        if em.file_type().is_symlink() {
            continue;
        }
        if em.is_dir() {
            if kind == "journal" && depth == 0 {
                let name = entry.file_name();
                if is_excluded_journal_subdir(&name) {
                    continue;
                }
            }
            walk_dir_no_symlink(repo_root, &p, kind, out, depth + 1);
            continue;
        }
        if !em.is_file() {
            continue;
        }
        let fname = entry.file_name();
        if !matches_extension_os(&fname) {
            continue;
        }
        let mtime_secs = em
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let rel_path = p
            .strip_prefix(repo_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from(&fname));
        out.push(CandidateFile {
            kind,
            rel_path,
            abs_path: p,
            mtime_secs,
        });
    }
}

fn is_excluded_journal_subdir(name: &std::ffi::OsStr) -> bool {
    #[cfg(unix)]
    {
        matches!(name.as_bytes(), b"cold" | b"daily" | b"weekly")
    }
    #[cfg(not(unix))]
    {
        matches!(name.to_str(), Some("cold" | "daily" | "weekly"))
    }
}

fn matches_extension_os(name: &std::ffi::OsStr) -> bool {
    #[cfg(unix)]
    {
        let raw = name.as_bytes();
        raw.ends_with(b".md")
            || raw.ends_with(b".jsonl")
            || raw.ends_with(b".json")
            || raw.ends_with(b".txt")
            || raw.ends_with(b".log")
    }
    #[cfg(not(unix))]
    {
        name.to_str().is_some_and(|s| {
            s.ends_with(".md")
                || s.ends_with(".jsonl")
                || s.ends_with(".json")
                || s.ends_with(".txt")
                || s.ends_with(".log")
        })
    }
}

fn read_tail_regular_file(path: &Path) -> Option<(String, usize)> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    let len = meta.len();
    let mut file = File::open(path).ok()?;
    let mut buf = Vec::new();
    if len > MAX_FILE_BYTES {
        file.seek(SeekFrom::Start(len - MAX_FILE_BYTES)).ok()?;
        file.read_to_end(&mut buf).ok()?;
        if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            buf.drain(..=nl);
        }
    } else {
        file.read_to_end(&mut buf).ok()?;
    }
    if buf.contains(&0) {
        return None;
    }
    Some((String::from_utf8_lossy(&buf).into_owned(), 0))
}

fn make_snippet(line: &str, query: &str, max_chars: usize) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    let q_lower = query.to_ascii_lowercase();
    let byte_idx = lower.find(&q_lower).unwrap_or(0);
    let char_idx = trimmed[..byte_idx].chars().count();
    let half = max_chars / 2;
    let start_char = char_idx.saturating_sub(half);
    let slice: String = trimmed.chars().skip(start_char).take(max_chars).collect();
    if start_char > 0 {
        format!("...{slice}")
    } else {
        format!("{slice}...")
    }
}

fn scrub_snippet(raw: &str) -> (String, bool) {
    match oms_scrub::scan_secret(raw.as_bytes(), &[]) {
        Ok(hits) if hits.is_empty() => (raw.to_string(), false),
        Ok(_) => ("[WITHHELD: sensitive match]".to_string(), true),
        Err(_) => ("[UNSCANNED]".to_string(), true),
    }
}
