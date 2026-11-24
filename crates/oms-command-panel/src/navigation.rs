use crate::input::Key;
use crate::layout::View;
use std::collections::BTreeSet;

/// Stable call identity matching `watch::call_identity`: `(participant, attempt)`.
pub type CallIdentity = (String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedSelection {
    pub identity: Option<CallIdentity>,
    pub selected: usize,
    pub offset: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Navigation {
    pub selected: usize,
    pub offset: usize,
    pub view: View,
    pub attention_only: bool,
    pub filter: String,
    pub filter_before_edit: Option<String>,
    pub selection_before_edit: Option<SavedSelection>,
    pub selection_before_attention: Option<SavedSelection>,
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
/// Stable call identity matching `watch::call_identity`.
pub fn call_identity(call: &serde_json::Value) -> CallIdentity {
    (
        crate::text(call, "participant").to_owned(),
        crate::text(call, "attempt").to_owned(),
    )
}

impl Navigation {
    pub fn apply(&mut self, key: &Key, count: usize, height: usize) -> Action {
        if self.editing {
            match key {
                Key::Escape => {
                    self.filter = self.filter_before_edit.take().unwrap_or_default();
                    self.selection_before_edit = None;
                    self.editing = false;
                    self.selected = 0;
                    self.offset = 0;
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
                self.selection_before_edit = None;
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
                self.selection_before_attention = None;
                self.selected = 0;
                self.offset = 0;
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
    /// Apply a Board key with call-identity awareness so filter-cancel (`Esc`),
    /// attention toggle (`b`), view changes and filter clears reconcile by call
    /// identity and clamp `selected`/`offset` against the newly visible rows.
    pub fn apply_with_calls<F>(
        &mut self,
        key: &Key,
        before: &[serde_json::Value],
        after_visible: F,
        height: usize,
    ) -> Action
    where
        F: FnOnce(&Self) -> Vec<serde_json::Value>,
    {
        let current_saved = SavedSelection {
            identity: before.get(self.selected).map(call_identity),
            selected: self.selected,
            offset: self.offset,
        };
        if self.editing && *key == Key::Escape {
            self.filter = self.filter_before_edit.take().unwrap_or_default();
            self.editing = false;
            let after = after_visible(self);
            if let Some(saved) = self.selection_before_edit.take() {
                self.restore_saved(saved, &after, height);
            } else {
                self.selected = 0;
                self.offset = 0;
                self.clamp_selection(after.len(), height);
            }
            return Action::None;
        }
        if !self.editing && !self.detail && *key == Key::Char('/') {
            self.filter_before_edit = Some(self.filter.clone());
            self.selection_before_edit = Some(current_saved);
            self.editing = true;
            return Action::None;
        }
        if !self.editing && *key == Key::Char('b') {
            self.attention_only = !self.attention_only;
            if self.attention_only {
                self.selection_before_attention = Some(current_saved);
                self.selected = 0;
                self.offset = 0;
            } else {
                let after = after_visible(self);
                if let Some(saved) = self.selection_before_attention.take() {
                    self.restore_saved(saved, &after, height);
                } else {
                    self.selected = 0;
                    self.offset = 0;
                    self.clamp_selection(after.len(), height);
                }
            }
            return Action::None;
        }
        let was_view = self.view;
        let was_filter = self.filter.clone();
        let was_editing = self.editing;
        let action = self.apply(key, before.len(), height);
        if self.view != was_view || (!was_editing && self.filter != was_filter) {
            let after = after_visible(self);
            self.restore_saved(current_saved, &after, height);
        }
        action
    }
    /// Reconcile `selected` and `offset` across a live refresh by call identity.
    /// Returns `true` when the previously selected call disappeared from `after`.
    pub fn reconcile_refresh(
        &mut self,
        before: &[serde_json::Value],
        after: &[serde_json::Value],
        height: usize,
    ) -> bool {
        let prev_id = before.get(self.selected).map(call_identity);
        let disappeared = prev_id
            .as_ref()
            .is_some_and(|id| !after.iter().any(|call| call_identity(call) == *id));
        self.selected = prev_id
            .and_then(|id| after.iter().position(|call| call_identity(call) == id))
            .unwrap_or_else(|| self.selected.min(after.len().saturating_sub(1)));
        self.clamp_selection(after.len(), height);
        disappeared
    }
    fn restore_saved(&mut self, saved: SavedSelection, after: &[serde_json::Value], height: usize) {
        self.selected = saved
            .identity
            .as_ref()
            .and_then(|id| after.iter().position(|call| call_identity(call) == *id))
            .unwrap_or_else(|| saved.selected.min(after.len().saturating_sub(1)));
        self.offset = saved.offset;
        self.clamp_selection(after.len(), height);
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

        // Index-only apply never restores an unchecked raw index after Esc.
        nav.selected = 3;
        nav.offset = 2;
        nav.apply(&Key::Char('/'), 5, 2);
        nav.apply(&Key::Backspace, 5, 2);
        nav.apply(&Key::Backspace, 5, 2);
        nav.apply(&Key::Char('z'), 5, 2);
        nav.apply(&Key::Escape, 0, 2);
        assert_eq!(nav.filter, "éx");
        assert!(!nav.editing);
        assert_eq!((nav.selected, nav.offset), (0, 0));

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
    }

    #[test]
    fn filter_cancel_attention_toggle_and_refresh_reconcile_by_call_identity_and_clamp() {
        let make_call = |id: &str, attempt: &str, title: &str, state: &str| {
            serde_json::json!({
                "participant": id,
                "attempt": attempt,
                "attempt_id": attempt,
                "title": title,
                "role": "builder",
                "provider": "codex",
                "state": state,
            })
        };
        let mut report = serde_json::json!({
            "room": {
                "id": "R1",
                "participants": [
                    make_call("call-0", "att-0", "alpha", "running"),
                    make_call("call-1", "att-1", "beta", "blocked"),
                    make_call("call-2", "att-2", "gamma", "running"),
                    make_call("call-3", "att-3", "delta", "running"),
                    make_call("call-4", "att-4", "target", "running"),
                    make_call("call-5", "att-5", "omega", "running"),
                ]
            }
        });

        let mut nav = Navigation {
            selected: 4,
            offset: 3,
            ..Default::default()
        };
        let height = 2;
        let visible_now = |rep: &serde_json::Value, n: &Navigation| crate::render::visible(rep, n);

        // Start filter edit while call-4 is selected at (4, 3).
        let before = visible_now(&report, &nav);
        assert_eq!(before.len(), 6);
        let target_idx = before
            .iter()
            .position(|c| call_identity(c) == ("call-4".into(), "att-4".into()))
            .unwrap();
        nav.selected = target_idx;
        nav.offset = target_idx.saturating_sub(1);
        nav.apply_with_calls(
            &Key::Char('/'),
            &before,
            |n| visible_now(&report, n),
            height,
        );
        let cur = visible_now(&report, &nav);
        nav.apply_with_calls(&Key::Char('z'), &cur, |n| visible_now(&report, n), height);
        assert!(visible_now(&report, &nav).is_empty());

        // Live refresh while editing: call-0..call-2 disappear and call-4 moves!
        let pre_refresh = visible_now(&report, &nav);
        report["room"]["participants"] = serde_json::json!([
            make_call("call-3", "att-3", "delta", "running"),
            make_call("call-4", "att-4", "target", "running"),
            make_call("call-5", "att-5", "omega", "running"),
        ]);
        let post_refresh = visible_now(&report, &nav);
        nav.reconcile_refresh(&pre_refresh, &post_refresh, height);

        // Esc cancels filter edit: reconciles by call identity to call-4's new index
        // and clamps offset (never restores stale raw index/offset).
        nav.apply_with_calls(
            &Key::Escape,
            &post_refresh,
            |n| visible_now(&report, n),
            height,
        );
        let after_cancel = visible_now(&report, &nav);
        assert!(!nav.editing && nav.filter.is_empty());
        assert_eq!(after_cancel.len(), 3);
        assert_eq!(
            call_identity(&after_cancel[nav.selected]),
            ("call-4".into(), "att-4".into())
        );
        assert!(nav.offset <= after_cancel.len().saturating_sub(height));
        assert!(nav.selected >= nav.offset && nav.selected < nav.offset + height);

        // Enter attention-only (`b`), then refresh removes call-4 and shrinks the list to 2 rows.
        report["room"]["participants"] = serde_json::json!([
            make_call("call-1", "att-1", "beta", "blocked"),
            make_call("call-3", "att-3", "delta", "running"),
            make_call("call-4", "att-4", "target", "running"),
            make_call("call-5", "att-5", "omega", "running"),
        ]);
        let before_b = visible_now(&report, &nav);
        let b_idx = before_b
            .iter()
            .position(|c| call_identity(c) == ("call-4".into(), "att-4".into()))
            .unwrap();
        nav.selected = b_idx;
        nav.offset = b_idx;
        nav.apply_with_calls(
            &Key::Char('b'),
            &before_b,
            |n| visible_now(&report, n),
            height,
        );
        assert!(nav.attention_only);
        assert_eq!((nav.selected, nav.offset), (0, 0));

        // Refresh removes call-4 and shrinks total rows to 2.
        let att_before = visible_now(&report, &nav);
        report["room"]["participants"] = serde_json::json!([
            make_call("call-1", "att-1", "beta", "blocked"),
            make_call("call-5", "att-5", "omega", "running"),
        ]);
        let att_after = visible_now(&report, &nav);
        assert!(!nav.reconcile_refresh(&att_before, &att_after, height));

        // Leaving attention-only falls back and clamps both selected and offset to 2 rows.
        nav.apply_with_calls(
            &Key::Char('b'),
            &att_after,
            |n| visible_now(&report, n),
            height,
        );
        assert!(!nav.attention_only);
        assert_eq!((nav.selected, nav.offset), (1, 0));

        // Direct reconcile_refresh detects disappearance and clamps stale offset.
        let before_shrink = vec![
            make_call("call-a", "att-a", "a", "running"),
            make_call("call-b", "att-b", "b", "running"),
            make_call("call-c", "att-c", "c", "running"),
            make_call("call-d", "att-d", "d", "running"),
            make_call("call-e", "att-e", "e", "running"),
        ];
        let after_shrink = vec![
            make_call("call-a", "att-a", "a", "running"),
            make_call("call-b", "att-b", "b", "running"),
        ];
        let mut shrunk = Navigation {
            selected: 4,
            offset: 3,
            ..Default::default()
        };
        assert!(shrunk.reconcile_refresh(&before_shrink, &after_shrink, 4));
        assert_eq!((shrunk.selected, shrunk.offset), (1, 0));

        // Index-only `b` toggle resets both selected and offset without leaving stale offset.
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
    }
}
