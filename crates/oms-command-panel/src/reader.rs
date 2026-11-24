use crate::{Error, Result, input::Key, render, rows, text};
use oms_panel_core::{Store, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const TRANSCRIPT_MAX_BYTES: usize = 8 * 1024 * 1024;
const TRANSCRIPT_VIEW_MAX_BYTES: usize = TRANSCRIPT_MAX_BYTES;
const TRANSCRIPT_EXCERPT_BYTES: usize = 8 * 1024;
const TRANSCRIPT_NOT_CAPTURED: &str = "transcript not captured";
const TRANSCRIPT_TRUNCATED_NOTE: &str =
    "[transcript truncated: retained last 8 MiB; earlier events unavailable]";
const TRANSCRIPT_BUDGET_TRUNCATED_NOTE: &str =
    "[transcript truncated: retained tail within view budget; earlier events unavailable]";
const TRANSCRIPT_BUDGET_EXHAUSTED_NOTE: &str = "[transcript omitted: view byte budget reached]";

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntryKey {
    pub task_index: usize,
    pub task_id: String,
    pub call_index: Option<usize>,
    pub call_label: String,
    pub patch: String,
    pub patch_sha256: String,
    pub path: String,
}

type FileRows = Vec<(usize, FileEntryKey)>;

enum FilesItem {
    Line(String),
    Wrapped(String),
    File {
        key: FileEntryKey,
        patch_index: usize,
        added: usize,
        removed: usize,
    },
}

struct FilesModel {
    items: Vec<FilesItem>,
    patches: Vec<Vec<u8>>,
}

enum TextItem {
    Line(String),
    Wrapped(String),
}

enum ViewCacheData {
    Files(FilesModel),
    Text(Vec<TextItem>),
}

struct ViewCache {
    repo: std::path::PathBuf,
    report: Value,
    view: View,
    data: ViewCacheData,
}

struct FilesListCache {
    width: usize,
    base_lines: Vec<String>,
    file_rows: FileRows,
}

struct OpenedDiffCache {
    key: FileEntryKey,
    safe_diff: Option<String>,
}

struct RenderedCache {
    repo: std::path::PathBuf,
    report: Value,
    view: View,
    width: usize,
    selected_file: usize,
    opened_file: Option<FileEntryKey>,
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
    pub opened_file: Option<FileEntryKey>,
    files_offset: usize,
    file_rows: FileRows,
    view_cache: Option<ViewCache>,
    files_list_cache: Option<FilesListCache>,
    opened_diff_cache: Option<OpenedDiffCache>,
    rendered: Option<RenderedCache>,
}
impl Reader {
    /// Rendered lines for the current view and width; artifact reads, patch
    /// digests and redaction rerun only when the underlying view or report changes.
    pub fn lines(&mut self, repo: &Path, report: &Value, width: usize) -> Result<Arc<Vec<String>>> {
        if let Some(cache) = &self.rendered
            && cache.view == self.view
            && cache.width == width
            && cache.selected_file == self.selected_file
            && cache.opened_file == self.opened_file
            && cache.repo == repo
            && cache.report == *report
        {
            return Ok(Arc::clone(&cache.lines));
        }
        let (rendered_lines, file_rows) = self.render_cached(repo, report, width)?;
        if let Some(prev) = &self.rendered
            && prev.view == self.view
            && prev.opened_file == self.opened_file
            && prev.repo == repo
            && prev.report == *report
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
            repo: repo.to_path_buf(),
            report: report.clone(),
            view: self.view,
            width,
            selected_file: self.selected_file,
            opened_file: self.opened_file.clone(),
            lines: Arc::clone(&lines),
        });
        Ok(lines)
    }
    pub fn render(&self, repo: &Path, report: &Value, width: usize) -> Result<Vec<String>> {
        if let Some(lines) = render_non_artifact_view(self.view, report, width)? {
            return Ok(lines);
        }
        let data = load_view_cache_data(repo, report, self.view, TRANSCRIPT_VIEW_MAX_BYTES)?;
        match &data {
            ViewCacheData::Files(model) => {
                if let Some(opened) = &self.opened_file {
                    let safe = compute_opened_file_diff(model, opened)?;
                    Ok(render_opened_file_diff(opened, safe.as_deref(), width))
                } else {
                    let (base, file_rows) = build_files_list(model, width);
                    Ok(apply_selected_marker(&base, &file_rows, self.selected_file))
                }
            }
            ViewCacheData::Text(items) => Ok(render_text_items(items, self.view, width)),
        }
    }
    fn render_cached(
        &mut self,
        repo: &Path,
        report: &Value,
        width: usize,
    ) -> Result<(Vec<String>, FileRows)> {
        if let Some(lines) = render_non_artifact_view(self.view, report, width)? {
            return Ok((lines, Vec::new()));
        }
        let cache_valid = self
            .view_cache
            .as_ref()
            .is_some_and(|c| c.view == self.view && c.repo == repo && c.report == *report);
        if !cache_valid {
            let data = load_view_cache_data(repo, report, self.view, TRANSCRIPT_VIEW_MAX_BYTES)?;
            self.view_cache = Some(ViewCache {
                repo: repo.to_path_buf(),
                report: report.clone(),
                view: self.view,
                data,
            });
            self.files_list_cache = None;
            self.opened_diff_cache = None;
        }
        let cache = self.view_cache.as_ref().expect("populated above");
        match &cache.data {
            ViewCacheData::Files(model) => {
                if let Some(opened) = self.opened_file.clone() {
                    let hit = self
                        .opened_diff_cache
                        .as_ref()
                        .is_some_and(|c| c.key == opened);
                    if !hit {
                        let safe_diff = compute_opened_file_diff(model, &opened)?;
                        self.opened_diff_cache = Some(OpenedDiffCache {
                            key: opened.clone(),
                            safe_diff,
                        });
                    }
                    let safe = self
                        .opened_diff_cache
                        .as_ref()
                        .and_then(|c| c.safe_diff.as_deref());
                    Ok((render_opened_file_diff(&opened, safe, width), Vec::new()))
                } else {
                    let list_hit = self
                        .files_list_cache
                        .as_ref()
                        .is_some_and(|c| c.width == width);
                    if !list_hit {
                        let (base_lines, file_rows) = build_files_list(model, width);
                        self.files_list_cache = Some(FilesListCache {
                            width,
                            base_lines,
                            file_rows,
                        });
                    }
                    let list = self.files_list_cache.as_ref().expect("populated above");
                    let lines = apply_selected_marker(
                        &list.base_lines,
                        &list.file_rows,
                        self.selected_file,
                    );
                    Ok((lines, list.file_rows.clone()))
                }
            }
            ViewCacheData::Text(items) => {
                Ok((render_text_items(items, self.view, width), Vec::new()))
            }
        }
    }
    fn switch_view(&mut self, view: View) {
        if self.view != view {
            self.view_cache = None;
            self.files_list_cache = None;
            self.opened_diff_cache = None;
        }
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
            } else if !self.file_rows.is_empty() {
                let count = self.file_rows.len();
                match key {
                    Key::Enter => {
                        let sel = self.selected_file.min(count - 1);
                        let key = self.file_rows[sel].1.clone();
                        self.selected_file = sel;
                        self.files_offset = self.offset;
                        self.opened_file = Some(key);
                        self.offset = 0;
                        return true;
                    }
                    Key::Up => {
                        if self.selected_file > 0 {
                            self.selected_file = self.selected_file.min(count - 1) - 1;
                            let line_idx = self.file_rows[self.selected_file].0;
                            if line_idx < self.offset {
                                self.offset = line_idx;
                            } else if line_idx >= self.offset.saturating_add(page) {
                                self.offset = line_idx.saturating_sub(page - 1);
                            }
                        } else if self.offset > 0 {
                            self.offset = self.offset.saturating_sub(1);
                        }
                        self.offset = self.offset.min(lines.len().saturating_sub(1));
                        return true;
                    }
                    Key::Down => {
                        let cur = self.selected_file.min(count - 1);
                        if cur + 1 < count {
                            self.selected_file = cur + 1;
                            let line_idx = self.file_rows[self.selected_file].0;
                            if line_idx < self.offset {
                                self.offset = line_idx;
                            } else if line_idx >= self.offset.saturating_add(page) {
                                self.offset = line_idx.saturating_sub(page - 1);
                            }
                        } else if self.offset.saturating_add(page) < lines.len() {
                            self.offset = self.offset.saturating_add(1);
                        }
                        self.offset = self.offset.min(lines.len().saturating_sub(1));
                        return true;
                    }
                    Key::PageUp
                    | Key::PageDown
                    | Key::Char(' ')
                    | Key::Home
                    | Key::End
                    | Key::Scroll(_) => {
                        let max_offset = lines.len().saturating_sub(page);
                        match key {
                            Key::PageUp => self.offset = self.offset.saturating_sub(page),
                            Key::PageDown | Key::Char(' ') => {
                                let ceiling = max_offset.max(self.offset);
                                self.offset = self
                                    .offset
                                    .saturating_add(page)
                                    .min(ceiling)
                                    .min(lines.len().saturating_sub(1));
                            }
                            Key::Home => {
                                self.offset = 0;
                                self.selected_file = 0;
                                return true;
                            }
                            Key::End => {
                                self.offset = max_offset;
                                self.selected_file = count - 1;
                                return true;
                            }
                            Key::Scroll(n) if *n < 0 => {
                                self.offset = self.offset.saturating_sub(n.unsigned_abs() as usize);
                            }
                            Key::Scroll(n) => {
                                let ceiling = max_offset.max(self.offset);
                                self.offset = self
                                    .offset
                                    .saturating_add(*n as usize)
                                    .min(ceiling)
                                    .min(lines.len().saturating_sub(1));
                            }
                            _ => unreachable!(),
                        }
                        self.sync_selected_file_to_viewport(page);
                        return true;
                    }
                    _ => {}
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
    fn sync_selected_file_to_viewport(&mut self, page: usize) {
        if self.file_rows.is_empty() {
            self.selected_file = 0;
            return;
        }
        let end = self.offset.saturating_add(page);
        let cur = self.selected_file.min(self.file_rows.len() - 1);
        let cur_line = self.file_rows[cur].0;
        if cur_line >= self.offset && cur_line < end {
            self.selected_file = cur;
            return;
        }
        if let Some(idx) = self
            .file_rows
            .iter()
            .position(|(line_idx, _)| *line_idx >= self.offset && *line_idx < end)
        {
            self.selected_file = idx;
        } else if self.offset > self.file_rows[self.file_rows.len() - 1].0 {
            self.selected_file = self.file_rows.len() - 1;
        } else if end <= self.file_rows[0].0 {
            self.selected_file = 0;
        }
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

fn call_display_label(call: &Value, index: usize) -> String {
    let attempt = call
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let participant = call
        .get("participant")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let call_id = call
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (participant, attempt) {
        (Some(p), Some(a)) if p != a => format!("{p} ({a})"),
        (_, Some(a)) => a.to_owned(),
        (Some(p), None) => p.to_owned(),
        (None, None) => call_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("call-{}", index + 1)),
    }
}

fn native_display_label(native_assignment: &Value) -> String {
    native_assignment
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            native_assignment
                .get("participant")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("assignment")
        .to_owned()
}

fn render_non_artifact_view(
    view: View,
    report: &Value,
    width: usize,
) -> Result<Option<Vec<String>>> {
    if report["kind"] == "oms-panel-routes" {
        let routes = &report["routes"];
        if matches!(view, View::Summary | View::Info) {
            return Ok(Some(
                render::routes(routes, width)
                    .lines()
                    .map(str::to_owned)
                    .collect(),
            ));
        }
        let bytes = serde_json::to_vec(routes).map_err(Error::new)?;
        return Ok(Some(render::wrap(&oms_panel_core::redact(&bytes)?, width)));
    }
    if report["kind"] == "oms-panel-teams" {
        if matches!(view, View::Summary | View::Info) {
            return Ok(Some(
                render::teams(report, width)
                    .lines()
                    .map(str::to_owned)
                    .collect(),
            ));
        }
        let bytes = serde_json::to_vec(report).map_err(Error::new)?;
        return Ok(Some(render::wrap(&oms_panel_core::redact(&bytes)?, width)));
    }
    if view == View::Summary {
        return Ok(Some(
            render::results(report, width)
                .lines()
                .map(str::to_owned)
                .collect(),
        ));
    }
    if matches!(view, View::Events | View::Info) {
        let mut out = Vec::new();
        for task in rows(&report["rows"]) {
            out.push(format!("Task {} · {:?}", text(task, "task_id"), view));
            for call in rows(&task["calls"]) {
                match view {
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
                    _ => {}
                }
            }
            let native = &task["native_assignment"]["report"];
            if native["status"] == "reported" {
                out.extend(render::wrap(
                    &format!(
                        "Native {} report · owner imported · acceptance ineligible",
                        text(native, "result_kind")
                    ),
                    width,
                ));
                out.extend(render::value_lines(native, width));
            } else if native["status"] == "unavailable" {
                out.extend(render::wrap(
                    &format!("Native report unavailable: {}", text(native, "issue")),
                    width,
                ));
            }
        }
        if out.is_empty() {
            out.push("No recorded evidence for this view".into());
        }
        return Ok(Some(out));
    }
    Ok(None)
}

fn load_view_cache_data(
    repo: &Path,
    report: &Value,
    view: View,
    transcript_budget: usize,
) -> Result<ViewCacheData> {
    match view {
        View::Files => Ok(ViewCacheData::Files(load_files_model(repo, report)?)),
        View::Transcript => Ok(ViewCacheData::Text(load_transcript_items(
            repo,
            report,
            transcript_budget,
        )?)),
        View::Diff | View::Raw => Ok(ViewCacheData::Text(load_artifact_text_items(
            repo, report, view,
        )?)),
        View::Summary | View::Events | View::Info => Ok(ViewCacheData::Text(Vec::new())),
    }
}

fn load_files_model(repo: &Path, report: &Value) -> Result<FilesModel> {
    let store = Store::open(repo)?;
    let mut items = Vec::new();
    let mut patches = Vec::new();
    for (task_index, task) in rows(&report["rows"]).iter().enumerate() {
        let task_id = text(task, "task_id");
        items.push(FilesItem::Line(format!(
            "Task {task_id} · {:?}",
            View::Files
        )));
        for (call_index, call) in rows(&task["calls"]).iter().enumerate() {
            if call["patch"].is_string() {
                let patch = oms_panel_core::results::verified_patch(&store, call)?;
                let changes = oms_panel_core::files::patch_changes(&patch)?;
                let patch_index = patches.len();
                patches.push(patch);
                let label = call_display_label(call, call_index);
                if !changes.is_empty() {
                    items.push(FilesItem::Line(format!("Call {label}")));
                }
                for change in changes {
                    items.push(FilesItem::File {
                        key: FileEntryKey {
                            task_index,
                            task_id: task_id.to_owned(),
                            call_index: Some(call_index),
                            call_label: label.clone(),
                            patch: text(call, "patch").to_owned(),
                            patch_sha256: text(call, "patch_sha256").to_owned(),
                            path: change.path,
                        },
                        patch_index,
                        added: change.added,
                        removed: change.removed,
                    });
                }
            }
        }
        let native = &task["native_assignment"]["report"];
        if native["status"] == "reported" {
            items.push(FilesItem::Wrapped(format!(
                "Native {} report · owner imported · acceptance ineligible",
                text(native, "result_kind")
            )));
            if native["patch"].is_object() {
                let row = serde_json::json!({
                    "patch": native["patch"]["path"],
                    "patch_sha256": native["patch"]["sha256"],
                });
                let patch = oms_panel_core::results::verified_patch(&store, &row)?;
                let changes = oms_panel_core::files::patch_changes(&patch)?;
                let patch_index = patches.len();
                patches.push(patch);
                let label = native_display_label(&task["native_assignment"]);
                for change in changes {
                    items.push(FilesItem::File {
                        key: FileEntryKey {
                            task_index,
                            task_id: task_id.to_owned(),
                            call_index: None,
                            call_label: label.clone(),
                            patch: text(&native["patch"], "path").to_owned(),
                            patch_sha256: text(&native["patch"], "sha256").to_owned(),
                            path: change.path,
                        },
                        patch_index,
                        added: change.added,
                        removed: change.removed,
                    });
                }
            }
        } else if native["status"] == "unavailable" {
            items.push(FilesItem::Wrapped(format!(
                "Native report unavailable: {}",
                text(native, "issue")
            )));
        }
    }
    Ok(FilesModel { items, patches })
}

fn build_files_list(model: &FilesModel, width: usize) -> (Vec<String>, FileRows) {
    let mut out = Vec::new();
    let mut file_rows = Vec::new();
    for item in &model.items {
        match item {
            FilesItem::Line(line) => out.push(line.clone()),
            FilesItem::Wrapped(text) => out.extend(render::wrap(text, width)),
            FilesItem::File {
                key,
                added,
                removed,
                ..
            } => {
                file_rows.push((out.len(), key.clone()));
                out.push(format!("  {} +{added} -{removed}", key.path));
            }
        }
    }
    if out.is_empty() {
        out.push("No recorded evidence for this view".into());
    }
    (out, file_rows)
}

fn apply_selected_marker(
    base_lines: &[String],
    file_rows: &FileRows,
    selected_file: usize,
) -> Vec<String> {
    let mut out = base_lines.to_vec();
    if !file_rows.is_empty() {
        let sel = selected_file.min(file_rows.len() - 1);
        let line_idx = file_rows[sel].0;
        if let Some(rest) = out[line_idx].strip_prefix("  ") {
            out[line_idx] = format!("▸ {rest}");
        }
    }
    out
}

fn compute_opened_file_diff(model: &FilesModel, opened: &FileEntryKey) -> Result<Option<String>> {
    for item in &model.items {
        if let FilesItem::File {
            key, patch_index, ..
        } = item
            && key == opened
        {
            let slice =
                oms_panel_core::files::file_diff(&model.patches[*patch_index], &opened.path)?;
            if slice.is_empty() {
                return Ok(None);
            }
            return Ok(Some(oms_panel_core::redact(&slice)?));
        }
    }
    Ok(None)
}

fn render_opened_file_diff(
    opened: &FileEntryKey,
    safe_diff: Option<&str>,
    width: usize,
) -> Vec<String> {
    let header = if opened.task_id.is_empty() {
        format!(
            "File diff: {} · {} (Esc returns to Files)",
            opened.path, opened.call_label
        )
    } else {
        format!(
            "File diff: {} · Task {} · {} (Esc returns to Files)",
            opened.path, opened.task_id, opened.call_label
        )
    };
    let mut out = render::wrap(&header, width);
    match safe_diff {
        Some(safe) if !safe.is_empty() => out.extend(render::wrap(safe, width)),
        _ => out.push("No diff recorded for this file".into()),
    }
    out
}

fn load_artifact_text_items(repo: &Path, report: &Value, view: View) -> Result<Vec<TextItem>> {
    let store = Store::open(repo)?;
    let mut items = Vec::new();
    for task in rows(&report["rows"]) {
        items.push(TextItem::Line(format!(
            "Task {} · {:?}",
            text(task, "task_id"),
            view
        )));
        for call in rows(&task["calls"]) {
            match view {
                View::Diff => {
                    if call["patch"].is_string() {
                        let patch = oms_panel_core::results::verified_patch(&store, call)?;
                        items.push(TextItem::Wrapped(oms_panel_core::redact(&patch)?));
                    }
                }
                View::Raw => {
                    if call["artifact"].is_string() {
                        let raw = oms_panel_core::results::indexed_artifact(&store, call)?;
                        items.push(TextItem::Wrapped(oms_panel_core::redact(&raw)?));
                    } else {
                        let excerpt = call["answer"]
                            .as_str()
                            .or_else(|| call["answer_excerpt"].as_str())
                            .unwrap_or_default();
                        items.push(TextItem::Wrapped(oms_panel_core::redact(
                            excerpt.as_bytes(),
                        )?));
                        if call["answer_truncated"] == true {
                            items.push(TextItem::Line("... (answer truncated)".into()));
                        }
                    }
                }
                _ => {}
            }
        }
        let native = &task["native_assignment"]["report"];
        if native["status"] == "reported" {
            items.push(TextItem::Wrapped(format!(
                "Native {} report · owner imported · acceptance ineligible",
                text(native, "result_kind")
            )));
            match view {
                View::Diff => {
                    if native["patch"].is_object() {
                        let row = serde_json::json!({
                            "patch": native["patch"]["path"],
                            "patch_sha256": native["patch"]["sha256"],
                        });
                        let patch = oms_panel_core::results::verified_patch(&store, &row)?;
                        items.push(TextItem::Wrapped(oms_panel_core::redact(&patch)?));
                    }
                }
                View::Raw => {
                    for (i, reference) in std::iter::once(&native["source"])
                        .chain(rows(&native["verification"]).iter())
                        .enumerate()
                    {
                        if i > 0 {
                            items.push(TextItem::Line(format!(
                                "Reported verification evidence {i} · no certified verdict"
                            )));
                        }
                        let row = serde_json::json!({
                            "artifact": reference["path"],
                            "artifact_sha256": reference["sha256"],
                        });
                        let raw = oms_panel_core::results::indexed_artifact(&store, &row)?;
                        items.push(TextItem::Wrapped(oms_panel_core::redact(&raw)?));
                    }
                }
                _ => {}
            }
        } else if native["status"] == "unavailable" {
            items.push(TextItem::Wrapped(format!(
                "Native report unavailable: {}",
                text(native, "issue")
            )));
        }
    }
    Ok(items)
}

fn load_transcript_items(
    repo: &Path,
    report: &Value,
    max_total_bytes: usize,
) -> Result<Vec<TextItem>> {
    let mut items = Vec::new();
    let mut remaining = max_total_bytes;
    for task in rows(&report["rows"]) {
        let task_id = text(task, "task_id");
        items.push(TextItem::Line(format!(
            "Task {task_id} · {:?}",
            View::Transcript
        )));
        let calls = rows(&task["calls"]);
        for (idx, call) in calls.iter().enumerate() {
            let label = call_display_label(call, idx);
            append_transcript_for_target(repo, call, "Call", &label, &mut remaining, &mut items)?;
        }
        let native = &task["native_assignment"]["report"];
        if native["status"] == "reported" {
            items.push(TextItem::Wrapped(format!(
                "Native {} report · owner imported · acceptance ineligible",
                text(native, "result_kind")
            )));
            let label = native_display_label(&task["native_assignment"]);
            append_transcript_for_target(
                repo,
                &task["native_assignment"],
                "Native",
                &label,
                &mut remaining,
                &mut items,
            )?;
        } else if native["status"] == "unavailable" {
            items.push(TextItem::Wrapped(format!(
                "Native report unavailable: {}",
                text(native, "issue")
            )));
        } else if calls.is_empty() {
            items.push(TextItem::Wrapped(if task_id.is_empty() {
                TRANSCRIPT_NOT_CAPTURED.into()
            } else {
                format!("Task {task_id}: {TRANSCRIPT_NOT_CAPTURED}")
            }));
        }
    }
    Ok(items)
}

fn append_transcript_for_target(
    repo: &Path,
    target: &Value,
    prefix: &str,
    label: &str,
    remaining_budget: &mut usize,
    items: &mut Vec<TextItem>,
) -> Result<()> {
    if *remaining_budget == 0 {
        items.push(TextItem::Wrapped(format!(
            "{prefix} {label}: {TRANSCRIPT_BUDGET_EXHAUSTED_NOTE}"
        )));
        return Ok(());
    }
    for candidate in transcript_candidates(target, prefix == "Call") {
        if let Some((source, read)) = read_dispatch_transcript(repo, &candidate, *remaining_budget)
        {
            *remaining_budget = remaining_budget.saturating_sub(read.bytes_read);
            let (formatted, heading, empty_note) = match source {
                TranscriptSource::DispatchLog => (
                    format_transcript_bytes(&read.bytes, read.truncated, read.budget_limited)?,
                    format!("{prefix} {label}"),
                    "No transcript events recorded",
                ),
                TranscriptSource::DispatchReport => (
                    format_report_bytes(&read.bytes, read.truncated, read.budget_limited),
                    format!("{prefix} {label} · dispatch report"),
                    "No dispatch report content recorded",
                ),
            };
            let safe = oms_panel_core::redact(formatted.as_bytes())?;
            items.push(TextItem::Line(heading));
            if safe.trim().is_empty() {
                items.push(TextItem::Line(empty_note.into()));
            } else {
                items.push(TextItem::Wrapped(safe));
            }
            return Ok(());
        }
    }
    items.push(TextItem::Wrapped(format!(
        "{prefix} {label}: {TRANSCRIPT_NOT_CAPTURED}"
    )));
    Ok(())
}

fn render_text_items(items: &[TextItem], view: View, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            TextItem::Line(line) => out.push(line.clone()),
            TextItem::Wrapped(text) => out.extend(render::wrap(text, width)),
        }
    }
    if out.is_empty() {
        out.push(
            if view == View::Transcript {
                TRANSCRIPT_NOT_CAPTURED
            } else {
                "No recorded evidence for this view"
            }
            .into(),
        );
    }
    out
}

