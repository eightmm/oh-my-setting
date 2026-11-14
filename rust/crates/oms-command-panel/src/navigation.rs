use crate::input::Key;
use crate::layout::View;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct Navigation {
    pub selected: usize,
    pub offset: usize,
    pub view: View,
    pub attention_only: bool,
    pub filter: String,
    pub editing: bool,
    pub detail: bool,
    pub health: bool,
    pub help: bool,
    pub folded: BTreeSet<String>,
    pub notice: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    Open,
    ReadAll,
    Results,
    Chats,
    Controls,
}
impl Navigation {
    pub fn apply(&mut self, key: &Key, count: usize, height: usize) -> Action {
        if self.editing {
            match key {
                Key::Escape => {
                    self.filter.clear();
                    self.editing = false;
                }
                Key::Enter => self.editing = false,
                Key::Backspace => {
                    self.filter.pop();
                }
                Key::Char(c) if !c.is_control() => {
                    if self.filter.chars().count() < 40 {
                        self.filter.push(*c);
                    }
                }
                _ => return self.movement(key, count, height),
            }
            self.selected = 0;
            self.offset = 0;
            return Action::None;
        }
        match key {
            Key::Char('q') | Key::Eof => return Action::Quit,
            Key::Char('u' | '9') | Key::Repaint => return Action::Refresh,
            Key::Char('/') if !self.detail => self.editing = true,
            Key::Escape => {
                if self.detail || self.help || self.health {
                    self.detail = false;
                    self.help = false;
                    self.health = false;
                } else {
                    self.filter.clear();
                }
            }
            Key::Char('g') => self.view = View::Graph,
            Key::Char('t') => self.view = View::Tree,
            Key::Tab => self.view = self.view.next(),
            Key::Char('b') => {
                self.attention_only = !self.attention_only;
                self.selected = 0;
            }
            Key::Char('k') => self.health = !self.health,
            Key::Char('?') | Key::Function(12) => self.help = !self.help,
            Key::Enter | Key::Char('f') => {
                self.detail = true;
                return Action::Open;
            }
            Key::Char('r') => return Action::ReadAll,
            Key::Char('R') => return Action::Results,
            Key::Char('h') => return Action::Chats,
            Key::Char('j') => return Action::Controls,
            _ => return self.movement(key, count, height),
        }
        Action::None
    }
    fn movement(&mut self, key: &Key, count: usize, height: usize) -> Action {
        let page = height.saturating_sub(6).max(1);
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = self.selected.saturating_add(1),
            Key::PageUp => self.selected = self.selected.saturating_sub(page),
            Key::PageDown => self.selected = self.selected.saturating_add(page),
            Key::Home => self.selected = 0,
            Key::End => self.selected = count.saturating_sub(1),
            Key::Scroll(n) if *n < 0 => {
                self.selected = self.selected.saturating_sub(n.unsigned_abs() as usize)
            }
            Key::Scroll(n) => self.selected = self.selected.saturating_add(*n as usize),
            _ => {}
        }
        self.selected = self.selected.min(count.saturating_sub(1));
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset.saturating_add(page) {
            self.offset = self.selected.saturating_sub(page - 1);
        }
        Action::None
    }
}
pub fn matches(call: &serde_json::Value, query: &str) -> bool {
    let haystack = [
        "title", "role", "purpose", "provider", "model", "alias", "state",
    ]
    .iter()
    .map(|k| crate::text(call, k))
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
    .replace('_', " ");
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| haystack.contains(word))
}

pub const BOARD_HINT: &str = "Up/Down Move  Enter Open  Tab View  / Filter  b Attention  g Graph  t Tree  u Refresh  ? Keys  q Quit";
pub const READER_HINT: &str = "Tab View  r Raw  e Events  d Diff  i Info  x Transcript  f Files  / Search  n/N Match  Space Page  q/Esc close";
pub const CONTROL_HINT: &str = "1 Claude  2 Pi  0 Codex  s Swap main  o Rooms  k Capacity  9 Refresh  h Chats  r Results  j Terminals and jobs  5 Explain  6 Implement  e Researcher  a Advisor  7 Review  c Council  f Finalize  n Retry delivery  q Quit";
