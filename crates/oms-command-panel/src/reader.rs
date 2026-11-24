use crate::{Error, Result, input::Key, render, rows, text};
use oms_panel_core::{Store, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const TRANSCRIPT_MAX_BYTES: usize = 8 * 1024 * 1024;
const TRANSCRIPT_EXCERPT_BYTES: usize = 8 * 1024;
const TRANSCRIPT_NOT_CAPTURED: &str = "transcript not captured";
const TRANSCRIPT_TRUNCATED_NOTE: &str =
    "[transcript truncated: retained last 8 MiB; earlier events unavailable]";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Summary,
    Raw,
    Events,
    Diff,
    Info,
    Files,
    Transcript,
}
impl View {
    pub fn next(self) -> Self {
        match self {
            Self::Summary => Self::Raw,
            Self::Raw => Self::Events,
            Self::Events => Self::Diff,
            Self::Diff => Self::Info,
            Self::Info => Self::Files,
            Self::Files => Self::Transcript,
            Self::Transcript => Self::Summary,
        }
    }
}
type FileRows = Vec<(usize, String)>;
struct RenderedCache {
    view: View,
    width: usize,
    selected_file: usize,
    opened_file: Option<String>,
    lines: Arc<Vec<String>>,
}
#[derive(Default)]
pub struct Reader {
    pub view: View,
    pub offset: usize,
    pub query: String,
    pub searching: bool,
    pub section: usize,
    pub selected_file: usize,
    pub opened_file: Option<String>,
    files_offset: usize,
    file_rows: FileRows,
    rendered: Option<RenderedCache>,
}
impl Reader {
    /// Rendered lines for the current view and width; artifact reads, patch
    /// digests and redaction rerun only when either changes.
    pub fn lines(&mut self, repo: &Path, report: &Value, width: usize) -> Result<Arc<Vec<String>>> {
        if let Some(cache) = &self.rendered
            && cache.view == self.view
            && cache.width == width
            && cache.selected_file == self.selected_file
            && cache.opened_file == self.opened_file
        {
            return Ok(Arc::clone(&cache.lines));
        }
        let (rendered_lines, file_rows) = self.render_with_files(repo, report, width)?;
        if let Some(prev) = &self.rendered
            && prev.view == self.view
            && prev.opened_file == self.opened_file
            && prev.width != width
        {
            self.offset = resume_offset(&prev.lines, self.offset, &rendered_lines);
        }
        self.offset = self.offset.min(rendered_lines.len().saturating_sub(1));
        if self.view == View::Files && self.opened_file.is_none() {
            if file_rows.is_empty() {
                self.selected_file = 0;
            } else {
                self.selected_file = self.selected_file.min(file_rows.len() - 1);
            }
            self.file_rows = file_rows;
        } else if self.view != View::Files {
            self.file_rows.clear();
        }
        let lines = Arc::new(rendered_lines);
        self.rendered = Some(RenderedCache {
            view: self.view,
            width,
            selected_file: self.selected_file,
            opened_file: self.opened_file.clone(),
            lines: Arc::clone(&lines),
        });
        Ok(lines)
    }
    pub fn render(&self, repo: &Path, report: &Value, width: usize) -> Result<Vec<String>> {
        self.render_with_files(repo, report, width)
            .map(|(lines, _)| lines)
    }
    fn render_with_files(
        &self,
        repo: &Path,
        report: &Value,
        width: usize,
    ) -> Result<(Vec<String>, FileRows)> {
        if report["kind"] == "oms-panel-routes" {
            let routes = &report["routes"];
            if matches!(self.view, View::Summary | View::Info) {
                return Ok((
                    render::routes(routes, width)
                        .lines()
                        .map(str::to_owned)
                        .collect(),
                    Vec::new(),
                ));
            }
            let bytes = serde_json::to_vec(routes).map_err(Error::new)?;
            return Ok((
                render::wrap(&oms_panel_core::redact(&bytes)?, width),
                Vec::new(),
            ));
        }
        if report["kind"] == "oms-panel-teams" {
            if matches!(self.view, View::Summary | View::Info) {
                return Ok((
                    render::teams(report, width)
                        .lines()
                        .map(str::to_owned)
                        .collect(),
                    Vec::new(),
                ));
            }
            let bytes = serde_json::to_vec(report).map_err(Error::new)?;
            return Ok((
                render::wrap(&oms_panel_core::redact(&bytes)?, width),
                Vec::new(),
            ));
        }
        let mut out = Vec::new();
        let mut file_rows = Vec::new();
        let store = Store::open(repo)?;
        if self.view == View::Summary {
            return Ok((
                render::results(report, width)
                    .lines()
                    .map(str::to_owned)
                    .collect(),
                Vec::new(),
            ));
        }
        if self.view == View::Files
            && let Some(path) = &self.opened_file
        {
            out.push(format!("File diff: {path} (Esc returns to Files)"));
            let mut found_diff = false;
            for task in rows(&report["rows"]) {
                for call in rows(&task["calls"]) {
                    if call["patch"].is_string() {
                        let patch = oms_panel_core::results::verified_patch(&store, call)?;
                        let slice = oms_panel_core::files::file_diff(&patch, path)?;
                        if !slice.is_empty() {
                            found_diff = true;
                            let safe = oms_panel_core::redact(&slice)?;
                            out.extend(render::wrap(&safe, width));
                        }
                    }
                }
                let native = &task["native_assignment"]["report"];
                if native["status"] == "reported" && native["patch"].is_object() {
                    let row = serde_json::json!({
                        "patch": native["patch"]["path"],
                        "patch_sha256": native["patch"]["sha256"],
                    });
                    let patch = oms_panel_core::results::verified_patch(&store, &row)?;
                    let slice = oms_panel_core::files::file_diff(&patch, path)?;
                    if !slice.is_empty() {
                        found_diff = true;
                        let safe = oms_panel_core::redact(&slice)?;
                        out.extend(render::wrap(&safe, width));
                    }
                }
            }
            if !found_diff {
                out.push("No diff recorded for this file".into());
            }
            return Ok((out, Vec::new()));
        }
        for task in rows(&report["rows"]) {
            out.push(format!("Task {} · {:?}", text(task, "task_id"), self.view));
            let calls = rows(&task["calls"]);
            for call in calls {
                match self.view {
                    View::Diff | View::Files => {
                        if call["patch"].is_string() {
                            let patch = oms_panel_core::results::verified_patch(&store, call)?;
                            if self.view == View::Files {
                                for change in oms_panel_core::files::patch_changes(&patch)? {
                                    file_rows.push((out.len(), change.path.clone()));
                                    out.push(format!(
                                        "  {} +{} -{}",
                                        change.path, change.added, change.removed
                                    ));
                                }
                            } else {
                                let safe = oms_panel_core::redact(&patch)?;
                                out.extend(render::wrap(&safe, width));
                            }
                        }
                    }
                    View::Events => out.extend(render::value_lines(call, width)),
                    View::Info => {
                        for key in [
                            "attempt_id",
                            "kind",
                            "selected_model",
                            "reason_code",
                            "exit",
                            "verify_exit",
                            "duration_ms",
                            "base_sha",
                            "patch_sha256",
                        ] {
                            if let Some(value) = call.get(key)
                                && !value.is_null()
                            {
                                out.extend(render::wrap(&format!("{key}: {value}"), width));
                            }
                        }
                    }
                    View::Raw => {
                        if call["artifact"].is_string() {
                            let raw = oms_panel_core::results::indexed_artifact(&store, call)?;
                            let safe = oms_panel_core::redact(&raw)?;
                            out.extend(render::wrap(&safe, width));
                        } else {
                            let excerpt = call["answer"]
                                .as_str()
                                .or_else(|| call["answer_excerpt"].as_str())
                                .unwrap_or_default();
                            let safe = oms_panel_core::redact(excerpt.as_bytes())?;
                            out.extend(render::wrap(&safe, width));
                            if call["answer_truncated"] == true {
                                out.push("... (answer truncated)".into());
                            }
                        }
                    }
                    View::Transcript => {
                        if let Some(lines) = load_dispatch_transcript(repo, call, width)? {
                            out.extend(lines);
                        } else {
                            out.push(TRANSCRIPT_NOT_CAPTURED.into());
                        }
                    }
                    View::Summary => {}
                }
            }
            // Native reports remain a separate evidence surface. They never
            // become delegate calls or supply implement/verify exit statuses.
            let native = &task["native_assignment"]["report"];
            if native["status"] == "reported" {
                out.extend(render::wrap(
                    &format!(
                        "Native {} report · owner imported · acceptance ineligible",
                        text(native, "result_kind")
                    ),
                    width,
                ));
                match self.view {
                    View::Diff | View::Files => {
                        if native["patch"].is_object() {
                            let row = serde_json::json!({"patch":native["patch"]["path"],
                                "patch_sha256":native["patch"]["sha256"]});
                            let patch = oms_panel_core::results::verified_patch(&store, &row)?;
                            if self.view == View::Files {
                                for change in oms_panel_core::files::patch_changes(&patch)? {
                                    file_rows.push((out.len(), change.path.clone()));
                                    out.push(format!(
                                        "  {} +{} -{}",
                                        change.path, change.added, change.removed
                                    ));
                                }
                            } else {
                                out.extend(render::wrap(&oms_panel_core::redact(&patch)?, width));
                            }
                        }
                    }
                    View::Raw => {
                        for (i, reference) in std::iter::once(&native["source"])
                            .chain(rows(&native["verification"]).iter())
                            .enumerate()
                        {
                            if i > 0 {
                                out.push(format!(
                                    "Reported verification evidence {i} · no certified verdict"
                                ));
                            }
                            let row = serde_json::json!({"artifact":reference["path"],
                                "artifact_sha256":reference["sha256"]});
                            let raw = oms_panel_core::results::indexed_artifact(&store, &row)?;
                            out.extend(render::wrap(&oms_panel_core::redact(&raw)?, width));
                        }
                    }
                    View::Transcript => {
                        if let Some(lines) =
                            load_dispatch_transcript(repo, &task["native_assignment"], width)?
                        {
                            out.extend(lines);
                        } else {
                            out.push(TRANSCRIPT_NOT_CAPTURED.into());
                        }
                    }
                    View::Events | View::Info => out.extend(render::value_lines(native, width)),
                    View::Summary => {}
                }
            } else if native["status"] == "unavailable" {
                out.extend(render::wrap(
                    &format!("Native report unavailable: {}", text(native, "issue")),
                    width,
                ));
            } else if self.view == View::Transcript && calls.is_empty() {
                out.push(TRANSCRIPT_NOT_CAPTURED.into());
            }
        }
        if self.view == View::Files && !file_rows.is_empty() {
            let sel = self.selected_file.min(file_rows.len() - 1);
            let line_idx = file_rows[sel].0;
            if let Some(rest) = out[line_idx].strip_prefix("  ") {
                out[line_idx] = format!("▸ {rest}");
            }
        }
        if out.is_empty() {
            out.push(
                if self.view == View::Transcript {
                    TRANSCRIPT_NOT_CAPTURED
                } else {
                    "No recorded evidence for this view"
                }
                .into(),
            );
        }
        Ok((out, file_rows))
    }
    fn switch_view(&mut self, view: View) {
        self.view = view;
        self.offset = 0;
        self.selected_file = 0;
        self.opened_file = None;
        self.files_offset = 0;
    }
    pub fn apply(&mut self, key: &Key, lines: &[String], height: usize) -> bool {
        let page = body_rows(height);
        if self.searching {
            match key {
                Key::Escape => {
                    self.searching = false;
                    self.query.clear();
                }
                Key::Enter => {
                    self.searching = false;
                    self.find(lines, false);
                }
                Key::Backspace => {
                    self.query.pop();
                }
                Key::Char(c) if self.query.chars().count() < 80 => self.query.push(*c),
                _ => {}
            }
            return true;
        }
        if self.view == View::Files {
            if self.opened_file.is_some() {
                if matches!(key, Key::Escape | Key::Char('f')) {
                    self.opened_file = None;
                    self.offset = self.files_offset;
                    return true;
                }
            } else {
                let parsed;
                let file_rows = if self.file_rows.is_empty() {
                    parsed = parse_file_rows(lines);
                    &parsed
                } else {
                    &self.file_rows
                };
                if !file_rows.is_empty() {
                    let count = file_rows.len();
                    let next = match key {
                        Key::Enter => {
                            let sel = self.selected_file.min(count - 1);
                            let path = file_rows[sel].1.clone();
                            self.selected_file = sel;
                            self.files_offset = self.offset;
                            self.opened_file = Some(path);
                            self.offset = 0;
                            return true;
                        }
                        Key::Up => Some(self.selected_file.saturating_sub(1)),
                        Key::Down => Some(self.selected_file.saturating_add(1).min(count - 1)),
                        Key::PageUp => Some(self.selected_file.saturating_sub(page)),
                        Key::PageDown | Key::Char(' ') => {
                            Some(self.selected_file.saturating_add(page).min(count - 1))
                        }
                        Key::Home => Some(0),
                        Key::End => Some(count - 1),
                        Key::Scroll(n) if *n < 0 => {
                            Some(self.selected_file.saturating_sub(n.unsigned_abs() as usize))
                        }
                        Key::Scroll(n) => Some(
                            self.selected_file
                                .saturating_add(*n as usize)
                                .min(count - 1),
                        ),
                        _ => None,
                    };
                    if let Some(sel) = next {
                        let line_idx = file_rows[sel].0;
                        self.selected_file = sel;
                        if sel == 0 && matches!(key, Key::Home) {
                            self.offset = 0;
                        } else {
                            if line_idx < self.offset {
                                self.offset = line_idx;
                            } else if line_idx >= self.offset.saturating_add(page) {
                                self.offset = line_idx.saturating_sub(page - 1);
                            }
                            self.offset = self.offset.min(lines.len().saturating_sub(1));
                        }
                        return true;
                    }
                }
            }
        }
        match key {
            Key::Char('q') | Key::Escape | Key::Eof => return false,
            Key::Tab => self.switch_view(self.view.next()),
            Key::Char('r') => self.switch_view(View::Raw),
            Key::Char('e') => self.switch_view(View::Events),
            Key::Char('d') => self.switch_view(View::Diff),
            Key::Char('i') => self.switch_view(View::Info),
            Key::Char('x') => self.switch_view(View::Transcript),
            Key::Char('f') => self.switch_view(View::Files),
            Key::Char('/') => self.searching = true,
            Key::Char('n') => self.find(lines, false),
            Key::Char('N') => self.find(lines, true),
            Key::Up => self.offset = self.offset.saturating_sub(1),
            Key::Down => self.offset = self.offset.saturating_add(1),
            Key::PageUp => self.offset = self.offset.saturating_sub(page),
            Key::PageDown | Key::Char(' ') => self.offset = self.offset.saturating_add(page),
            Key::Home => self.offset = 0,
            Key::End => self.offset = lines.len().saturating_sub(page),
            Key::Scroll(n) if *n < 0 => {
                self.offset = self.offset.saturating_sub(n.unsigned_abs() as usize)
            }
            Key::Scroll(n) => self.offset = self.offset.saturating_add(*n as usize),
            Key::Char('[' | ']') => {
                let backwards = *key == Key::Char('[');
                let sections: Vec<_> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| line.starts_with("Task ") || line.starts_with('#'))
                    .map(|(i, _)| i)
                    .collect();
                if backwards {
                    if let Some(i) = sections.into_iter().rev().find(|i| *i < self.offset) {
                        self.offset = i;
                    }
                } else if let Some(i) = sections.into_iter().find(|i| *i > self.offset) {
                    self.offset = i;
                }
            }
            _ => {}
        }
        self.offset = self.offset.min(lines.len().saturating_sub(1));
        true
    }
    fn find(&mut self, lines: &[String], backwards: bool) {
        if self.query.is_empty() || lines.is_empty() {
            return;
        }
        let needle = self.query.to_lowercase();
        for n in 1..=lines.len() {
            let index = if backwards {
                (self.offset + lines.len() - n % lines.len()) % lines.len()
            } else {
                (self.offset + n) % lines.len()
            };
            if lines[index].to_lowercase().contains(&needle) {
                self.offset = index;
                return;
            }
        }
    }
}