fn body_rows(height: usize) -> usize {
    height
        .saturating_sub(if height >= 5 { 3 } else { 2 })
        .max(1)
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

const DISPATCH_REPORT_SUBDIRS: [&str; 6] =
    ["delegate", "ask", "advise", "consult", "review", "call"];

#[derive(Debug, PartialEq, Eq)]
enum TranscriptCandidate {
    DispatchLog(String),
    DispatchReport {
        subdir: &'static str,
        filename: String,
        expected_sha256: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptSource {
    DispatchLog,
    DispatchReport,
}

struct ReadTranscript {
    bytes: Vec<u8>,
    bytes_read: usize,
    truncated: bool,
    budget_limited: bool,
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_report_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name.ends_with(".md")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn dispatch_report_candidate(call: &Value) -> Option<TranscriptCandidate> {
    let rel = call.get("artifact").and_then(Value::as_str)?;
    let rest = rel.strip_prefix(".oms/artifacts/")?;
    let (subdir_raw, filename) = rest.split_once('/')?;
    if !is_report_filename(filename) {
        return None;
    }
    let &subdir = DISPATCH_REPORT_SUBDIRS
        .iter()
        .find(|&&allowed| allowed == subdir_raw)?;
    let expected_sha256 = match call.get("artifact_sha256") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_sha256_hex(s) => Some(s.to_owned()),
        _ => return None,
    };
    Some(TranscriptCandidate::DispatchReport {
        subdir,
        filename: filename.to_owned(),
        expected_sha256,
    })
}

fn transcript_candidates(call: &Value, allow_report_fallback: bool) -> Vec<TranscriptCandidate> {
    let attempt_raws = [
        call.get("attempt_id").and_then(Value::as_str),
        call.get("call_id").and_then(Value::as_str),
    ];
    let has_attempt_id = attempt_raws
        .into_iter()
        .flatten()
        .any(|raw| !raw.is_empty());
    let candidate_raws: Vec<&str> = if has_attempt_id {
        attempt_raws.into_iter().flatten().collect()
    } else {
        [
            call.get("participant").and_then(Value::as_str),
            call.get("refs")
                .and_then(|r| r.get("panel_room_participant"))
                .and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        .collect()
    };
    let mut ids = Vec::new();
    for raw in candidate_raws {
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
    let mut candidates: Vec<TranscriptCandidate> = filenames
        .into_iter()
        .map(TranscriptCandidate::DispatchLog)
        .collect();
    if allow_report_fallback && let Some(report) = dispatch_report_candidate(call) {
        candidates.push(report);
    }
    candidates
}

/// Read an owner-scoped, repository-confined file without following symlinks,
/// accepting hardlinks, leaving the repository, or reading provider homes.
fn read_confined_file(
    repo: &Path,
    dirs: &[&str],
    filename: &str,
    max_bytes: usize,
) -> Option<ReadTranscript> {
    use rustix::fs::{self, FileType, Mode, OFlags};

    let cap = max_bytes.min(TRANSCRIPT_MAX_BYTES);
    if cap == 0 || filename.is_empty() || filename.contains('/') || filename.contains('\0') {
        return None;
    }
    let euid = rustix::process::geteuid().as_raw();
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut cur_fd = fs::open(
        repo,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    for dir in dirs {
        cur_fd = fs::openat(&cur_fd, *dir, dir_flags, Mode::empty()).ok()?;
    }
    let dir_stat = fs::fstat(&cur_fd).ok()?;
    if FileType::from_raw_mode(dir_stat.st_mode) != FileType::Directory
        || dir_stat.st_uid != euid
        || (dir_stat.st_mode & 0o022) != 0
    {
        return None;
    }
    let fd = fs::openat(
        &cur_fd,
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
    let (bytes_read, truncated, budget_limited) = if size > cap as u64 {
        let start = size - cap as u64;
        file.seek(SeekFrom::Start(start)).ok()?;
        (&mut file).take(cap as u64).read_to_end(&mut bytes).ok()?;
        let read_len = bytes.len();
        if let Some(pos) = bytes.iter().position(|&b| b == b'\n') {
            bytes.drain(..=pos);
        } else {
            bytes.clear();
        }
        (read_len, true, cap < TRANSCRIPT_MAX_BYTES)
    } else {
        (&mut file)
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > cap {
            return None;
        }
        let read_len = bytes.len();
        (read_len, false, false)
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
    Some(ReadTranscript {
        bytes,
        bytes_read,
        truncated,
        budget_limited,
    })
}

/// Read an owner-scoped dispatch transcript from `.oms/panel-state/dispatch-logs/`
/// or a per-attempt dispatch report under `.oms/artifacts/` without following
/// symlinks, leaving the repository, or reading provider homes.
fn read_dispatch_transcript(
    repo: &Path,
    candidate: &TranscriptCandidate,
    max_bytes: usize,
) -> Option<(TranscriptSource, ReadTranscript)> {
    match candidate {
        TranscriptCandidate::DispatchLog(filename) => {
            if !filename.ends_with(".transcript") {
                return None;
            }
            let read = read_confined_file(
                repo,
                &[".oms", "panel-state", "dispatch-logs"],
                filename,
                max_bytes,
            )?;
            Some((TranscriptSource::DispatchLog, read))
        }
        TranscriptCandidate::DispatchReport {
            subdir,
            filename,
            expected_sha256,
        } => {
            if !is_report_filename(filename) {
                return None;
            }
            let read =
                read_confined_file(repo, &[".oms", "artifacts", subdir], filename, max_bytes)?;
            if let Some(expected) = expected_sha256
                && !read.truncated
                && oms_panel_core::digest(&read.bytes) != *expected
            {
                return None;
            }
            Some((TranscriptSource::DispatchReport, read))
        }
    }
}

fn format_report_bytes(bytes: &[u8], truncated: bool, budget_limited: bool) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = Vec::new();
    if truncated {
        lines.push(
            if budget_limited {
                TRANSCRIPT_BUDGET_TRUNCATED_NOTE
            } else {
                TRANSCRIPT_TRUNCATED_NOTE
            }
            .into(),
        );
    }
    let mut kept: Vec<&str> = raw.lines().map(|l| l.trim_end_matches('\r')).collect();
    // A tail cut can drop a PEM block's BEGIN line, and redaction only matches whole
    // blocks, so drop everything through an END line that has no BEGIN before it.
    if truncated
        && let Some(end) = kept.iter().position(|l| l.contains("-----END "))
        && !kept[..end].iter().any(|l| l.contains("-----BEGIN "))
    {
        kept.drain(..=end);
    }
    lines.extend(kept.into_iter().map(str::to_owned));
    lines.join("\n")
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

/// Redact before truncating so a secret straddling the 8 KiB excerpt boundary
/// never leaks an unredacted prefix.
fn transcript_excerpt(value: &Value) -> Result<String> {
    let text = transcript_value_text(value);
    if text.len() <= TRANSCRIPT_EXCERPT_BYTES {
        return Ok(text);
    }
    let safe = oms_panel_core::redact(text.as_bytes())?;
    if safe.len() <= TRANSCRIPT_EXCERPT_BYTES {
        return Ok(safe);
    }
    let mut end = TRANSCRIPT_EXCERPT_BYTES;
    while end > 0 && !safe.is_char_boundary(end) {
        end -= 1;
    }
    Ok(format!("{}\n[output excerpt truncated]", &safe[..end]))
}

fn format_transcript_bytes(bytes: &[u8], truncated: bool, budget_limited: bool) -> Result<String> {
    let raw = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = Vec::new();
    let mut tools: HashMap<String, usize> = HashMap::new();
    let mut saw_truncation = false;
    if truncated {
        lines.push(
            if budget_limited {
                TRANSCRIPT_BUDGET_TRUNCATED_NOTE
            } else {
                TRANSCRIPT_TRUNCATED_NOTE
            }
            .into(),
        );
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
                        let excerpt = transcript_excerpt(output)?;
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
                        let excerpt = transcript_excerpt(res)?;
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
                                let excerpt = transcript_excerpt(c)?;
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
    Ok(lines.join("\n"))
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
        Option<FileEntryKey>,
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
                (View::Files, Some(opened)) => {
                    if opened.task_id.is_empty() {
                        format!("Files · {} ({})", opened.path, opened.call_label)
                    } else {
                        format!(
                            "Files · {} ({}/{})",
                            opened.path, opened.task_id, opened.call_label
                        )
                    }
                }
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
            missing_lines
                .iter()
                .any(|l| l.contains("att-1") && l.contains(TRANSCRIPT_NOT_CAPTURED)),
            "expected named not captured: {missing_lines:?}"
        );
        assert!(
            !missing_lines
                .join("\n")
                .contains("indexed-raw-artifact-body")
        );

        // With a private dispatch log containing JSONL events + a secret-shaped token + >8KiB output.
        let secret_like = [
            ["sk", "-", "live", "_"].concat(),
            ["1234567890", "abcdefghij", "klmnop"].concat(),
        ]
        .concat();
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
        let transcript_file = logs_dir.join("call-att-1.transcript");
        std::fs::write(&transcript_file, jsonl.as_bytes()).unwrap();
        std::fs::set_permissions(&transcript_file, std::fs::Permissions::from_mode(0o600)).unwrap();

        let captured = reader.render(&repo.0, &report_with_capture, 120).unwrap();
        let joined = captured.join("\n");
        assert!(joined.contains("Checked parser state"), "{joined}");
        assert!(joined.contains("Command: cargo test / exit 0"), "{joined}");
        assert!(joined.contains("[output excerpt truncated]"), "{joined}");
        assert!(joined.contains("Final report:"), "{joined}");
        assert!(!joined.contains(&secret_like), "secret leaked: {joined}");
        assert!(!joined.contains("indexed-raw-artifact-body"), "{joined}");

        // Symlinked transcript is refused and reports not captured.
        let outside = repo.write_file("outside.txt", b"should-not-read\n");
        std::os::unix::fs::symlink(&outside, logs_dir.join("call-att-2.transcript")).unwrap();
        let sym_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T2",
                "calls": [{ "participant": "call-sym", "attempt_id": "att-2" }]
            }]
        });
        let sym_lines = reader.render(&repo.0, &sym_report, 100).unwrap();
        assert!(
            sym_lines
                .iter()
                .any(|l| l.contains("att-2") && l.contains(TRANSCRIPT_NOT_CAPTURED))
        );
        assert!(!sym_lines.join("\n").contains("should-not-read"));

        // Hardlinked transcript is refused.
        let hard_src = repo.write_file("hard.txt", b"hardlink-body\n");
        std::fs::hard_link(&hard_src, logs_dir.join("call-att-3.transcript")).unwrap();
        let hard_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T3",
                "calls": [{ "participant": "call-hard", "attempt_id": "att-3" }]
            }]
        });
        let hard_lines = reader.render(&repo.0, &hard_report, 100).unwrap();
        assert!(
            hard_lines
                .iter()
                .any(|l| l.contains("att-3") && l.contains(TRANSCRIPT_NOT_CAPTURED))
        );
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
        assert!(
            native_tx
                .iter()
                .any(|l| l.contains("att-native") && l.contains(TRANSCRIPT_NOT_CAPTURED))
        );
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
        assert_eq!(
            reader.opened_file.as_ref().map(|k| k.path.as_str()),
            Some("src/beta.rs")
        );
        assert_eq!(
            reader.opened_file.as_ref().map(|k| k.call_label.as_str()),
            Some("att-1")
        );
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

    #[test]
    fn files_view_isolates_duplicate_path_diffs_across_tasks_retries_and_native() {
        let repo = TempRepo::new("files-duplicate-path");
        let make_patch = |path: &str, added: &str| -> Vec<u8> {
            format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1,2 @@\n base\n+{added}\n"
            )
            .into_bytes()
        };
        let mut p1 = make_patch("src/shared.rs", "t1_att1_only");
        p1.extend_from_slice(&make_patch("src/other.rs", "t1_other_only"));
        let p2 = make_patch("src/shared.rs", "t1_att2_retry_only");
        let p3 = make_patch("src/shared.rs", "t2_att3_only");
        let p4 = make_patch("src/shared.rs", "t2_native_only");

        let rel1 = ".oms/artifacts/t1-att1.patch";
        let rel2 = ".oms/artifacts/t1-att2.patch";
        let rel3 = ".oms/artifacts/t2-att3.patch";
        let rel4 = ".oms/artifacts/t2-native.patch";
        let path1 = repo.write_file(rel1, &p1);
        let path2 = repo.write_file(rel2, &p2);
        let path3 = repo.write_file(rel3, &p3);
        let path4 = repo.write_file(rel4, &p4);

        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [
                {
                    "task_id": "T1",
                    "calls": [
                        {
                            "attempt_id": "att-1",
                            "patch": rel1,
                            "patch_sha256": oms_panel_core::digest(&p1),
                        },
                        {
                            "attempt_id": "att-2",
                            "patch": rel2,
                            "patch_sha256": oms_panel_core::digest(&p2),
                        }
                    ]
                },
                {
                    "task_id": "T2",
                    "calls": [
                        {
                            "attempt_id": "att-3",
                            "patch": rel3,
                            "patch_sha256": oms_panel_core::digest(&p3),
                        }
                    ],
                    "native_assignment": {
                        "attempt_id": "att-native",
                        "report": {
                            "status": "reported",
                            "result_kind": "candidate",
                            "patch": {
                                "path": rel4,
                                "sha256": oms_panel_core::digest(&p4),
                            },
                            "source": { "path": rel4, "sha256": oms_panel_core::digest(&p4) },
                            "verification": []
                        }
                    }
                }
            ]
        });

        let mut reader = Reader {
            view: View::Files,
            ..Default::default()
        };
        let list = reader.lines(&repo.0, &report, 100).unwrap();

        // Remove all patch files from disk after the initial Files load: Up/Down,
        // Enter, Esc and width changes must reuse cached patches without rereading.
        for p in [&path1, &path2, &path3, &path4] {
            std::fs::remove_file(p).unwrap();
        }

        // Row indices in file_rows:
        // 0: T1 att-1 src/shared.rs
        // 1: T1 att-1 src/other.rs
        // 2: T1 att-2 src/shared.rs
        // 3: T2 att-3 src/shared.rs
        // 4: T2 native src/shared.rs
        let expected_markers = [
            (0usize, "+t1_att1_only"),
            (2usize, "+t1_att2_retry_only"),
            (3usize, "+t2_att3_only"),
            (4usize, "+t2_native_only"),
        ];
        let mut cur_lines = list;
        let mut cur_idx = 0usize;
        for (target_idx, expected) in expected_markers {
            while cur_idx < target_idx {
                assert!(reader.apply(&Key::Down, &cur_lines, 20));
                cur_idx += 1;
                cur_lines = reader.lines(&repo.0, &report, 100).unwrap();
            }
            assert!(reader.apply(&Key::Enter, &cur_lines, 20));
            let diff_lines = reader.lines(&repo.0, &report, 100).unwrap();
            let diff_text = diff_lines.join("\n");
            assert!(
                diff_text.contains(expected),
                "row {target_idx} missing {expected}:\n{diff_text}"
            );
            for (_, other) in expected_markers {
                if other != expected {
                    assert!(
                        !diff_text.contains(other),
                        "row {target_idx} leaked {other}:\n{diff_text}"
                    );
                }
            }
            // Resize while diff is open must also succeed without rereading disk.
            let narrow = reader.lines(&repo.0, &report, 40).unwrap();
            assert!(narrow.join("\n").contains(expected));
            assert!(reader.apply(&Key::Escape, &narrow, 20));
            cur_lines = reader.lines(&repo.0, &report, 100).unwrap();
        }
    }

    #[test]
    fn transcript_excerpt_redacts_secret_straddling_8k_boundary() {
        let repo = TempRepo::new("transcript-boundary-secret");
        let prefix = ["sk", "-", "live", "_"].concat();
        let tail = ["1234567890", "abcdefghij", "klmnop"].concat();
        let sample = format!("{prefix}{tail}");
        let line_chunk = format!("{}\n", "a".repeat(63)); // 64 bytes per line
        let prefix_lines = line_chunk.repeat((TRANSCRIPT_EXCERPT_BYTES - 64) / 64);
        let pad_in_line = "b".repeat(TRANSCRIPT_EXCERPT_BYTES - prefix_lines.len() - 12 - 1);
        let payload = format!(
            "{prefix_lines}{pad_in_line} {sample} tail\n{}",
            "z".repeat(256)
        );
        let line = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "command": "echo check",
                "exit_code": 0,
                "aggregated_output": payload,
            }
        });
        let logs_dir = repo.0.join(".oms/panel-state/dispatch-logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::set_permissions(&logs_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tx = logs_dir.join("att-boundary.transcript");
        std::fs::write(&tx, format!("{line}\n").as_bytes()).unwrap();
        std::fs::set_permissions(&tx, std::fs::Permissions::from_mode(0o600)).unwrap();

        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [{ "attempt_id": "att-boundary", "participant": "call-boundary" }]
            }]
        });
        let reader = Reader {
            view: View::Transcript,
            ..Default::default()
        };
        let rendered = reader.render(&repo.0, &report, 120).unwrap().join("\n");
        assert!(
            rendered.contains("[output excerpt truncated]"),
            "{rendered}"
        );
        assert!(!rendered.contains(&sample), "full sample leaked");
        assert!(
            !rendered.contains(&sample[..12]),
            "sample prefix leaked across 8 KiB cut"
        );
    }

    #[test]
    fn files_view_page_keys_and_wheel_reach_trailing_lines_after_last_file_row() {
        let repo = TempRepo::new("files-trailing");
        let patch = concat!(
            "diff --git a/src/only.rs b/src/only.rs\n",
            "--- a/src/only.rs\n",
            "+++ b/src/only.rs\n",
            "@@ -1 +1,2 @@\n",
            " old\n",
            "+new\n",
        )
        .as_bytes();
        let patch_rel = ".oms/artifacts/only.patch";
        repo.write_file(patch_rel, patch);
        let trailing_issue = format!(
            "{} TRAILING_PREVIEW_END",
            (0..20)
                .map(|i| format!("segment_{i}_word"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [
                {
                    "task_id": "T1",
                    "calls": [{
                        "attempt_id": "att-1",
                        "patch": patch_rel,
                        "patch_sha256": oms_panel_core::digest(patch),
                    }]
                },
                {
                    "task_id": "T2",
                    "calls": [],
                    "native_assignment": {
                        "report": {
                            "status": "unavailable",
                            "issue": trailing_issue,
                        }
                    }
                }
            ]
        });

        let mut reader = Reader {
            view: View::Files,
            ..Default::default()
        };
        let lines = reader.lines(&repo.0, &report, 30).unwrap();
        let height = 8; // body_rows = 5
        let page = body_rows(height);
        assert!(
            lines.len() > page + 4,
            "need enough trailing lines to test paging: {lines:?}"
        );

        let visible_has_end = |r: &Reader| -> bool {
            lines
                .iter()
                .skip(r.offset)
                .take(page)
                .any(|l| l.contains("TRAILING_PREVIEW_END"))
        };
        assert!(!visible_has_end(&reader));

        // PageDown must reach every retained line including the final trailing line.
        for _ in 0..10 {
            if visible_has_end(&reader) {
                break;
            }
            assert!(reader.apply(&Key::PageDown, &lines, height));
        }
        assert!(
            visible_has_end(&reader),
            "PageDown never reached TRAILING_PREVIEW_END; offset={} of {} lines",
            reader.offset,
            lines.len()
        );

        // Home returns to top; mouse wheel Scroll(+n) must also reach the end.
        assert!(reader.apply(&Key::Home, &lines, height));
        assert_eq!(reader.offset, 0);
        for _ in 0..20 {
            if visible_has_end(&reader) {
                break;
            }
            assert!(reader.apply(&Key::Scroll(3), &lines, height));
        }
        assert!(
            visible_has_end(&reader),
            "Scroll never reached TRAILING_PREVIEW_END; offset={} of {} lines",
            reader.offset,
            lines.len()
        );

        // Home + End jumps straight to the last page containing TRAILING_PREVIEW_END.
        assert!(reader.apply(&Key::Home, &lines, height));
        assert!(reader.apply(&Key::End, &lines, height));
        assert!(visible_has_end(&reader));
    }

    #[test]
    fn transcript_prefers_attempt_id_for_retries_names_missing_calls_and_reuses_cache_on_resize() {
        let repo = TempRepo::new("transcript-retries");
        let logs_dir = repo.0.join(".oms/panel-state/dispatch-logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::set_permissions(&logs_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let write_tx = |name: &str, msg: &str| {
            let file = logs_dir.join(name);
            let body = format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"{msg}\"}}}}\n"
            );
            std::fs::write(&file, body.as_bytes()).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            file
        };
        let shared = write_tx("call-worker.transcript", "SHARED_PARTICIPANT_FALLBACK");
        let legacy = write_tx("call-legacy.transcript", "PARTICIPANT_ONLY_LEGACY");
        let att1 = write_tx("att-1.transcript", "ATTEMPT_ONE_UNIQUE_EVENT");
        let att2 = write_tx("att-2.transcript", "ATTEMPT_TWO_UNIQUE_EVENT");

        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [
                    { "participant": "call-worker", "attempt_id": "att-1" },
                    { "participant": "call-worker", "attempt_id": "att-2" },
                    { "participant": "call-worker", "attempt_id": "att-missing-retry" },
                    { "participant": "call-legacy" },
                    { "participant": "call-missing", "attempt_id": "att-3" },
                ]
            }]
        });

        let mut reader = Reader {
            view: View::Transcript,
            ..Default::default()
        };
        let wide = reader.lines(&repo.0, &report, 100).unwrap();
        let text = wide.join("\n");
        assert!(text.contains("ATTEMPT_ONE_UNIQUE_EVENT"), "{text}");
        assert!(text.contains("ATTEMPT_TWO_UNIQUE_EVENT"), "{text}");
        assert!(text.contains("PARTICIPANT_ONLY_LEGACY"), "{text}");
        assert!(
            !text.contains("SHARED_PARTICIPANT_FALLBACK"),
            "attempt_id must not fall back to shared participant: {text}"
        );
        assert!(
            wide.iter()
                .any(|l| l.contains("att-missing-retry") && l.contains(TRANSCRIPT_NOT_CAPTURED)),
            "missing retry attempt must report not captured instead of shared participant: {wide:?}"
        );
        assert!(
            wide.iter()
                .any(|l| l.contains("att-3") && l.contains(TRANSCRIPT_NOT_CAPTURED)),
            "missing transcript line must name its call: {wide:?}"
        );

        // Delete transcript files from disk; width changes must reuse cached data.
        for f in [&shared, &legacy, &att1, &att2] {
            std::fs::remove_file(f).unwrap();
        }
        let narrow = reader.lines(&repo.0, &report, 28).unwrap().join("\n");
        assert!(narrow.contains("ATTEMPT_ONE_UNIQUE_EVENT"), "{narrow}");
        assert!(narrow.contains("ATTEMPT_TWO_UNIQUE_EVENT"), "{narrow}");
    }

    #[test]
    fn transcript_view_bounds_total_bytes_read_across_calls() {
        let repo = TempRepo::new("transcript-budget");
        let logs_dir = repo.0.join(".oms/panel-state/dispatch-logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::set_permissions(&logs_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let make_line = |tag: &str| {
            format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"{tag}_{}\"}}}}\n",
                "x".repeat(60)
            )
        };
        let body1 = format!("{}{}", make_line("CALL1_A"), make_line("CALL1_B"));
        let body2 = format!("{}{}", make_line("CALL2_HEAD"), make_line("CALL2_TAIL"));
        let body3 = make_line("CALL3_NEVER_READ");

        for (name, body) in [
            ("att-1.transcript", &body1),
            ("att-2.transcript", &body2),
            ("att-3.transcript", &body3),
        ] {
            let p = logs_dir.join(name);
            std::fs::write(&p, body.as_bytes()).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [
                    { "attempt_id": "att-1" },
                    { "attempt_id": "att-2" },
                    { "attempt_id": "att-3" },
                ]
            }]
        });

        // Budget covers all of att-1 plus only a partial tail of att-2, leaving 0 for att-3.
        let budget = body1.len() + (body2.len() / 2) + 20;
        let items = load_transcript_items(&repo.0, &report, budget).unwrap();
        let rendered = render_text_items(&items, View::Transcript, 120).join("\n");
        assert!(rendered.contains("CALL1_A"), "{rendered}");
        assert!(rendered.contains("CALL1_B"), "{rendered}");
        assert!(
            rendered.contains(TRANSCRIPT_BUDGET_TRUNCATED_NOTE),
            "expected budget truncation note for att-2:\n{rendered}"
        );
        assert!(
            !rendered.contains("CALL2_HEAD"),
            "att-2 head should be truncated by remaining budget:\n{rendered}"
        );
        assert!(
            rendered.contains("CALL2_TAIL"),
            "att-2 tail should still be retained within remaining budget:\n{rendered}"
        );
        assert!(
            rendered.contains("att-3") && rendered.contains(TRANSCRIPT_BUDGET_EXHAUSTED_NOTE),
            "att-3 should report budget exhaustion:\n{rendered}"
        );
        assert!(
            !rendered.contains("CALL3_NEVER_READ"),
            "att-3 must not be read after budget exhaustion:\n{rendered}"
        );
    }

    #[test]
    fn transcript_reads_masked_bounded_dispatch_report_and_prefers_dispatch_log() {
        let repo = TempRepo::new("transcript-delegate-report");
        let delegate_dir = repo.0.join(".oms/artifacts/delegate");
        std::fs::create_dir_all(&delegate_dir).unwrap();
        std::fs::set_permissions(&delegate_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let secret_like = [
            ["sk", "-", "live", "_"].concat(),
            ["1234567890", "abcdefghij", "klmnop"].concat(),
        ]
        .concat();
        let report_rel = ".oms/artifacts/delegate/codex-delegate-run1.md";
        let report_body = format!(
            concat!(
                "# OMS peer-delegate\n\n",
                "- provider: codex\n\n",
                "## Output\n\n",
                "Checked delegate report\n",
                "{{\"status\":\"ok\"}}\n",
                "Leaked value {}\n\n",
                "## Exit\n\n0\n"
            ),
            secret_like
        );
        let report_path = repo.0.join(report_rel);
        std::fs::write(&report_path, report_body.as_bytes()).unwrap();
        std::fs::set_permissions(&report_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let report_sha = oms_panel_core::digest(report_body.as_bytes());

        let report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T1",
                "calls": [{
                    "kind": "delegate",
                    "participant": "call-builder",
                    "attempt_id": "att-delegate",
                    "artifact": report_rel,
                    "artifact_sha256": report_sha,
                }]
            }]
        });

        // Without .oms/panel-state/dispatch-logs/, Transcript reads the per-attempt dispatch report
        // masked and preserving JSON lines from worker output.
        let mut reader = Reader {
            view: View::Transcript,
            ..Default::default()
        };
        let rendered = reader.lines(&repo.0, &report, 120).unwrap().join("\n");
        assert!(rendered.contains("Checked delegate report"), "{rendered}");
        assert!(rendered.contains("{\"status\":\"ok\"}"), "{rendered}");
        assert!(rendered.contains("## Exit"), "{rendered}");
        assert!(
            !rendered.contains(TRANSCRIPT_NOT_CAPTURED),
            "expected report instead of not captured:\n{rendered}"
        );
        assert!(
            !rendered.contains(&secret_like),
            "secret leaked:\n{rendered}"
        );

        // Resize reuses cached report without rereading disk.
        std::fs::remove_file(&report_path).unwrap();
        let narrow = reader.lines(&repo.0, &report, 36).unwrap().join("\n");
        assert!(narrow.contains("Checked delegate report"), "{narrow}");
        std::fs::write(&report_path, report_body.as_bytes()).unwrap();
        std::fs::set_permissions(&report_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // When a .oms/panel-state/dispatch-logs/ transcript exists, it stays first choice.
        let logs_dir = repo.0.join(".oms/panel-state/dispatch-logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::set_permissions(&logs_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tx_file = logs_dir.join("att-delegate.transcript");
        std::fs::write(
            &tx_file,
            b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"DISPATCH_LOG_FIRST_CHOICE\"}}\n",
        )
        .unwrap();
        std::fs::set_permissions(&tx_file, std::fs::Permissions::from_mode(0o600)).unwrap();

        let with_log = reader.render(&repo.0, &report, 120).unwrap().join("\n");
        assert!(with_log.contains("DISPATCH_LOG_FIRST_CHOICE"), "{with_log}");
        assert!(
            !with_log.contains("Checked delegate report"),
            "dispatch-logs must take precedence over dispatch report:\n{with_log}"
        );

        // Symlink, hardlink, traversal, non-dispatch subdir, digest mismatch, and group-writable
        // reports are all refused and fall back to not captured.
        let outside = repo.write_file("outside-report.md", b"OUTSIDE_SECRET_BODY\n");
        std::os::unix::fs::symlink(&outside, delegate_dir.join("sym-report.md")).unwrap();

        let hard_src = repo.write_file("hard-report.md", b"HARDLINK_SECRET_BODY\n");
        std::fs::hard_link(&hard_src, delegate_dir.join("hard-report.md")).unwrap();

        let non_dispatch_dir = repo.0.join(".oms/artifacts/panel-results");
        std::fs::create_dir_all(&non_dispatch_dir).unwrap();
        std::fs::set_permissions(&non_dispatch_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let non_dispatch_file = non_dispatch_dir.join("result.md");
        std::fs::write(&non_dispatch_file, b"NON_DISPATCH_BODY\n").unwrap();
        std::fs::set_permissions(&non_dispatch_file, std::fs::Permissions::from_mode(0o600))
            .unwrap();

        let tampered_file = delegate_dir.join("tampered-report.md");
        std::fs::write(&tampered_file, b"TAMPERED_BODY\n").unwrap();
        std::fs::set_permissions(&tampered_file, std::fs::Permissions::from_mode(0o600)).unwrap();

        let writable_file = delegate_dir.join("writable-report.md");
        std::fs::write(&writable_file, b"WRITABLE_BODY\n").unwrap();
        std::fs::set_permissions(&writable_file, std::fs::Permissions::from_mode(0o660)).unwrap();

        let refused_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T2",
                "calls": [
                    { "attempt_id": "att-sym", "artifact": ".oms/artifacts/delegate/sym-report.md" },
                    { "attempt_id": "att-hard", "artifact": ".oms/artifacts/delegate/hard-report.md" },
                    { "attempt_id": "att-trav", "artifact": ".oms/artifacts/delegate/../../outside-report.md" },
                    { "attempt_id": "att-nondisp", "artifact": ".oms/artifacts/panel-results/result.md" },
                    {
                        "attempt_id": "att-badsha",
                        "artifact": ".oms/artifacts/delegate/tampered-report.md",
                        "artifact_sha256": "0".repeat(64)
                    },
                    { "attempt_id": "att-writable", "artifact": ".oms/artifacts/delegate/writable-report.md" },
                ]
            }]
        });
        let refused = reader.render(&repo.0, &refused_report, 120).unwrap();
        let refused_text = refused.join("\n");
        for forbidden in [
            "OUTSIDE_SECRET_BODY",
            "HARDLINK_SECRET_BODY",
            "NON_DISPATCH_BODY",
            "TAMPERED_BODY",
            "WRITABLE_BODY",
        ] {
            assert!(
                !refused_text.contains(forbidden),
                "refused artifact leaked {forbidden}:\n{refused_text}"
            );
        }
        for att in [
            "att-sym",
            "att-hard",
            "att-trav",
            "att-nondisp",
            "att-badsha",
            "att-writable",
        ] {
            assert!(
                refused
                    .iter()
                    .any(|l| l.contains(att) && l.contains(TRANSCRIPT_NOT_CAPTURED)),
                "expected not captured for {att}:\n{refused_text}"
            );
        }

        // View byte budget bounds total bytes read across dispatch reports.
        let r1 = format!(
            "REPORT1_A_{}\nREPORT1_B_{}\n",
            "a".repeat(60),
            "b".repeat(60)
        );
        let r2 = format!(
            "REPORT2_HEAD_{}\nREPORT2_TAIL\nsecret {secret_like}\n",
            "c".repeat(80)
        );
        let r3 = format!("REPORT3_NEVER_READ_{}\n", "d".repeat(60));
        for (name, body) in [("r1.md", &r1), ("r2.md", &r2), ("r3.md", &r3)] {
            let p = delegate_dir.join(name);
            std::fs::write(&p, body.as_bytes()).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let budget_report = serde_json::json!({
            "kind": "oms-panel-results",
            "rows": [{
                "task_id": "T3",
                "calls": [
                    {
                        "attempt_id": "att-r1",
                        "artifact": ".oms/artifacts/delegate/r1.md",
                        "artifact_sha256": oms_panel_core::digest(r1.as_bytes()),
                    },
                    {
                        "attempt_id": "att-r2",
                        "artifact": ".oms/artifacts/delegate/r2.md",
                        "artifact_sha256": oms_panel_core::digest(r2.as_bytes()),
                    },
                    {
                        "attempt_id": "att-r3",
                        "artifact": ".oms/artifacts/delegate/r3.md",
                        "artifact_sha256": oms_panel_core::digest(r3.as_bytes()),
                    },
                ]
            }]
        });
        let budget = r1.len() + (r2.len() / 2) + 20;
        let items = load_transcript_items(&repo.0, &budget_report, budget).unwrap();
        let budgeted = render_text_items(&items, View::Transcript, 120).join("\n");
        assert!(budgeted.contains("REPORT1_A_"), "{budgeted}");
        assert!(budgeted.contains("REPORT1_B_"), "{budgeted}");
        assert!(
            budgeted.contains(TRANSCRIPT_BUDGET_TRUNCATED_NOTE),
            "expected budget truncation note for att-r2:\n{budgeted}"
        );
        assert!(
            !budgeted.contains("REPORT2_HEAD_"),
            "att-r2 head should be truncated:\n{budgeted}"
        );
        assert!(
            budgeted.contains("REPORT2_TAIL"),
            "att-r2 tail should be retained:\n{budgeted}"
        );
        // A tail cut inside a PEM block must not show the body whose BEGIN line was dropped.
        let dashes = "-".repeat(5);
        let cut = format!(
            "KEYBODY_LINE\n{dashes}END {}{dashes}\nAFTER_KEY",
            ["PRIVATE", "KEY"].join(" ")
        );
        let shown = format_report_bytes(cut.as_bytes(), true, true);
        assert!(!shown.contains("KEYBODY_LINE"), "{shown}");
        assert!(shown.contains("AFTER_KEY"), "{shown}");
        assert!(format_report_bytes(cut.as_bytes(), false, false).contains("KEYBODY_LINE"));
        let indented = cut.replace('\n', "\n    > ");
        let shown = format_report_bytes(indented.as_bytes(), true, true);
        assert!(
            !shown.contains("KEYBODY_LINE") && shown.contains("AFTER_KEY"),
            "{shown}"
        );
        assert!(
            !budgeted.contains(&secret_like),
            "secret in truncated report tail leaked:\n{budgeted}"
        );
        assert!(
            budgeted.contains("att-r3") && budgeted.contains(TRANSCRIPT_BUDGET_EXHAUSTED_NOTE),
            "att-r3 should report budget exhaustion:\n{budgeted}"
        );
        assert!(
            !budgeted.contains("REPORT3_NEVER_READ_"),
            "att-r3 must not be read after budget exhaustion:\n{budgeted}"
        );
    }
}
