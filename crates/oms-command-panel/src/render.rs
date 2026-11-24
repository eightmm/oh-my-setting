//! Terminal text is untrusted. Only renderer-owned escape sequences reach the TTY.
use crate::layout::View;
use crate::{attention, navigation, rows, text};
use oms_panel_core::Value;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use unicode_general_category::{GeneralCategory, get_general_category};

const SGR_RESET: &str = "\x1b[0m";
const SGR_BOLD: &str = "\x1b[1m";
const SGR_DIM: &str = "\x1b[2m";
const SGR_AMBER_BOLD: &str = "\x1b[1;38;5;215m";
const SGR_MAIN_TITLE: &str = "\x1b[1;38;5;16;48;5;222m";
const SGR_VIOLET_BOLD: &str = "\x1b[1;38;5;141m";
const SGR_SKY_BOLD: &str = "\x1b[1;38;5;111m";
const SGR_SAGE_BOLD: &str = "\x1b[1;38;5;114m";
const SGR_SAGE: &str = "\x1b[38;5;114m";
const SGR_CYAN: &str = "\x1b[38;5;117m";
const SGR_GOLD_BOLD: &str = "\x1b[1;38;5;222m";
const SGR_CORAL_BOLD: &str = "\x1b[1;38;5;203m";
const SGR_MUTED: &str = "\x1b[38;5;245m";
const SGR_BORDER: &str = "\x1b[38;5;240m";
const SGR_TEXT: &str = "\x1b[38;5;252m";
const SGR_SEL_BG: &str = "\x1b[48;5;236m";

#[derive(Clone)]
struct Span {
    plain: String,
    sgr: Option<&'static str>,
}

impl Span {
    fn raw(text: impl Into<String>, limit: usize, sgr: Option<&'static str>) -> Self {
        Self {
            plain: clean(&text.into(), limit),
            sgr,
        }
    }
    fn literal(text: &'static str, sgr: Option<&'static str>) -> Self {
        Self {
            plain: text.to_owned(),
            sgr,
        }
    }
}

pub fn color_enabled(tty: bool) -> bool {
    let no_color = std::env::var("NO_COLOR").ok();
    let panel_color = std::env::var("OMS_PANEL_COLOR").ok();
    let term = std::env::var("TERM").ok();
    resolve_color(
        tty,
        no_color.as_deref(),
        panel_color.as_deref(),
        term.as_deref(),
    )
}

fn resolve_color(
    tty: bool,
    no_color: Option<&str>,
    panel_color: Option<&str>,
    term: Option<&str>,
) -> bool {
    if let Some(flag) = panel_color.map(str::trim) {
        if matches!(
            flag.to_ascii_lowercase().as_str(),
            "0" | "never" | "false" | "off"
        ) {
            return false;
        }
        if matches!(
            flag.to_ascii_lowercase().as_str(),
            "1" | "always" | "true" | "on"
        ) && no_color.is_none_or(str::is_empty)
        {
            return true;
        }
    }
    if no_color.is_some_and(|v| !v.is_empty()) {
        return false;
    }
    tty && term != Some("dumb")
}

pub fn clean(value: &str, maximum: usize) -> String {
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    let mut count = 0;
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | '^' | '_') => {
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (escaped && c == '\\') {
                            break;
                        }
                        escaped = c == '\u{1b}';
                    }
                }
                _ => {}
            }
            continue;
        }
        if c.is_control()
            || matches!(
                get_general_category(c),
                GeneralCategory::Format | GeneralCategory::Surrogate
            )
        {
            continue;
        }
        if count == maximum {
            break;
        }
        out.push(c);
        count += 1;
    }
    out
}

pub fn cell_width(c: char) -> usize {
    if c.is_control()
        || matches!(
            get_general_category(c),
            GeneralCategory::NonspacingMark
                | GeneralCategory::EnclosingMark
                | GeneralCategory::Format
        )
    {
        return 0;
    }
    if matches!(c as u32, 0x1100..=0x115f | 0x2329..=0x232a | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe10..=0xfe19 | 0xfe30..=0xfe6f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff | 0x20000..=0x3fffd)
    {
        2
    } else {
        1
    }
}

pub fn width(value: &str) -> usize {
    value.chars().map(cell_width).sum()
}

pub fn clip(value: &str, maximum: usize) -> String {
    let value = clean(value, 1_048_576);
    if width(&value) <= maximum {
        return value;
    }
    if maximum == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in value.chars() {
        let cells = cell_width(c);
        if used + cells > maximum.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += cells;
    }
    out.push('…');
    out
}

pub fn wrap(value: &str, maximum: usize) -> Vec<String> {
    let maximum = maximum.max(1);
    let mut out = Vec::new();
    for line in value.lines() {
        let line = clean(line, 1_048_576);
        let mut current = String::new();
        let mut used = 0;
        for c in line.chars() {
            let cells = cell_width(c);
            if used + cells > maximum && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                used = 0;
            }
            if cells <= maximum {
                current.push(c);
                used += cells;
            }
        }
        out.push(current);
    }
    out
}

fn wrap_words(value: &str, maximum: usize) -> Vec<String> {
    let maximum = maximum.max(1);
    let mut out = Vec::new();
    for line in value.lines() {
        let line = clean(line, 1_048_576);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if width(trimmed) <= maximum {
            out.push(trimmed.to_owned());
            continue;
        }
        let mut current = String::new();
        let mut used = 0usize;
        for word in trimmed.split_whitespace() {
            let word_w = width(word);
            if current.is_empty() {
                if word_w <= maximum {
                    current.push_str(word);
                    used = word_w;
                } else {
                    let mut parts = wrap(word, maximum);
                    if let Some(last) = parts.pop() {
                        out.extend(parts);
                        used = width(&last);
                        current = last;
                    }
                }
            } else if used + 1 + word_w <= maximum {
                current.push(' ');
                current.push_str(word);
                used += 1 + word_w;
            } else {
                out.push(std::mem::take(&mut current));
                if word_w <= maximum {
                    current.push_str(word);
                    used = word_w;
                } else {
                    let mut parts = wrap(word, maximum);
                    if let Some(last) = parts.pop() {
                        out.extend(parts);
                        used = width(&last);
                        current = last;
                    } else {
                        used = 0;
                    }
                }
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| width(&s.plain)).sum()
}

fn clip_spans(spans: Vec<Span>, maximum: usize) -> Vec<Span> {
    if spans_width(&spans) <= maximum {
        return spans;
    }
    if maximum == 0 {
        return vec![];
    }
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let remaining = maximum.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let w = width(&span.plain);
        if w <= remaining {
            used += w;
            out.push(span);
        } else {
            let clipped = clip(&span.plain, remaining);
            if !clipped.is_empty() {
                out.push(Span {
                    plain: clipped,
                    sgr: span.sgr,
                });
            }
            break;
        }
    }
    out
}

fn emit_spans(out: &mut String, spans: &[Span], colored: bool, row_bg: Option<&'static str>) {
    for span in spans {
        if span.plain.is_empty() {
            continue;
        }
        if colored {
            if let Some(sgr) = span.sgr {
                out.push_str(sgr);
            }
            out.push_str(&span.plain);
            if span.sgr.is_some() {
                out.push_str(SGR_RESET);
                if let Some(bg) = row_bg {
                    out.push_str(bg);
                }
            }
        } else {
            out.push_str(&span.plain);
        }
    }
}

fn left_budget(inner_width: usize, right_width: usize) -> usize {
    inner_width.saturating_sub(right_width + usize::from(right_width > 0))
}

fn render_lr(
    left: Vec<Span>,
    right: Vec<Span>,
    inner_width: usize,
    pad_tail: bool,
    row_bg: Option<&'static str>,
    colored: bool,
) -> String {
    let right = clip_spans(right, inner_width);
    let right_w = spans_width(&right);
    let left = clip_spans(left, left_budget(inner_width, right_w));
    let left_w = spans_width(&left);
    let gap = if right_w > 0 {
        inner_width.saturating_sub(left_w + right_w)
    } else if pad_tail || (colored && row_bg.is_some()) {
        inner_width.saturating_sub(left_w)
    } else {
        0
    };
    let mut out = String::new();
    if colored && let Some(bg) = row_bg {
        out.push_str(bg);
    }
    emit_spans(&mut out, &left, colored, row_bg);
    if gap > 0 {
        out.extend(std::iter::repeat_n(' ', gap));
    }
    emit_spans(&mut out, &right, colored, row_bg);
    if colored && row_bg.is_some() {
        out.push_str(SGR_RESET);
    }
    out
}

fn card_top(title: &str, tag: Option<&str>, columns: usize, colored: bool) -> String {
    card_top_styled(title, tag, columns, colored, SGR_BOLD)
}

fn card_top_styled(
    title: &str,
    tag: Option<&str>,
    columns: usize,
    colored: bool,
    title_style: &'static str,
) -> String {
    let inner = columns.saturating_sub(4);
    if columns < 44 {
        let mut left = vec![Span::raw(title, inner, Some(title_style))];
        if let Some(t) = tag.filter(|v| !v.is_empty()) {
            left.push(Span::literal(" · ", Some(SGR_MUTED)));
            left.push(Span::raw(t, inner, Some(SGR_MUTED)));
        }
        return render_lr(left, vec![], columns, false, None, colored);
    }

    let clean_tag = tag
        .filter(|v| !v.is_empty())
        .map(|v| clip(v, (columns / 3).max(6)))
        .filter(|v| !v.is_empty());
    let right_len = clean_tag.as_ref().map_or(0, |t| width(t) + 3);
    let clean_title = clip(title, columns.saturating_sub(5 + right_len));
    let title_w = width(&clean_title);
    let prefix_len = 3usize; // "╭─ "
    let title_space = 1usize; // " "
    let end_cap = 1usize; // "╮"
    let used = prefix_len + title_w + title_space + right_len + end_cap;
    let fill = columns.saturating_sub(used);
    let mut spans = vec![
        Span::literal("╭─ ", Some(SGR_BORDER)),
        Span {
            plain: clean_title,
            sgr: Some(title_style),
        },
        Span::literal(" ", None),
        Span {
            plain: "─".repeat(fill),
            sgr: Some(SGR_BORDER),
        },
    ];
    if let Some(t) = clean_tag {
        spans.push(Span::literal(" ", None));
        spans.push(Span {
            plain: t,
            sgr: Some(SGR_MUTED),
        });
        spans.push(Span::literal(" ─", Some(SGR_BORDER)));
    }
    spans.push(Span::literal("╮", Some(SGR_BORDER)));
    let mut out = String::new();
    emit_spans(&mut out, &spans, colored, None);
    out
}

const MAIN_SWITCH_LABELS: [&str; 2] = ["[s Switch Main]", "[s Switch]"];

fn card_top_with_action(
    title: &str,
    tag: Option<&str>,
    action_labels: &[&'static str],
    columns: usize,
    colored: bool,
    title_style: &'static str,
) -> (String, Option<(usize, usize)>) {
    if columns < 44 {
        return (
            card_top_styled(title, tag, columns, colored, title_style),
            None,
        );
    }
    let clean_tag = tag
        .filter(|v| !v.is_empty())
        .map(|v| clip(v, (columns / 3).max(6)))
        .filter(|v| !v.is_empty());
    let clean_title = clean(title, columns);
    let title_w = width(&clean_title);
    let prefix_len = 3usize; // "╭─ "
    let title_space = 1usize; // " "
    let end_cap = 1usize; // "╮"
    let tag_w = clean_tag.as_deref().map_or(0, width);
    let Some(&action) = action_labels.iter().find(|&&label| {
        let action_w = width(label);
        let right_len = if clean_tag.is_some() {
            tag_w + action_w + 6
        } else {
            action_w + 3
        };
        prefix_len + title_w + title_space + right_len + end_cap < columns
    }) else {
        return (
            card_top_styled(title, tag, columns, colored, title_style),
            None,
        );
    };
    let action_w = width(action);
    let right_len = if clean_tag.is_some() {
        tag_w + action_w + 6
    } else {
        action_w + 3
    };
    let used = prefix_len + title_w + title_space + right_len + end_cap;
    let fill = columns.saturating_sub(used);
    let mut spans = vec![
        Span::literal("╭─ ", Some(SGR_BORDER)),
        Span {
            plain: clean_title,
            sgr: Some(title_style),
        },
        Span::literal(" ", None),
        Span {
            plain: "─".repeat(fill),
            sgr: Some(SGR_BORDER),
        },
        Span::literal(" ", None),
    ];
    if let Some(t) = clean_tag {
        spans.push(Span {
            plain: t,
            sgr: Some(SGR_MUTED),
        });
        spans.push(Span::literal(" · ", Some(SGR_MUTED)));
    }
    let start = spans_width(&spans) + 1;
    let end = start + action_w - 1;
    spans.push(Span::literal(action, Some(SGR_GOLD_BOLD)));
    spans.push(Span::literal(" ─", Some(SGR_BORDER)));
    spans.push(Span::literal("╮", Some(SGR_BORDER)));
    let mut out = String::new();
    emit_spans(&mut out, &spans, colored, None);
    (out, Some((start, end)))
}

fn card_row_lr(
    left: Vec<Span>,
    right: Vec<Span>,
    columns: usize,
    use_cards: bool,
    row_bg: Option<&'static str>,
    colored: bool,
) -> String {
    if !use_cards {
        return render_lr(left, right, columns, false, row_bg, colored);
    }
    let inner = columns.saturating_sub(4);
    let body = render_lr(left, right, inner, true, row_bg, colored);
    if colored {
        format!("{SGR_BORDER}│{SGR_RESET} {body} {SGR_BORDER}│{SGR_RESET}")
    } else {
        format!("│ {body} │")
    }
}

fn card_bottom(columns: usize, use_cards: bool, colored: bool) -> Option<String> {
    if !use_cards || columns < 2 {
        return None;
    }
    let bar = format!("╰{}╯", "─".repeat(columns - 2));
    Some(if colored {
        format!("{SGR_BORDER}{bar}{SGR_RESET}")
    } else {
        bar
    })
}

fn card_bottom_labeled(label: &str, columns: usize, colored: bool) -> String {
    if columns < 6 {
        return card_bottom(columns, true, colored).unwrap_or_default();
    }
    let budget = columns.saturating_sub(5);
    let clean_label = clip(label, budget);
    let fill = columns.saturating_sub(5 + width(&clean_label));
    let spans = vec![
        Span::literal("╰─ ", Some(SGR_BORDER)),
        Span {
            plain: clean_label,
            sgr: Some(SGR_MUTED),
        },
        Span::literal(" ", None),
        Span {
            plain: "─".repeat(fill),
            sgr: Some(SGR_BORDER),
        },
        Span::literal("╯", Some(SGR_BORDER)),
    ];
    let mut out = String::new();
    emit_spans(&mut out, &spans, colored, None);
    out
}

fn role_style(role: &str) -> (&'static str, &'static str) {
    match role {
        "main" => ("■", SGR_AMBER_BOLD),
        "reviewer" | "verifier" | "security-auditor" => ("◆", SGR_VIOLET_BOLD),
        "advisor" | "architect" | "critic" | "researcher" | "scholar" => ("◇", SGR_SKY_BOLD),
        "question" => ("?", SGR_GOLD_BOLD),
        "mail" => ("·", SGR_CYAN),
        "task" => ("!", SGR_AMBER_BOLD),
        _ => ("●", SGR_SAGE_BOLD),
    }
}

fn state_badge(state: &str) -> (String, &'static str) {
    match state {
        "working" | "running" | "starting" | "queued" => ("[⟳ run]".into(), SGR_CYAN),
        "verifying" => ("[⟳ verify]".into(), SGR_CYAN),
        "reading" => ("[⟳ reading]".into(), SGR_CYAN),
        "editing" => ("[⟳ editing]".into(), SGR_CYAN),
        "generating" => ("[⟳ generating]".into(), SGR_CYAN),
        "review" => ("[⚑ review]".into(), SGR_GOLD_BOLD),
        "done" => ("[✓ exited]".into(), SGR_SAGE),
        "completed" => ("[✓ completed]".into(), SGR_SAGE),
        "accepted" => ("[✓ accepted]".into(), SGR_SAGE),
        "failed" | "guard_stop" | "orphaned" => ("[× failed]".into(), SGR_CORAL_BOLD),
        "timed_out" => ("[◷ timeout]".into(), SGR_CORAL_BOLD),
        "blocked" => ("[! blocked]".into(), SGR_AMBER_BOLD),
        "waiting_input" | "waiting_approval" => ("[! wait]".into(), SGR_AMBER_BOLD),
        "" => ("[· -]".into(), SGR_MUTED),
        other => (format!("[· {}]", clean(other, 14)), SGR_MUTED),
    }
}

/// Current verb from a provider's `activity` text. Only a fixed allowlist
/// shows, and only while the row is live; anything else keeps the state badge.
fn active_verb(call: &Value) -> Option<&'static str> {
    if !matches!(
        text(call, "state"),
        "working" | "running" | "reading" | "editing" | "generating"
    ) {
        return None;
    }
    match text(call, "activity") {
        "Reading file" => Some("reading"),
        "Searching" => Some("searching"),
        "Editing" => Some("editing"),
        "Running command" => Some("running"),
        "Generating" => Some("generating"),
        _ => None,
    }
}

fn call_badge(call: &Value) -> (String, &'static str) {
    if let Some(kind) = call["attention_kind"].as_str() {
        return match (kind, text(call, "attention_class")) {
            ("message", "needs") => ("[? question]".to_owned(), SGR_GOLD_BOLD),
            ("message", _) => ("[· mail]".to_owned(), SGR_CYAN),
            ("task", _) => ("[! held]".to_owned(), SGR_AMBER_BOLD),
            _ => state_badge(text(call, "state")),
        };
    }
    if call["native_assignment_transport"].is_string() {
        match text(call, "state") {
            "assigned" => {
                let label = match text(call, "native_assignment_transport") {
                    "intent" => "[· intent]",
                    "submitted" => "[· submitted]",
                    _ => "[! send unknown]",
                };
                return (label.to_owned(), SGR_MUTED);
            }
            "done" => return ("[· closed]".to_owned(), SGR_MUTED),
            _ => {}
        }
    }
    if call["returned_at"].is_string() {
        ("[✓ returned]".to_owned(), SGR_SAGE)
    } else if call["lifecycle_stale"] == true {
        ("[no events]".to_owned(), SGR_AMBER_BOLD)
    } else if let Some(verb) = active_verb(call) {
        (format!("[⟳ {verb}]"), SGR_CYAN)
    } else {
        state_badge(text(call, "state"))
    }
}

fn view_label(view: View) -> &'static str {
    match view {
        View::Auto => "Overview",
        View::Compact => "All tasks",
        View::Summary => "Summary",
        View::Detail => "Detail rows",
        View::Graph | View::Tree => "Relationships",
    }
}

pub fn calls(report: &Value) -> Vec<Value> {
    let room = &report["room"];
    let room_id = text(room, "id");
    let now = oms_panel_core::metrics::stamp(&report["generated_at"])
        .unwrap_or_else(|| oms_panel_core::timestamp() as f64);
    let mut by_attempt: BTreeMap<String, &Value> = BTreeMap::new();
    let mut by_participant: BTreeMap<String, &Value> = BTreeMap::new();
    let attempt_rows = ["recent", "active_recent", "active"]
        .into_iter()
        .flat_map(|key| rows(&report["attempts"][key]))
        .chain(
            report["room_attempts"]
                .as_object()
                .into_iter()
                .flat_map(|map| map.values()),
        );
    for row in attempt_rows {
        let aid = text(row, "attempt_id");
        if attempt_foreign_to_room(room_id, &row["refs"]) {
            if !aid.is_empty() {
                by_attempt.remove(aid);
                by_participant.retain(|_, existing| text(existing, "attempt_id") != aid);
            }
            continue;
        }
        if !aid.is_empty()
            && by_attempt
                .get(aid)
                .is_none_or(|existing| prefer_attempt(row, existing))
        {
            by_attempt.insert(aid.to_owned(), row);
        }
        let pid = text(&row["refs"], "panel_room_participant");
        if !pid.is_empty()
            && (room_id.is_empty() || event_room_matches(room_id, &row["refs"]))
            && by_participant
                .get(pid)
                .is_none_or(|existing| prefer_attempt(row, existing))
        {
            by_participant.insert(pid.to_owned(), row);
        }
    }

    let tokens_last_observed = match report["collection"]["ok"].as_bool() {
        Some(true) => {
            Some(report["collection"]["pending"] == true || report["capacity_stale"] == true)
        }
        Some(false) if report["capacity_stale"] == true => Some(true),
        _ => None,
    };
    let mut room_member_attempts = BTreeMap::new();
    if tokens_last_observed.is_some() {
        // The same Events attempt can appear in several projections. Distinct
        // attempts bound to one participant cannot prove an implicit join.
        for &row in by_attempt.values() {
            let participant = text(&row["refs"], "panel_room_participant");
            if !participant.is_empty() && event_room_matches(room_id, &row["refs"]) {
                room_member_attempts
                    .entry(participant)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(text(row, "attempt_id")));
            }
        }
    }
    let participants = rows(&room["participants"]);
    let has_participants = !participants.is_empty();
    let current_main = current_room_main_custodian(report, room, room_id);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cached_agy_model: Option<Option<String>> = None;
    for member in participants {
        let attempt = text(member, "attempt");
        let participant = text(member, "participant");
        // The exact Room projection owns this operation's lifecycle state.
        // Recent/active copies and participant aliases cannot revive it.
        let assignment = &report["room_attempts"][participant];
        let assignment_parent = native_assignment_parent(report, room, member, assignment);
        let matched = (!attempt.is_empty())
            .then(|| by_attempt.get(attempt).copied())
            .flatten()
            .filter(|row| member_matches_attempt(room_id, room, member, row))
            .or_else(|| {
                // A legacy Main's Room participant is its attempt ID.
                // Advisor work can share the participant, but not that ID.
                (member["role"] == "main")
                    .then(|| by_attempt.get(participant).copied())
                    .flatten()
                    .filter(|row| {
                        row["tool"] == "panel-main"
                            && member_matches_attempt(room_id, room, member, row)
                    })
            })
            .or_else(|| {
                (!participant.is_empty()
                    && attempt.is_empty()
                    && (member["initial_seq"].is_null() || member["initial_seq"] == member["seq"]))
                    .then(|| {
                        by_participant.get(participant).copied().or_else(|| {
                            room_id
                                .is_empty()
                                .then(|| by_attempt.get(participant).copied())
                                .flatten()
                        })
                    })
                    .flatten()
                    .filter(|row| member_matches_attempt(room_id, room, member, row))
            });
        let matched = assignment_parent.map(|_| assignment).or(matched);
        let mut call = normalize(matched.unwrap_or(member), &mut cached_agy_model);
        for key in [
            "participant",
            "parent",
            "provider",
            "model",
            "role",
            "task_id",
            "alias",
            "purpose",
            "joined",
        ] {
            if let Some(v) = member.get(key).filter(|v| !v.is_null() && *v != "") {
                call[key] = v.clone();
            }
        }
        if let Some(strategy) =
            matched.and_then(|row| attached_display_strategy(room_id, member, row))
        {
            // The Room role still controls filtering, grouping, and raw detail.
            call["display_role"] = json!(strategy);
        }
        if let (Some(last_observed), Some(row)) = (tokens_last_observed, matched)
            && by_attempt.get(text(row, "attempt_id")).copied() == Some(row)
            && attached_token_attempt(
                room,
                member,
                row,
                room_member_attempts.get(participant).copied().flatten(),
            )
        {
            record_attempt_tokens(
                &mut call,
                row,
                &report["board_receipt_tokens"][text(row, "attempt_id")],
                last_observed,
            );
        }
        if call["role"] == "main"
            && call["provider"] == "codex"
            && !text(&call, "attempt").is_empty()
        {
            // Only a Codex Main with a live attempt has a rollout to observe.
            // The stored model is launch configuration, never shown as current.
            let observed = &report["main_model_observation"];
            let fresh = report["collection"]["ok"] == true
                && report["collection"]["pending"] != true
                && report["capacity_stale"] != true;
            let state = match observed["source"].as_str() {
                Some("rollout") if fresh => "fresh",
                Some("footer") if fresh => "footer",
                Some("rollout" | "rollout-last" | "footer" | "footer-last") => "last",
                _ => "unverified",
            };
            let model = observed["model"].as_str().filter(|model| !model.is_empty());
            let verified =
                call["attempt"] == observed["attempt"] && state != "unverified" && model.is_some();
            call["model_state"] = json!(if verified { state } else { "unverified" });
            if verified {
                call["model"] = json!(model);
            }
        }
        let override_title = text(member, "label_override");
        if !override_title.is_empty() {
            call["title"] = json!(override_title);
        } else if text(&call, "title").is_empty()
            && let Some(fallback) = [text(member, "title"), text(member, "label")]
                .into_iter()
                .find(|v| !v.is_empty())
        {
            call["title"] = json!(fallback);
        }
        if (matched.is_none() || text(&call, "state").is_empty())
            && !text(member, "state").is_empty()
        {
            call["state"] = member["state"].clone();
        }
        if let Some(parent) = assignment_parent {
            let refs = &assignment["refs"];
            let task_id = text(assignment, "task_id");
            let plan = &report["plan"];
            let task = &plan["tasks"][task_id];
            let title = text(task, "title");
            if override_title.is_empty() {
                call["title"] = json!(if plan["plan_id"] == refs["plan_id"]
                    && task["id"] == task_id
                    && task["lease_id"] == refs["lease_id"]
                    && !title.is_empty()
                {
                    title
                } else {
                    task_id
                });
            }
            call["native_assignment_transport"] = refs["native_transport"].clone();
            call["display_role"] = json!("assignment");
            if assignment["terminal"] == false {
                call["state"] = json!("assigned");
            }
            if member["joined"] == false
                || parent["joined"] == false
                || room_main_id(room) != text(parent, "participant")
                || room["closed"] == true
                || !room["successor"].is_null()
                || report["room_attempts"][text(member, "parent")]["terminal"] == true
                || rows(&room["swaps"]).iter().any(|swap| {
                    swap["attempt"] == parent["participant"]
                        && swap["participant"] == parent["participant"]
                })
            {
                call["room_only_history"] = json!(true);
            }
        }
        let native_tokens = &report["main_token_observation"];
        if let Some(last_observed) = tokens_last_observed
            && let Some(total) = native_tokens["total"].as_u64()
            && matches!(
                native_tokens["source"].as_str(),
                Some("rollout" | "rollout-last")
            )
            && call["role"] == "main"
            && call["provider"] == "codex"
            && member["joined"] == true
            && !room_id.is_empty()
            && native_tokens["room"] == room_id
            && !participant.is_empty()
            && native_tokens["participant"] == participant
            && room_main_id(room) == participant
            && !text(&call, "attempt").is_empty()
            && native_tokens["attempt"] == call["attempt"]
            && oms_panel_core::state::LIVE_STATES.contains(&text(&call, "state"))
        {
            call["native_session_tokens"] = json!({
                "total":total,
                "last_observed":last_observed || native_tokens["source"] == "rollout-last"
            });
        }
        if (call["created_at"].is_null() || call["created_at"] == "")
            && !member["joined_at"].is_null()
            && member["joined_at"] != ""
        {
            call["created_at"] = member["joined_at"].clone();
        }
        if matched.is_none()
            && call["role"] != "main"
            && !classify_room_only_call(report, room, room_id, member, &mut call, now, current_main)
        {
            continue;
        }
        output.push(call);
        if !participant.is_empty() {
            seen.insert(participant.to_owned());
        }
        if !attempt.is_empty() {
            seen.insert(attempt.to_owned());
        }
        if let Some(matched_row) = matched {
            let maid = text(matched_row, "attempt_id");
            if !maid.is_empty() {
                seen.insert(maid.to_owned());
            }
        }
    }
    for (id, attempt) in by_attempt {
        if id.is_empty() || seen.contains(&id) || attempt["tool"] == "panel-native-assignment" {
            continue;
        }
        let pid = text(&attempt["refs"], "panel_room_participant");
        if !pid.is_empty() && seen.contains(pid) {
            continue;
        }
        if !room_id.is_empty()
            && (attempt_foreign_to_room(room_id, &attempt["refs"])
                || (!event_room_matches(room_id, &attempt["refs"]) && has_participants))
        {
            continue;
        }
        output.push(normalize(attempt, &mut cached_agy_model));
    }
    output.extend(
        rows(&report["native_children"])
            .iter()
            .cloned()
            .map(|mut child| {
                if let Some(fields) = child.as_object_mut() {
                    fields.remove("event_tokens");
                    fields.remove("receipt_tokens");
                    fields.remove("board_recent");
                }
                child
            }),
    );
    for call in &mut output {
        let finished = &report["returned_workers"][text(call, "attempt")];
        if call["state"] == "review"
            && call["refs"]["panel_access"] == "read"
            && oms_panel_core::metrics::stamp(finished).is_some()
        {
            call["returned_at"] = finished.clone();
        }
        if matches!(text(call, "state"), "review" | "done")
            && matches!(
                report["finalized"][text(call, "task_id")].as_str(),
                Some("completed" | "accepted")
            )
        {
            call["completed_task"] = true.into();
        }
        if let Some(observed) = oms_panel_core::metrics::stamp(&report["generated_at"]) {
            let elapsed = if call["role"] == "main" {
                main_elapsed(report, call)
            } else {
                worker_elapsed(report, call, observed)
            };
            call["board_elapsed"] = json!(elapsed.as_deref().unwrap_or("—"));
        }
    }
    let direct: BTreeSet<_> = direct_work(report, &output, overall_main(room, &output))
        .into_iter()
        .map(|(participant, attempt)| (participant.to_owned(), attempt.to_owned()))
        .collect();
    for call in &mut output {
        if let Some(fields) = call.as_object_mut() {
            fields.remove("lifecycle_stale");
        }
        if direct.contains(&(
            text(call, "participant").into(),
            text(call, "attempt").into(),
        )) && stale_lifecycle(report, &report["room_attempts"][text(call, "attempt")])
        {
            call["lifecycle_stale"] = true.into();
        }
    }
    // A current interaction outranks a verified rollout creation. Keep the
    // same three-row cap and newest-first ordering as native discovery.
    let observed = oms_panel_core::metrics::stamp(&report["generated_at"]);
    let recency = |call: &Value| codex_board_recency(report, call, observed?);
    {
        let mut recent: Vec<_> = output
            .iter_mut()
            .filter_map(|call| Some((recency(call)?, call)))
            .collect();
        recent.sort_by_key(|(recency, _)| std::cmp::Reverse(*recency));
        for (rank, (_, call)) in recent.into_iter().take(3).enumerate() {
            // Carry the stable discovery tie-break through the global sort.
            call["board_recent"] = json!(rank);
        }
    }
    output.sort_by_cached_key(|v| {
        (
            !oms_panel_core::state::LIVE_STATES.contains(&text(v, "state")),
            text(v, "role") != "main",
            text(v, "created_at").to_owned(),
            text(v, "participant").to_owned(),
        )
    });
    // Reorder only the recent Codex child slots, preserving other cohorts.
    let recent_slots: Vec<_> = output
        .iter()
        .enumerate()
        .filter_map(|(index, call)| call["board_recent"].is_u64().then_some(index))
        .collect();
    let mut recent: Vec<_> = recent_slots
        .iter()
        .map(|&index| std::mem::take(&mut output[index]))
        .collect();
    recent.sort_by_key(|call| call["board_recent"].as_u64());
    for (index, mut call) in recent_slots.into_iter().zip(recent) {
        call["board_recent"] = true.into();
        output[index] = call;
    }
    output
}

fn stale_lifecycle(report: &Value, row: &Value) -> bool {
    if report["collection"]["ok"] != true
        || report["collection"]["pending"] == true
        || report["capacity_stale"] == true
        || report["room"].get("error").is_some_and(|v| !v.is_null())
        || row["tool"] == "panel-native-assignment"
        || row["refs"]["panel_native_scheduling"] == "unmanaged"
        || ["panel_heartbeat_interval", "panel_heartbeat_ttl"]
            .iter()
            .any(|key| row["refs"].get(*key).is_some_and(|v| !v.is_string()))
        || !matches!(
            text(row, "state"),
            "starting" | "working" | "running" | "verifying"
        )
    {
        return false;
    }
    let stamp = oms_panel_core::metrics::stamp;
    let (Some(now), Some(created), Some(updated), Some(heartbeat)) = (
        stamp(&report["generated_at"]),
        stamp(&row["created_at"]),
        stamp(&row["updated_at"]),
        stamp(&row["last_heartbeat_at"]),
    ) else {
        return false;
    };
    if !(0.0 <= created && created <= heartbeat && heartbeat <= updated && updated <= now) {
        return false;
    }
    // A usage event is also current lifecycle evidence. Reuse the heartbeat
    // window without treating it as output progress or changing the receipt.
    let evidence = json!({
        "refs": row["refs"], "created_at": row["created_at"], "last_heartbeat_at": row["updated_at"]
    });
    oms_panel_core::release::heartbeat_fresh(&evidence, now).is_ok_and(|fresh| !fresh)
}

fn event_room_matches(room: &str, refs: &Value) -> bool {
    let rooms = [text(refs, "panel_room_id"), text(refs, "panel_room")];
    !room.is_empty() && rooms.contains(&room) && rooms.iter().all(|id| id.is_empty() || *id == room)
}

fn attempt_foreign_to_room(room_id: &str, refs: &Value) -> bool {
    !room_id.is_empty()
        && [text(refs, "panel_room_id"), text(refs, "panel_room")]
            .into_iter()
            .any(|id| !id.is_empty() && id != room_id)
}

fn prefer_attempt(candidate: &Value, existing: &Value) -> bool {
    let live = |row| oms_panel_core::state::LIVE_STATES.contains(&text(row, "state"));
    live(candidate) || !live(existing)
}

fn native_assignment_parent<'a>(
    report: &Value,
    room: &'a Value,
    member: &Value,
    row: &Value,
) -> Option<&'a Value> {
    let participant = text(member, "participant");
    let parent = text(member, "parent");
    let room_id = text(room, "id");
    let refs = &row["refs"];
    let hex = |value: &str, len| {
        value.len() == len
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let id = |value: &str, prefix| value.strip_prefix(prefix).is_some_and(|hash| hex(hash, 32));
    if row["tool"] != "panel-native-assignment"
        || refs["native_assignment_schema"] != "1"
        || refs["panel_native_scheduling"] != "unmanaged"
        || !id(participant, "att_")
        || row["attempt_id"] != participant
        || (!member["attempt"].is_null() && member["attempt"] != participant)
        || !member["joined"].is_boolean()
        || member["seq"].as_u64().is_none()
        || member["initial_seq"] != member["seq"]
        || member["role"] != "worker"
        || member["provider"] != "codex"
        || row["provider"] != "codex"
        || !id(parent, "att_")
        || parent == participant
        || row["parent_attempt_id"] != parent
        || text(row, "task_id").is_empty()
        || (!member["task_id"].is_null() && member["task_id"] != row["task_id"])
        || [
            ("panel_room_participant", participant),
            ("panel_room_parent", parent),
            ("panel_provider", "codex"),
            ("panel_role", "worker"),
            ("panel_room", room_id),
        ]
        .into_iter()
        .any(|(key, expected)| refs.get(key).is_some_and(|value| value != expected))
        || refs["panel_room_id"] != room_id
        || !event_room_matches(room_id, refs)
        || !id(text(refs, "plan_id"), "plan_")
        || !id(text(refs, "lease_id"), "lease_")
        || !hex(text(refs, "native_intent_sha256"), 64)
        || !hex(text(refs, "native_payload_sha256"), 64)
    {
        return None;
    }
    let terminal = oms_panel_core::state::TERMINAL_STATES.contains(&text(row, "state"));
    if (terminal && row["terminal"] != true)
        || (!terminal && (row["terminal"] != false || row["state"] != "queued"))
    {
        return None;
    }
    match text(refs, "native_transport") {
        "intent" => {}
        "sending" | "submitted" | "unknown"
            if hex(text(refs, "native_binding_sha256"), 64)
                && id(text(refs, "native_room_message"), "msg-")
                && refs["native_binding"]
                    == format!(".oms/lifecycle/attempts/{participant}/native-assignment.json") => {}
        _ => return None,
    }
    let main = &report["room_attempts"][parent];
    let members = rows(&room["participants"]);
    let mut parents = members
        .iter()
        .filter(|member| member["participant"] == parent);
    let parent_member = parents.next()?;
    let provider_matches = parent_member["provider"] == main["provider"]
        || rows(&room["swaps"]).iter().any(|swap| {
            swap["attempt"] == parent
                && swap["participant"] == parent
                && swap["from"] == main["provider"]
        });
    (main["attempt_id"] == parent
        && main["tool"] == "panel-main"
        && main["refs"]["panel_role"] == "main"
        && main["refs"]["panel_native_scheduling"] == "unmanaged"
        && event_room_matches(room_id, &main["refs"])
        && oms_panel_core::state::main_participant(main) == parent
        && !text(main, "provider").is_empty()
        && refs["panel_owner"] == main["provider"]
        && provider_matches
        && parent_member["role"] == "main"
        && parent_member["joined"].is_boolean()
        && text(parent_member, "parent").is_empty()
        && parents.next().is_none()
        && members
            .iter()
            .filter(|member| member["participant"] == participant)
            .count()
            == 1)
        .then_some(parent_member)
}

fn member_matches_attempt(room_id: &str, room: &Value, member: &Value, row: &Value) -> bool {
    let refs = &row["refs"];
    if row["tool"] == "panel-native-assignment" || attempt_foreign_to_room(room_id, refs) {
        return false;
    }
    let parent = text(member, "parent");
    let attempt_id = text(row, "attempt_id");
    if !parent.is_empty() && attempt_id == parent {
        return false;
    }
    let member_is_main = member["role"] == "main";
    let roles = [text(row, "role"), text(refs, "panel_role")];
    let event_is_main = row["tool"] == "panel-main" || roles.contains(&"main");
    if member_is_main != event_is_main
        || (member_is_main && roles.iter().any(|r| !r.is_empty() && *r != "main"))
    {
        return false;
    }
    let provider = text(member, "provider");
    if !provider.is_empty()
        && [text(row, "provider"), text(refs, "panel_provider")]
            .into_iter()
            .any(|id| !id.is_empty() && id != provider)
    {
        return false;
    }
    let participant = text(member, "participant");
    let event_participant = text(refs, "panel_room_participant");
    if !participant.is_empty() && !event_participant.is_empty() && event_participant != participant
    {
        return false;
    }
    if room_id.is_empty() {
        return true;
    }
    let member_attempt = text(member, "attempt");
    event_room_matches(room_id, refs)
        || (!member_attempt.is_empty()
            && attempt_id == member_attempt
            && [text(refs, "panel_room_id"), text(refs, "panel_room")] == ["", ""])
        || (member_is_main
            && row["tool"] == "panel-main"
            && event_participant.is_empty()
            && oms_panel_core::state::main_participant(row) == participant
            && room_main_id(room) == participant)
}

fn current_room_main_custodian<'a>(
    report: &'a Value,
    room: &'a Value,
    room_id: &str,
) -> Option<(&'a str, &'a str)> {
    if room_id.is_empty() || room["closed"] == true || !room["successor"].is_null() {
        return None;
    }
    let owner = room_main_id(room);
    if owner.is_empty() {
        return None;
    }
    let mut main_members = rows(&room["participants"]).iter().filter(|member| {
        member["role"] == "main" && member["participant"] == owner && member["joined"] == true
    });
    let main_member = main_members.next()?;
    if main_members.next().is_some() {
        return None;
    }
    let member_attempt = text(main_member, "attempt");
    let main_attempt = if !member_attempt.is_empty() {
        member_attempt
    } else {
        let mut found: Option<&str> = None;
        for row in report["room_attempts"].as_object()?.values() {
            let aid = text(row, "attempt_id");
            if aid.is_empty()
                || row["tool"] != "panel-main"
                || row["terminal"] != false
                || !oms_panel_core::state::LIVE_STATES.contains(&text(row, "state"))
                || !event_room_matches(room_id, &row["refs"])
                || oms_panel_core::state::main_participant(row) != owner
            {
                continue;
            }
            match found {
                None => found = Some(aid),
                Some(prev) if prev == aid => {}
                Some(_) => return None,
            }
        }
        found?
    };
    let attempt = &report["room_attempts"][main_attempt];
    if attempt["attempt_id"] != main_attempt
        || attempt["tool"] != "panel-main"
        || attempt["terminal"] != false
        || !oms_panel_core::state::LIVE_STATES.contains(&text(attempt, "state"))
        || attempt["provider"] != main_member["provider"]
        || !event_room_matches(room_id, &attempt["refs"])
        || !(oms_panel_core::state::main_participant(attempt) == owner
            || (text(&attempt["refs"], "panel_room_participant").is_empty()
                && member_attempt == main_attempt))
        || rows(&room["swaps"])
            .iter()
            .any(|swap| swap["participant"] == owner && swap["attempt"] == main_attempt)
    {
        return None;
    }
    Some((owner, main_attempt))
}

/// Enrich and classify a non-main Room member lacking a matched lifecycle attempt
/// without fabricating live execution state, tokens, or elapsed time.
fn classify_room_only_call(
    report: &Value,
    room: &Value,
    room_id: &str,
    member: &Value,
    call: &mut Value,
    now: f64,
    current_main: Option<(&str, &str)>,
) -> bool {
    let participant = text(member, "participant");
    let dispatch = (!room_id.is_empty() && !participant.is_empty())
        .then(|| &report["room_dispatches"][participant])
        .filter(|v| v.is_object() && v["room"] == room_id);
    let has_call_result = !participant.is_empty() && room["call_results"][participant].is_object();
    if let Some(d) = dispatch {
        if text(call, "task_id").is_empty() && !text(d, "task_id").is_empty() {
            call["task_id"] = d["task_id"].clone();
        }
        if text(call, "purpose").is_empty() && !text(&d["route"], "purpose").is_empty() {
            call["purpose"] = d["route"]["purpose"].clone();
        }
        if text(call, "model").is_empty() && !text(&d["route"], "model").is_empty() {
            call["model"] = d["route"]["model"].clone();
        }
        let route_role = text(&d["route"], "role");
        if matches!(text(call, "role"), "worker" | "reviewer")
            && oms_panel_core::routing::kind(route_role).ok() == Some(text(call, "role"))
        {
            call["display_role"] = json!(route_role);
        }
        if (call["created_at"].is_null() || call["created_at"] == "")
            && !d["created_at"].is_null()
            && d["created_at"] != ""
        {
            call["created_at"] = d["created_at"].clone();
        }
    }
    let finalized_done = !text(call, "task_id").is_empty()
        && matches!(
            report["finalized"][text(call, "task_id")].as_str(),
            Some("completed" | "accepted")
        );
    let dispatch_state = dispatch.map_or("", |d| text(d, "state"));
    let dispatch_access = dispatch.map_or("", |d| text(&d["route"], "access"));
    let dispatch_exit = dispatch.and_then(|d| d["exit"].as_i64());
    let stale_dispatch_custodian = dispatch.is_some_and(|d| {
        current_main.is_none_or(|(owner, main_attempt)| {
            text(d, "main_attempt") != main_attempt || text(d, "main") != owner
        })
    });
    let raw_state = text(call, "state").to_owned();
    let has_open_question = !participant.is_empty()
        && rows(&room["open_questions"])
            .iter()
            .any(|q| q["sender"] == participant || q["recipient"] == participant);
    let stamp = oms_panel_core::metrics::stamp(&call["created_at"]);
    let clear_queued = |call: &mut Value| {
        if call["state"] == "queued" {
            call["state"] = json!("");
        }
    };
    if member["joined"] == false {
        let base_left_history = has_call_result
            || matches!(dispatch_state, "settled" | "awaiting")
            || oms_panel_core::state::TERMINAL_STATES.contains(&raw_state.as_str());
        if !base_left_history && raw_state.is_empty() {
            return false;
        }
        if base_left_history {
            clear_queued(call);
        }
        call["room_only_history"] = true.into();
        return true;
    }

    // Step A: preserve exact baseline raw "queued" normalization for --sessions parity.
    let base_history = has_call_result
        || dispatch_state == "settled"
        || finalized_done
        || (dispatch_state == "awaiting"
            && (dispatch_access != "write" || dispatch_exit != Some(0)));
    if base_history {
        clear_queued(call);
    } else if matches!(raw_state.as_str(), "" | "queued") {
        let base_fresh_300 = stamp.is_none_or(|ts| ts <= now + 5.0 && now - ts < 300.0);
        let base_actionable = (dispatch_state == "awaiting"
            && dispatch_access == "write"
            && dispatch_exit == Some(0)
            && !finalized_done
            && base_fresh_300)
            || (matches!(dispatch_state, "reserved" | "running" | "unknown") && base_fresh_300)
            || has_open_question
            || (raw_state.is_empty()
                && stamp.is_some_and(|ts| ts <= now + 5.0 && now - ts < 120.0))
            || (raw_state == "queued" && base_fresh_300);
        if !base_actionable {
            clear_queued(call);
        }
    }

    // Step B: compute Board-only display flags without mutating call["state"].
    if base_history || stale_dispatch_custodian {
        call["room_only_history"] = true.into();
    } else if matches!(raw_state.as_str(), "" | "queued" | "unknown") {
        let fresh_300 = stamp.is_some_and(|ts| ts <= now + 5.0 && now - ts < 300.0);
        let room_open = room["closed"] != true && room["successor"].is_null();
        let board_actionable = (current_main.is_some()
            && ((dispatch_state == "awaiting"
                && dispatch_access == "write"
                && dispatch_exit == Some(0)
                && !finalized_done
                && fresh_300)
                || (matches!(dispatch_state, "reserved" | "running" | "unknown") && fresh_300)
                || (raw_state.is_empty()
                    && stamp.is_some_and(|ts| ts <= now + 5.0 && now - ts < 120.0))
                || (raw_state == "queued" && fresh_300)))
            || (room_open && has_open_question);
        if !board_actionable {
            call["room_only_stale"] = true.into();
        }
    }
    true
}

fn attached_token_attempt(
    room: &Value,
    member: &Value,
    row: &Value,
    unique_attempt: Option<&str>,
) -> bool {
    let participant = text(member, "participant");
    let attempt = text(row, "attempt_id");
    let provider = text(member, "provider");
    let refs = &row["refs"];
    let event_participant = text(refs, "panel_room_participant");
    let member_is_main = member["role"] == "main";
    let roles = [text(row, "role"), text(refs, "panel_role")];
    let event_is_main = row["tool"] == "panel-main" || roles.contains(&"main");
    let legacy_main = member_is_main
        && row["tool"] == "panel-main"
        && event_participant.is_empty()
        && oms_panel_core::state::main_participant(row) == participant;
    if participant.is_empty()
        || attempt.is_empty()
        || attempt == text(member, "parent")
        || !event_room_matches(text(room, "id"), refs)
        || (event_participant != participant && !legacy_main)
        || member_is_main != event_is_main
        || (member_is_main && roles.iter().any(|role| !role.is_empty() && *role != "main"))
        || (!provider.is_empty()
            && [text(row, "provider"), text(refs, "panel_provider")]
                .iter()
                .any(|id| !id.is_empty() && *id != provider))
    {
        return false;
    }
    let member_attempt = text(member, "attempt");
    if !member_attempt.is_empty() {
        return member_attempt == attempt;
    }
    // Room carry can preserve an older result and its sequence across enrollment.
    if !member["initial_seq"].is_null() && member["initial_seq"] != member["seq"] {
        return false;
    }
    if member["role"] == "main"
        && rows(&room["swaps"])
            .iter()
            .any(|swap| swap["participant"] == participant && swap["attempt"] == attempt)
    {
        return false;
    }
    let result_attempt = text(&room["call_results"][participant], "attempt_id");
    if !result_attempt.is_empty() {
        return result_attempt == attempt;
    }
    legacy_main || unique_attempt == Some(attempt)
}

fn record_attempt_tokens(call: &mut Value, row: &Value, receipts: &Value, last_observed: bool) {
    if receipts["calls"].as_u64().is_some_and(|calls| calls > 0) {
        let mut receipts = receipts.clone();
        receipts["last_observed"] = last_observed.into();
        call["receipt_tokens"] = receipts;
    }
    if row["usage_reports"]["tokens"]
        .as_u64()
        .is_some_and(|count| count > 0)
        && let Some(total) = row["usage"]["tokens"].as_u64()
    {
        call["event_tokens"] = json!({"total": total, "last_observed": last_observed});
    }
}

fn attached_display_strategy<'a>(room: &str, member: &Value, row: &'a Value) -> Option<&'a str> {
    let participant = text(member, "participant");
    let attempt = text(member, "attempt");
    let provider = text(member, "provider");
    let refs = &row["refs"];
    let providers = [text(row, "provider"), text(refs, "panel_provider")];
    let strategy = text(refs, "panel_strategy");
    let role = text(member, "role");
    (matches!(role, "worker" | "reviewer")
        && refs["panel_role"] == role
        && !participant.is_empty()
        && refs["panel_room_participant"] == participant
        && !text(row, "attempt_id").is_empty()
        && (attempt.is_empty() || row["attempt_id"] == attempt)
        && event_room_matches(room, refs)
        && (provider.is_empty() || providers.iter().all(|id| id.is_empty() || *id == provider))
        && oms_panel_core::routing::kind(strategy).ok() == Some(role))
    .then_some(strategy)
}

fn normalize(row: &Value, cached_agy_model: &mut Option<Option<String>>) -> Value {
    let refs = &row["refs"];
    let choose = |key, reference| {
        row[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .or_else(|| refs[reference].as_str().filter(|v| !v.is_empty()))
            .unwrap_or("")
            .to_owned()
    };
    let attempt = text(row, "attempt_id");
    let participant = refs["panel_room_participant"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| row["participant"].as_str().filter(|v| !v.is_empty()))
        .unwrap_or(attempt);
    let mut role = choose("role", "panel_role");
    if role.is_empty() {
        role = text(row, "tool").to_owned();
    }
    let title = [
        text(row, "label_override"),
        text(row, "title"),
        text(row, "label"),
        text(refs, "panel_label"),
    ]
    .into_iter()
    .find(|v| !v.is_empty())
    .unwrap_or("");
    let mut model = choose("model", "panel_model");
    let provider = choose("provider", "panel_provider");
    let terminal = matches!(
        text(row, "state"),
        "done" | "failed" | "cancelled" | "timed_out" | "abandoned" | "guard_stop"
    );
    if model.is_empty() && role == "main" && provider == "antigravity" && !terminal {
        model = cached_agy_model
            .get_or_insert_with(crate::native_panel::detect_antigravity_model)
            .clone()
            .unwrap_or_default();
    }
    json!({"participant":participant,"attempt":attempt,"tool":text(row,"tool"),"task_id":text(row,"task_id"),
        "role":role,"provider":provider,
        "model":model,"purpose":choose("purpose","panel_purpose"),
        "alias":choose("alias","panel_alias"),"title":title,
        "parent":refs["panel_room_parent"].as_str().unwrap_or(text(row,"parent_attempt_id")),
        "state":row["state"],"ending":row["reason_code"],"updated_at":row["updated_at"],"created_at":row["created_at"],
        "started_at":row["created_at"],"ended_at":row["ended_at"],
        "duration_ms":row["usage_reports"]["duration_ms"].as_u64().filter(|count| *count > 0)
            .and_then(|_| row["usage"]["duration_ms"].as_f64()),"refs":refs})
}

pub fn visible(report: &Value, nav: &navigation::Navigation) -> Vec<Value> {
    let mut all = calls(report);
    let main_id = overall_main(&report["room"], &all).map(|call| {
        (
            text(call, "participant").to_owned(),
            text(call, "attempt").to_owned(),
        )
    });
    let attention = if nav.attention_only {
        attention::items(report, &BTreeMap::new())
    } else {
        Vec::new()
    };
    if nav.attention_only {
        all.extend(attention_extra_rows(report, &all, &attention));
    }
    let main = main_id.as_ref().and_then(|(participant, attempt)| {
        all.iter().find(|call| {
            call["role"] == "main"
                && text(call, "participant") == participant
                && text(call, "attempt") == attempt
        })
    });
    let direct = direct_work(report, &all, main);
    let attention_calls = attention_call_ids(&attention);
    main.into_iter()
        .chain(
            board_rows(
                visible_from(&all, nav, main, &direct, &attention_calls),
                nav,
                main,
                &all,
            )
            .into_iter()
            .map(|entry| entry.call),
        )
        .cloned()
        .collect()
}

fn room_main_id(room: &Value) -> &str {
    room["owner"]
        .as_str()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| text(room, "main"))
}

/// The Board's pinned Overall Main, not an earlier or competing main-role row.
pub(crate) fn pinned_main(report: &Value) -> Option<Value> {
    overall_main(&report["room"], &calls(report)).cloned()
}

pub(crate) fn is_overall_main(report: &Value, call: &Value) -> bool {
    // Other roles skip the projection: the pinned Main is always main-role.
    call["role"] == "main"
        && pinned_main(report).is_some_and(|main| {
            main["participant"] == call["participant"] && main["attempt"] == call["attempt"]
        })
}

fn overall_main<'a>(room: &Value, calls: &'a [Value]) -> Option<&'a Value> {
    let mut mains = calls.iter().filter(|call| {
        call["role"] == "main" && call["joined"] != false && !text(call, "participant").is_empty()
    });
    let id = room_main_id(room);
    if !id.is_empty() {
        if !text(room, "owner").is_empty() {
            return mains.find(|call| call["participant"] == id);
        }
        return mains
            .clone()
            .find(|call| call["participant"] == id)
            .or_else(|| mains.find(|call| call["attempt"] == id));
    }
    let mut active =
        mains.filter(|call| oms_panel_core::state::LIVE_STATES.contains(&text(call, "state")));
    active.next().filter(|_| active.next().is_none())
}

fn direct_work<'a>(
    report: &Value,
    calls: &'a [Value],
    main: Option<&Value>,
) -> BTreeSet<(&'a str, &'a str)> {
    let room = &report["room"];
    let room_id = text(room, "id");
    let Some(main) = main else {
        return BTreeSet::new();
    };
    let owner = room_main_id(room);
    let main_attempt = text(main, "attempt");
    let attempt = &report["room_attempts"][main_attempt];
    if owner.is_empty()
        || room["closed"] == true
        || main["participant"] != owner
        || main["joined"] != true
        || main_attempt.is_empty()
        || !oms_panel_core::state::LIVE_STATES.contains(&text(main, "state"))
        || attempt["attempt_id"] != main_attempt
        || attempt["tool"] != "panel-main"
        || attempt["terminal"] != false
        || !oms_panel_core::state::LIVE_STATES.contains(&text(attempt, "state"))
        || attempt["provider"] != main["provider"]
        || oms_panel_core::state::main_participant(attempt) != owner
        || !event_room_matches(room_id, &attempt["refs"])
        || calls.iter().any(|call| {
            !std::ptr::eq(call, main)
                && ["participant", "attempt"]
                    .iter()
                    .any(|key| [owner, main_attempt].contains(&text(call, key)))
        })
        || rows(&room["swaps"])
            .iter()
            .any(|swap| swap["participant"] == owner && swap["attempt"] == main_attempt)
    {
        return BTreeSet::new();
    }
    // Room membership proves the direct relationship. Events distinguishes
    // attempts that share a participant after a Main restart or resume.
    let members: BTreeSet<_> = rows(&room["participants"])
        .iter()
        .filter(|member| member["joined"] == true && member["parent"] == owner)
        .map(|member| (text(member, "participant"), text(member, "attempt")))
        .collect();
    calls
        .iter()
        .filter(|call| {
            matches!(
                text(call, "role"),
                "worker" | "reviewer" | "researcher" | "advisor"
            ) && call["native_child"] != true
                && call["joined"] == true
                && call["completed_task"] != true
                && !call["returned_at"].is_string()
                && oms_panel_core::state::LIVE_STATES.contains(&text(call, "state"))
        })
        .filter_map(|call| {
            let participant = text(call, "participant");
            let attempt = text(call, "attempt");
            let row = &report["room_attempts"][attempt];
            let refs = &row["refs"];
            (!participant.is_empty()
                && !attempt.is_empty()
                && (members.contains(&(participant, attempt))
                    || members.contains(&(participant, "")))
                && row["attempt_id"] == attempt
                && row["parent_attempt_id"] == main_attempt
                && row["terminal"] == false
                && oms_panel_core::state::LIVE_STATES.contains(&text(row, "state"))
                && row["provider"] == call["provider"]
                && refs["panel_room_participant"] == participant
                && event_room_matches(room_id, refs))
            .then_some((participant, attempt))
        })
        .collect()
}

fn is_direct_work(call: &Value, direct: &BTreeSet<(&str, &str)>) -> bool {
    call["native_child"] != true
        && direct.contains(&(text(call, "participant"), text(call, "attempt")))
}

fn provider_bucket(call: &Value) -> usize {
    match text(call, "provider") {
        "codex" => 0,
        "claude" => 1,
        "antigravity" | "agy" => 2,
        _ => 3,
    }
}

fn provider_style(group: usize) -> &'static str {
    [
        "\x1b[1;38;5;223m",
        "\x1b[1;38;5;174m",
        SGR_SKY_BOLD,
        SGR_BOLD,
    ][group]
}

fn provider_label(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude",
        "antigravity" | "agy" => "Antigravity",
        "pi" => "Pi",
        _ => provider,
    }
}

fn formatted_model(model: &str) -> &str {
    if model.eq_ignore_ascii_case("gpt-6.2-sol") {
        "GPT-6.2-Sol"
    } else {
        model
    }
}

/// Main-row model states: fresh is bare; last known takes the token field's
/// `~` prefix; unverified (no matching observation) keeps the configured
/// name with a `?` suffix, or `—` (the unknown mark) when none is stored.
fn model_label(call: &Value) -> String {
    let model = formatted_model(text(call, "model"));
    match (call["model_state"].as_str(), model.is_empty()) {
        (Some("fresh" | "footer"), false) | (None, false) => model.to_owned(),
        (Some("last"), false) => format!("~{model}"),
        (Some(_), false) => format!("{model}?"),
        (Some("unverified"), true) => "—".to_owned(),
        _ => "model —".to_owned(),
    }
}

fn child_model_label(call: &Value) -> String {
    let observed = text(call, "observed_model").trim();
    let sanitized = clean(observed, 1_048_576);
    let model = sanitized.trim();
    if model != observed
        || [
            "",
            "—",
            "unknown",
            "unverified",
            "inherit",
            "default",
            "provider-default",
        ]
        .iter()
        .any(|unknown| model.eq_ignore_ascii_case(unknown))
    {
        "—".to_owned()
    } else {
        formatted_model(model).to_owned()
    }
}

/// Detail view: names the provenance instead of a bare marker.
fn model_detail(call: &Value) -> String {
    if uses_observed_model(call) {
        return child_model_label(call);
    }
    let model = text(call, "model");
    match call["model_state"].as_str() {
        Some("fresh") | None => model.to_owned(),
        Some("footer") => format!("{model} · observed in pane"),
        Some("last") => format!("{model} · last known"),
        Some(_) if model.is_empty() => "unverified".to_owned(),
        Some(_) => format!("{model} · configured, unverified"),
    }
}

fn uses_observed_model(call: &Value) -> bool {
    call["native_child"] == true
        || (call["tool"] != "panel-native-assignment"
            && !matches!(
                text(call, "role"),
                "main" | "lead" | "team_lead" | "orchestrator"
            )
            && !text(call, "parent").is_empty())
}

fn headline_tokens(call: &Value) -> (&Value, &'static str) {
    let native = &call["native_session_tokens"];
    let receipts = &call["receipt_tokens"];
    if native["total"].is_u64() {
        (native, "Session incl. cache")
    } else if receipts["total"].is_u64() {
        (receipts, "Receipts")
    } else {
        (&call["event_tokens"], "Events")
    }
}

/// Right-aligned 6-cell total: `     0`, `   999`, `  1.0K`, ` 12.5K`, `300.0K`,
/// `  1.2M`. Every K/M/B value keeps one decimal and is rounded before the unit
/// is chosen, so 999_950 is `  1.0M` and never `1000.0K`. Totals of 999.95B or
/// more cannot fit, so they saturate to ` >999B` rather than widen the field.
fn compact_tokens(total: u64) -> String {
    if total < 1_000 {
        return format!("{total:>6}");
    }
    for (scale, unit) in [(1_000u64, 'K'), (1_000_000, 'M'), (1_000_000_000, 'B')] {
        let step = scale / 10;
        let tenths = total.saturating_add(step / 2) / step;
        if tenths < 10_000 {
            return format!("{:>5}{unit}", format!("{}.{}", tenths / 10, tenths % 10));
        }
    }
    " >999B".to_owned()
}

/// Last-response producer keys and labels in display priority: Codex and AGY
/// write `cached_input`, Claude `cache_read` and `cache_creation`.
const RESPONSE_COUNTERS: [(&str, &str); 8] = [
    ("total_tokens", "total"),
    ("input_tokens", "in"),
    ("output_tokens", "out"),
    ("cached_input_tokens", "cache read"),
    ("cache_read_input_tokens", "cache read"),
    ("cache_creation_input_tokens", "cache write"),
    ("cache_write_input_tokens", "cache write"),
    ("reasoning_output_tokens", "reasoning"),
];

/// Counters of one native child's last verified response, ordered as
/// `RESPONSE_COUNTERS`. Lead, turn and cumulative counters are never read here.
fn native_response(call: &Value) -> Option<[Option<u64>; 8]> {
    let response = &call["native_response_usage"]["response"];
    (call["native_child"] == true && response.is_object())
        .then(|| RESPONSE_COUNTERS.map(|(key, _)| response[key].as_u64()))
}

/// Token field: a fixed `~` slot (space unless last-observed) before the
/// 6-cell number keeps every row's last digit on one column. Unknown is `—`.
/// A native child without another total shows its last response total with
/// `rsp` in place of `tok` (marker `r` when no unit fits).
fn token_field(call: &Value, columns: usize) -> (String, String, &'static str) {
    let (observed, _) = headline_tokens(call);
    let response = observed["total"]
        .as_u64()
        .is_none()
        .then(|| native_response(call).and_then(|counts| counts[0]))
        .flatten();
    let Some(total) = observed["total"].as_u64().or(response) else {
        return (format!("{:>7}", "—"), "— ".to_owned(), " tok");
    };
    let number = compact_tokens(total);
    let unit = if response.is_some() && columns >= 56 {
        " rsp"
    } else {
        " tok"
    };
    let marker = if response.is_some() {
        if columns >= 56 { "" } else { "r" }
    } else if observed["last_observed"] == true {
        "~"
    } else {
        ""
    };
    // The trimmed form keeps the old width, including the unit-space after
    // plain integers, so narrow rows spend exactly what they did before.
    let unit_space = if total < 1_000 { " " } else { "" };
    (
        format!("{marker:>1}{number}"),
        format!("{marker}{}{unit_space}", number.trim_start()),
        unit,
    )
}

fn token_span(call: &Value, limit: usize, columns: usize, boxed: bool) -> Vec<Span> {
    token_span_with(call, limit, columns, boxed, true)
}

/// `placeholder` false drops an unknown total's `— tok` so a narrow row can
/// keep its operation badge.
fn token_span_with(
    call: &Value,
    limit: usize,
    columns: usize,
    boxed: bool,
    placeholder: bool,
) -> Vec<Span> {
    if call["attention_kind"].is_string() {
        let elapsed = text(call, "board_elapsed");
        if columns >= 44 && !elapsed.is_empty() && elapsed != "—" && width(elapsed) <= limit {
            return vec![Span::raw(elapsed, limit, Some(SGR_MUTED))];
        }
        return Vec::new();
    }
    // Narrow fallback: when the aligned field does not fit, drop the padding
    // (`~12.5K`, `0 `, `— `) rather than the value.
    let (aligned, trimmed, unit) = token_field(call, columns);
    let unit = if columns >= 56 { unit } else { "" };
    let unknown = trimmed.starts_with('—');
    // The unboxed Main on a short board shares the boxed call rows' right edge.
    let edge = if columns >= 44 && !boxed { "  " } else { "" };
    let elapsed = text(call, "board_elapsed");
    let elapsed =
        (columns >= 56 && !elapsed.is_empty()).then(|| format!("  {:>9}", clip(elapsed, 9)));
    // Counters without a total show separately; no total is invented.
    let split = (unknown && columns >= 80)
        .then(|| native_response(call))
        .flatten()
        .filter(|counts| counts[0].is_none())
        .map(|[_, input, output, ..]| {
            let part = |label: &str, count: Option<u64>| {
                count.map(|n| format!("{label} {}", compact_tokens(n).trim_start()))
            };
            ["rsp".to_owned()]
                .into_iter()
                .chain(part("in", input))
                .chain(part("out", output))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|label| label != "rsp");
    let aligned = (columns >= 44).then_some(aligned.as_str());
    let fields = [aligned, Some(trimmed.as_str())];
    let extras: Vec<String> = [
        split
            .as_ref()
            .map(|io| format!("  {io}{}", elapsed.as_deref().unwrap_or(""))),
        elapsed.clone(),
        Some(String::new()),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Prefer the aligned field with elapsed time, then save space without
    // consuming the caller's reserved identity cells.
    let value = extras.iter().find_map(|extra| {
        fields.into_iter().flatten().find_map(|field| {
            let value = if unknown && !placeholder {
                format!("{}{edge}", extra.trim_start())
            } else {
                format!("{field}{unit}{extra}{edge}")
            };
            (width(&value) <= limit).then_some(value)
        })
    });
    value
        .map(|value| Span::raw(value, limit, Some(SGR_MUTED)))
        .into_iter()
        .collect()
}

fn provider_groups(calls: &[Value], main: Option<&Value>) -> Vec<usize> {
    let preferred = main.map_or(0, provider_bucket);
    let mut groups = vec![0, 1, 2];
    if calls.iter().any(|call| provider_bucket(call) == 3) {
        groups.push(3);
    }
    groups.sort_by_key(|group| (*group != preferred, *group));
    groups
}

fn default_overview(nav: &navigation::Navigation) -> bool {
    nav.view == View::Auto && nav.filter.is_empty() && !nav.attention_only
}

fn awaiting_review(call: &Value) -> bool {
    call["state"] == "review"
        && !call["returned_at"].is_string()
        && call["completed_task"] != true
        && call["room_only_history"] != true
        && call["room_only_stale"] != true
}

fn fold_inactive(nav: &navigation::Navigation, call: &Value) -> bool {
    if !default_overview(nav) {
        return false;
    }
    let stub = call["native_child"] == true
        && call["provider"] == "codex"
        && call["state"] == "unknown"
        && call["board_recent"] != true;
    stub || call["room_only_stale"] == true
        || (awaiting_review(call)
            && oms_panel_core::metrics::stamp(&call["updated_at"])
                .is_some_and(|updated| oms_panel_core::timestamp() as f64 - updated >= 300.0))
}

fn collapsed_history(nav: &navigation::Navigation, call: &Value) -> bool {
    if !default_overview(nav) {
        return false;
    }
    let state = text(call, "state");
    // A write awaiting admission uses "review", or keeps its terminal grace
    // period if the provider already called it done without an OMS outcome.
    call["room_only_history"] == true
        || call["completed_task"] == true
        || call["returned_at"].is_string()
        || (state == "done" && call["refs"]["panel_access"] != "write")
        || (oms_panel_core::state::TERMINAL_STATES.contains(&state)
            && oms_panel_core::metrics::stamp(&call["updated_at"])
                .is_none_or(|finished| (oms_panel_core::timestamp() as f64 - finished) >= 300.0))
}

fn attention_call_ids(items: &[attention::Item]) -> BTreeSet<&str> {
    items
        .iter()
        .filter(|item| item.row_id.0 == "call")
        .map(|item| item.row_id.1.as_str())
        .collect()
}

fn attention_item_age(
    report: &Value,
    calls: &[Value],
    item: &attention::Item,
    now: f64,
) -> Option<String> {
    let parse_str = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        oms_panel_core::metrics::stamp(&Value::String(trimmed.to_owned())).or_else(|| {
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
        })
    };
    let ts = parse_str(&item.stamp).or_else(|| match item.row_id.0.as_str() {
        "call" => calls
            .iter()
            .find(|call| text(call, "participant") == item.row_id.1)
            .and_then(|call| oms_panel_core::metrics::stamp(&call["updated_at"])),
        "message" => rows(&report["room"]["messages"])
            .iter()
            .find(|msg| text(msg, "id") == item.row_id.1)
            .and_then(|msg| oms_panel_core::metrics::stamp(&msg["ts"])),
        "task" => match &report["plan"]["tasks"] {
            Value::Object(map) => map
                .get(&item.row_id.1)
                .or_else(|| {
                    map.values().find(|task| {
                        text(task, "id") == item.row_id.1 || text(task, "task_id") == item.row_id.1
                    })
                })
                .and_then(|task| {
                    oms_panel_core::metrics::stamp(&task["claimed_at"])
                        .or_else(|| oms_panel_core::metrics::stamp(&task["updated"]))
                }),
            Value::Array(list) => list
                .iter()
                .find(|task| {
                    text(task, "id") == item.row_id.1 || text(task, "task_id") == item.row_id.1
                })
                .and_then(|task| {
                    oms_panel_core::metrics::stamp(&task["claimed_at"])
                        .or_else(|| oms_panel_core::metrics::stamp(&task["updated"]))
                }),
            _ => None,
        },
        _ => None,
    })?;
    if !ts.is_finite() || ts <= 0.0 || ts > now {
        return None;
    }
    Some(crate::usage::format_age((now - ts).max(0.0)))
}

fn attention_extra_rows(report: &Value, all: &[Value], items: &[attention::Item]) -> Vec<Value> {
    let now = oms_panel_core::metrics::stamp(&report["generated_at"])
        .unwrap_or_else(|| oms_panel_core::timestamp() as f64);
    let mut rows_out = Vec::new();
    for item in items {
        let elapsed = attention_item_age(report, all, item, now).unwrap_or_default();
        match item.row_id.0.as_str() {
            "message" => {
                let is_question = item.cls == "needs";
                let title = if item.who.is_empty() {
                    item.what.clone()
                } else if item.what.is_empty() {
                    item.who.clone()
                } else {
                    format!("{}: {}", item.who, item.what)
                };
                rows_out.push(json!({
                    "participant": format!("message:{}", item.row_id.1),
                    "attempt": "",
                    "alias": if is_question { "Q" } else { "M" },
                    "role": if is_question { "question" } else { "mail" },
                    "provider": "room",
                    "state": if is_question { "waiting_input" } else { "mail" },
                    "title": title,
                    "purpose": item.what,
                    "updated_at": item.stamp,
                    "board_elapsed": elapsed,
                    "attention_kind": "message",
                    "attention_class": item.cls,
                    "attention_who": item.who,
                    "attention_what": item.what,
                    "attention_id": item.row_id.1,
                }));
            }
            "task" => {
                let title = if item.what.is_empty() {
                    item.who.clone()
                } else {
                    item.what.clone()
                };
                rows_out.push(json!({
                    "participant": format!("task:{}", item.row_id.1),
                    "attempt": "",
                    "alias": item.row_id.1,
                    "role": "task",
                    "provider": "plan",
                    "state": "held",
                    "title": title,
                    "purpose": item.what,
                    "task_id": item.row_id.1,
                    "updated_at": item.stamp,
                    "board_elapsed": elapsed,
                    "attention_kind": "task",
                    "attention_class": item.cls,
                    "attention_who": item.who,
                    "attention_what": item.what,
                    "attention_id": item.row_id.1,
                }));
            }
            _ => {}
        }
    }
    rows_out
}

/// Match a non-synthetic Room call to a task in the active plan, ignoring calls
/// explicitly stamped with a different `refs.plan_id`.
pub(crate) fn call_matches_plan_task(call: &Value, task_id: &str, plan_id: &str) -> bool {
    if call["attention_kind"].is_string() || task_id.is_empty() || text(call, "task_id") != task_id
    {
        return false;
    }
    let ref_plan = text(&call["refs"], "plan_id");
    ref_plan.is_empty() || plan_id.is_empty() || ref_plan == plan_id
}

/// Read-only dock title and body for synthetic Attention rows (`message` / `task`).
pub(crate) fn attention_row_dock(report: &Value, call: &Value) -> Option<(String, String)> {
    match call["attention_kind"].as_str()? {
        "message" => {
            let id = text(call, "attention_id");
            let msg = if id.is_empty() {
                None
            } else {
                rows(&report["room"]["messages"])
                    .iter()
                    .find(|m| text(m, "id") == id)
            };
            let is_question = text(call, "attention_class") == "needs"
                || msg.is_some_and(|m| m["message_kind"] == "question");
            let sender = msg
                .map(|m| text(m, "sender"))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| text(call, "attention_who"));
            let body = msg
                .map(|m| text(m, "text"))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| text(call, "attention_what"));
            let kind_title = if is_question {
                "Room question"
            } else {
                "Room message"
            };
            let title = if sender.is_empty() {
                kind_title.to_owned()
            } else {
                format!("{kind_title} · {sender}")
            };
            let mut lines = vec![format!(
                "{} from {}",
                if is_question { "Question" } else { "Message" },
                if sender.is_empty() {
                    "unknown sender"
                } else {
                    sender
                }
            )];
            if let Some(m) = msg {
                let pending: Vec<&str> = rows(&m["pending_for"])
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .collect();
                if !pending.is_empty() {
                    lines.push(format!("Pending for: {}", pending.join(", ")));
                }
            }
            lines.push(String::new());
            if body.trim().is_empty() {
                lines.push("Message text unavailable".to_owned());
            } else {
                lines.push(body.trim().to_owned());
            }
            Some((title, lines.join("\n")))
        }
        "task" => {
            let id = [text(call, "attention_id"), text(call, "task_id")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("");
            let task = if id.is_empty() {
                None
            } else {
                match &report["plan"]["tasks"] {
                    Value::Object(map) => map.get(id).or_else(|| {
                        map.values()
                            .find(|t| text(t, "id") == id || text(t, "task_id") == id)
                    }),
                    Value::Array(list) => list
                        .iter()
                        .find(|t| text(t, "id") == id || text(t, "task_id") == id),
                    _ => None,
                }
            };
            let state = task
                .map(plan_task_state)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown");
            let task_title = task.map(|t| text(t, "title")).unwrap_or("");
            let holder = task
                .map(|t| text(t, "claimed_by_participant"))
                .unwrap_or("");
            let reason = [text(call, "attention_what"), "workers ended, still held"]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("workers ended, still held");
            let title = if id.is_empty() {
                "Held task".to_owned()
            } else {
                format!("Task {id}")
            };
            let mut lines = vec![format!(
                "Task {} · state: {state}",
                if id.is_empty() { "unknown" } else { id }
            )];
            if !task_title.is_empty() {
                lines.push(format!("Title: {}", clean(task_title, 200)));
            }
            if !holder.is_empty() {
                lines.push(format!("Claimed by: {holder}"));
            }
            lines.push(format!("Status: {reason}"));
            let plan_id = text(&report["plan"], "plan_id");
            let workers: Vec<String> = if id.is_empty() {
                Vec::new()
            } else {
                calls(report)
                    .iter()
                    .filter(|c| call_matches_plan_task(c, id, plan_id))
                    .map(|c| {
                        let who = text(c, "participant");
                        let st = text(c, "state");
                        if st.is_empty() {
                            who.to_owned()
                        } else {
                            format!("{who} ({st})")
                        }
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            if workers.is_empty() {
                lines.push("Workers: none recorded".to_owned());
            } else {
                lines.push(format!("Workers: {}", workers.join(", ")));
            }
            Some((title, lines.join("\n")))
        }
        _ => None,
    }
}

fn attention_bucket(cls: &str) -> usize {
    match cls {
        "needs" => 0,
        "failed" | "guard_stop" | "timed_out" | "verify_failed" | "verify_timed_out"
        | "provider_error" => 1,
        "decide" => 2,
        "stale" => 3,
        "mail" => 4,
        _ => 5,
    }
}

fn attention_item_badge(cls: &str, compact: bool) -> (&'static str, &'static str) {
    match attention_bucket(cls) {
        0 => (if compact { "? you" } else { "? needs" }, SGR_GOLD_BOLD),
        1 => (if compact { "× fail" } else { "× failed" }, SGR_CORAL_BOLD),
        2 => (if compact { "⚑ dec" } else { "⚑ decide" }, SGR_GOLD_BOLD),
        3 => ("! stale", SGR_AMBER_BOLD),
        4 => ("· mail", SGR_CYAN),
        _ => ("· info", SGR_MUTED),
    }
}

fn render_attention_band(
    report: &Value,
    all: &[Value],
    items: &[attention::Item],
    columns: usize,
    use_cards: bool,
    max_rows: usize,
    colored: bool,
) -> Vec<String> {
    if items.is_empty() || max_rows == 0 {
        return Vec::new();
    }
    let mut counts = [0usize; 6];
    let mut unread = 0usize;
    for item in items {
        counts[attention_bucket(&item.cls)] += 1;
        if item.unread {
            unread += 1;
        }
    }
    let total = items.len();
    let format_header = |labels: [&str; 6]| {
        let mut parts = vec![format!("Attention {total}")];
        for (count, label) in counts.into_iter().zip(labels) {
            if count > 0 {
                parts.push(format!("{count} {label}"));
            }
        }
        parts.join(" · ")
    };
    let wide_title = format_header(["needs you", "failed", "decide", "stale", "mail", "info"]);
    let short_title = format_header(["you", "fail", "dec", "stale", "mail", "info"]);
    let bare_title = format!("Attention {total}");
    let tag_wide = format!("{unread} new / {total}");
    let tag_compact = format!("{unread} new/{total}");

    let boxed = use_cards && max_rows >= 3;
    let item_limit = if boxed {
        max_rows.saturating_sub(2).min(3)
    } else {
        let full = max_rows.saturating_sub(1).min(3);
        if items.len() > full && max_rows >= 2 {
            max_rows.saturating_sub(2).min(3)
        } else {
            full
        }
    };
    let mut out = Vec::new();
    if boxed {
        let tag_limit = (columns / 3).max(6);
        let tag = if width(&tag_wide) <= tag_limit {
            tag_wide.as_str()
        } else {
            tag_compact.as_str()
        };
        let right_len = width(tag) + 3;
        let title_budget = columns.saturating_sub(5 + right_len);
        let title = if width(&wide_title) <= title_budget {
            wide_title.as_str()
        } else if width(&short_title) <= title_budget {
            short_title.as_str()
        } else {
            bare_title.as_str()
        };
        out.push(card_top_styled(
            title,
            Some(tag),
            columns,
            colored,
            SGR_GOLD_BOLD,
        ));
    } else {
        let tag = if columns >= 36 && width(&bare_title) + 2 + width(&tag_wide) <= columns {
            tag_wide.as_str()
        } else if width(&bare_title) + 1 + width(&tag_compact) <= columns {
            tag_compact.as_str()
        } else {
            ""
        };
        let right_w = width(tag);
        let title_budget = left_budget(columns, right_w);
        let title = if width(&wide_title) <= title_budget {
            wide_title.as_str()
        } else if width(&short_title) <= title_budget {
            short_title.as_str()
        } else {
            bare_title.as_str()
        };
        let right = if tag.is_empty() {
            vec![]
        } else {
            vec![Span::raw(tag, columns, Some(SGR_AMBER_BOLD))]
        };
        out.push(render_lr(
            vec![Span::raw(title, title_budget.max(1), Some(SGR_GOLD_BOLD))],
            right,
            columns,
            false,
            None,
            colored,
        ));
    }

    let now = oms_panel_core::metrics::stamp(&report["generated_at"])
        .unwrap_or_else(|| oms_panel_core::timestamp() as f64);
    let inner = columns.saturating_sub(if boxed { 4 } else { 0 });
    for item in items.iter().take(item_limit) {
        let (badge, badge_sgr) = attention_item_badge(&item.cls, columns < 44);
        let marker = if item.unread { "* " } else { "  " };
        let marker_sgr = item.unread.then_some(SGR_GOLD_BOLD);
        let who = clean(&item.who, 80);
        let what = clean(&item.what, 160);
        let age = attention_item_age(report, all, item, now);
        let badge_w = width(marker) + width(badge);
        let right = match age {
            Some(age) if columns >= 44 || badge_w + width(&age) + 12 <= inner => {
                vec![Span::raw(age, 8, Some(SGR_MUTED))]
            }
            _ => Vec::new(),
        };
        let left_limit = left_budget(inner, spans_width(&right));
        let mut left = vec![
            Span::literal(marker, marker_sgr),
            Span::literal(badge, Some(badge_sgr)),
        ];
        let body_budget = left_limit.saturating_sub(badge_w);
        if !who.is_empty() && !what.is_empty() && body_budget > 3 {
            let who_limit = if columns < 44 {
                (body_budget / 3).max(6)
            } else {
                (body_budget / 3).max(12)
            };
            let who_clipped = clip(&who, who_limit);
            let sep = " · ";
            let dash = " — ";
            left.push(Span::literal(sep, Some(SGR_MUTED)));
            left.push(Span {
                plain: who_clipped.clone(),
                sgr: Some(SGR_BOLD),
            });
            let rem = body_budget.saturating_sub(width(sep) + width(&who_clipped) + width(dash));
            if rem > 0 {
                left.push(Span::literal(dash, Some(SGR_MUTED)));
                left.push(Span {
                    plain: clip(&what, rem),
                    sgr: Some(SGR_TEXT),
                });
            }
        } else if !who.is_empty() && body_budget > 3 {
            left.push(Span::literal(" · ", Some(SGR_MUTED)));
            left.push(Span {
                plain: clip(&who, body_budget.saturating_sub(3)),
                sgr: Some(SGR_BOLD),
            });
        } else if !what.is_empty() && body_budget > 3 {
            left.push(Span::literal(" · ", Some(SGR_MUTED)));
            left.push(Span {
                plain: clip(&what, body_budget.saturating_sub(3)),
                sgr: Some(SGR_TEXT),
            });
        }
        out.push(card_row_lr(left, right, columns, boxed, None, colored));
    }
    let hidden = items.len().saturating_sub(item_limit);
    if boxed {
        if hidden > 0 {
            out.push(card_bottom_labeled(
                &format!("+{hidden} more · b shows all"),
                columns,
                colored,
            ));
        } else if let Some(bottom) = card_bottom(columns, true, colored) {
            out.push(bottom);
        }
    } else if hidden > 0 && out.len() < max_rows {
        out.push(render_lr(
            vec![Span::raw(
                format!("  +{hidden} more · b shows all"),
                columns,
                Some(SGR_MUTED),
            )],
            vec![],
            columns,
            false,
            None,
            colored,
        ));
    }
    out
}

fn visible_from<'a>(
    all: &'a [Value],
    nav: &navigation::Navigation,
    main: Option<&Value>,
    direct: &BTreeSet<(&str, &str)>,
    attention_calls: &BTreeSet<&str>,
) -> Vec<&'a Value> {
    let groups = provider_groups(all, main);
    let words = navigation::words(&nav.filter);
    let mut filtered: Vec<_> = all
        .iter()
        .filter(|v| !main.is_some_and(|main| std::ptr::eq(*v, main)))
        .filter(|v| !collapsed_history(nav, v) && !fold_inactive(nav, v))
        .filter(|v| navigation::matches_words(v, &words))
        .filter(|v| {
            !nav.attention_only
                || v["attention_kind"].is_string()
                || (v["room_only_history"] != true
                    && v["room_only_stale"] != true
                    && (awaiting_review(v)
                        || v["lifecycle_stale"] == true
                        || attention_calls.contains(text(v, "participant"))
                        || [
                            "waiting_input",
                            "waiting_approval",
                            "blocked",
                            "orphaned",
                            "failed",
                            "timed_out",
                            "abandoned",
                        ]
                        .contains(&text(v, "state"))))
        })
        .collect();
    if nav.view == View::Auto {
        filtered.sort_by_key(|call| {
            (
                !is_direct_work(call, direct),
                groups
                    .iter()
                    .position(|group| *group == provider_bucket(call)),
            )
        });
    }
    filtered
}

fn board_rows<'a>(
    filtered: Vec<&'a Value>,
    nav: &navigation::Navigation,
    main: Option<&'a Value>,
    all: &'a [Value],
) -> Vec<TreeEntry<'a>> {
    if matches!(nav.view, View::Tree | View::Graph) {
        // Keep Main in the tree so children retain their links and folding rules.
        let tree_calls: Vec<_> = main.into_iter().chain(filtered).collect();
        collect_tree(
            &tree_calls,
            all.iter(),
            nav,
            nav.view == View::Graph,
            usize::MAX,
        )
        .into_iter()
        .filter(|entry| !main.is_some_and(|main| std::ptr::eq(entry.call, main)))
        .collect()
    } else {
        filtered
            .into_iter()
            .map(|call| TreeEntry {
                prefix: String::new(),
                call,
            })
            .collect()
    }
}

pub fn frame(
    report: &Value,
    nav: &navigation::Navigation,
    columns: usize,
    height: usize,
    read: &BTreeMap<String, String>,
) -> Vec<String> {
    frame_styled(report, nav, columns, height, read, false, false)
}

pub fn frame_styled(
    report: &Value,
    nav: &navigation::Navigation,
    columns: usize,
    height: usize,
    read: &BTreeMap<String, String>,
    colored: bool,
    fill_height: bool,
) -> Vec<String> {
    board_frame(report, nav, columns, height, read, colored, fill_height).lines
}

#[derive(Default)]
pub(crate) struct BoardFrame {
    pub lines: Vec<String>,
    pub provider_rows: [Option<usize>; 3],
    /// Painted calls, including Overall Main, keyed by zero-based terminal row.
    pub worker_rows: BTreeMap<usize, (String, String)>,
    pub weekly_usage: Option<WeeklyUsageHit>,
    pub main_switch: Option<ActionZone>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActionZone {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

impl ActionZone {
    pub(crate) fn contains(self, x: u16, y: u16) -> bool {
        let col = usize::from(x);
        usize::from(y).checked_sub(1) == Some(self.row) && (self.start..=self.end).contains(&col)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct WeeklyUsageHit {
    pub row: usize,
    pub columns: usize,
    pub now: f64,
}

pub(crate) fn board_frame(
    report: &Value,
    nav: &navigation::Navigation,
    columns: usize,
    height: usize,
    read: &BTreeMap<String, String>,
    colored: bool,
    fill_height: bool,
) -> BoardFrame {
    board_frame_with_native_activity(
        report,
        nav,
        columns,
        height,
        read,
        colored,
        fill_height,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn board_frame_with_native_activity(
    report: &Value,
    nav: &navigation::Navigation,
    columns: usize,
    height: usize,
    read: &BTreeMap<String, String>,
    colored: bool,
    fill_height: bool,
    native_activity_current: bool,
) -> BoardFrame {
    frame_with_capacity(
        report,
        nav,
        columns,
        height,
        read,
        colored,
        fill_height,
        native_activity_current,
        false,
        &mut 1,
    )
}

pub fn page_capacity(
    report: &Value,
    nav: &navigation::Navigation,
    columns: usize,
    height: usize,
    read: &BTreeMap<String, String>,
) -> usize {
    if nav.help || nav.health || nav.detail {
        return 1;
    }
    let mut capacity = 1;
    frame_with_capacity(
        report,
        nav,
        columns,
        height,
        read,
        false,
        false,
        false,
        true,
        &mut capacity,
    );
    capacity
}

#[cfg(test)]
thread_local! {
    /// Work-row and overlay painting calls on this thread.
    static ROW_RENDERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_row_render() {
    #[cfg(test)]
    ROW_RENDERS.with(|count| count.set(count.get() + 1));
}

#[allow(clippy::too_many_arguments)]
fn frame_with_capacity(
    report: &Value,
    nav: &navigation::Navigation,
    columns: usize,
    height: usize,
    read: &BTreeMap<String, String>,
    colored: bool,
    fill_height: bool,
    native_activity_current: bool,
    capacity_only: bool,
    page_capacity: &mut usize,
) -> BoardFrame {
    let columns = columns.max(1);
    let height = height.max(3);
    let mut provider_rows = [None; 3];
    let mut worker_rows = BTreeMap::new();
    let mut weekly_usage = None;
    let mut main_switch = None;
    let use_cards = columns >= 44;
    let inner_width = if use_cards {
        columns.saturating_sub(4)
    } else {
        columns
    };
    let mut all = calls(report);
    let attention = attention::items(report, read);
    let main_id = overall_main(&report["room"], &all).map(|call| {
        (
            text(call, "participant").to_owned(),
            text(call, "attempt").to_owned(),
        )
    });
    if nav.attention_only {
        all.extend(attention_extra_rows(report, &all, &attention));
    }
    let pinned = main_id.as_ref().and_then(|(participant, attempt)| {
        all.iter().find(|call| {
            call["role"] == "main"
                && text(call, "participant") == participant
                && text(call, "attempt") == attempt
        })
    });
    let direct = direct_work(report, &all, pinned);
    let pinned_count = usize::from(pinned.is_some());
    let total = all.len() - pinned_count;
    let history = all
        .iter()
        .filter(|call| !pinned.is_some_and(|main| std::ptr::eq(*call, main)))
        .filter(|call| collapsed_history(nav, call))
        .count();
    let inactive = all
        .iter()
        .filter(|call| !pinned.is_some_and(|main| std::ptr::eq(*call, main)))
        .filter(|call| fold_inactive(nav, call))
        .count();
    let attention_calls = attention_call_ids(&attention);
    let filtered = visible_from(&all, nav, pinned, &direct, &attention_calls);
    let review = if default_overview(nav) {
        all.iter().filter(|call| awaiting_review(call)).count()
    } else {
        filtered.iter().filter(|call| awaiting_review(call)).count()
    };
    let filtered_count = filtered.len();
    let calls = board_rows(filtered, nav, pinned, &all);
    let selected_worker = nav.selected.checked_sub(pinned_count);
    let main_selected = pinned.is_some() && nav.selected == 0;
    let selected_entry = selected_worker.and_then(|index| calls.get(index));
    let view = nav.view.resolved(columns, height);
    let plan = room_plan(report, &all, &attention);

    let mut lines = Vec::new();
    lines.push(header_line(
        report,
        &calls,
        review,
        &attention,
        plan.as_ref(),
        if nav.view == View::Auto {
            View::Auto
        } else {
            view
        },
        columns,
        colored,
    ));
    let overlay = nav.help || nav.health || nav.detail;
    let mut main_cards = use_cards && height >= 12;
    let (mut main, mut main_switch_cols) = render_main_section(
        plan.as_ref(),
        pinned,
        main_selected,
        columns,
        main_cards,
        colored,
        !overlay,
        overlay,
    );
    let mut notices = Vec::new();
    if let Some(notice) = &nav.notice {
        for part in wrap(notice, columns.saturating_sub(2).max(1))
            .into_iter()
            .take(2)
        {
            notices.push(render_lr(
                vec![
                    Span::literal("› ", Some(SGR_CYAN)),
                    Span::raw(part, columns, Some(SGR_CYAN)),
                ],
                vec![],
                columns,
                false,
                None,
                colored,
            ));
        }
    }

    let stale = report["collection"]["ok"] != true || report["capacity_stale"] == true;
    let filtering = !nav.filter.is_empty() || nav.editing;
    let short_board = (9..14).contains(&height) && !overlay;
    let filtered_out = total.saturating_sub(history + inactive + filtered_count);
    let folded = filtered_count.saturating_sub(calls.len());
    // One call and the footer always need space. Leave an extra row if an omission count can appear.
    let minimum =
        if short_board && calls.len() <= 1 && history + inactive + filtered_out + folded == 0 {
            2
        } else {
            3
        };
    let fixed = lines.len() + notices.len() + usize::from(stale) + usize::from(filtering) + minimum;
    let required = fixed + main.len();
    // Keep the Main and Goal, dropping their card borders only when needed for Weekly.
    let compact_main = (short_board && main_cards && !main.is_empty() && required >= height)
        .then(|| {
            render_main_section(
                plan.as_ref(),
                pinned,
                main_selected,
                columns,
                false,
                colored,
                false,
                true,
            )
            .0
        })
        .filter(|compact| fixed + compact.len() < height);
    let mut optional = Vec::new();
    if height >= 14 || (short_board && required < height) || compact_main.is_some() {
        // One usage projection per frame; painting never rereads provider state.
        let now = oms_panel_core::timestamp() as f64;
        let usage = crate::usage::snapshot(report, now);
        let (line, complete) = provider_line(
            report,
            &usage,
            now,
            columns,
            colored,
            native_activity_current,
        );
        weekly_usage = complete.map(|columns| WeeklyUsageHit {
            row: lines.len(),
            columns,
            now,
        });
        if height >= 14 || complete.is_some() {
            if let Some(compact) = compact_main {
                main = compact;
                main_cards = false;
                main_switch_cols = None;
            }
            lines.push(line);
        }
    }
    if columns >= 68 && height >= 18 && !overlay {
        optional.push(legend_line(colored, columns));
    }
    if stale {
        lines.push(render_lr(
            vec![Span::literal(
                "! STALE: refresh failed; retained evidence cannot authorize actions",
                Some(SGR_CORAL_BOLD),
            )],
            vec![],
            columns,
            false,
            None,
            colored,
        ));
    }
    if overlay {
        count_row_render();
        lines.extend(optional.iter().cloned());
        lines.extend(notices.iter().cloned());
        lines.extend(main.iter().cloned());
    }
    if nav.help {
        lines.extend(render_help_card(columns, inner_width, use_cards, colored));
    } else if nav.health {
        lines.extend(render_health_card(
            report,
            &all,
            columns,
            inner_width,
            use_cards,
            colored,
        ));
    } else if nav.detail {
        lines.extend(match pinned.filter(|_| main_selected) {
            Some(main) => render_main_detail_card(
                main,
                plan.as_ref(),
                columns,
                inner_width,
                use_cards,
                colored,
                height.saturating_sub(1 + lines.len()),
            ),
            None => render_detail_card(
                selected_entry.map(|entry| entry.call),
                columns,
                inner_width,
                use_cards,
                colored,
                height.saturating_sub(1 + lines.len()),
            ),
        });
    } else {
        if filtering {
            let filter_line = format!(
                "Filter /{}{} · shown {} of {} · Esc clears",
                nav.filter,
                if nav.editing { "_" } else { "" },
                calls.len(),
                total
            );
            lines.push(render_lr(
                vec![Span::raw(filter_line, columns, Some(SGR_GOLD_BOLD))],
                vec![],
                columns,
                false,
                None,
                colored,
            ));
        }

        // Keep a real target above the footer when status lines and the dock shorten the board.
        let body = height - 1;
        let worker_row = usize::from(!calls.is_empty() && (!main_selected || body >= 3));
        let main_row = usize::from(pinned.is_some() && (main_selected || body >= 3));
        lines.truncate(body.saturating_sub(worker_row + main_row));
        let main_room = body.saturating_sub(lines.len() + worker_row);
        if main.len() > main_room {
            if main_cards {
                let (compact_boxed, compact_switch) = render_main_section(
                    plan.as_ref(),
                    pinned,
                    main_selected,
                    columns,
                    true,
                    colored,
                    !overlay,
                    true,
                );
                if compact_boxed.len() <= main_room {
                    main = compact_boxed;
                    main_switch_cols = compact_switch;
                } else {
                    (main, main_switch_cols) = render_main_section(
                        plan.as_ref(),
                        pinned,
                        main_selected,
                        columns,
                        false,
                        colored,
                        false,
                        true,
                    );
                    main_cards = false;
                }
            } else {
                (main, main_switch_cols) = render_main_section(
                    plan.as_ref(),
                    pinned,
                    main_selected,
                    columns,
                    false,
                    colored,
                    false,
                    true,
                );
            }
        }
        main.truncate(main_room);
        let groups = provider_groups(&all, pinned);
        let has_direct = calls
            .iter()
            .any(|entry| is_direct_work(entry.call, &direct));
        // Keep page capacity steady when the direct rows scroll offscreen.
        let provider_chrome = groups.len() + 1 + 2 * usize::from(has_direct);
        let content_start = lines.len() + main.len();
        // On short boards, wrapped notices take precedence over decorative chrome.
        let chrome_start = content_start + if short_board { notices.len() } else { 0 };
        let provider_view = nav.view == View::Auto
            && use_cards
            && height.saturating_sub(chrome_start) >= provider_chrome + minimum;
        let activity_chrome = usize::from(!provider_view && chrome_start + 4 < height);
        let bottom_chrome =
            !provider_view && use_cards && chrome_start + activity_chrome + 4 < height;
        // A pending report keeps the roster: Watch carries it only from the
        // same scope's last read and drops it on any scope change.
        let roster = if provider_view && !stale && !filtering && !nav.attention_only {
            rows(&report["provider_activity"]["antigravity"]["children"])
        } else {
            &[]
        };
        let roster = &roster[..roster.len().min(crate::provider_activity::AGY_ROSTER_LIMIT)];
        let roster_limit = roster
            .len()
            .min(height.saturating_sub(chrome_start + provider_chrome + minimum));
        let chrome = if provider_view {
            provider_chrome + roster_limit
        } else {
            activity_chrome + usize::from(bottom_chrome)
        };
        let reserved = minimum + chrome;
        let available = height
            .saturating_sub(reserved)
            .saturating_sub(content_start);
        if available > 0 && !notices.is_empty() {
            lines.push(notices.remove(0));
        }
        let remaining = height
            .saturating_sub(reserved)
            .saturating_sub(lines.len() + main.len());
        lines.extend(optional.into_iter().chain(notices).take(remaining));
        if let Some(call) = pinned {
            if main_cards
                && !main.is_empty()
                && let Some((start, end)) = main_switch_cols
            {
                main_switch = Some(ActionZone {
                    row: lines.len(),
                    start,
                    end,
                });
            }
            let row = usize::from(main_cards);
            if row < main.len() {
                register_call(&mut worker_rows, lines.len() + row, call);
            }
            if row < main.len() && lines.len() + main.len() + reserved < height {
                main.insert(
                    row + 1,
                    checkout_line(report, call, columns, main_cards, colored),
                );
            }
        }
        lines.extend(main);
        if !attention.is_empty() && !nav.attention_only {
            let attn_budget = height.saturating_sub(reserved + lines.len());
            lines.extend(render_attention_band(
                report,
                &all,
                &attention,
                columns,
                use_cards,
                attn_budget,
                colored,
            ));
        }
        let remaining_rows = body.saturating_sub(lines.len() + chrome);
        let row_capacity = remaining_rows.saturating_sub(1).max(1).min(remaining_rows);
        let detail_rows = !provider_view && view == View::Detail && row_capacity >= 2;
        let capacity = if detail_rows {
            row_capacity / 2
        } else {
            row_capacity
        };
        *page_capacity = capacity.max(1);
        // Per-key paging needs only the capacity; nothing below changes it.
        if capacity_only {
            return BoardFrame::default();
        }
        let selected = selected_worker
            .unwrap_or(0)
            .min(calls.len().saturating_sub(1));
        let offset = nav
            .offset
            .saturating_sub(pinned_count)
            .min(selected)
            .max(selected.saturating_add(1).saturating_sub(capacity))
            .min(calls.len().saturating_sub(capacity));
        if activity_chrome > 0 {
            let title = if nav.attention_only {
                format!("Attention · {}", calls.len())
            } else {
                format!("Tasks · {}", calls.len())
            };
            lines.push(card_top(&title, None, columns, colored));
        }
        if provider_view {
            let base = lines.len();
            lines.extend(render_provider_rows(
                &groups,
                &all,
                &calls,
                &direct,
                nav,
                selected_worker,
                offset,
                capacity,
                columns,
                colored,
                &report["provider_sessions"],
                &report["provider_models"],
                &report["provider_accounts"],
                &report["provider_activity"],
                &report["provider_delivery"],
                !stale && report["collection"]["pending"] != true,
                roster,
                roster_limit,
                &mut provider_rows,
                &mut worker_rows,
                base,
            ));
            for row in provider_rows.iter_mut().flatten() {
                *row += base;
            }
        } else if capacity > 0 && calls.is_empty() {
            let empty = if nav.attention_only {
                "  No attention items"
            } else if pinned.is_some() {
                "  No visible lead or worker calls"
            } else {
                "  No recorded calls — press ? for shortcuts or dispatch a worker"
            };
            lines.push(card_row_lr(
                vec![Span::literal(empty, Some(SGR_MUTED))],
                vec![],
                columns,
                use_cards,
                None,
                colored,
            ));
        } else if capacity > 0 {
            match view {
                View::Summary if nav.view == View::Summary => {
                    lines.extend(render_summary_rows(
                        &calls, capacity, columns, use_cards, colored,
                    ));
                }
                _ => {
                    for (index, entry) in calls.iter().enumerate().skip(offset).take(capacity) {
                        let call = entry.call;
                        let selected = Some(index) == selected_worker;
                        register_call(&mut worker_rows, lines.len(), call);
                        let bg = (colored && selected).then_some(SGR_SEL_BG);
                        lines.push(render_card_row(
                            call,
                            &entry.prefix,
                            "",
                            selected,
                            columns,
                            use_cards,
                            colored,
                            false,
                        ));
                        if detail_rows {
                            lines.push(card_row_lr(
                                vec![
                                    Span::literal("      ", None),
                                    Span::raw(call_detail_meta(call), inner_width, Some(SGR_MUTED)),
                                ],
                                vec![],
                                columns,
                                use_cards,
                                bg,
                                colored,
                            ));
                        }
                    }
                }
            }
        }
        let offscreen = calls.len().saturating_sub(capacity.min(calls.len()));
        if filtered_out + folded + offscreen + history + inactive > 0 {
            lines.push(render_lr(
                vec![Span::raw(
                    [
                        (offscreen, "hidden"),
                        (filtered_out, "filt"),
                        (folded, "fold"),
                        (inactive, "folded (Tab all)"),
                        (history, "past (Tab all)"),
                    ]
                    .into_iter()
                    .filter(|(count, _)| *count > 0)
                    .map(|(count, label)| format!("{count} {label}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                    columns,
                    Some(SGR_MUTED),
                )],
                vec![],
                columns,
                false,
                None,
                colored,
            ));
        }
        if bottom_chrome && let Some(bottom) = card_bottom(columns, use_cards, colored) {
            lines.push(bottom);
        }
    }

    let body_cap = height.saturating_sub(1);
    lines.truncate(body_cap);
    if fill_height {
        while lines.len() < body_cap {
            lines.push(String::new());
        }
    }
    let painted = lines.len().min(body_cap);
    for row in &mut provider_rows {
        if row.is_some_and(|row| row >= painted) {
            *row = None;
        }
    }
    worker_rows.retain(|row, _| *row < painted);
    weekly_usage = weekly_usage.filter(|hit| hit.row < painted);
    main_switch = main_switch.filter(|zone| zone.row < painted);
    let tab = if nav.view == View::Auto {
        "All"
    } else {
        "Overview"
    };
    let tree = if matches!(nav.view, View::Tree | View::Graph) {
        ""
    } else {
        "  g/t Tree"
    };
    let hints = [
        format!(
            "↑↓ Move  Enter Open  Tab {tab}  D Inspect{tree}  / Filter  b Attention  u Refresh  ? Keys  q Quit"
        ),
        format!("↑↓ Enter Tab {tab} D Inspect{tree} ? q"),
        format!("↑↓ Enter Tab {tab} D Inspect ? q"),
        "↑↓ Enter ? q".to_owned(),
    ];
    let hint = hints
        .into_iter()
        .find(|hint| width(hint) <= columns)
        .unwrap_or_else(|| clip("↑↓ Enter ? q", columns));
    lines.push(if colored {
        format!("{SGR_DIM}{hint}{SGR_RESET}")
    } else {
        hint
    });
    BoardFrame {
        lines,
        provider_rows,
        worker_rows,
        weekly_usage,
        main_switch,
    }
}

fn raw_label(call: &Value) -> &str {
    [
        text(call, "alias"),
        text(call, "participant"),
        text(call, "attempt"),
    ]
    .into_iter()
    .find(|v| !v.is_empty())
    .unwrap_or("call")
}

fn call_label(call: &Value) -> String {
    short_call_label(raw_label(call))
}

fn short_call_label(raw: &str) -> String {
    if (raw.starts_with("att_") || raw.starts_with("call-")) && raw.chars().count() > 16 {
        raw.chars().take(12).collect()
    } else {
        raw.to_owned()
    }
}

struct CallDisplay<'a> {
    name: String,
    role: &'a str,
    role_label: &'a str,
    title: &'a str,
}

impl CallDisplay<'_> {
    fn board_spans(name: &str, task: &str, budget: usize) -> [Span; 3] {
        let task_reserve = if !task.is_empty() && budget > 6 {
            (width(task) + 3).min(budget / 2)
        } else {
            0
        };
        let name = clip(name, budget.saturating_sub(task_reserve));
        let remaining = budget.saturating_sub(width(&name));
        let separator = if name.is_empty() {
            ""
        } else if remaining <= 5 {
            " "
        } else {
            " · "
        };
        let task = clip(task, budget.saturating_sub(width(&name) + width(separator)));
        [
            Span {
                plain: name,
                sgr: Some(SGR_BOLD),
            },
            Span::literal(
                if task.is_empty() { "" } else { separator },
                Some(SGR_MUTED),
            ),
            Span {
                plain: task,
                sgr: Some(SGR_TEXT),
            },
        ]
    }
}

fn board_role_label(value: &str, role: &str) -> bool {
    value.eq_ignore_ascii_case(role)
        || matches!(role, "Builder" | "Worker")
            && ["Builder", "Worker"]
                .iter()
                .any(|label| value.eq_ignore_ascii_case(label))
}

fn board_brief<'a>(value: &'a str, role: &str) -> &'a str {
    let mut value = value.trim();
    loop {
        if let Some(brief) = value.strip_prefix(crate::native_children::TASK_PREFIX) {
            value = brief.trim_start();
        } else if let Some((kind, rest)) = value.split_once(char::is_whitespace)
            && kind.eq_ignore_ascii_case("role")
            && let Some(end) = rest
                .trim_start()
                .find(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ':' | '·'))
            && board_role_label(&rest.trim_start()[..end], role)
        {
            value = rest.trim_start()[end..].trim_start_matches(|c: char| {
                c.is_whitespace() || matches!(c, '.' | ',' | ':' | '·')
            });
        } else if let Some((prefix, brief)) = value.split_once(". ")
            && let Some((kind, label)) = prefix.split_once(' ')
            && kind.eq_ignore_ascii_case("role")
            && board_role_label(label.trim(), role)
        {
            value = brief.trim_start();
        } else if let Some((prefix, brief)) = value.split_once([':', '·'])
            && {
                let prefix = prefix.trim_end();
                board_role_label(prefix, role)
                    || prefix.eq_ignore_ascii_case("task")
                    || matches!(role, "Builder" | "Worker")
                        && ["Builder brief", "Worker brief", "Brief"]
                            .iter()
                            .any(|label| prefix.eq_ignore_ascii_case(label))
            }
        {
            value = brief.trim_start();
        } else {
            return value;
        }
    }
}

#[cfg(test)]
fn render_call_row(
    call: &Value,
    prefix: &str,
    suffix: &str,
    selected: bool,
    columns: usize,
    use_cards: bool,
    colored: bool,
) -> String {
    render_card_row(
        call, prefix, suffix, selected, columns, use_cards, colored, false,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_card_row(
    call: &Value,
    prefix: &str,
    suffix: &str,
    selected: bool,
    columns: usize,
    use_cards: bool,
    colored: bool,
    align_roles: bool,
) -> String {
    count_row_render();
    let inner = columns - if use_cards { 4 } else { 0 };
    let display = call_display(call);
    let child = uses_observed_model(call);
    let (glyph, role_sgr) = role_style(display.role);
    let mut role = display.role.chars();
    let role = role.next().map_or_else(
        || "Worker".to_owned(),
        |first| format!("{}{}", first.to_uppercase(), role.as_str()),
    );
    let role = clip(&role, 24);
    let name = if !explicit_worker_alias(call) && display.name == text(call, "title") {
        board_brief(&display.name, &role)
    } else {
        &display.name
    };
    let name = clean(name, 1_048_576);
    let name = if !explicit_worker_alias(call) && board_role_label(name.trim(), &role) {
        ""
    } else {
        &name
    };
    let task = clean(board_brief(display.title, &role), 1_048_576);
    let task = if task.eq_ignore_ascii_case(name) || board_role_label(&task, &role) {
        String::new()
    } else {
        task
    };
    let task = if task.is_empty() {
        let purpose = clean(board_brief(text(call, "purpose"), &role), 1_048_576);
        if purpose.eq_ignore_ascii_case(name) || board_role_label(&purpose, &role) {
            String::new()
        } else {
            purpose
        }
    } else {
        task
    };
    let branch = if prefix.is_empty() { " " } else { prefix };
    let mut left = vec![
        Span::literal(if selected { "▸" } else { " " }, Some(SGR_AMBER_BOLD)),
        Span::raw(clip(branch, (inner / 6).max(1)), inner, Some(SGR_BORDER)),
    ];
    if use_cards {
        left.extend([
            Span::literal(glyph, Some(role_sgr)),
            Span::literal(" ", None),
        ]);
    }
    left.push(Span::raw(&role, 24, Some(role_sgr)));
    if !name.is_empty() || !task.is_empty() {
        let field = if align_roles
            || child && columns >= 56
            || columns >= 72
                && matches!(
                    role.as_str(),
                    "Builder" | "Researcher" | "Reviewer" | "Verifier" | "Worker"
                ) {
            width("Researcher")
        } else if matches!(role.as_str(), "Builder" | "Worker") {
            width("Builder")
        } else {
            0
        };
        let pad = field.saturating_sub(width(&role)) + 1;
        left.push(Span {
            plain: " ".repeat(pad),
            sgr: None,
        });
    }
    let prefix_width = spans_width(&left);
    let label_min = if name.is_empty() {
        width(&task).min(4)
    } else {
        width(name).min(3)
    };
    let text_min = if name.is_empty() || task.is_empty() {
        (width(name) + width(&task)).min(8)
    } else {
        label_min + 7
    };
    let metadata_limit = left_budget(inner, prefix_width + text_min);
    let mut right = token_span(
        call,
        left_budget(inner, prefix_width + label_min),
        columns,
        use_cards,
    );
    let (badge, badge_sgr) = call_badge(call);
    let badge_width = width(&badge);
    let mut available = metadata_limit.saturating_sub(spans_width(&right) + 2);
    // An operation or stale-event advisory outranks an unknown-total placeholder.
    if badge_width > available && (active_verb(call).is_some() || call["lifecycle_stale"] == true) {
        let compact = token_span_with(
            call,
            left_budget(inner, prefix_width + label_min),
            columns,
            use_cards,
            false,
        );
        let room = metadata_limit.saturating_sub(spans_width(&compact) + 2);
        if badge_width <= room {
            right = compact;
            available = room;
        }
    }
    if call["lifecycle_stale"] == true && badge_width > available {
        right.clear();
        available = left_budget(inner, prefix_width + label_min).saturating_sub(2);
    }
    if badge_width <= available {
        let field = if columns >= 88 && available >= 14 {
            14
        } else {
            badge_width
        };
        let badge = format!("{badge}{}", " ".repeat(field.saturating_sub(badge_width)));
        right.splice(
            0..0,
            [
                Span {
                    plain: badge,
                    sgr: Some(badge_sgr),
                },
                Span::literal("  ", None),
            ],
        );
    }
    let model_gap = if child && columns < 88 { " " } else { "  " };
    let model_overhead = spans_width(&right) + model_gap.len();
    let model_budget = metadata_limit.saturating_sub(width(suffix).min(15) + model_overhead);
    let model = if child {
        let preferred = match columns {
            0..=63 => 1,
            64..=87 => 4,
            _ => (columns / 9).min(18),
        };
        // Compact rows can spend a few brief cells to keep the model slot.
        // The token, elapsed and state fields retain their allocated space.
        let compact = if (44..88).contains(&columns) {
            left_budget(inner, prefix_width + label_min)
                .saturating_sub(model_overhead)
                .min(preferred)
        } else {
            0
        };
        let field = preferred.min(model_budget.max(compact));
        let label = clip(&child_model_label(call), field);
        format!("{label}{}", " ".repeat(field.saturating_sub(width(&label))))
    } else {
        clip(&model_label(call), (columns / 4).max(12))
    };
    if (child || use_cards)
        && !model.is_empty()
        && model != "model —"
        && (child || width(&model) <= model_budget)
    {
        right.splice(
            0..0,
            [
                Span::raw(model, columns, Some(provider_style(provider_bucket(call)))),
                Span::literal(model_gap, None),
            ],
        );
    }
    let budget = left_budget(inner, spans_width(&right)).saturating_sub(prefix_width);
    let suffix_budget = budget.saturating_sub(text_min.max(budget / 3)).min(32);
    let suffix = if suffix_budget >= 5 {
        clip(suffix, suffix_budget)
    } else {
        String::new()
    };
    left.extend(CallDisplay::board_spans(
        name,
        &task,
        budget.saturating_sub(width(&suffix)),
    ));
    left.push(Span {
        plain: suffix,
        sgr: Some(SGR_MUTED),
    });
    card_row_lr(
        left,
        right,
        columns,
        use_cards,
        (colored && selected).then_some(SGR_SEL_BG),
        colored,
    )
}

fn generated_id(label: &str) -> bool {
    let label = label.trim();
    let hex_id =
        |id: &str| matches!(id.len(), 16 | 32) && id.bytes().all(|byte| byte.is_ascii_hexdigit());
    label.strip_prefix("panel-").is_some_and(|id| {
        id.len() == 12
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) || label.strip_prefix("call-").is_some_and(hex_id)
        || label.strip_prefix("att_").is_some_and(hex_id)
        || label
            .strip_prefix("native-")
            .and_then(|id| id.rsplit_once('-'))
            .is_some_and(|(_, id)| id.len() == 16 && hex_id(id))
}

const UNREPORTED_WORK: &str = "work unreported";

/// Generated panel/call/attempt/native IDs and `task_`/`task-` hex IDs name no work.
fn synthetic_id(label: &str) -> bool {
    let label = label.trim();
    generated_id(label)
        || ["task_", "task-"].iter().any(|prefix| {
            label.strip_prefix(prefix).is_some_and(|id| {
                matches!(id.len(), 16 | 32) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
}

fn explicit_worker_alias(call: &Value) -> bool {
    let alias = text(call, "alias");
    !alias.is_empty() && call["display_role"].is_string() && call["refs"]["panel_alias"] == alias
}

/// Peer adapters name live attempts `{provider}-{tool}` (oms-command-peer
/// peer.rs `call`) or `{provider}-delegate` (delegate.rs) unless dispatch sets
/// `OMS_PANEL_ALIAS`; only that exact form yields to the task title.
fn adapter_alias(call: &Value) -> bool {
    const TOOLS: [&str; 6] = [
        "peer-ask",
        "peer-review",
        "consult",
        "advise",
        "agent-call",
        "delegate",
    ];
    let provider = text(call, "provider");
    let alias = text(call, "alias");
    !provider.is_empty()
        && !explicit_worker_alias(call)
        && alias
            .strip_prefix(provider)
            .and_then(|tool| tool.strip_prefix('-'))
            .is_some_and(|tool| TOOLS.contains(&tool))
}

fn call_display(call: &Value) -> CallDisplay<'_> {
    let raw_role = text(call, "role");
    let explicit_alias = explicit_worker_alias(call);
    let mut name = if explicit_alias {
        text(call, "alias").to_owned()
    } else {
        call_label(call)
    };
    let mut title = text(call, "title");
    if raw_role == "main" {
        return CallDisplay {
            name,
            role: raw_role,
            role_label: raw_role,
            title,
        };
    }

    let role = match raw_role {
        "appserver" | "subagent" | "" => "worker",
        "worker" | "reviewer" => call["display_role"].as_str().unwrap_or(raw_role),
        role => role,
    };
    let runtime_label = |label: &str| {
        ["↳", "appserver", "subagent", "Native subagent", "worker"]
            .iter()
            .any(|generic| label.trim().eq_ignore_ascii_case(generic))
    };
    if runtime_label(title) || title.trim().eq_ignore_ascii_case(role) {
        title = "";
    }
    let raw_name = raw_label(call);
    if !explicit_alias && (runtime_label(raw_name) || generated_id(raw_name) || adapter_alias(call))
    {
        let fallback = [
            title,
            text(call, "participant"),
            text(call, "attempt"),
            text(call, "task_id"),
        ]
        .into_iter()
        .find(|label| !label.is_empty() && !runtime_label(label) && !synthetic_id(label))
        .unwrap_or(UNREPORTED_WORK);
        name = if fallback == title || fallback == UNREPORTED_WORK {
            fallback.to_owned()
        } else {
            short_call_label(fallback)
        };
    }
    if name.trim() == title.trim() {
        title = "";
    }
    let role_label = if name.trim().eq_ignore_ascii_case(role) {
        ""
    } else {
        role
    };
    CallDisplay {
        name,
        role,
        role_label,
        title,
    }
}

fn call_detail_meta(call: &Value) -> String {
    let provider = text(call, "provider");
    let model = model_detail(call);
    let task = text(call, "task_id");
    [provider, model.as_str(), task]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn project_title(report: &Value) -> &str {
    let name = text(report, "project_name").trim();
    if name.is_empty() || name.contains(['/', '\\']) {
        "workspace"
    } else {
        name
    }
}

fn plan_task_terminal(task: &Value) -> bool {
    task["terminal"] == true
        || [
            "done",
            "completed",
            "failed",
            "abandoned",
            "cancelled",
            "canceled",
            "timed_out",
            "retired",
            "superseded",
            "closed",
        ]
        .contains(
            &[text(task, "state"), text(task, "status")]
                .into_iter()
                .find(|v| !v.is_empty())
                .unwrap_or(""),
        )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlanProgress {
    goal: String,
    /// `None` means the task denominator is unknown (missing, invalid, or empty).
    /// All-terminal plans (including all-cancelled plans) hide the Goal via `room_plan` returning `None`.
    total: Option<usize>,
    verified: usize,
    in_review: usize,
    claimed: usize,
    stale: usize,
}

impl PlanProgress {
    fn bar(&self) -> Option<String> {
        let total = self.total.filter(|total| *total > 0)?;
        let verified = self.verified.min(total);
        let filled = if verified == 0 {
            0
        } else if verified == total {
            10
        } else {
            ((verified * 10 + total / 2) / total).clamp(1, 9)
        };
        Some(format!(
            "▕{}{}▏",
            "█".repeat(filled),
            "░".repeat(10 - filled)
        ))
    }

    fn counts_text(&self, include_stale: bool, include_stages: bool) -> String {
        let Some(total) = self.total else {
            return "progress unknown".to_owned();
        };
        let mut parts = vec![format!("{}/{total} verified", self.verified)];
        if include_stages {
            if self.in_review > 0 {
                parts.push(format!("{} in review", self.in_review));
            }
            if self.claimed > 0 {
                parts.push(format!("{} claimed", self.claimed));
            }
            if include_stale && self.stale > 0 {
                parts.push(format!("{} stale", self.stale));
            }
        }
        parts.join(" · ")
    }

    fn goal_progress_label(&self, budget: usize, show_bar: bool) -> String {
        let mut candidates = Vec::new();
        if show_bar && let Some(bar) = self.bar() {
            candidates.push(format!("{bar} {}", self.counts_text(true, true)));
            candidates.push(format!("{bar} {}", self.counts_text(false, true)));
            candidates.push(format!("{bar} {}", self.counts_text(false, false)));
        }
        candidates.push(self.counts_text(true, true));
        candidates.push(self.counts_text(false, true));
        candidates.push(self.counts_text(false, false));
        if let Some(total) = self.total {
            candidates.push(format!("{}/{total} ver", self.verified));
            candidates.push(format!("{}/{total}", self.verified));
        } else {
            candidates.push("unknown".to_owned());
        }
        candidates
            .iter()
            .find(|label| width(label) <= budget)
            .cloned()
            .unwrap_or_else(|| clip(candidates.last().unwrap(), budget))
    }

    fn header_label(&self, budget: usize) -> Option<String> {
        let candidates = if let Some(total) = self.total {
            vec![
                self.counts_text(false, true),
                self.counts_text(false, false),
                format!("{}/{total} ver", self.verified),
            ]
        } else {
            vec![
                "progress unknown".to_owned(),
                "plan unknown".to_owned(),
                "unknown".to_owned(),
            ]
        };
        candidates.into_iter().find(|label| width(label) <= budget)
    }
}

fn plan_task_state(task: &Value) -> &str {
    [text(task, "state"), text(task, "status")]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or("")
}

fn plan_task_cancelled(task: &Value) -> bool {
    matches!(plan_task_state(task), "cancelled" | "canceled")
}

/// Active plan explicitly bound to this Room via `room.plan_id`, `room.task_id`,
/// a Room call's `refs.plan_id`, or a joined Room participant's task claim.
/// Unbound workspace plans and plans whose tasks are all terminal (including all
/// cancelled) stay hidden.
fn room_plan(
    report: &Value,
    calls: &[Value],
    attention: &[attention::Item],
) -> Option<PlanProgress> {
    let plan = &report["plan"];
    let goal = clean(text(plan, "goal"), 1_048_576);
    let goal = goal.trim();
    if goal.is_empty() {
        return None;
    }
    let plan_id = text(plan, "plan_id");
    let room = &report["room"];
    let room_id = text(room, "id");
    let room_task = text(room, "task_id");
    let tasks_present = matches!(&plan["tasks"], Value::Object(_) | Value::Array(_));
    let tasks: Vec<(&str, &Value)> = match &plan["tasks"] {
        Value::Object(map) => map.iter().map(|(id, task)| (id.as_str(), task)).collect(),
        Value::Array(rows) => rows
            .iter()
            .map(|task| {
                let id = [text(task, "id"), text(task, "task_id")]
                    .into_iter()
                    .find(|v| !v.is_empty())
                    .unwrap_or("");
                (id, task)
            })
            .collect(),
        _ => Vec::new(),
    };

    let room_plan_match = !plan_id.is_empty() && text(room, "plan_id") == plan_id;
    let room_task_match = !room_task.is_empty()
        && tasks.iter().any(|(id, task)| {
            *id == room_task || text(task, "id") == room_task || text(task, "task_id") == room_task
        });
    let mut room_participants: BTreeSet<&str> = rows(&room["participants"])
        .iter()
        .filter(|member| member["joined"] != false)
        .map(|member| text(member, "participant"))
        .filter(|id| !id.is_empty())
        .collect();
    for id in [text(room, "owner"), text(room, "main")] {
        if !id.is_empty() {
            room_participants.insert(id);
        }
    }
    let claim_match = !plan_id.is_empty()
        && tasks.iter().any(|(_, task)| {
            let holder = text(task, "claimed_by_participant");
            let task_plan = text(task, "plan_id");
            !holder.is_empty()
                && room_participants.contains(holder)
                && (task_plan.is_empty() || task_plan == plan_id)
        });

    let call_plan_match = !plan_id.is_empty()
        && calls.iter().any(|call| {
            if call["attention_kind"].is_string() {
                return false;
            }
            let refs = &call["refs"];
            if text(refs, "plan_id") != plan_id {
                return false;
            }
            let ref_rooms = [text(refs, "panel_room_id"), text(refs, "panel_room")];
            if ref_rooms.iter().any(|r| !r.is_empty() && *r != room_id) {
                return false;
            }
            ref_rooms
                .iter()
                .find(|r| !r.is_empty())
                .copied()
                .unwrap_or(room_id)
                == room_id
        });

    let explicitly_bound = room_plan_match || room_task_match || call_plan_match || claim_match;
    if !explicitly_bound {
        return None;
    }

    if !tasks.is_empty() && tasks.iter().all(|(_, task)| plan_task_terminal(task)) {
        return None;
    }

    let stale_tasks: BTreeSet<&str> = attention
        .iter()
        .filter(|item| item.row_id.0 == "task" && item.cls == "stale")
        .map(|item| item.row_id.1.as_str())
        .collect();
    let mut total_count = 0usize;
    let mut verified = 0usize;
    let mut in_review = 0usize;
    let mut claimed = 0usize;
    let mut stale = 0usize;
    for (id, task) in &tasks {
        if plan_task_cancelled(task) {
            continue;
        }
        total_count += 1;
        let st = plan_task_state(task);
        let task_id = attention::plan_task_id(id, task);
        match st {
            "done" => verified += 1,
            "review" => {
                if task["review_lease_abandoned"] == true || task["claim_expired"] == true {
                    stale += 1;
                } else {
                    in_review += 1;
                }
            }
            "claimed" | "running" | "landing" | "held" => {
                if task["claim_expired"] == true || stale_tasks.contains(task_id) {
                    stale += 1;
                } else {
                    claimed += 1;
                }
            }
            _ => {}
        }
    }
    let total = (tasks_present && total_count > 0).then_some(total_count);
    Some(PlanProgress {
        goal: goal.to_owned(),
        total,
        verified,
        in_review,
        claimed,
        stale,
    })
}

#[allow(clippy::too_many_arguments)]
fn header_line(
    report: &Value,
    calls: &[TreeEntry<'_>],
    review: usize,
    attention: &[attention::Item],
    plan: Option<&PlanProgress>,
    view: View,
    columns: usize,
    colored: bool,
) -> String {
    let project = project_title(report);
    let room = &report["room"];
    let room_title_active = report["collection"]["ok"] == true
        && report["capacity_stale"] != true
        && !text(room, "id").is_empty()
        && {
            let cleaned = clean(text(room, "title"), 160);
            let title = cleaned.trim();
            !title.is_empty() && title != "Shared work" && title != project
        };
    let (mut left, right) = if columns < 44 {
        let left_text = format!("OMS · {project}");
        let left = vec![Span::raw(left_text.clone(), columns, Some(SGR_AMBER_BOLD))];
        let mut right = Vec::new();
        if let Some(plan) = plan
            && columns >= 30
        {
            let attn_w = if attention.is_empty() {
                0
            } else {
                width(&format!("!{}", attention.len())) + 1
            };
            let free = columns.saturating_sub(width(&left_text) + attn_w + 2);
            let cap = if room_title_active { 9 } else { 16 };
            if let Some(label) = plan.header_label(free.min(cap)) {
                right.push(Span::raw(label, columns, Some(SGR_CYAN)));
            }
        }
        if !attention.is_empty() {
            if !right.is_empty() {
                right.push(Span::literal(" ", None));
            }
            right.push(Span::raw(
                format!("!{}", attention.len()),
                columns,
                Some(SGR_AMBER_BOLD),
            ));
        }
        (left, right)
    } else {
        let running = calls
            .iter()
            .filter(|entry| {
                matches!(
                    text(entry.call, "state"),
                    "working" | "running" | "starting" | "queued" | "verifying"
                )
            })
            .count();
        let mut status_parts = Vec::new();
        if running > 0 {
            status_parts.push(format!("{running} running"));
        }
        if review > 0 {
            status_parts.push(format!("{review} review"));
        }
        if !attention.is_empty() {
            status_parts.push(format!("{} attention", attention.len()));
        }
        let status_summary = if status_parts.is_empty() {
            if plan.is_some() && calls.is_empty() {
                String::new()
            } else {
                format!("{} calls", calls.len())
            }
        } else {
            status_parts.join(" · ")
        };
        let left = vec![
            Span::literal("OMS", Some(SGR_AMBER_BOLD)),
            Span::literal(" · ", Some(SGR_MUTED)),
            Span::raw(project, columns / 2, Some(SGR_BOLD)),
        ];
        let view_tag = format!("[{}]", view_label(view));
        let summary_budget = columns
            .saturating_sub(
                spans_width(&left) + width(&view_tag) + 4 + if room_title_active { 16 } else { 0 },
            )
            .max(columns / 2);
        let summary = if let Some(plan) = plan {
            let join_with_status = |plan_part: String| {
                if status_summary.is_empty() {
                    plan_part
                } else {
                    format!("{plan_part} · {status_summary}")
                }
            };
            let full = join_with_status(plan.counts_text(false, true));
            let core = join_with_status(plan.counts_text(false, false));
            if width(&full) <= summary_budget {
                full
            } else if width(&core) <= summary_budget {
                core
            } else if let Some(short) = plan.header_label(summary_budget) {
                let combined = join_with_status(short.clone());
                if width(&combined) <= summary_budget {
                    combined
                } else {
                    short
                }
            } else {
                status_summary
            }
        } else {
            status_summary
        };
        let right = vec![
            Span::raw(summary, summary_budget, Some(SGR_CYAN)),
            Span::literal("  ", None),
            Span::raw(view_tag, 16, Some(SGR_MUTED)),
        ];
        (left, right)
    };

    // Give the project and status their existing budgets before adding the Room.
    const SEPARATOR: &str = " · ";
    let available = left_budget(columns, spans_width(&right))
        .saturating_sub(spans_width(&left) + width(SEPARATOR))
        .min(28);
    if available >= 8 && room_title_active {
        let title = clean(text(room, "title"), 160);
        let title = title.trim();
        left.push(Span::literal(SEPARATOR, Some(SGR_MUTED)));
        left.push(Span {
            plain: clip(title, available),
            sgr: Some(SGR_MUTED),
        });
    }
    render_lr(left, right, columns, false, None, colored)
}

#[allow(clippy::too_many_arguments)]
fn render_provider_rows(
    groups: &[usize],
    all: &[Value],
    calls: &[TreeEntry<'_>],
    direct: &BTreeSet<(&str, &str)>,
    nav: &navigation::Navigation,
    selected_worker: Option<usize>,
    offset: usize,
    capacity: usize,
    columns: usize,
    colored: bool,
    sessions: &Value,
    models: &Value,
    accounts: &Value,
    activity: &Value,
    delivery: &Value,
    current: bool,
    agy_roster: &[Value],
    agy_roster_limit: usize,
    provider_rows: &mut [Option<usize>; 3],
    worker_rows: &mut BTreeMap<usize, (String, String)>,
    base: usize,
) -> Vec<String> {
    count_row_render();
    let mut lines = Vec::new();
    let direct_page = calls
        .iter()
        .enumerate()
        .skip(offset)
        .take(capacity)
        .filter(|(_, entry)| is_direct_work(entry.call, direct));
    if direct_page.clone().next().is_some() {
        lines.push(card_top_styled(
            "Direct work",
            None,
            columns,
            colored,
            SGR_AMBER_BOLD,
        ));
        for (index, entry) in direct_page {
            let call = entry.call;
            register_call(worker_rows, base + lines.len(), call);
            let provider = provider_label(text(call, "provider"));
            lines.push(render_card_row(
                call,
                "",
                &format!(" · {provider}"),
                Some(index) == selected_worker,
                columns,
                true,
                colored,
                false,
            ));
        }
        lines.extend(card_bottom(columns, true, colored));
    }
    let provider_start = lines.len();
    for &group in groups {
        let members: Vec<_> = calls
            .iter()
            .map(|entry| entry.call)
            .enumerate()
            .filter(|(_, call)| provider_bucket(call) == group && !is_direct_work(call, direct))
            .collect();
        let label = if group == 3
            && !members.is_empty()
            && members
                .iter()
                .all(|(_, call)| call["attention_kind"].is_string())
        {
            "Room & Plan"
        } else {
            ["Codex", "Claude", "Antigravity", "Other providers"][group]
        };
        let lead = all
            .iter()
            .filter(|call| provider_bucket(call) == group)
            .find(|call| {
                matches!(text(call, "role"), "lead" | "team_lead" | "orchestrator")
                    && oms_panel_core::state::LIVE_STATES.contains(&text(call, "state"))
            });
        let history = all
            .iter()
            .filter(|call| provider_bucket(call) == group && collapsed_history(nav, call))
            .count();
        let connected = crate::orchestrator::PROVIDERS
            .get(group)
            .is_some_and(|provider| {
                crate::rows(sessions)
                    .iter()
                    .any(|item| item.as_str() == Some(provider))
            });
        let session = if connected {
            provider_rows[group] = Some(lines.len());
            format!("Lead chat [{}]", group + 1)
        } else {
            "Activity".into()
        };
        let working = members
            .iter()
            .filter(|(_, call)| {
                matches!(
                    text(call, "state"),
                    "working" | "running" | "starting" | "verifying"
                )
            })
            .count();
        let observed = crate::orchestrator::PROVIDERS
            .get(group)
            .map(|provider| &activity[*provider]);
        let state = observed
            .and_then(|value| value.as_str().or_else(|| value["state"].as_str()))
            .filter(|state| matches!(*state, "working" | "idle" | "unknown"));
        let mut summary = match state {
            Some(state) => state.to_owned(),
            None if working > 0 => format!("{working} working"),
            None if connected => "state unreported".to_owned(),
            None if members.is_empty() => "no visible work".to_owned(),
            None => format!("{} recorded", members.len()),
        };
        let delivered = (connected && current)
            .then(|| crate::orchestrator::PROVIDERS.get(group))
            .flatten()
            .and_then(|provider| match text(delivery, provider) {
                "draft_retained" => Some("draft last seen"),
                "input_cleared_unverified" => Some("input cleared · unverified"),
                "turn_observed" => Some("turn observed"),
                "unverified" => Some("send unverified"),
                _ => None,
            });
        if let Some(label) = delivered {
            summary.push_str(" · ");
            summary.push_str(label);
        }
        let awaiting = if default_overview(nav) {
            all.iter()
                .filter(|call| {
                    provider_bucket(call) == group
                        && !is_direct_work(call, direct)
                        && awaiting_review(call)
                })
                .count()
        } else {
            members
                .iter()
                .filter(|(_, call)| awaiting_review(call))
                .count()
        };
        if awaiting > 0 {
            summary.push_str(&format!(" · {awaiting} review"));
        }
        if history > 0 {
            summary.push_str(&format!(" · {history} past"));
        }
        let model = crate::orchestrator::PROVIDERS
            .get(group)
            .and_then(|provider| models[*provider].as_str())
            .map(|model| formatted_model(model).to_owned())
            .or_else(|| lead.map(model_label))
            .unwrap_or_else(|| "model —".to_owned());
        let full_title = if connected {
            format!("{label} · {session} · {model}")
        } else {
            format!("{label} · {session}")
        };
        let mut title = if connected && width(&full_title) + 6 > columns {
            let label = ["Codex", "Claude", "Antigravity"][group];
            format!("{label} · {session}")
        } else {
            full_title
        };
        let mut detail = summary.clone();
        if current
            && state == Some("working")
            && let Some(body) = observed.and_then(|value| match text(value, "detail") {
                "command" => Some("command running"),
                "terminal_wait" => Some("waiting for terminal"),
                "agent_wait" => Some("waiting for agents"),
                _ => None,
            })
        {
            detail.push_str(" · ");
            detail.push_str(body);
        }
        let visible_agy_roster = &agy_roster[..agy_roster_limit.min(agy_roster.len())];
        if connected && group == 2 && !visible_agy_roster.is_empty() {
            let count = visible_agy_roster.len();
            let label = if count == 1 { "agent" } else { "agents" };
            detail.push_str(&format!(" · {count} {label}"));
        }
        if connected
            && current
            && let Some(provider) = crate::orchestrator::PROVIDERS.get(group)
            && accounts[*provider]["verification"] == "verified"
            && let Some(alias) = accounts[*provider]["account"]
                .as_str()
                .filter(|alias| crate::cli::valid_account_id(alias))
            && width(&title) + width(&detail) + alias.len() + 12 <= columns
        {
            title.push_str(" · @");
            title.push_str(alias);
        }
        let tag = [detail.as_str(), summary.as_str()]
            .into_iter()
            .chain(delivered)
            .find(|tag| width(&title) + width(tag) + 8 <= columns);
        let header = card_top_styled(&title, tag, columns, colored, provider_style(group));
        lines.push(if lines.len() > provider_start {
            header.replacen('╭', "├", 1).replacen('╮', "┤", 1)
        } else {
            header
        });
        let on_page: Vec<_> = members
            .iter()
            .filter(|(index, _)| *index >= offset && *index < offset + capacity)
            .collect();
        let agy_matches: BTreeMap<&str, &Value> = if connected && group == 2 {
            let mut roster_by_title: BTreeMap<&str, Option<&Value>> = BTreeMap::new();
            for child in agy_roster {
                let title = text(child, "title");
                if !title.is_empty() {
                    roster_by_title
                        .entry(title)
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(child));
                }
            }
            let mut native_counts: BTreeMap<&str, usize> = BTreeMap::new();
            for (_, call) in &members {
                if call["native_child"] == true {
                    let title = text(call, "title");
                    if !title.is_empty() {
                        *native_counts.entry(title).or_default() += 1;
                    }
                }
            }
            roster_by_title
                .into_iter()
                .filter_map(|(title, child)| {
                    (native_counts.get(title).copied() == Some(1))
                        .then_some(child.map(|child| (title, child)))
                        .flatten()
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let children: Vec<_> = visible_agy_roster
            .iter()
            .filter(|_| connected && group == 2)
            .filter(|child| {
                !members.iter().any(|(_, call)| {
                    call["native_child"] == true && text(call, "title") == text(child, "title")
                })
            })
            .collect();
        for child in children {
            let title = text(child, "title");
            let elapsed = text(child, "elapsed");
            let card = json!({
                "title":title, "alias":title, "provider":"antigravity",
                "role":text(child,"role").to_ascii_lowercase(), "state":child["state"], "activity":child["activity"],
                "board_elapsed": if elapsed.is_empty() { "—" } else { elapsed }
            });
            lines.push(render_card_row(
                &card, "", "", false, columns, true, colored, true,
            ));
        }
        for &(index, call) in on_page {
            register_call(worker_rows, base + lines.len(), call);
            let projected = (call["native_child"] == true && text(call, "state") != "done")
                .then(|| agy_matches.get(text(call, "title")).copied())
                .flatten()
                .map(|roster_child| project_agy_roster_match(call, roster_child));
            lines.push(render_card_row(
                projected.as_ref().unwrap_or(call),
                "",
                "",
                Some(index) == selected_worker,
                columns,
                true,
                colored,
                group == 2,
            ));
        }
    }
    lines.extend(card_bottom(columns, true, colored));
    lines
}

fn project_agy_roster_match(call: &Value, roster_child: &Value) -> Value {
    let mut projected = call.clone();
    let roster_role = text(roster_child, "role");
    if !roster_role.is_empty() && matches!(text(call, "role"), "" | "subagent" | "worker") {
        projected["role"] = json!(roster_role.to_ascii_lowercase());
    }
    let roster_state = text(roster_child, "state");
    if text(call, "state") == "unknown" && !roster_state.is_empty() {
        projected["state"] = roster_child["state"].clone();
    }
    if !roster_child["activity"].is_null() && text(call, "activity").is_empty() {
        projected["activity"] = roster_child["activity"].clone();
    }
    let roster_elapsed = text(roster_child, "elapsed");
    let current_elapsed = text(call, "board_elapsed");
    if !roster_elapsed.is_empty() && (current_elapsed.is_empty() || current_elapsed == "—") {
        projected["board_elapsed"] = json!(roster_elapsed);
    }
    projected
}

fn register_call(hits: &mut BTreeMap<usize, (String, String)>, row: usize, call: &Value) {
    let participant = text(call, "participant");
    if !participant.is_empty() {
        hits.insert(
            row,
            (participant.to_owned(), text(call, "attempt").to_owned()),
        );
    }
}

pub(crate) fn dock_top(title: &str, columns: usize, colored: bool) -> String {
    if columns >= 44 {
        card_top(title, Some("↕ Drag"), columns, colored)
    } else {
        clip(&format!("↕ {title}"), columns)
    }
}

pub(crate) fn native_dock_bar(
    provider: &str,
    return_label: &'static str,
    columns: usize,
    colored: bool,
) -> String {
    let group = crate::orchestrator::PROVIDERS
        .iter()
        .position(|p| *p == provider)
        .unwrap_or(3);
    render_lr(
        vec![
            Span::literal(return_label, Some(SGR_TEXT)),
            Span::literal("  │  ", Some(SGR_MUTED)),
            Span::raw(
                format!(
                    "▼ {} Orchestrator",
                    crate::orchestrator::provider_name(provider)
                ),
                columns,
                Some(provider_style(group)),
            ),
        ],
        Vec::new(),
        columns,
        false,
        Some(SGR_SEL_BG),
        colored,
    )
}

pub(crate) fn dock_line(line: &str, columns: usize) -> String {
    let available = columns.saturating_sub(4);
    let plain = clean(line, 4096);
    let width_before_clip = width(&plain);
    let clipped;
    let (line, used) = if width_before_clip > available {
        clipped = clip(&plain, available);
        (clipped.as_str(), width(&clipped))
    } else {
        (line, width_before_clip)
    };
    let padding = available - used;
    format!("│ {line}{} │", " ".repeat(padding))
}

pub(crate) fn dock_bottom(columns: usize, colored: bool) -> String {
    card_bottom(columns, true, colored).unwrap_or_default()
}

/// Remaining share of the weekly window from the report's raw reading, which
/// the usage snapshot only validated and rounded. Exact 0% and 100% appear only
/// for exact readings; fractions near either end read `<1%` / `>99%`.
fn weekly_remaining(raw: &Value) -> Option<String> {
    let used =
        oms_panel_core::metrics::number(&raw["weekly_used"]).filter(|used| *used <= 100.0)?;
    let left = 100.0 - used;
    Some(if left == 0.0 || left == 100.0 {
        format!("{left}%")
    } else if left < 1.0 {
        "<1%".into()
    } else if left > 99.0 {
        ">99%".into()
    } else {
        format!("{}%", left.round() as u8)
    })
}

const WEEKLY_PROVIDERS: [(&str, &str); 3] = [
    ("codex", "Codex"),
    ("claude", "Claude"),
    ("antigravity", "AGY"),
];
const WEEKLY_ACTIVITY_LEGEND: &str =
    "Activity · * working · - idle · ? unknown · ~ pending · ! stale";

fn weekly_lead_connected(report: &Value, provider: &str) -> bool {
    rows(&report["provider_sessions"])
        .iter()
        .any(|session| session == provider)
}

#[derive(Clone)]
enum WeeklyReading {
    Reset,
    Stale(Option<f64>),
    Current(String),
    Footer(String),
    Missing,
}

fn weekly_window(provider: &Value) -> Option<&Value> {
    rows(&provider["windows"])
        .iter()
        .find(|window| window["name"] == "weekly")
}

fn weekly_reset_bound(provider: &Value) -> bool {
    // Native refresh overlays a percentage and observation time on the cached report.
    !matches!(
        text(&provider["reading"], "source"),
        "Claude native usage" | "AGY Gemini quota"
    )
}

fn weekly_reading(raw: &Value, provider: &Value, now: f64) -> WeeklyReading {
    let reading = &provider["reading"];
    let weekly = weekly_window(provider).is_some();
    if weekly
        && weekly_reset_bound(provider)
        && oms_panel_core::metrics::stamp(&raw["weekly_resets_at"])
            .is_some_and(|reset| reset <= now)
    {
        WeeklyReading::Reset
    } else if reading["stale"] == true {
        WeeklyReading::Stale(reading["age_seconds"].as_f64())
    } else if weekly && let Some(left) = weekly_remaining(raw) {
        WeeklyReading::Current(left)
    } else {
        WeeklyReading::Missing
    }
}

fn local_claude_weekly(report: &Value, now: f64) -> Option<(WeeklyReading, f64, f64)> {
    if report["collection"]["ok"] != true
        || report["collection"]["pending"] == true
        || report["capacity_stale"] == true
    {
        return None;
    }
    let local = &report["claude_local_weekly"];
    let observed = local["callback_observed_at"].as_f64()?;
    let reset = local["resets_at"].as_f64()?;
    // The Board clock is whole seconds. The reader already checked the exact callback time.
    if !(0.0..=now).contains(&observed.floor())
        || reset <= observed
        || reset > observed + 7.0 * 86400.0
    {
        return None;
    }
    let remaining = weekly_remaining(local)?;
    let age = (now - observed).max(0.0);
    let reading = if reset <= now {
        WeeklyReading::Reset
    } else if age > oms_panel_core::metrics::READING_FRESH_SECONDS {
        WeeklyReading::Stale(Some(age))
    } else {
        WeeklyReading::Current(remaining)
    };
    Some((reading, age, reset))
}

fn agy_footer_weekly(report: &Value) -> Option<&str> {
    if report["collection"]["ok"] != true
        || report["collection"]["pending"] == true
        || report["capacity_stale"] == true
    {
        return None;
    }
    report["agy_footer_weekly"]["remaining"]
        .as_str()
        .filter(|value| crate::provider_activity::valid_agy_footer_remaining(value))
}

fn report_held(report: &Value) -> bool {
    report["collection"]["pending"] == true
        || report["capacity_stale"] == true
        || report.get("collection").is_some_and(|c| c["ok"] != true)
}

fn verified_codex_weekly_account(report: &Value, now: f64) -> Option<&str> {
    use oms_panel_core::metrics::{percent, stamp};

    if !now.is_finite() || now < 0.0 || report["collection"]["ok"] != true || report_held(report) {
        return None;
    }
    let alias = report["codex_weekly_account"]
        .as_str()
        .filter(|id| crate::cli::valid_account_id(id))?;
    let status = &report["provider_status"]["codex"];
    if status["source"].as_str() != Some("session log")
        || status.get("weekly_native_observed_at").is_some()
        || status.get("weekly_lead_observed_at").is_some()
    {
        return None;
    }
    let row = status["accounts"].get(alias).filter(|v| v.is_object())?;
    if row["source"].as_str() != Some("session log")
        || percent(&status["weekly_used"])? != percent(&row["weekly_used"])?
    {
        return None;
    }
    let obs = stamp(&row["observed_at"]).filter(|t| t.is_finite() && (0.0..=now).contains(t))?;
    if stamp(&status["observed_at"]) != Some(obs) {
        return None;
    }
    let reset = stamp(&row["resets_at"]).filter(|r| r.is_finite() && *r > obs)?;
    if stamp(&status["weekly_resets_at"]) != Some(reset) {
        return None;
    }
    Some(alias)
}

fn registered_codex_account_lines(report: &Value, now: f64) -> Vec<String> {
    use oms_panel_core::metrics::{READING_FRESH_SECONDS, stamp};

    if !now.is_finite() || now < 0.0 || report_held(report) {
        return Vec::new();
    }
    let Some(accounts) = report["provider_status"]["codex"]["accounts"].as_object() else {
        return Vec::new();
    };
    let valid: Vec<(&str, &Value)> = accounts
        .iter()
        .filter(|(alias, row)| crate::cli::valid_account_id(alias) && row.is_object())
        .map(|(alias, row)| (alias.as_str(), row))
        .collect();
    if valid.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(valid.len() + 1);
    lines.push("  Registered Codex accounts (Codex session log)".to_owned());
    for (alias, row) in valid {
        let reading = (row["source"].as_str() == Some("session log"))
            .then(|| {
                let left = weekly_remaining(row)?;
                let observed = stamp(&row["observed_at"])
                    .filter(|t| t.is_finite() && (0.0..=now).contains(t))?;
                let reset = stamp(&row["resets_at"]).filter(|r| r.is_finite() && *r > observed)?;
                Some((left, observed, reset))
            })
            .flatten();
        let line = match reading {
            Some((left, observed, reset)) => {
                let age = crate::usage::format_age((now - observed).max(0.0));
                if reset <= now {
                    format!("  @{alias} · window expired · Log age {age} · Reset elapsed")
                } else if now - observed > READING_FRESH_SECONDS {
                    format!("  @{alias} · stale · Log age {age} · Reset unreported")
                } else {
                    let rem = crate::usage::format_duration(reset - now);
                    format!("  @{alias} · {left} left · Log age {age} · Reset in {rem}")
                }
            }
            None => {
                format!("  @{alias} · missing weekly reading · Log age — · Reset unreported")
            }
        };
        lines.push(line);
    }
    lines
}

pub(crate) fn weekly_usage_detail(report: &Value, now: f64) -> String {
    let usage = crate::usage::snapshot(report, now);
    let mut detail = WEEKLY_PROVIDERS
        .into_iter()
        .map(|(name, label)| {
            if name == "claude" && let Some((reading, age, reset)) = local_claude_weekly(report, now) {
                let age = crate::usage::format_age(age);
                let (state, reset) = match reading {
                    WeeklyReading::Current(left) => (format!("{left} left"),
                        format!("in {}", crate::usage::format_duration(reset - now))),
                    WeeklyReading::Footer(_) => unreachable!("local Claude reading never emits footer"),
                    WeeklyReading::Reset => ("window expired".into(), "elapsed".into()),
                    WeeklyReading::Stale(_) => ("stale".into(), "unreported".into()),
                    WeeklyReading::Missing => ("missing weekly reading".into(), "unreported".into()),
                };
                return format!("{label} · {state}\n  Source Claude local statusline · Callback age {age} · Reset {reset}");
            }
            let provider = &usage["providers"][name];
            let base = weekly_reading(&report["provider_status"][name], provider, now);
            if name == "antigravity"
                && matches!(base, WeeklyReading::Missing | WeeklyReading::Stale(_))
                && let Some(left) = agy_footer_weekly(report)
            {
                return format!(
                    "{label} · {left} left\n  Source AGY native footer · Age unknown · Reset unknown"
                );
            }
            let source = match (name, text(&provider["reading"], "source")) {
                ("codex", "Codex session log") => "Codex session log",
                ("claude", "HUD cache") => "HUD cache",
                ("claude", "Claude lead HUD") => "Claude lead HUD",
                ("claude", "Claude native usage") => "Claude native usage",
                ("antigravity", "AGY Gemini quota") => "AGY Gemini quota",
                _ => "unreported",
            };
            let (state, reset) = match base {
                WeeklyReading::Reset => ("window expired".into(), "elapsed".into()),
                WeeklyReading::Stale(age) => (
                    age.map_or_else(
                        || "stale".into(),
                        |age| format!("stale ({})", crate::usage::format_age(age)),
                    ),
                    "unreported".into(),
                ),
                WeeklyReading::Current(left) => (
                    format!("{left} left"),
                    weekly_window(provider)
                        .filter(|_| weekly_reset_bound(provider))
                        .and_then(|window| window["resets_in"].as_str())
                        .map_or_else(|| "unreported".into(), |time| format!("in {time}")),
                ),
                WeeklyReading::Footer(_) => unreachable!("weekly_reading never emits footer"),
                WeeklyReading::Missing => ("missing weekly reading".into(), "unreported".into()),
            };
            let headline = if name == "codex"
                && let Some(alias) = verified_codex_weekly_account(report, now)
            {
                format!("{label} · @{alias} · {state}")
            } else {
                format!("{label} · {state}")
            };
            let mut block = format!("{headline}\n  Source {source} · Reset {reset}");
            if name == "codex" {
                let accounts = registered_codex_account_lines(report, now);
                if !accounts.is_empty() {
                    block.push('\n');
                    block.push_str(&accounts.join("\n"));
                }
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n");
    if WEEKLY_PROVIDERS
        .iter()
        .any(|(name, _)| weekly_lead_connected(report, name))
    {
        detail.push('\n');
        detail.push_str(WEEKLY_ACTIVITY_LEGEND);
    }
    detail
}

/// Weekly remaining per provider. The usage snapshot decides whether a weekly
/// reading exists and is fresh; a reading whose reset time has passed no longer
/// applies (`reset`), a stale one shows `stale` (with its age when room allows)
/// and never its number, and no reading shows `—`.
fn provider_line(
    report: &Value,
    usage: &Value,
    now: f64,
    columns: usize,
    colored: bool,
    native_activity_current: bool,
) -> (String, Option<usize>) {
    // 0 tiny, 1 compact, 2 medium, 3 full; fall back while the line overflows.
    let widest = match columns {
        0..28 => 0,
        28..56 => 1,
        56..100 => 2,
        _ => 3,
    };
    let local_claude = local_claude_weekly(report, now);
    // Lead activity rides beside each connected provider's weekly value. The
    // orchestrator writes only "working" and omits the key when unsure, so a
    // connected lead without an explicit state is `unknown`, never `idle`.
    // A native sample accepted by this frame can precede a queued follow-up.
    // That follow-up keeps action authority pending without changing this sample.
    // Tier 0 and unconnected providers have no activity suffix.
    let carried_activity = if report["collection"]["ok"] != true || report["capacity_stale"] == true
    {
        Some("stale")
    } else if report["collection"]["pending"] == true && !native_activity_current {
        Some("pending")
    } else {
        None
    };
    let activity = WEEKLY_PROVIDERS.map(|(name, _)| {
        if !weekly_lead_connected(report, name) {
            return None;
        }
        Some(carried_activity.unwrap_or_else(|| {
            let observed = &report["provider_activity"][name];
            observed
                .as_str()
                .or_else(|| observed["state"].as_str())
                .filter(|state| matches!(*state, "working" | "idle"))
                .unwrap_or("unknown")
        }))
    });
    let readings = WEEKLY_PROVIDERS.map(|(name, _)| match &local_claude {
        Some((reading, _, _)) if name == "claude" => reading.clone(),
        _ => {
            let reading = weekly_reading(
                &report["provider_status"][name],
                &usage["providers"][name],
                now,
            );
            if name == "antigravity"
                && matches!(reading, WeeklyReading::Missing | WeeklyReading::Stale(_))
                && let Some(left) = agy_footer_weekly(report)
            {
                WeeklyReading::Footer(left.to_owned())
            } else {
                reading
            }
        }
    });
    #[derive(Clone, Copy)]
    enum ActivityDisplay {
        Words,
        Markers,
        Hidden,
    }
    use ActivityDisplay::{Hidden, Markers, Words};
    // No account label: the active-account match is ambient (which home is
    // active now), not bound to the process that produced this reading.
    let line = |tier: usize, display: ActivityDisplay, compact_value: bool| {
        let values = WEEKLY_PROVIDERS
            .into_iter()
            .enumerate()
            .map(|(index, (name, full))| {
                let provider = &usage["providers"][name];
                let footer = matches!(&readings[index], WeeklyReading::Footer(_));
                let label = match tier {
                    0 if footer => "Af",
                    0 => ["C", "L", "A"][index],
                    1 if footer => "AGYf",
                    1 => ["Cdx", "Cld", "AGY"][index],
                    _ if footer => "AGY footer",
                    _ if name == "antigravity"
                        && provider["reading"]["source"] == "AGY Gemini quota" =>
                    {
                        "AGY Gemini"
                    }
                    _ => full,
                };
                let value = match &readings[index] {
                    WeeklyReading::Reset => if tier == 0 { "?" } else { "reset" }.into(),
                    WeeklyReading::Stale(age) => match (tier, age) {
                        (0, _) => "!".into(),
                        (3, Some(age)) => format!("stale {}", crate::usage::format_age(*age)),
                        _ => "stale".into(),
                    },
                    WeeklyReading::Current(left) | WeeklyReading::Footer(left)
                        if tier >= 2 && !compact_value && left == "0%" =>
                    {
                        "0% exhausted".into()
                    }
                    WeeklyReading::Current(left) | WeeklyReading::Footer(left) => left.clone(),
                    WeeklyReading::Missing => "—".into(),
                };
                let tail = match (activity[index], display) {
                    (Some(state), Words) => format!(" · {state}"),
                    (Some(state), Markers) => match state {
                        "working" => "*",
                        "idle" => "-",
                        "pending" => "~",
                        "stale" => "!",
                        _ => "?",
                    }
                    .to_owned(),
                    _ => String::new(),
                };
                format!("{label}{}{value}{tail}", if tier < 2 { "" } else { " " })
            })
            .collect::<Vec<_>>();
        format!(
            "{}{}",
            ["W ", "Wk left ", "Weekly left · ", "Weekly left · "][tier],
            values.join(if tier < 2 { " " } else { " │ " })
        )
    };
    // Keep a complete strip. Compact values can preserve rich provider labels
    // and activity when "exhausted" or a stale age would make the strip too wide.
    let has_activity = activity.iter().any(Option::is_some);
    let plain = [
        (3, Words, false),
        (3, Markers, false),
        (2, Words, false),
        (2, Markers, false),
        (2, Markers, true),
        (1, Markers, false),
        (3, Hidden, false),
        (2, Hidden, false),
        (1, Hidden, false),
    ]
    .into_iter()
    .filter(|(tier, display, _)| *tier <= widest && (has_activity || matches!(display, Hidden)))
    .map(|(tier, display, compact_value)| line(tier, display, compact_value))
    .find(|plain| width(plain) <= columns)
    .unwrap_or_else(|| line(0, Hidden, false));
    let columns_used = width(&plain);
    let complete = (columns_used <= columns).then_some(columns_used);
    let line = render_lr(
        vec![Span::raw(plain, 1_048_576, Some(SGR_MUTED))],
        vec![],
        columns,
        false,
        None,
        colored,
    );
    (line, complete)
}

fn main_elapsed(report: &Value, main: &Value) -> Option<String> {
    let id = text(main, "attempt");
    let room = text(&report["room"], "id");
    let attempt = &report["room_attempts"][id];
    if id.is_empty()
        || report["collection"]["ok"] != true
        || report["collection"]["pending"] == true
        || report["capacity_stale"] == true
        || report["room"]["closed"] == true
        || !report["room"]["successor"].is_null()
        || main["joined"] != true
        || attempt["attempt_id"] != id
        || attempt["tool"] != "panel-main"
        || attempt["refs"]["panel_role"] != "main"
        || attempt["terminal"] != false
        || attempt["provider"] != main["provider"]
        || !event_room_matches(room, &attempt["refs"])
        || oms_panel_core::state::main_participant(attempt) != text(main, "participant")
        || !oms_panel_core::state::LIVE_STATES.contains(&text(attempt, "state"))
    {
        return None;
    }
    let observed = oms_panel_core::metrics::stamp(&report["generated_at"])?;
    let started = oms_panel_core::metrics::stamp(&attempt["created_at"])?;
    elapsed_between(started, observed)
}

fn elapsed_between(started: f64, ended: f64) -> Option<String> {
    (started > 0.0)
        .then(|| elapsed_label(ended - started))
        .flatten()
}

fn elapsed_label(elapsed: f64) -> Option<String> {
    (elapsed.is_finite() && elapsed >= 0.0).then(|| {
        if elapsed < 60.0 {
            format!("{}s", elapsed as u64)
        } else {
            crate::usage::format_duration(elapsed)
        }
    })
}

fn native_recency_current(report: &Value, call: &Value) -> bool {
    report["collection"]["ok"] == true
        && report["collection"]["pending"] != true
        && report["capacity_stale"] != true
        && call["state"] == "unknown"
}

fn native_seen_age(call: &Value, observed: f64) -> Option<u64> {
    let observed_ms = call["seen_observed_ms"]
        .as_u64()?
        .max((observed * 1000.0).round() as u64);
    crate::native_children::codex_recent_age(call["seen_at"].as_u64()?, observed_ms)
}

fn codex_board_recency(
    report: &Value,
    call: &Value,
    observed: f64,
) -> Option<crate::native_children::CodexChildRecency> {
    use crate::native_children::{CodexChildRecency, codex_recent_age};

    if call["provider"] != "codex"
        || call["native_child"] != true
        || !native_recency_current(report, call)
    {
        return None;
    }
    if native_seen_age(call, observed).is_some() {
        return call["seen_at"].as_u64().map(CodexChildRecency::Interacted);
    }
    let proof = &call["codex_recent_creation"];
    let at = proof["at_ms"].as_u64()?;
    let sampled = proof["observed_ms"].as_u64()?;
    codex_recent_age(at, sampled)?;
    codex_recent_age(at, sampled.max((observed * 1000.0).round() as u64))?;
    Some(CodexChildRecency::Created(at))
}

/// A Codex child whose parent recently recorded an interaction with it. Its
/// `state` stays "unknown"; only the elapsed cell says "seen".
fn native_seen_label(report: &Value, call: &Value, observed: f64) -> Option<String> {
    let age = native_recency_current(report, call)
        .then(|| native_seen_age(call, observed))
        .flatten()?;
    Some(format!("seen {}s", age / 1000))
}

/// Age since a verified start, 9 cells at most; `~` marks a coarsened value.
fn started_age_label(age: f64) -> Option<String> {
    let exact = elapsed_label(age)? + " ago";
    Some(if exact.chars().count() <= 9 {
        exact
    } else {
        format!("~{} ago", crate::usage::format_age(age))
    })
}

fn worker_elapsed(report: &Value, call: &Value, observed: f64) -> Option<String> {
    if call["native_assignment_transport"].is_string() {
        return None;
    }
    use oms_panel_core::metrics::stamp;
    let state = text(call, "state");
    if call["native_child"] == true {
        if call["provider"] == "codex"
            && let Some(label) = native_seen_label(report, call, observed)
        {
            return Some(label);
        }
        // A duration needs a confirmed start and end on a terminal row. A live
        // row with only a verified start shows its age ("3m ago"): time since
        // start, not run time, and no claim that it is working.
        if oms_panel_core::state::TERMINAL_STATES.contains(&state) {
            return elapsed_between(stamp(&call["started_at"])?, stamp(&call["ended_at"])?);
        }
        let started = stamp(&call["started_at"])?;
        return (report["collection"]["ok"] == true
            && report["collection"]["pending"] != true
            && report["capacity_stale"] != true
            && call["ended_at"].is_null()
            && started > 0.0
            && started <= observed)
            .then(|| started_age_label(observed - started))
            .flatten();
    }
    let room = text(&report["room"], "id");
    if text(call, "attempt").is_empty()
        || (!room.is_empty() && !event_room_matches(room, &call["refs"]))
    {
        return None;
    }
    let terminal = oms_panel_core::state::TERMINAL_STATES.contains(&state)
        || matches!(state, "review" | "guard_stop");
    if terminal && let Some(seconds) = call["duration_ms"].as_f64().map(|ms| ms / 1_000.0) {
        return elapsed_label(seconds);
    }
    let started = stamp(&call["started_at"])?;
    if terminal {
        let ended = stamp(&call["returned_at"]).or_else(|| stamp(&call["ended_at"]))?;
        return elapsed_between(started, ended);
    }
    (matches!(state, "starting" | "running" | "working" | "verifying")
        && report["collection"]["ok"] == true
        && report["collection"]["pending"] != true
        && report["capacity_stale"] != true)
        .then(|| elapsed_between(started, observed))
        .flatten()
}

fn valid_checkout_label(observation: &Value) -> Option<String> {
    let detached = match observation.get("detached") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return None,
    };
    let branch = if detached {
        match observation.get("branch") {
            None | Some(Value::Null) => "detached",
            Some(_) => return None,
        }
    } else {
        observation.get("branch").and_then(Value::as_str)?
    };
    let worktree = observation.get("worktree").and_then(Value::as_str)?;
    if !branch.is_empty()
        && !worktree.is_empty()
        && !worktree.contains(['/', '\\'])
        && !branch.starts_with(['/', '\\'])
        && !branch.chars().any(char::is_whitespace)
        && clean(worktree, 4096) == worktree
        && clean(branch, 4096) == branch
    {
        Some(format!("Branch {branch} · Worktree {worktree}"))
    } else {
        None
    }
}

fn checkout_line(
    report: &Value,
    main: &Value,
    columns: usize,
    boxed: bool,
    colored: bool,
) -> String {
    let checkout = if report["collection"]["ok"] == true
        && report["capacity_stale"] != true
        && !text(main, "attempt").is_empty()
    {
        if report["collection"]["pending"] != true {
            let observation = &report["board_checkout"];
            (observation["attempt"] == main["attempt"])
                .then(|| valid_checkout_label(observation))
                .flatten()
        } else {
            // Pending reports never trust raw `board_checkout`; they require an
            // explicit display-only last-known proof bound to this room and Main.
            let proof = &report["board_checkout_last_known"];
            let room_id = text(&report["room"], "id");
            (proof["display_only"] == true
                && !room_id.is_empty()
                && report["room"]["closed"] != true
                && report["room"]["successor"].is_null()
                && proof["room"] == report["room"]["id"]
                && proof["attempt"] == main["attempt"])
                .then(|| valid_checkout_label(proof))
                .flatten()
        }
        .unwrap_or_else(|| "Checkout unknown".to_owned())
    } else {
        "Checkout unknown".to_owned()
    };
    let inner = columns - if boxed { 4 } else { 0 };
    let elapsed = (columns < 56)
        .then(|| main_elapsed(report, main))
        .flatten()
        .map(|time| format!("Main elapsed {time}"));
    let right = elapsed
        .filter(|time| width(&checkout) + width(time) < inner)
        .map(|time| Span::raw(time, inner, Some(SGR_MUTED)))
        .into_iter()
        .collect();
    card_row_lr(
        vec![Span::raw(clip(&checkout, inner), inner, Some(SGR_MUTED))],
        right,
        columns,
        boxed,
        None,
        colored,
    )
}

fn legend_line(colored: bool, columns: usize) -> String {
    let spans = vec![
        Span::literal("■ Main  ", Some(SGR_AMBER_BOLD)),
        Span::literal("◆ Gate  ", Some(SGR_VIOLET_BOLD)),
        Span::literal("◇ Counsel  ", Some(SGR_SKY_BOLD)),
        Span::literal("● Workers", Some(SGR_SAGE_BOLD)),
    ];
    render_lr(spans, vec![], columns, false, None, colored)
}

#[allow(clippy::too_many_arguments)]
fn render_main_section(
    plan: Option<&PlanProgress>,
    main: Option<&Value>,
    selected: bool,
    columns: usize,
    use_cards: bool,
    colored: bool,
    allow_switch: bool,
    compact_goal: bool,
) -> (Vec<String>, Option<(usize, usize)>) {
    if main.is_none() && plan.is_none() {
        return (vec![], None);
    }
    let model = main
        .map(|call| {
            if call["model_state"].is_string() {
                match model_label(call).as_str() {
                    "model —" => String::new(),
                    label => label.to_owned(),
                }
            } else {
                formatted_model(text(call, "model")).to_owned()
            }
        })
        .unwrap_or_default();
    let provider = main
        .map(|call| provider_label(text(call, "provider")))
        .unwrap_or("");
    let state = main.map(|call| text(call, "state")).unwrap_or("");
    let mut out = Vec::new();
    let mut switch_cols = None;
    if use_cards {
        let (top, cols) = if allow_switch
            && main.is_some_and(|call| {
                call["provider"] != "claude"
                    || crate::native_panel::claude_assisted_exit_available()
            }) {
            card_top_with_action(
                "Overall Main",
                Some(state),
                &MAIN_SWITCH_LABELS,
                columns,
                colored,
                SGR_MAIN_TITLE,
            )
        } else {
            (
                card_top_styled(
                    "Overall Main",
                    Some(state),
                    columns,
                    colored,
                    SGR_MAIN_TITLE,
                ),
                None,
            )
        };
        switch_cols = cols;
        out.push(top);
    }
    let identity = if main.is_none() {
        "No active Main".to_owned()
    } else if use_cards {
        [provider, model.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        let label = if columns >= 28 {
            "Overall Main"
        } else {
            "Main"
        };
        if model.is_empty() {
            format!("{label} {provider}")
        } else {
            format!("{label} {model}")
        }
    };
    let marker = if selected { "▸ " } else { "  " };
    let inner = columns - if use_cards { 4 } else { 0 };
    let mut right = main.map_or_else(Vec::new, |call| {
        let identity_width = width(&identity);
        let identity_reserve = if !use_cards && columns < 28 {
            // Keep "Main" and a model hint beside the complete compact total.
            identity_width.min(10)
        } else {
            identity_width.min(18)
        };
        token_span(
            call,
            left_budget(inner, width(marker) + identity_reserve),
            columns,
            use_cards,
        )
    });
    if !use_cards
        && !state.is_empty()
        && width(marker) + width(&identity) + width(state) + spans_width(&right) + 3 <= inner
    {
        right.splice(
            0..0,
            [
                Span::raw(state, columns, Some(SGR_CYAN)),
                Span::literal("  ", None),
            ],
        );
    }
    let mut left = vec![Span::literal(marker, Some(SGR_AMBER_BOLD))];
    if !identity.is_empty() {
        left.push(Span::raw(identity, columns, Some(SGR_AMBER_BOLD)));
    }
    if let Some(call) = main
        && let title = text(call, "title")
        && !title.is_empty()
    {
        if left.len() > 1 {
            left.push(Span::literal(" · ", Some(SGR_MUTED)));
        }
        left.push(Span::raw(title, columns, Some(SGR_TEXT)));
    }
    out.push(card_row_lr(
        left,
        right,
        columns,
        use_cards,
        (colored && selected).then_some(SGR_SEL_BG),
        colored,
    ));
    if let Some(plan) = plan {
        let show_bar = columns >= 72;
        let progress_budget = inner.saturating_sub(7).max(7);
        let progress = plan.goal_progress_label(progress_budget, show_bar);
        let inline_fit = width("Goal: ") + width(&plan.goal) + 2 + width(&progress) <= inner;
        if inline_fit || compact_goal {
            let right_budget = inner.saturating_sub(14).max(7);
            let inline_progress = if inline_fit {
                progress
            } else {
                plan.goal_progress_label(right_budget, false)
            };
            out.push(card_row_lr(
                vec![Span::raw(
                    format!("Goal: {}", plan.goal),
                    1_048_576,
                    Some(SGR_TEXT),
                )],
                vec![Span::raw(inline_progress, inner, Some(SGR_CYAN))],
                columns,
                use_cards,
                None,
                colored,
            ));
        } else {
            out.push(card_row_lr(
                vec![Span::literal("Goal:", Some(SGR_GOLD_BOLD))],
                vec![Span::raw(progress, progress_budget, Some(SGR_CYAN))],
                columns,
                use_cards,
                None,
                colored,
            ));
            let indent = if use_cards { "  " } else { " " };
            let wrap_width = inner.saturating_sub(width(indent)).max(1);
            let mut wrapped = wrap_words(&plan.goal, wrap_width);
            if wrapped.len() > 2 {
                wrapped.truncate(2);
                wrapped[1] = clip(
                    &format!("{}…", wrapped[1].trim_end_matches('…')),
                    wrap_width,
                );
            }
            for part in wrapped {
                out.push(card_row_lr(
                    vec![
                        Span::literal(indent, None),
                        Span::raw(part, wrap_width, Some(SGR_TEXT)),
                    ],
                    vec![],
                    columns,
                    use_cards,
                    None,
                    colored,
                ));
            }
        }
    }
    if let Some(bottom) = card_bottom(columns, use_cards, colored) {
        out.push(bottom);
    }
    (out, switch_cols)
}

pub(crate) fn main_switch_frame(
    menu: &navigation::MainSwitchMenu,
    columns: usize,
    height: usize,
) -> BoardFrame {
    let mut lines = vec![clip(
        &format!("Switch Main · current {}", menu.current),
        columns,
    )];
    if let Some(choice) = menu.selected.and_then(|index| menu.choices.get(index)) {
        lines.extend(wrap(
            &format!("Replace Main with {choice}?"),
            columns.max(1),
        ));
        lines.push("Y Confirm · Esc Cancel".into());
    } else {
        for (index, choice) in menu.choices.iter().enumerate() {
            lines.push(format!("{}  {choice}", index + 1));
        }
        lines.push("Number Select · Esc Cancel".into());
    }
    lines.truncate(height);
    lines.resize(height, String::new());
    for line in &mut lines {
        *line = clip(line, columns);
    }
    BoardFrame {
        lines,
        provider_rows: [None; 3],
        worker_rows: Default::default(),
        weekly_usage: None,
        main_switch: None,
    }
}

fn render_help_card(
    columns: usize,
    inner_width: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let rows_data: &[(&str, &str)] = &[
        ("Up/Down · PgUp/Dn", "Select call / scroll list"),
        ("Home · End", "First / last call"),
        ("Enter", "Open chat or task results"),
        ("D", "Read selected Call Detail and tokens"),
        ("f", "Open selected task results"),
        ("Click title · 1/2/3", "Read Codex, Claude or AGY chat"),
        ("s", "Choose another Main and confirm the switch"),
        ("Click worker · c", "Read worker details or latest council"),
        ("Click weekly · w", "Read weekly usage details"),
        ("U", "Refresh Claude and AGY weekly usage"),
        (
            "* · - · ? · ~ · !",
            "Lead working / idle / unknown / pending / stale",
        ),
        ("R · r", "All results / mark attention read"),
        ("Tab · g · t", "Overview / all tasks · relationship tree"),
        ("/ · Esc", "Filter calls / clear or return to board"),
        ("b · k", "Toggle attention filter / health"),
        ("h · j", "Joined chat notice / resource counts"),
        ("u · 9", "Refresh"),
        ("? · F12 · q", "Help / Quit"),
    ];
    let mut out = vec![card_top(
        "Keyboard Shortcuts",
        Some("Esc / ? close"),
        columns,
        colored,
    )];
    for (keys, desc) in rows_data {
        let left = vec![Span::raw(format!("{keys:<18}"), 24, Some(SGR_AMBER_BOLD))];
        let right = vec![Span::raw(
            *desc,
            inner_width.saturating_sub(20),
            Some(SGR_TEXT),
        )];
        out.push(card_row_lr(left, right, columns, use_cards, None, colored));
    }
    if let Some(bottom) = card_bottom(columns, use_cards, colored) {
        out.push(bottom);
    }
    out
}

fn write_slots_line(report: &Value, columns: usize) -> Option<String> {
    let slots = report.get("board_write_slots")?;
    let unavailable = || Some("Write slots unavailable".to_owned());
    if report["collection"]["ok"] != true || report["capacity_stale"] == true {
        return unavailable();
    }
    let Some(used) = slots["used"].as_u64() else {
        return unavailable();
    };
    let Some(limit) = slots["limit"].as_u64() else {
        return unavailable();
    };
    let counts = [
        slots["active"].as_u64(),
        slots["awaiting"].as_u64(),
        slots["unconfirmed"].as_u64(),
    ];
    let [Some(active), Some(awaiting), Some(unconfirmed)] = counts else {
        return unavailable();
    };
    let line = |title, labels: [&str; 3], separator, between| {
        let mut parts = vec![format!("{title} {used}/{limit}")];
        for (count, label) in [active, awaiting, unconfirmed].into_iter().zip(labels) {
            if count > 0 {
                parts.push(format!("{count}{separator}{label}"));
            }
        }
        parts.join(between)
    };
    let wide = line(
        "Write slots",
        ["active", "awaiting decision", "unconfirmed"],
        " ",
        " · ",
    );
    if width(&wide) <= columns {
        return Some(wide);
    }
    let compact = line("Write", ["act", "dec", "unconf"], " ", " · ");
    if width(&compact) <= columns {
        return Some(compact);
    }
    Some(line("Write", ["act", "dec", "unconf"], "", " "))
}

fn render_health_card(
    report: &Value,
    all: &[Value],
    columns: usize,
    inner_width: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let now = oms_panel_core::timestamp() as f64;
    let health = oms_panel_core::health::project(report, now);
    let alerts = oms_panel_core::health::alerts(report, now);
    let mut out = vec![card_top(
        "Health & Capacity",
        Some("Esc / k close"),
        columns,
        colored,
    )];
    if let Some(line) = write_slots_line(report, inner_width) {
        out.push(card_row_lr(
            vec![Span::raw(line, inner_width, Some(SGR_TEXT))],
            vec![],
            columns,
            use_cards,
            None,
            colored,
        ));
    }
    let status = if report["collection"]["ok"] == true && report["capacity_stale"] != true {
        ("ok", SGR_SAGE_BOLD)
    } else {
        ("stale", SGR_CORAL_BOLD)
    };
    out.push(card_row_lr(
        vec![Span::literal("Collection", Some(SGR_MUTED))],
        vec![Span::literal(status.0, Some(status.1))],
        columns,
        use_cards,
        None,
        colored,
    ));
    let pairs: Vec<(String, String)> = [
        (
            "Live calls",
            health["calls"]["live"]
                .as_u64()
                .or_else(|| health["live"].as_u64()),
        ),
        ("Running", health["running"].as_u64()),
        ("Awaiting", health["awaiting"].as_u64()),
        (
            "Active room",
            report["room"]["participants"].as_array().map(|_| {
                all.iter()
                    .filter(|call| {
                        call["joined"] != false
                            && call["room_only_stale"] != true
                            && call["room_only_history"] != true
                            && call["completed_task"] != true
                            && !call["returned_at"].is_string()
                            && oms_panel_core::state::LIVE_STATES.contains(&text(call, "state"))
                    })
                    .count() as u64
            }),
        ),
        (
            "Plan tasks",
            report["plan"]["tasks"].as_object().map(|v| v.len() as u64),
        ),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|n| (k.to_owned(), n.to_string())))
    .collect();
    for (k, v) in pairs {
        out.push(card_row_lr(
            vec![Span::raw(k, 24, Some(SGR_MUTED))],
            vec![Span::raw(v, 24, Some(SGR_TEXT))],
            columns,
            use_cards,
            None,
            colored,
        ));
    }
    if alerts.is_empty() {
        out.push(card_row_lr(
            vec![Span::literal("No active capacity alerts", Some(SGR_SAGE))],
            vec![],
            columns,
            use_cards,
            None,
            colored,
        ));
    } else {
        for alert in alerts.into_iter().take(6) {
            for wrapped in wrap(&alert, inner_width.saturating_sub(4).max(1)) {
                out.push(card_row_lr(
                    vec![
                        Span::literal("! ", Some(SGR_GOLD_BOLD)),
                        Span::raw(wrapped, inner_width, Some(SGR_TEXT)),
                    ],
                    vec![],
                    columns,
                    use_cards,
                    None,
                    colored,
                ));
            }
        }
    }
    if let Some(bottom) = card_bottom(columns, use_cards, colored) {
        out.push(bottom);
    }
    out
}

/// Overall Main facts already on its Board row. Absent evidence reads
/// "unavailable" rather than a guess.
pub(crate) fn main_summary(call: &Value) -> Vec<(&'static str, String)> {
    let (observed, source) = headline_tokens(call);
    let tokens = observed["total"]
        .as_u64()
        .map_or_else(String::new, |total| {
            let retained = if observed["last_observed"] == true {
                " · last observed"
            } else {
                ""
            };
            format!("{total} · {source}{retained}")
        });
    [
        (
            "Provider",
            provider_label(text(call, "provider")).to_owned(),
        ),
        ("Model", model_detail(call)),
        ("State", text(call, "state").to_owned()),
        ("Work", text(call, "title").to_owned()),
        ("Task", text(call, "task_id").to_owned()),
        ("Tokens", tokens),
    ]
    .into_iter()
    .map(|(label, value)| match value.trim() {
        "" => (label, "unavailable".to_owned()),
        value => (label, value.to_owned()),
    })
    .collect()
}

/// `main_summary` as Provider, Model, State, Tokens, Task, Work pieces: the
/// identity values read bare unless unavailable.
fn main_fact_pieces(call: &Value) -> [String; 6] {
    let facts = main_summary(call);
    ["Provider", "Model", "State", "Tokens", "Task", "Work"].map(|label| {
        let value = facts
            .iter()
            .find(|(name, _)| *name == label)
            .map_or("unavailable", |(_, value)| value.as_str());
        match label {
            "Provider" | "Model" | "State" if value != "unavailable" => value.to_owned(),
            _ => format!("{label} {value}"),
        }
    })
}

/// `main_summary` in the two lines a four-row reader dock shows whole.
pub(crate) fn main_summary_lines(call: &Value) -> [String; 2] {
    let [provider, model, state, tokens, task, work] = main_fact_pieces(call);
    [
        [provider, model, state, tokens].join(" · "),
        format!("{work} · {task}"),
    ]
}

/// The Main facts in at most `rows` lines of at most `columns` cells, for
/// the short-Board card and, repainted at each width, the reader dock.
/// Priority, highest first:
/// 1. Identity (Provider, Model, State, Tokens) packs in reading order.
/// 2. Labeled Work takes the next free row, else the first line with room
///    for `Work`, two cells and `…`; it widens into that line's slack last.
/// 3. Task joins Work's line, else the first other line with room, else a
///    free row.
///
/// The widest identity fact clips, one cell at a time, until Work at its
/// floor fits and, given two or more rows, Task too; Task, never clipped,
/// yields only when it still cannot fit. Where even Work cannot fit, the
/// rows hold identity alone. A fact left out whole ends the last row in `…`.
pub(crate) fn main_bounded_lines(call: &Value, columns: usize, rows: usize) -> Vec<String> {
    const TASK: usize = 4;
    const WORK: usize = 5;
    if rows == 0 || columns == 0 {
        return Vec::new();
    }
    let facts = main_fact_pieces(call).map(|piece| clip(&piece, columns));
    let floor = width(&facts[WORK]).min(8);
    let cells = |pieces: &[String], piece: usize| match piece {
        WORK => floor,
        _ => width(&pieces[piece]),
    };
    let used = |pieces: &[String], line: &[usize]| {
        line.iter()
            .map(|&piece| cells(pieces, piece))
            .sum::<usize>()
            + 3 * line.len().saturating_sub(1)
    };
    let fits = |pieces: &[String], line: &[usize], piece: usize| {
        line.is_empty() || used(pieces, line) + 3 + cells(pieces, piece) <= columns
    };
    let identity = |pieces: &[String]| {
        let mut lines: Vec<Vec<usize>> = Vec::new();
        for piece in 0..TASK {
            match lines.last_mut() {
                Some(line) if fits(pieces, line, piece) => line.push(piece),
                _ => lines.push(vec![piece]),
            }
        }
        lines
    };
    let place = |pieces: &[String], need_task: bool| {
        let mut lines = identity(pieces);
        let at = match lines.len() {
            n if n < rows => {
                lines.push(Vec::new());
                n
            }
            n if n == rows => lines.iter().position(|line| fits(pieces, line, WORK))?,
            _ => return None,
        };
        lines[at].push(WORK);
        match std::iter::once(at)
            .chain((0..lines.len()).filter(|&line| line != at))
            .find(|&line| fits(pieces, &lines[line], TASK))
        {
            Some(line) => lines[line].push(TASK),
            None if lines.len() < rows => lines.push(vec![TASK]),
            None if need_task => return None,
            None => {}
        }
        Some((lines, at))
    };
    let join = |pieces: &[String], lines: Vec<Vec<usize>>| {
        let omitted = lines.iter().map(Vec::len).sum::<usize>() < pieces.len();
        let mut out: Vec<String> = lines
            .iter()
            .map(|line| {
                let line = line.iter().map(|&piece| pieces[piece].as_str());
                line.collect::<Vec<_>>().join(" · ")
            })
            .collect();
        if let Some(last) = out
            .last_mut()
            .filter(|last| omitted && !last.ends_with('…'))
        {
            *last = clip(&format!("{last}…"), columns);
        }
        out
    };
    let shrink = |pieces: &mut [String; 6]| {
        let widest = (0..TASK).max_by_key(|&piece| width(&pieces[piece]));
        let Some(widest) = widest.filter(|&piece| width(&pieces[piece]) > 1) else {
            return false;
        };
        pieces[widest] = clip(&pieces[widest], width(&pieces[widest]) - 1);
        true
    };
    for need_task in [rows > 1, false] {
        let mut pieces = facts.clone();
        loop {
            if let Some((lines, at)) = place(&pieces, need_task) {
                let slack = columns - used(&pieces, &lines[at]);
                pieces[WORK] = clip(&pieces[WORK], floor + slack);
                return join(&pieces, lines);
            }
            if !shrink(&mut pieces) {
                break;
            }
        }
    }
    let mut pieces = facts;
    while identity(&pieces).len() > rows && shrink(&mut pieces) {}
    let mut lines = identity(&pieces);
    lines.truncate(rows);
    join(&pieces, lines)
}

/// A short Board that cannot hold every row reads the two-line summary.
fn render_main_detail_card(
    call: &Value,
    plan: Option<&PlanProgress>,
    columns: usize,
    inner_width: usize,
    use_cards: bool,
    colored: bool,
    room: usize,
) -> Vec<String> {
    let mut out = vec![card_top("Overall Main", Some("Esc back"), columns, colored)];
    let mut rows = main_summary(call);
    rows.extend(
        [("Participant", "participant"), ("Attempt", "attempt")]
            .into_iter()
            .map(|(label, key)| (label, text(call, key).to_owned()))
            .filter(|(_, value)| !value.is_empty()),
    );
    if let Some(plan) = plan {
        rows.push(("Goal", plan.goal.clone()));
    }
    let budget = inner_width.saturating_sub(16);
    for (label, value) in rows {
        let wrap_w = inner_width.saturating_sub(4).max(1);
        let mut wrapped = if width(&value) > budget {
            if label == "Goal" {
                wrap_words(&value, wrap_w)
            } else {
                wrap(&value, wrap_w)
            }
        } else {
            vec![]
        };
        if label != "Goal" && wrapped.len() > 4 {
            wrapped.truncate(4);
            wrapped[3] = clip(&format!("{}…", wrapped[3]), wrap_w);
        }
        let right = if wrapped.is_empty() {
            value
        } else {
            String::new()
        };
        out.push(card_row_lr(
            vec![Span::raw(label, 14, Some(SGR_MUTED))],
            vec![Span::raw(right, budget, Some(SGR_TEXT))],
            columns,
            use_cards,
            None,
            colored,
        ));
        for part in wrapped {
            out.push(card_row_lr(
                vec![Span::raw(part, inner_width, Some(SGR_TEXT))],
                vec![],
                columns,
                use_cards,
                None,
                colored,
            ));
        }
    }
    if let Some(bottom) = card_bottom(columns, use_cards, colored) {
        out.push(bottom);
    }
    if out.len() > room {
        // The heading stays; the bottom border goes before any fact does.
        out.truncate(1);
        let bottom = card_bottom(columns, use_cards, colored);
        let mut packed = main_bounded_lines(call, inner_width.max(1), usize::MAX);
        if let Some(plan) = plan {
            let goal_lines = wrap_words(&format!("Goal: {}", plan.goal), inner_width.max(1));
            packed.extend(goal_lines);
        }
        let bottom = bottom.filter(|_| packed.len() + 2 <= room);
        let rows = room.saturating_sub(1 + usize::from(bottom.is_some()));
        let main_lines = main_bounded_lines(call, inner_width.max(1), rows);
        let main_used = main_lines.len();
        for part in main_lines {
            out.push(card_row_lr(
                vec![Span::raw(part, inner_width, Some(SGR_TEXT))],
                vec![],
                columns,
                use_cards,
                None,
                colored,
            ));
        }
        if let Some(plan) = plan
            && rows > main_used
        {
            let goal_budget = rows - main_used;
            let mut goal_lines = wrap_words(&format!("Goal: {}", plan.goal), inner_width.max(1));
            if goal_lines.len() > goal_budget {
                goal_lines.truncate(goal_budget);
                if let Some(last) = goal_lines.last_mut() {
                    *last = clip(
                        &format!("{}…", last.trim_end_matches('…')),
                        inner_width.max(1),
                    );
                }
            }
            for part in goal_lines {
                out.push(card_row_lr(
                    vec![Span::raw(part, inner_width, Some(SGR_TEXT))],
                    vec![],
                    columns,
                    use_cards,
                    None,
                    colored,
                ));
            }
        }
        out.extend(bottom);
    }
    out
}

fn render_detail_card(
    selected: Option<&Value>,
    columns: usize,
    inner_width: usize,
    use_cards: bool,
    colored: bool,
    room: usize,
) -> Vec<String> {
    let Some(call) = selected else {
        let mut out = vec![card_top("Call Detail", Some("Esc back"), columns, colored)];
        out.push(card_row_lr(
            vec![Span::literal("No call selected", Some(SGR_MUTED))],
            vec![],
            columns,
            use_cards,
            None,
            colored,
        ));
        if let Some(bottom) = card_bottom(columns, use_cards, colored) {
            out.push(bottom);
        }
        return out;
    };
    let (badge, badge_sgr) = call_badge(call);
    let display = call_display(call);
    let heading = card_top(
        &format!("Call Detail · {}", display.name),
        Some("Esc back"),
        columns,
        colored,
    );
    let raw_title = text(call, "title");
    let title = if display.title.is_empty()
        && raw_title.trim() == display.name.trim()
        && !clean(&heading, columns).contains(raw_title.trim())
    {
        // A title used as the name still needs to be readable when clipped.
        raw_title
    } else {
        display.title
    };
    let mut out = vec![heading];
    let (glyph, role_sgr) = role_style(display.role);
    let subtitle = match (display.role_label, text(call, "purpose")) {
        ("", value) | (value, "") => value.to_owned(),
        (role, purpose) => format!("{role} ({purpose})"),
    };
    out.push(card_row_lr(
        vec![
            Span::literal(glyph, Some(role_sgr)),
            Span::literal(" ", None),
            Span::raw(subtitle, inner_width, Some(role_sgr)),
        ],
        vec![Span::raw(badge, 20, Some(badge_sgr))],
        columns,
        use_cards,
        None,
        colored,
    ));
    if call["lifecycle_stale"] == true {
        for notice in [
            "No recent lifecycle events".to_owned(),
            format!("Last recorded state: {}", text(call, "state")),
        ] {
            for part in wrap(&notice, inner_width.max(1)) {
                out.push(card_row_lr(
                    vec![Span::raw(part, inner_width, Some(SGR_AMBER_BOLD))],
                    vec![],
                    columns,
                    use_cards,
                    None,
                    colored,
                ));
            }
        }
    }
    let (observed, source) = headline_tokens(call);
    let token_width = left_budget(inner_width, width("Tokens"));
    let source_short = if source == "Session incl. cache" {
        "Session"
    } else {
        source
    };
    let tokens = match observed["total"].as_u64() {
        Some(total) if observed["last_observed"] == true => {
            let full = format!("{total} · {source} · last observed");
            if width(&full) <= token_width {
                full
            } else {
                format!("last {total} {source_short}")
            }
        }
        Some(total) => {
            let full = format!("{total} · {source}");
            if width(&full) <= token_width {
                full
            } else {
                format!("{total} · {source_short}")
            }
        }
        None => "unreported".to_owned(),
    };
    let unreported_row = observed["total"].as_u64().is_none() && width(&tokens) <= token_width;
    if width(&tokens) <= token_width {
        out.push(card_row_lr(
            vec![Span::literal("Tokens", Some(SGR_MUTED))],
            vec![Span::raw(tokens, token_width, Some(SGR_TEXT))],
            columns,
            use_cards,
            None,
            colored,
        ));
    }
    let response_at = out.len();
    out.extend(receipt_token_details(
        call,
        columns,
        inner_width,
        use_cards,
        colored,
    ));
    for (label, value) in [
        ("Participant", text(call, "participant")),
        ("Attempt", text(call, "attempt")),
        (
            "Alias",
            if adapter_alias(call) {
                text(call, "alias")
            } else {
                ""
            },
        ),
        ("Provider", text(call, "provider")),
        ("Runtime", text(call, "tool")),
        ("Role", text(call, "role")),
        ("Model", model_detail(call).as_str()),
        ("Task", text(call, "task_id")),
        (
            "Input",
            match text(call, "native_assignment_transport") {
                "intent" => "Intent recorded",
                "submitted" => "Submitted · consumption unconfirmed",
                "sending" | "unknown" => "Submission unconfirmed",
                _ => "",
            },
        ),
        ("Parent", text(call, "parent")),
        ("Reason", text(call, "ending")),
        ("Created", text(call, "created_at")),
        ("Updated", text(call, "updated_at")),
    ] {
        if value.is_empty() {
            continue;
        }
        out.push(card_row_lr(
            vec![Span::raw(label, 14, Some(SGR_MUTED))],
            vec![Span::raw(
                value,
                inner_width.saturating_sub(16),
                Some(SGR_TEXT),
            )],
            columns,
            use_cards,
            None,
            colored,
        ));
    }
    if !title.is_empty() {
        for part in wrap(title, inner_width.saturating_sub(4).max(1))
            .into_iter()
            .take(4)
        {
            out.push(card_row_lr(
                vec![Span::raw(part, inner_width, Some(SGR_TEXT))],
                vec![],
                columns,
                use_cards,
                None,
                colored,
            ));
        }
    }
    if let Some(bottom) = card_bottom(columns, use_cards, colored) {
        out.push(bottom);
    }
    // The response gets the viewport's spare rows plus the slot of a Tokens row
    // that would only say "unreported", which it replaces; beside a real
    // headline with no spare row it is not shown. Counters past the cap are
    // counted by `+K more`, never clipped.
    let slot = usize::from(unreported_row);
    let cap = room
        .saturating_sub(out.len())
        .saturating_add(slot)
        .min(LAST_RESPONSE_ROWS);
    let rows = if cap == 0 {
        Vec::new()
    } else {
        last_response_rows(call, inner_width, cap)
    };
    if !rows.is_empty() {
        let tail = out.split_off(response_at);
        out.truncate(response_at - slot);
        out.extend(rows.into_iter().enumerate().map(|(index, value)| {
            let left = if index == 0 {
                vec![Span::literal(LAST_RESPONSE, Some(SGR_MUTED))]
            } else {
                Vec::new()
            };
            card_row_lr(
                left,
                vec![Span::raw(value, inner_width, Some(SGR_TEXT))],
                columns,
                use_cards,
                None,
                colored,
            )
        }));
        out.extend(tail);
    }
    out
}

const LAST_RESPONSE: &str = "Last response";
const LAST_RESPONSE_ROWS: usize = 4;

/// Values of the Call Detail's "Last response" rows; the first sits beside
/// the label and may be empty. Only known counters appear, never summed, as
/// whole `N label` parts in priority order. Parts past `cap` rows are counted
/// by a final `+K more`. Empty when neither a part nor the count fits.
fn last_response_rows(call: &Value, inner_width: usize, cap: usize) -> Vec<String> {
    let Some(counts) = native_response(call) else {
        return Vec::new();
    };
    let codex = call["provider"] == "codex";
    let known: Vec<(&str, &str, u64)> = RESPONSE_COUNTERS
        .iter()
        .zip(counts)
        .filter_map(|(&(key, label), count)| count.map(|n| (key, label, n)))
        .collect();
    // Two producer keys sharing a label keep apart by their own names.
    // Codex maps omitted cache/reasoning detail to zeros, so 0 renders as `?`.
    let parts = known.iter().map(|&(key, label, n)| {
        let value = if codex
            && n == 0
            && matches!(
                key,
                "cached_input_tokens" | "cache_write_input_tokens" | "reasoning_output_tokens"
            ) {
            "?".to_owned()
        } else {
            n.to_string()
        };
        if known.iter().filter(|other| other.1 == label).count() > 1 {
            format!(
                "{value} {}",
                key.trim_end_matches("_tokens").replace('_', " ")
            )
        } else {
            format!("{value} {label}")
        }
    });
    // Continuation rows keep a two-cell indent so they never read as a label.
    let budget = |row: usize| {
        if row == 0 {
            left_budget(inner_width, width(LAST_RESPONSE))
        } else {
            inner_width.saturating_sub(2)
        }
    };
    let fits = |row: &[String], extra: &str, budget: usize| {
        width(&row.join(" · ")) + 3 * usize::from(!row.is_empty()) + width(extra) <= budget
    };
    let mut rows: Vec<Vec<String>> = vec![Vec::new()];
    let mut more = 0;
    for part in parts {
        let last = rows.len() - 1;
        if more > 0 {
            more += 1;
        } else if fits(&rows[last], &part, budget(last)) {
            rows[last].push(part);
        } else if rows.len() < cap && width(&part) <= budget(1) {
            rows.push(vec![part]);
        } else {
            more = 1;
        }
    }
    while more > 0 {
        let marker = format!("+{more} more");
        let last = rows.len() - 1;
        if fits(&rows[last], &marker, budget(last)) {
            rows[last].push(marker);
            break;
        }
        if rows.len() < cap && width(&marker) <= budget(1) {
            rows.push(vec![marker]);
            break;
        }
        if rows[last].pop().is_none() {
            return Vec::new();
        }
        more += 1;
    }
    if width(LAST_RESPONSE) > inner_width || rows.iter().all(Vec::is_empty) {
        return Vec::new();
    }
    rows.iter().map(|row| row.join(" · ")).collect()
}

fn receipt_token_details(
    call: &Value,
    columns: usize,
    inner_width: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let receipt = &call["receipt_tokens"];
    let (Some(calls), Some(reported), Some(providers)) = (
        receipt["calls"].as_u64(),
        receipt["reported"].as_u64(),
        receipt["providers"].as_object(),
    ) else {
        return Vec::new();
    };
    let line = |label: &str, value: String| {
        let budget = left_budget(inner_width, width(label));
        (width(&value) <= budget).then(|| {
            card_row_lr(
                vec![Span::raw(label, inner_width, Some(SGR_MUTED))],
                vec![Span::raw(value, budget, Some(SGR_TEXT))],
                columns,
                use_cards,
                None,
                colored,
            )
        })
    };
    let retained = if receipt["last_observed"] == true {
        " · last observed"
    } else {
        ""
    };
    let overflow = if reported == calls && receipt["total"].is_null() {
        " · sum unavailable"
    } else {
        ""
    };
    let grouped = providers
        .values()
        .any(|row| !rows(&row["launch_accounts"]).is_empty());
    let heading = if grouped {
        "Launch receipts"
    } else {
        "Receipts"
    };
    let Some(coverage) = line(
        heading,
        format!("{reported}/{calls} call totals{overflow}{retained}"),
    )
    .or_else(|| {
        grouped
            .then(|| {
                let last = if retained.is_empty() { "" } else { " last" };
                line(heading, format!("{reported}/{calls} totals{last}"))
            })
            .flatten()
    }) else {
        return Vec::new();
    };
    let count = |components: &Value, field: &str, partial: Option<&str>| {
        if let Some(n) = components[field].as_u64() {
            return n.to_string();
        }
        match (
            components["reported"][field].as_u64(),
            components["calls"].as_u64(),
            partial,
        ) {
            (Some(n), Some(m), Some(prefix)) if n > 0 && n < m => format!("{prefix}{n}/{m}"),
            _ => "?".into(),
        }
    };
    let mut out = vec![coverage];
    for (provider, components) in providers {
        let provider = match provider.as_str() {
            "codex" => "Codex",
            "claude" => "Claude",
            "antigravity" | "agy" => "AGY",
            other => other,
        };
        let accounts = rows(&components["launch_accounts"]);
        if accounts.is_empty() {
            let row = |partial, compact| {
                let [input, output, read, write] = ["input", "output", "cache_read", "cache_write"]
                    .map(|field| count(components, field, partial));
                if compact {
                    format!("In {input} Out {output} Read {read} Write {write}")
                } else {
                    format!("In {input} · Out {output} · Cache read {read} · Write {write}")
                }
            };
            out.extend(
                line(provider, row(Some("partial "), false))
                    .or_else(|| line(provider, row(Some("p"), false)))
                    .or_else(|| line(provider, row(Some("p"), true)))
                    .or_else(|| line(provider, row(None, true))),
            );
            continue;
        }
        for account in accounts {
            let alias = account["alias"]
                .as_str()
                .filter(|id| crate::cli::valid_account_id(id));
            let label = format!("{provider} @{}", alias.unwrap_or("?"));
            let fields = [
                "input",
                "output",
                "cache_read",
                "cache_write",
                "reasoning",
                "total",
            ];
            let [input, output, read, write, reasoning, total] =
                fields.map(|field| count(account, field, Some("p")));
            let full = format!(
                "In {input} · Out {output} · Cache {read}/{write} · Rsn {reasoning} · Total {total}"
            );
            if let Some(row) = line(&label, full.clone()) {
                out.push(row);
                continue;
            }
            let compact =
                format!("In {input} Out {output} R{read} W{write} Rsn{reasoning} T{total}");
            if let Some(row) = line(&label, compact.clone()) {
                out.push(row);
                continue;
            }
            // Keep the exact alias and counter names when both cannot fit one line.
            for row in wrap(&format!("{label} · {full}"), inner_width.max(1)) {
                out.extend(line(&row, String::new()));
            }
        }
    }
    out
}

fn render_summary_rows(
    calls: &[TreeEntry<'_>],
    capacity: usize,
    columns: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    count_row_render();
    let mut states = BTreeMap::new();
    for entry in calls {
        *states.entry(text(entry.call, "state")).or_insert(0usize) += 1;
    }
    let total = calls.len().max(1);
    let mut out = Vec::new();
    for (state, count) in states.into_iter().take(capacity) {
        let (badge, badge_sgr) = state_badge(state);
        let bar_max = 12usize;
        let bar_len = (count * bar_max).div_ceil(total).clamp(1, bar_max);
        let bar = "█".repeat(bar_len);
        out.push(card_row_lr(
            vec![
                Span::raw(badge, 14, Some(badge_sgr)),
                Span::literal("  ", None),
                Span::raw(bar, 16, Some(badge_sgr)),
            ],
            vec![Span::raw(format!("{count} call(s)"), 16, Some(SGR_TEXT))],
            columns,
            use_cards,
            None,
            colored,
        ));
    }
    out
}

pub fn tree(
    calls: &[Value],
    nav: &navigation::Navigation,
    graph: bool,
    maximum: usize,
) -> Vec<String> {
    let calls: Vec<_> = calls.iter().collect();
    collect_tree(&calls, calls.iter().copied(), nav, graph, maximum)
        .into_iter()
        .map(|entry| {
            let display = call_display(entry.call);
            let role_space = if display.role_label.is_empty() {
                ""
            } else {
                " "
            };
            let title_space = if display.title.is_empty() { "" } else { " " };
            format!(
                "{}{}{}{} [{}]{}{}",
                entry.prefix,
                display.name,
                role_space,
                display.role_label,
                text(entry.call, "state"),
                title_space,
                display.title,
            )
        })
        .collect()
}

struct TreeEntry<'a> {
    prefix: String,
    call: &'a Value,
}

fn collect_tree<'a>(
    calls: &[&'a Value],
    all: impl IntoIterator<Item = &'a Value>,
    nav: &navigation::Navigation,
    graph: bool,
    maximum: usize,
) -> Vec<TreeEntry<'a>> {
    let mut ids = BTreeMap::new();
    for call in all {
        for id in [text(call, "participant"), text(call, "attempt")]
            .into_iter()
            .filter(|id| !id.is_empty())
        {
            ids.entry(id)
                .and_modify(|owner: &mut Option<&Value>| {
                    if owner.is_some_and(|owner| !std::ptr::eq(owner, call)) {
                        *owner = None;
                    }
                })
                .or_insert(Some(call));
        }
    }
    let unique = |id: &str| ids.get(id).copied().flatten();
    let visible_ids: BTreeSet<_> = calls.iter().map(|call| text(call, "participant")).collect();
    // Compare exact identities against all rows, including those hidden by a filter.
    let parent_of = |call: &Value| {
        let parent = unique(text(call, "parent"))?;
        let parent_id = text(parent, "participant");
        (!std::ptr::eq(parent, call)
            && unique(text(call, "participant")).is_some_and(|node| std::ptr::eq(node, call))
            && unique(parent_id).is_some_and(|node| std::ptr::eq(node, parent))
            && visible_ids.contains(parent_id)
            && (call["native_child"] != true
                || (!text(call, "provider").is_empty() && call["provider"] == parent["provider"])))
            .then_some(parent)
    };
    let mut children: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for &call in calls {
        if let Some(parent) = parent_of(call) {
            children
                .entry(text(parent, "participant"))
                .or_default()
                .push(call);
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    #[allow(clippy::too_many_arguments)]
    fn visit<'a>(
        call: &'a Value,
        ancestors_last: &mut Vec<bool>,
        is_last: bool,
        children: &BTreeMap<&str, Vec<&'a Value>>,
        seen: &mut BTreeSet<&'a str>,
        out: &mut Vec<TreeEntry<'a>>,
        nav: &navigation::Navigation,
        graph: bool,
        maximum: usize,
    ) {
        let id = text(call, "participant");
        if !seen.insert(id) || out.len() >= maximum {
            return;
        }
        let mut prefix = String::new();
        if !ancestors_last.is_empty() {
            for &last in ancestors_last.iter().skip(1) {
                prefix.push_str(if last { "   " } else { "│  " });
            }
            prefix.push_str(match (graph, is_last) {
                (true, true) => "└→ ",
                (true, false) => "├→ ",
                (false, true) => "└─ ",
                (false, false) => "├─ ",
            });
        }
        out.push(TreeEntry { prefix, call });
        if nav.folded.contains(id) {
            return;
        }
        if let Some(rows) = children.get(id) {
            ancestors_last.push(is_last);
            let total = rows.len();
            for (idx, child) in rows.iter().enumerate() {
                visit(
                    child,
                    ancestors_last,
                    idx + 1 == total,
                    children,
                    seen,
                    out,
                    nav,
                    graph,
                    maximum,
                );
            }
            ancestors_last.pop();
        }
    }
    let roots: Vec<&Value> = calls
        .iter()
        .copied()
        .filter(|v| parent_of(v).is_none())
        .collect();
    let total_roots = roots.len();
    for (idx, call) in roots.into_iter().enumerate() {
        let mut stack = Vec::new();
        visit(
            call,
            &mut stack,
            idx + 1 == total_roots,
            &children,
            &mut seen,
            &mut out,
            nav,
            graph,
            maximum,
        );
    }
    for &call in calls {
        let mut ancestors = BTreeSet::new();
        let mut hidden = false;
        let mut node = call;
        while let Some(parent) = parent_of(node) {
            let id = text(parent, "participant");
            if !ancestors.insert(id) {
                break;
            }
            if nav.folded.contains(id) {
                hidden = true;
                break;
            }
            node = parent;
        }
        if hidden {
            continue;
        }
        let mut stack = Vec::new();
        visit(
            call, &mut stack, true, &children, &mut seen, &mut out, nav, graph, maximum,
        );
    }
    out
}

pub fn value_lines(value: &Value, columns: usize) -> Vec<String> {
    serde_json::to_string_pretty(value)
        .map(|v| wrap(&v, columns))
        .unwrap_or_default()
}

/// Proposed route evidence from the existing selection snapshot, without live
/// capability or billing inference. Unknown fields remain visibly unknown.
pub fn routes(report: &Value, columns: usize) -> String {
    let value = |v: &Value| {
        v.as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
            .or_else(|| v.as_bool().map(|v| v.to_string()))
            .unwrap_or_else(|| "unknown".into())
    };
    let values = |v: &Value| {
        let items: Vec<_> = rows(v).iter().filter_map(Value::as_str).collect();
        if items.is_empty() {
            "unknown".into()
        } else {
            items.join(", ")
        }
    };
    let mut lines = vec![
        "OMS · Proposed routes (read only)".to_owned(),
        "Selection preview; launch and approval require their existing checks.".into(),
        "Reported source ages are not freshness or account entitlement proofs.".into(),
        format!(
            "Owner: {} · policy schema {}",
            value(&report["owner"]),
            value(&report["schema"])
        ),
    ];
    if let Some(chosen) = report["chosen"].as_object() {
        for (role, choice) in chosen {
            let preview = &choice["evidence"];
            lines.push(format!(
                "# {role} · tier {}",
                value(
                    preview
                        .get("tier")
                        .unwrap_or(&report["roles"][role]["tier"])
                )
            ));
            let allocation = &preview["allocation"];
            let comparison = match allocation["native_order"].as_str() {
                Some("weekly_used") => Some("lower weekly use"),
                Some("activity_within_weekly_tie_band") => {
                    Some("in-flight, recent, owner tie-break (weekly use within tie band)")
                }
                Some("activity_missing_weekly_evidence") => {
                    Some("in-flight, recent, owner tie-break (weekly evidence incomplete)")
                }
                Some("policy_order") => Some("policy order"),
                _ => None,
            };
            if let Some(comparison) = comparison {
                lines.push(format!("Provider ranking: {comparison}"));
            }
            if let Some(adjustments) = allocation["order_adjustments"].as_array()
                && !adjustments.is_empty()
            {
                lines.push(format!(
                    "Priority adjustments: {}",
                    values(&allocation["order_adjustments"]).replace('_', " ")
                ));
            }
            for candidate in rows(&allocation["candidates"]) {
                let provider = value(&candidate["provider"]);
                let disposition = candidate["rank"].as_u64().map_or_else(
                    || value(&candidate["excluded_by"]).replace('_', " "),
                    |rank| format!("rank {rank}"),
                );
                lines.push(format!(
                    "  {provider} · {disposition} · provider quota {} · account quota {} · in-flight {} · recent {}",
                    value(&candidate["provider_quota"]),
                    value(&candidate["account_quota"]),
                    value(&candidate["in_flight"]),
                    value(&candidate["non_main_attempts"])
                ));
                let accounts = &candidate["registered_quota"];
                if accounts.is_object() {
                    lines.push(format!(
                        "    Registered quota: {} fresh · {} stale · {} unavailable · {} unverified",
                        value(&accounts["fresh"]),
                        value(&accounts["stale"]),
                        value(&accounts["unavailable"]),
                        value(&accounts["unverified"])
                    ));
                }
            }
            if let Some(error) = choice["error"].as_str() {
                lines.push(format!("Unavailable: {error}"));
                continue;
            }
            let snapshot = &choice["snapshot"];
            lines.push(format!(
                "Harness/provider proposal: {} · family {} ({}) · execution profile {}",
                value(&snapshot["resolved"]["provider"]),
                value(&snapshot["model_family"]),
                value(&snapshot["family_evidence"]),
                value(&snapshot["execution_profile"])
            ));
            lines.push(format!(
                "Model: configured {} · requested {} · resolved {} · served {}",
                value(&snapshot["configured"]["model"]),
                value(&snapshot["requested_model"]),
                value(&snapshot["resolved"]["model"]),
                value(&snapshot["served_model"])
            ));
            lines.push(format!(
                "Effort: configured {} · requested {} · selected {} · applied/served {}",
                value(&snapshot["configured"]["effort"]),
                value(&snapshot["requested_effort"]),
                value(&snapshot["resolved"]["effort"]),
                value(&snapshot["served_effort"])
            ));
            lines.push(format!(
                "Account: requested {} · proposed {} · pinned {} · served {}",
                value(&snapshot["requested_account"]),
                value(&snapshot["selected_account"]),
                value(&snapshot["account_pinned"]),
                value(&snapshot["served_account"])
            ));
            if preview.is_object() {
                let events = match preview["events_source"].as_str() {
                    Some("repository_events") => "repository Events",
                    Some("unavailable") => "Events unavailable",
                    Some("no_repository") => "no repository",
                    _ => "unknown",
                };
                lines.push(format!(
                    "Provider spread: {} non-Main attempts {} · {events}",
                    value(&preview["recent"]["scope"]),
                    value(&preview["recent"]["non_main_attempts"])
                ));
                let launch = match preview["launch_account"].as_str() {
                    Some("headless_may_reselect") => "headless adapter may reselect",
                    _ => "unreported",
                };
                lines.push(format!(
                    "Account selection: {} · launch {launch}",
                    value(&preview["account_reason"])
                ));
                let rank = &preview["account_rank"];
                if rank.is_object() {
                    let workload_unknown = rank["policy"] == "spread"
                        && preview["events_source"] != "repository_events";
                    let decision = match rank["decided_by"].as_str() {
                        Some("only_eligible") => "only eligible account",
                        Some("weekly_bucket") => "lower weekly-use bucket",
                        Some("working_attempts") if workload_unknown => {
                            "incomplete working/running inputs"
                        }
                        Some("working_attempts") => "fewer working/running attempts",
                        Some("registry_order") if workload_unknown => {
                            "registry order (workload unknown)"
                        }
                        Some("registry_order") => "registry order",
                        _ => "unknown",
                    };
                    lines.push(format!(
                        "Account ranking: {} · eligible {} · decided by {decision}",
                        value(&rank["policy"]),
                        value(&rank["eligible_accounts"])
                    ));
                    lines.push(format!(
                        "Weekly-use bucket: proposed {} · next {}",
                        value(&rank["weekly_used_bucket"]),
                        value(&rank["next_weekly_used_bucket"])
                    ));
                    if rank["policy"] == "spread" {
                        lines.push(format!(
                            "Repo working/running attempts: proposed {} · next {}",
                            value(&rank["repo_working_attempts"]),
                            value(&rank["next_repo_working_attempts"])
                        ));
                    }
                }
            }
            let catalog = &snapshot["catalog"];
            lines.push(format!(
                "Catalog: source {} · probe {} · reported age {} seconds · read at {}",
                value(&catalog["source"]),
                value(&catalog["reported_probe"]),
                value(&catalog["catalog_age_seconds"]),
                value(&catalog["read_at"])
            ));
            lines.push(format!(
                "Catalog models: {} · resolved present {} · account {} · endpoint {}",
                value(&catalog["count"]),
                value(&catalog["resolved_present"]),
                value(&catalog["account_alias"]),
                value(&catalog["endpoint"])
            ));
            let capabilities = &snapshot["capabilities"];
            lines.push(format!(
                "Capability source: {} · harness version {} · reported probe age {} seconds",
                value(&capabilities["source"]),
                value(&capabilities["harness_version"]),
                value(&capabilities["probe_age_seconds"])
            ));
            lines.push(format!(
                "Exact effort support: {} · exact model values: {}",
                value(&capabilities["effort_support"]),
                values(&capabilities["exact_model_efforts"])
            ));
            lines.push(format!(
                "Effort mechanism: {} · provider-reported values: {}",
                value(&capabilities["effort_mechanism"]),
                values(&capabilities["provider_reported_efforts"])
            ));
            lines.push(format!(
                "Capability account: {} · endpoint {}",
                value(&capabilities["account_alias"]),
                value(&capabilities["endpoint"])
            ));
            let evidence = &capabilities["evidence"];
            lines.push(format!(
                "Capability lifetime: source {} · observed {} · expires {} (Unix seconds)",
                value(&evidence["source"]),
                value(&evidence["observed_at"]),
                value(&evidence["expires_at"])
            ));
            lines.push(format!(
                "Reported scope freshness at read: {} · selected account matches {}",
                value(&capabilities["freshness"]),
                value(&capabilities["selected_account_matches"])
            ));
            lines.push(
                "Selected endpoint binding: unknown; source freshness grants no route eligibility."
                    .into(),
            );
            lines.push("Entitlement: unknown · billing: unknown · quota: unknown".into());
            lines.push(format!(
                "Policy digest: {} · snapshot schema {}",
                value(&snapshot["policy_sha256"]),
                value(&snapshot["schema"])
            ));
        }
    } else {
        lines.push("Proposed routes: unknown".into());
    }
    let safe = oms_panel_core::redact(lines.join("\n").as_bytes())
        .unwrap_or_else(|_| "Route text withheld: redaction unavailable".into());
    wrap(&safe, columns).join("\n") + "\n"
}

/// Optional Team inspector. The report contains observations, never actions.
pub fn teams(report: &Value, columns: usize) -> String {
    let value = |v: &Value| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
            .or_else(|| v.as_u64().map(|n| n.to_string()))
            .or_else(|| v.as_bool().map(|value| value.to_string()))
            .unwrap_or_else(|| "unknown".into())
    };
    let mut lines = vec!["OMS · Teams (read only)".to_owned()];
    for name in ["teams", "resources", "physical_intents"] {
        let source = &report["sources"][name];
        lines.push(format!(
            "{name} source: {}{}",
            value(&source["status"]),
            source["reason"]
                .as_str()
                .map(|v| format!(" · {v}"))
                .unwrap_or_default()
        ));
    }
    for root in rows(&report["roots"]) {
        lines.push(format!(
            "# Root {} / generation {} (contract owner)",
            value(&root["id"]),
            value(&root["generation"])
        ));
        for team in rows(&root["teams"]) {
            lines.push(format!(
                "## Team {} / generation {} · task {} · {}",
                value(&team["id"]),
                value(&team["generation"]),
                value(&team["task_id"]),
                value(&team["mode"])
            ));
            lines.push(format!(
                "Contract: {} · {}",
                value(&team["contract_phase"]),
                team["reason"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("execution/next actor unreported")
            ));
            for member in rows(&team["members"]) {
                let branch = if member["role"] == "lead" {
                    "  Lead"
                } else {
                    "    Member"
                };
                lines.push(format!(
                    "{branch} {} / generation {} · {} · {} · parent {}",
                    value(&member["id"]),
                    value(&member["generation"]),
                    value(&member["display_name"]),
                    value(&member["role"]),
                    value(&member["logical_parent"])
                ));
                let route = &member["route"];
                lines.push(format!("      contract {} / {} · requested {} · resolved {} · account {} · served unknown",value(&route["harness"]),value(&route["provider"]),value(&route["requested_model"]),value(&route["resolved_model"]),value(&route["requested_account"])));
                lines.push("      enrollment/execution/cleanup/usage: unreported".into());
                for call in rows(&member["physical_intents"]) {
                    lines.push(format!(
                        "      intent {} ({}) · selected {} · account {} · running unknown",
                        value(&call["call_id"]),
                        value(&call["intent_phase"]),
                        value(&call["selected_model"]),
                        value(&call["account"])
                    ));
                }
                if member["omitted_intents"].as_u64().is_some_and(|n| n > 0) {
                    lines.push(format!(
                        "      omitted intents: {}",
                        value(&member["omitted_intents"])
                    ));
                }
            }
            if team["omitted_members"].as_u64().is_some_and(|n| n > 0) {
                lines.push(format!(
                    "    omitted members: {}",
                    value(&team["omitted_members"])
                ));
            }
            lines.push(format!(
                "Evidence: verified {} / admitted {} / landed {} / published {}",
                value(&team["verified"]),
                value(&team["admitted"]),
                value(&team["landed"]),
                value(&team["published"])
            ));
        }
    }
    for team in rows(&report["unbound_teams"]) {
        lines.push(format!(
            "## Team {} · unverified · {}",
            value(&team["id"]),
            value(&team["reason"])
        ));
    }
    if report["roots"].as_array().is_some_and(Vec::is_empty)
        && report["unbound_teams"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        lines.push("No retained Team contracts; execution remains unknown".into());
    }
    if report["omitted_teams"].as_u64().is_some_and(|n| n > 0) {
        lines.push(format!(
            "Omitted Teams: {} (display bound)",
            value(&report["omitted_teams"])
        ));
    }
    let resource = &report["resources"];
    lines.push(format!(
        "Resource scope: {} · domain {} · active {}",
        value(&resource["scope"]),
        value(&resource["domain"]),
        value(&resource["active"])
    ));
    lines.push(format!(
        "Claims: invocations {} / open processes {} / write WIP {}",
        value(&resource["occupancy"]["invocations"]),
        value(&resource["occupancy"]["open_process_claims"]),
        value(&resource["occupancy"]["write_wip"])
    ));
    lines.push(format!(
        "Limits: invocations {} / process claims {} / write WIP {} / reviewer headroom {}",
        value(&resource["limits"]["invocations"]),
        value(&resource["limits"]["open_process_claims"]),
        value(&resource["limits"]["write_wip"]),
        value(&resource["limits"]["reviewer_headroom"])
    ));
    lines.push(format!("Retained physical intents: {} · unmatched in display {} · omitted {} (not a running count)",value(&report["physical_intents"]["retained_count"]),value(&report["physical_intents"]["unmatched_in_display"]),value(&report["physical_intents"]["omitted_intents"])));
    lines.push("Native children/usage: unknown; native global hard cap: unsupported".into());
    let safe = oms_panel_core::redact(lines.join("\n").as_bytes())
        .unwrap_or_else(|_| "Team text withheld: redaction unavailable".into());
    wrap(&safe, columns).join("\n") + "\n"
}

pub fn results(report: &Value, columns: usize) -> String {
    let mut lines = vec!["OMS · Results".into(), "─".repeat(columns.clamp(12, 60))];
    for row in rows(&report["rows"]) {
        lines.push(format!(
            "■ {} · {} · {}",
            text(row, "task_id"),
            text(row, "outcome"),
            text(row, "title")
        ));
        if let Some(issue) = row["issue"].as_str().filter(|v| !v.is_empty()) {
            lines.extend(wrap(issue, columns));
        }
        for key in ["summary", "answer", "answer_excerpt"] {
            if let Some(value) = row[key].as_str().filter(|v| !v.is_empty()) {
                lines.extend(wrap(value, columns));
            }
        }
        if row["answer_truncated"] == true {
            lines.push("... (answer truncated)".into());
        }
        for call in rows(&row["calls"]) {
            lines.push(format!(
                "  ├─ {} · {} · {}",
                text(call, "kind"),
                text(call, "selected_model"),
                text(call, "reason_code")
            ));
            for key in ["answer", "answer_excerpt", "summary"] {
                if let Some(value) = call[key].as_str().filter(|v| !v.is_empty()) {
                    lines.extend(wrap(value, columns));
                    break;
                }
            }
            if call["answer_truncated"] == true {
                lines.push("  ... (answer truncated)".into());
            }
        }
        if let Some(summary) = row["result"]["summary"].as_str().filter(|v| !v.is_empty())
            && row["summary"].as_str() != Some(summary)
        {
            lines.extend(wrap(summary, columns));
        }
        let entry = &row["native_assignment"];
        if entry.is_object() {
            lines.extend(wrap(
                &format!(
                    "  ◇ Native assignment {} · {} · consumption {} · decision {}",
                    text(entry, "attempt_id"),
                    text(entry, "transport_label"),
                    text(entry, "consumption"),
                    text(entry, "acceptance")
                ),
                columns,
            ));
            if entry["return_state"].is_string() {
                lines.extend(wrap(
                    &format!(
                        "    Return {} · resolution {} · verification {}",
                        text(entry, "return_state"),
                        text(entry, "resolution"),
                        text(entry, "verification")
                    ),
                    columns,
                ));
                if let Some(next) = entry["next_action"]["reason"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    lines.extend(wrap(&format!("    Next: {next}"), columns));
                }
            }
            if let Some(issue) = entry["verification_issue"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                lines.extend(wrap(&format!("    Verification: {issue}"), columns));
            }
            let report = &entry["report"];
            if report.is_object() {
                // Managed rows reuse `acceptance` for the panel outcome; only a
                // typed canonical landing names its landing id.
                let landing = text(&entry["landing"], "landing_id");
                let decision = if entry["acceptance"] == "accepted"
                    && entry["verification"] == "verified"
                    && entry["landing"]["source"] == "canonical-landing"
                    && !landing.is_empty()
                {
                    format!("accepted through canonical landing {landing}")
                } else {
                    "acceptance ineligible".into()
                };
                lines.extend(wrap(
                    &format!(
                        "    Native report {} · {} · verification {} · {decision}",
                        text(report, "status"),
                        text(report, "result_kind"),
                        text(report, "verification_state")
                    ),
                    columns,
                ));
                for key in ["summary", "issue"] {
                    if let Some(value) = report[key].as_str().filter(|v| !v.is_empty()) {
                        lines.extend(wrap(value, columns));
                    }
                }
                for missing in rows(&report["missing_evidence"]) {
                    if let Some(missing) = missing.as_str() {
                        lines.extend(wrap(&format!("    Missing: {missing}"), columns));
                    }
                }
            }
        }
        lines.push(String::new());
    }
    if rows(&report["rows"]).is_empty() {
        lines.push("No retained task results".into());
    }
    if report["has_more"] == true {
        lines.push("More results: use --results --page N".into());
    }
    lines
        .into_iter()
        .map(|v| clip(&v, columns))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

pub fn debate(report: &Value, columns: usize) -> String {
    let mut lines = vec![
        "Council · recorded positions".into(),
        "─".repeat(columns.clamp(12, 60)),
    ];
    for task in rows(&report["rows"]) {
        for call in rows(&task["calls"]) {
            lines.push(format!(
                "◇ {} · {}",
                text(call, "kind"),
                text(call, "selected_model")
            ));
            lines.extend(wrap(text(call, "answer_excerpt"), columns));
        }
    }
    if let Some(synthesis) = report["synthesis"]["text"].as_str() {
        lines.push("■ Synthesis".into());
        lines.extend(wrap(synthesis, columns));
    }
    lines.push("Agreement records identical positions; it does not prove correctness.".into());
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_sgr(text: &str) -> String {
        clean(text, 10_000)
    }

    #[test]
    fn capacity_shows_exact_write_slots_in_narrow_and_wide_frames() {
        let report = json!({
            "collection": {"ok": true},
            "project_name": "Work",
            "room": {"id":"room", "main":"M", "participants":[
                {"participant":"M","role":"main","state":"working","provider":"codex","joined":true}
            ]},
            "board_write_slots": {"used":10,"limit":10,"active":0,"awaiting":8,"unconfirmed":2}
        });
        let nav = navigation::Navigation {
            health: true,
            ..Default::default()
        };
        let read = BTreeMap::new();
        for ((cols, height), label) in [
            ((30, 10), "Write 10/10 · 8 dec · 2 unconf"),
            ((44, 16), "Write 10/10 · 8 dec · 2 unconf"),
            (
                (100, 30),
                "Write slots 10/10 · 8 awaiting decision · 2 unconfirmed",
            ),
        ] {
            let lines = frame(&report, &nav, cols, height, &read);
            assert!(lines.iter().any(|line| line.contains(label)), "{lines:?}");
            assert!(lines.iter().all(|line| width(line) <= cols));
        }
        let mut pending = report.clone();
        pending["collection"]["pending"] = true.into();
        assert!(
            frame(&pending, &nav, 44, 16, &read)
                .iter()
                .any(|line| line.contains("Write 10/10 · 8 dec · 2 unconf"))
        );
        let mut active = report.clone();
        active["board_write_slots"]["active"] = 1.into();
        active["board_write_slots"]["awaiting"] = 7.into();
        assert_eq!(
            write_slots_line(&active, 96).as_deref(),
            Some("Write slots 10/10 · 1 active · 7 awaiting decision · 2 unconfirmed")
        );
        let compressed = write_slots_line(&active, 30).unwrap();
        assert!(width(&compressed) <= 30, "{compressed}");
        assert!(compressed.contains("1act") && compressed.contains("2unconf"));
        for (key, value) in [
            ("board_write_slots", Value::Null),
            ("collection", json!({"ok":false,"error":"failed"})),
            ("capacity_stale", json!(true)),
        ] {
            let mut unavailable = report.clone();
            unavailable[key] = value;
            let lines = frame(&unavailable, &nav, 30, 10, &read);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("Write slots unavailable")),
                "{lines:?}"
            );
            assert!(lines.iter().all(|line| !line.contains("10/10")));
        }
        let ordinary = frame(&report, &navigation::Navigation::default(), 100, 30, &read);
        assert!(ordinary.iter().all(|line| !line.contains("Write slots")));
    }

    #[test]
    fn ordinary_lead_delivery_board_keeps_activity_separate() {
        let mut report = json!({
            "collection":{"ok":true},
            "provider_sessions":["claude", "antigravity"],
            "provider_activity":{"claude":"idle", "antigravity":"working"},
            "provider_delivery":{"claude":"input_cleared_unverified","antigravity":"turn_observed"},
        });
        let read = BTreeMap::new();
        let nav = navigation::Navigation::default();
        let visible = |r: &Value| frame(r, &nav, 110, 24, &read).join("\n");
        let view = visible(&report);
        assert!(view.contains("idle · input cleared · unverified"), "{view}");
        assert!(view.contains("working · turn observed"), "{view}");

        report["provider_delivery"]["claude"] = json!("draft_retained");
        assert!(visible(&report).contains("idle · draft last seen"));

        report["collection"]["pending"] = json!(true);
        let held = visible(&report);
        assert!(!held.contains("draft last seen") && !held.contains("turn observed"));
        report["collection"]["pending"] = json!(false);
        report["provider_sessions"] = json!(["antigravity"]);
        assert!(!visible(&report).contains("draft last seen"));
    }

    #[test]
    fn provider_workers_use_clean_labels_and_observed_activity() {
        for (brief, role, expected) in [
            (
                "Role builder R63, 메인 토큰 확인",
                "Builder",
                "R63, 메인 토큰 확인",
            ),
            (
                "Role worker. Inspect checkout",
                "Builder",
                "Inspect checkout",
            ),
            (
                "Role RESEARCHER: R44 읽기 비용",
                "Researcher",
                "R44 읽기 비용",
            ),
            (
                "Role builder-worktree cleanup",
                "Builder",
                "Role builder-worktree cleanup",
            ),
            ("Role verifier R63", "Builder", "Role verifier R63"),
        ] {
            assert_eq!(board_brief(brief, role), expected);
        }
        let row = json!({
            "title":"Role builder R63, 메인 토큰 확인","provider":"codex",
            "role":"worker","display_role":"builder","participant":"call-fixture",
            "state":"working","event_tokens":{"total":12500}
        });
        for columns in [56, 100, 140] {
            let line = render_call_row(&row, "", "", false, columns, true, false);
            assert!(line.contains("Builder") && line.contains("R63"), "{line}");
            assert!(!line.contains("Role builder"), "{line}");
            assert_eq!(width(&line), columns);
        }

        let report = json!({"collection":{"ok":true},
            "provider_sessions":["codex", "claude", "antigravity"],
            "provider_activity":{"codex":"working", "claude":"idle", "antigravity":"unknown"},
            "attempts":{"active":[{"attempt_id":"codex-attempt", "tool":"codex-runtime", "provider":"codex", "state":"working"}]},
            "room":{"participants":[
                {"participant":"codex-id", "attempt":"codex-attempt", "alias":"Ada", "provider":"codex", "role":"appserver", "state":"working", "title":"Inspect code"},
                {"participant":"claude-id", "provider":"claude", "role":"worker", "state":"working", "title":"Review code"}
            ]},
            "native_children":[
                {"participant":"agy-id", "alias":"↳", "native_child":true, "provider":"antigravity", "role":"subagent", "state":"working", "title":"Research"},
                {"participant":"native-antigravity-1111111111111111", "alias":"↳", "native_child":true, "provider":"antigravity", "role":"subagent", "state":"working", "title":"Native subagent"},
                {"participant":"native-antigravity-2222222222222222", "alias":"subagent", "native_child":true, "provider":"antigravity", "role":"subagent", "state":"working", "title":"worker"}
            ]
        });
        for (call, name, role, title) in [
            (
                json!({"participant":"call-1234567890abcdef", "role":"worker", "title":"Build feature"}),
                "Build feature",
                "worker",
                "",
            ),
            (
                json!({"participant":"att_1234567890abcdef", "role":"appserver", "title":"Inspect code"}),
                "Inspect code",
                "worker",
                "",
            ),
            (
                json!({"participant":"native-codex-1234567890abcdef", "native_child":true, "role":"subagent", "title":"Review code"}),
                "Review code",
                "worker",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"Ada", "role":"builder", "title":"Build feature"}),
                "Ada",
                "builder",
                "Build feature",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"call-builder", "role":"worker", "title":"Build feature"}),
                "call-builder",
                "worker",
                "Build feature",
            ),
            (
                json!({"participant":"native-codex-1234567890abcdef", "alias":"Lin", "native_child":true, "role":"subagent", "title":"Review code"}),
                "Lin",
                "worker",
                "Review code",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "attempt":"att_1234567890abcdef", "task_id":"task_1234567890abcdef", "role":"worker"}),
                "work unreported",
                "worker",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "task_id":"panel-012345abcdef", "role":"worker"}),
                "work unreported",
                "worker",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "task_id":"panel-release-review", "role":"worker"}),
                "panel-release-review",
                "worker",
                "",
            ),
            (
                json!({"participant":"native-codex-1234567890abcdef", "native_child":true, "role":"subagent", "title":"Native subagent"}),
                "work unreported",
                "worker",
                "",
            ),
            (
                json!({"participant":"att_1234567890abcdef", "role":"builder", "task_id":"T7", "title":"Room label"}),
                "Room label",
                "builder",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"Mira", "role":"worker", "state":"working"}),
                "Mira",
                "worker",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "role":"main", "title":"Main task"}),
                "call-1234567",
                "main",
                "Main task",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"codex-peer-review", "provider":"codex", "role":"reviewer", "title":"Review labels"}),
                "Review labels",
                "reviewer",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"claude-delegate", "provider":"claude", "role":"delegate", "title":"Fix parser"}),
                "Fix parser",
                "delegate",
                "",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"claude-peer-review", "provider":"codex", "role":"reviewer", "title":"Review labels"}),
                "claude-peer-review",
                "reviewer",
                "Review labels",
            ),
            (
                json!({"participant":"call-1234567890abcdef", "alias":"codex-builder", "provider":"codex", "role":"builder", "title":"Build feature"}),
                "codex-builder",
                "builder",
                "Build feature",
            ),
        ] {
            let display = call_display(&call);
            assert_eq!(
                (display.name.as_str(), display.role, display.title),
                (name, role, title),
                "{call}"
            );
        }
        for label in [
            "panel-012345abcde",
            "panel-012345abcdef0",
            "panel-012345abcdeF",
            "panel-012345abcdeg",
            "panel-release-review",
        ] {
            assert!(!synthetic_id(label), "{label}");
        }
        assert!(synthetic_id("panel-012345abcdef"));
        for columns in [56, 100, 160] {
            let rows = [
                json!({"participant":"call-1234567890abcdef","role":"worker","state":"working","event_tokens":{"total":12500}}),
                json!({"participant":"call-fedcba0987654321","alias":"Ada","role":"worker","title":"Fix parser","state":"working","event_tokens":{"total":9}}),
                json!({"participant":"call-1111222233334444","alias":"Ada","role":"worker","title":"Write docs","state":"working","event_tokens":{"total":9}}),
            ];
            let lines: Vec<String> = rows
                .iter()
                .map(|row| render_call_row(row, "", "", false, columns, true, false))
                .collect();
            assert!(lines[0].contains("work unreported"), "{}", lines[0]);
            assert!(lines[1].contains("Ada · Fix parser"), "{}", lines[1]);
            assert!(lines[2].contains("Ada · Write docs"), "{}", lines[2]);
            for line in &lines {
                assert!(!line.contains("call-") && !line.contains("att_"), "{line}");
                assert_eq!(width(line), columns, "{line}");
            }
        }
        let nav = navigation::Navigation::default();
        let read = BTreeMap::new();
        let board = board_frame(&report, &nav, 160, 30, &read, false, true);
        let output = board.lines.join("\n");
        for expected in [
            "Worker     Ada · Inspect code",
            "Worker     claude-id · Review code",
            "Worker     Research",
            "working",
            "idle",
            "unknown",
        ] {
            assert!(output.contains(expected), "missing {expected}: {output}");
        }
        for (provider, state) in [
            ("Codex", "working"),
            ("Claude", "idle"),
            ("Antigravity", "unknown"),
        ] {
            assert!(
                output
                    .lines()
                    .any(|line| line.contains(provider) && line.contains(state)),
                "{output}"
            );
        }
        assert!(
            !output.contains("appserver") && !output.contains("subagent") && !output.contains("↳"),
            "{output}"
        );
        assert!(
            board
                .worker_rows
                .values()
                .any(|(participant, _)| participant == "agy-id")
        );
        assert!(
            board
                .worker_rows
                .values()
                .any(|(participant, attempt)| participant == "codex-id"
                    && attempt == "codex-attempt")
        );
        for view in [
            View::Auto,
            View::Compact,
            View::Detail,
            View::Graph,
            View::Tree,
        ] {
            let nav = navigation::Navigation {
                view,
                ..Default::default()
            };
            let lines = frame_styled(&report, &nav, 160, 30, &read, false, true);
            let output = lines.join("\n");
            assert!(
                lines.iter().all(|line| width(line) <= 160),
                "{view:?}: {output}"
            );
            for expected in [
                "Worker     Ada · Inspect code",
                "Worker     claude-id · Review code",
                "Worker     Research",
                "Worker     work unreported",
            ] {
                assert!(
                    output.contains(expected),
                    "{view:?} missing {expected}: {output}"
                );
            }
            for duplicate in [
                "appserver",
                "subagent",
                "111111111111",
                "222222222222",
                "Worker     worker",
                "Worker     Research · Research",
            ] {
                assert!(
                    !output.contains(duplicate),
                    "{view:?} contains {duplicate}: {output}"
                );
            }
        }
        for graph in [false, true] {
            let output = tree(&calls(&report), &nav, graph, 10).join("\n");
            assert!(
                output.contains("Ada worker [working] Inspect code"),
                "{output}"
            );
            assert!(output.contains("Research worker [working]"), "{output}");
            assert!(
                !output.contains("appserver") && !output.contains("subagent"),
                "{output}"
            );
        }
        let detail = render_detail_card(
            calls(&report)
                .iter()
                .find(|c| text(c, "participant") == "codex-id"),
            100,
            96,
            true,
            false,
            usize::MAX,
        )
        .join("\n");
        for (label, raw) in [
            ("Participant", "codex-id"),
            ("Attempt", "codex-attempt"),
            ("Provider", "codex"),
            ("Runtime", "codex-runtime"),
            ("Role", "appserver"),
        ] {
            assert!(
                detail
                    .lines()
                    .any(|line| line.contains(label) && line.contains(raw)),
                "missing {label} {raw}: {detail}"
            );
        }
        let title =
            "Build the caption cafe\u{301}한글 without clipping any part or runtime metadata";
        let participant = "call-1234567890abcdef";
        let attempt = "att_1234567890abcdef";
        let headless = json!({"collection":{"ok":true}, "provider_sessions":["codex"],
        "room":{"id":"room-r", "main":"owner", "participants":[{
            "participant":participant, "attempt":attempt, "role":"worker", "provider":"codex",
            "parent":"owner", "state":"working", "title":title
        }]},
        "attempts":{"active":[{
            "attempt_id":attempt, "tool":"codex-runtime", "provider":"codex",
            "model":"gpt-6.1-sol", "state":"working", "refs":{
                "panel_room_id":"room-r", "panel_room_participant":participant,
                "panel_role":"worker", "panel_strategy":"builder"
            }
        }]}});
        for (columns, shown_title) in [(160, title), (78, "Build the"), (56, "Build")] {
            let board = board_frame(&headless, &nav, columns, 24, &read, false, true);
            let row = board
                .worker_rows
                .iter()
                .find(|(_, (member, id))| member == participant && id == attempt)
                .map(|(&index, _)| &board.lines[index])
                .unwrap();
            assert!(
                row.contains(&format!("Builder    {shown_title}"))
                    && !row.contains(['├', '└', '←']),
                "{row}"
            );
            if columns >= 78 {
                assert!(row.contains('—') && row.contains("[⟳ run]"), "{row}");
            }
            assert!(!row.contains("gpt-6.1-sol"), "{row}");
            assert_eq!(width(row), columns, "{row}");
        }
        let headless_calls = calls(&headless);
        assert_eq!(headless_calls[0]["role"], "worker");
        let detail = render_detail_card(headless_calls.first(), 120, 116, true, false, usize::MAX)
            .join("\n");
        for raw in [participant, attempt, "codex-runtime"] {
            assert!(detail.contains(raw), "missing {raw}: {detail}");
        }
        assert!(
            detail
                .lines()
                .any(|line| line.contains("Role") && line.contains("worker"))
        );
        assert!(!detail.contains("gpt-6.1-sol"), "{detail}");
        for alias in ["codex-delegate", "call-explicit-identifier"] {
            let mut explicitly_aliased = headless.clone();
            explicitly_aliased["room"]["participants"][0]["alias"] = json!(alias);
            explicitly_aliased["attempts"]["active"][0]["refs"]["panel_alias"] = json!(alias);
            let named_calls = calls(&explicitly_aliased);
            assert_eq!(call_display(&named_calls[0]).name, alias);
            assert_eq!(call_display(&named_calls[0]).title, title);
        }
        for (field, invalid) in [
            ("panel_strategy", "reviewer"),
            ("panel_strategy", "quoted builder"),
            ("panel_room_id", "foreign"),
            ("panel_room_participant", "another-worker"),
            ("panel_provider", "claude"),
        ] {
            let mut unrelated = headless.clone();
            unrelated["attempts"]["active"][0]["refs"][field] = json!(invalid);
            let unrelated_calls = calls(&unrelated);
            assert_eq!(call_display(&unrelated_calls[0]).role, "worker", "{field}");
        }
        let mut native = calls(&report)
            .into_iter()
            .find(|call| text(call, "participant") == "agy-id")
            .unwrap();
        native["title"] =
            json!("Research a practical path for the native panel and compare the options");
        let narrow_detail = render_detail_card(Some(&native), 44, 40, true, false, usize::MAX);
        assert!(
            narrow_detail
                .iter()
                .skip(1)
                .any(|line| line.contains("compare the options")),
            "{narrow_detail:?}"
        );
        let main = "att_8aa76df1a6c6415c8ce8d8ef2f639c67";
        let review = "att_0123456789abcdef0123456789abcdef";
        let child = |attempt: &str, tool: &str, alias: &str, role: &str, label: &str| {
            json!({"attempt_id":attempt, "tool":tool, "provider":"codex", "state":"working",
                "parent_attempt_id":main, "created_at":attempt,
                "refs":{"panel_room_id":"room-r", "panel_alias":alias, "panel_role":role, "panel_label":label}})
        };
        let room = json!({"collection":{"ok":true}, "provider_sessions":["codex"],
        "room":{"id":"room-r", "main":main, "participants":[
            {"participant":main, "attempt":main, "role":"main", "provider":"codex", "state":"working", "title":"Own goal"}
        ]},
        "attempts":{"active":[
            {"attempt_id":main, "tool":"panel-main", "provider":"codex", "state":"working", "refs":{"panel_room_id":"room-r", "panel_role":"main"}},
            child(review, "peer-review", "codex-peer-review", "reviewer", "Review label pass"),
            child("att_1", "peer-delegate", "codex-delegate", "delegate", "Fix parser"),
            child("att_2", "peer-delegate", "Ada", "builder", "Build feature"),
            child("att_3", "peer-review", "claude-peer-review", "reviewer", "Cross review")
        ]}});
        let output = board_frame(&room, &nav, 200, 40, &read, false, true)
            .lines
            .join("\n");
        let row = |name: &str| {
            output
                .lines()
                .find(|line| line.starts_with('│') && line.contains(&format!(" {name} ")))
                .unwrap_or_else(|| panic!("missing {name}: {output}"))
        };
        for name in [
            "Review label pass",
            "Fix parser",
            "Ada",
            "claude-peer-review",
        ] {
            assert!(!row(name).contains('←'), "{output}");
        }
        assert!(!output.contains("← att_"), "{output}");
        assert!(
            !output.contains("codex-peer-review") && !output.contains("codex-delegate"),
            "{output}"
        );
        assert!(row("Review label pass").contains("Reviewer   Review label pass"));
        assert!(row("Fix parser").contains("Delegate   Fix parser"));
        assert!(row("Ada").contains("Build feature"), "{output}");
        let calls = calls(&room);
        let reviewer = calls.iter().find(|call| call["attempt"] == review).unwrap();
        let detail =
            render_detail_card(Some(reviewer), 200, 196, true, false, usize::MAX).join("\n");
        for expected in [review, main, "codex-peer-review", "Review label pass"] {
            assert!(detail.contains(expected), "{expected}: {detail}");
        }
        let empty = json!({"collection":{"ok":true}, "provider_sessions":["codex"]});
        let output = frame(&empty, &nav, 160, 24, &read).join("\n");
        assert!(
            output
                .lines()
                .any(|line| line.contains("Codex · Lead chat [1]")
                    && line.contains("state unreported")),
            "{output}"
        );
    }

    #[test]
    fn finished_workers_leave_overview_without_hiding_review() {
        let report = json!({
            "collection":{"ok":true},
            "attempts":{"recent":[
                {"attempt_id":"read-done","provider":"codex","state":"review","refs":{"panel_access":"read"}},
                {"attempt_id":"read-open","provider":"codex","state":"review","refs":{"panel_access":"read"}},
                {"attempt_id":"write-review","provider":"codex","state":"review","refs":{"panel_access":"write"}},
                {"attempt_id":"write-failed","provider":"codex","state":"failed","refs":{"panel_access":"write"}},
                {"attempt_id":"approved","task_id":"accepted-task","provider":"codex","state":"review",
                    "refs":{"panel_access":"write"}}
            ]},
            "returned_workers":{"read-done":"2026-11-19T07:00:00Z"},
            "finalized":{"accepted-task":"accepted"}
        });
        let read = BTreeMap::new();
        let attention = attention::items(&report, &read);
        for (id, expected) in [
            ("read-done", false),
            ("read-open", true),
            ("write-review", true),
            ("write-failed", true),
            ("approved", false),
        ] {
            assert_eq!(
                attention.iter().any(|item| item.row_id.1 == id),
                expected,
                "{id}"
            );
        }
        let nav = navigation::Navigation {
            attention_only: true,
            view: View::Compact,
            ..Default::default()
        };
        let filtered = visible(&report, &nav);
        assert!(
            !filtered
                .iter()
                .any(|call| call["attempt"] == "read-done" || call["attempt"] == "approved")
        );
        let all = navigation::Navigation {
            attention_only: false,
            ..nav
        };
        let done = visible(&report, &all)
            .into_iter()
            .find(|call| call["attempt"] == "read-done")
            .expect("returned call remains in history");
        assert!(
            render_detail_card(Some(&done), 100, 96, true, false, usize::MAX)
                .iter()
                .any(|line| line.contains("✓ returned"))
        );

        let now = oms_panel_core::timestamp();
        let nav = navigation::Navigation::default();
        let done = json!({"state":"done","updated_at":now});
        let failed = json!({"state":"failed","updated_at":now});
        let old = json!({"state":"failed","updated_at":now.saturating_sub(360)});
        let review = json!({"state":"review","updated_at":now.saturating_sub(360)});
        let returned_at = chrono::DateTime::from_timestamp(now.try_into().unwrap(), 0)
            .unwrap()
            .to_rfc3339();
        let returned = json!({"state":"review","returned_at":returned_at});
        let write = json!({"state":"done","updated_at":now,"refs":{"panel_access":"write"}});
        assert!(collapsed_history(&nav, &done));
        assert!(collapsed_history(&nav, &returned));
        assert!(!collapsed_history(&nav, &failed));
        assert!(collapsed_history(&nav, &old));
        assert!(!collapsed_history(&nav, &review));
        assert!(fold_inactive(&nav, &review));
        let fresh_review = json!({"state":"review","updated_at":now});
        assert!(!fold_inactive(&nav, &fresh_review));
        assert!(!collapsed_history(&nav, &write));
        let codex_stub = json!({"native_child":true,"provider":"codex","state":"unknown"});
        assert!(fold_inactive(&nav, &codex_stub));
        let mut active_native = codex_stub.clone();
        active_native["state"] = json!("working");
        assert!(!fold_inactive(&nav, &active_native));
        assert!(!fold_inactive(
            &nav,
            &json!({"provider":"codex","state":"unknown"})
        ));

        let report = |outcome: &str, state: &str| {
            json!({
                "attempts":{"recent":[{
                    "attempt_id":"builder-1","task_id":"build","provider":"codex",
                    "tool":"codex-delegate","state":state,"refs":{"panel_access":"write"}
                }]},
                "finalized":{"build":outcome}
            })
        };
        let rows = calls(&report("accepted", "review"));
        assert_eq!(rows[0]["state"], "review");
        assert!(collapsed_history(&nav, &rows[0]));
        assert!(!collapsed_history(
            &nav,
            &calls(&report("failed", "review"))[0]
        ));
        assert!(!collapsed_history(
            &nav,
            &calls(&report("accepted", "working"))[0]
        ));

        let dated = report("unaccepted", "review");
        let mut dated = dated;
        dated["attempts"]["recent"][0]["updated_at"] = json!(now.saturating_sub(360));
        dated["attempts"]["recent"][0]["title"] = json!("Pending decision");
        dated["native_children"] = json!([{
            "participant":"native-codex-stub","native_child":true,"provider":"codex","state":"unknown",
            "title":"Recorded native child"
        }]);
        let read = BTreeMap::new();
        let overview = frame(&dated, &nav, 120, 26, &read).join("\n");
        assert!(
            overview.contains("1 review") && overview.contains("2 folded (Tab all)"),
            "{overview}"
        );
        // The folded review item surfaces in the Attention band while staying out of provider rows.
        assert!(
            overview.contains("Attention 1 · 1 decide") && overview.contains("Pending decision"),
            "{overview}"
        );
        let provider_section = overview
            .split_once("╭─ Codex")
            .map(|(_, rest)| rest)
            .unwrap_or(&overview);
        assert!(
            !provider_section.contains("Pending decision")
                && !overview.contains("Recorded native child"),
            "{overview}"
        );
        let attention = navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        assert!(
            visible(&dated, &attention)
                .iter()
                .any(|row| row["title"] == "Pending decision")
        );
        let history = navigation::Navigation {
            view: View::Auto.next(),
            ..Default::default()
        };
        assert!(
            visible(&dated, &history)
                .iter()
                .any(|row| row["title"] == "Recorded native child")
        );

        for other in [
            navigation::Navigation {
                view: View::Auto.next(),
                ..Default::default()
            },
            navigation::Navigation {
                filter: "worker".into(),
                ..Default::default()
            },
            navigation::Navigation {
                attention_only: true,
                ..Default::default()
            },
        ] {
            assert!(!collapsed_history(&other, &done));
            assert!(!collapsed_history(&other, &returned));
            assert!(!collapsed_history(&other, &rows[0]));
            assert!(!fold_inactive(&other, &review));
            assert!(!fold_inactive(&other, &codex_stub));
        }
    }

    #[test]
    fn started_only_native_child_shows_age_in_the_shared_time_slot() {
        let report = |extra: Value| {
            let mut report = json!({"generated_at": 10_180, "collection":{"ok":true}, "native_children":[{
                "participant":"native-claude-a1","alias":"↳","native_child":true,"provider":"claude",
                "state":"working","title":"Aged child","started_at":10_000
            }]});
            for (key, value) in extra.as_object().unwrap() {
                report[key] = value.clone();
            }
            report
        };
        let history = navigation::Navigation {
            filter: "Aged".into(),
            ..Default::default()
        };
        let shown =
            |report: &Value, columns: usize| frame(report, &history, columns, 24, &BTreeMap::new());
        for columns in [60, 80, 140] {
            let board = shown(&report(json!({})), columns);
            let row = board.iter().find(|line| line.contains("3m ago")).unwrap();
            let frame_width = width(&board[0]).max(width(row));
            assert!(
                board.iter().all(|line| width(line) <= columns)
                    && row.trim_end().ends_with("3m ago │")
                    && width(row) == width(board.iter().find(|l| l.starts_with('├')).unwrap()),
                "{columns}/{frame_width}: {board:?}"
            );
        }
        for hidden in [
            json!({"collection":{"ok":true,"pending":true}}),
            json!({"capacity_stale":true}),
        ] {
            let board = shown(&report(hidden), 80).join("\n");
            assert!(
                board.contains("Aged child") && !board.contains(" ago"),
                "{board}"
            );
        }
    }

    #[test]
    fn recently_seen_codex_child_shows_a_compact_marker_and_stays_unknown() {
        let report = |seen: Value| {
            json!({"generated_at": 10_000, "collection":{"ok":true}, "native_children":[{
                "participant":"native-codex-x1","native_child":true,"provider":"codex",
                "state":"unknown","title":"Seen child","seen_at":seen.as_u64().map(|seconds| seconds * 1000),"seen_observed_ms":10_000_000
            }]})
        };
        let history = navigation::Navigation {
            filter: "Seen".into(),
            ..Default::default()
        };
        let shown =
            |report: &Value, columns: usize| frame(report, &history, columns, 24, &BTreeMap::new());
        let board = shown(&report(json!(9_970)), 120).join("\n");
        assert!(board.contains("seen 30s"), "{board}");
        assert!(board.contains("unknown"), "{board}");
        for stale in [json!(9_879), json!(null), json!("9970"), json!(10_006)] {
            let board = shown(&report(stale), 120).join("\n");
            assert!(
                board.contains("Seen child") && !board.contains("seen "),
                "{board}"
            );
        }
        for columns in [19, 28, 40, 55, 56] {
            for line in shown(&report(json!(9_970)), columns) {
                assert!(width(&line) <= columns, "{columns}: {line}");
            }
        }
    }

    #[test]
    fn native_seen_label_requires_unknown_state_and_a_bounded_age() {
        let fresh = json!({"collection":{"ok":true}});
        let call = |state: &str, seen: u64| json!({"state":state,"seen_at":seen,"seen_observed_ms":10_000_000});
        let at = |state: &str, seen: u64| native_seen_label(&fresh, &call(state, seen), 10_000.0);
        assert_eq!(at("unknown", 9_880_000).as_deref(), Some("seen 120s"));
        assert_eq!(at("unknown", 9_879_999), None);
        assert_eq!(at("unknown", 10_005_000).as_deref(), Some("seen 0s"));
        assert_eq!(at("unknown", 10_005_001), None);
        for state in ["running", "working", "done", "review", ""] {
            assert_eq!(at(state, 9_970_000), None, "{state}");
        }
        let report = json!({"generated_at": 10_000, "collection":{"ok":true}, "native_children":[{
            "participant":"native-codex-x1","native_child":true,"provider":"codex",
            "state":"unknown","title":"Seen child","seen_at":9_970_000,"seen_observed_ms":10_000_000
        }]});
        // Only Codex children carry the marker.
        for provider in ["claude", "antigravity"] {
            let mut other = report.clone();
            other["native_children"][0]["provider"] = json!(provider);
            for view in [View::Auto, View::Compact] {
                let nav = navigation::Navigation {
                    view,
                    ..Default::default()
                };
                let board = frame(&other, &nav, 120, 24, &BTreeMap::new()).join("\n");
                assert!(
                    board.contains("Seen child") && !board.contains("seen "),
                    "{provider}: {board}"
                );
            }
        }
        // A non-unknown state with a fresh seen_at shows no marker.
        let mut running = report.clone();
        running["native_children"][0]["state"] = json!("running");
        let filtered = navigation::Navigation {
            filter: "Seen".into(),
            ..Default::default()
        };
        let board = frame(&running, &filtered, 120, 24, &BTreeMap::new()).join("\n");
        assert!(
            board.contains("Seen child") && !board.contains("seen "),
            "{board}"
        );
    }

    #[test]
    fn default_overview_unfolds_up_to_three_recently_seen_codex_children() {
        let child = |name: &str, seen: Value| {
            json!({"participant":format!("native-codex-{name}"),"native_child":true,
                "provider":"codex","state":"unknown","title":format!("Child {name}"),
                "seen_at":seen.as_u64().map(|seconds| seconds * 1000),"seen_observed_ms":10_000_000})
        };
        let mut forged = child("h", json!(9_000));
        forged["board_recent"] = json!(true);
        let report = json!({"generated_at": 10_000, "collection":{"ok":true}, "native_children":[
            child("a", json!(9_990)), child("b", json!(9_980)), child("c", json!(9_970)),
            child("d", json!(9_960)), child("e", json!(9_879)), child("f", json!(10_006)),
            child("g", Value::Null), forged,
        ]});
        let read = BTreeMap::new();
        let overview_nav = navigation::Navigation::default();
        let overview = frame(&report, &overview_nav, 120, 30, &read);
        let board = overview.join("\n");
        for (name, age) in [("a", 10), ("b", 20), ("c", 30)] {
            let line = overview
                .iter()
                .find(|line| line.contains(&format!("Child {name}")))
                .unwrap_or_else(|| panic!("{board}"));
            assert!(line.contains(&format!("seen {age}s")), "{line}");
            assert!(line.contains("unknown"), "{line}");
            assert!(
                !line.contains("running") && !line.contains("done"),
                "{line}"
            );
        }
        for name in ["d", "e", "f", "g", "h"] {
            assert!(!board.contains(&format!("Child {name}")), "{board}");
        }
        assert!(board.contains("5 folded (Tab all)"), "{board}");
        // Tab keeps every row; only the fresh ones carry a marker.
        let tab = navigation::Navigation {
            view: View::Auto.next(),
            ..Default::default()
        };
        assert_eq!(visible(&report, &tab).len(), 8);
        let tab_board = frame(&report, &tab, 120, 60, &read).join("\n");
        assert!(
            tab_board.contains("Child e") && tab_board.contains("Child g"),
            "{tab_board}"
        );
        assert_eq!(tab_board.matches("seen ").count(), 4, "{tab_board}");
        // A seen row in the overview hits the same target as in Tab.
        for columns in [120, 40] {
            let hits = |nav: &navigation::Navigation| {
                board_frame(&report, nav, columns, 60, &read, false, false)
                    .worker_rows
                    .into_values()
                    .filter(|(participant, _)| participant.starts_with("native-codex-"))
                    .collect::<BTreeSet<_>>()
            };
            let shown = hits(&overview_nav);
            let expected: BTreeSet<_> = ["a", "b", "c"]
                .map(|name| (format!("native-codex-{name}"), String::new()))
                .into();
            assert_eq!(shown, expected, "{columns}");
            assert!(shown.is_subset(&hits(&tab)), "{columns}");
        }
        let selectable = visible(&report, &overview_nav);
        assert_eq!(
            selectable
                .iter()
                .map(|call| text(call, "participant"))
                .collect::<BTreeSet<_>>(),
            ["native-codex-a", "native-codex-b", "native-codex-c"].into()
        );
        // Fresh rows arrive in any order; the three newest stay unfolded.
        let mut shuffled = report.clone();
        shuffled["native_children"] = json!([
            child("w", json!(9_960)),
            child("x", json!(9_990)),
            child("y", json!(9_970)),
            child("z", json!(9_980)),
        ]);
        shuffled["native_children"][1]["seen_at"] = json!(9_981_900);
        shuffled["native_children"][3]["seen_at"] = json!(9_981_100);
        shuffled["native_children"][1]["created_at"] = json!("z");
        shuffled["native_children"][3]["created_at"] = json!("a");
        let unfolded: Vec<_> = visible(&shuffled, &overview_nav)
            .iter()
            .map(|call| text(call, "participant").to_owned())
            .collect();
        assert_eq!(
            unfolded,
            ["native-codex-x", "native-codex-z", "native-codex-y"]
        );
        for (phase, stale) in [
            (json!({"ok":false}), false),
            (json!({"ok":true,"pending":true}), false),
            (json!({"ok":true}), true),
        ] {
            let mut retained = report.clone();
            retained["collection"] = phase;
            retained["capacity_stale"] = json!(stale);
            assert!(visible(&retained, &overview_nav).is_empty());
            let history = frame(&retained, &tab, 120, 60, &read).join("\n");
            assert!(history.contains("Child a") && !history.contains("seen "));
        }
        // Without a report clock nothing is fresh, so every stub stays folded.
        let mut undated = report.clone();
        undated.as_object_mut().unwrap().remove("generated_at");
        assert!(visible(&undated, &overview_nav).is_empty());
    }

    #[test]
    fn default_overview_preserves_codex_discovery_order_at_equal_recency() {
        const NOW: u64 = 1_800_000_000;
        let child = |name: &str, envelope: &str| {
            json!({"participant":format!("native-codex-{name}"), "title":format!("Child {name}"),
                "native_child":true, "provider":"codex", "state":"unknown", "created_at":envelope})
        };
        for interacted in [true, false] {
            let recent = |name, envelope| {
                let mut row = child(name, envelope);
                let at = (NOW - 1) * 1000;
                if interacted {
                    row["seen_at"] = json!(at);
                    row["seen_observed_ms"] = json!(NOW * 1000);
                } else {
                    row["codex_recent_creation"] = json!({"at_ms":at,"observed_ms":NOW * 1000});
                }
                row
            };
            // Both the global envelope and participant sort prefer Alpha.
            let report = json!({"generated_at":NOW,"collection":{"ok":true},"native_children":[
                recent("Zulu", "2027-01-15T08:00:02Z"),
                child("History", "2027-01-15T08:00:01Z"),
                recent("Alpha", "2027-01-15T08:00:00Z"),
            ]});
            let ids = |rows: Vec<Value>| {
                rows.into_iter()
                    .map(|row| text(&row, "participant").to_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                ids(visible(&report, &navigation::Navigation::default())),
                ["native-codex-Zulu", "native-codex-Alpha"],
            );
            assert_eq!(
                ids(calls(&report)),
                [
                    "native-codex-Zulu",
                    "native-codex-History",
                    "native-codex-Alpha"
                ],
            );
        }
    }

    #[test]
    fn default_overview_unfolds_verified_recent_creations_without_claiming_activity() {
        const NOW: u64 = 1_800_000_000;
        let child = |name: &str| {
            json!({"participant":format!("native-codex-{name}"), "title":format!("Child {name}"),
                "native_child":true, "provider":"codex", "state":"unknown"})
        };
        let created = |name, at: u64| {
            let mut row = child(name);
            row["codex_recent_creation"] = json!({"at_ms":at,"observed_ms":NOW * 1000});
            row
        };
        let mut interacted = child("Seen");
        interacted["seen_at"] = json!((NOW - 30) * 1000);
        interacted["seen_observed_ms"] = json!(NOW * 1000);
        let mut historical = child("Historical");
        historical["started_at"] = json!(NOW - 2);
        historical["updated_at"] = json!(NOW - 1);
        historical["board_recent"] = json!(true);
        let newest = created("Newest", (NOW - 2) * 1000);
        let report = json!({"generated_at":NOW,"collection":{"ok":true},"native_children":[
            historical, created("Other", (NOW - 8) * 1000), interacted, newest,
            created("Older", (NOW - 10) * 1000), created("Future", NOW * 1000 + 5_001),
        ]});
        let overview = navigation::Navigation::default();
        let tab = navigation::Navigation {
            view: overview.view.next(),
            ..Default::default()
        };
        let names = |report: &Value, nav: &navigation::Navigation| {
            visible(report, nav)
                .iter()
                .map(|row| {
                    assert_eq!(row["state"], "unknown");
                    text(row, "participant").to_owned()
                })
                .collect::<Vec<_>>()
        };
        let expected = [
            "native-codex-Seen",
            "native-codex-Newest",
            "native-codex-Other",
        ];
        assert_eq!(names(&report, &overview), expected);
        assert_eq!(names(&report, &tab).len(), 6);
        let read = BTreeMap::new();
        let board = frame(&report, &overview, 120, 30, &read);
        let new_line = board
            .iter()
            .find(|line| line.contains("Child Newest"))
            .unwrap();
        assert!(
            new_line.contains("unknown") && !new_line.contains("seen "),
            "{new_line}"
        );
        assert!(board.iter().all(|line| !line.contains("1 running")));
        for columns in [40, 120] {
            let rendered = board_frame(&report, &overview, columns, 35, &read, false, false);
            let hits: BTreeSet<_> = rendered
                .worker_rows
                .into_values()
                .map(|(id, _)| id)
                .collect();
            assert_eq!(hits, expected.map(str::to_owned).into(), "{columns}");
        }
        let mut expired = report.clone();
        expired["generated_at"] = json!(NOW + 121);
        assert!(names(&expired, &overview).is_empty());
        assert_eq!(names(&expired, &tab).len(), 6);
        let mut pending = report;
        pending["collection"]["pending"] = json!(true);
        assert!(names(&pending, &overview).is_empty());
    }

    #[test]
    fn board_footer_preserves_essential_keys() {
        let report = json!({});
        for view in [View::Auto, View::Compact, View::Tree, View::Graph] {
            let nav = navigation::Navigation {
                view,
                ..Default::default()
            };
            for columns in [19, 28, 40, 56, 100, 120] {
                let frame =
                    frame_styled(&report, &nav, columns, 24, &BTreeMap::new(), false, false);
                let footer = frame.last().unwrap();
                for key in ["↑↓", "Enter", "?", "q"] {
                    assert!(footer.contains(key), "{view:?} {columns}: {footer}");
                }
                assert!(width(footer) <= columns);
                if columns >= 40 {
                    let tab = if view == View::Auto {
                        "Tab All"
                    } else {
                        "Tab Overview"
                    };
                    assert!(footer.contains(tab), "{view:?} {columns}: {footer}");
                    assert!(footer.contains("D Inspect"), "{view:?} {columns}: {footer}");
                }
                if columns >= 56 && matches!(view, View::Auto | View::Compact) {
                    assert!(footer.contains("g/t Tree"), "{view:?} {columns}: {footer}");
                }
                let colored =
                    frame_styled(&report, &nav, columns, 24, &BTreeMap::new(), true, false);
                assert_eq!(strip_sgr(colored.last().unwrap()), *footer);
            }
        }
        assert_eq!(view_label(View::Graph), "Relationships");
        assert_eq!(view_label(View::Tree), "Relationships");
    }

    #[test]
    fn help_omits_unimplemented_actions() {
        let help = render_help_card(120, 116, true, false).join("\n");
        for false_claim in [
            "Focus Claude",
            "Swap main",
            "Rooms /",
            "Controls /",
            "Dispatch Explain",
            "Review gate",
            "Council debate",
            "Retry delivery",
        ] {
            assert!(!help.contains(false_claim), "{false_claim}: {help}");
        }
    }

    #[test]
    fn main_model_states_are_fresh_last_known_or_unverified() {
        let report = |observation: Value| {
            let mut report = json!({
                "collection": {"ok": true},
                "room": {"id": "room-model", "owner": "main-1", "participants": [{
                    "participant": "main-1", "role": "main", "provider": "codex",
                    "model": "gpt-6.1-sol", "joined": true
                }]},
                "room_attempts": {"main-1": {"attempt_id": "main-1", "tool": "panel-main",
                    "provider": "codex", "state": "working",
                    "refs": {"panel_room_id": "room-model", "panel_role": "main"}}}
            });
            if !observation.is_null() {
                report["main_model_observation"] = observation;
            }
            report
        };
        let obs = |attempt: &str, source: Option<&str>| {
            let mut v = json!({"attempt": attempt, "model": "gpt-6.2-sol"});
            if let Some(source) = source {
                v["source"] = json!(source);
            }
            v
        };
        let read = BTreeMap::new();
        let nav = navigation::Navigation::default();
        let main_line = |report: &Value, columns: usize| {
            let board = board_frame(report, &nav, columns, 24, &read, false, true);
            let (&row, _) = board.worker_rows.first_key_value().unwrap();
            board.lines[row].clone()
        };
        let cases = [
            (obs("main-1", Some("rollout")), "fresh", "GPT-6.2-Sol"),
            (obs("main-1", Some("rollout-last")), "last", "~GPT-6.2-Sol"),
            (obs("main-1", Some("footer")), "footer", "GPT-6.2-Sol"),
            (obs("main-1", Some("footer-last")), "last", "~GPT-6.2-Sol"),
            (
                obs("main-1", Some("future-kind")),
                "unverified",
                "gpt-6.1-sol?",
            ),
            (obs("main-1", None), "unverified", "gpt-6.1-sol?"),
            (Value::Null, "unverified", "gpt-6.1-sol?"),
            (obs("other", Some("rollout")), "unverified", "gpt-6.1-sol?"),
        ];
        for (observation, state, label) in cases {
            let report = report(observation);
            let main = &calls(&report)[0];
            assert_eq!(main["model_state"], state, "{main}");
            assert_eq!(model_label(main), label);
            for columns in [80, 95, 100] {
                let line = main_line(&report, columns);
                assert!(line.contains(label), "{columns}: {line}");
                if state == "fresh" {
                    assert!(!line.contains('~') && !line.contains('?'), "{line}");
                }
            }
        }
        // A mismatch never promotes the observation's model.
        let mismatch = report(obs("other", Some("rollout")));
        assert_eq!(calls(&mismatch)[0]["model"], "gpt-6.1-sol");
        let detail = model_detail(&calls(&mismatch)[0]);
        assert_eq!(detail, "gpt-6.1-sol · configured, unverified");
        assert_eq!(
            model_detail(&calls(&report(obs("main-1", Some("rollout-last"))))[0]),
            "gpt-6.2-sol · last known"
        );
        let footer = report(obs("main-1", Some("footer")));
        assert_eq!(
            model_detail(&calls(&footer)[0]),
            "gpt-6.2-sol · observed in pane"
        );
        let mut held = footer;
        held["collection"]["pending"] = json!(true);
        assert_eq!(model_label(&calls(&held)[0]), "~GPT-6.2-Sol");
        // A Main with nothing stored and nothing observed reads as unknown.
        let mut empty = report(Value::Null);
        empty["room"]["participants"][0]["model"] = json!("");
        assert_eq!(model_label(&calls(&empty)[0]), "—");
    }

    #[test]
    fn pending_checkout_requires_valid_bound_display_only_proof() {
        let read = BTreeMap::new();
        let make_pending = |proof: Option<Value>| {
            let mut report = json!({
                "collection": {"ok": true, "pending": true},
                "room": {
                    "id": "room-checkout",
                    "closed": false,
                    "successor": null,
                    "owner": "main-1",
                    "participants": [{
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "attempt": "main-1",
                        "state": "working",
                        "joined": true,
                    }]
                },
                "room_attempts": {
                    "main-1": {
                        "attempt_id": "main-1",
                        "tool": "panel-main",
                        "provider": "codex",
                        "state": "working",
                        "created_at": "2026-11-23T05:00:00Z",
                        "refs": {"panel_room_id": "room-checkout", "panel_role": "main"}
                    }
                },
                "board_checkout": {
                    "attempt": "main-1",
                    "branch": "raw-untrusted",
                    "worktree": "raw-wt",
                    "detached": false
                }
            });
            if let Some(proof) = proof {
                report["board_checkout_last_known"] = proof;
            }
            report
        };

        // Raw `board_checkout` without explicit display-only proof is never trusted on pending.
        let raw_only = frame(&make_pending(None), &Default::default(), 100, 20, &read).join("\n");
        assert!(
            raw_only.contains("Checkout unknown") && !raw_only.contains("raw-untrusted"),
            "{raw_only}"
        );

        // Valid bound display-only proof preserves the stable label during pending refresh.
        let valid = make_pending(Some(json!({
            "room": "room-checkout",
            "attempt": "main-1",
            "branch": "oms/review",
            "worktree": "scratch-r37",
            "detached": false,
            "display_only": true
        })));
        let rendered = frame(&valid, &Default::default(), 100, 20, &read).join("\n");
        assert!(
            rendered.contains("Branch oms/review · Worktree scratch-r37")
                && !rendered.contains("Main elapsed"),
            "{rendered}"
        );

        // Detached valid bound proof also renders cleanly.
        let detached = make_pending(Some(json!({
            "room": "room-checkout",
            "attempt": "main-1",
            "branch": null,
            "worktree": "scratch-r37",
            "detached": true,
            "display_only": true
        })));
        let rendered = frame(&detached, &Default::default(), 100, 20, &read).join("\n");
        assert!(
            rendered.contains("Branch detached · Worktree scratch-r37"),
            "{rendered}"
        );

        // Wrong room, wrong attempt, missing display_only, or malformed metadata degrade to unknown.
        for bad in [
            json!({"room": "other-room", "attempt": "main-1", "branch": "oms/review", "worktree": "scratch-r37", "detached": false, "display_only": true}),
            json!({"room": "room-checkout", "attempt": "main-2", "branch": "oms/review", "worktree": "scratch-r37", "detached": false, "display_only": true}),
            json!({"room": "room-checkout", "attempt": "main-1", "branch": "oms/review", "worktree": "scratch-r37", "detached": false, "display_only": false}),
            json!({"room": "room-checkout", "attempt": "main-1", "branch": "bad branch", "worktree": "scratch-r37", "detached": false, "display_only": true}),
            json!({"room": "room-checkout", "attempt": "main-1", "branch": "oms/review", "worktree": "../escape", "detached": false, "display_only": true}),
            json!({"room": "room-checkout", "attempt": "main-1", "branch": "oms/review", "worktree": "scratch-r37", "detached": true, "display_only": true}),
        ] {
            let line = frame(
                &make_pending(Some(bad)),
                &Default::default(),
                100,
                20,
                &read,
            )
            .join("\n");
            assert!(line.contains("Checkout unknown"), "{line}");
        }

        // Closed/superseded room or stale/failed collection ignores the display-only proof.
        let mut stale = valid.clone();
        stale["capacity_stale"] = json!(true);
        assert!(
            frame(&stale, &Default::default(), 100, 20, &read)
                .join("\n")
                .contains("Checkout unknown")
        );
        let mut closed = valid;
        closed["room"]["closed"] = json!(true);
        assert!(
            frame(&closed, &Default::default(), 100, 20, &read)
                .join("\n")
                .contains("Checkout unknown")
        );
    }

    #[test]
    fn viewport_detail_attention_and_geometry() {
        let report = json!({"collection":{"ok":true}, "room":{"title":"room", "participants":[
            {"participant":"main", "role":"main", "state":"working", "model":"long-model", "title":"Live"},
            {"participant":"old", "role":"main", "state":"done", "title":"History"},
            {"participant":"review", "state":"review", "title":"Review"},
            {"participant":"blocked", "state":"blocked", "title":"Blocked", "provider":"codex", "model":"m1", "task_id":"task-1",
                "refs":{"plan_id":"plan-1"}}
        ]}, "plan":{"plan_id":"plan-1", "goal":"Ship", "tasks":{"one":{"state":"done"}, "task-1":{"state":"running"}}}, "provider_status":{"codex":{"weekly_used":25}}});
        let mut observed = report.clone();
        observed["room"]["participants"][0]["attempt"] = json!("attempt-main");
        observed["room"]["participants"][0]["provider"] = json!("codex");
        observed["room_attempts"]["attempt-main"] = json!({
            "attempt_id":"attempt-main", "provider":"codex", "role":"main", "state":"working"
        });
        observed["main_model_observation"] =
            json!({"attempt":"attempt-main","model":"observed-model","source":"rollout"});
        assert_eq!(calls(&observed)[0]["model"], "observed-model");
        observed["main_model_observation"]["attempt"] = json!("unrelated");
        assert_eq!(calls(&observed)[0]["model"], "long-model");

        let implicit_main = json!({
            "collection": {"ok": true},
            "room": {"id": "room-model", "owner": "main-1", "participants": [{
                "participant": "main-1", "role": "main", "provider": "codex",
                "model": "gpt-6.1-sol", "joined": true
            }]},
            "room_attempts": {
                "main-1": {"attempt_id": "main-1", "tool": "panel-main",
                    "provider": "codex", "state": "working",
                    "refs": {"panel_room_id": "room-model", "panel_role": "main"}},
                "council-1": {"attempt_id": "council-1", "tool": "panel-council",
                    "provider": "codex", "state": "failed", "terminal": true,
                    "refs": {"panel_room_id": "room-model", "panel_role": "advisor",
                        "panel_room_participant": "main-1"}}
            },
            "main_model_observation": {"attempt": "main-1", "model": "gpt-6.2-sol", "source": "rollout"}
        });
        let main_calls = calls(&implicit_main);
        assert_eq!(main_calls[0]["attempt"], "main-1");
        assert_eq!(main_calls[0]["state"], "working");
        assert_eq!(main_calls[0]["model"], "gpt-6.2-sol");

        let order_report = json!({"room":{"participants":[
            {"participant":"same", "state":"done", "created_at":"2", "title":"first"},
            {"participant":"z", "state":"working", "created_at":"1"},
            {"participant":"same", "state":"done", "created_at":"2", "title":"second"},
            {"participant":"a", "state":"done", "created_at":"2"},
            {"participant":"main", "role":"main", "state":"working", "created_at":"3"},
            {"participant":"early", "state":"done", "created_at":"1"}
        ]}});
        let ordered = calls(&order_report);
        assert_eq!(
            ordered
                .iter()
                .map(|row| text(row, "participant"))
                .collect::<Vec<_>>(),
            ["main", "z", "early", "a", "same", "same"]
        );
        assert_eq!(
            ordered
                .iter()
                .skip(4)
                .map(|row| text(row, "title"))
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let read = BTreeMap::new();
        let overview = frame(&report, &navigation::Navigation::default(), 80, 24, &read);
        let main_top = overview
            .iter()
            .position(|line| line.starts_with("╭─ Overall Main "))
            .expect("Main must be the first overview card");
        let status = overview
            .iter()
            .position(|line| line.contains("Weekly left"))
            .unwrap();
        let legend = overview
            .iter()
            .position(|line| line.contains("Gate"))
            .unwrap();
        assert!(status < legend && legend < main_top, "{overview:?}");
        let main_bottom = overview
            .iter()
            .enumerate()
            .skip(main_top + 1)
            .find(|(_, line)| line.starts_with('╰'))
            .map(|(index, _)| index)
            .unwrap();
        let goal = overview
            .iter()
            .position(|line| line.contains("Goal: Ship"))
            .unwrap();
        assert!(main_top < goal && goal < main_bottom, "{overview:?}");
        assert!(overview[main_bottom + 1].starts_with("╭─ "), "{overview:?}");
        assert!(!overview.iter().any(|line| line.contains("! Attention")));
        let mut nav = navigation::Navigation {
            view: View::Detail,
            ..Default::default()
        };
        let calls = visible(&report, &nav);
        assert_eq!(text(&calls[0], "participant"), "main");
        assert!(
            calls
                .iter()
                .position(|v| text(v, "participant") == "blocked")
                .unwrap()
                < calls
                    .iter()
                    .position(|v| text(v, "participant") == "old")
                    .unwrap()
        );
        nav.selected = calls
            .iter()
            .position(|v| text(v, "participant") == "blocked")
            .unwrap();
        for columns in [19, 28, 40, 80, 120] {
            let frame = frame_styled(&report, &nav, columns, 24, &read, false, true);
            println!("FRAME {columns}\n{}", frame.join("\n"));
            assert!(
                frame
                    .iter()
                    .any(|v| v.contains("▸") && v.to_lowercase().contains("blo")),
                "{frame:?}"
            );
            assert!(
                frame
                    .iter()
                    .any(|v| v.contains("codex") && v.contains("m1")),
                "{frame:?}"
            );
            assert!(frame.iter().all(|v| width(v) <= columns), "{frame:?}");
            assert!(page_capacity(&report, &nav, columns, 24, &read) >= 1);
        }
        let mut stale_usage = report.clone();
        stale_usage["provider_status"]["claude"] =
            json!({"source":"cache stale", "weekly_used":30});
        let now = oms_panel_core::timestamp() as f64;
        let usage = provider_line(
            &stale_usage,
            &crate::usage::snapshot(&stale_usage, now),
            now,
            80,
            false,
            false,
        )
        .0;
        assert!(usage.contains("Claude stale"), "{usage}");
        assert!(!usage.contains("30%"), "{usage}");
        let mut stale = report.clone();
        stale["collection"]["ok"] = json!(false);
        stale["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .push(json!({"participant":"extra", "state":"blocked", "title":"Extra"}));
        nav.filter = "".into();
        nav.editing = true;
        let frame = frame_styled(&stale, &nav, 56, 12, &read, false, false);
        assert!(
            frame
                .iter()
                .any(|line| line.contains("▸") && line.to_lowercase().contains("blocked")),
            "{frame:?}"
        );
        assert!(
            frame.iter().any(|line| line.contains("hidden")),
            "{frame:?}"
        );
        for columns in [19, 28, 40, 55, 56, 80, 120] {
            for height in [8, 10, 12, 14, 24] {
                let frame = frame_styled(&stale, &nav, columns, height, &read, false, false);
                assert!(frame.len() <= height, "{columns}x{height}: {frame:?}");
                assert!(
                    frame.iter().all(|line| width(line) <= columns),
                    "{columns}x{height}: {frame:?}"
                );
                for required in ["▸", "!", "Filter", "STALE"] {
                    assert!(
                        frame.iter().any(|line| line.contains(required)),
                        "{columns}x{height} missing {required}: {frame:?}"
                    );
                }
            }
        }
        for cols in [44, 80] {
            let line = card_top(
                "매우 긴 제목 아주 긴 제목 긴 제목 매우 긴 제목",
                Some("Escape back"),
                cols,
                false,
            );
            assert_eq!(width(&line), cols, "{line}");
            assert_eq!(
                line,
                strip_sgr(&card_top(
                    "매우 긴 제목 아주 긴 제목 긴 제목 매우 긴 제목",
                    Some("Escape back"),
                    cols,
                    true
                ))
            );
        }
        assert_eq!(state_badge("done").0, "[✓ exited]");
        assert_eq!(state_badge("completed").0, "[✓ completed]");
        assert_eq!(state_badge("accepted").0, "[✓ accepted]");
    }

    #[test]
    fn narrow_metadata_clipping_and_omission_counts() {
        let report = json!({"collection":{"ok":true}, "room":{"title":"OMS panel", "participants":[
            {"participant":"M", "role":"main", "provider":"codex", "state":"working", "title":"Main task", "task_id":"a",
                "refs":{"plan_id":"plan-1"}},
            {"participant":"C", "parent":"M", "state":"blocked", "title":"長い注意長い注意長い注意長い注意長い注意長い注意", "task_id":"b",
                "refs":{"plan_id":"plan-1"}},
            {"participant":"X", "state":"done", "title":"Other"}
        ]}, "plan":{"plan_id":"plan-1", "goal":"長い目標長い目標長い目標長い目標長い目標長い目標", "tasks":{"a":{"state":"queued"},"b":{"state":"queued"}}},
        "provider_status":{"codex":{"weekly_used":18,"five_hour_used":11,"five_hour_resets_at":oms_panel_core::timestamp()+3600},"claude":{"weekly_used":30}}});
        let read = BTreeMap::new();
        for columns in [19, 28, 40] {
            let frame = frame(
                &report,
                &navigation::Navigation::default(),
                columns,
                14,
                &read,
            )
            .join("\n");
            assert!(
                frame.contains(if columns < 28 { "L70%" } else { "Cld70%" })
                    && frame.contains("Main Codex")
                    && frame.contains("!1"),
                "{frame}"
            );
            assert!(!frame.contains("Plan 0/2"), "{columns}: {frame}");
            assert!(
                frame.contains("82%") && !frame.contains("89%"),
                "{columns}: {frame}"
            );
            assert!(
                frame
                    .lines()
                    .any(|s| s.starts_with("Goal:") && s.contains("0/2 verified")),
                "{frame}"
            );
            assert!(
                frame
                    .lines()
                    .filter(|s| s.contains("長い目標") || s.contains("目標長い"))
                    .count()
                    >= 2,
                "goal should wrap instead of clipping to one line: {frame}"
            );
            assert!(!frame.contains("OMS OMS"), "{frame}");
            assert!(
                frame.contains(if columns < 28 { "A—" } else { "AGY—" }),
                "{frame}"
            );
        }
        let mut stale = report.clone();
        stale["collection"]["ok"] = json!(false);
        let nav = navigation::Navigation {
            filter: "Main".into(),
            notice: Some(
                "A long notice that wraps onto a second line and contains more words".into(),
            ),
            ..Default::default()
        };
        for columns in [19, 28, 40] {
            let frame = frame(&stale, &nav, columns, 14, &read).join("\n");
            assert!(
                frame.contains("!1") && frame.contains("Filter /Main"),
                "{columns}: {frame}"
            );
            assert!(
                !frame.contains("Plan 0/2")
                    && frame.contains(if columns < 28 { "C82%" } else { "Cdx82%" }),
                "{columns}: {frame}"
            );
            assert!(frame.contains("STALE"), "{columns}: {frame}");
        }
        let mut full_quota = report.clone();
        full_quota["provider_status"]["codex"]["weekly_used"] = json!(0);
        full_quota["provider_status"]["codex"]["five_hour_used"] = json!(0);
        let quota_frame = frame(
            &full_quota,
            &navigation::Navigation::default(),
            19,
            14,
            &read,
        )
        .join("\n");
        assert!(quota_frame.contains("C100%"), "{quota_frame}");
        let mut nav = navigation::Navigation {
            view: View::Tree,
            ..Default::default()
        };
        nav.folded.insert("M".into());
        let folded_frame = frame(&report, &nav, 80, 20, &read).join("\n");
        assert!(
            folded_frame.contains("1 fold") && !folded_frame.contains("1 filt"),
            "{folded_frame}"
        );
        nav.filter = "Other".into();
        let frame = frame(&report, &nav, 80, 20, &read).join("\n");
        assert!(
            frame.contains("1 filt") && !frame.contains("1 fold"),
            "{frame}"
        );
    }

    #[test]
    fn page_capacity_skips_row_painting_and_matches_full_frame() {
        use navigation::Navigation;

        let mut participants = vec![json!({
            "participant":"M", "role":"main", "provider":"claude",
            "state":"working", "joined":true, "title":"Lead",
        })];
        for n in 0..40 {
            participants.push(json!({
                "participant":format!("P{n:02}"), "role":"worker", "parent":"M",
                "provider":(["codex", "claude", "antigravity"][n % 3]),
                "state":"working", "title":format!("Work {n}"),
            }));
        }
        let report = json!({
            "collection":{"ok":true}, "plan":{"goal":"Ship"},
            "provider_sessions":["codex", "claude", "antigravity"],
            "room":{"id":"room", "main":"M", "participants":participants},
        });
        let read = BTreeMap::new();
        let cases = [
            ("provider overview", 80, 24, Navigation::default()),
            (
                "detail page",
                80,
                24,
                Navigation {
                    view: View::Detail,
                    selected: 25,
                    offset: 20,
                    ..Default::default()
                },
            ),
            (
                "narrow filtered board",
                40,
                12,
                Navigation {
                    filter: "Work 1".into(),
                    editing: true,
                    notice: Some("Dispatched worker; this notice wraps across two rows".into()),
                    ..Default::default()
                },
            ),
            (
                "summary",
                80,
                24,
                Navigation {
                    view: View::Summary,
                    ..Default::default()
                },
            ),
            (
                "help overlay",
                80,
                24,
                Navigation {
                    help: true,
                    ..Default::default()
                },
            ),
            (
                "health overlay",
                80,
                24,
                Navigation {
                    health: true,
                    ..Default::default()
                },
            ),
            (
                "detail overlay",
                80,
                24,
                Navigation {
                    detail: true,
                    ..Default::default()
                },
            ),
        ];
        for (name, columns, height, nav) in cases {
            let mut expected = 1;
            ROW_RENDERS.with(|count| count.set(0));
            let frame = frame_with_capacity(
                &report,
                &nav,
                columns,
                height,
                &read,
                false,
                false,
                false,
                false,
                &mut expected,
            );
            assert!(ROW_RENDERS.with(std::cell::Cell::get) > 0, "{name}");
            if nav.help || nav.health || nav.detail {
                assert_eq!(expected, 1, "{name}");
            } else if nav.view == View::Detail {
                let selected = visible(&report, &nav);
                assert!(
                    expected > 1 && expected < nav.selected,
                    "{name}: {expected}"
                );
                assert!(
                    frame.worker_rows.values().any(|(participant, _)| {
                        participant == text(&selected[nav.selected], "participant")
                    }),
                    "{name}"
                );
            } else if name == "provider overview" {
                assert!(frame.provider_rows.iter().all(Option::is_some), "{name}");
                assert!(expected > 1 && expected < 40, "{name}: {expected}");
            }
            ROW_RENDERS.with(|count| count.set(0));
            assert_eq!(
                page_capacity(&report, &nav, columns, height, &read),
                expected,
                "{name}"
            );
            assert_eq!(ROW_RENDERS.with(std::cell::Cell::get), 0, "{name}");
        }
    }

    #[test]
    fn folded_tree_hides_descendants() {
        let calls = vec![
            json!({"participant":"M1"}),
            json!({"participant":"W1", "parent":"M1"}),
        ];
        let mut nav = navigation::Navigation::default();
        nav.folded.insert("M1".into());
        assert_eq!(tree(&calls, &nav, false, 10).len(), 1);
    }

    #[test]
    fn narrow_rows_and_viewport_keep_selected_call_visible() {
        let mut participants = vec![
            json!({"participant":"M1", "role":"main", "model":"model-x", "state":"working", "title":"Live task"}),
        ];
        for n in 0..20 {
            participants.push(json!({"participant":format!("W{n}"), "role":"worker", "state":"done", "title":format!("Task{n}")}));
        }
        let report = json!({"collection":{"ok":true}, "room":{"id":"room", "participants":participants}, "plan":{"goal":"Ship work"}});
        let mut nav = navigation::Navigation {
            view: View::Compact,
            ..Default::default()
        };
        let read = BTreeMap::new();
        for columns in [19, 28, 40, 80, 120] {
            let plain = frame_styled(&report, &nav, columns, 14, &read, false, true);
            let color = frame_styled(&report, &nav, columns, 14, &read, true, true);
            assert!(
                plain.iter().any(|row| row.contains("model-x")),
                "{columns}: {plain:?}"
            );
            assert!(
                plain.iter().any(|row| row.contains("hidden")),
                "{columns}: {plain:?}"
            );
            for (p, c) in plain.iter().zip(&color) {
                assert_eq!(*p, strip_sgr(c));
                assert!(width(p) <= columns);
            }
        }
        nav.selected = visible(&report, &nav)
            .iter()
            .position(|v| text(v, "title") == "Task19")
            .unwrap();
        let selected_frame = frame(&report, &nav, 28, 14, &read).join("\n");
        assert!(
            selected_frame.contains("▸ Worker  W19 · Task19"),
            "{selected_frame}"
        );
        nav.selected = 20;
        nav.offset = 20;
        let resized = frame(&report, &nav, 28, 40, &read).join("\n");
        assert!(resized.contains("▸ Worker  W9 · Task9"), "{resized}");
        assert!(resized.contains("model-x"), "{resized}");
        assert!(!resized.contains("hidden"), "{resized}");
        nav.filter = "Task19".into();
        nav.selected = 0;
        let frame = frame(&report, &nav, 28, 14, &read).join("\n");
        assert!(frame.contains("19 filt"), "{frame}");
        // A short Main card clips the widest facts, long tokens included,
        // before a labeled Work or Task leaves it; one fact row keeps
        // identity first and fits Work only beside it.
        let main = json!({"participant":"M1", "role":"main", "provider":"codex", "model":"model-x",
            "state":"working", "task_id":"t-main-42", "title":"Overall Main goal ".repeat(12),
            "native_session_tokens":{"total":1_234_567, "last_observed":true}});
        let mut unobserved = main.clone();
        unobserved["native_session_tokens"] = Value::Null;
        for (call, tokens) in [
            (&main, "Tokens 1234567"),
            (&unobserved, "Tokens unavailable"),
        ] {
            for columns in [40, 50, 60] {
                for room in [2, 3, 4] {
                    let card = render_main_detail_card(
                        call,
                        None,
                        columns,
                        columns - 4,
                        true,
                        false,
                        room,
                    );
                    assert!(card.len() <= room, "{columns}/{room}: {card:?}");
                    assert!(card[0].contains("Overall Main") && card[0].contains("Esc back"));
                    assert!(card.iter().all(|line| width(line) <= columns), "{card:?}");
                    let facts = card[1..].join("\n");
                    let pieces = facts
                        .split(['\n', '·'])
                        .map(|piece| piece.trim_matches(['│', ' ']))
                        .collect::<Vec<_>>();
                    let work = pieces
                        .iter()
                        .any(|piece| piece.starts_with("Work Ov") && piece.ends_with('…'));
                    let task = pieces.contains(&"Task t-main-42");
                    // One fact row clips every identity fact widest-first so
                    // a labeled Work still fits; Task needs a second row there.
                    let short = |fact: &'static str| if room > 2 { fact } else { &fact[..3] };
                    for fact in ["Codex", "model-x", "working", tokens] {
                        assert!(
                            facts.contains(short(fact)),
                            "{columns}/{room} {fact}: {card:?}"
                        );
                    }
                    assert!(work && task == (room > 2), "{columns}/{room}: {card:?}");
                }
            }
        }
        // Text a card leaves out is marked: a tall card keeps four Work rows,
        // the last ending in `…`, and one fact row without Task ends in `…`.
        let mut brief = unobserved.clone();
        brief["title"] = json!("Ship");
        brief["task_id"] = json!("t-main-42-".repeat(6));
        for columns in [40, 50, 60] {
            let card = render_main_detail_card(&main, None, columns, columns - 4, true, false, 64);
            let rows: Vec<_> = card
                .iter()
                .skip_while(|line| !line.contains("Work"))
                .skip(1)
                .take_while(|line| !line.contains("Task"))
                .map(|line| line.trim_matches(['│', ' ']))
                .collect();
            assert!(
                rows.len() == 4 && rows[3].ends_with('…'),
                "{columns}: {card:?}"
            );
            assert!(card.iter().all(|line| width(line) <= columns), "{card:?}");
        }
        for columns in [80, 120] {
            let card = render_main_detail_card(&brief, None, columns, columns - 4, true, false, 2);
            let row = card[1].trim_matches(['│', ' ']);
            assert!(
                row.ends_with("Work Ship…") && !row.contains("Task"),
                "{card:?}"
            );
            assert!(width(&card[1]) <= columns, "{card:?}");
        }
    }

    #[test]
    fn tree_display_order_matches_visible_order_and_selection() {
        let report = json!({"collection":{"ok":true}, "room":{"participants":[
            {"participant":"M", "role":"main", "state":"working"},
            {"participant":"other", "role":"worker", "state":"working"},
            {"participant":"child", "parent":"M", "role":"worker", "state":"done"}
        ]}});
        let nav = navigation::Navigation {
            view: View::Tree,
            selected: 1,
            ..Default::default()
        };
        let calls = visible(&report, &nav);
        assert_eq!(text(&calls[1], "participant"), "child");
        let rendered = frame(&report, &nav, 80, 20, &BTreeMap::new()).join("\n");
        assert!(
            rendered
                .lines()
                .any(|row| row.contains("▸└─") && row.contains("child")),
            "{rendered}"
        );

        let report = json!({"collection":{"ok":true}, "plan":{"goal":"Ship teams"}, "room":{"participants":[
            {"participant":"M", "provider":"codex", "role":"main", "state":"working", "title":"Own goal"},
            {"participant":"CW", "provider":"codex", "role":"worker", "parent":"M", "state":"working", "title":"Codex task"},
            {"participant":"L", "provider":"claude", "role":"lead", "parent":"M", "state":"working", "title":"Coordinate review"},
            {"participant":"LW", "provider":"claude", "role":"worker", "parent":"L", "state":"review", "title":"Claude task"},
            {"participant":"AW", "provider":"antigravity", "role":"worker", "parent":"M", "state":"working", "title":"AGY task"}
        ]}});
        let mut nav = navigation::Navigation::default();
        let visible_calls = visible(&report, &nav);
        assert_eq!(
            visible_calls
                .iter()
                .map(|call| text(call, "participant"))
                .collect::<Vec<_>>(),
            ["M", "CW", "L", "LW", "AW"]
        );
        let read = BTreeMap::new();
        for (columns, height) in [(200, 50), (120, 30), (80, 24), (56, 20), (40, 12), (19, 14)] {
            for (index, call) in visible_calls.iter().enumerate() {
                nav.selected = index;
                let plain = frame_styled(&report, &nav, columns, height, &read, false, true);
                let color = frame_styled(&report, &nav, columns, height, &read, true, true);
                assert_eq!(plain.len(), height, "{columns}x{height}: {plain:?}");
                assert!(plain.iter().all(|line| width(line) <= columns));
                assert!(
                    plain
                        .iter()
                        .zip(color)
                        .all(|(plain, color)| *plain == strip_sgr(&color))
                );
                let selected_label = if index == 0 && columns < 44 {
                    "Main"
                } else if index == 0 {
                    "Codex"
                } else {
                    text(call, "participant")
                };
                assert!(
                    plain
                        .iter()
                        .any(|line| line.contains('▸') && line.contains(selected_label)),
                    "{columns}x{height} {index}: {plain:?}"
                );
                if columns >= 56 {
                    let joined = plain.join("\n");
                    for required in [
                        "Codex · Activity",
                        "Claude · Activity",
                        "Antigravity · Activity",
                    ] {
                        assert!(
                            joined
                                .lines()
                                .any(|line| line.starts_with(&format!("├─ {required}"))
                                    || line.starts_with(&format!("╭─ {required}"))),
                            "{columns}x{height}: {joined}"
                        );
                    }
                    assert!(
                        joined
                            .lines()
                            .filter(|line| line.contains("Weekly left"))
                            .count()
                            == 1
                    );
                }
            }
        }
        let mut paged = report.clone();
        for n in 0..18 {
            paged["room"]["participants"].as_array_mut().unwrap().push(json!({
                "participant":format!("P{n:02}"), "provider":(["codex","claude","antigravity"][n%3]),
                "role":"worker", "parent":"M", "state":"working", "title":format!("Work {n}")
            }));
        }
        let mut page_nav = navigation::Navigation::default();
        for (columns, height) in [(80, 24), (56, 20)] {
            for (index, call) in visible(&paged, &page_nav).iter().enumerate() {
                page_nav.selected = index;
                let frame = frame_styled(&paged, &page_nav, columns, height, &read, false, true);
                let identity = if index == 0 {
                    " Codex ".to_owned()
                } else {
                    format!(" {} ", text(call, "participant"))
                };
                assert!(
                    frame
                        .iter()
                        .any(|line| line.contains('▸') && line.contains(&identity)),
                    "{columns}x{height} {index}: {frame:?}"
                );
                assert!(frame.len() == height && frame.iter().all(|line| width(line) <= columns));
                assert!(
                    frame.iter().any(|line| line.contains("hidden")),
                    "{frame:?}"
                );
                assert!(page_capacity(&paged, &page_nav, columns, height, &read) < 23);
            }
        }
        let mut historical = report.clone();
        historical["room"]["participants"][4]["state"] = json!("done");
        nav.selected = 0;
        assert_eq!(visible(&historical, &nav).len(), 4);
        let history = frame(&historical, &nav, 80, 24, &read).join("\n");
        assert!(
            history.contains("1 past (Tab all)") && !history.contains("AGY task"),
            "{history}"
        );
        let legacy = navigation::Navigation {
            view: View::Compact,
            ..Default::default()
        };
        assert_eq!(visible(&historical, &legacy).len(), 5);
        nav.filter = "AGY task".into();
        assert_eq!(visible(&historical, &nav).len(), 2);
        let searched = frame(&historical, &nav, 80, 24, &read).join("\n");
        assert!(
            searched.contains("AGY task") && searched.contains("[✓ exited]"),
            "{searched}"
        );
        let empty = frame(
            &json!({"collection":{"ok":true}}),
            &navigation::Navigation::default(),
            80,
            24,
            &read,
        )
        .join("\n");
        assert_eq!(empty.matches(" · Activity").count(), 3, "{empty}");
        assert!(empty.contains("no visible work") && !empty.contains("idle"));
        let mut connected = report.clone();
        connected["provider_sessions"] = json!(["codex", "antigravity"]);
        let board = board_frame(
            &connected,
            &navigation::Navigation::default(),
            80,
            24,
            &read,
            true,
            true,
        );
        let [Some(codex), None, Some(agy)] = board.provider_rows else {
            panic!("only connected provider headers should be clickable");
        };
        assert!(clean(&board.lines[codex], 80).contains("Codex · Lead chat [1]"));
        assert!(clean(&board.lines[agy], 80).contains("Antigravity · Lead chat [3]"));
        nav.filter = "Claude task".into();
        nav.selected = 1;
        assert_eq!(visible(&report, &nav).len(), 2);
        let filtered = frame(&report, &nav, 80, 24, &read).join("\n");
        assert!(
            filtered
                .lines()
                .any(|line| line.contains('▸') && line.contains("LW")),
            "{filtered}"
        );
        assert!(filtered.contains("3 filt"), "{filtered}");
        assert!(filtered.contains("Claude · Activity"), "{filtered}");
    }

    #[test]
    fn overview_groups_only_work_bound_to_the_live_room_owner_attempt() {
        let mut report = json!({
            "collection":{"ok":true},
            "provider_sessions":["antigravity"],
            "room":{
                "id":"room-one", "owner":"M",
                "participants":[
                    {"participant":"M","role":"main","attempt":"main-live","provider":"codex","joined":true},
                    {"participant":"lead","role":"lead","parent":"M","provider":"codex","joined":true,"state":"working"},
                    {"participant":"old","role":"main","provider":"claude","joined":false,"state":"done"},
                    {"participant":"direct","role":"worker","parent":"M","provider":"claude","joined":true,"state":"working","title":"Direct changes"},
                    {"participant":"foreign","role":"reviewer","parent":"M","provider":"codex","joined":true,"state":"working"},
                    {"participant":"old-work","role":"advisor","parent":"M","provider":"codex","joined":true,"state":"working"},
                    {"participant":"indirect","role":"worker","parent":"lead","provider":"codex","joined":true,"state":"working","title":"Indirect work"},
                    {"participant":"unbound","role":"worker","parent":"M","provider":"codex","joined":true,"state":"working"}
                ]
            },
            "room_attempts":{
                "main-live":{"attempt_id":"main-live","tool":"panel-main","provider":"codex","state":"working","terminal":false,
                    "refs":{"panel_room_id":"room-one","panel_room_participant":"M"}},
                "direct-attempt":{"attempt_id":"direct-attempt","parent_attempt_id":"main-live","provider":"claude","state":"working","terminal":false,
                    "refs":{"panel_room_id":"room-one","panel_room_participant":"direct"}},
                "foreign-attempt":{"attempt_id":"foreign-attempt","parent_attempt_id":"main-live","provider":"codex","state":"working","terminal":false,
                    "refs":{"panel_room_id":"room-one","panel_room":"room-two","panel_room_participant":"foreign"}},
                "old-attempt":{"attempt_id":"old-attempt","parent_attempt_id":"main-old","provider":"codex","state":"working","terminal":false,
                    "refs":{"panel_room_id":"room-one","panel_room_participant":"old-work"}},
                "indirect-attempt":{"attempt_id":"indirect-attempt","parent_attempt_id":"main-live","provider":"codex","state":"working","terminal":false,
                    "refs":{"panel_room_id":"room-one","panel_room_participant":"indirect"}}
            },
            "native_children":[{"participant":"native","role":"subagent","parent":"M","provider":"claude","native_child":true,"state":"working"}]
        });
        let mut nav = navigation::Navigation::default();
        let read = BTreeMap::new();
        let items = visible(&report, &nav);
        assert_eq!(text(&items[0], "participant"), "M");
        assert_eq!(text(&items[1], "participant"), "direct");
        let mut lifecycle = report.clone();
        let now = 1_800_000_600u64;
        lifecycle["generated_at"] = now.into();
        lifecycle["room_attempts"]["direct-attempt"]["created_at"] = (now - 600).into();
        lifecycle["room_attempts"]["direct-attempt"]["updated_at"] = (now - 300).into();
        lifecycle["room_attempts"]["direct-attempt"]["last_heartbeat_at"] = (now - 300).into();
        let stale_ids = |report: &Value| {
            attention::items(report, &read)
                .into_iter()
                .filter(|item| item.cls == "stale")
                .map(|item| item.row_id.1)
                .collect::<Vec<_>>()
        };
        assert_eq!(stale_ids(&lifecycle), ["direct"]);
        let stale = attention::items(&lifecycle, &read);
        assert_eq!(stale[0].what, "no recent lifecycle events: Direct changes");
        let marked = BTreeMap::from([(attention::row_key(&stale[0].row_id), "stale".into())]);
        assert!(!attention::items(&lifecycle, &marked)[0].unread);
        let attention_nav = navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        assert_eq!(
            visible(&lifecycle, &attention_nav)
                .iter()
                .map(|call| text(call, "participant"))
                .collect::<Vec<_>>(),
            ["M", "direct"]
        );
        for columns in [32, 56, 160] {
            let board = board_frame(&lifecycle, &attention_nav, columns, 24, &read, false, true);
            assert!(board.lines.iter().all(|line| width(line) <= columns));
            let (&hit, _) = board
                .worker_rows
                .iter()
                .find(|(_, (id, _))| id == "direct")
                .unwrap();
            assert!(
                board.lines[hit].contains("[no events]"),
                "{}",
                board.lines[hit]
            );
            let detail_nav = navigation::Navigation {
                selected: 1,
                detail: true,
                ..attention_nav.clone()
            };
            let detail = frame(&lifecycle, &detail_nav, columns, 24, &read);
            assert!(detail.iter().all(|line| width(line) <= columns));
            let detail = detail.join("\n");
            assert!(detail.contains("No recent lifecycle events"), "{detail}");
            assert!(detail.contains("Last recorded state: working"), "{detail}");
        }
        assert!(
            calls(&lifecycle)
                .iter()
                .any(|call| call["participant"] == "direct" && call["state"] == "working")
        );
        for (path, value) in [
            ("generated_at", json!(now - 1)),
            ("generated_at", Value::Null),
            ("collection.ok", json!(false)),
            ("collection.pending", json!(true)),
            ("capacity_stale", json!(true)),
            ("room.error", json!("unavailable")),
            ("room.closed", json!(true)),
            ("room_attempts.main-live.terminal", json!(true)),
            (
                "room_attempts.direct-attempt.parent_attempt_id",
                json!("main-old"),
            ),
            ("room_attempts.direct-attempt.terminal", json!(true)),
            ("room_attempts.direct-attempt.created_at", json!(now + 1)),
            ("room_attempts.direct-attempt.updated_at", json!(now + 1)),
            ("room_attempts.direct-attempt.updated_at", json!(now - 299)),
            ("room_attempts.direct-attempt.updated_at", Value::Null),
            ("room_attempts.direct-attempt.updated_at", json!("invalid")),
            (
                "room_attempts.direct-attempt.last_heartbeat_at",
                Value::Null,
            ),
            (
                "room_attempts.direct-attempt.last_heartbeat_at",
                json!(now + 1),
            ),
            (
                "room_attempts.direct-attempt.tool",
                json!("panel-native-assignment"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_native_scheduling",
                json!("unmanaged"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_room_id",
                json!("room-other"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_heartbeat_ttl",
                json!("600"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_heartbeat_ttl",
                json!("0"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_heartbeat_ttl",
                json!("bad"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_heartbeat_ttl",
                json!("30"),
            ),
            (
                "room_attempts.direct-attempt.refs.panel_heartbeat_ttl",
                json!(300),
            ),
        ] {
            let mut changed = lifecycle.clone();
            let mut target = &mut changed;
            for key in path.split('.') {
                target = &mut target[key];
            }
            *target = value.clone();
            assert!(stale_ids(&changed).is_empty(), "{path}={value}");
        }
        for state in [
            "queued",
            "review",
            "done",
            "waiting_input",
            "waiting_approval",
            "blocked",
        ] {
            let mut changed = lifecycle.clone();
            changed["room_attempts"]["direct-attempt"]["state"] = state.into();
            assert!(stale_ids(&changed).is_empty(), "{state}");
        }
        // Persisted snapshots age only at their recorded observation time;
        // native children cannot supply a stale marker as lifecycle evidence.
        let mut held = lifecycle.clone();
        held["generated_at"] = (now - 300).into();
        held["native_children"][0]["lifecycle_stale"] = true.into();
        assert!(stale_ids(&held).is_empty());
        nav.selected = 1;
        for (columns, height) in [(160, 34), (56, 20), (32, 12)] {
            let board = board_frame(&report, &nav, columns, height, &read, false, true);
            let (&hit, value) = board
                .worker_rows
                .iter()
                .find(|(_, (participant, _))| participant == "direct")
                .unwrap();
            assert_eq!(value, &("direct".to_owned(), "direct-attempt".to_owned()));
            assert!(board.lines[hit].contains('▸') && !board.lines[hit].contains('←'));
            assert!(board.lines.iter().all(|line| width(line) <= columns));
            assert_eq!(board.lines.len(), height);
            if columns >= 56 {
                assert!(board.lines[hit - 1].contains("Direct work"));
                assert!(!board.lines[hit - 1].contains("Overall Main"));
                let agy = board.provider_rows[2].unwrap();
                assert!(agy > hit && board.lines[agy].contains("Lead chat [3]"));
                assert!(!board.worker_rows.contains_key(&agy));
            } else {
                assert_eq!(board.provider_rows, [None; 3]);
            }
            if columns >= 160 {
                assert!(board.lines[hit].contains("Claude"));
                assert!(board.lines[hit + 2].starts_with("╭─ Codex · Activity"));
                for (row, (participant, _)) in &board.worker_rows {
                    assert!(["M", "direct"].contains(&participant.as_str()) || *row > hit + 2);
                }
            }
        }
        nav.selected = items.len() - 1;
        nav.offset = nav.selected;
        let board = board_frame(&report, &nav, 80, 15, &read, false, true);
        assert!(!board.lines.iter().any(|line| line.contains("Direct work")));
        assert!(
            board.worker_rows.values().any(|(participant, _)| {
                participant == text(items.last().unwrap(), "participant")
            })
        );
        nav.selected = 1;
        nav.offset = 0;
        nav.filter = "indirect".into();
        let board = board_frame(&report, &nav, 80, 24, &read, false, true);
        assert!(!board.lines.iter().any(|line| line.contains("Direct work")));
        assert!(
            board
                .lines
                .iter()
                .any(|line| line.contains("▸") && line.contains("indirect"))
        );
        nav.filter.clear();
        nav.view = View::Tree;
        nav.folded.insert("M".into());
        assert!(
            !visible(&report, &nav)
                .iter()
                .any(|call| call["participant"] == "direct")
        );

        nav.view = View::Auto;
        nav.folded.clear();
        let current = report["room_attempts"]["direct-attempt"].clone();
        report["room_attempts"]["direct-attempt"]["state"] = json!("done");
        assert!(
            !visible(&report, &nav)
                .iter()
                .any(|call| call["participant"] == "direct")
        );
        nav.filter = "direct changes".into();
        let board = board_frame(&report, &nav, 100, 24, &read, false, true);
        assert!(!board.lines.iter().any(|line| line.contains("Direct work")));
        assert!(
            board
                .lines
                .iter()
                .any(|line| line.contains("direct") && line.contains("exited"))
        );
        nav.filter.clear();
        report["room_attempts"]["direct-attempt"] = current;
        report["room_attempts"]["direct-attempt"]["terminal"] = json!(true);
        let board = board_frame(&report, &nav, 160, 34, &read, false, true);
        assert!(!board.lines.iter().any(|line| line.contains("Direct work")));
        report["room_attempts"]["direct-attempt"]["terminal"] = json!(false);

        for change in [
            json!({"participant":"main-live","role":"worker","state":"working"}),
            json!({"participant":"M","role":"main","state":"working","joined":true}),
        ] {
            let mut ambiguous = report.clone();
            ambiguous["room"]["participants"]
                .as_array_mut()
                .unwrap()
                .push(change);
            let board = board_frame(&ambiguous, &nav, 160, 34, &read, false, true);
            assert!(!board.lines.iter().any(|line| line.contains("Direct work")));
        }
        report["room"]["swaps"] = json!([{"participant":"M","attempt":"main-live"}]);
        let board = board_frame(&report, &nav, 160, 34, &read, false, true);
        assert!(!board.lines.iter().any(|line| line.contains("Direct work")));
    }

    #[test]
    fn color_policy_respects_no_color_env_and_tty() {
        assert!(!resolve_color(false, None, None, None));
        assert!(resolve_color(true, None, None, Some("xterm-256color")));
        assert!(!resolve_color(true, Some("1"), None, None));
        assert!(!resolve_color(true, Some("1"), Some("1"), None));
        assert!(resolve_color(false, None, Some("always"), None));
        assert!(!resolve_color(true, None, Some("0"), None));
        assert!(!resolve_color(true, None, None, Some("dumb")));
    }

    #[test]
    fn frame_styled_handles_empty_room_hangul_alignment_and_escape_stripping() {
        let report = json!({
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {"id": "room-demo", "title": "", "participants": []},
            "attempts": {
                "active": [],
                "active_recent": [],
                "recent": [
                    {
                        "attempt_id": "att_main",
                        "role": "main",
                        "provider": "claude",
                        "model": "claude-opus-5-5",
                        "state": "working",
                        "task_id": "t1",
                        "created_at": "2026-11-17T00:00:00Z",
                        "refs": {
                            "panel_role": "main",
                            "plan_id": "plan-1",
                            "panel_alias": "main-owner",
                            "panel_label": "\u{1b}[2J한글 제목 테스트\u{1b}]0;evil\u{7}"
                        }
                    },
                    {
                        "attempt_id": "att_w1",
                        "parent_attempt_id": "att_main",
                        "role": "builder",
                        "provider": "codex",
                        "model": "gpt-6.1-sol",
                        "task_id": "task-1",
                        "state": "review",
                        "created_at": "2026-11-17T00:01:00Z",
                        "refs": {
                            "panel_role": "builder",
                            "plan_id": "plan-1",
                            "panel_alias": "W1",
                            "panel_label": "Build UI"
                        }
                    },
                    {
                        "attempt_id": "att_w2",
                        "parent_attempt_id": "att_main",
                        "role": "reviewer",
                        "provider": "claude",
                        "model": "claude-sonnet-5-5",
                        "state": "done",
                        "created_at": "2026-11-17T00:02:00Z",
                        "refs": {
                            "panel_role": "reviewer",
                            "panel_alias": "R1",
                            "panel_label": "Verify diff"
                        }
                    }
                ]
            },
            "provider_status": {
                "claude": {"weekly_used": 28, "five_hour_used": 23, "five_hour_resets_at": oms_panel_core::timestamp()+3600}
            },
            "plan": {
                "plan_id": "plan-1",
                "tasks": {
                    "t1": {"state": "done"},
                    "task-1": {"state": "running"}
                }
            }
        });
        let nav = navigation::Navigation {
            view: View::Compact,
            ..Default::default()
        };
        let read = BTreeMap::new();
        let columns = 84;
        let height = 24;

        let plain = frame_styled(&report, &nav, columns, height, &read, false, true);
        assert_eq!(plain.len(), height);
        assert!(plain[0].starts_with("OMS · workspace"), "{}", plain[0]);
        assert!(!plain[0].contains("·  ·"), "{}", plain[0]);
        assert!(
            plain
                .iter()
                .any(|l| l.contains("Weekly left · Codex — │ Claude 72% │ AGY —")),
            "{plain:?}"
        );
        assert!(plain.iter().all(|l| !l.contains("Plan 1/2 done")));
        assert!(plain.iter().any(|l| l.contains("[⚑ review]")));
        assert!(plain.iter().any(|l| l.contains("[✓ exited]")));
        // Untrusted CSI/OSC from title must be stripped cleanly and Hangul cells must keep border aligned.
        let joined = plain.join("\n");
        assert!(!joined.contains('\u{1b}'));
        assert!(joined.contains("한글 제목 테스트"));
        for line in &plain {
            assert!(width(line) <= columns, "overflow {}: {}", width(line), line);
            if line.starts_with('╭') || line.starts_with('│') || line.starts_with('╰') {
                assert_eq!(width(line), columns, "card alignment mismatch: {line}");
            }
        }

        // Colored TTY frame keeps identical visual cell geometry after stripping renderer SGR.
        let colored = frame_styled(&report, &nav, columns, height, &read, true, true);
        assert_eq!(colored.len(), height);
        assert!(colored.iter().any(|l| l.contains("\x1b[1;38;5;215m")));
        for (p, c) in plain.iter().zip(colored.iter()) {
            assert_eq!(strip_sgr(c), *p);
        }

        // Tree view renders sibling connectors ├─ and last-child └─ properly.
        let mut tree_nav = nav.clone();
        tree_nav.view = View::Tree;
        let tree_frame = frame_styled(&report, &tree_nav, columns, height, &read, false, false);
        let tree_joined = tree_frame.join("\n");
        assert!(tree_joined.contains("├─ ● Builder    W1"), "{tree_joined}");
        assert!(tree_joined.contains("└─ ◆ Reviewer   R1"), "{tree_joined}");

        // Overlays render structured cards instead of raw JSON objects.
        for mut overlay in [
            {
                let mut n = nav.clone();
                n.detail = true;
                n
            },
            {
                let mut n = nav.clone();
                n.health = true;
                n
            },
            {
                let mut n = nav.clone();
                n.help = true;
                n
            },
        ] {
            let rendered = frame_styled(&report, &overlay, columns, height, &read, false, false);
            assert!(
                rendered.iter().all(|l| !l.trim_start().starts_with('{')),
                "{rendered:?}"
            );
            overlay.notice = None;
        }
    }

    #[test]
    fn room_participant_joins_attempt_isolates_foreign_rooms_and_avoids_dangling_attention_colons()
    {
        let report = json!({
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "title": "Shared work",
                "participants": [
                    {
                        "participant": "att_8aa76df1a6c6415c8ce8d8ef2f639c67",
                        "role": "main",
                        "provider": "claude",
                        "joined_at": "2026-11-17T00:00:00Z",
                        "label_override": "Main Tower"
                    },
                    {
                        "participant": "call-w1",
                        "role": "worker",
                        "alias": "W1",
                        "provider": "codex",
                        "model": "gpt-6.1-sol",
                        "state": "queued",
                        "label": "Worker label",
                        "parent": "att_8aa76df1a6c6415c8ce8d8ef2f639c67",
                        "joined_at": "2026-11-17T00:01:00Z"
                    }
                ]
            },
            "room_attempts": {
                "att_8aa76df1a6c6415c8ce8d8ef2f639c67": {
                    "attempt_id": "att_8aa76df1a6c6415c8ce8d8ef2f639c67",
                    "tool": "panel-main",
                    "provider": "claude",
                    "state": "working",
                    "created_at": "2026-11-17T00:00:00Z",
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att_w1": {
                    "attempt_id": "att_w1",
                    "tool": "peer-delegate",
                    "provider": "codex",
                    "state": "done",
                    "created_at": "2026-11-17T00:01:05Z",
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-w1",
                        "panel_role": "worker"
                    }
                },
                "call-w1": {
                    "attempt_id": "att_w1",
                    "tool": "peer-delegate",
                    "provider": "codex",
                    "state": "done",
                    "created_at": "2026-11-17T00:01:05Z",
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-w1",
                        "panel_role": "worker"
                    }
                }
            },
            "attempts": {
                "active": [
                    {
                        "attempt_id": "att_foreign",
                        "tool": "peer-delegate",
                        "state": "blocked",
                        "refs": {"panel_room_id": "room-other", "panel_role": "worker"}
                    },
                    {
                        "attempt_id": "att_unroomed_admit",
                        "tool": "patch-admit",
                        "state": "blocked",
                        "refs": {}
                    },
                    {
                        "attempt_id": "att_local_blocked",
                        "tool": "peer-delegate",
                        "state": "blocked",
                        "created_at": "2026-11-17T00:02:00Z",
                        "refs": {"panel_room_id": "room-demo"}
                    }
                ],
                "active_recent": [],
                "recent": []
            }
        });

        let rows = calls(&report);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.iter().all(|r| r["attempt"] != "att_foreign"));
        assert!(rows.iter().all(|r| r["attempt"] != "att_unroomed_admit"));
        let w1 = rows.iter().find(|r| r["participant"] == "call-w1").unwrap();
        assert_eq!(w1["state"], json!("done"));
        assert_eq!(w1["title"], json!("Worker label"));

        let nav = navigation::Navigation {
            view: View::Compact,
            ..Default::default()
        };
        let read = BTreeMap::new();
        let lines = frame_styled(&report, &nav, 90, 24, &read, false, false);
        let joined = lines.join("\n");
        assert!(
            joined.contains("Overall Main") && joined.contains("▸ Claude"),
            "{joined}"
        );
        assert!(joined.contains("Overall Main"), "{joined}");
        assert!(joined.contains("Worker     W1 · Worker label"), "{joined}");
        assert!(joined.contains("[✓ exited]"), "{joined}");
        assert!(!joined.contains(": blocked:"), "{joined}");
        assert!(!joined.contains("blocked:\n"), "{joined}");
        assert!(joined.contains("blocked"), "{joined}");
    }

    fn top_bar_report() -> Value {
        json!({"collection":{"ok":true}, "room":{"id":"room-x", "title":"Shared work", "task_id":"t0", "participants":[
            {"participant":"M", "role":"main", "provider":"claude", "model":"opus", "state":"working", "title":"Panel polish"},
            {"participant":"W1", "role":"worker", "provider":"codex", "state":"working", "title":"Top bar", "task_id":"t1",
                "refs":{"plan_id":"plan-1"}},
            {"participant":"W2", "role":"worker", "provider":"codex", "state":"done", "title":"Old", "task_id":"t2",
                "refs":{"plan_id":"plan-1", "panel_room_id":"room-x"}}
        ]}, "attempts":{"active":[
            {"attempt_id":"att_foreign", "state":"working", "task_id":"t9", "refs":{"panel_room_id":"room-other", "plan_id":"plan-1"}}
        ]}, "plan":{"plan_id":"plan-1", "goal":"Ship top bar", "tasks":{
            "t0":{"state":"done"}, "t1":{"state":"running"}, "t2":{"state":"done"},
            "t3":{"state":"queued"}, "t9":{"state":"running"}
        }}})
    }

    #[test]
    fn local_claude_weekly_is_board_only_and_hides_retained_numbers() {
        let now = 1_000_000.0;
        let mut report = json!({
            "collection":{"ok":true},
            "provider_status":{"claude":{"source":"unavailable"}},
            "claude_local_weekly":{"weekly_used":14.0,"callback_observed_at":now-25.0, "resets_at":now + 3600.0}
        });
        let generic = crate::usage::snapshot(&report, now);
        assert!(weekly_window(&generic["providers"]["claude"]).is_none());
        let line = |r: &Value, at| provider_line(r, &generic, at, 80, false, false).0;
        assert!(line(&report, now).contains("Claude 86%"));
        assert!(weekly_usage_detail(&report, now).contains(
            "Claude · 86% left\n  Source Claude local statusline · Callback age 25s · Reset in 1h00m"
        ));
        assert!(line(&report, now + 576.0).contains("Claude stale"));
        assert!(line(&report, now + 3600.0).contains("Claude reset"));
        for (collection, held) in [
            (json!({"ok":true,"pending":true}), false),
            (json!({"ok":false}), false),
            (json!({"ok":true}), true),
        ] {
            report["collection"] = collection;
            report["capacity_stale"] = json!(held);
            assert!(line(&report, now).contains("Claude —"));
            assert!(!weekly_usage_detail(&report, now).contains("Claude local statusline"));
        }
    }

    #[test]
    fn agy_footer_weekly_is_board_only_bounded_and_yields_to_fresh_usage() {
        let now = 1_000_000.0;
        let mut report = json!({
            "collection":{"ok":true},
            "provider_status":{
                "codex":{"source":"session log","weekly_used":25,"observed_at":now - 30.0},
                "claude":{"source":"HUD cache","weekly_used":14,"observed_at":now - 30.0},
                "antigravity":{"source":"unavailable"}
            },
            "agy_footer_weekly":{"remaining":"99%"}
        });
        let generic = crate::usage::snapshot(&report, now);
        assert!(weekly_window(&generic["providers"]["antigravity"]).is_none());
        let line = |r: &Value, cols, at| {
            let usage = crate::usage::snapshot(r, at);
            provider_line(r, &usage, at, cols, false, false)
                .0
                .trim_end()
                .to_owned()
        };

        // Normal, low (<1%, 0%), and bounded >99% footer values across header widths and detail.
        assert_eq!(
            line(&report, 80, now),
            "Weekly left · Codex 75% │ Claude 86% │ AGY footer 99%"
        );
        assert_eq!(line(&report, 50, now), "Wk left Cdx75% Cld86% AGYf99%");
        assert_eq!(line(&report, 24, now), "W C75% L86% Af99%");
        assert!(
            weekly_usage_detail(&report, now).contains(
                "AGY · 99% left\n  Source AGY native footer · Age unknown · Reset unknown"
            )
        );

        for (value, header, detail_left) in [
            ("<1%", "AGY footer <1%", "<1% left"),
            (">99%", "AGY footer >99%", ">99% left"),
            ("0%", "AGY footer 0% exhausted", "0% left"),
            ("100%", "AGY footer 100%", "100% left"),
        ] {
            report["agy_footer_weekly"] = json!({"remaining": value});
            assert!(line(&report, 80, now).contains(header), "{value}");
            assert!(
                weekly_usage_detail(&report, now).contains(&format!(
                    "AGY · {detail_left}\n  Source AGY native footer · Age unknown · Reset unknown"
                )),
                "{value}"
            );
        }

        // Unknown / malformed footer values stay unknown.
        for bad in [
            json!({"remaining":"101%"}),
            json!({"remaining":"09%"}),
            json!({"remaining":"<2%"}),
            json!(null),
        ] {
            report["agy_footer_weekly"] = bad;
            assert!(line(&report, 80, now).contains("AGY —"));
            assert!(
                weekly_usage_detail(&report, now).contains(
                    "AGY · missing weekly reading\n  Source unreported · Reset unreported"
                )
            );
        }

        // Fresh verified /usage takes priority; stale /usage falls back to current footer.
        report["agy_footer_weekly"] = json!({"remaining": "99%"});
        report["provider_status"]["antigravity"] = json!({
            "weekly_used": 40,
            "weekly_native_observed_at": now - 30.0
        });
        assert!(line(&report, 80, now).contains("AGY Gemini 60%"));
        assert!(
            weekly_usage_detail(&report, now)
                .contains("AGY · 60% left\n  Source AGY Gemini quota · Reset unreported")
        );
        let stale_at = now + 601.0;
        assert!(line(&report, 80, stale_at).contains("AGY footer 99%"));
        assert!(
            weekly_usage_detail(&report, stale_at).contains(
                "AGY · 99% left\n  Source AGY native footer · Age unknown · Reset unknown"
            )
        );

        // Holds, pending refresh, and failed collection clear the footer display.
        for (collection, held) in [
            (json!({"ok":true,"pending":true}), false),
            (json!({"ok":false}), false),
            (json!({"ok":true}), true),
        ] {
            report["collection"] = collection;
            report["capacity_stale"] = json!(held);
            assert!(!line(&report, 80, stale_at).contains("AGY footer"));
            assert!(!weekly_usage_detail(&report, stale_at).contains("AGY native footer"));
        }
    }

    #[test]
    fn available_agy_chrome_is_a_display_only_roster() {
        let mut report = top_bar_report();
        report["provider_sessions"] = json!(["claude", "antigravity"]);
        report["provider_accounts"]["claude"] =
            json!({"account":"review-seat","verification":"verified"});
        report["provider_activity"]["antigravity"] = json!({
            "state":"working", "detail":"command",
            "children":[{"role":"Researcher","title":"Inspect custody","state":"reading","elapsed":"53m17s"}]
        });
        let paint_nav = |r: &Value, nav: &navigation::Navigation, columns| {
            board_frame(r, nav, columns, 27, &BTreeMap::new(), false, true)
        };
        let paint = |r: &Value| paint_nav(r, &Default::default(), 140);
        let frame = paint(&report);
        let roster_row = |frame: &BoardFrame| {
            frame
                .lines
                .iter()
                .position(|line| line.contains("Researcher Inspect custody"))
        };
        let row = roster_row(&frame).unwrap();
        assert!(frame.lines[row].contains("reading") && frame.lines[row].contains("53m17s"));
        assert!(!frame.worker_rows.contains_key(&row));
        let heading = frame.provider_rows[2].unwrap();
        assert!(heading < row);
        assert!(frame.lines[heading].contains("command running"));
        assert!(frame.lines[heading].contains("1 agent"));
        let claude = frame.provider_rows[1].unwrap();
        assert!(frame.lines[claude].contains("@review-seat"));
        for account in [
            json!({"account":"review-seat","verification":"unverified"}),
            json!({"account":"invalid/path","verification":"verified"}),
        ] {
            let mut unproved = report.clone();
            unproved["provider_accounts"]["claude"] = account;
            assert!(!paint(&unproved).lines[claude].contains(" · @"));
        }
        let mut activity_report = report.clone();
        activity_report["provider_activity"]["antigravity"]["children"][0]["activity"] =
            json!("Searching");
        activity_report["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "participant":"A1", "role":"advisor", "provider":"antigravity",
                "state":"working", "title":"Weigh options"
            }));
        for columns in [60, 80, 140] {
            let frame = board_frame(
                &activity_report,
                &Default::default(),
                columns,
                27,
                &BTreeMap::new(),
                false,
                true,
            );
            let researcher = frame
                .lines
                .iter()
                .find(|line| line.contains("Inspect custody"))
                .unwrap_or_else(|| panic!("{columns}: {:?}", frame.lines));
            let advisor = frame
                .lines
                .iter()
                .find(|line| line.contains("Weigh options"))
                .unwrap();
            assert!(researcher.starts_with("│   ") && advisor.starts_with("│   "));
            assert!(
                researcher.contains("[⟳ searching]") && researcher.contains("53m17s"),
                "{researcher}"
            );
            assert_eq!(researcher.contains("— tok"), columns >= 80, "{researcher}");
            assert_eq!(
                researcher.matches('—').count(),
                usize::from(columns >= 80),
                "{researcher}"
            );
            assert!(advisor.contains("A1 · Weigh options"), "{advisor}");
            assert_eq!(
                width(&researcher[..researcher.find("Inspect custody").unwrap()]),
                width(&advisor[..advisor.find("A1 · Weigh options").unwrap()]),
            );
            assert!(frame.lines.iter().all(|line| width(line) <= columns));
        }
        for (state, activity) in [
            ("done", "Searching"),
            ("working", "unrecognized operation"),
            ("working", ""),
        ] {
            assert!(active_verb(&json!({"state":state, "activity":activity})).is_none());
        }
        // A same-scope pending report keeps the carried roster as a preview;
        // live detail still waits for a current read.
        report["collection"]["pending"] = true.into();
        let pending = paint(&report);
        assert!(
            !pending
                .lines
                .iter()
                .any(|line| line.contains("command running"))
        );
        let row = roster_row(&pending).expect("pending keeps the roster");
        assert!(!pending.worker_rows.contains_key(&row));
        assert!(pending.lines[pending.provider_rows[2].unwrap()].contains("1 agent"));
        let mut stale = report.clone();
        stale["capacity_stale"] = true.into();
        let filtered = navigation::Navigation {
            filter: "custody".into(),
            ..Default::default()
        };
        let attention = navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        for (r, nav) in [
            (&stale, &Default::default()),
            (&report, &filtered),
            (&report, &attention),
        ] {
            assert!(roster_row(&paint_nav(r, nav, 140)).is_none());
        }

        // Roster previews and recorded work use the same flat role column.
        let mut mixed = report.clone();
        mixed["collection"] = json!({"ok":true});
        mixed["provider_activity"]["antigravity"]["children"] = json!([
            {"role":"Researcher","title":"Inspect custody","state":"reading","elapsed":"53m17s"},
            {"role":"Worker","title":"Draft notes","state":"working","elapsed":"2m"}
        ]);
        let participants = mixed["room"]["participants"].as_array_mut().unwrap();
        participants.extend([
            json!({"participant":"A1", "role":"worker", "provider":"antigravity", "state":"working", "title":"Port fixtures", "parent":"W1"}),
            json!({"participant":"A2", "role":"advisor", "provider":"antigravity", "state":"working", "title":"Weigh options"}),
            json!({"participant":"A3", "role":"researcher", "provider":"antigravity", "state":"working", "title":"Survey docs", "parent":"M"}),
        ]);
        for columns in [140, 80, 60] {
            let frame = paint_nav(&mixed, &Default::default(), columns);
            assert!(frame.lines.iter().all(|line| width(line) <= columns));
            let heading = frame.provider_rows[2].unwrap();
            let agy: Vec<_> = (heading + 1..frame.lines.len())
                .take_while(|row| frame.lines[*row].starts_with('│'))
                .collect();
            for (at, row) in agy.iter().enumerate() {
                let line = &frame.lines[*row];
                assert!(
                    line.starts_with("│   ") && !line.contains(['├', '└', '←']),
                    "{columns}: {line}"
                );
                let member = [None, None, Some("A1"), Some("A2"), Some("A3")][at];
                assert_eq!(
                    frame.worker_rows.get(row).map(|(id, _)| id.as_str()),
                    member,
                    "{columns}: {line}"
                );
            }
            assert_eq!(agy.len(), 5, "{columns}: {:?}", frame.lines);
            // Every AGY role and name starts in one display column.
            let starts: Vec<_> = agy
                .iter()
                .map(|row| {
                    let line = &frame.lines[*row];
                    let glyph = line.find(['●', '◇', '◆']).unwrap();
                    let role = glyph + line[glyph..].chars().next().unwrap().len_utf8() + 1;
                    let end = role + line[role..].find(' ').unwrap();
                    let name = end + line[end..].find(|c| c != ' ').unwrap();
                    (&line[role..end], width(&line[..role]), width(&line[..name]))
                })
                .collect();
            let roles: Vec<_> = starts.iter().map(|start| start.0).collect();
            assert_eq!(
                roles,
                ["Researcher", "Worker", "Worker", "Advisor", "Researcher"]
            );
            assert!(
                starts
                    .iter()
                    .all(|start| start.1 == starts[0].1 && start.2 == starts[0].2),
                "{columns}: AGY role/name columns misaligned: {starts:?}"
            );
            let codex = frame
                .lines
                .iter()
                .find(|line| line.contains("W1 · Top bar"))
                .unwrap();
            assert!(codex.starts_with("│   ● Worker"), "{codex}");
        }
    }

    #[test]
    fn child_rows_share_model_and_usage_slots_with_scoped_graph_edges() {
        let child = |provider, id, role, title, model: Option<&str>| {
            json!({
                "native_child":true, "provider":provider, "participant":id, "role":role,
                "state":"working", "parent":format!("{provider}-lead"), "title":title,
                "model":"legacy-guess", "observed_model":model, "started_at":10_000,
                "native_response_usage":{"response":{"total_tokens":12_500}}
            })
        };
        let mut report = top_bar_report();
        report["generated_at"] = json!(10_180);
        report["provider_sessions"] = json!(["codex", "claude", "antigravity"]);
        report["provider_models"] =
            json!({"codex":"ParentSol", "claude":"ParentOpus", "antigravity":"ParentGemini"});
        report["room"]["participants"].as_array_mut().unwrap().extend([
            json!({"participant":"claude-lead", "parent":"M", "role":"lead", "provider":"claude", "state":"working", "title":"Lead work", "model":"lead-model"}),
            json!({"participant":"codex-lead", "parent":"M", "role":"lead", "provider":"codex", "state":"working", "title":"Codex lead"}),
            json!({"participant":"antigravity-lead", "parent":"M", "role":"lead", "provider":"antigravity", "state":"working", "title":"AGY lead"}),
            json!({"participant":"other", "parent":"absent", "role":"advisor", "provider":"antigravity", "state":"working", "title":"Other work", "model":"route-guess"}),
            json!({"participant":"linked", "parent":"claude-lead", "role":"reviewer", "provider":"codex", "state":"working", "title":"Linked", "model":"route-guess"}),
        ]);
        report["native_children"] = json!([
            child("codex", "c", "builder", "C fixture", Some(" Sol ")),
            child("claude", "l", "reviewer", "L fixture", Some("Opus")),
            child(
                "antigravity",
                "a",
                "researcher",
                "A fixture",
                Some(" \u{1b}[31m Un\u{200b}known\u{1b}[0m")
            ),
            json!({"native_child":true, "provider":"claude", "participant":"unlinked",
                "state":"working", "parent":"absent", "title":"Unlinked fixture",
                "observed_model":" \u{009b}31mUnknown\u{009b}0m ", "model":"route-guess"}),
            json!({"native_child":true, "provider":"codex", "participant":"mismatched",
                "state":"working", "parent":"claude-lead", "title":"Mismatched fixture", "observed_model":"Sol"}),
        ]);
        for view in [View::Auto, View::Compact, View::Tree, View::Graph] {
            let graph = matches!(view, View::Tree | View::Graph);
            let connector = if view == View::Graph { '→' } else { '─' };
            let has_branch = |line: &str| {
                line.contains(&format!("├{connector} ")) || line.contains(&format!("└{connector} "))
            };
            for columns in [64, 84, 140] {
                let nav = navigation::Navigation {
                    view,
                    ..Default::default()
                };
                let frame = board_frame(&report, &nav, columns, 36, &BTreeMap::new(), false, true);
                let row = |id: &str| -> &str {
                    frame
                        .worker_rows
                        .iter()
                        .find_map(|(row, (participant, _))| {
                            (participant == id).then(|| frame.lines[*row].as_str())
                        })
                        .unwrap_or_else(|| panic!("{id} {columns}: {:?}", frame.lines))
                };
                let mut slots = vec![];
                for (id, expected) in [("c", "Sol"), ("l", "Opus"), ("a", "—")] {
                    let line = row(id);
                    assert_eq!(has_branch(line), graph, "{view:?} {columns}: {line}");
                    assert_eq!(width(line), columns);
                    assert!(
                        !line.contains("legacy-guess")
                            && !line.contains("Parent")
                            && !line.contains("Unknown"),
                        "{line}"
                    );
                    let positions = [expected, "[⟳ run]", "12.5K rsp", "3m ago"].map(|field| {
                        let at = line
                            .find(field)
                            .unwrap_or_else(|| panic!("{view:?} {columns} {field}: {line}"));
                        width(&line[..at])
                    });
                    slots.push(positions);
                }
                assert!(
                    slots.iter().all(|slot| *slot == slots[0]),
                    "{view:?} {columns}: {slots:?}"
                );
                let linked = row("linked");
                assert!(
                    has_branch(linked) == graph
                        && linked.contains('—')
                        && !linked.contains("route-guess"),
                    "{linked}"
                );
                assert_eq!(
                    has_branch(row("claude-lead")),
                    graph,
                    "{view:?}: {}",
                    row("claude-lead")
                );
                assert!(!has_branch(row("other")), "{}", row("other"));
                let unlinked = row("unlinked");
                assert!(
                    !has_branch(unlinked)
                        && unlinked.contains('—')
                        && !unlinked.contains("31m")
                        && !unlinked.contains("route-guess"),
                    "{unlinked}"
                );
                let mismatched = row("mismatched");
                assert!(
                    !has_branch(mismatched) && mismatched.contains("Sol"),
                    "{mismatched}"
                );
            }
        }
        for view in [View::Compact, View::Graph] {
            let mut nav = navigation::Navigation {
                view,
                detail: true,
                ..Default::default()
            };
            let visible = visible(&report, &nav);
            for (id, model) in [
                ("linked", "—"),
                ("unlinked", "—"),
                ("other", "—"),
                ("a", "—"),
                ("l", "Opus"),
                ("claude-lead", "lead-model"),
            ] {
                nav.selected = visible
                    .iter()
                    .position(|row| row["participant"] == id)
                    .unwrap();
                let detail = frame(&report, &nav, 100, 30, &BTreeMap::new());
                assert!(
                    detail
                        .iter()
                        .any(|line| line.contains("Model") && line.contains(model)),
                    "{view:?} {id}: {detail:?}"
                );
                assert!(
                    !detail
                        .iter()
                        .any(|line| line.contains("route-guess") || line.contains("Unknown")),
                    "{detail:?}"
                );
            }
        }
        let mut ambiguous = calls(&report);
        let duplicate = ambiguous
            .iter()
            .find(|call| call["participant"] == "claude-lead")
            .unwrap()
            .clone();
        ambiguous.push(duplicate);
        for graph in [false, true] {
            let rows = tree(&ambiguous, &Default::default(), graph, usize::MAX);
            for title in ["L fixture", "Linked"] {
                let line = rows.iter().find(|line| line.contains(title)).unwrap();
                assert!(!line.contains(['├', '└']), "{line}");
            }
        }
    }

    #[test]
    fn agy_roster_dedup_carries_live_fields_only_on_unique_active_match() {
        let now = oms_panel_core::timestamp();
        let paint = |r: &Value| {
            board_frame(
                r,
                &Default::default(),
                140,
                27,
                &BTreeMap::new(),
                false,
                true,
            )
        };
        let agy_card_rows = |frame: &BoardFrame| -> Vec<usize> {
            let heading = frame
                .lines
                .iter()
                .position(|line| line.contains("Antigravity · "))
                .unwrap();
            (heading + 1..frame.lines.len())
                .take_while(|row| frame.lines[*row].starts_with('│'))
                .collect()
        };
        let roster_item = || {
            json!({
                "role": "Researcher",
                "title": "Inspect custody",
                "state": "reading",
                "activity": "Reading file",
                "elapsed": "12s"
            })
        };
        let native_child = |id: &str, state: &str| {
            json!({
                "participant": id,
                "native_child": true,
                "provider": "antigravity",
                "role": "subagent",
                "state": state,
                "title": "Inspect custody",
                "native_response_usage": {
                    "response": {
                        "input_tokens": 1200,
                        "output_tokens": 340
                    }
                }
            })
        };
        let base = || {
            let mut report = top_bar_report();
            report["generated_at"] = json!(now);
            report["provider_sessions"] = json!(["claude", "antigravity"]);
            report["provider_activity"]["antigravity"] = json!({
                "state": "working",
                "children": [roster_item()]
            });
            report
        };

        // 1. Unique match (working/reading): suppresses duplicate preview, carries live role,
        // activity and elapsed, preserves real split response tokens and selection identity.
        let mut unique = base();
        unique["native_children"] = json!([native_child(
            "native-antigravity-1111111111111111",
            "unknown"
        )]);
        let frame = paint(&unique);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 1, "{:?}", frame.lines);
        let line = &frame.lines[agy[0]];
        assert!(line.starts_with("│   "), "{line}");
        assert!(line.contains("Researcher Inspect custody"), "{line}");
        assert!(line.contains("[⟳ reading]"), "{line}");
        assert!(line.contains("12s"), "{line}");
        assert!(line.contains("— tok"), "{line}");
        assert!(line.contains("rsp in 1.2K out 340"), "{line}");
        assert!(!line.contains("1.5K"), "{line}");
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111"),
            "{line}"
        );

        // 2a. Duplicate title in native_children (ambiguous): no carryover onto either row.
        let mut dup_native = base();
        dup_native["native_children"] = json!([
            native_child("native-antigravity-1111111111111111", "unknown"),
            native_child("native-antigravity-2222222222222222", "unknown")
        ]);
        let frame = paint(&dup_native);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 2, "{:?}", frame.lines);
        for row in &agy {
            let line = &frame.lines[*row];
            assert!(line.starts_with("│   "), "{line}");
            assert!(line.contains("Worker"), "{line}");
            assert!(!line.contains("Researcher"), "{line}");
            assert!(!line.contains("[⟳ reading]"), "{line}");
            assert!(!line.contains("12s"), "{line}");
        }
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111")
        );
        assert_eq!(
            frame.worker_rows.get(&agy[1]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-2222222222222222")
        );

        // 2b. Duplicate title in agy_roster (ambiguous): no carryover onto transcript child.
        let mut dup_roster = base();
        dup_roster["provider_activity"]["antigravity"]["children"] =
            json!([roster_item(), roster_item()]);
        dup_roster["native_children"] = json!([native_child(
            "native-antigravity-1111111111111111",
            "unknown"
        )]);
        let frame = paint(&dup_roster);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 1, "{:?}", frame.lines);
        let line = &frame.lines[agy[0]];
        assert!(line.starts_with("│   "), "{line}");
        assert!(line.contains("Worker"), "{line}");
        assert!(!line.contains("Researcher"), "{line}");
        assert!(!line.contains("[⟳ reading]"), "{line}");
        assert!(!line.contains("12s"), "{line}");
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111")
        );

        // 2c. Short viewport truncates a 2-row duplicate roster to 1 visible row:
        // ambiguity still evaluates against the full observed roster, so no merge occurs.
        let selected = visible(&dup_roster, &Default::default())
            .iter()
            .position(|call| call["participant"] == "native-antigravity-1111111111111111")
            .unwrap();
        let agy_nav = navigation::Navigation {
            selected,
            ..Default::default()
        };
        let paint_short = |r: &Value, height: usize| {
            board_frame(r, &agy_nav, 140, height, &BTreeMap::new(), false, true)
        };
        let truncated_height = (10..=20)
            .find(|&height| {
                let frame = paint_short(&dup_roster, height);
                frame.provider_rows[2]
                    .is_some_and(|heading| frame.lines[heading].contains("1 agent"))
            })
            .expect("viewport height truncating 2-row roster to 1");
        let frame = paint_short(&dup_roster, truncated_height);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 1, "{:?}", frame.lines);
        let line = &frame.lines[agy[0]];
        assert!(line.starts_with("│ ▸ "), "{line}");
        assert!(line.contains("Worker"), "{line}");
        assert!(!line.contains("Researcher"), "{line}");
        assert!(!line.contains("[⟳ reading]"), "{line}");
        assert!(!line.contains("12s"), "{line}");
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111")
        );

        // 2d. Genuine unique match in that same short viewport still merges cleanly.
        let frame = paint_short(&unique, truncated_height);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 1, "{:?}", frame.lines);
        let line = &frame.lines[agy[0]];
        assert!(line.starts_with("│ ▸ "), "{line}");
        assert!(line.contains("Researcher Inspect custody"), "{line}");
        assert!(line.contains("[⟳ reading]"), "{line}");
        assert!(line.contains("12s"), "{line}");
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111")
        );

        // 3a. Terminal/stale transcript child (state: "done"): never inherits live roster state.
        let mut terminal = base();
        let mut done_child = native_child("native-antigravity-3333333333333333", "done");
        done_child["updated_at"] = json!(now.saturating_sub(10));
        done_child["refs"] = json!({"panel_access": "write"});
        terminal["native_children"] = json!([done_child]);
        let frame = paint(&terminal);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 1, "{:?}", frame.lines);
        let line = &frame.lines[agy[0]];
        assert!(line.starts_with("│   "), "{line}");
        assert!(line.contains("Worker"), "{line}");
        assert!(line.contains("[✓ exited]"), "{line}");
        assert!(!line.contains("Researcher"), "{line}");
        assert!(!line.contains("[⟳ reading]"), "{line}");
        assert!(!line.contains("12s"), "{line}");
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-3333333333333333")
        );

        // 3b. Disconnected AGY card (!connected): never merges roster state.
        let mut disconnected = base();
        disconnected["provider_sessions"] = json!(["claude"]);
        disconnected["native_children"] = json!([native_child(
            "native-antigravity-1111111111111111",
            "unknown"
        )]);
        let frame = paint(&disconnected);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 1, "{:?}", frame.lines);
        let line = &frame.lines[agy[0]];
        assert!(line.starts_with("│   "), "{line}");
        assert!(line.contains("Worker"), "{line}");
        assert!(!line.contains("Researcher"), "{line}");
        assert!(!line.contains("[⟳ reading]"), "{line}");
        assert!(!line.contains("12s"), "{line}");
        assert_eq!(
            frame.worker_rows.get(&agy[0]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111")
        );

        // 4. Unmatched roster preview + uniquely matched working child: stable flat rows and elapsed.
        let mut mixed = base();
        mixed["provider_activity"]["antigravity"]["children"] = json!([
            {
                "role": "Advisor",
                "title": "Plan next step",
                "state": "working",
                "activity": "Generating",
                "elapsed": "45s"
            },
            roster_item()
        ]);
        mixed["native_children"] = json!([native_child(
            "native-antigravity-1111111111111111",
            "unknown"
        )]);
        let frame = paint(&mixed);
        let agy = agy_card_rows(&frame);
        assert_eq!(agy.len(), 2, "{:?}", frame.lines);
        let preview = &frame.lines[agy[0]];
        let matched = &frame.lines[agy[1]];
        assert!(preview.starts_with("│   "), "{preview}");
        assert!(preview.contains("Advisor"), "{preview}");
        assert!(preview.contains("Plan next step"), "{preview}");
        assert!(preview.contains("[⟳ generating]"), "{preview}");
        assert!(preview.contains("45s"), "{preview}");
        assert!(!frame.worker_rows.contains_key(&agy[0]), "{preview}");
        assert!(matched.starts_with("│   "), "{matched}");
        assert!(matched.contains("Researcher Inspect custody"), "{matched}");
        assert!(matched.contains("[⟳ reading]"), "{matched}");
        assert!(matched.contains("rsp in 1.2K out 340"), "{matched}");
        assert!(matched.contains("12s"), "{matched}");
        assert_eq!(
            frame.worker_rows.get(&agy[1]).map(|(id, _)| id.as_str()),
            Some("native-antigravity-1111111111111111")
        );
        assert!(frame.lines.iter().all(|line| width(line) <= 140));
    }

    #[test]
    fn weekly_line_carries_lead_activity_without_new_rows() {
        let now = 1_000_000.0;
        let base = json!({"collection":{"ok":true}, "provider_status":{
            "codex":{"source":"session log", "observed_at":now - 30.0, "weekly_used":25},
            "claude":{"source":"HUD cache", "observed_at":now - 30.0, "weekly_used":20},
            "antigravity":{"source":"unavailable"}}});
        let with = |edit: &dyn Fn(&mut Value)| {
            let mut report = base.clone();
            report["provider_sessions"] = json!(["codex", "claude", "antigravity"]);
            report["provider_activity"] = json!({"codex":"working", "claude":{"state":"idle"}});
            edit(&mut report);
            report
        };
        let line = |report: &Value, columns| {
            let usage = crate::usage::snapshot(report, now);
            provider_line(report, &usage, now, columns, false, false)
                .0
                .trim_end()
                .to_owned()
        };
        let report = with(&|_| {});
        assert_eq!(
            line(&report, 170),
            "Weekly left · Codex 75% · working │ Claude 80% · idle │ AGY — · unknown"
        );
        // Unconnected providers carry no activity token.
        let partial = with(&|r| r["provider_sessions"] = json!(["codex"]));
        assert_eq!(
            line(&partial, 170),
            "Weekly left · Codex 75% · working │ Claude 80% │ AGY —"
        );
        // Keep full provider names when activity words only fit as suffixes.
        assert_eq!(
            line(&report, 60),
            "Weekly left · Codex 75%* │ Claude 80%- │ AGY —?"
        );
        let gemini = with(&|r| {
            r["provider_status"]["claude"]["weekly_used"] = json!(0);
            r["provider_status"]["antigravity"] =
                json!({"weekly_native_observed_at":now - 30.0, "weekly_used":5.38});
        });
        assert_eq!(
            line(&gemini, 80),
            "Weekly left · Codex 75%* │ Claude 100%- │ AGY Gemini 95%?"
        );
        assert_eq!(
            line(&gemini, 170),
            "Weekly left · Codex 75% · working │ Claude 100% · idle │ AGY Gemini 95% · unknown"
        );
        let mut words_fit = gemini.clone();
        words_fit["provider_status"]["claude"]["weekly_used"] = json!(20);
        assert_eq!(
            line(&words_fit, 80),
            "Weekly left · Codex 75% · working │ Claude 80% · idle │ AGY Gemini 95% · unknown"
        );
        assert_eq!(line(&gemini, 56), "Wk left Cdx75%* Cld100%- AGY95%?");
        assert_eq!(line(&gemini, 24), "W C75% L100% A95%");
        let mut pending = gemini.clone();
        pending["collection"]["pending"] = json!(true);
        assert_eq!(
            line(&pending, 80),
            "Weekly left · Codex 75%~ │ Claude 100%~ │ AGY Gemini 95%~"
        );
        let accepted_activity = |report: &Value, columns| {
            provider_line(
                report,
                &crate::usage::snapshot(report, now),
                now,
                columns,
                false,
                true,
            )
            .0
            .trim_end()
            .to_owned()
        };
        for columns in [40, 80, 170] {
            assert_eq!(accepted_activity(&pending, columns), line(&gemini, columns));
            let mut stale = pending.clone();
            stale["capacity_stale"] = json!(true);
            assert_eq!(accepted_activity(&stale, columns), line(&stale, columns));
        }
        let mut exhausted = gemini.clone();
        for provider in ["codex", "claude", "antigravity"] {
            exhausted["provider_status"][provider]["weekly_used"] = json!(100);
        }
        assert_eq!(
            line(&exhausted, 80),
            "Weekly left · Codex 0%* │ Claude 0%- │ AGY Gemini 0%?"
        );
        exhausted["capacity_stale"] = json!(true);
        assert_eq!(
            line(&exhausted, 80),
            "Weekly left · Codex 0%! │ Claude 0%! │ AGY Gemini 0%!"
        );
        assert_eq!(line(&report, 40), "Wk left Cdx75%* Cld80%- AGY—?");
        assert_eq!(line(&report, 28), "Wk left Cdx75% Cld80% AGY—");
        assert_eq!(line(&report, 24), "W C75% L80% A—");
        for columns in [24, 28, 40, 60, 80, 100, 170] {
            for report in [&report, &partial] {
                let text = line(report, columns);
                assert!(width(&text) <= columns, "{columns}: {text}");
                assert!(text.contains("75%") && text.contains("80%"), "{text}");
            }
        }
        // Only a current read is pending. A failed or held read carries stale activity.
        for (collection, capacity_stale, state, marker) in [
            (json!({"ok":true,"pending":true}), false, "pending", "~"),
            (json!({"ok":true}), true, "stale", "!"),
            (
                json!({"ok":false,"error":"unavailable"}),
                true,
                "stale",
                "!",
            ),
            (json!({"ok":false,"pending":true}), false, "stale", "!"),
            (json!({"ok":true,"pending":true}), true, "stale", "!"),
        ] {
            let held = with(&|r| {
                r["collection"] = collection.clone();
                r["capacity_stale"] = json!(capacity_stale);
            });
            let text = line(&held, 170);
            assert!(
                !text.contains("working") && !text.contains("idle"),
                "{text}"
            );
            assert_eq!(text.matches(state).count(), 3, "{text}");
            assert_eq!(
                line(&held, 40),
                format!("Wk left Cdx75%{marker} Cld80%{marker} AGY—{marker}")
            );
            assert_eq!(line(&held, 28), "Wk left Cdx75% Cld80% AGY—");
            assert_eq!(line(&held, 24), "W C75% L80% A—");
        }
        // The weekly hit stays on the same single row and covers its text; no row is added.
        let nav = navigation::Navigation::default();
        let read = BTreeMap::new();
        let plain = frame_styled(&base, &nav, 170, 14, &read, false, false);
        let lit = frame_styled(&report, &nav, 170, 14, &read, false, false);
        assert_eq!(plain.len(), lit.len(), "{lit:#?}");
        let at = lit
            .iter()
            .position(|l| l.starts_with("Weekly left · "))
            .unwrap();
        for token in ["· working", "· idle", "· unknown"] {
            assert!(lit[at].contains(token), "{lit:#?}");
        }
        assert_eq!(
            plain.iter().position(|l| l.starts_with("Weekly left · ")),
            Some(at)
        );
    }

    #[test]
    fn project_top_bar_room_scoped_goal_and_weekly_remaining() {
        let read = BTreeMap::new();
        let nav = navigation::Navigation::default();
        let mut bound = top_bar_report();
        bound["project_name"] = json!("canonical");
        let variant = |edit: &dyn Fn(&mut Value)| {
            let mut report = bound.clone();
            report["room"]["task_id"] = Value::Null;
            edit(&mut report);
            report
        };
        let worker_refs = |report: &mut Value, refs: Value| {
            for index in [1, 2] {
                report["room"]["participants"][index]["refs"] = refs.clone();
            }
        };
        let all_closed = {
            let mut report = bound.clone();
            for task in report["plan"]["tasks"]
                .as_object_mut()
                .unwrap()
                .values_mut()
            {
                task["state"] = json!("done");
            }
            report
        };
        // A reused task id binds only with this plan's exact id, in this Room.
        let unbound = [
            all_closed,
            variant(&|r| worker_refs(r, json!({}))),
            variant(&|r| worker_refs(r, json!({"plan_id":"plan-old"}))),
            variant(&|r| {
                r["plan"].as_object_mut().unwrap().remove("plan_id");
            }),
            variant(&|r| worker_refs(r, json!({"plan_id":"plan-1", "panel_room_id":"room-other"}))),
        ];
        let room_link = variant(&|r| {
            worker_refs(r, json!({}));
            r["room"]["task_id"] = json!("t3");
        });
        for columns in [40, 80, 120] {
            let unbound_header = if columns < 44 {
                "OMS · canonical".to_owned()
            } else {
                let right = "1 running  [Overview]";
                format!(
                    "OMS · canonical{}{right}",
                    " ".repeat(columns - 15 - right.chars().count())
                )
            };
            let usage = if columns < 44 {
                "Wk left Cdx— Cld— AGY—"
            } else {
                "Weekly left · Codex — │ Claude — │ AGY —"
            };
            for report in [&bound, &room_link] {
                for fill in [false, true] {
                    let lines = frame_styled(report, &nav, columns, 24, &read, false, fill);
                    assert!(lines[0].starts_with("OMS · canonical"), "{lines:#?}");
                    assert!(lines[0].contains("2/5 verified"), "{lines:#?}");
                    assert_eq!(lines[1], usage, "{lines:#?}");
                    assert!(lines.iter().all(|line| !line.contains("Plan ")));
                    assert!(
                        lines.iter().any(|line| line.contains("Goal:")),
                        "{lines:#?}"
                    );
                    assert!(
                        lines.iter().any(|line| line.contains("Ship top bar")),
                        "{lines:#?}"
                    );
                    assert!(
                        lines.iter().any(|line| line.contains("2/5 verified") && line.contains("2 claimed")),
                        "{lines:#?}"
                    );
                    assert!(!lines.join("\n").contains("Shared work"));
                    if fill {
                        assert_eq!(lines.len(), 24, "managed frame keeps its height");
                    }
                }
            }
            for unrelated in &unbound {
                let lines = frame_styled(unrelated, &nav, columns, 24, &read, false, true);
                assert_eq!(lines.len(), 24);
                assert_eq!(lines[..2], [&unbound_header, usage], "{lines:#?}");
                assert!(
                    lines
                        .iter()
                        .all(|line| !line.contains("Goal:") && !line.starts_with("Plan ")),
                    "{lines:#?}"
                );
                assert!(lines.iter().any(|line| line.contains("Main")));
            }
        }
        let mut titled = bound.clone();
        titled["room"]["title"] = json!("Release prep");
        for (columns, height, show_room) in [(24, 13, false), (40, 24, true), (80, 13, true)] {
            let before = board_frame(&bound, &nav, columns, height, &read, false, true);
            let after = board_frame(&titled, &nav, columns, height, &read, false, true);
            assert!(after.lines[0].starts_with("OMS · canonical"));
            assert_eq!(after.lines[0].contains("Release prep"), show_room);
            assert_eq!(after.lines[1..], before.lines[1..]);
            assert_eq!(after.provider_rows, before.provider_rows);
            assert_eq!(after.worker_rows, before.worker_rows);
            let hit = |frame: &BoardFrame| frame.weekly_usage.map(|hit| (hit.row, hit.columns));
            assert_eq!(hit(&after), hit(&before));
        }
        titled["room"]["id"] = json!("room-unrelated");
        assert!(frame(&titled, &nav, 80, 24, &read)[0].starts_with("OMS · canonical"));
        let neutral_header = &frame(&bound, &nav, 80, 24, &read)[0];
        for title in [Value::Null, json!("   "), json!("  Shared work  ")] {
            let mut neutral = bound.clone();
            neutral["room"]["title"] = title;
            assert_eq!(&frame(&neutral, &nav, 80, 24, &read)[0], neutral_header);
        }
        let mut unavailable = titled.clone();
        unavailable["room"]["id"] = Value::Null;
        assert!(!frame(&unavailable, &nav, 80, 24, &read)[0].contains("Release prep"));
        unavailable = titled.clone();
        unavailable["collection"]["ok"] = false.into();
        assert!(!frame(&unavailable, &nav, 80, 24, &read)[0].contains("Release prep"));
        titled["room"]["title"] = json!(format!("\x1b[31m{}\x1b[0m", "릴리스 작업 ".repeat(20)));
        let wide = &frame(&titled, &nav, 80, 24, &read)[0];
        assert!(wide.starts_with("OMS · canonical · 릴리스 작업"));
        assert!(wide.contains('…') && !wide.contains('\x1b'));
        assert!(wide.ends_with("[Overview]") && width(wide) == 80);
        let mut idle = bound.clone();
        idle["room"]["participants"][0]["state"] = json!("done");
        assert!(frame(&idle, &nav, 80, 24, &read)[0].starts_with("OMS · canonical"));
        idle["project_name"] = json!("another-project");
        assert!(frame(&idle, &nav, 80, 24, &read)[0].starts_with("OMS · another-project"));

        let now = 1_000_000.0;
        let exhausted = json!({"provider_status":{
            "codex":{"source":"session log", "observed_at":now - 30.0, "weekly_used":100,
                "account_label":"acct-zeta", "accounts":{"acct-zeta":{}, "spare":{}}},
            "claude":{"source":"HUD cache", "observed_at":now - 700.0, "weekly_used":30},
            "antigravity":{"source":"unavailable"}
        }});
        let full = json!({"provider_status":{
            "codex":{"source":"session log", "observed_at":now - 30.0, "weekly_used":0},
            "claude":{"source":"HUD cache", "observed_at":now - 30.0, "weekly_used":99.6}
        }});
        let expired = json!({"provider_status":{
            "codex":{"source":"session log", "observed_at":now - 30.0, "weekly_used":40,
                "weekly_resets_at":now - 10.0},
            "claude":{"source":"HUD cache", "observed_at":now - 30.0, "weekly_used":0.4,
                "weekly_resets_at":now + 3600.0}
        }});
        for (report, expected) in [
            (
                &exhausted,
                [
                    "Wk left Cdx0% Cldstale AGY—",
                    "Weekly left · Codex 0% exhausted │ Claude stale │ AGY —",
                    "Weekly left · Codex 0% exhausted │ Claude stale 11m │ AGY —",
                ],
            ),
            (
                &full,
                [
                    "Wk left Cdx100% Cld<1% AGY—",
                    "Weekly left · Codex 100% │ Claude <1% │ AGY —",
                    "Weekly left · Codex 100% │ Claude <1% │ AGY —",
                ],
            ),
            (
                &expired,
                [
                    "Wk left Cdxreset Cld>99% AGY—",
                    "Weekly left · Codex reset │ Claude >99% │ AGY —",
                    "Weekly left · Codex reset │ Claude >99% │ AGY —",
                ],
            ),
        ] {
            let usage = crate::usage::snapshot(report, now);
            for (columns, line) in [40, 80, 120].into_iter().zip(expected) {
                assert!(!line.contains("acct-zeta"));
                assert_eq!(
                    provider_line(report, &usage, now, columns, false, false)
                        .0
                        .trim_end(),
                    line
                );
            }
        }
        let usage = crate::usage::snapshot(&exhausted, now);
        assert_eq!(
            provider_line(&exhausted, &usage, now, 24, false, false)
                .0
                .trim_end(),
            "W C0% L! A—"
        );
        assert_eq!(
            weekly_usage_detail(&exhausted, now),
            "Codex · 0% left\n  Source Codex session log · Reset unreported\n  \
             Registered Codex accounts (Codex session log)\n  \
             @acct-zeta · missing weekly reading · Log age — · Reset unreported\n  \
             @spare · missing weekly reading · Log age — · Reset unreported\n\
             Claude · stale (11m)\n  Source HUD cache · Reset unreported\n\
             AGY · missing weekly reading\n  Source unreported · Reset unreported"
        );
        let detail = weekly_usage_detail(&expired, now);
        assert!(
            detail.contains("Codex · window expired\n  Source Codex session log · Reset elapsed")
        );
        assert!(detail.contains("Claude · >99% left\n  Source HUD cache · Reset in 1h00m"));
        for reset in [now - 60.0, now + 60.0] {
            let mut native = expired.clone();
            native["provider_status"]["claude"]["weekly_native_observed_at"] = json!(now - 30.0);
            native["provider_status"]["claude"]["weekly_resets_at"] = json!(reset);
            native["provider_status"]["antigravity"] = json!({
                "source":"ambient-account-name", "account_label":"ambient-account-name",
                "weekly_used":40, "weekly_native_observed_at":now - 10.0,
                "weekly_resets_at":reset
            });
            let detail = weekly_usage_detail(&native, now);
            assert!(
                detail.contains(
                    "Claude · >99% left\n  Source Claude native usage · Reset unreported"
                )
            );
            assert!(
                detail.contains("AGY · 60% left\n  Source AGY Gemini quota · Reset unreported")
            );
            assert!(!detail.contains("ambient-account-name"));
            let usage = crate::usage::snapshot(&native, now);
            assert!(
                provider_line(&native, &usage, now, 120, false, false)
                    .0
                    .contains("Claude >99%")
            );
        }
        let mut gemini = exhausted.clone();
        gemini["provider_status"]["antigravity"] =
            json!({"weekly_native_observed_at":now - 30.0, "weekly_used":40});
        let usage = crate::usage::snapshot(&gemini, now);
        assert_eq!(
            provider_line(&gemini, &usage, now, 80, false, false)
                .0
                .trim_end(),
            "Weekly left · Codex 0% exhausted │ Claude stale │ AGY Gemini 60%"
        );
        // A variant wider than the row falls back whole instead of clipping a value.
        assert_eq!(
            provider_line(&gemini, &usage, now, 56, false, false)
                .0
                .trim_end(),
            "Wk left Cdx0% Cldstale AGY60%"
        );

        // Schema 1 is unchanged: rendering reads fractions from the report itself.
        let snap = crate::usage::snapshot(&full, now);
        let keys = |value: &Value| {
            let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        assert_eq!(
            keys(&snap["providers"]["claude"]),
            [
                "account",
                "cache_read_share",
                "context_used",
                "reading",
                "seats",
                "tightest",
                "tokens",
                "windows",
                "windows_unreported"
            ]
        );
        assert_eq!(
            keys(&snap["providers"]["claude"]["windows"][0]),
            [
                "age_seconds",
                "left",
                "level",
                "name",
                "pace",
                "resets_at",
                "resets_in",
                "source",
                "stale",
                "used"
            ]
        );
        assert_eq!(snap["providers"]["claude"]["windows"][0]["used"], 100);
    }

    #[test]
    fn weekly_line_follows_header_above_the_stale_banner() {
        let mut report = top_bar_report();
        report["collection"]["ok"] = json!(false);
        report["provider_sessions"] = json!(["codex", "claude", "antigravity"]);
        report["provider_status"] =
            json!({"codex":{"weekly_used":25, "account_label":"acct-zeta"}});
        let read = BTreeMap::new();
        for columns in [40, 80, 120] {
            let board = board_frame(
                &report,
                &navigation::Navigation::default(),
                columns,
                30,
                &read,
                false,
                true,
            );
            assert!(board.lines[1].contains("75%"), "{:#?}", board.lines);
            assert!(board.lines[2].starts_with("! STALE"), "{:#?}", board.lines);
            assert!(board.lines.iter().all(|line| !line.contains("acct-zeta")));
            let headers: Vec<_> = board.provider_rows.iter().flatten().copied().collect();
            assert_eq!(headers.len(), usize::from(columns >= 44) * 3, "{columns}");
            for row in headers {
                assert!(row > 2 && board.lines[row].contains("Lead chat"));
            }
        }
        let mut pending = report.clone();
        pending["collection"] = json!({"ok":true,"pending":true});
        for (report, activity) in [(&report, "stale"), (&pending, "pending")] {
            for (columns, height, hit) in [
                (1, 30, false),
                (2, 30, false),
                (3, 30, false),
                (9, 30, false),
                (3, 13, false),
                (20, 30, true),
                (20, 10, true),
                (80, 13, true),
                (80, 10, true),
                (80, 9, true),
                (80, 8, false),
                (120, 30, true),
            ] {
                let board = board_frame(
                    report,
                    &Default::default(),
                    columns,
                    height,
                    &read,
                    true,
                    true,
                );
                assert_eq!(
                    board.weekly_usage.is_some(),
                    hit,
                    "{activity} {columns}x{height}"
                );
                if columns == 3 {
                    assert_eq!(strip_sgr(&board.lines[1]).starts_with("W "), height >= 14);
                }
                if let Some(hit) = board.weekly_usage {
                    let printed = &board.lines[hit.row];
                    assert_eq!(hit.row, 1);
                    assert!(printed.contains(SGR_MUTED));
                    assert_eq!(hit.columns, width(&strip_sgr(printed)));
                    assert!(hit.columns <= columns);
                    if columns == 120 {
                        assert!(printed.contains(&format!("75% · {activity}")), "{printed}");
                        assert_eq!(
                            strip_sgr(&board.lines[2]).starts_with("! STALE"),
                            activity == "stale"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn overall_main_and_worker_rows_use_separate_selectable_targets() {
        let mut report = json!({
            "project_name":"project-한글", "generated_at":1_700_000_037,
            "collection":{"ok":true},
            "board_checkout":{"attempt":"main-1","branch":"oms/review","worktree":"scratch-r37"},
            "room":{"id":"room-x","main":"pinned","participants":[
                {"participant":"earlier","role":"main","provider":"codex","model":"other-main","state":"working","created_at":"1"},
                {"participant":"pinned","attempt":"main-1","role":"main","provider":"claude","model":"opus","state":"working","joined":true},
                {"participant":"worker","attempt":"build-1","role":"worker","alias":"아다","provider":"codex","state":"working","parent":"pinned","title":"Role builder. Builder · Worker brief: Task: Fix 한글 clipping"},
                {"participant":"lead","attempt":"lead-1","role":"lead","alias":"Lee","provider":"claude","state":"working","title":"Coordinate review"},
                {"participant":"next","role":"worker","alias":"샘","provider":"codex","state":"working","parent":"lead","title":"Worker: Brief: Read fixtures"}
            ]},
            "room_attempts":{
                "main-1":{"attempt_id":"main-1","tool":"panel-main","provider":"claude","state":"working","terminal":false,
                    "created_at":1_700_000_000,"refs":{"panel_room_id":"room-x","panel_room_participant":"pinned","panel_role":"main"},
                    "usage":{"tokens":2450000},"usage_reports":{"tokens":1}},
                "build-1":{"attempt_id":"build-1","provider":"codex","model":"gpt-x","state":"working","created_at":1_699_999_912,
                    "refs":{"panel_room_id":"room-x","panel_room_participant":"worker","panel_role":"worker","panel_strategy":"builder","panel_alias":"아다"},
                    "usage":{"tokens":12500},"usage_reports":{"tokens":1}},
                "lead-1":{"attempt_id":"lead-1","provider":"claude","state":"working",
                    "refs":{"panel_room_id":"room-x","panel_room_participant":"lead"},
                    "usage":{"tokens":0},"usage_reports":{"tokens":1}}
            }
        });
        let read = BTreeMap::new();
        for columns in [19, 20, 28, 43, 44, 55, 56, 72, 95, 96, 100] {
            let nav = navigation::Navigation::default();
            let board = board_frame(&report, &nav, columns, 20, &read, false, true);
            let colored = board_frame(&report, &nav, columns, 20, &read, true, true);
            assert_eq!(board.lines.len(), 20);
            let find_row = |id| {
                let (&row, _) = board
                    .worker_rows
                    .iter()
                    .find(|(_, hit)| hit.0 == id)
                    .unwrap();
                (row, board.lines[row].trim_end_matches(" │"))
            };
            let (main_row, main) = find_row("pinned");
            assert!(
                board.lines[0].starts_with("OMS · project-한글"),
                "{:?}",
                board.lines
            );
            assert!(main.contains("▸") && main.contains("opus"), "{main}");
            if columns >= 100 {
                assert!(
                    board.lines[main_row + 1].contains("Branch oms/review · Worktree scratch-r37")
                );
                assert!(!board.lines[main_row + 1].contains("Main elapsed"));
            }
            assert!(
                board
                    .lines
                    .iter()
                    .filter(|line| line.contains(if columns < 28 {
                        "Main opus"
                    } else {
                        "Overall Main"
                    }))
                    .count()
                    == 1
            );
            // When room permits, align the 6-cell value before the elapsed column.
            // A 1-cell `~` slot marks last-observed totals.
            let wide = columns >= 44;
            for (id, leading, snippet, token) in [
                (
                    "worker",
                    if columns >= 56 {
                        "Builder    아"
                    } else {
                        "Builder 아"
                    },
                    "Fi",
                    if wide { "  12.5K" } else { "12.5K" },
                ),
                (
                    "lead",
                    "Lead Lee",
                    "Coo",
                    if wide { "      0" } else { "0 " },
                ),
                (
                    "next",
                    if columns >= 56 {
                        "Worker     샘"
                    } else {
                        "Worker  샘"
                    },
                    "Rea",
                    if wide { "      —" } else { "— " },
                ),
            ] {
                let (row, line) = find_row(id);
                assert!(row > main_row && row < board.lines.len() - 1);
                assert!(
                    line.contains(leading) && (columns < 28 || line.contains(snippet)),
                    "{line}"
                );
                assert!(
                    if columns >= 56 {
                        let (prefix, elapsed) = line.rsplit_once(" tok").unwrap();
                        prefix.ends_with(token)
                            && elapsed.trim() == if id == "worker" { "2m" } else { "—" }
                    } else {
                        line.ends_with(token)
                    },
                    "{line}"
                );
                assert_eq!(
                    width(line),
                    columns - usize::from(columns >= 44) * 2,
                    "{line}"
                );
            }
            let (_, builder) = find_row("worker");
            let (_, worker) = find_row("next");
            let (_, lead) = find_row("lead");
            if columns >= 56 {
                assert_eq!(main.rsplit_once(" tok").unwrap().1.trim(), "37s", "{main}");
            }
            let token_end = |line: &str, numeral: &str| {
                width(&line[..line.rfind(numeral).unwrap()]) + width(numeral)
            };
            if wide {
                // Whole fields share one right edge; the last digit is column-aligned.
                let last_digit = |line: &str, glyph: char| {
                    let tokens = line.rsplit_once(" tok").map_or(line, |(tokens, _)| tokens);
                    width(&tokens[..tokens.rfind(glyph).unwrap()]) + 1
                };
                assert_eq!(last_digit(lead, '0'), last_digit(worker, '—'));
                assert_eq!(token_end(builder, "12.5") + 1, last_digit(lead, '0'));
            } else {
                assert_eq!(
                    token_end(builder, "12.5"),
                    token_end(lead, "0"),
                    "{columns}: {builder:?} / {lead:?} / {worker:?}"
                );
                assert_eq!(token_end(builder, "12.5"), token_end(worker, "—"));
                assert_eq!(token_end(builder, "12.5"), token_end(main, "2.5"));
            }
            assert_eq!(
                width(&builder[..builder.find('아').unwrap()]),
                width(&worker[..worker.find('샘').unwrap()])
            );
            assert!(
                !builder.contains("Role builder.")
                    && !builder.contains("Worker brief")
                    && !builder.contains("Task:")
                    && !builder.contains('←')
            );
            assert!(!worker.contains("Brief:") && !worker.contains("Worker:"));
            if columns <= 20 {
                assert!(worker.contains("Re"), "{worker}");
            }
            assert!(!worker.contains(['├', '└', '←']), "{worker}");
            for (plain, styled) in board.lines.iter().zip(&colored.lines) {
                assert_eq!(*plain, strip_sgr(styled));
                assert!(width(plain) <= columns);
            }
            if wide {
                let expected = if columns >= 46 {
                    "[s Switch Main]"
                } else {
                    "[s Switch]"
                };
                let zone = board
                    .main_switch
                    .expect("Switch Main hit zone on Overall Main card");
                assert_eq!(colored.main_switch, Some(zone));
                assert_eq!(zone.row, main_row - 1);
                let top = &board.lines[zone.row];
                let hit_text: String = top
                    .chars()
                    .skip(zone.start - 1)
                    .take(zone.end - zone.start + 1)
                    .collect();
                assert_eq!(hit_text, expected, "{columns}: {top}");
                assert!(zone.contains(zone.start as u16, (zone.row + 1) as u16));
                assert!(zone.contains(zone.end as u16, (zone.row + 1) as u16));
                assert!(!zone.contains((zone.start - 1) as u16, (zone.row + 1) as u16));
                assert!(!zone.contains((zone.end + 1) as u16, (zone.row + 1) as u16));
                assert!(!zone.contains(zone.start as u16, zone.row as u16));
                assert!(!zone.contains(zone.start as u16, (zone.row + 2) as u16));
            } else {
                assert!(board.main_switch.is_none());
                assert!(colored.main_switch.is_none());
            }
        }
        let clipped = board_frame(
            &report,
            &navigation::Navigation::default(),
            100,
            11,
            &read,
            false,
            true,
        );
        assert!(clipped.main_switch.is_none());
        assert!(!clipped.lines.iter().any(|line| line.contains("[s Switch")));
        let mut unpinned = report.clone();
        unpinned["room"]["main"] = Value::Null;
        unpinned["room"]["participants"] = json!([
            {"participant":"worker","attempt":"build-1","role":"worker","alias":"아다","provider":"codex","state":"working","title":"Fix"}
        ]);
        unpinned["room_attempts"]
            .as_object_mut()
            .unwrap()
            .remove("main-1");
        let no_main = board_frame(
            &unpinned,
            &navigation::Navigation::default(),
            100,
            20,
            &read,
            false,
            true,
        );
        assert!(no_main.main_switch.is_none());
        assert!(!no_main.lines.iter().any(|line| line.contains("[s Switch")));
        let detail = render_detail_card(
            calls(&report).iter().find(|c| c["participant"] == "worker"),
            100,
            96,
            true,
            false,
            usize::MAX,
        )
        .join("\n");
        assert!(
            detail.contains("Role builder. Builder · Worker brief: Task: Fix 한글 clipping")
                && detail.contains("12500 · Events"),
            "{detail}"
        );
        // D reads the summary only on the pinned Overall Main; an earlier
        // main-role row keeps the generic Call Detail.
        let rows = visible(&report, &Default::default());
        for (participant, overall, heading) in [
            ("pinned", true, "Overall Main"),
            ("earlier", false, "Call Detail"),
        ] {
            let selected = rows
                .iter()
                .position(|call| call["participant"] == participant)
                .unwrap();
            assert_eq!(is_overall_main(&report, &rows[selected]), overall);
            let nav = navigation::Navigation {
                detail: true,
                selected,
                ..Default::default()
            };
            let detail_board = board_frame(&report, &nav, 100, 30, &read, false, false);
            assert!(detail_board.main_switch.is_none());
            let card = detail_board.lines.join("\n");
            assert!(!card.contains("[s Switch"), "{card}");
            assert!(
                card.lines()
                    .any(|line| line.contains(heading) && line.contains("Esc back")),
                "{card}"
            );
            assert_eq!(card.contains("Call Detail"), !overall, "{card}");
        }
        let mut unknown = report.clone();
        unknown["room"]["closed"] = json!(true);
        let closed = frame(&unknown, &Default::default(), 100, 20, &read).join("\n");
        assert!(!closed.contains("Main elapsed"));
        unknown["room"]["closed"] = json!(false);
        unknown["room"]["successor"] = json!("room-next");
        let superseded = frame(&unknown, &Default::default(), 100, 20, &read).join("\n");
        assert!(!superseded.contains("Main elapsed"));
        unknown["room"]["successor"] = Value::Null;
        unknown["board_checkout"]["attempt"] = json!("other-main");
        unknown["room_attempts"]["main-1"]["created_at"] = json!(null);
        let missing = frame(&unknown, &Default::default(), 100, 20, &read).join("\n");
        assert!(missing.contains("Checkout unknown") && !missing.contains("Main elapsed"));
        unknown["board_checkout"] =
            json!({"attempt":"main-1","detached":true,"worktree":"scratch-r37"});
        let detached = frame(&unknown, &Default::default(), 100, 20, &read).join("\n");
        assert!(detached.contains("Branch detached · Worktree scratch-r37"));
        unknown["collection"]["pending"] = json!(true);
        let pending = frame(&unknown, &Default::default(), 100, 20, &read).join("\n");
        assert!(pending.contains("Checkout unknown") && !pending.contains("Branch detached"));
        let order = visible(&report, &Default::default());
        assert_eq!(order[0]["participant"], "pinned");
        let worker = order
            .iter()
            .position(|call| call["participant"] == "worker")
            .unwrap();
        for (selected, id) in [(0, "pinned"), (worker, "worker")] {
            for height in [3, 4] {
                let nav = navigation::Navigation {
                    selected,
                    ..Default::default()
                };
                let board = board_frame(&report, &nav, 28, height, &read, false, true);
                let (&row, _) = board
                    .worker_rows
                    .iter()
                    .find(|(_, hit)| hit.0 == id)
                    .unwrap();
                assert!(row < height - 1 && board.lines[row].contains("▸"));
                assert!(board.lines[height - 1].contains("Enter"));
            }
        }
        for (total, label) in [
            (0, "     0"),
            (999, "   999"),
            (1_000, "  1.0K"),
            (12_500, " 12.5K"),
            (300_000, "300.0K"),
            (999_949, "999.9K"),
            (999_950, "  1.0M"),
            (1_200_000, "  1.2M"),
            (2_450_000, "  2.5M"),
            (12_000_000, " 12.0M"),
            (999_949_999, "999.9M"),
            (999_950_000, "  1.0B"),
            (1_000_000_000, "  1.0B"),
            (999_949_999_999, "999.9B"),
            (999_950_000_000, " >999B"),
            (u64::MAX, " >999B"),
        ] {
            assert_eq!(compact_tokens(total), label);
            assert_eq!(width(label), 6);
        }
        let mut main_only = report.clone();
        main_only["room"]["participants"] = json!([{
            "participant":"pinned","attempt":"main-1","role":"main","provider":"claude",
            "model":"gpt-6.1-sol-high","state":"working"
        }]);
        main_only["room_attempts"] = json!({"main-1":{
            "attempt_id":"main-1","tool":"panel-main","provider":"claude","state":"working",
            "refs":{"panel_room_id":"room-x","panel_room_participant":"pinned"},
            "usage":{"tokens":12500},"usage_reports":{"tokens":1}
        }});
        let nav = navigation::Navigation {
            view: View::Compact,
            ..Default::default()
        };
        for (total, pending, label) in [
            (12500, false, "12.5K"),
            (0, false, "0"),
            (12500, true, "~12.5K"),
        ] {
            main_only["room_attempts"]["main-1"]["usage"]["tokens"] = json!(total);
            main_only["collection"]["pending"] = json!(pending);
            for columns in [19, 20] {
                let board = board_frame(&main_only, &nav, columns, 14, &read, true, true);
                let (&row, _) = board.worker_rows.first_key_value().unwrap();
                let line = strip_sgr(&board.lines[row]);
                assert!(
                    line.starts_with("▸ Main gpt")
                        && line.trim_end().ends_with(&format!(" {label}")),
                    "{line}"
                );
                assert_eq!(width(&line), columns, "{line}");
            }
        }
        let main_only_text = frame(&main_only, &nav, 80, 14, &read).join("\n");
        assert!(
            main_only_text.contains("No visible lead or worker calls"),
            "{main_only_text}"
        );
        let empty_text = frame(&json!({"collection":{"ok":true}}), &nav, 80, 14, &read).join("\n");
        assert!(empty_text.contains("No recorded calls"), "{empty_text}");
        report["room"]["main"] = json!("earlier");
        report["room"]["owner"] = json!("pinned");
        assert_eq!(
            visible(&report, &Default::default())[0]["participant"],
            "pinned"
        );
        report["room"]["participants"][1]["joined"] = json!(false);
        report["room"]["participants"][0]["attempt"] = json!("pinned");
        let all = calls(&report);
        assert!(overall_main(&report["room"], &all).is_none());
    }

    #[test]
    fn wide_worker_roles_and_status_have_shared_anchors() {
        let mut anchors = Vec::new();
        for (role, status, total) in [
            ("builder", "running", 300_000),
            ("researcher", "reading", 1_200_000),
            ("reviewer", "review", 12_500),
            ("verifier", "verifying", 300_000),
            ("worker", "editing", 1_200_000),
        ] {
            let call = json!({
                "role":role,"state":status,"title":"한글 task","alias":"한글 task",
                "board_elapsed":"11m", "event_tokens":{"total":total}
            });
            let row = render_call_row(&call, "├─ ", "", false, 102, true, false);
            let column = |glyph: &str| width(&row[..row.find(glyph).unwrap()]);
            anchors.push((column("한글"), column("["), column(" tok"), column("11m")));
            assert_eq!(102, width(&row));
        }
        assert!(anchors.iter().all(|row| *row == anchors[0]), "{anchors:?}");

        let mut report = top_bar_report();
        let member = &mut report["room"]["participants"][1];
        member["role"] = json!("reviewer");
        member["attempt"] = json!("exact-review");
        report["room_attempts"]["exact-review"] = json!({
            "attempt_id":"exact-review","state":"working","provider":"codex","purpose":"verifier",
            "refs":{"panel_room_id":"room-x","panel_room_participant":"W1",
                "panel_role":"reviewer","panel_strategy":"verifier"}
        });
        let display_role = |report: &Value| {
            let calls = calls(report);
            call_display(
                calls
                    .iter()
                    .find(|call| call["participant"] == "W1")
                    .unwrap(),
            )
            .role
            .to_owned()
        };
        assert_eq!(display_role(&report), "verifier");
        report["room_attempts"]["exact-review"]["refs"]["panel_room_id"] = json!("room-other");
        assert_eq!(display_role(&report), "reviewer");
    }

    #[test]
    fn wide_token_fields_share_decimal_and_unit_columns() {
        let row = |call: Value| {
            let right = token_span(&call, 40, 100, true);
            render_lr(
                vec![Span::literal("Row", None)],
                right,
                40,
                false,
                None,
                false,
            )
        };
        let rows = [
            row(json!({"receipt_tokens":{"total":300_000}})),
            row(json!({"receipt_tokens":{"total":1_200_000}})),
            row(json!({"receipt_tokens":{"total":12_500,"last_observed":true}})),
            row(json!({"receipt_tokens":{"total":0}})),
            row(json!({"event_tokens":{"total":0}})),
            row(json!({"receipt_tokens":{"total":"unknown"}})),
        ];
        let column = |line: &str, glyph: char| width(&line[..line.find(glyph).unwrap()]);
        assert!(rows.iter().all(|line| width(line) == 40), "{rows:?}");
        assert!(rows[0].ends_with("300.0K tok") && rows[1].ends_with("  1.2M tok"));
        assert_eq!(column(&rows[0], '.'), column(&rows[1], '.'));
        // `~` sits in its own slot, so the observed row stays aligned and marked.
        assert!(rows[2].ends_with("~ 12.5K tok"), "{}", rows[2]);
        assert_eq!(column(&rows[2], '.'), column(&rows[0], '.'));
        // Zero and unknown stay distinct in the same cells.
        assert!(rows[3].ends_with("      0 tok") && rows[4].ends_with("      0 tok"));
        assert!(rows[5].ends_with("      — tok"), "{}", rows[5]);
        // Medium boards align the field when there is enough room.
        let narrow = |total: u64, columns: usize| {
            let spans = token_span(
                &json!({"receipt_tokens":{"total":total}}),
                12,
                columns,
                true,
            );
            spans
                .iter()
                .map(|span| span.plain.as_str())
                .collect::<String>()
        };
        assert_eq!(narrow(12_500, 43), "12.5K");
        assert_eq!(narrow(12_500, 44), "  12.5K");
        assert_eq!(narrow(12_500, 55), "  12.5K");
        assert_eq!(narrow(12_500, 56), "  12.5K tok");
        assert_eq!(narrow(0, 20), "0 ");
        assert_eq!(narrow(999_950, 20), "1.0M");
    }

    #[test]
    fn elapsed_requires_observed_run_or_explicit_finished_evidence() {
        let mut report = json!({"collection":{"ok":true}});
        let mut call = json!({"attempt":"a","state":"working","started_at":1000,"updated_at":1800});
        assert_eq!(
            worker_elapsed(&report, &call, 1125.0).as_deref(),
            Some("2m")
        );
        report["collection"]["pending"] = json!(true);
        assert!(worker_elapsed(&report, &call, 1125.0).is_none());
        report["collection"]["pending"] = json!(false);
        call["state"] = json!("done");
        assert!(worker_elapsed(&report, &call, 8125.0).is_none());
        call["ended_at"] = json!(1037);
        for observed in [2125.0, 8125.0] {
            assert_eq!(
                worker_elapsed(&report, &call, observed).as_deref(),
                Some("37s")
            );
        }
        call["ended_at"] = json!(999);
        assert!(worker_elapsed(&report, &call, 2125.0).is_none());
        call["duration_ms"] = json!(3_600_000);
        assert_eq!(
            worker_elapsed(&report, &call, 2125.0).as_deref(),
            Some("1h00m")
        );
        let mut native =
            json!({"native_child":true,"state":"unknown","started_at":1000,"ended_at":1037});
        assert!(worker_elapsed(&report, &native, 2125.0).is_none());
        native["state"] = json!("done");
        assert_eq!(
            worker_elapsed(&report, &native, 2125.0).as_deref(),
            Some("37s")
        );
        // Only a confirmed start and end count; a start alone or a reversed
        // pair stays unknown, and a live child never borrows an end.
        native["state"] = json!("failed");
        assert_eq!(
            worker_elapsed(&report, &native, 9e3).as_deref(),
            Some("37s")
        );
        // Live row with only a verified start: age, not duration.
        native["ended_at"] = json!(null);
        for (state, observed, label) in [
            ("working", 1180.0, Some("3m ago")),
            ("unknown", 1012.0, Some("12s ago")),
            ("running", 1000.0 + 3600.0 * 5.0 + 300.0, Some("5h05m ago")),
            ("running", 1000.0 + 86400.0 * 120.0, Some("~120d ago")),
            ("working", 999.0, None),
        ] {
            native["state"] = json!(state);
            native["started_at"] = json!(1000);
            let shown = worker_elapsed(&report, &native, observed);
            assert_eq!(shown.as_deref(), label, "{state}");
        }
        native["state"] = json!("working");
        native["started_at"] = json!("bad");
        assert!(worker_elapsed(&report, &native, 1180.0).is_none());
        native["started_at"] = json!(1000);
        for stale in [("collection", "pending"), ("capacity_stale", "")] {
            let mut other = report.clone();
            if stale.1.is_empty() {
                other[stale.0] = json!(true);
            } else {
                other[stale.0][stale.1] = json!(true);
            }
            assert!(worker_elapsed(&other, &native, 1180.0).is_none());
        }
        native["state"] = json!("done");
        for (state, started, ended) in [
            ("done", json!(1000), json!(null)),
            ("done", json!(null), json!(1037)),
            ("done", json!(1037), json!(1000)),
            ("done", json!(1000), json!("bad")),
            ("working", json!(1000), json!(1037)),
        ] {
            native["state"] = json!(state);
            native["started_at"] = started;
            native["ended_at"] = ended;
            assert!(
                worker_elapsed(&report, &native, 2125.0).is_none(),
                "{native}"
            );
        }
    }

    #[test]
    fn native_response_total_has_response_provenance_and_never_invents_a_total() {
        let rendered = |call: Value, limit, columns| {
            token_span(&call, limit, columns, true)
                .iter()
                .map(|span| span.plain.as_str())
                .collect::<String>()
        };
        let child = |response: Value| json!({"native_child":true,"native_response_usage":{"response":response}});
        let wide = rendered(child(json!({"total_tokens":12_500})), 40, 100);
        assert!(
            wide.ends_with("  12.5K rsp") && !wide.contains("tok"),
            "{wide}"
        );
        assert_eq!(
            rendered(child(json!({"total_tokens":1_200_000})), 40, 100).trim(),
            "1.2M rsp"
        );
        // No unit fits below 56 columns, so the marker slot names the source.
        assert_eq!(
            rendered(child(json!({"total_tokens":12_500})), 12, 50),
            "r 12.5K"
        );
        // A known cumulative total keeps its own source; managed rows and
        // non-native rows never read response counters.
        let mut session = child(json!({"total_tokens":7}));
        session["native_session_tokens"] = json!({"total":9_000});
        assert!(rendered(session, 40, 100).ends_with("  9.0K tok"));
        let managed = json!({"receipt_tokens":{"total":300_000},
            "native_response_usage":{"response":{"total_tokens":7}}});
        assert!(rendered(managed, 40, 100).ends_with("300.0K tok"));
        let foreign = json!({"native_response_usage":{"response":{"total_tokens":7}}});
        assert!(rendered(foreign, 40, 100).ends_with("— tok"));
        // Absent, invalid, lead and cumulative counters stay unknown.
        for response in [
            json!(null),
            json!({}),
            json!({"total_tokens":-1}),
            json!({"total_tokens":1.5}),
            json!({"total_tokens":"12"}),
            json!({"thread_total_tokens":7,"turn_total_tokens":7}),
        ] {
            let line = rendered(child(response.clone()), 40, 100);
            assert!(line.ends_with("— tok"), "{response}: {line}");
        }
        // Input/output alone show separately, with no sum and no total.
        let split = rendered(
            child(json!({"input_tokens":14_000,"output_tokens":5})),
            80,
            100,
        );
        assert!(
            split.contains("— tok") && split.contains("rsp in 14.0K out 5"),
            "{split}"
        );
        assert!(!split.contains("14.0K tok"), "{split}");
        let output = rendered(
            child(json!({"output_tokens":2_000,"input_tokens":"x"})),
            80,
            100,
        );
        assert!(
            output.contains("rsp out 2.0K") && !output.contains(" in "),
            "{output}"
        );
        // A total wins over any split and is not recomputed from it.
        let both = rendered(
            child(json!({"total_tokens":99,"input_tokens":14_000,"output_tokens":5})),
            80,
            100,
        );
        assert!(both.ends_with("99 rsp") && !both.contains(" in "), "{both}");
        // The selected Call Detail lists the same response counters as whole
        // `N label` parts on bounded rows and leaves the Tokens line as it was.
        let detail = |call: &Value, columns: usize| {
            let lines =
                render_detail_card(Some(call), columns, columns - 4, true, false, usize::MAX);
            assert!(
                lines.iter().skip(1).all(|line| width(line) == columns),
                "{lines:#?}"
            );
            let bodies: Vec<&str> = lines
                .iter()
                .skip(1)
                .filter_map(|line| line.strip_prefix("│ ")?.strip_suffix(" │"))
                .collect();
            let tokens = bodies.iter().find_map(|body| {
                let rest = body.strip_prefix("Tokens ")?;
                Some(rest.trim().to_owned())
            });
            let start = bodies
                .iter()
                .position(|body| body.starts_with("Last response"));
            let rows = start.map(|start| {
                let first = &bodies[start]["Last response".len()..];
                let rest = bodies[start + 1..]
                    .iter()
                    .take_while(|body| body.starts_with(' '));
                [first]
                    .into_iter()
                    .chain(rest.copied())
                    .map(|row| {
                        row.split(" · ")
                            .map(str::trim)
                            .filter(|part| !part.is_empty())
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            });
            (tokens, rows)
        };
        let unreported = Some("unreported".to_owned());
        let codex = json!({"total_tokens":12_500,"input_tokens":14_000,
            "cached_input_tokens":9_000,"cache_write_input_tokens":0,"output_tokens":5,
            "reasoning_output_tokens":3});
        let claude = json!({"input_tokens":3,"output_tokens":700,
            "cache_creation_input_tokens":1_200,"cache_read_input_tokens":20_000});
        let agy = json!({"input_tokens":1_200,"cached_input_tokens":800,"output_tokens":40});
        let both_keys = json!({"cached_input_tokens":9_000,"cache_read_input_tokens":20_000,
            "cache_creation_input_tokens":1,"cache_write_input_tokens":2});
        let cases = [
            (json!({"total_tokens":12_500}), vec!["12500 total"]),
            (
                json!({"total_tokens":12_500,"input_tokens":14_000,"output_tokens":5}),
                vec!["12500 total", "14000 in", "5 out"],
            ),
            (
                json!({"input_tokens":14_000,"output_tokens":5}),
                vec!["14000 in", "5 out"],
            ),
            (
                json!({"total_tokens":0,"output_tokens":0,"input_tokens":"x"}),
                vec!["0 total", "0 out"],
            ),
            (
                codex.clone(),
                vec![
                    "12500 total",
                    "14000 in",
                    "5 out",
                    "9000 cache read",
                    "0 cache write",
                    "3 reasoning",
                ],
            ),
            (
                json!({"total_tokens":20,"input_tokens":10,"cached_input_tokens":0,"output_tokens":10}),
                vec!["20 total", "10 in", "10 out", "0 cache read"],
            ),
            (
                claude.clone(),
                vec!["3 in", "700 out", "20000 cache read", "1200 cache write"],
            ),
            (
                json!({"input_tokens":3,"output_tokens":700,"cache_creation_input_tokens":0}),
                vec!["3 in", "700 out", "0 cache write"],
            ),
            (agy.clone(), vec!["1200 in", "40 out", "800 cache read"]),
            (
                json!({"input_tokens":1_200,"cached_input_tokens":0,"output_tokens":40}),
                vec!["1200 in", "40 out", "0 cache read"],
            ),
            (
                json!({"input_tokens":1_200,"output_tokens":40}),
                vec!["1200 in", "40 out"],
            ),
            (
                json!({"input_tokens":5,"cached_input_tokens":-1,"cache_read_input_tokens":1.5,
                    "cache_write_input_tokens":"12","reasoning_output_tokens":null}),
                vec!["5 in"],
            ),
            (
                both_keys.clone(),
                vec![
                    "9000 cached input",
                    "20000 cache read input",
                    "1 cache creation input",
                    "2 cache write input",
                ],
            ),
        ];
        for (response, expected) in &cases {
            let values: Vec<u64> = response
                .as_object()
                .unwrap()
                .values()
                .filter_map(Value::as_u64)
                .collect();
            for columns in [100, 44, 30] {
                let (tokens, rows) = detail(&child(response.clone()), columns);
                let at = format!("{response} at {columns}: {rows:?}");
                // The response takes the slot of a Tokens row saying "unreported".
                assert_eq!(tokens, None, "{at}");
                let rows = rows.expect(&at);
                let parts: Vec<&str> = rows.iter().flatten().map(String::as_str).collect();
                let (marker, shown): (Vec<&str>, Vec<&str>) =
                    parts.iter().partition(|part| part.starts_with('+'));
                let more = match marker[..] {
                    [] => 0,
                    [marker] => marker
                        .strip_prefix('+')
                        .and_then(|rest| rest.strip_suffix(" more"))
                        .and_then(|count| count.parse().ok())
                        .expect(&at),
                    _ => panic!("{at}"),
                };
                // Shown parts keep priority order; the marker counts the rest.
                assert!(!shown.is_empty() && rows.len() <= 4, "{at}");
                assert_eq!(shown[..], expected[..shown.len()], "{at}");
                assert_eq!(shown.len() + more, expected.len(), "{at}");
                let one_row = width(&expected.join(" · ")) + 14 <= columns - 4;
                assert_eq!(rows.len() == 1, one_row, "{at}");
                if response.get("total_tokens").is_none() {
                    assert!(parts.iter().all(|part| !part.contains("total")), "{at}");
                }
                for (i, a) in values.iter().enumerate() {
                    for b in &values[i + 1..] {
                        let sum = format!("{} ", a + b);
                        let invented = !values.contains(&(a + b));
                        assert!(
                            !(invented && parts.iter().any(|p| p.starts_with(&sum))),
                            "{at}"
                        );
                    }
                }
            }
        }
        // Every provider shape fits one row at normal width and splits narrow.
        for response in [&codex, &claude, &agy] {
            let rows = |columns| detail(&child(response.clone()), columns).1.unwrap().len();
            assert_eq!(rows(100), 1, "{response}");
            assert!(rows(44) > 1 && rows(30) > 1, "{response}");
        }
        // The fourth row ends with an exact count of the parts it could not
        // hold, and a part too wide beside the label starts the next row.
        let codex_narrow = detail(&child(codex.clone()), 30).1.unwrap();
        assert_eq!(codex_narrow.len(), 4);
        assert_eq!(codex_narrow[3].last().map(String::as_str), Some("+1 more"));
        let keys_narrow = detail(&child(both_keys), 30).1.unwrap();
        assert!(keys_narrow[0].is_empty(), "{keys_narrow:?}");
        assert_eq!(keys_narrow[3], ["+2 more"]);
        // A known part wider than any row is counted, never silently dropped.
        let wide_part = json!({"input_tokens":5,"cache_creation_input_tokens":20_000});
        let (tokens, rows) = detail(&child(wide_part), 22);
        let parts: Vec<String> = rows.into_iter().flatten().flatten().collect();
        assert_eq!(tokens, None);
        assert!(parts.contains(&"5 in".to_owned()), "{parts:?}");
        assert!(parts.contains(&"+1 more".to_owned()), "{parts:?}");
        assert!(
            !parts.iter().any(|part| part.contains("20000")),
            "{parts:?}"
        );
        // The board row keeps its own compact split and never reads components.
        assert!(rendered(child(agy), 80, 100).contains("— tok  rsp in 1.2K out 40"));
        assert!(rendered(child(claude), 80, 100).contains("— tok  rsp in 3 out 700"));
        // Too narrow for the label: omitted rather than shown unlabelled.
        let tiny = detail(&child(json!({"input_tokens":14_000,"output_tokens":5})), 14);
        assert_eq!(tiny, (None, None));
        let mut session = child(json!({"total_tokens":7}));
        session["native_session_tokens"] = json!({"total":9_000});
        for columns in [100, 44] {
            assert_eq!(
                detail(&session, columns),
                (
                    Some("9000 · Session incl. cache".to_owned()),
                    Some(vec![vec!["7 total".to_owned()]])
                )
            );
        }
        let managed = json!({"receipt_tokens":{"total":300_000},
            "native_response_usage":{"response":{"total_tokens":7,"cached_input_tokens":7}}});
        let foreign = json!({"native_response_usage":{"response":{"cache_read_input_tokens":7}}});
        assert_eq!(detail(&managed, 100).1, None);
        assert_eq!(detail(&foreign, 100), (unreported.clone(), None));
        for response in [
            json!(null),
            json!({}),
            json!({"total_tokens":-1,"input_tokens":1.5,"output_tokens":"12"}),
            json!({"thread_total_tokens":7,"turn_total_tokens":7}),
            json!({"cached_input_tokens":-1,"cache_creation_input_tokens":"3"}),
        ] {
            for columns in [100, 44, 30] {
                let shown = detail(&child(response.clone()), columns);
                assert_eq!(shown, (unreported.clone(), None), "{response}");
            }
        }
        // The selected Board Detail spends only the viewport's rows: at 30×10
        // under the pinned Main and Goal, every known part is shown or counted.
        let mut report = top_bar_report();
        report["native_children"] = json!([{"participant":"native-codex-r1","native_child":true,
            "provider":"codex","role":"subagent","parent":"M","state":"working",
            "native_response_usage":{"response":codex}}]);
        let selected = visible(&report, &Default::default())
            .iter()
            .position(|call| call["participant"] == "native-codex-r1")
            .unwrap();
        let nav = navigation::Navigation {
            selected,
            detail: true,
            ..Default::default()
        };
        let known = [
            "12500 total",
            "14000 in",
            "5 out",
            "9000 cache read",
            "? cache write",
            "3 reasoning",
        ];
        // The first response row reuses the "Tokens unreported" slot, so Role
        // stays visible at 30×10 and every counter is still counted there.
        for (columns, height, visible, counted) in [(30, 10, 0, 6), (100, 30, 6, 0)] {
            let board = frame(&report, &nav, columns, height, &BTreeMap::new());
            let at = board.join("\n");
            assert!(board.len() <= height, "{at}");
            assert!(at.contains("Goal") && at.contains("Last response"), "{at}");
            assert!(board.iter().any(|line| line.contains("Role")), "{at}");
            assert!(
                !board
                    .iter()
                    .any(|line| line.contains("Tokens") && line.contains("unreported")),
                "{at}"
            );
            let shown = known.iter().filter(|part| at.contains(*part)).count();
            let more: usize = at
                .match_indices('+')
                .filter_map(|(index, _)| {
                    let rest = &at[index + 1..];
                    let digits = rest.chars().take_while(char::is_ascii_digit).count();
                    rest[digits..]
                        .starts_with(" more")
                        .then(|| rest[..digits].parse::<usize>().ok())?
                })
                .sum();
            assert_eq!((shown, more), (visible, counted), "{at}");
            assert_eq!(shown + more, known.len(), "{at}");
        }
        // Codex maps omitted cache/reasoning detail to 0, so Call Detail renders
        // those as `?` while keeping explicit total/in/out 0s, positive values,
        // and AGY/Claude zero detail semantics intact.
        let codex_zeros = json!({"participant":"native-codex-z0","native_child":true,
            "provider":"codex","native_response_usage":{"response":{
                "total_tokens":0,"input_tokens":0,"output_tokens":0,
                "cached_input_tokens":0,"cache_write_input_tokens":0,
                "reasoning_output_tokens":0}}});
        let codex_pos = json!({"participant":"native-codex-p1","native_child":true,
            "provider":"codex","native_response_usage":{"response":{
                "total_tokens":19,"input_tokens":14,"output_tokens":5,
                "cached_input_tokens":7,"cache_write_input_tokens":3,
                "reasoning_output_tokens":2}}});
        let agy_zero = json!({"participant":"native-antigravity-z0","native_child":true,
            "provider":"antigravity","native_response_usage":{"response":{
                "input_tokens":1_200,"cached_input_tokens":0,"output_tokens":40}}});
        let claude_zero = json!({"participant":"native-claude-z0","native_child":true,
            "provider":"claude","native_response_usage":{"response":{
                "input_tokens":3,"output_tokens":700,
                "cache_read_input_tokens":0,"cache_creation_input_tokens":0}}});
        for (entry, expected) in [
            (
                codex_zeros,
                vec![
                    "0 total",
                    "0 in",
                    "0 out",
                    "? cache read",
                    "? cache write",
                    "? reasoning",
                ],
            ),
            (
                codex_pos,
                vec![
                    "19 total",
                    "14 in",
                    "5 out",
                    "7 cache read",
                    "3 cache write",
                    "2 reasoning",
                ],
            ),
            (agy_zero, vec!["1200 in", "40 out", "0 cache read"]),
            (
                claude_zero,
                vec!["3 in", "700 out", "0 cache read", "0 cache write"],
            ),
        ] {
            let (tokens, rows) = detail(&entry, 120);
            assert_eq!(tokens, None, "{entry}");
            let parts: Vec<String> = rows
                .unwrap_or_else(|| panic!("{entry}"))
                .into_iter()
                .flatten()
                .collect();
            assert_eq!(parts, expected, "{entry}");
        }
    }

    #[test]
    fn main_tokens_use_only_the_owner_rollout_and_label_the_session_total() {
        let mut report = top_bar_report();
        report["room"]["owner"] = json!("M");
        report["room"]["participants"][0]["provider"] = json!("codex");
        report["room"]["participants"][0]["joined"] = json!(true);
        report["room"]["participants"][0]["attempt"] = json!("main-1");
        report["room_attempts"]["main-1"] = json!({
            "attempt_id":"main-1", "tool":"panel-main", "provider":"codex", "state":"working",
            "refs":{"panel_room_id":"room-x","panel_room_participant":"M"},
            "usage":{"tokens":12500},"usage_reports":{"tokens":1}
        });
        report["main_token_observation"] = json!({
            "room":"room-x","participant":"M","attempt":"main-1",
            "total":1_025_123_456u64,"source":"rollout"
        });
        let read = BTreeMap::new();
        let paint = |r: &Value, detail| {
            let nav = navigation::Navigation {
                detail,
                ..Default::default()
            };
            frame(r, &nav, 100, 24, &read).join("\n")
        };
        assert!(paint(&report, false).contains("1.0B tok"));
        assert!(paint(&report, true).contains("1025123456 · Session incl. cache"));
        let mut foreign = report.clone();
        foreign["main_token_observation"]["room"] = json!("elsewhere");
        assert!(!paint(&foreign, false).contains("1.0B tok"));
        assert!(paint(&foreign, true).contains("12500 · Events"));
        report["collection"]["pending"] = json!(true);
        assert!(paint(&report, false).contains("~  1.0B tok"));
    }

    #[test]
    fn attempt_tokens_preserve_the_short_board_and_identify_the_detail_source() {
        let mut report = top_bar_report();
        report["room"]["participants"][1]["attempt"] = json!("cmp-1");
        report["room_attempts"]["cmp-1"] = json!({
            "attempt_id":"cmp-1", "task_id":"t1", "provider":"codex",
            "model":"gpt-x", "state":"working", "title":"Top bar",
            "refs":{"panel_room_id":"room-x", "panel_room_participant":"W1"},
            "usage":{"tokens":12500}, "usage_reports":{"tokens":1}
        });
        let read = BTreeMap::new();
        let nav = navigation::Navigation {
            selected: 1,
            ..Default::default()
        };
        // A 13-row interactive terminal reserves four rows for the dock by default.
        for height in [13, 13 - 4] {
            let board = board_frame(&report, &nav, 80, height, &read, false, true);
            let (&row, hit) = board
                .worker_rows
                .iter()
                .find(|(_, (id, _))| id == "W1")
                .expect("selectable call");
            assert_eq!(hit, &("W1".to_owned(), "cmp-1".to_owned()));
            assert_eq!(board.lines.len(), height);
            for label in ["▸", "Top bar", "gpt-x", "12.5K tok"] {
                assert!(board.lines[row].contains(label), "{:#?}", board.lines);
            }
            let (&main, _) = board.worker_rows.first_key_value().unwrap();
            let builder = &board.lines[row];
            let main = &board.lines[main];
            let numeral_column = |line: &str, numeral: &str| {
                width(&line[..line.rfind(numeral).unwrap()]) + width(numeral)
            };
            assert_eq!(
                numeral_column(builder, "12.5") + 1,
                numeral_column(main, "—")
            );
            assert!(board.weekly_usage.is_some());
        }

        let nav = navigation::Navigation {
            detail: true,
            ..nav
        };
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        for label in ["Call Detail", "Tokens", "12500", "Events"] {
            assert!(detail.contains(label), "{detail}");
        }

        for (collection, stale) in [
            (json!({"ok": true, "pending": true}), false),
            (json!({"ok": false, "error": "fixture failure"}), true),
        ] {
            report["collection"] = collection;
            report["capacity_stale"] = json!(stale);
            let board = frame(&report, &Default::default(), 120, 24, &read).join("\n");
            assert!(board.contains("~ 12.5K tok"), "{board}");
            let detail = frame(&report, &nav, 80, 13, &read).join("\n");
            assert!(
                detail.contains("12500 · Events · last observed"),
                "{detail}"
            );
            let narrow = frame(&report, &nav, 24, 13, &read).join("\n");
            assert!(narrow.contains("Tokens last 12500 Events"), "{narrow}");
            let narrower = frame(&report, &nav, 23, 13, &read).join("\n");
            assert!(!narrower.contains("Tokens"), "{narrower}");
        }

        report["collection"] = json!({"ok":true});
        report["capacity_stale"] = json!(false);
        report["board_receipt_tokens"]["cmp-1"] = json!({
            "total":300, "reported":2, "calls":2,
            "providers": {
                "codex":{"input":150,"output":20,"cache_read":40,"cache_write":null,
                    "calls":2,"reported":{"input":2,"output":2,"cache_read":2,"cache_write":0}},
                "claude":{"input":50,"output":15,"cache_read":65,"cache_write":0,
                    "calls":1,"reported":{"input":1,"output":1,"cache_read":1,"cache_write":1}}
            }
        });
        for height in [13, 9] {
            let board = board_frame(&report, &Default::default(), 80, height, &read, false, true);
            let (&row, _) = board
                .worker_rows
                .iter()
                .find(|(_, (id, _))| id == "W1")
                .unwrap();
            assert!(board.lines[row].contains("300 tok"), "{:#?}", board.lines);
            assert!(!board.lines[row].contains("12500"));
        }
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        for label in [
            "300 · Receipts",
            "2/2 call totals",
            "In 150 · Out 20 · Cache read 40 · Write ?",
            "In 50 · Out 15 · Cache read 65 · Write 0",
        ] {
            assert!(detail.contains(label), "{detail}");
        }

        let codex = &mut report["board_receipt_tokens"]["cmp-1"]["providers"]["codex"];
        codex["cache_read"] = Value::Null;
        codex["reported"]["cache_read"] = json!(1);
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(
            detail.contains("In 150 · Out 20 · Cache read partial 1/2 · Write ?"),
            "{detail}"
        );
        let codex = &mut report["board_receipt_tokens"]["cmp-1"]["providers"]["codex"];
        codex["input"] = json!(0);
        codex["output"] = Value::Null;
        codex["reported"]["output"] = json!(1);
        codex["reported"]["cache_write"] = json!(2);
        codex["reported"]["input"] = json!(0);
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(
            detail.contains("In 0 · Out partial 1/2 · Cache read partial 1/2 · Write ?"),
            "{detail}"
        );
        for columns in [43, 47] {
            let detail = frame(&report, &nav, columns, 13, &read);
            assert!(
                detail
                    .iter()
                    .any(|row| row.contains("In 0 Out p1/2 Read p1/2 Write ?")),
                "{columns}: {detail:?}"
            );
            assert!(detail.iter().all(|row| width(row) <= columns));
        }
        let codex = &mut report["board_receipt_tokens"]["cmp-1"]["providers"]["codex"];
        codex["input"] = Value::Null;
        codex["reported"]["input"] = json!(1);
        codex["reported"]["cache_write"] = json!(1);
        codex["cache_write"] = Value::Null;
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(
            detail.contains("In p1/2 · Out p1/2 · Cache read p1/2 · Write p1/2"),
            "{detail}"
        );
        for columns in [43, 47] {
            let detail = frame(&report, &nav, columns, 13, &read);
            assert!(
                detail
                    .iter()
                    .any(|row| row.contains("In p1/2 Out p1/2 Read p1/2 Write p1/2")),
                "{columns}: {detail:?}"
            );
        }
        let codex = &mut report["board_receipt_tokens"]["cmp-1"]["providers"]["codex"];
        *codex = json!({"input":150,"output":20,"cache_read":40,"cache_write":null});
        report["board_receipt_tokens"]["cmp-1"]["total"] = Value::Null;
        report["board_receipt_tokens"]["cmp-1"]["reported"] = json!(1);
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(detail.contains("12500 · Events"), "{detail}");
        assert!(detail.contains("1/2 call totals"), "{detail}");

        for (collection, stale) in [
            (json!({"ok":true,"pending":true}), false),
            (json!({"ok":false}), true),
        ] {
            report["collection"] = collection;
            report["capacity_stale"] = json!(stale);
            let detail = frame(&report, &nav, 80, 13, &read).join("\n");
            assert!(
                detail.contains("12500 · Events · last observed"),
                "{detail}"
            );
            assert!(
                detail.contains("1/2 call totals · last observed"),
                "{detail}"
            );
        }
        report["board_receipt_tokens"]["cmp-1"]["reported"] = json!(2);
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(
            detail.contains("12500 · Events · last observed"),
            "{detail}"
        );
        assert!(
            detail.contains("2/2 call totals · sum unavailable · last observed"),
            "{detail}"
        );
        let detail = frame(&report, &nav, 54, 13, &read).join("\n");
        assert!(
            !detail.contains("In 50") && !detail.contains("In 150"),
            "{detail}"
        );

        report["collection"] = json!({"ok":true});
        report["capacity_stale"] = json!(false);
        report["board_receipt_tokens"]["cmp-1"]["providers"] = json!({
            "codex":{"launch_accounts":[
                {"alias":"seat-a", "calls":1, "input":0, "output":0, "cache_read":0,
                    "cache_write":null, "reasoning":null, "total":0},
                {"alias":null, "calls":2, "input":null, "output":5, "cache_read":null,
                    "cache_write":null, "reasoning":null, "total":null,
                    "reported":{"input":1,"total":1}}
            ]}
        });
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        for label in [
            "Launch receipts",
            "Codex @seat-a",
            "In 0 · Out 0 · Cache 0/? · Rsn ? · Total 0",
            "Codex @?",
            "In p1/2 · Out 5",
            "Total p1/2",
        ] {
            assert!(detail.contains(label), "{label}: {detail}");
        }
        report["board_receipt_tokens"]["cmp-1"]["providers"]["codex"]["launch_accounts"][0]["reasoning"] =
            json!(6);
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(detail.contains("Rsn 6 · Total 0"), "{detail}");
        report["board_receipt_tokens"]["cmp-1"]["providers"]["codex"]["launch_accounts"][0]["reasoning"] =
            json!(null);
        let narrow = frame(&report, &nav, 43, 13, &read);
        assert!(
            narrow.iter().any(
                |row| row.contains("Codex @seat-a") && row.contains("In 0 Out 0 R0 W? Rsn? T0")
            ),
            "{narrow:?}"
        );
        assert!(
            narrow
                .iter()
                .any(|row| row.contains("Codex @?")
                    && row.contains("In p1/2 Out 5 R? W? Rsn? Tp1/2")),
            "{narrow:?}"
        );
        assert!(narrow.iter().all(|row| width(row) <= 43));
        report["board_receipt_tokens"]["cmp-1"]["total"] = json!(0);
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(detail.contains("0 · Receipts"), "{detail}");
        report
            .as_object_mut()
            .unwrap()
            .remove("board_receipt_tokens");
        let detail = frame(&report, &nav, 80, 13, &read).join("\n");
        assert!(detail.contains("12500 · Events"), "{detail}");
        assert!(!detail.contains("call totals"), "{detail}");
    }

    #[test]
    fn attempt_tokens_stay_with_their_room_participant_and_attempt() {
        let mut report = top_bar_report();
        let worker = json!({
            "attempt_id":"cmp-1", "provider":"codex", "state":"working",
            "refs":{"panel_room_id":"room-x", "panel_room_participant":"W1"},
            "usage":{"tokens":12500}, "usage_reports":{"tokens":1}
        });
        report["attempts"]["recent"] = json!([worker.clone()]);
        report["room_attempts"] = json!({
            "M":{
                "attempt_id":"M", "tool":"panel-main", "provider":"claude", "state":"working",
                "refs":{"panel_room_id":"room-x"},
                "usage":{"tokens":345}, "usage_reports":{"tokens":1}
            },
            "lead-1":{
                "attempt_id":"lead-1", "provider":"codex", "state":"working",
                "refs":{"panel_room_id":"room-x", "panel_room_participant":"L1"},
                "usage":{"tokens":0}, "usage_reports":{"tokens":1}
            },
            "cmp-1":worker,
            "cmp-2":{
                "attempt_id":"cmp-2", "provider":"codex", "state":"done",
                "refs":{"panel_room_id":"room-x", "panel_room_participant":"W2"},
                "usage":{"tokens":0}, "usage_reports":{"tokens":0}
            },
            "unbound":{
                "attempt_id":"unbound", "provider":"codex", "state":"working",
                "refs":{"panel_room_id":"room-x"},
                "usage":{"tokens":320}, "usage_reports":{"tokens":1}
            },
        });
        report["room"]["participants"][2]["attempt"] = json!("cmp-2");
        report["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "participant":"L1", "attempt":"lead-1", "role":"lead", "provider":"codex"
            }));
        report["native_children"] = json!([{
            "participant":"native-worker", "attempt":"cmp-1", "role":"worker", "parent":"W1",
            "event_tokens":{"total":12500, "last_observed":false},
            "receipt_tokens":{"total":59, "last_observed":false},
            "usage":{"tokens":12500}, "usage_reports":{"tokens":1}
        }]);
        let detail = |report: &Value, participant: &str| {
            let rows = calls(report);
            let call = rows
                .iter()
                .find(|row| row["participant"] == participant)
                .unwrap();
            render_detail_card(Some(call), 80, 76, true, false, usize::MAX)
                .into_iter()
                .find(|line| line.contains("Tokens"))
                .unwrap()
        };
        for (participant, label) in [
            ("M", "345 · Events"),
            ("L1", "0 · Events"),
            ("W1", "12500 · Events"),
            ("W2", "unreported"),
            ("unbound", "unreported"),
            ("native-worker", "unreported"),
        ] {
            let shown = detail(&report, participant);
            assert!(shown.contains(label), "{participant}: {shown}");
        }
        let main = calls(&report);
        let main = main.iter().find(|row| row["participant"] == "M").unwrap();
        assert!(main_summary(main).contains(&("Tokens", "345 · Events".to_owned())));
        let board = frame(&report, &Default::default(), 120, 24, &BTreeMap::new());
        assert_eq!(
            board
                .iter()
                .filter(|line| line.contains("   345 tok"))
                .count(),
            1
        );

        report["board_receipt_tokens"]["cmp-1"] = json!({"total":59,"calls":1,"reported":1,
            "providers":{"codex":{"launch_accounts":[{"alias":"isolated","total":59,"calls":1}]}}});
        report["board_receipt_tokens"]["foreign-room"] = json!({"total":999,"calls":1,"reported":1,
            "providers":{"codex":{"launch_accounts":[{"alias":"foreign","total":999,"calls":1}]}}});
        assert!(detail(&report, "W1").contains("59 · Receipts"));
        let call_rows = calls(&report);
        let own = call_rows
            .iter()
            .find(|row| row["participant"] == "W1")
            .unwrap();
        assert_eq!(
            own["receipt_tokens"]["providers"]["codex"]["launch_accounts"][0]["alias"],
            "isolated"
        );
        assert!(!json!(call_rows).to_string().contains("foreign"));
        assert!(detail(&report, "W2").contains("unreported"));
        assert!(detail(&report, "native-worker").contains("unreported"));

        let mut ambiguous = report.clone();
        let mut older = ambiguous["room_attempts"]["cmp-1"].clone();
        older["attempt_id"] = json!("cmp-0");
        older["usage"]["tokens"] = json!(400);
        ambiguous["room_attempts"]["cmp-0"] = older;
        assert!(detail(&ambiguous, "W1").contains("unreported"));
        ambiguous["room"]["call_results"]["W1"]["attempt_id"] = json!("cmp-1");
        assert!(detail(&ambiguous, "W1").contains("unreported"));
        ambiguous["room_attempts"]["W1"] = ambiguous["room_attempts"]["cmp-1"].clone();
        assert!(detail(&ambiguous, "W1").contains("59 · Receipts"));

        let mut new_attempt = report.clone();
        new_attempt["room"]["call_results"]["W1"]["attempt_id"] = json!("cmp-1");
        new_attempt["room"]["participants"][1]["attempt"] = json!("cmp-new");
        assert!(detail(&new_attempt, "W1").contains("unreported"));
        new_attempt["room_attempts"]["cmp-new"] = report["room_attempts"]["cmp-1"].clone();
        new_attempt["room_attempts"]["cmp-new"]["attempt_id"] = json!("cmp-new");
        new_attempt["room_attempts"]["cmp-new"]["usage"]["tokens"] = json!(30);
        assert!(detail(&new_attempt, "W1").contains("30 · Events"));

        let mut reenrolled = report.clone();
        reenrolled["room"]["participants"][1]["initial_seq"] = json!(2);
        reenrolled["room"]["participants"][1]["seq"] = json!(3);
        assert!(detail(&reenrolled, "W1").contains("unreported"));
        reenrolled["room"]["call_results"]["W1"] = json!({"attempt_id":"cmp-1", "seq":2});
        assert!(detail(&reenrolled, "W1").contains("unreported"));
        reenrolled["room"]["call_results"]["W1"]["seq"] = json!(1000);
        assert!(detail(&reenrolled, "W1").contains("unreported"));
        reenrolled["room"]["participants"][1]["attempt"] = json!("cmp-new");
        reenrolled["room_attempts"]["cmp-new"] = new_attempt["room_attempts"]["cmp-new"].clone();
        assert!(detail(&reenrolled, "W1").contains("30 · Events"));

        let mut released = report.clone();
        released["room"]["swaps"] = json!([{"participant":"M","attempt":"M"}]);
        assert!(detail(&released, "M").contains("unreported"));
        let mut parent = report.clone();
        parent["room_attempts"]["M"]["refs"]["panel_role"] = json!("worker");
        assert!(detail(&parent, "M").contains("unreported"));
        parent["room"]["participants"][1]["attempt"] = json!("cmp-1");
        parent["room_attempts"]["cmp-1"]["tool"] = json!("panel-main");
        assert!(detail(&parent, "W1").contains("unreported"));

        // An exact attempt ID alone does not establish a Room or participant.
        for (key, value) in [
            ("panel_room_participant", "sibling"),
            ("panel_room_id", "room-other"),
            ("panel_room", "room-other"),
        ] {
            let mut conflicting = report.clone();
            conflicting["room"]["participants"][1]["attempt"] = json!("cmp-1");
            conflicting["room_attempts"]["cmp-1"]["refs"][key] = json!(value);
            assert!(detail(&conflicting, "W1").contains("unreported"), "{key}");
        }
        let mut wrong_room = report.clone();
        wrong_room["room"]["id"] = json!("room-other");
        assert!(detail(&wrong_room, "W1").contains("unreported"));
        report["collection"] = json!({"ok":false});
        assert!(detail(&report, "W1").contains("unreported"));
    }

    #[test]
    fn short_weekly_strip_reserves_notices_and_a_selectable_call() {
        let mut report = top_bar_report();
        report["collection"]["ok"] = json!(false);
        report["provider_status"]["codex"] = json!({"weekly_used":25});
        let nav = navigation::Navigation {
            view: View::Compact,
            filter: "Top".into(),
            editing: true,
            notice: Some(format!("First {}Second notice", "a".repeat(72))),
            ..Default::default()
        };
        for (height, weekly) in [(13, true), (12, true), (11, true), (10, false)] {
            for fill in [false, true] {
                let board = board_frame(&report, &nav, 80, height, &BTreeMap::new(), false, fill);
                assert!(board.lines.len() <= height, "{:#?}", board.lines);
                if fill {
                    assert_eq!(board.lines.len(), height);
                }
                assert_eq!(board.weekly_usage.is_some(), weekly, "{:#?}", board.lines);
                let mut lines = board.lines.iter();
                for prefix in ["! STALE", "Filter /Top", "› First", "› Second notice"] {
                    assert!(
                        lines.by_ref().any(|line| line.starts_with(prefix)),
                        "{prefix}: {:#?}",
                        board.lines
                    );
                }
                let (&row, _) = board
                    .worker_rows
                    .iter()
                    .find(|(_, (id, _))| id == "W1")
                    .expect("selectable call");
                assert!(row < board.lines.len() - 1 && board.lines[row].contains("Top bar"));
                assert!(board.lines.last().unwrap().contains("Enter"));
                assert!(
                    board
                        .lines
                        .iter()
                        .any(|line| line.contains("Goal: Ship top bar"))
                );
            }
        }
        for nav in [
            navigation::Navigation {
                help: true,
                ..nav.clone()
            },
            navigation::Navigation {
                health: true,
                ..nav.clone()
            },
            navigation::Navigation {
                detail: true,
                ..nav.clone()
            },
        ] {
            let board = board_frame(&report, &nav, 80, 13, &BTreeMap::new(), false, true);
            assert!(board.weekly_usage.is_none());
            assert!(
                board
                    .lines
                    .iter()
                    .all(|line| !line.starts_with("Weekly left"))
            );
        }
    }

    #[test]
    fn clipped_frames_keep_hits_on_painted_rows() {
        let mut participants = Vec::new();
        for (n, provider) in ["codex", "claude", "codex", "antigravity", "codex", "claude"]
            .into_iter()
            .enumerate()
        {
            participants.push(json!({"participant":format!("W{n}"), "role":"worker",
                "provider":provider, "state":"working", "title":format!("Task{n}")}));
        }
        let report = json!({"collection":{"ok":true}, "room":{"id":"room", "participants":participants},
            "provider_sessions":["codex", "claude", "antigravity"]});
        let read = BTreeMap::new();
        let mut checked = (0, 0);
        for (view, notice) in [
            (View::Auto, None),
            (View::Compact, None),
            (View::Detail, Some("Dispatch queued for review".to_owned())),
        ] {
            let nav = navigation::Navigation {
                view,
                notice,
                editing: view == View::Detail,
                ..Default::default()
            };
            for columns in [40, 80, 120] {
                for height in 3..=16 {
                    for fill in [false, true] {
                        let board = board_frame(&report, &nav, columns, height, &read, false, fill);
                        let painted = board.lines.len() - 1;
                        assert!(painted < height, "{columns}x{height}: {:#?}", board.lines);
                        if fill {
                            assert_eq!(board.lines.len(), height);
                        }
                        for row in board.provider_rows.into_iter().flatten() {
                            assert!(row < painted, "{columns}x{height}: provider hit {row}");
                            assert!(board.lines[row].contains("Lead chat"));
                            checked.0 += 1;
                        }
                        for (row, (participant, _)) in &board.worker_rows {
                            assert!(*row < painted, "{columns}x{height}: worker hit {row}");
                            let task = format!("Task{}", &participant[1..]);
                            assert!(
                                board.lines[*row].contains(participant.as_str())
                                    || board.lines[*row].contains(&task[..4.min(task.len())]),
                                "{columns}x{height}: {row} {participant} {:#?}",
                                board.lines
                            );
                            checked.1 += 1;
                        }
                    }
                }
            }
        }
        assert!(checked.0 > 0 && checked.1 > 0, "{checked:?}");
    }

    #[test]
    fn codex_weekly_detail_separates_unverified_current_quota_and_validates_registered_accounts() {
        let now = 1_000_000.0;
        let base_accounts = json!({
            "alternate": {
                "source": "session log", "weekly_used": 12,
                "observed_at": now - 45.0, "resets_at": now + 7200.0
            },
            "primary": {
                "source": "session log", "weekly_used": 77,
                "observed_at": now - 45.0, "resets_at": now + 7200.0
            }
        });
        let ambient = json!({
            "collection": {"ok": true},
            "provider_status": {
                "codex": {
                    "source": "session log",
                    "account_label": "primary",
                    "weekly_used": 77,
                    "observed_at": now - 45.0,
                    "weekly_resets_at": now + 7200.0,
                    "accounts": base_accounts
                },
                "claude": {"source": "unavailable"},
                "antigravity": {"source": "unavailable"}
            }
        });
        let usage = crate::usage::snapshot(&ambient, now);
        let strip = provider_line(&ambient, &usage, now, 120, false, false).0;
        assert!(strip.contains("Weekly left · Codex 23%"), "{strip}");
        for alias in ["primary", "alternate", "@"] {
            assert!(!strip.contains(alias), "{strip}");
        }
        let detail = weekly_usage_detail(&ambient, now);
        assert!(
            detail.starts_with(
                "Codex · 23% left\n  Source Codex session log · Reset in 2h00m\n  \
                 Registered Codex accounts (Codex session log)\n  \
                 @alternate · 88% left · Log age 45s · Reset in 2h00m\n  \
                 @primary · 23% left · Log age 45s · Reset in 2h00m\n"
            ),
            "{detail}"
        );
        assert!(!detail.contains("Codex · @"), "{detail}");

        let mut matrix = ambient.clone();
        matrix["provider_status"]["codex"]["accounts"] = json!({
            "alternate": base_accounts["alternate"],
            "primary": base_accounts["primary"],
            "old": {
                "source": "session log", "weekly_used": 15,
                "observed_at": now - 601.0, "resets_at": now + 7200.0
            },
            "expired": {
                "source": "session log", "weekly_used": 40,
                "observed_at": now - 120.0, "resets_at": now - 5.0
            },
            "missing-null": {
                "source": "session log", "weekly_used": null,
                "observed_at": now - 45.0, "resets_at": now + 7200.0
            },
            "api-source": {
                "source": "API", "weekly_used": 10,
                "observed_at": now - 45.0, "resets_at": now + 7200.0
            },
            "future-obs": {
                "source": "session log", "weekly_used": 10,
                "observed_at": now + 1.0, "resets_at": now + 7200.0
            },
            "bad-reset": {
                "source": "session log", "weekly_used": 10,
                "observed_at": now - 45.0, "resets_at": now - 45.0
            },
            "over-100": {
                "source": "session log", "weekly_used": 120,
                "observed_at": now - 45.0, "resets_at": now + 7200.0
            },
            "neg-used": {
                "source": "session log", "weekly_used": -1,
                "observed_at": now - 45.0, "resets_at": now + 7200.0
            }
        });
        let matrix_detail = weekly_usage_detail(&matrix, now);
        assert!(
            matrix_detail.contains("  @old · stale · Log age 10m · Reset unreported\n"),
            "{matrix_detail}"
        );
        assert!(
            matrix_detail.contains("  @expired · window expired · Log age 2m · Reset elapsed\n"),
            "{matrix_detail}"
        );
        for bad in [
            "missing-null",
            "api-source",
            "future-obs",
            "bad-reset",
            "over-100",
            "neg-used",
        ] {
            let expected =
                format!("  @{bad} · missing weekly reading · Log age — · Reset unreported\n");
            assert!(matrix_detail.contains(&expected), "{bad}: {matrix_detail}");
        }
        for line in matrix_detail.lines().filter(|line| {
            [
                "@old ",
                "@expired ",
                "@missing-null ",
                "@api-source ",
                "@future-obs ",
                "@bad-reset ",
                "@over-100 ",
                "@neg-used ",
            ]
            .iter()
            .any(|prefix| line.trim_start().starts_with(prefix))
        }) {
            assert!(!line.contains('%'), "{line}");
        }

        // Remapped, ambient-only, and mismatched codex_weekly_account claims never label current quota.
        for case in [
            {
                let mut r = ambient.clone();
                r["codex_weekly_account"] = json!("primary");
                r["provider_status"]["codex"]["accounts"]["primary"]["weekly_used"] = json!(12);
                r
            },
            {
                let mut r = ambient.clone();
                r["codex_weekly_account"] = json!("primary");
                r["provider_status"]["codex"]["accounts"]["primary"]["observed_at"] =
                    json!(now - 46.0);
                r
            },
            {
                let mut r = ambient.clone();
                r["codex_weekly_account"] = json!("primary");
                r["provider_status"]["codex"]["accounts"]["primary"]["resets_at"] =
                    json!(now + 7201.0);
                r
            },
            {
                let mut r = ambient.clone();
                r["codex_weekly_account"] = json!("alternate");
                r
            },
            {
                let mut r = ambient.clone();
                r["codex_weekly_account"] = json!("primary");
                r["provider_status"]["codex"]["weekly_native_observed_at"] = json!(now - 45.0);
                r
            },
            {
                let mut r = ambient.clone();
                r["codex_weekly_account"] = json!("primary");
                r["provider_status"]["codex"]["weekly_lead_observed_at"] = json!(now - 45.0);
                r
            },
        ] {
            let rendered = weekly_usage_detail(&case, now);
            assert!(!rendered.contains("Codex · @"), "{rendered}");
            assert!(rendered.starts_with("Codex · 23% left\n"), "{rendered}");
        }

        // Foreign provider accounts and invalid account keys never leak.
        let home_key = concat!("/ho", "me/u");
        let mut foreign = json!({
            "collection": {"ok": true},
            "codex_weekly_account": "BadUpper",
            "provider_accounts": {"codex": {"account": "foreign-codex"}},
            "provider_status": {
                "codex": {
                    "source": "session log",
                    "weekly_used": 77,
                    "observed_at": now - 45.0,
                    "weekly_resets_at": now + 7200.0,
                    "accounts": {
                        "BadUpper": {"source": "session log", "weekly_used": 10, "observed_at": now - 45.0, "resets_at": now + 7200.0},
                        "../etc": {"source": "session log", "weekly_used": 10, "observed_at": now - 45.0, "resets_at": now + 7200.0}
                    }
                },
                "claude": {"source": "unavailable", "accounts": {"claude-acct": {"weekly_used": 10}}},
                "pi": {"source": "unavailable", "accounts": {"pi-acct": {"weekly_used": 10}}},
                "antigravity": {"source": "unavailable"}
            }
        });
        foreign["provider_status"]["codex"]["accounts"]
            .as_object_mut()
            .unwrap()
            .insert(
                home_key.to_owned(),
                json!({"source": "session log", "weekly_used": 10, "observed_at": now - 45.0, "resets_at": now + 7200.0}),
            );
        let foreign_detail = weekly_usage_detail(&foreign, now);
        for leaked in [
            "BadUpper",
            "../etc",
            home_key,
            "foreign-codex",
            "claude-acct",
            "pi-acct",
            "Registered Codex accounts",
            "@",
        ] {
            assert!(
                !foreign_detail.contains(leaked),
                "{leaked}: {foreign_detail}"
            );
        }

        // Exact verified positive pairs current quota with @alternate and lists registered accounts.
        let positive = json!({
            "collection": {"ok": true},
            "codex_weekly_account": "alternate",
            "provider_status": {
                "codex": {
                    "source": "session log",
                    "weekly_used": 12,
                    "observed_at": now - 45.0,
                    "weekly_resets_at": now + 7200.0,
                    "accounts": base_accounts
                },
                "claude": {"source": "unavailable"},
                "antigravity": {"source": "unavailable"}
            }
        });
        let positive_detail = weekly_usage_detail(&positive, now);
        assert!(
            positive_detail.starts_with(
                "Codex · @alternate · 88% left\n  Source Codex session log · Reset in 2h00m\n  \
                 Registered Codex accounts (Codex session log)\n  \
                 @alternate · 88% left · Log age 45s · Reset in 2h00m\n  \
                 @primary · 23% left · Log age 45s · Reset in 2h00m\n"
            ),
            "{positive_detail}"
        );

        // Held reports leak zero aliases.
        for held in [
            {
                let mut r = positive.clone();
                r["collection"] = json!({"ok": true, "pending": true});
                r
            },
            {
                let mut r = positive.clone();
                r["collection"] = json!({"ok": false});
                r
            },
            {
                let mut r = positive.clone();
                r["capacity_stale"] = json!(true);
                r
            },
        ] {
            let held_detail = weekly_usage_detail(&held, now);
            for leaked in ["@", "alternate", "primary", "Registered Codex accounts"] {
                assert!(!held_detail.contains(leaked), "{leaked}: {held_detail}");
            }
        }
        let mut missing_collection = positive;
        missing_collection
            .as_object_mut()
            .unwrap()
            .remove("collection");
        let missing_headline = weekly_usage_detail(&missing_collection, now);
        assert!(
            missing_headline.starts_with("Codex · 88% left\n"),
            "{missing_headline}"
        );
        assert!(
            !missing_headline.contains("Codex · @"),
            "{missing_headline}"
        );
        missing_collection["provider_status"]["codex"]
            .as_object_mut()
            .unwrap()
            .remove("accounts");
        missing_collection["provider_accounts"] = json!({"codex": {"account": "alternate"}});
        let missing_no_accounts = weekly_usage_detail(&missing_collection, now);
        for leaked in ["@", "alternate", "primary"] {
            assert!(
                !missing_no_accounts.contains(leaked),
                "{leaked}: {missing_no_accounts}"
            );
        }
    }

    #[test]
    fn native_assignment_result_renders_separate_facts() {
        let plain = json!({"task_id":"task-plain","outcome":"accepted","title":"Plain",
            "summary":"Reviewed","calls":[{"kind":"delegate","selected_model":"m","reason_code":"ok"}]});
        let before = "OMS · Results\n────────────────────────────────────────────────────────────\n■ task-plain · accepted · Plain\nReviewed\n  ├─ delegate · m · ok\n\n";
        assert_eq!(results(&json!({"rows":[plain.clone()]}), 140), before);
        let native = json!({"task_id":"task-native","outcome":null,"title":"Native","native_assignment":
            {"attempt_id":"native-a","transport_label":"Submitted","consumption":"unconfirmed","acceptance":"pending"}});
        let output = results(&json!({"rows":[native.clone(), plain]}), 140);
        let lines = output.lines().collect::<Vec<_>>();
        let start = lines
            .iter()
            .position(|v| v.starts_with("■ task-native"))
            .unwrap();
        assert_eq!(
            lines[start + 1..start + 3],
            [
                "  ◇ Native assignment native-a · Submitted · consumption unconfirmed · decision pending",
                "",
            ]
        );
        assert!(
            output.ends_with(
                "\n■ task-plain · accepted · Plain\nReviewed\n  ├─ delegate · m · ok\n\n"
            )
        );
        assert!(!output.contains("Input receipt is not"));
        let narrow = results(&json!({"rows":[native.clone()]}), 30);
        assert!(narrow.lines().all(|v| width(v) <= 30), "{narrow}");
        assert!(
            narrow.replace('\n', "").contains("decision pending"),
            "{narrow}"
        );
        let mut reported = native;
        reported["native_assignment"]["report"] = json!({"status":"reported","result_kind":"candidate",
            "verification_state":"reported","summary":"Owner retained a candidate",
            "missing_evidence":["native completion and child cleanup are unconfirmed"]});
        let output = results(&json!({"rows":[reported.clone()]}), 140);
        assert!(output.contains(
            "Native report reported · candidate · verification reported · acceptance ineligible"
        ));
        assert!(output.contains("decision pending"));
        assert!(output.contains("Missing: native completion and child cleanup are unconfirmed"));
        reported["native_assignment"]["return_state"] = "reported".into();
        reported["native_assignment"]["resolution"] = "pending".into();
        reported["native_assignment"]["verification"] = "unverified".into();
        reported["native_assignment"]["next_action"] =
            json!({"reason":"Reconcile the exact retained result digest"});
        for columns in [32, 56, 160] {
            let shown = results(&json!({"rows":[reported.clone()]}), columns);
            assert!(shown.lines().all(|line| width(line) <= columns), "{shown}");
            let joined: String = shown.chars().filter(|c| !c.is_whitespace()).collect();
            for fact in [
                "Return reported",
                "resolution pending",
                "verification unverified",
                "Reconcile the exact retained result digest",
            ] {
                let compact: String = fact.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(joined.contains(&compact), "{fact}: {shown}");
            }
        }
        let narrow = results(&json!({"rows":[reported.clone()]}), 30);
        assert!(narrow.lines().all(|v| width(v) <= 30), "{narrow}");
        let mut issue = reported.clone();
        issue["native_assignment"]["verification_issue"] =
            "no completed canonical landing receipt for this reviewed patch".into();
        let shown = results(&json!({"rows":[issue]}), 160);
        assert!(shown.contains(
            "    Verification: no completed canonical landing receipt for this reviewed patch"
        ));
        assert!(shown.contains("acceptance ineligible"));
        let mut managed = reported.clone();
        managed["native_assignment"]["acceptance"] = "accepted".into();
        managed["native_assignment"]["landing"] = json!({"landing_id":"foreign-mark"});
        let shown = results(&json!({"rows":[managed]}), 160);
        assert!(shown.contains("acceptance ineligible"), "{shown}");
        assert!(!shown.contains("canonical landing"), "{shown}");
        let mut landed = reported;
        landed["native_assignment"]["acceptance"] = "accepted".into();
        landed["native_assignment"]["verification"] = "verified".into();
        let id = format!("land-20261124T010203Z-{}", "a".repeat(16));
        landed["native_assignment"]["landing"] =
            json!({"source":"canonical-landing","landing_id":id});
        let accepted = format!("accepted through canonical landing {id}");
        for columns in [32, 56, 160] {
            let shown = results(&json!({"rows":[landed.clone()]}), columns);
            assert!(shown.lines().all(|line| width(line) <= columns), "{shown}");
            assert!(!shown.contains("ineligible"), "{shown}");
            assert!(!shown.contains("Verification:"), "{shown}");
            let joined: String = shown.chars().filter(|c| !c.is_whitespace()).collect();
            for fact in [
                "decision accepted",
                "verification verified",
                accepted.as_str(),
            ] {
                let compact: String = fact.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(joined.contains(&compact), "{fact}: {shown}");
            }
        }
    }

    #[test]
    fn native_assignment_board_joins_canonical_membership_once() {
        use crate::orchestrator::account_binding::test_support::TempDir;
        use oms_command_operations as events;
        use oms_command_operations::value::{V, obj};
        use oms_command_room::{project, value as room_value};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("native-assignment-board");
        let stop = AtomicBool::new(false);
        let room_id = "room-native";
        let task = "inspect-source";
        let plan_id = format!("plan_{}", "a".repeat(32));
        let lease_id = format!("lease_{}", "b".repeat(32));
        let refs = |value: Value| V::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        let main = events::create_attempt(
            &dir.0,
            &events::StartRequest {
                provider: "codex".into(),
                tool: "panel-main".into(),
                refs: refs(json!({"panel_room_id":room_id, "panel_role":"main",
                    "panel_native_scheduling":"unmanaged"})),
                ..Default::default()
            },
            &stop,
        )
        .unwrap();
        let attempt = events::create_attempt(
            &dir.0,
            &events::StartRequest {
                provider: "codex".into(),
                tool: "panel-native-assignment".into(),
                task_id: task.into(),
                parent_attempt_id: main.clone(),
                refs: refs(json!({"panel_room_id":room_id, "panel_owner":"codex",
                    "panel_native_scheduling":"unmanaged",
                    "native_assignment_schema":"1", "plan_id":plan_id, "lease_id":lease_id,
                    "native_intent_sha256":"c".repeat(64), "native_payload_sha256":"d".repeat(64),
                    "native_transport":"intent"})),
                ..Default::default()
            },
            &stop,
        )
        .unwrap();
        let receipt = events::new_event(
            &attempt,
            0usize.into(),
            "attempt.usage",
            obj([
                ("actor", obj([("kind", "owner".into()), ("name", "test".into())])),
                ("refs", refs(json!({"native_transport":"submitted",
                    "native_binding":format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json"),
                    "native_binding_sha256":"e".repeat(64),
                    "native_room_message":format!("msg-{}", "f".repeat(32))
                }))),
            ]),
        )
        .unwrap();
        events::append_lifecycle(&dir.0, receipt, &stop).unwrap();
        let (status, out, err) = events::run(
            events::App::Events,
            &["list".into(), "--json".into()],
            &dir.0,
            &stop,
        );
        assert_eq!(status, 0, "{}", String::from_utf8_lossy(&err));
        let attempts: Value = serde_json::from_slice(&out).unwrap();
        let find = |id: &str| {
            rows(&attempts)
                .iter()
                .find(|row| row["attempt_id"] == id)
                .unwrap()
        };

        let room_rows = [
            json!({"thread":room_id, "seq":1, "room_event":{"kind":"created","participant":main}}),
            json!({"thread":room_id, "seq":2, "room_event":{"kind":"join","participant":main,
                "provider":"codex","role":"main","label":"Main"}}),
            json!({"thread":room_id, "seq":3, "room_event":{"kind":"join","participant":attempt,
                "provider":"codex","role":"worker","parent":main,"label":"Native task assignment"}}),
        ]
        .map(|event| room_value::parse(&serde_json::to_vec(&event).unwrap(), 4096).unwrap());
        let room: Value = serde_json::from_slice(
            &room_value::encode(&project(&room_rows).unwrap(), false, true).unwrap(),
        )
        .unwrap();
        let mut report = json!({
            "collection":{"ok":true},
            "room":room,
            "plan":{"plan_id":plan_id, "tasks":{(task):{
                "id":task, "lease_id":lease_id, "title":"Inspect assigned source"
            }}},
            "room_attempts":{(&main):find(&main), (&attempt):find(&attempt)},
            "attempts":{"recent":attempts, "active":attempts}
        });
        let assignment = |report: &Value| {
            let rows = calls(report);
            let mut matches = rows
                .into_iter()
                .filter(|row| row["tool"] == "panel-native-assignment");
            let found = matches.next();
            assert!(matches.next().is_none(), "{report}");
            found
        };
        let call = assignment(&report).unwrap();
        assert_eq!(call["attempt"], attempt);
        assert_eq!(call["parent"], main);
        assert_eq!(call["task_id"], task);
        assert_eq!(call["state"], "assigned");
        assert_ne!(call["native_child"], true);
        assert_eq!(call_badge(&call).0, "[· submitted]");
        for view in [View::Auto, View::Compact, View::Detail, View::Graph] {
            let nav = navigation::Navigation {
                view,
                ..Default::default()
            };
            assert!(
                visible(&report, &nav)
                    .iter()
                    .any(|row| row["attempt"] == attempt)
            );
        }
        let nav = navigation::Navigation::default();
        let board = board_frame(&report, &nav, 120, 30, &BTreeMap::new(), false, true);
        let row = board
            .worker_rows
            .iter()
            .find(|(_, (id, aid))| id == &attempt && aid == &attempt)
            .unwrap();
        let line = &board.lines[*row.0];
        assert!(
            line.contains("Assignment")
                && line.contains("Inspect assigned source")
                && line.contains("submitted")
                && !line.contains(['├', '└']),
            "{line}"
        );
        let detail = render_detail_card(Some(&call), 120, 116, true, false, usize::MAX).join("\n");
        assert!(
            detail.contains(task) && detail.contains("Submitted · consumption unconfirmed"),
            "{detail}"
        );

        for (field, wrong) in [
            ("panel_room_id", json!("room-other")),
            ("panel_room", json!(true)),
            ("native_assignment_schema", json!(1)),
            ("native_binding", json!("tmp/malformed.json")),
            ("panel_owner", json!("claude")),
            ("panel_room_participant", json!("another-participant")),
        ] {
            let mut bad = report.clone();
            bad["room_attempts"][&attempt]["refs"][field] = wrong;
            assert!(assignment(&bad).is_none(), "{field}");
        }
        for (field, wrong) in [
            ("parent_attempt_id", json!("another-main")),
            ("provider", json!("claude")),
        ] {
            let mut bad = report.clone();
            bad["room_attempts"][&attempt][field] = wrong;
            assert!(assignment(&bad).is_none(), "{field}");
        }
        let mut bad = report.clone();
        let child = bad["room"]["participants"][1].clone();
        bad["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .push(child);
        assert!(assignment(&bad).is_none());
        bad["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        assert!(assignment(&bad).is_none());
        let mut mismatched_parent = report.clone();
        mismatched_parent["room"]["participants"][0]["provider"] = json!("claude");
        assert!(assignment(&mismatched_parent).is_none());
        let mut replaced_parent = mismatched_parent;
        replaced_parent["room"]["swaps"] =
            json!([{"attempt":main,"participant":main,"from":"codex","to":"claude"}]);
        assert!(assignment(&replaced_parent).is_some());
        assert!(
            !visible(&replaced_parent, &nav)
                .iter()
                .any(|row| row["attempt"] == attempt)
        );
        let mut changed_lease = report.clone();
        changed_lease["plan"]["tasks"][task]["lease_id"] = json!(null);
        assert_eq!(assignment(&changed_lease).unwrap()["title"], task);

        report["room"]["participants"][1]["joined"] = json!(false);
        assert!(
            !visible(&report, &nav)
                .iter()
                .any(|row| row["attempt"] == attempt)
        );
        assert_eq!(assignment(&report).unwrap()["state"], "assigned");
        report["room_attempts"][&attempt]["state"] = json!("done");
        report["room_attempts"][&attempt]["terminal"] = json!(true);
        let history = visible(
            &report,
            &navigation::Navigation {
                view: View::Compact,
                ..nav
            },
        );
        let ended = history
            .iter()
            .find(|row| row["attempt"] == attempt)
            .unwrap();
        assert_eq!(call_badge(ended).0, "[· closed]");
        assert_eq!(ended["native_assignment_transport"], "submitted");
        assert_eq!(ended["completed_task"], Value::Null);
    }

    #[test]
    fn stale_room_placeholders_fold_and_never_fabricate_live_activity() {
        let now = oms_panel_core::timestamp();
        let mut participants = vec![json!({
            "participant": "main-1",
            "attempt": "att-main",
            "role": "main",
            "provider": "codex",
            "model": "gpt-6.1-sol",
            "joined": true,
            "joined_at": now - 600
        })];
        for idx in 0..128 {
            participants.push(json!({
                "participant": format!("call-stale-{idx:03}"),
                "role": "worker",
                "provider": (["codex", "claude", "antigravity"][idx % 3]),
                "title": format!("Stale task {idx:03}"),
                "joined": true,
                "joined_at": now - 600
            }));
        }
        for idx in 0..16 {
            participants.push(json!({
                "participant": format!("call-left-{idx:02}"),
                "role": "worker",
                "provider": "codex",
                "title": format!("Left unbacked {idx:02}"),
                "joined": false,
                "joined_at": now - 600
            }));
        }
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": participants
            },
            "room_attempts": {
                "att-main": {
                    "attempt_id": "att-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 60,
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                }
            }
        });
        let rows = calls(&report);
        assert_eq!(rows.len(), 129, "unbacked left rows must not be emitted");
        for row in rows.iter().filter(|row| row["role"] != "main") {
            assert_eq!(text(row, "state"), "", "{row}");
            assert!(row["event_tokens"].is_null(), "{row}");
            assert!(row["receipt_tokens"].is_null(), "{row}");
            assert_eq!(text(row, "board_elapsed"), "—", "{row}");
        }

        let read = BTreeMap::new();
        let overview_nav = navigation::Navigation::default();
        let overview = frame(&report, &overview_nav, 140, 30, &read).join("\n");
        assert!(overview.contains("0 calls"), "{overview}");
        assert!(!overview.contains("running"), "{overview}");
        assert!(overview.contains("128 folded (Tab all)"), "{overview}");
        assert!(overview.contains("no visible work"), "{overview}");
        assert!(!overview.contains("Stale task"), "{overview}");
        assert!(!overview.contains("Left unbacked"), "{overview}");

        let health_nav = navigation::Navigation {
            health: true,
            ..Default::default()
        };
        let health = frame(&report, &health_nav, 100, 24, &read).join("\n");
        let active_line = health
            .lines()
            .find(|line| line.contains("Active room"))
            .expect("Active room line");
        assert!(
            active_line.contains(" 1 ")
                && !active_line.contains("129")
                && !active_line.contains("145"),
            "{active_line}"
        );

        let compact_nav = navigation::Navigation {
            view: View::Compact,
            ..Default::default()
        };
        let compact = frame(&report, &compact_nav, 120, 40, &read).join("\n");
        assert!(compact.contains("[· -]"), "{compact}");
        assert!(compact.contains("— tok"), "{compact}");
        assert!(!compact.contains("[⟳ run]"), "{compact}");
        assert!(!compact.contains("Left unbacked"), "{compact}");

        let mut mixed_participants = vec![json!({
            "participant": "main-1",
            "attempt": "att-main",
            "role": "main",
            "provider": "codex",
            "model": "gpt-6.1-sol",
            "joined": true,
            "joined_at": now - 60
        })];
        let mut mixed_dispatches = serde_json::Map::new();
        for (idx, provider) in ["codex", "claude", "antigravity"].into_iter().enumerate() {
            mixed_participants.extend([
                json!({
                    "participant": format!("left-wait-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "waiting_input",
                    "joined": false,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("left-unk-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "unknown",
                    "joined": false,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("left-queued-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": false,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("undated-queued-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": true
                }),
                json!({
                    "participant": format!("undated-unk-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "unknown",
                    "joined": true
                }),
                json!({
                    "participant": format!("undated-disp-res-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "joined": true
                }),
                json!({
                    "participant": format!("undated-disp-unk-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "joined": true
                }),
                json!({
                    "participant": format!("term-main-disp-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("old-work-main-disp-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("missing-custodian-disp-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("swapped-disp-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("live-queued-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "queued",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("live-wait-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "waiting_input",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("live-approval-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "waiting_approval",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("live-blocked-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "blocked",
                    "joined": true,
                    "joined_at": now - 30
                }),
                json!({
                    "participant": format!("live-review-{idx}"),
                    "role": "worker",
                    "provider": provider,
                    "state": "review",
                    "joined": true,
                    "joined_at": now - 30
                }),
            ]);
            mixed_dispatches.insert(
                format!("undated-disp-res-{idx}"),
                json!({
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "reserved"
                }),
            );
            mixed_dispatches.insert(
                format!("undated-disp-unk-{idx}"),
                json!({
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "unknown"
                }),
            );
            mixed_dispatches.insert(
                format!("term-main-disp-{idx}"),
                json!({
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-old-main",
                    "state": "running",
                    "created_at": now - 30
                }),
            );
            mixed_dispatches.insert(
                format!("old-work-main-disp-{idx}"),
                json!({
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-old-working",
                    "state": "running",
                    "created_at": now - 30
                }),
            );
            mixed_dispatches.insert(
                format!("missing-custodian-disp-{idx}"),
                json!({
                    "room": "room-demo",
                    "main": "main-1",
                    "state": "reserved",
                    "created_at": now - 30
                }),
            );
            mixed_dispatches.insert(
                format!("swapped-disp-{idx}"),
                json!({
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-swapped-main",
                    "state": "running",
                    "created_at": now - 30
                }),
            );
        }
        mixed_participants.extend([
            json!({
                "participant": "left-review",
                "role": "worker",
                "provider": "codex",
                "state": "review",
                "joined": false,
                "joined_at": now - 30
            }),
            json!({
                "participant": "live-disp",
                "role": "worker",
                "provider": "codex",
                "joined": true
            }),
            json!({
                "participant": "ancient-queued",
                "role": "worker",
                "provider": "codex",
                "state": "queued",
                "joined": true,
                "joined_at": now - 600
            }),
        ]);
        mixed_dispatches.insert(
            "live-disp".to_owned(),
            json!({
                "room": "room-demo",
                "main": "main-1",
                "main_attempt": "att-main",
                "state": "reserved",
                "created_at": now - 30
            }),
        );
        let mixed_report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": mixed_participants,
                "swaps": [
                    {"participant": "main-1", "attempt": "att-swapped-main"}
                ]
            },
            "room_attempts": {
                "att-main": {
                    "attempt_id": "att-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 60,
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-old-main": {
                    "attempt_id": "att-old-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "done",
                    "terminal": true,
                    "created_at": now - 900,
                    "updated_at": now - 600,
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-old-working": {
                    "attempt_id": "att-old-working",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 900,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_role": "main",
                        "panel_room_participant": "main-1"
                    }
                },
                "att-swapped-main": {
                    "attempt_id": "att-swapped-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 400,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_role": "main",
                        "panel_room_participant": "main-1"
                    }
                }
            },
            "room_dispatches": mixed_dispatches
        });
        let mixed_calls = calls(&mixed_report);
        let auto_visible = visible(&mixed_report, &overview_nav);
        let attn_nav = navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let attn_visible = visible(&mixed_report, &attn_nav);
        let compact_visible = visible(&mixed_report, &compact_nav);
        let has_id = |list: &[Value], id: &str| list.iter().any(|r| r["participant"] == id);

        for idx in 0..3 {
            for prefix in [
                "left-wait",
                "left-unk",
                "left-queued",
                "term-main-disp",
                "old-work-main-disp",
                "missing-custodian-disp",
                "swapped-disp",
            ] {
                let id = format!("{prefix}-{idx}");
                let row = mixed_calls
                    .iter()
                    .find(|r| r["participant"] == id)
                    .expect("history placeholder row");
                assert_eq!(row["room_only_history"], true, "{id}: {row}");
                assert!(!has_id(&auto_visible, &id), "{id} leaked into View::Auto");
                assert!(
                    !has_id(&attn_visible, &id),
                    "{id} leaked into attention_only"
                );
                assert!(
                    has_id(&compact_visible, &id),
                    "{id} missing from View::Compact"
                );
            }
            for prefix in [
                "left-queued",
                "undated-queued",
                "term-main-disp",
                "old-work-main-disp",
                "missing-custodian-disp",
                "swapped-disp",
            ] {
                let id = format!("{prefix}-{idx}");
                let row = mixed_calls
                    .iter()
                    .find(|r| r["participant"] == id)
                    .expect("queued retention row");
                assert_eq!(
                    text(row, "state"),
                    "queued",
                    "{id} lost raw queued state: {row}"
                );
                assert!(
                    has_id(&compact_visible, &id),
                    "{id} missing from View::Compact"
                );
            }
            for prefix in [
                "undated-queued",
                "undated-unk",
                "undated-disp-res",
                "undated-disp-unk",
            ] {
                let id = format!("{prefix}-{idx}");
                let row = mixed_calls
                    .iter()
                    .find(|r| r["participant"] == id)
                    .expect("stale undated placeholder row");
                assert_eq!(row["room_only_stale"], true, "{id}: {row}");
                assert_ne!(text(row, "state"), "done", "{id} fabricated done: {row}");
                assert!(!has_id(&auto_visible, &id), "{id} leaked into View::Auto");
                assert!(
                    !has_id(&attn_visible, &id),
                    "{id} leaked into attention_only"
                );
                assert!(
                    has_id(&compact_visible, &id),
                    "{id} missing from View::Compact"
                );
            }
            let queued_id = format!("live-queued-{idx}");
            assert!(
                has_id(&auto_visible, &queued_id),
                "{queued_id} should stay visible in View::Auto"
            );
            for prefix in ["live-wait", "live-approval", "live-blocked", "live-review"] {
                let id = format!("{prefix}-{idx}");
                assert!(
                    has_id(&auto_visible, &id),
                    "{id} should stay visible in View::Auto"
                );
                assert!(
                    has_id(&attn_visible, &id),
                    "{id} should stay visible in attention_only"
                );
            }
        }

        let left_review = mixed_calls
            .iter()
            .find(|r| r["participant"] == "left-review")
            .expect("left-review row");
        assert_eq!(left_review["room_only_history"], true, "{left_review}");
        assert_eq!(text(left_review, "state"), "review", "{left_review}");
        assert!(
            !has_id(&auto_visible, "left-review"),
            "left-review leaked into View::Auto"
        );
        assert!(
            !has_id(&attn_visible, "left-review"),
            "left-review leaked into attention_only"
        );
        assert!(
            has_id(&compact_visible, "left-review"),
            "left-review missing from View::Compact"
        );
        assert!(
            has_id(&auto_visible, "live-disp"),
            "live-disp on active Main should stay visible in View::Auto"
        );

        let mixed_attention = crate::attention::items(&mixed_report, &read);
        let attention_ids: BTreeSet<&str> = mixed_attention
            .iter()
            .map(|item| item.row_id.1.as_str())
            .collect();
        assert_eq!(mixed_attention.len(), 12, "{mixed_attention:?}");
        for forbidden in [
            "left-wait-0",
            "left-wait-1",
            "left-wait-2",
            "left-queued-0",
            "left-review",
            "term-main-disp-0",
            "term-main-disp-1",
            "term-main-disp-2",
            "old-work-main-disp-0",
            "missing-custodian-disp-0",
            "swapped-disp-0",
            "undated-queued-0",
            "undated-unk-0",
            "undated-disp-res-0",
            "undated-disp-unk-0",
        ] {
            assert!(
                !attention_ids.contains(forbidden),
                "{forbidden} leaked into attention::items: {attention_ids:?}"
            );
        }
        let mixed_overview = frame(&mixed_report, &overview_nav, 140, 30, &read).join("\n");
        let mixed_header = mixed_overview.lines().next().unwrap_or("");
        assert!(
            mixed_header.contains("3 review · 12 attention"),
            "{mixed_header}"
        );

        let mut no_main_report = mixed_report.clone();
        no_main_report["room_attempts"]
            .as_object_mut()
            .unwrap()
            .remove("att-main");
        let no_main_calls = calls(&no_main_report);
        let no_main_auto = visible(&no_main_report, &overview_nav);
        let no_main_disp = no_main_calls
            .iter()
            .find(|r| r["participant"] == "live-disp")
            .unwrap();
        assert_eq!(no_main_disp["room_only_history"], true, "{no_main_disp}");
        assert!(!has_id(&no_main_auto, "live-disp"));
        let no_main_queued = no_main_calls
            .iter()
            .find(|r| r["participant"] == "live-queued-0")
            .unwrap();
        assert_eq!(no_main_queued["room_only_stale"], true, "{no_main_queued}");
        assert_eq!(text(no_main_queued, "state"), "queued", "{no_main_queued}");
        let no_main_ancient = no_main_calls
            .iter()
            .find(|r| r["participant"] == "ancient-queued")
            .unwrap();
        assert_eq!(
            no_main_ancient["room_only_stale"], true,
            "{no_main_ancient}"
        );
        assert_eq!(text(no_main_ancient, "state"), "", "{no_main_ancient}");

        for (field, value) in [("closed", json!(true)), ("successor", json!("room-next"))] {
            let mut sealed_report = mixed_report.clone();
            sealed_report["room"][field] = value;
            let sealed_calls = calls(&sealed_report);
            let sealed_auto = visible(&sealed_report, &overview_nav);
            let sealed_disp = sealed_calls
                .iter()
                .find(|r| r["participant"] == "live-disp")
                .unwrap();
            assert_eq!(
                sealed_disp["room_only_history"], true,
                "{field}: {sealed_disp}"
            );
            assert!(!has_id(&sealed_auto, "live-disp"), "{field}");
            let sealed_queued = sealed_calls
                .iter()
                .find(|r| r["participant"] == "live-queued-0")
                .unwrap();
            assert_eq!(
                sealed_queued["room_only_stale"], true,
                "{field}: {sealed_queued}"
            );
            assert_eq!(
                text(sealed_queued, "state"),
                "queued",
                "{field}: {sealed_queued}"
            );
            let sealed_ancient = sealed_calls
                .iter()
                .find(|r| r["participant"] == "ancient-queued")
                .unwrap();
            assert_eq!(
                sealed_ancient["room_only_stale"], true,
                "{field}: {sealed_ancient}"
            );
            assert_eq!(
                text(sealed_ancient, "state"),
                "",
                "{field}: {sealed_ancient}"
            );
        }

        let main_attempt_row = json!({
            "attempt_id": "att-main",
            "tool": "panel-main",
            "provider": "codex",
            "state": "working",
            "terminal": false,
            "created_at": now - 60,
            "refs": {
                "panel_room_id": "room-demo",
                "panel_role": "main",
                "panel_room_participant": "main-1"
            }
        });
        let alias_report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 60
                    },
                    {
                        "participant": "live-disp",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "prev-disp",
                        "role": "worker",
                        "provider": "claude",
                        "state": "queued",
                        "joined": true,
                        "joined_at": now - 30
                    }
                ]
            },
            "room_attempts": {
                "att-main": main_attempt_row,
                "main-1": main_attempt_row,
                "att-old-main": {
                    "attempt_id": "att-old-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "done",
                    "terminal": true,
                    "created_at": now - 900,
                    "updated_at": now - 600,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_role": "main",
                        "panel_room_participant": "main-1"
                    }
                }
            },
            "room_dispatches": {
                "live-disp": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "reserved",
                    "created_at": now - 30
                },
                "prev-disp": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-old-main",
                    "state": "running",
                    "created_at": now - 30
                }
            }
        });
        assert_eq!(
            current_room_main_custodian(&alias_report, &alias_report["room"], "room-demo"),
            Some(("main-1", "att-main"))
        );
        let alias_auto = visible(&alias_report, &overview_nav);
        assert!(has_id(&alias_auto, "live-disp"));
        assert!(!has_id(&alias_auto, "prev-disp"));
        let alias_prev = calls(&alias_report)
            .into_iter()
            .find(|r| r["participant"] == "prev-disp")
            .unwrap();
        assert_eq!(alias_prev["room_only_history"], true, "{alias_prev}");
        assert_eq!(text(&alias_prev, "state"), "queued", "{alias_prev}");

        let mut ambiguous_alias = alias_report.clone();
        ambiguous_alias["room_attempts"]
            .as_object_mut()
            .unwrap()
            .insert(
                "att-other-live".to_owned(),
                json!({
                    "attempt_id": "att-other-live",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 30,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_role": "main",
                        "panel_room_participant": "main-1"
                    }
                }),
            );
        assert_eq!(
            current_room_main_custodian(&ambiguous_alias, &ambiguous_alias["room"], "room-demo"),
            None
        );
        assert!(!has_id(
            &visible(&ambiguous_alias, &overview_nav),
            "live-disp"
        ));
    }

    #[test]
    fn cold_tasks_and_current_candidates_stay_visible_without_fabricated_telemetry() {
        let now = oms_panel_core::timestamp();
        let cold_joined_iso =
            chrono::DateTime::from_timestamp(i64::try_from(now.saturating_sub(15)).unwrap(), 0)
                .unwrap()
                .to_rfc3339();
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "attempt": "att-main",
                        "role": "main",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "call-cold",
                        "role": "worker",
                        "provider": "codex",
                        "title": "Cold newly joined",
                        "joined": true,
                        "joined_at": cold_joined_iso
                    },
                    {
                        "participant": "call-reserved",
                        "role": "worker",
                        "provider": "claude",
                        "title": "Reserved dispatch task",
                        "joined": true,
                        "joined_at": now - 200
                    },
                    {
                        "participant": "call-dispatched",
                        "role": "worker",
                        "provider": "codex",
                        "title": "Running dispatch task",
                        "joined": true,
                        "joined_at": now - 200
                    },
                    {
                        "participant": "call-question",
                        "role": "worker",
                        "provider": "codex",
                        "title": "Open question task",
                        "joined": true,
                        "joined_at": now - 900
                    },
                    {
                        "participant": "call-awaiting",
                        "role": "worker",
                        "provider": "codex",
                        "title": "Awaiting write candidate",
                        "joined": true,
                        "joined_at": now - 100
                    },
                    {
                        "participant": "call-stale-old",
                        "role": "worker",
                        "provider": "codex",
                        "title": "Stale old unbacked",
                        "joined": true,
                        "joined_at": now - 600
                    }
                ],
                "open_questions": [
                    {
                        "sender": "call-question",
                        "recipient": "main-1",
                        "text": "Need schema decision"
                    }
                ]
            },
            "room_attempts": {
                "att-main": {
                    "attempt_id": "att-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 60,
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                }
            },
            "room_dispatches": {
                "call-reserved": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "reserved",
                    "task_id": "t-res",
                    "created_at": now - 200,
                    "route": {
                        "role": "builder",
                        "purpose": "Build reserved feature",
                        "model": "claude-sonnet-5-5",
                        "access": "write"
                    }
                },
                "call-dispatched": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "running",
                    "task_id": "t-run",
                    "created_at": now - 200,
                    "route": {
                        "role": "worker",
                        "purpose": "Inspect dispatch",
                        "model": "gpt-6.1-sol",
                        "access": "read"
                    }
                },
                "call-awaiting": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "awaiting",
                    "exit": 0,
                    "task_id": "t-await",
                    "created_at": now - 100,
                    "route": {
                        "role": "builder",
                        "purpose": "Patch awaiting",
                        "model": "gpt-6.1-sol",
                        "access": "write"
                    }
                }
            }
        });

        let read = BTreeMap::new();
        let nav = navigation::Navigation::default();
        let board = board_frame(&report, &nav, 140, 30, &read, false, true);
        let output = board.lines.join("\n");
        assert!(output.contains("5 calls"), "{output}");
        assert!(!output.contains("running"), "{output}");
        assert!(output.contains("1 folded (Tab all)"), "{output}");
        assert!(!output.contains("Stale old unbacked"), "{output}");
        for expected in [
            "Cold newly joined",
            "Reserved dispatch task",
            "Running dispatch task",
            "Open question task",
            "Awaiting write candidate",
        ] {
            let line = board
                .lines
                .iter()
                .find(|line| line.contains(expected))
                .unwrap_or_else(|| panic!("missing {expected}: {output}"));
            assert!(line.contains("[· -]"), "{expected}: {line}");
            assert!(line.contains("— tok"), "{expected}: {line}");
            assert!(
                line.trim_end_matches(" │").ends_with('—'),
                "{expected}: {line}"
            );
        }
        let rows = calls(&report);
        let reserved = rows
            .iter()
            .find(|r| r["participant"] == "call-reserved")
            .unwrap();
        assert_eq!(reserved["display_role"], "builder");
        assert_eq!(reserved["task_id"], "t-res");
        assert_eq!(reserved["purpose"], "Build reserved feature");
        assert_eq!(reserved["model"], "claude-sonnet-5-5");
        assert_eq!(text(reserved, "state"), "");
    }

    #[test]
    fn live_attempts_and_native_children_keep_exact_tokens_and_elapsed() {
        let now = oms_panel_core::timestamp();
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "attempt": "att-main",
                        "role": "main",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "call-work",
                        "attempt": "att-work",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Active worker"
                    },
                    {
                        "participant": "call-start",
                        "attempt": "att-start",
                        "role": "worker",
                        "provider": "claude",
                        "joined": true,
                        "title": "Starting worker"
                    },
                    {
                        "participant": "call-queue",
                        "attempt": "att-queue",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Queued attempt"
                    }
                ]
            },
            "room_attempts": {
                "att-main": {
                    "attempt_id": "att-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "created_at": now - 90,
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-work": {
                    "attempt_id": "att-work",
                    "tool": "peer-delegate",
                    "provider": "codex",
                    "state": "working",
                    "created_at": now - 45,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-work",
                        "panel_role": "worker"
                    },
                    "usage": {"tokens": 12500},
                    "usage_reports": {"tokens": 1}
                },
                "att-start": {
                    "attempt_id": "att-start",
                    "tool": "peer-delegate",
                    "provider": "claude",
                    "state": "starting",
                    "created_at": now - 12,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-start",
                        "panel_role": "worker"
                    },
                    "usage": {"tokens": 400},
                    "usage_reports": {"tokens": 1}
                },
                "att-queue": {
                    "attempt_id": "att-queue",
                    "tool": "peer-delegate",
                    "provider": "codex",
                    "state": "queued",
                    "created_at": now - 5,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-queue",
                        "panel_role": "worker"
                    }
                }
            },
            "native_children": [
                {
                    "participant": "native-claude-1111111111111111",
                    "native_child": true,
                    "provider": "claude",
                    "role": "subagent",
                    "state": "working",
                    "title": "Active native child"
                },
                {
                    "participant": "native-codex-2222222222222222",
                    "native_child": true,
                    "provider": "codex",
                    "role": "subagent",
                    "state": "unknown",
                    "title": "Seen codex child",
                    "seen_at": (now - 30) * 1000,
                    "seen_observed_ms": now * 1000
                }
            ]
        });

        let read = BTreeMap::new();
        let output = frame(&report, &navigation::Navigation::default(), 140, 30, &read).join("\n");
        assert!(output.contains("4 running"), "{output}");
        assert!(
            output.lines().any(|l| l.contains("Active worker")
                && l.contains("12.5K tok")
                && l.contains("45s")),
            "{output}"
        );
        assert!(
            output.lines().any(|l| l.contains("Starting worker")
                && l.contains("400 tok")
                && l.contains("12s")),
            "{output}"
        );
        assert!(
            output
                .lines()
                .any(|l| l.contains("Queued attempt") && l.contains("[⟳ run]")),
            "{output}"
        );
        assert!(output.contains("Active native child"), "{output}");
        assert!(
            output
                .lines()
                .any(|l| l.contains("Seen codex child") && l.contains("seen 30s")),
            "{output}"
        );
    }

    #[test]
    fn review_rows_stay_visible_until_returned_or_finalized() {
        let now = oms_panel_core::timestamp();
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "attempt": "att-main",
                        "role": "main",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "call-write-rev",
                        "attempt": "att-write-rev",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Write review pending"
                    },
                    {
                        "participant": "call-read-rev",
                        "attempt": "att-read-rev",
                        "role": "worker",
                        "provider": "claude",
                        "joined": true,
                        "title": "Read review open"
                    },
                    {
                        "participant": "call-read-ret",
                        "attempt": "att-read-ret",
                        "role": "worker",
                        "provider": "claude",
                        "joined": true,
                        "title": "Read review returned"
                    },
                    {
                        "participant": "call-fin",
                        "attempt": "att-fin",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "task_id": "task-final",
                        "title": "Finalized write review"
                    }
                ]
            },
            "room_attempts": {
                "att-main": {
                    "attempt_id": "att-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-write-rev": {
                    "attempt_id": "att-write-rev",
                    "provider": "codex",
                    "state": "review",
                    "updated_at": now,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-write-rev",
                        "panel_access": "write"
                    }
                },
                "att-read-rev": {
                    "attempt_id": "att-read-rev",
                    "provider": "claude",
                    "state": "review",
                    "updated_at": now,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-read-rev",
                        "panel_access": "read"
                    }
                },
                "att-read-ret": {
                    "attempt_id": "att-read-ret",
                    "provider": "claude",
                    "state": "review",
                    "updated_at": now,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-read-ret",
                        "panel_access": "read"
                    }
                },
                "att-fin": {
                    "attempt_id": "att-fin",
                    "provider": "codex",
                    "state": "review",
                    "task_id": "task-final",
                    "updated_at": now,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-fin",
                        "panel_access": "write"
                    }
                }
            },
            "returned_workers": {
                "att-read-ret": chrono::DateTime::from_timestamp(
                    i64::try_from(now.saturating_sub(10)).unwrap(),
                    0,
                )
                .unwrap()
                .to_rfc3339()
            },
            "finalized": {"task-final": "accepted"}
        });

        let read = BTreeMap::new();
        let overview =
            frame(&report, &navigation::Navigation::default(), 140, 30, &read).join("\n");
        assert!(overview.contains("2 review"), "{overview}");
        assert!(overview.contains("2 past (Tab all)"), "{overview}");
        assert!(overview.contains("Write review pending"), "{overview}");
        assert!(overview.contains("Read review open"), "{overview}");
        assert!(overview.contains("[⚑ review]"), "{overview}");
        assert!(!overview.contains("Read review returned"), "{overview}");
        assert!(!overview.contains("Finalized write review"), "{overview}");

        let attention_nav = navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let attention_rows = visible(&report, &attention_nav);
        assert!(
            attention_rows
                .iter()
                .any(|r| r["participant"] == "call-write-rev")
        );
        assert!(
            attention_rows
                .iter()
                .any(|r| r["participant"] == "call-read-rev")
        );
        assert!(
            !attention_rows
                .iter()
                .any(|r| r["participant"] == "call-read-ret" || r["participant"] == "call-fin")
        );
    }

    #[test]
    fn stopped_and_settled_history_fold_into_past_without_inflation() {
        let now = oms_panel_core::timestamp();
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "attempt": "att-main",
                        "role": "main",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "call-done",
                        "attempt": "att-done",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Done task"
                    },
                    {
                        "participant": "call-failed",
                        "attempt": "att-failed",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Failed old task"
                    },
                    {
                        "participant": "call-timeout",
                        "attempt": "att-timeout",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Timed out task"
                    },
                    {
                        "participant": "call-abandoned",
                        "attempt": "att-abandoned",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Abandoned task"
                    },
                    {
                        "participant": "call-cancelled",
                        "attempt": "att-cancelled",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "title": "Cancelled task"
                    },
                    {
                        "participant": "call-returned",
                        "attempt": "att-returned",
                        "role": "worker",
                        "provider": "claude",
                        "joined": true,
                        "title": "Returned task"
                    },
                    {
                        "participant": "call-res-joined",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "state": "queued",
                        "title": "Result joined"
                    },
                    {
                        "participant": "call-res-left",
                        "role": "worker",
                        "provider": "codex",
                        "joined": false,
                        "state": "queued",
                        "title": "Result left"
                    },
                    {
                        "participant": "call-set-joined",
                        "role": "worker",
                        "provider": "claude",
                        "joined": true,
                        "state": "queued",
                        "title": "Settled joined"
                    },
                    {
                        "participant": "call-set-left",
                        "role": "worker",
                        "provider": "claude",
                        "joined": false,
                        "title": "Settled left"
                    }
                ],
                "call_results": {
                    "call-res-joined": {"attempt_id": "missing-1", "seq": 1},
                    "call-res-left": {"attempt_id": "missing-2", "seq": 2}
                }
            },
            "room_attempts": {
                "att-main": {
                    "attempt_id": "att-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-done": {
                    "attempt_id": "att-done",
                    "provider": "codex",
                    "state": "done",
                    "updated_at": now - 10,
                    "refs": {"panel_room_id": "room-demo", "panel_room_participant": "call-done"}
                },
                "att-failed": {
                    "attempt_id": "att-failed",
                    "provider": "codex",
                    "state": "failed",
                    "updated_at": now - 360,
                    "refs": {"panel_room_id": "room-demo", "panel_room_participant": "call-failed"}
                },
                "att-timeout": {
                    "attempt_id": "att-timeout",
                    "provider": "codex",
                    "state": "timed_out",
                    "updated_at": now - 360,
                    "refs": {"panel_room_id": "room-demo", "panel_room_participant": "call-timeout"}
                },
                "att-abandoned": {
                    "attempt_id": "att-abandoned",
                    "provider": "codex",
                    "state": "abandoned",
                    "updated_at": now - 360,
                    "refs": {"panel_room_id": "room-demo", "panel_room_participant": "call-abandoned"}
                },
                "att-cancelled": {
                    "attempt_id": "att-cancelled",
                    "provider": "codex",
                    "state": "cancelled",
                    "updated_at": now - 360,
                    "refs": {"panel_room_id": "room-demo", "panel_room_participant": "call-cancelled"}
                },
                "att-returned": {
                    "attempt_id": "att-returned",
                    "provider": "claude",
                    "state": "review",
                    "updated_at": now,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-returned",
                        "panel_access": "read"
                    }
                }
            },
            "returned_workers": {
                "att-returned": chrono::DateTime::from_timestamp(
                    i64::try_from(now.saturating_sub(15)).unwrap(),
                    0,
                )
                .unwrap()
                .to_rfc3339()
            },
            "room_dispatches": {
                "call-set-joined": {"room": "room-demo", "state": "settled", "created_at": now - 60},
                "call-set-left": {"room": "room-demo", "state": "settled", "created_at": now - 60}
            }
        });

        let all_calls = calls(&report);
        assert_eq!(all_calls.len(), 11, "{all_calls:?}");
        for id in [
            "call-res-joined",
            "call-res-left",
            "call-set-joined",
            "call-set-left",
        ] {
            let row = all_calls.iter().find(|r| r["participant"] == id).unwrap();
            assert_eq!(row["room_only_history"], true, "{id}: {row}");
            assert_eq!(text(row, "state"), "", "{id}: {row}");
        }

        let read = BTreeMap::new();
        let overview =
            frame(&report, &navigation::Navigation::default(), 140, 30, &read).join("\n");
        assert!(overview.contains("3 attention"), "{overview}");
        assert!(!overview.contains("running"), "{overview}");
        assert!(overview.contains("no visible work"), "{overview}");
        assert!(overview.contains("10 past (Tab all)"), "{overview}");
        assert!(!overview.contains("[⟳ run]"), "{overview}");

        let compact = frame(
            &report,
            &navigation::Navigation {
                view: View::Compact,
                ..Default::default()
            },
            140,
            40,
            &read,
        )
        .join("\n");
        assert!(compact.contains("Attention 3 · 3 failed"), "{compact}");
        let tasks_section = compact
            .split_once("╭─ Tasks · ")
            .map(|(_, rest)| rest)
            .expect("Tasks section in compact view");
        for title in [
            "Done task",
            "Failed old task",
            "Timed out task",
            "Abandoned task",
            "Cancelled task",
            "Returned task",
            "Result joined",
            "Result left",
            "Settled joined",
            "Settled left",
        ] {
            assert_eq!(
                tasks_section.matches(title).count(),
                1,
                "{title}: {compact}"
            );
        }
        assert!(!compact.contains("[⟳ run]"), "{compact}");
    }

    #[test]
    fn cross_room_isolation_rejects_foreign_contradictory_and_colliding_attempts() {
        let now = oms_panel_core::timestamp();
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-demo",
                "participants": [
                    {
                        "participant": "main-demo",
                        "attempt": "att-main-demo",
                        "role": "main",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "call-collide",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 15,
                        "title": "Local colliding participant"
                    },
                    {
                        "participant": "call-sibling-target",
                        "attempt": "att-sibling-claim",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 15,
                        "title": "Should not bind sibling"
                    },
                    {
                        "participant": "sibling",
                        "attempt": "att-sibling-own",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "state": "done",
                        "updated_at": now - 400,
                        "title": "Sibling own finished"
                    },
                    {
                        "participant": "call-foreign-dispatch",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 15,
                        "title": "No foreign dispatch leak"
                    }
                ]
            },
            "attempts": {
                "recent": [
                    {
                        "attempt_id": "att-foreign-recent",
                        "provider": "codex",
                        "state": "working",
                        "title": "Foreign recent leak",
                        "refs": {"panel_room_id": "room-other"}
                    },
                    {
                        "attempt_id": "call-collide",
                        "provider": "codex",
                        "state": "working",
                        "title": "Unroomed collision attempt",
                        "refs": {},
                        "usage": {"tokens": 777},
                        "usage_reports": {"tokens": 1}
                    }
                ],
                "active": [
                    {
                        "attempt_id": "att-foreign-active",
                        "provider": "claude",
                        "state": "working",
                        "title": "Foreign active leak",
                        "refs": {"panel_room": "room-other"}
                    },
                    {
                        "attempt_id": "att-contradict-1",
                        "provider": "codex",
                        "state": "working",
                        "title": "Contradictory room 1",
                        "refs": {"panel_room_id": "room-demo", "panel_room": "room-other"}
                    },
                    {
                        "attempt_id": "att-contradict-2",
                        "provider": "codex",
                        "state": "working",
                        "title": "Contradictory room 2",
                        "refs": {"panel_room_id": "room-other", "panel_room": "room-demo"}
                    }
                ]
            },
            "room_attempts": {
                "att-main-demo": {
                    "attempt_id": "att-main-demo",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-foreign-room-attempts": {
                    "attempt_id": "att-foreign-room-attempts",
                    "provider": "codex",
                    "state": "working",
                    "title": "Foreign room_attempts leak",
                    "refs": {"panel_room_id": "room-other"}
                },
                "att-sibling-claim": {
                    "attempt_id": "att-sibling-claim",
                    "provider": "codex",
                    "state": "working",
                    "title": "Sibling owned attempt",
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "sibling"
                    },
                    "usage": {"tokens": 888},
                    "usage_reports": {"tokens": 1}
                }
            },
            "room_dispatches": {
                "call-foreign-dispatch": {
                    "room": "room-other",
                    "state": "settled",
                    "task_id": "t-foreign",
                    "route": {
                        "role": "builder",
                        "purpose": "Foreign purpose",
                        "model": "foreign-model",
                        "access": "read"
                    }
                }
            }
        });

        let rows = calls(&report);
        let serialized = json!(rows).to_string();
        for forbidden in [
            "att-foreign-recent",
            "att-foreign-active",
            "att-foreign-room-attempts",
            "att-contradict-1",
            "att-contradict-2",
            "Foreign recent leak",
            "Foreign active leak",
            "Foreign room_attempts leak",
            "Contradictory room 1",
            "Contradictory room 2",
            "Unroomed collision attempt",
            "Foreign purpose",
            "foreign-model",
            "t-foreign",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "leaked {forbidden}: {serialized}"
            );
        }

        let collide = rows
            .iter()
            .find(|r| r["participant"] == "call-collide")
            .unwrap();
        assert_eq!(text(collide, "state"), "");
        assert_eq!(text(collide, "attempt"), "");
        assert!(collide["event_tokens"].is_null());

        let target = rows
            .iter()
            .find(|r| r["participant"] == "call-sibling-target")
            .unwrap();
        assert_eq!(text(target, "state"), "");
        assert!(target["event_tokens"].is_null());

        let foreign_dispatch = rows
            .iter()
            .find(|r| r["participant"] == "call-foreign-dispatch")
            .unwrap();
        assert!(foreign_dispatch["room_only_history"].is_null());
        assert_eq!(text(foreign_dispatch, "task_id"), "");

        // Parent, role, provider, re-enrollment sequence, and live-attempt preference checks.
        let custody = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "room": {
                "id": "room-demo",
                "owner": "main-demo",
                "participants": [
                    {
                        "participant": "main-demo",
                        "attempt": "att-main-demo",
                        "role": "main",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "call-parent-attempt",
                        "attempt": "att-main-demo",
                        "parent": "att-main-demo",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 15
                    },
                    {
                        "participant": "call-provider-mismatch",
                        "attempt": "att-claude",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 15
                    },
                    {
                        "participant": "call-reenrolled",
                        "role": "worker",
                        "provider": "codex",
                        "initial_seq": 1,
                        "seq": 2,
                        "joined": true,
                        "joined_at": now - 15
                    },
                    {
                        "participant": "call-prefer-live",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true
                    }
                ]
            },
            "attempts": {
                "recent": [
                    {
                        "attempt_id": "att-live-pref",
                        "provider": "codex",
                        "state": "working",
                        "refs": {
                            "panel_room_id": "room-demo",
                            "panel_room_participant": "call-prefer-live"
                        }
                    }
                ]
            },
            "room_attempts": {
                "att-main-demo": {
                    "attempt_id": "att-main-demo",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "refs": {"panel_room_id": "room-demo", "panel_role": "main"}
                },
                "att-claude": {
                    "attempt_id": "att-claude",
                    "provider": "claude",
                    "state": "working",
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-provider-mismatch"
                    }
                },
                "att-old-reenroll": {
                    "attempt_id": "att-old-reenroll",
                    "provider": "codex",
                    "state": "done",
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-reenrolled"
                    }
                },
                "att-dead-later": {
                    "attempt_id": "att-dead-later",
                    "provider": "codex",
                    "state": "done",
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_room_participant": "call-prefer-live"
                    }
                }
            }
        });
        let custody_rows = calls(&custody);
        for unattached in [
            "call-parent-attempt",
            "call-provider-mismatch",
            "call-reenrolled",
        ] {
            let row = custody_rows
                .iter()
                .find(|r| r["participant"] == unattached)
                .unwrap();
            assert_eq!(text(row, "state"), "", "{unattached}: {row}");
        }
        let prefer = custody_rows
            .iter()
            .find(|r| r["participant"] == "call-prefer-live")
            .unwrap();
        assert_eq!(prefer["attempt"], "att-live-pref");
        assert_eq!(prefer["state"], "working");
    }

    #[test]
    fn ux01_attention_band_shows_classes_unread_top_three_with_age_and_omits_when_empty() {
        let now = 1_800_000_600u64;
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-ux01",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "model": "gpt-5",
                        "state": "working",
                        "joined": true,
                        "title": "Coordinate UX"
                    },
                    {
                        "participant": "w-fail",
                        "role": "builder",
                        "provider": "claude",
                        "model": "sonnet",
                        "state": "failed",
                        "joined": true,
                        "title": "Fix parser",
                        "updated_at": now - 300
                    },
                    {
                        "participant": "w-rev",
                        "role": "reviewer",
                        "provider": "codex",
                        "model": "gpt-5",
                        "state": "review",
                        "joined": true,
                        "title": "Approve patch",
                        "updated_at": now - 120
                    },
                    {
                        "participant": "w-ended",
                        "role": "worker",
                        "provider": "codex",
                        "state": "done",
                        "joined": true,
                        "task_id": "t-held",
                        "updated_at": now - 60
                    }
                ],
                "messages": [
                    {
                        "id": "msg-q1",
                        "message_kind": "question",
                        "sender": "main-1",
                        "text": "Ship option A or B?",
                        "pending_for": ["user"],
                        "answered": false,
                        "ts": now - 420
                    },
                    {
                        "id": "msg-m1",
                        "message_kind": "note",
                        "sender": "advisor-1",
                        "text": "FYI fifth item note",
                        "pending_for": ["main-1"],
                        "answered": false,
                        "ts": now - 60
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-ux01",
                "goal": "Ship UX fixes",
                "tasks": {
                    "t-held": {
                        "id": "t-held",
                        "state": "running",
                        "claimed_at": now - 600
                    }
                }
            }
        });
        let items = attention::items(&report, &BTreeMap::new());
        assert_eq!(items.len(), 5, "{items:?}");
        let read_item = items
            .iter()
            .find(|item| item.row_id == ("call".into(), "w-fail".into()))
            .expect("failed call in attention");
        let read = BTreeMap::from([(attention::row_key(&read_item.row_id), read_item.cls.clone())]);

        for columns in [40, 80, 120] {
            let plain = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                30,
                &read,
                false,
                true,
            );
            let colored = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                30,
                &read,
                true,
                true,
            );
            assert_eq!(plain.len(), 30);
            for (p, c) in plain.iter().zip(&colored) {
                assert!(width(p) <= columns, "{columns}: overflow {p}");
                assert_eq!(*p, strip_sgr(c), "{columns}: sgr geometry mismatch");
            }
            let joined = plain.join("\n");
            assert!(joined.contains("Attention 5"), "{columns}: {joined}");
            assert!(
                joined.contains("4 new / 5") || joined.contains("4 new/5"),
                "{columns}: missing unread ratio: {joined}"
            );
            if columns >= 120 {
                for fragment in ["1 needs you", "1 failed", "1 decide", "1 stale", "1 mail"] {
                    assert!(
                        joined.contains(fragment),
                        "{columns}: missing {fragment}\n{joined}"
                    );
                }
            } else if columns >= 80 {
                for fragment in ["1 you", "1 fail", "1 dec", "1 stale", "1 mail"] {
                    assert!(
                        joined.contains(fragment),
                        "{columns}: missing {fragment}\n{joined}"
                    );
                }
            }
            if columns >= 80 {
                for fragment in [
                    "Ship option A or B?",
                    "Fix parser",
                    "Approve patch",
                    "main-1",
                    "sonnet builder",
                    "gpt-5 reviewer",
                    "7m",
                    "5m",
                    "2m",
                ] {
                    assert!(
                        joined.contains(fragment),
                        "{columns}: missing {fragment}\n{joined}"
                    );
                }
                assert!(
                    !joined.contains("FYI fifth item note"),
                    "{columns}: top-3 band should omit 5th item\n{joined}"
                );
            }
            let attn_start = plain
                .iter()
                .position(|line| line.contains("Attention 5"))
                .expect("Attention band start");
            let band_last_line = &plain[attn_start + 4];
            assert!(
                band_last_line.contains("+2 more · b shows all"),
                "{columns}: truncated Attention band last line missing overflow footer: {band_last_line:?}\n{joined}"
            );
        }

        // Non-truncated band (<= 3 items) must not show a "+N more" footer.
        let mut three_items = report.clone();
        three_items["room"]["messages"]
            .as_array_mut()
            .unwrap()
            .retain(|msg| msg["id"] != "msg-m1");
        three_items["plan"]["tasks"] = json!({});
        let three_joined = frame(
            &three_items,
            &navigation::Navigation::default(),
            80,
            30,
            &read,
        )
        .join("\n");
        assert!(
            !three_joined.contains("more · b shows all"),
            "non-truncated band should omit overflow footer:\n{three_joined}"
        );

        // Four non-zero classes at 80 cols keep the full class words per contract.
        let mut four_class = report.clone();
        four_class["room"]["messages"]
            .as_array_mut()
            .unwrap()
            .retain(|msg| msg["id"] != "msg-m1");
        let four_joined = frame(
            &four_class,
            &navigation::Navigation::default(),
            80,
            30,
            &read,
        )
        .join("\n");
        assert!(
            four_joined.contains("Attention 4 · 1 needs you · 1 failed · 1 decide · 1 stale"),
            "{four_joined}"
        );

        // Undated attention item never fabricates an age.
        let mut undated = report.clone();
        undated["room"]["messages"][0]["ts"] = Value::Null;
        let undated_lines = frame(&undated, &navigation::Navigation::default(), 80, 30, &read);
        let q_line = undated_lines
            .iter()
            .find(|line| line.contains("Ship option A or B?"))
            .expect("question line");
        assert!(
            !q_line.contains("7m") && !q_line.contains("0s"),
            "undated attention row fabricated age: {q_line}"
        );

        // Empty attention omits the Attention band completely.
        let clean_report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-clean",
                "owner": "main-1",
                "participants": [
                    {"participant": "main-1", "role": "main", "provider": "codex", "state": "working", "joined": true},
                    {"participant": "w-ok", "role": "worker", "provider": "codex", "state": "working", "joined": true, "title": "Healthy"}
                ]
            }
        });
        let empty_lines = frame(
            &clean_report,
            &navigation::Navigation::default(),
            100,
            24,
            &BTreeMap::new(),
        );
        let body_lines = &empty_lines[..empty_lines.len().saturating_sub(1)];
        assert!(
            body_lines
                .iter()
                .all(|line| !line.contains("Attention ") && !line.contains("new /")),
            "{empty_lines:#?}"
        );
    }

    #[test]
    fn ux01_attention_only_mode_includes_room_questions_and_held_tasks() {
        let now = 1_800_000_600u64;
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-attn-b",
                "owner": "main-1",
                "participants": [
                    {"participant": "main-1", "role": "main", "provider": "codex", "state": "working", "joined": true},
                    {"participant": "w-ok", "role": "worker", "provider": "codex", "state": "working", "joined": true, "title": "Healthy worker"},
                    {"participant": "w-ended", "role": "worker", "provider": "codex", "state": "done", "joined": true, "task_id": "t-held", "updated_at": now - 400}
                ],
                "messages": [
                    {
                        "id": "msg-q1",
                        "message_kind": "question",
                        "sender": "main-1",
                        "text": "Approve release scope?",
                        "pending_for": ["user"],
                        "answered": false,
                        "ts": now - 180
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-b",
                "goal": "Check attention filter",
                "tasks": {
                    "t-held": {
                        "id": "t-held",
                        "state": "claimed",
                        "claimed_at": now - 600
                    }
                }
            }
        });
        let nav = navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let rows = visible(&report, &nav);
        assert!(
            rows.iter().any(|r| r["participant"] == "message:msg-q1"),
            "room question missing from attention_only visible: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r["participant"] == "task:t-held"),
            "held task missing from attention_only visible: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r["participant"] == "w-ok"),
            "healthy worker leaked into attention_only: {rows:?}"
        );
        let read = BTreeMap::new();
        for columns in [40, 80, 120] {
            let lines = frame(&report, &nav, columns, 24, &read);
            assert!(
                lines.iter().all(|line| width(line) <= columns),
                "{columns}: {lines:?}"
            );
            let joined = lines.join("\n");
            assert!(joined.contains("[? question]"), "{columns}: {joined}");
            assert!(joined.contains("[! held]"), "{columns}: {joined}");
            assert!(joined.contains("t-held"), "{columns}: {joined}");
            assert!(
                !joined.contains("Task t-held · Task t-held"),
                "{columns}: {joined}"
            );
            if columns >= 80 {
                assert!(
                    joined.contains("Approve release scope?"),
                    "{columns}: {joined}"
                );
                assert!(
                    joined.contains("workers ended, still held"),
                    "{columns}: {joined}"
                );
            }
            assert!(
                !joined.contains("No visible lead or worker calls"),
                "{columns}: {joined}"
            );
            assert!(
                !joined.contains("new /")
                    && !joined.contains("new/")
                    && !joined.contains("Attention 2"),
                "{columns}: Attention-only mode must not duplicate the Attention band\n{joined}"
            );
        }
        // Filtering and Detail overlay remain usable on message and task rows in attention-only mode.
        let filtered_nav = navigation::Navigation {
            attention_only: true,
            filter: "release".into(),
            ..Default::default()
        };
        let filtered_rows = visible(&report, &filtered_nav);
        assert!(
            filtered_rows
                .iter()
                .any(|r| r["participant"] == "message:msg-q1")
                && !filtered_rows
                    .iter()
                    .any(|r| r["participant"] == "task:t-held"),
            "{filtered_rows:?}"
        );
        let detail_nav = navigation::Navigation {
            attention_only: true,
            selected: 1,
            detail: true,
            ..Default::default()
        };
        let detail = frame(&report, &detail_nav, 80, 24, &read).join("\n");
        assert!(
            detail.contains("Call Detail") && detail.contains("[? question]"),
            "{detail}"
        );
    }

    #[test]
    fn ux07_goal_wraps_full_text_and_shows_verified_progress_and_unknown_denominator() {
        let now = 1_800_000_600u64;
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "project_name": "oms-demo",
            "room": {
                "id": "room-goal",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "state": "working",
                        "joined": true,
                        "title": "Main",
                        "refs": {"plan_id": "plan-ux07", "panel_room_id": "room-goal"}
                    },
                    {"participant": "w-stale", "role": "worker", "provider": "codex", "state": "done", "joined": true, "task_id": "t-stale", "updated_at": now - 400}
                ]
            },
            "plan": {
                "plan_id": "plan-ux07",
                "goal": "Deliver wrapped terminal goal progress with verified counts across narrow normal and wide panes",
                "tasks": {
                    "t-done-1": {"state": "done"},
                    "t-done-2": {"state": "done"},
                    "t-rev": {"state": "review"},
                    "t-claimed": {"state": "claimed"},
                    "t-stale": {"state": "running"},
                    "t-open": {"state": "ready"},
                    "t-cancel": {"state": "cancelled"}
                }
            }
        });
        let read = BTreeMap::new();
        for columns in [40, 80, 120] {
            let plain = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                28,
                &read,
                false,
                true,
            );
            let colored = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                28,
                &read,
                true,
                true,
            );
            for (p, c) in plain.iter().zip(&colored) {
                assert!(width(p) <= columns, "{columns}: overflow {p}");
                assert_eq!(*p, strip_sgr(c), "{columns}: sgr geometry mismatch");
            }
            let header = &plain[0];
            assert!(
                header.contains("2/6 verified") || header.contains("2/6 ver"),
                "{columns}: header missing plan progress: {header}"
            );
            let joined = plain.join("\n");
            // Wrapped goal spans up to 2 lines (full at 80/120 cols; 2-line cap with ellipsis at 40 cols).
            for word in ["Deliver", "wrapped", "terminal", "verified", "narrow"] {
                assert!(
                    joined.contains(word),
                    "{columns}: goal missing {word}\n{joined}"
                );
            }
            if columns >= 80 {
                assert!(
                    joined.contains("panes"),
                    "{columns}: goal clipped missing panes\n{joined}"
                );
            } else {
                assert!(
                    joined.contains('…'),
                    "{columns}: 3rd wrapped line should cap with ellipsis\n{joined}"
                );
            }
            assert!(joined.contains("2/6 verified"), "{columns}: {joined}");
            assert!(
                !joined.contains("2/7"),
                "{columns}: cancelled task inflated denominator\n{joined}"
            );
            if columns >= 80 {
                assert!(joined.contains("1 in review"), "{columns}: {joined}");
                assert!(joined.contains("1 claimed"), "{columns}: {joined}");
                assert!(joined.contains("1 stale"), "{columns}: {joined}");
            }
        }

        // Unknown denominator (missing or empty tasks) says unknown instead of 0/0.
        for tasks in [Value::Null, json!({})] {
            let unknown_report = json!({
                "generated_at": now,
                "collection": {"ok": true},
                "capacity_stale": false,
                "project_name": "oms-demo",
                "room": {
                    "id": "room-unknown",
                    "owner": "main-1",
                    "participants": [
                        {
                            "participant": "main-1",
                            "role": "main",
                            "provider": "codex",
                            "state": "working",
                            "joined": true,
                            "refs": {"plan_id": "plan-empty", "panel_room_id": "room-unknown"}
                        }
                    ]
                },
                "plan": {
                    "plan_id": "plan-empty",
                    "goal": "Bootstrap plan before adding tasks",
                    "tasks": tasks
                }
            });
            let joined = frame(
                &unknown_report,
                &navigation::Navigation::default(),
                80,
                24,
                &read,
            )
            .join("\n");
            assert!(
                joined.contains("Bootstrap plan before adding tasks"),
                "{joined}"
            );
            assert!(joined.to_lowercase().contains("unknown"), "{joined}");
            assert!(
                !joined.contains("0/0"),
                "must never fabricate 0/0: {joined}"
            );
        }

        // All-cancelled bound plan is terminal and hides the Goal cleanly.
        let mut all_cancelled = report.clone();
        all_cancelled["plan"]["tasks"] = json!({
            "t1": {"state": "cancelled"},
            "t2": {"state": "canceled"}
        });
        assert_eq!(
            room_plan(
                &all_cancelled,
                &calls(&all_cancelled),
                &attention::items(&all_cancelled, &read),
            ),
            None
        );
        let cancelled_joined = frame(
            &all_cancelled,
            &navigation::Navigation::default(),
            80,
            24,
            &read,
        )
        .join("\n");
        assert!(
            !cancelled_joined.contains("Goal:") && !cancelled_joined.contains("progress unknown"),
            "{cancelled_joined}"
        );

        // A Room call from an older plan reusing a task_id must not mark the current
        // plan's claimed/running task stale in room_plan, Goal row, or header.
        let mut foreign_plan_task_reuse = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "project_name": "oms-demo",
            "room": {
                "id": "room-reuse",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "state": "working",
                        "joined": true,
                        "refs": {"plan_id": "plan-new", "panel_room_id": "room-reuse"}
                    },
                    {
                        "participant": "w-old-ended",
                        "role": "worker",
                        "provider": "codex",
                        "state": "done",
                        "joined": true,
                        "task_id": "t-1",
                        "updated_at": now - 400,
                        "refs": {"plan_id": "plan-old", "panel_room_id": "room-reuse"}
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-new",
                "goal": "Current plan with reused task id",
                "tasks": {
                    "t-1": {"id": "t-1", "state": "running"},
                    "t-2": {"id": "t-2", "state": "done"}
                }
            }
        });
        let reuse_attention = attention::items(&foreign_plan_task_reuse, &read);
        assert!(
            reuse_attention.is_empty(),
            "old-plan call must not emit stale attention item: {reuse_attention:?}"
        );
        let reuse_progress = room_plan(
            &foreign_plan_task_reuse,
            &calls(&foreign_plan_task_reuse),
            &reuse_attention,
        )
        .expect("bound current plan");
        assert_eq!(reuse_progress.stale, 0, "{reuse_progress:?}");
        assert_eq!(reuse_progress.claimed, 1, "{reuse_progress:?}");
        let reuse_joined = frame(
            &foreign_plan_task_reuse,
            &navigation::Navigation::default(),
            100,
            24,
            &read,
        )
        .join("\n");
        assert!(
            !reuse_joined.contains("1 stale"),
            "Goal row/header falsely reported stale without matching Attention item:\n{reuse_joined}"
        );
        // Conversely, an active call from an older plan reusing t-1 must not mask a
        // genuinely stale current-plan task whose current-plan worker already ended.
        foreign_plan_task_reuse["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "participant": "w-cur-ended",
                "role": "worker",
                "provider": "codex",
                "state": "done",
                "joined": true,
                "task_id": "t-1",
                "updated_at": now - 120,
                "refs": {"plan_id": "plan-new", "panel_room_id": "room-reuse"}
            }));
        foreign_plan_task_reuse["room"]["participants"][1]["state"] = json!("working");
        let current_stale_attention = attention::items(&foreign_plan_task_reuse, &read);
        assert!(
            current_stale_attention
                .iter()
                .any(|item| item.row_id == ("task".into(), "t-1".into()) && item.cls == "stale"),
            "current-plan stale task missing from attention: {current_stale_attention:?}"
        );
        let current_stale_progress = room_plan(
            &foreign_plan_task_reuse,
            &calls(&foreign_plan_task_reuse),
            &current_stale_attention,
        )
        .expect("bound current plan");
        assert_eq!(
            current_stale_progress.stale, 1,
            "{current_stale_progress:?}"
        );

        // Decide-waiting finished worker and failed worker must NOT be counted stale
        // by Goal progress; Goal row and Attention band must agree on both.
        let decide_and_failed = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "project_name": "oms-demo",
            "awaiting_admission": ["w-decide"],
            "room": {
                "id": "room-agree",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "state": "working",
                        "joined": true,
                        "refs": {"plan_id": "plan-agree", "panel_room_id": "room-agree"}
                    },
                    {
                        "participant": "w-decide",
                        "role": "worker",
                        "provider": "codex",
                        "state": "done",
                        "joined": true,
                        "task_id": "t-decide",
                        "title": "Waiting on accept/reject",
                        "updated_at": now - 120,
                        "refs": {"plan_id": "plan-agree", "panel_room_id": "room-agree"}
                    },
                    {
                        "participant": "w-failed",
                        "role": "worker",
                        "provider": "claude",
                        "state": "failed",
                        "joined": true,
                        "task_id": "t-failed",
                        "title": "Build failed",
                        "updated_at": now - 180,
                        "refs": {"plan_id": "plan-agree", "panel_room_id": "room-agree"}
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-agree",
                "goal": "Keep Goal and Attention counts aligned",
                "tasks": {
                    "t-decide": {"id": "t-decide", "state": "running"},
                    "t-failed": {"id": "t-failed", "state": "claimed"},
                    "t-done": {"id": "t-done", "state": "done"}
                }
            }
        });
        let agree_attention = attention::items(&decide_and_failed, &read);
        assert!(
            agree_attention
                .iter()
                .any(|item| item.cls == "decide" && item.row_id.1 == "w-decide"),
            "decide worker missing from attention: {agree_attention:?}"
        );
        assert!(
            agree_attention
                .iter()
                .any(|item| item.cls == "failed" && item.row_id.1 == "w-failed"),
            "failed worker missing from attention: {agree_attention:?}"
        );
        assert!(
            agree_attention.iter().all(|item| item.cls != "stale"),
            "decide/failed workers must not emit stale attention items: {agree_attention:?}"
        );
        let agree_progress = room_plan(
            &decide_and_failed,
            &calls(&decide_and_failed),
            &agree_attention,
        )
        .expect("bound plan");
        assert_eq!(agree_progress.stale, 0, "{agree_progress:?}");
        assert_eq!(agree_progress.claimed, 2, "{agree_progress:?}");
        let agree_joined = frame(
            &decide_and_failed,
            &navigation::Navigation::default(),
            120,
            24,
            &read,
        )
        .join("\n");
        assert!(
            !agree_joined.contains("stale"),
            "Goal row and Attention band must agree (0 stale):\n{agree_joined}"
        );
        assert!(
            agree_joined.contains("1 failed") && agree_joined.contains("1 decide"),
            "Attention band must show failed and decide:\n{agree_joined}"
        );
    }

    #[test]
    fn ux07_long_goal_caps_at_two_lines_to_preserve_attention_items_on_24_row_board() {
        let now = 1_800_000_600u64;
        let long_goal = "Deliver a comprehensive multi-stage verification pipeline with deterministic replay, bounded terminal layout, full goal visibility in the detail overlay, and uninterrupted visibility of high-priority attention items even on compact twenty-four row terminal viewports tailmarker";
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "project_name": "oms-demo",
            "room": {
                "id": "room-24",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "state": "working",
                        "joined": true,
                        "title": "Coordinate release",
                        "refs": {"plan_id": "plan-24", "panel_room_id": "room-24"}
                    },
                    {
                        "participant": "w-fail",
                        "role": "builder",
                        "provider": "claude",
                        "model": "sonnet",
                        "state": "failed",
                        "joined": true,
                        "task_id": "t-1",
                        "title": "Fix parser crash",
                        "updated_at": now - 300,
                        "refs": {"plan_id": "plan-24", "panel_room_id": "room-24"}
                    },
                    {
                        "participant": "w-rev",
                        "role": "reviewer",
                        "provider": "codex",
                        "model": "gpt-5",
                        "state": "review",
                        "joined": true,
                        "task_id": "t-2",
                        "title": "Review auth diff",
                        "updated_at": now - 120,
                        "refs": {"plan_id": "plan-24", "panel_room_id": "room-24"}
                    },
                    {
                        "participant": "w-ok",
                        "role": "worker",
                        "provider": "antigravity",
                        "state": "working",
                        "joined": true,
                        "task_id": "t-3",
                        "title": "Run smoke checks",
                        "updated_at": now - 30,
                        "refs": {"plan_id": "plan-24", "panel_room_id": "room-24"}
                    }
                ],
                "messages": [
                    {
                        "id": "msg-q1",
                        "message_kind": "question",
                        "sender": "main-1",
                        "text": "Approve staging deploy?",
                        "pending_for": ["user"],
                        "answered": false,
                        "ts": now - 420
                    },
                    {
                        "id": "msg-m1",
                        "message_kind": "note",
                        "sender": "advisor-1",
                        "text": "Fourth item note",
                        "pending_for": ["main-1"],
                        "answered": false,
                        "ts": now - 60
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-24",
                "goal": long_goal,
                "tasks": {
                    "t-1": {"id": "t-1", "state": "claimed"},
                    "t-2": {"id": "t-2", "state": "review"},
                    "t-3": {"id": "t-3", "state": "running"},
                    "t-4": {"id": "t-4", "state": "done"}
                }
            }
        });
        let read = BTreeMap::new();
        for columns in [40, 80, 120] {
            let plain = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                24,
                &read,
                false,
                true,
            );
            let colored = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                24,
                &read,
                true,
                true,
            );
            assert_eq!(plain.len(), 24);
            for (p, c) in plain.iter().zip(&colored) {
                assert!(width(p) <= columns, "{columns}: overflow {p}");
                assert_eq!(*p, strip_sgr(c), "{columns}: sgr geometry mismatch");
            }
            let joined = plain.join("\n");
            // Goal is capped at 2 wrapped lines with ellipsis (tailmarker clipped on board).
            assert!(
                !joined.contains("tailmarker"),
                "{columns}: long goal was not capped at 2 lines:\n{joined}"
            );
            let goal_idx = plain
                .iter()
                .position(|line| line.contains("Goal:"))
                .expect("Goal header row");
            let goal_body_end = plain[goal_idx + 1..]
                .iter()
                .position(|line| line.starts_with('╰') || line.contains("Attention "))
                .map(|off| goal_idx + 1 + off)
                .expect("end of Goal section");
            let goal_body_lines = &plain[goal_idx + 1..goal_body_end];
            assert_eq!(
                goal_body_lines.len(),
                2,
                "{columns}: expected 2 wrapped Goal lines, got {goal_body_lines:?}\n{joined}"
            );
            assert!(
                goal_body_lines[1]
                    .trim_end_matches(['│', ' '])
                    .ends_with('…'),
                "{columns}: second wrapped Goal line missing ellipsis: {:?}\n{joined}",
                goal_body_lines[1]
            );
            // Attention band keeps its items (not shrunk to header only) on 24-row board.
            assert!(joined.contains("Attention 4"), "{columns}: {joined}");
            assert!(
                joined.contains("main-1"),
                "{columns}: attention item 1 missing\n{joined}"
            );
            assert!(
                joined.contains("+1 more · b shows all"),
                "{columns}: attention overflow footer missing on 24-row board\n{joined}"
            );
            if columns >= 80 {
                assert!(
                    joined.contains("Approve staging deploy?"),
                    "{columns}: attention item 1 body missing\n{joined}"
                );
                assert!(
                    joined.contains("Fix parser crash"),
                    "{columns}: attention item 2 missing\n{joined}"
                );
                assert!(
                    joined.contains("Review auth diff"),
                    "{columns}: attention item 3 missing\n{joined}"
                );
            }
        }
        // Full goal text stays visible in Overall Main detail view.
        let detail_nav = navigation::Navigation {
            selected: 0,
            detail: true,
            ..Default::default()
        };
        let detail_joined = frame(&report, &detail_nav, 80, 24, &read).join("\n");
        assert!(
            detail_joined.contains("tailmarker"),
            "Main detail view must keep full Goal text:\n{detail_joined}"
        );
    }

    #[test]
    fn room_plan_hides_unbound_workspace_plan_even_without_foreign_attempts() {
        let now = 1_800_000_600u64;
        // Unrelated room with no room.plan_id, no room.task_id, no call plan_id refs,
        // no task claims, and empty attempts (no att_foreign in active/recent).
        let unbound_empty_attempts = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "project_name": "oms-demo",
            "room": {
                "id": "room-unrelated",
                "owner": "main-other",
                "participants": [
                    {
                        "participant": "main-other",
                        "role": "main",
                        "provider": "codex",
                        "state": "working",
                        "joined": true,
                        "title": "Independent work"
                    }
                ]
            },
            "attempts": {"active": [], "recent": []},
            "plan": {
                "plan_id": "plan-other-room",
                "goal": "Secret other room goal",
                "tasks": {
                    "t1": {"id": "t1", "state": "done"},
                    "t2": {"id": "t2", "state": "running"}
                }
            }
        });
        assert_eq!(
            room_plan(
                &unbound_empty_attempts,
                &calls(&unbound_empty_attempts),
                &attention::items(&unbound_empty_attempts, &BTreeMap::new()),
            ),
            None
        );
        let rendered = frame_styled(
            &unbound_empty_attempts,
            &navigation::Navigation::default(),
            80,
            24,
            &BTreeMap::new(),
            false,
            true,
        )
        .join("\n");
        assert!(
            !rendered.contains("Goal:")
                && !rendered.contains("Secret other room goal")
                && !rendered.contains("1/2 verified"),
            "unrelated room leaked workspace plan without att_foreign:\n{rendered}"
        );

        // Same check on top_bar_report with att_foreign removed and worker refs cleared.
        let mut no_foreign = top_bar_report();
        no_foreign["room"]["task_id"] = Value::Null;
        no_foreign["attempts"] = json!({"active": [], "recent": []});
        for index in [1, 2] {
            no_foreign["room"]["participants"][index]["refs"] = json!({});
        }
        assert_eq!(
            room_plan(
                &no_foreign,
                &calls(&no_foreign),
                &attention::items(&no_foreign, &BTreeMap::new()),
            ),
            None
        );

        // Contradictory room refs on a call must not bind this room.
        let mut contradictory = no_foreign.clone();
        contradictory["room"]["participants"][1]["refs"] = json!({
            "plan_id": "plan-1",
            "panel_room_id": "room-x",
            "panel_room": "room-other"
        });
        assert_eq!(
            room_plan(
                &contradictory,
                &calls(&contradictory),
                &attention::items(&contradictory, &BTreeMap::new()),
            ),
            None
        );
    }
}
