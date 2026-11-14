use crate::{Error, Result, input::Key, render, rows, text};
use oms_panel_core::{Store, Value};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
#[derive(Default)]
pub struct Reader {
    pub view: View,
    pub offset: usize,
    pub query: String,
    pub searching: bool,
    pub section: usize,
}
impl Reader {
    pub fn render(&self, repo: &Path, report: &Value, width: usize) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let store = Store::open(repo)?;
        if self.view == View::Summary {
            return Ok(render::results(report, width)
                .lines()
                .map(str::to_owned)
                .collect());
        }
        for task in rows(&report["rows"]) {
            out.push(format!("Task {} · {:?}", text(task, "task_id"), self.view));
            for call in rows(&task["calls"]) {
                match self.view {
                    View::Diff | View::Files => {
                        if call["patch"].is_string() {
                            let patch = oms_panel_core::results::verified_patch(&store, call)?;
                            if self.view == View::Files {
                                for change in oms_panel_core::files::patch_changes(&patch)? {
                                    out.push(format!(
                                        "{} +{} -{}",
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
                    View::Raw | View::Transcript => {
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
                    View::Summary => {}
                }
            }
        }
        if out.is_empty() {
            out.push("No recorded evidence for this view".into());
        }
        Ok(out)
    }
    pub fn apply(&mut self, key: &Key, lines: &[String], height: usize) -> bool {
        let page = height.saturating_sub(3).max(1);
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
        match key {
            Key::Char('q') | Key::Escape | Key::Eof => return false,
            Key::Tab => {
                self.view = self.view.next();
                self.offset = 0;
            }
            Key::Char('r') => {
                self.view = View::Raw;
                self.offset = 0;
            }
            Key::Char('e') => {
                self.view = View::Events;
                self.offset = 0;
            }
            Key::Char('d') => {
                self.view = View::Diff;
                self.offset = 0;
            }
            Key::Char('i') => {
                self.view = View::Info;
                self.offset = 0;
            }
            Key::Char('x') => {
                self.view = View::Transcript;
                self.offset = 0;
            }
            Key::Char('f') => {
                self.view = View::Files;
                self.offset = 0;
            }
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
        for n in 1..=lines.len() {
            let index = if backwards {
                (self.offset + lines.len() - n % lines.len()) % lines.len()
            } else {
                (self.offset + n) % lines.len()
            };
            if lines[index]
                .to_lowercase()
                .contains(&self.query.to_lowercase())
            {
                self.offset = index;
                return;
            }
        }
    }
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
    while !interrupted.load(Ordering::Acquire) {
        let (width, height) = size();
        let lines = reader.render(repo, report, width)?;
        let mut screen = if tty {
            String::from("\x1b[H\x1b[2J")
        } else {
            String::new()
        };
        for line in lines
            .iter()
            .skip(reader.offset)
            .take(height.saturating_sub(2))
        {
            screen.push_str(&render::clip(line, width));
            screen.push_str(eol);
        }
        screen.push_str(&render::clip(
            &format!(
                "{} / {}{}",
                reader.offset + 1,
                lines.len(),
                if reader.searching {
                    format!(" /{}", reader.query)
                } else {
                    String::new()
                }
            ),
            width,
        ));
        screen.push_str(eol);
        screen.push_str(&render::clip(crate::navigation::READER_HINT, width));
        if !tty {
            screen.push('\n');
        }
        out.write_all(screen.as_bytes()).map_err(Error::new)?;
        out.flush().map_err(Error::new)?;
        for key in next_keys(Duration::from_millis(100))? {
            if !reader.apply(&key, &lines, height) {
                return Ok(());
            }
        }
    }
    Ok(())
}