fn body_rows(height: usize) -> usize {
    height
        .saturating_sub(if height >= 5 { 3 } else { 2 })
        .max(1)
}

fn parse_file_rows(lines: &[String]) -> Vec<(usize, String)> {
    let mut rows = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let stripped = line
            .strip_prefix("▸ ")
            .or_else(|| line.strip_prefix("  "))
            .unwrap_or(line.as_str());
        let mut parts = stripped.rsplitn(3, ' ');
        let Some(removed) = parts.next() else {
            continue;
        };
        let Some(added) = parts.next() else {
            continue;
        };
        let Some(path) = parts.next() else {
            continue;
        };
        if !path.is_empty()
            && added.starts_with('+')
            && added[1..].bytes().all(|b| b.is_ascii_digit())
            && removed.starts_with('-')
            && removed[1..].bytes().all(|b| b.is_ascii_digit())
        {
            rows.push((i, path.to_owned()));
        }
    }
    rows
}

fn visible_non_ws_count(line: &str) -> usize {
    let mut count = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for seq in chars.by_ref() {
                if seq == 'm' {
                    break;
                }
            }
            continue;
        }
        if !c.is_whitespace() {
            count += 1;
        }
    }
    count
}

/// Re-anchor `offset` after a width resize by counting visible non-whitespace
/// characters above the first shown row so wrapped text keeps its place.
fn resume_offset(old_lines: &[String], offset: usize, new_lines: &[String]) -> usize {
    if offset == 0 || new_lines.is_empty() {
        return 0;
    }
    let before: usize = old_lines
        .iter()
        .take(offset)
        .map(|line| visible_non_ws_count(line))
        .sum();
    if before == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (index, line) in new_lines.iter().enumerate() {
        seen = seen.saturating_add(visible_non_ws_count(line));
        if seen > before {
            return index;
        }
    }
    new_lines.len().saturating_sub(1)
}

