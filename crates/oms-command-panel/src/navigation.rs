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
    Quota,
    Council,
    Open,
    Detail,
    ReadAll,
    Results,
    Chats,
    Controls,
    Provider(usize),
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
            Key::Char('U') => return Action::Quota,
            Key::Char('c') => return Action::Council,
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
            Key::Enter => {
                self.detail = true;
                return Action::Open;
            }
            Key::Char('f') => {
                self.detail = true;
                return Action::Detail;
            }
            Key::Char('r') => return Action::ReadAll,
            Key::Char('R') => return Action::Results,
            Key::Char('h') => return Action::Chats,
            Key::Char('j') => return Action::Controls,
            Key::Char('1') => return Action::Provider(0),
            Key::Char('2') => return Action::Provider(1),
            Key::Char('3') => return Action::Provider(2),
            _ => return self.movement(key, count, height),
        }
        Action::None
    }
    fn movement(&mut self, key: &Key, count: usize, height: usize) -> Action {
        let page = height.max(1);
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
/// Lowercased, whitespace-split filter words, computed once per filter.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}
pub fn matches(call: &serde_json::Value, query: &str) -> bool {
    matches_words(call, &words(query))
}
pub fn matches_words(call: &serde_json::Value, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let haystack = [
        "title", "role", "purpose", "provider", "model", "alias", "state",
    ]
    .iter()
    .map(|k| crate::text(call, k))
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
    .replace('_', " ");
    words.iter().all(|word| haystack.contains(word.as_str()))
}

pub const BOARD_HINT: &str = "Up/Down Move  Enter Open  Tab View  / Filter  b Attention  g Graph  t Tree  u Refresh  ? Keys  q Quit";
pub const READER_HINT: &str = "Tab View  r Raw  e Events  d Diff  i Info  x Transcript  f Files  / Search  n/N Match  Space Page  q/Esc close";
pub const CONTROL_HINT: &str = "1 Claude  2 Pi  0 Codex  s Swap main  o Rooms  k Capacity  9 Refresh  h Chats  r Results  j Terminals and jobs  5 Explain  6 Implement  e Researcher  a Advisor  7 Review  c Council  f Finalize  n Retry delivery  q Quit";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_shortcuts_apply_actual_actions_and_navigation() {
        let mut nav = Navigation::default();
        for (key, action) in [
            (Key::Enter, Action::Open),
            (Key::Char('f'), Action::Detail),
            (Key::Char('R'), Action::Results),
            (Key::Char('r'), Action::ReadAll),
            (Key::Char('h'), Action::Chats),
            (Key::Char('j'), Action::Controls),
            (Key::Char('u'), Action::Refresh),
            (Key::Char('U'), Action::Quota),
            (Key::Char('9'), Action::Refresh),
            (Key::Repaint, Action::Refresh),
            (Key::Char('q'), Action::Quit),
        ] {
            assert_eq!(nav.apply(&key, 5, 2), action, "{key:?}");
        }
        nav.apply(&Key::Down, 5, 2);
        assert_eq!(nav.selected, 1);
        nav.apply(&Key::PageDown, 5, 2);
        assert_eq!(nav.selected, 3);
        nav.apply(&Key::End, 5, 2);
        assert_eq!(nav.selected, 4);
        nav.apply(&Key::Home, 5, 2);
        assert_eq!(nav.selected, 0);
        nav.apply(&Key::Char('g'), 5, 2);
        assert_eq!(nav.view, View::Graph);
        nav.apply(&Key::Char('t'), 5, 2);
        assert_eq!(nav.view, View::Tree);
        let before = nav.view;
        nav.apply(&Key::Tab, 5, 2);
        assert_ne!(nav.view, before);
        nav.apply(&Key::Char('b'), 5, 2);
        assert!(nav.attention_only);
        nav.apply(&Key::Char('k'), 5, 2);
        assert!(nav.health);
        nav.apply(&Key::Char('?'), 5, 2);
        assert!(nav.help);
        nav.apply(&Key::Function(12), 5, 2);
        assert!(!nav.help);
        nav.apply(&Key::Escape, 5, 2);
        assert!(!nav.health && !nav.detail);
        nav.apply(&Key::Char('/'), 5, 2);
        assert!(nav.editing);
        nav.apply(&Key::Char('x'), 5, 2);
        assert_eq!(nav.filter, "x");
        nav.apply(&Key::Escape, 5, 2);
        assert!(!nav.editing && nav.filter.is_empty());
    }
}
