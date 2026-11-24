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
    pub filter_before_edit: Option<String>,
    pub selection_before_edit: Option<(usize, usize)>,
    pub selection_before_attention: Option<(usize, usize)>,
    pub editing: bool,
    pub detail: bool,
    pub health: bool,
    pub help: bool,
    pub main_switch: Option<MainSwitchMenu>,
    pub folded: BTreeSet<String>,
    pub notice: Option<String>,
}
#[derive(Clone, Debug)]
pub struct MainSwitchMenu {
    pub current: String,
    pub choices: Vec<String>,
    pub selected: Option<usize>,
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
    SwitchMain,
    Provider(usize),
}
impl Navigation {
    pub fn apply(&mut self, key: &Key, count: usize, height: usize) -> Action {
        if self.editing {
            match key {
                Key::Escape => {
                    self.filter = self.filter_before_edit.take().unwrap_or_default();
                    if let Some((selected, offset)) = self.selection_before_edit.take() {
                        self.selected = selected;
                        self.offset = offset;
                    }
                    self.editing = false;
                }
                Key::Enter => {
                    self.filter_before_edit = None;
                    self.selection_before_edit = None;
                    self.editing = false;
                }
                Key::Backspace => {
                    if self.filter.pop().is_some() {
                        self.selected = 0;
                        self.offset = 0;
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    if self.filter.chars().count() < 40 {
                        self.filter.push(*c);
                        self.selected = 0;
                        self.offset = 0;
                    }
                }
                _ => return self.movement(key, count, height),
            }
            return Action::None;
        }
        match key {
            Key::Char('q') | Key::Eof => return Action::Quit,
            Key::Char('u' | '9') | Key::Repaint => return Action::Refresh,
            Key::Char('U') => return Action::Quota,
            Key::Char('c') => return Action::Council,
            Key::Char('/') if !self.detail => {
                self.filter_before_edit = Some(self.filter.clone());
                self.selection_before_edit = Some((self.selected, self.offset));
                self.editing = true;
            }
            Key::Escape => {
                if self.detail || self.help || self.health {
                    self.detail = false;
                    self.help = false;
                    self.health = false;
                } else {
                    self.filter.clear();
                }
            }
            Key::Char('g' | 't') => self.view = View::Tree,
            Key::Tab => self.view = self.view.next(),
            Key::Char('b') => {
                self.attention_only = !self.attention_only;
                if self.attention_only {
                    self.selection_before_attention = Some((self.selected, self.offset));
                    self.selected = 0;
                    self.offset = 0;
                } else if let Some((selected, offset)) = self.selection_before_attention.take() {
                    self.selected = selected;
                    self.offset = offset;
                } else {
                    self.selected = 0;
                    self.offset = 0;
                }
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
            Key::Char('s') => return Action::SwitchMain,
            Key::Char('1') => return Action::Provider(0),
            Key::Char('2') => return Action::Provider(1),
            Key::Char('3') => return Action::Provider(2),
            _ => return self.movement(key, count, height),
        }
        Action::None
    }
    /// Clamp `selected` to `[0, count)` and reconcile `offset` so the selection
    /// stays inside a `height`-row viewport without leaving trailing blank rows.
    pub fn clamp_selection(&mut self, count: usize, height: usize) {
        let page = height.max(1);
        self.selected = self.selected.min(count.saturating_sub(1));
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset.saturating_add(page) {
            self.offset = self.selected.saturating_sub(page - 1);
        }
        let max_offset = count.saturating_sub(page);
        if self.offset > max_offset {
            self.offset = max_offset;
        }
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
        self.clamp_selection(count, height);
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
            (Key::Char('s'), Action::SwitchMain),
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
        assert_eq!(nav.view, View::Tree);
        nav.apply(&Key::Char('t'), 5, 2);
        assert_eq!(nav.view, View::Tree);
        nav.apply(&Key::Tab, 5, 2);
        assert_eq!(nav.view, View::Auto);
        nav.apply(&Key::Tab, 5, 2);
        assert_eq!(nav.view, View::Compact);
        nav.apply(&Key::Tab, 5, 2);
        assert_eq!(nav.view, View::Auto);
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

    #[test]
    fn filter_edit_escape_restores_committed_filter_and_enter_commits() {
        let mut nav = Navigation::default();
        nav.apply(&Key::Char('/'), 5, 2);
        nav.apply(&Key::Char('é'), 5, 2);
        nav.apply(&Key::Char('x'), 5, 2);
        nav.apply(&Key::Enter, 5, 2);
        assert_eq!(nav.filter, "éx");
        assert!(!nav.editing);

        nav.selected = 3;
        nav.offset = 2;
        nav.apply(&Key::Char('/'), 5, 2);
        nav.apply(&Key::Backspace, 5, 2);
        nav.apply(&Key::Backspace, 5, 2);
        nav.apply(&Key::Char('z'), 5, 2);
        // Even if the in-progress filter currently matches 0 rows, Esc restores
        // both the committed filter and the pre-edit selection/scroll position.
        nav.apply(&Key::Escape, 0, 2);
        assert_eq!(nav.filter, "éx");
        assert!(!nav.editing);
        assert_eq!((nav.selected, nav.offset), (3, 2));

        // Moving selection with arrows while editing survives Enter commit.
        nav.apply(&Key::Char('/'), 5, 2);
        nav.apply(&Key::Backspace, 5, 2);
        nav.apply(&Key::Down, 5, 2);
        nav.apply(&Key::Down, 5, 2);
        assert_eq!((nav.selected, nav.offset), (2, 1));
        nav.apply(&Key::Enter, 5, 2);
        assert_eq!(nav.filter, "é");
        assert!(!nav.editing);
        assert_eq!((nav.selected, nav.offset), (2, 1));

        nav.apply(&Key::Escape, 5, 2);
        assert!(nav.filter.is_empty());
        nav.selected = 4;
        nav.offset = 3;
        nav.apply(&Key::Char('/'), 5, 2);
        nav.apply(&Key::Char('z'), 5, 2);
        nav.apply(&Key::Escape, 5, 2);
        assert!(nav.filter.is_empty());
        assert_eq!((nav.selected, nav.offset), (4, 3));
    }

    #[test]
    fn attention_toggle_and_clamp_keep_selection_and_offset_consistent() {
        let mut nav = Navigation {
            selected: 5,
            offset: 4,
            ..Default::default()
        };
        // Entering attention-only resets both selected and offset together.
        nav.apply(&Key::Char('b'), 10, 3);
        assert!(nav.attention_only);
        assert_eq!((nav.selected, nav.offset), (0, 0));

        // Leaving attention-only restores the pre-toggle position.
        nav.apply(&Key::Char('b'), 2, 3);
        assert!(!nav.attention_only);
        assert_eq!((nav.selected, nav.offset), (5, 4));

        // Toggling off when started in attention-only resets offset with selected.
        let mut started_attention = Navigation {
            attention_only: true,
            selected: 4,
            offset: 3,
            ..Default::default()
        };
        started_attention.apply(&Key::Char('b'), 6, 2);
        assert!(!started_attention.attention_only);
        assert_eq!(
            (started_attention.selected, started_attention.offset),
            (0, 0)
        );

        // Refresh reconciliation clamping brings stale offset back into bounds.
        let mut shrunk = Navigation {
            selected: 8,
            offset: 7,
            ..Default::default()
        };
        shrunk.selected = 1;
        shrunk.clamp_selection(3, 4);
        assert_eq!((shrunk.selected, shrunk.offset), (1, 0));

        let mut moved_down = Navigation {
            selected: 7,
            offset: 1,
            ..Default::default()
        };
        moved_down.clamp_selection(10, 3);
        assert_eq!((moved_down.selected, moved_down.offset), (7, 5));
    }
}