fn transcript_candidates(call: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for raw in [
        call.get("participant").and_then(Value::as_str),
        call.get("call_id").and_then(Value::as_str),
        call.get("refs")
            .and_then(|r| r.get("panel_room_participant"))
            .and_then(Value::as_str),
        call.get("attempt_id").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    {
        if crate::identifier(raw).is_ok() && !ids.iter().any(|existing| existing == raw) {
            ids.push(raw.to_owned());
        }
    }
    let mut filenames = Vec::new();
    for id in ids {
        let direct = format!("{id}.transcript");
        if !filenames.contains(&direct) {
            filenames.push(direct);
        }
        if !id.starts_with("call-") {
            let prefixed = format!("call-{id}");
            if crate::identifier(&prefixed).is_ok() {
                let name = format!("{prefixed}.transcript");
                if !filenames.contains(&name) {
                    filenames.push(name);
                }
            }
        }
    }
    filenames
}

/// Read an owner-scoped dispatch transcript from `.oms/panel-state/dispatch-logs/`
/// without following symlinks, leaving the repository, or reading provider homes.
fn read_dispatch_transcript(repo: &Path, filename: &str) -> Option<(Vec<u8>, bool)> {
    use rustix::fs::{self, FileType, Mode, OFlags};

    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\0')
        || !filename.ends_with(".transcript")
    {
        return None;
    }
    let euid = rustix::process::geteuid().as_raw();
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root_fd = fs::open(
        repo,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let oms_fd = fs::openat(&root_fd, ".oms", dir_flags, Mode::empty()).ok()?;
    let state_fd = fs::openat(&oms_fd, "panel-state", dir_flags, Mode::empty()).ok()?;
    let logs_fd = fs::openat(&state_fd, "dispatch-logs", dir_flags, Mode::empty()).ok()?;
    let dir_stat = fs::fstat(&logs_fd).ok()?;
    if FileType::from_raw_mode(dir_stat.st_mode) != FileType::Directory
        || dir_stat.st_uid != euid
        || (dir_stat.st_mode & 0o022) != 0
    {
        return None;
    }
    let fd = fs::openat(
        &logs_fd,
        filename,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let before = fs::fstat(&fd).ok()?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || before.st_nlink != 1
        || before.st_uid != euid
        || (before.st_mode & 0o022) != 0
        || before.st_size < 0
    {
        return None;
    }
    let size = before.st_size as u64;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    let truncated = if size > TRANSCRIPT_MAX_BYTES as u64 {
        let start = size - TRANSCRIPT_MAX_BYTES as u64;
        file.seek(SeekFrom::Start(start)).ok()?;
        (&mut file)
            .take(TRANSCRIPT_MAX_BYTES as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if let Some(pos) = bytes.iter().position(|&b| b == b'\n') {
            bytes.drain(..=pos);
        } else {
            bytes.clear();
        }
        true
    } else {
        (&mut file)
            .take(TRANSCRIPT_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > TRANSCRIPT_MAX_BYTES {
            return None;
        }
        false
    };
    let after = fs::fstat(&file).ok()?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || after.st_nlink != 1
        || after.st_uid != euid
        || (after.st_mode & 0o022) != 0
    {
        return None;
    }
    Some((bytes, truncated))
}

fn transcript_value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn transcript_excerpt(value: &Value) -> String {
    let text = transcript_value_text(value);
    if text.len() <= TRANSCRIPT_EXCERPT_BYTES {
        return text;
    }
    let mut end = TRANSCRIPT_EXCERPT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output excerpt truncated]", &text[..end])
}

fn format_transcript_bytes(bytes: &[u8], truncated: bool) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = Vec::new();
    let mut tools: HashMap<String, usize> = HashMap::new();
    let mut saw_truncation = false;
    if truncated {
        lines.push(TRANSCRIPT_TRUNCATED_NOTE.into());
        saw_truncation = true;
    }
    for raw_line in raw.lines() {
        let trimmed = raw_line.trim_end_matches('\r');
        if trimmed.is_empty() {
            continue;
        }
        let Ok(Value::Object(doc)) = serde_json::from_str::<Value>(trimmed) else {
            lines.push(trimmed.to_owned());
            continue;
        };
        if doc.get("oms_transcript_truncated") == Some(&Value::Bool(true)) {
            if !saw_truncation {
                lines.push(TRANSCRIPT_TRUNCATED_NOTE.into());
                saw_truncation = true;
            }
            continue;
        }
        let doc_type = doc.get("type").and_then(Value::as_str).unwrap_or_default();
        if doc_type == "item.completed"
            && let Some(Value::Object(item)) = doc.get("item")
        {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            match kind {
                "agent_message" | "reasoning" | "summary" => {
                    lines.push(if kind == "agent_message" {
                        "Assistant:".into()
                    } else {
                        "Reasoning:".into()
                    });
                    let body = item
                        .get("text")
                        .or_else(|| item.get("summary"))
                        .map(transcript_value_text)
                        .unwrap_or_default();
                    if !body.is_empty() {
                        lines.push(body);
                    }
                }
                "command_execution" => {
                    let cmd = item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let exit = item
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "unknown".into());
                    lines.push(format!("Command: {cmd} / exit {exit}"));
                    if let Some(output) = item.get("aggregated_output") {
                        let excerpt = transcript_excerpt(output);
                        if !excerpt.is_empty() {
                            lines.push(excerpt);
                        }
                    }
                }
                "file_change" => {
                    if let Some(changes) = item.get("changes").and_then(Value::as_array) {
                        for change in changes {
                            if let Some(obj) = change.as_object() {
                                let path =
                                    obj.get("path").and_then(Value::as_str).unwrap_or_default();
                                lines.push(format!("Edit: {path}"));
                                let diff = obj
                                    .get("diff")
                                    .or_else(|| obj.get("patch"))
                                    .map(transcript_value_text)
                                    .unwrap_or_default();
                                if !diff.is_empty() {
                                    lines.push(diff);
                                }
                            }
                        }
                    }
                }
                _ => {
                    let name = item.get("name").and_then(Value::as_str).unwrap_or(kind);
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    if !name.is_empty() {
                        lines.push(format!("Tool: {name} / status {status}"));
                    }
                    if let Some(args) = item.get("arguments") {
                        let text = transcript_value_text(args);
                        if !text.is_empty() {
                            lines.push(text);
                        }
                    }
                    if let Some(res) = item.get("result").or_else(|| item.get("error")) {
                        let excerpt = transcript_excerpt(res);
                        if !excerpt.is_empty() {
                            lines.push(excerpt);
                        }
                    }
                }
            }
        } else if matches!(doc_type, "assistant" | "user") {
            let content = doc
                .get("message")
                .and_then(|m| m.get("content"))
                .or_else(|| doc.get("content"))
                .and_then(Value::as_array);
            if let Some(blocks) = content {
                for block in blocks {
                    let Some(obj) = block.as_object() else {
                        continue;
                    };
                    let kind = obj.get("type").and_then(Value::as_str).unwrap_or_default();
                    match kind {
                        "text" | "thinking" if doc_type == "assistant" => {
                            lines.push(if kind == "text" {
                                "Assistant:".into()
                            } else {
                                "Reasoning:".into()
                            });
                            let body = obj
                                .get("text")
                                .or_else(|| obj.get("thinking"))
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !body.is_empty() {
                                lines.push(body.to_owned());
                            }
                        }
                        "tool_use" => {
                            let name = obj.get("name").and_then(Value::as_str).unwrap_or("unknown");
                            let input = obj.get("input").and_then(Value::as_object);
                            let prefix = if matches!(name, "Bash" | "bash") {
                                let cmd = input
                                    .and_then(|m| m.get("command"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                format!("Command: {cmd}")
                            } else {
                                format!("Tool: {name}")
                            };
                            if let Some(id) = obj.get("id").and_then(Value::as_str) {
                                tools.insert(id.to_owned(), lines.len());
                            }
                            lines.push(format!("{prefix} / status pending"));
                            if matches!(name, "Edit" | "Write" | "MultiEdit")
                                && let Some(input_map) = input
                            {
                                let file_path = input_map
                                    .get("file_path")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                lines.push(format!("Edit: {file_path}"));
                                if name == "Write" {
                                    if let Some(c) =
                                        input_map.get("content").and_then(Value::as_str)
                                    {
                                        lines.push(c.to_owned());
                                    }
                                } else {
                                    let edits: Vec<&Value> = if name == "MultiEdit" {
                                        input_map
                                            .get("edits")
                                            .and_then(Value::as_array)
                                            .map(|v| v.iter().collect())
                                            .unwrap_or_default()
                                    } else {
                                        vec![obj.get("input").unwrap()]
                                    };
                                    for edit in edits {
                                        if let Some(p) =
                                            edit.get("file_path").and_then(Value::as_str)
                                            && p != file_path
                                        {
                                            lines.push(format!("Edit: {p}"));
                                        }
                                        lines.push("@@ edit @@".into());
                                        if let Some(old) =
                                            edit.get("old_string").and_then(Value::as_str)
                                        {
                                            for l in old.lines() {
                                                lines.push(format!("-{l}"));
                                            }
                                        }
                                        if let Some(new) =
                                            edit.get("new_string").and_then(Value::as_str)
                                        {
                                            for l in new.lines() {
                                                lines.push(format!("+{l}"));
                                            }
                                        }
                                    }
                                }
                            } else if !matches!(name, "Bash" | "bash")
                                && let Some(raw_input) = obj.get("input")
                            {
                                let text = transcript_value_text(raw_input);
                                if !text.is_empty() {
                                    lines.push(text);
                                }
                            }
                        }
                        "tool_result" => {
                            let code = doc
                                .get("tool_use_result")
                                .and_then(|r| r.get("exit_code"))
                                .and_then(Value::as_i64);
                            let status = if let Some(c) = code {
                                format!("exit {c}")
                            } else if obj.get("is_error") == Some(&Value::Bool(true)) {
                                "status failed".into()
                            } else if obj.get("is_error") == Some(&Value::Bool(false)) {
                                "status succeeded".into()
                            } else {
                                "status unknown".into()
                            };
                            if let Some(id) = obj.get("tool_use_id").and_then(Value::as_str)
                                && let Some(&idx) = tools.get(id)
                                && let Some(slot) = lines.get_mut(idx)
                            {
                                let base = slot.strip_suffix(" / status pending").unwrap_or(slot);
                                *slot = format!("{base} / {status}");
                            }
                            lines.push(format!("Tool result: {status}"));
                            if let Some(c) = obj.get("content") {
                                let excerpt = transcript_excerpt(c);
                                if !excerpt.is_empty() {
                                    lines.push(excerpt);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        } else if doc_type == "result"
            && let Some(result_text) = doc.get("result").and_then(Value::as_str)
        {
            lines.push("Final report:".into());
            lines.push(result_text.to_owned());
        }
    }
    lines.join("\n")
}

fn load_dispatch_transcript(
    repo: &Path,
    call: &Value,
    width: usize,
) -> Result<Option<Vec<String>>> {
    for filename in transcript_candidates(call) {
        if let Some((bytes, truncated)) = read_dispatch_transcript(repo, &filename) {
            let formatted = format_transcript_bytes(&bytes, truncated);
            let safe = oms_panel_core::redact(formatted.as_bytes())?;
            let wrapped = render::wrap(&safe, width);
            return Ok(Some(if wrapped.is_empty() {
                vec!["No transcript events recorded".into()]
            } else {
                wrapped
            }));
        }
    }
    Ok(None)
}
pub fn page(
    repo: &Path,
    report: &Value,
    terminal: &mut crate::input::TerminalInput,
    interrupted: &AtomicBool,
) -> Result<()> {
    use std::io::IsTerminal;
    let mut stdout = std::io::stdout().lock();
    let tty = stdout.is_terminal();
    page_with(
        repo,
        report,
        &mut |timeout| terminal.wait(timeout),
        &mut stdout,
        &crate::layout::dimensions,
        tty,
        interrupted,
    )
}

pub fn page_with(
    repo: &Path,
    report: &Value,
    next_keys: &mut dyn FnMut(Duration) -> Result<Vec<Key>>,
    out: &mut dyn Write,
    size: &dyn Fn() -> (usize, usize),
    tty: bool,
    interrupted: &AtomicBool,
) -> Result<()> {
    let mut reader = Reader::default();
    let eol = if tty { "\r\n" } else { "\n" };
    let colored = render::color_enabled(tty);
    type Drawn = (
        usize,
        usize,
        usize,
        bool,
        String,
        View,
        usize,
        usize,
        Option<String>,
    );
    let mut drawn: Option<Drawn> = None;
    let mut rows: Vec<String> = Vec::new();
    while !interrupted.load(Ordering::Acquire) {
        let (width, height) = size();
        let lines = reader.lines(repo, report, width)?;
        reader.offset = reader.offset.min(lines.len().saturating_sub(1));
        let state: Drawn = (
            width,
            height,
            reader.offset,
            reader.searching,
            reader.query.clone(),
            reader.view,
            lines.len(),
            reader.selected_file,
            reader.opened_file.clone(),
        );
        if drawn.as_ref() != Some(&state) {
            let has_header = height >= 5;
            let body_rows = body_rows(height);
            let diff_styled = colored
                && (reader.view == View::Diff
                    || (reader.view == View::Files && reader.opened_file.is_some()));
            let mut frame = Vec::new();
            if has_header {
                frame.push(tab_header(reader.view, width, colored));
            }
            for line in lines.iter().skip(reader.offset).take(body_rows) {
                let clipped = render::clip(line, width);
                frame.push(if diff_styled {
                    style_diff_line(&clipped)
                } else {
                    clipped
                });
            }
            let total_lines = lines.len().max(1);
            let pos = (reader.offset + 1).min(total_lines);
            let pct = (pos * 100) / total_lines;
            let view_label = match (&reader.view, &reader.opened_file) {
                (View::Files, Some(path)) => format!("Files · {path}"),
                _ => view_name(reader.view).to_owned(),
            };
            let status_plain = render::clip(
                &format!(
                    "View: {view_label} · {} / {} ({pct}%){}",
                    reader.offset + 1,
                    lines.len(),
                    if reader.searching {
                        format!(" · /{}", reader.query)
                    } else {
                        String::new()
                    }
                ),
                width,
            );
            frame.push(if colored {
                format!("\x1b[38;5;245m{status_plain}\x1b[0m")
            } else {
                status_plain
            });
            let hint_plain = render::clip(crate::navigation::READER_HINT, width);
            frame.push(if colored {
                format!("\x1b[2m{hint_plain}\x1b[0m")
            } else {
                hint_plain
            });
            let mut screen = String::new();
            if tty {
                // Row overwrite inside synchronized output: no blank flash.
                let full = drawn.as_ref().is_none_or(|d| (d.0, d.1) != (width, height));
                screen.push_str("\x1b[?2026h");
                for (i, row) in frame.iter().enumerate() {
                    if full || rows.get(i) != Some(row) {
                        screen.push_str(&format!("\x1b[{};1H{row}\x1b[K", i + 1));
                    }
                }
                screen.push_str(&format!("\x1b[{};1H\x1b[J\x1b[?2026l", frame.len() + 1));
            } else {
                for (i, row) in frame.iter().enumerate() {
                    screen.push_str(row);
                    screen.push_str(if i + 1 < frame.len() { eol } else { "\n\n" });
                }
                screen.truncate(screen.len() - 1);
            }
            out.write_all(screen.as_bytes()).map_err(Error::new)?;
            out.flush().map_err(Error::new)?;
            rows = frame;
            drawn = Some(state);
        }
        for key in next_keys(Duration::from_millis(100))? {
            if !reader.apply(&key, &lines, height) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn view_name(view: View) -> &'static str {
    match view {
        View::Summary => "Summary",
        View::Raw => "Raw",
        View::Events => "Events",
        View::Diff => "Diff",
        View::Info => "Info",
        View::Files => "Files",
        View::Transcript => "Transcript",
    }
}

fn tab_header(active: View, width: usize, colored: bool) -> String {
    let tabs = [
        View::Summary,
        View::Raw,
        View::Events,
        View::Diff,
        View::Info,
        View::Files,
        View::Transcript,
    ];
    let items: Vec<String> = tabs
        .iter()
        .map(|&v| {
            let label = view_name(v);
            if v == active {
                format!("[{label}]")
            } else {
                label.to_owned()
            }
        })
        .collect();
    let plain = render::clip(&format!("OMS Reader │ {}", items.join("  ")), width);
    if colored {
        format!("\x1b[1;38;5;215m{plain}\x1b[0m")
    } else {
        plain
    }
}

fn style_diff_line(line: &str) -> String {
    if line.starts_with("diff ") || line.starts_with("--- ") || line.starts_with("+++ ") {
        format!("\x1b[1;38;5;215m{line}\x1b[0m")
    } else if line.starts_with("@@") {
        format!("\x1b[38;5;117m{line}\x1b[0m")
    } else if line.starts_with('+') {
        format!("\x1b[38;5;114m{line}\x1b[0m")
    } else if line.starts_with('-') {
        format!("\x1b[38;5;203m{line}\x1b[0m")
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicU64;

    struct TempRepo(std::path::PathBuf);
    impl TempRepo {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "oms-reader-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write_file(&self, rel: &str, bytes: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn transcript_reads_masked_dispatch_log_and_falls_back_to_not_captured() {
        let repo = TempRepo::new("transcript");
        let artifact = b"indexed-raw-artifact-body\n";
        let artifact_rel = ".oms/artifacts/call-raw.txt";
        repo.write_file(artifact_rel, artifact);
        let artifact_sha = oms_panel_core::digest(artifact);

        let report_with_capture = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [{
                    "participant": "call-alpha",
                    "attempt_id": "att-1",
                    "artifact": artifact_rel,
                    "artifact_sha256": artifact_sha,
                }]
            }]
        });

        // Without any dispatch log file, Transcript reports not captured while Raw reads artifact.
        let mut reader = Reader {
            view: View::Raw,
            ..Default::default()
        };
        let raw_lines = reader.render(&repo.0, &report_with_capture, 100).unwrap();
        assert!(raw_lines.join("\n").contains("indexed-raw-artifact-body"));

        reader.view = View::Transcript;
        let missing_lines = reader.render(&repo.0, &report_with_capture, 100).unwrap();
        assert!(
            missing_lines.iter().any(|l| l == TRANSCRIPT_NOT_CAPTURED),
            "expected not captured: {missing_lines:?}"
        );
        assert!(
            !missing_lines
                .join("\n")
                .contains("indexed-raw-artifact-body")
        );

        // With a private dispatch log containing JSONL events + a secret-shaped token + >8KiB output.
        let secret_like = concat!("sk-", "live_", "1234567890abcdefghijklmnop");
        let big_output = "x".repeat(TRANSCRIPT_EXCERPT_BYTES + 256);
        let jsonl = format!(
            concat!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"Checked parser state\"}}}}\n",
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"command\":\"cargo test\",\"exit_code\":0,\"aggregated_output\":\"{}\"}}}}\n",
                "{{\"type\":\"result\",\"result\":\"Done with token {}\"}}\n"
            ),
            big_output, secret_like
        );
        let logs_dir = repo.0.join(".oms/panel-state/dispatch-logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::set_permissions(&logs_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let transcript_file = logs_dir.join("call-alpha.transcript");
        std::fs::write(&transcript_file, jsonl.as_bytes()).unwrap();
        std::fs::set_permissions(&transcript_file, std::fs::Permissions::from_mode(0o600)).unwrap();

        let captured = reader.render(&repo.0, &report_with_capture, 120).unwrap();
        let joined = captured.join("\n");
        assert!(joined.contains("Checked parser state"), "{joined}");
        assert!(joined.contains("Command: cargo test / exit 0"), "{joined}");
        assert!(joined.contains("[output excerpt truncated]"), "{joined}");
        assert!(joined.contains("Final report:"), "{joined}");
        assert!(!joined.contains(secret_like), "secret leaked: {joined}");
        assert!(!joined.contains("indexed-raw-artifact-body"), "{joined}");

        // Symlinked transcript is refused and reports not captured.
        let outside = repo.write_file("outside.txt", b"should-not-read\n");
        std::os::unix::fs::symlink(&outside, logs_dir.join("call-sym.transcript")).unwrap();
        let sym_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T2",
                "calls": [{ "participant": "call-sym", "attempt_id": "att-2" }]
            }]
        });
        let sym_lines = reader.render(&repo.0, &sym_report, 100).unwrap();
        assert!(sym_lines.iter().any(|l| l == TRANSCRIPT_NOT_CAPTURED));
        assert!(!sym_lines.join("\n").contains("should-not-read"));

        // Hardlinked transcript is refused.
        let hard_src = repo.write_file("hard.txt", b"hardlink-body\n");
        std::fs::hard_link(&hard_src, logs_dir.join("call-hard.transcript")).unwrap();
        let hard_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T3",
                "calls": [{ "participant": "call-hard", "attempt_id": "att-3" }]
            }]
        });
        let hard_lines = reader.render(&repo.0, &hard_report, 100).unwrap();
        assert!(hard_lines.iter().any(|l| l == TRANSCRIPT_NOT_CAPTURED));
        assert!(!hard_lines.join("\n").contains("hardlink-body"));

        // Native report never reads native source artifact as a transcript.
        let native_src = b"native-source-only\n";
        let native_rel = ".oms/artifacts/native-src.txt";
        repo.write_file(native_rel, native_src);
        let native_sha = oms_panel_core::digest(native_src);
        let native_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T4",
                "calls": [],
                "native_assignment": {
                    "attempt_id": "att-native",
                    "report": {
                        "status": "reported",
                        "result_kind": "candidate",
                        "source": { "path": native_rel, "sha256": native_sha },
                        "verification": []
                    }
                }
            }]
        });
        reader.view = View::Raw;
        assert!(
            reader
                .render(&repo.0, &native_report, 100)
                .unwrap()
                .join("\n")
                .contains("native-source-only")
        );
        reader.view = View::Transcript;
        let native_tx = reader.render(&repo.0, &native_report, 100).unwrap();
        assert!(native_tx.iter().any(|l| l == TRANSCRIPT_NOT_CAPTURED));
        assert!(!native_tx.join("\n").contains("native-source-only"));
    }

    #[test]
    fn files_view_selects_files_drills_into_single_file_diff_and_returns_on_esc() {
        let repo = TempRepo::new("files");
        let patch = concat!(
            "diff --git a/src/alpha.rs b/src/alpha.rs\n",
            "--- a/src/alpha.rs\n",
            "+++ b/src/alpha.rs\n",
            "@@ -1 +1,2 @@\n",
            " old_alpha\n",
            "+new_alpha_line\n",
            "diff --git a/src/beta.rs b/src/beta.rs\n",
            "--- a/src/beta.rs\n",
            "+++ b/src/beta.rs\n",
            "@@ -1 +1,2 @@\n",
            " old_beta\n",
            "+new_beta_line\n",
        )
        .as_bytes();
        let patch_rel = ".oms/artifacts/worker.patch";
        repo.write_file(patch_rel, patch);
        let patch_sha = oms_panel_core::digest(patch);
        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [{
                    "attempt_id": "att-1",
                    "patch": patch_rel,
                    "patch_sha256": patch_sha,
                }]
            }]
        });

        let mut reader = Reader {
            view: View::Files,
            ..Default::default()
        };
        let list_lines = reader.lines(&repo.0, &report, 100).unwrap();
        assert!(
            list_lines.iter().any(|l| l == "▸ src/alpha.rs +1 -0"),
            "{list_lines:?}"
        );
        assert!(
            list_lines.iter().any(|l| l == "  src/beta.rs +1 -0"),
            "{list_lines:?}"
        );

        // Down selects the second file.
        assert!(reader.apply(&Key::Down, &list_lines, 12));
        assert_eq!(reader.selected_file, 1);
        let list_lines_2 = reader.lines(&repo.0, &report, 100).unwrap();
        assert!(list_lines_2.iter().any(|l| l == "  src/alpha.rs +1 -0"));
        assert!(list_lines_2.iter().any(|l| l == "▸ src/beta.rs +1 -0"));

        // Enter opens only beta.rs's diff slice.
        assert!(reader.apply(&Key::Enter, &list_lines_2, 12));
        assert_eq!(reader.opened_file.as_deref(), Some("src/beta.rs"));
        let diff_lines = reader.lines(&repo.0, &report, 100).unwrap();
        let diff_text = diff_lines.join("\n");
        assert!(diff_text.contains("File diff: src/beta.rs"), "{diff_text}");
        assert!(diff_text.contains("+new_beta_line"), "{diff_text}");
        assert!(
            !diff_text.contains("+new_alpha_line"),
            "alpha diff leaked: {diff_text}"
        );

        // Esc returns to the Files list without closing the reader and preserves selection.
        assert!(reader.apply(&Key::Escape, &diff_lines, 12));
        assert_eq!(reader.opened_file, None);
        assert_eq!(reader.selected_file, 1);
        let returned = reader.lines(&repo.0, &report, 100).unwrap();
        assert!(returned.iter().any(|l| l == "▸ src/beta.rs +1 -0"));

        // Esc again from Files list closes the reader.
        assert!(!reader.apply(&Key::Escape, &returned, 12));
    }

    #[test]
    fn resize_reanchors_reader_offset_to_first_visible_text() {
        let repo = TempRepo::new("resize");
        let answer = concat!(
            "alpha beta gamma delta epsilon zeta eta theta\n",
            "MARKER_SECOND_PARAGRAPH iota kappa lambda mu nu xi omicron pi\n",
            "rho sigma tau upsilon phi chi psi omega\n"
        );
        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [{ "attempt_id": "att-1", "answer": answer }]
            }]
        });

        let mut reader = Reader {
            view: View::Raw,
            ..Default::default()
        };
        let wide = reader.lines(&repo.0, &report, 80).unwrap();
        let marker_idx = wide
            .iter()
            .position(|l| l.starts_with("MARKER_SECOND_PARAGRAPH"))
            .unwrap();
        reader.offset = marker_idx;

        // Narrowing wraps the preceding paragraph into more lines; offset advances to keep MARKER at top.
        let narrow = reader.lines(&repo.0, &report, 24).unwrap();
        assert!(
            narrow[reader.offset].contains("MARKER_SECOND_PARAGRAPH"),
            "narrow offset {} landed on {:?} in {:?}",
            reader.offset,
            narrow.get(reader.offset),
            narrow
        );
        assert!(reader.offset > marker_idx);

        // Widening back restores the exact original row index.
        let wide_again = reader.lines(&repo.0, &report, 80).unwrap();
        assert_eq!(reader.offset, marker_idx);
        assert!(wide_again[reader.offset].starts_with("MARKER_SECOND_PARAGRAPH"));

        // Also verify page_with keeps the anchored text on resize without panicking or losing place.
        let step = Cell::new(0usize);
        let mut keys = vec![
            vec![Key::Char('r')],
            vec![Key::Down; marker_idx],
            vec![], // resize tick
            vec![Key::Char('q')],
        ]
        .into_iter();
        let mut out = Vec::new();
        let stop = AtomicBool::new(false);
        page_with(
            &repo.0,
            &report,
            &mut |_| {
                step.set(step.get() + 1);
                Ok(keys.next().unwrap_or_else(|| vec![Key::Char('q')]))
            },
            &mut out,
            &|| if step.get() >= 2 { (24, 8) } else { (80, 8) },
            false,
            &stop,
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();
        // The last drawn frame (after resize to width 24) starts its body on MARKER_SECOND_PARAGRAPH.
        let last_frame = rendered.rsplit("OMS Reader │ ").next().unwrap_or(&rendered);
        let body_first = last_frame.lines().nth(1).unwrap_or_default();
        assert!(
            body_first.contains("MARKER_SECOND_PARAGRAPH"),
            "expected MARKER on first body row after resize, got {body_first:?} in:\n{last_frame}"
        );
    }
}
