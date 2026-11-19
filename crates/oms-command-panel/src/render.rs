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
    let gap_min = usize::from(right_w > 0 && inner_width > right_w);
    let left_budget = inner_width.saturating_sub(right_w + gap_min);
    let left = clip_spans(left, left_budget);
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

fn role_style(role: &str) -> (&'static str, &'static str) {
    match role {
        "main" => ("■", SGR_AMBER_BOLD),
        "reviewer" | "verifier" | "security-auditor" => ("◆", SGR_VIOLET_BOLD),
        "advisor" | "architect" | "critic" | "researcher" | "scholar" => ("◇", SGR_SKY_BOLD),
        _ => ("●", SGR_SAGE_BOLD),
    }
}

fn state_badge(state: &str) -> (String, &'static str) {
    match state {
        "working" | "running" | "starting" | "queued" => ("[⟳ run]".into(), SGR_CYAN),
        "verifying" => ("[⟳ verify]".into(), SGR_CYAN),
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

fn view_label(view: View) -> &'static str {
    match view {
        View::Auto => "Teams",
        View::Compact => "Compact",
        View::Summary => "Summary",
        View::Detail => "Detail",
        View::Graph => "Graph",
        View::Tree => "Tree",
    }
}

pub fn calls(report: &Value) -> Vec<Value> {
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
        if !aid.is_empty() {
            by_attempt.insert(aid.to_owned(), row);
        }
        let pid = text(&row["refs"], "panel_room_participant");
        if !pid.is_empty() {
            by_participant.insert(pid.to_owned(), row);
        }
    }

    let room_id = text(&report["room"], "id");
    let participants = rows(&report["room"]["participants"]);
    let has_participants = !participants.is_empty();
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cached_agy_model: Option<Option<String>> = None;
    for member in participants {
        let attempt = text(member, "attempt");
        let participant = text(member, "participant");
        let matched = (!attempt.is_empty())
            .then(|| by_attempt.get(attempt).copied())
            .flatten()
            .or_else(|| {
                (!participant.is_empty())
                    .then(|| {
                        by_participant
                            .get(participant)
                            .copied()
                            .or_else(|| by_attempt.get(participant).copied())
                    })
                    .flatten()
            });
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
        ] {
            if let Some(v) = member.get(key).filter(|v| !v.is_null() && *v != "") {
                call[key] = v.clone();
            }
        }
        let observed = &report["main_model_observation"];
        if call["role"] == "main"
            && call["provider"] == "codex"
            && !text(&call, "attempt").is_empty()
            && call["attempt"] == observed["attempt"]
            && let Some(model) = observed["model"].as_str().filter(|v| !v.is_empty())
        {
            call["model"] = json!(model);
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
        if text(&call, "created_at").is_empty() && !text(member, "joined_at").is_empty() {
            call["created_at"] = member["joined_at"].clone();
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
        if id.is_empty() || seen.contains(&id) {
            continue;
        }
        let pid = text(&attempt["refs"], "panel_room_participant");
        if !pid.is_empty() && seen.contains(pid) {
            continue;
        }
        if !room_id.is_empty() {
            let att_room = [
                text(&attempt["refs"], "panel_room_id"),
                text(&attempt["refs"], "panel_room"),
            ]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or("");
            if (!att_room.is_empty() && att_room != room_id)
                || (att_room.is_empty() && has_participants)
            {
                continue;
            }
        }
        output.push(normalize(attempt, &mut cached_agy_model));
    }
    output.extend(rows(&report["native_children"]).iter().cloned());
    for call in &mut output {
        let finished = &report["returned_workers"][text(call, "attempt")];
        if call["state"] == "review"
            && call["refs"]["panel_access"] == "read"
            && oms_panel_core::metrics::stamp(finished).is_some()
        {
            call["returned_at"] = finished.clone();
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
    output
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
        "state":row["state"],"ending":row["reason_code"],"updated_at":row["updated_at"],"created_at":row["created_at"],"refs":refs})
}

pub fn visible(report: &Value, nav: &navigation::Navigation) -> Vec<Value> {
    let filtered = visible_from(calls(report), nav);
    if matches!(nav.view, View::Tree | View::Graph) {
        collect_tree(&filtered, nav, nav.view == View::Graph, usize::MAX)
            .into_iter()
            .map(|entry| entry.call.clone())
            .collect()
    } else {
        filtered
    }
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

fn model_label(call: &Value) -> &str {
    call["model"]
        .as_str()
        .filter(|model| !model.is_empty())
        .unwrap_or("model —")
}

fn model_and_state(
    call: &Value,
    columns: usize,
    badge: String,
    badge_style: &'static str,
) -> Vec<Span> {
    let mut spans = Vec::new();
    if columns >= 44 {
        spans.extend([
            Span::raw(
                model_label(call),
                (columns / 4).max(12),
                Some(provider_style(provider_bucket(call))),
            ),
            Span::literal("  ", None),
        ]);
    }
    spans.push(Span::raw(badge, 20, Some(badge_style)));
    spans
}

fn provider_groups(calls: &[Value]) -> Vec<usize> {
    let preferred = calls
        .iter()
        .find(|call| {
            text(call, "role") == "main"
                && oms_panel_core::state::LIVE_STATES.contains(&text(call, "state"))
        })
        .map(provider_bucket)
        .unwrap_or(0);
    let mut groups = vec![0, 1, 2];
    if calls.iter().any(|call| provider_bucket(call) == 3) {
        groups.push(3);
    }
    groups.sort_by_key(|group| (*group != preferred, *group));
    groups
}

fn unique_live_main(calls: &[Value]) -> Option<&Value> {
    let mut mains = calls.iter().filter(|call| {
        call["role"] == "main" && oms_panel_core::state::LIVE_STATES.contains(&text(call, "state"))
    });
    let main = mains.next()?;
    if mains.next().is_some() {
        return None;
    }
    let identities = [text(main, "participant"), text(main, "attempt")];
    if identities.iter().all(|id| id.is_empty()) {
        return None;
    }
    for id in identities.into_iter().filter(|id| !id.is_empty()) {
        if calls
            .iter()
            .filter(|call| text(call, "participant") == id || text(call, "attempt") == id)
            .count()
            != 1
        {
            return None;
        }
    }
    Some(main)
}

fn direct_main_work(call: &Value, main: Option<&Value>) -> bool {
    let Some(main) = main else {
        return false;
    };
    let parent = text(call, "parent");
    !parent.is_empty()
        && call["native_child"] != true
        && !matches!(
            text(call, "role"),
            "main" | "lead" | "team_lead" | "orchestrator"
        )
        && [text(main, "participant"), text(main, "attempt")].contains(&parent)
}

fn collapsed_history(nav: &navigation::Navigation, call: &Value) -> bool {
    nav.view == View::Auto
        && nav.filter.is_empty()
        && !nav.attention_only
        && (oms_panel_core::state::TERMINAL_STATES.contains(&text(call, "state"))
            || call["returned_at"].is_string())
        && oms_panel_core::metrics::stamp(&call["returned_at"])
            .or_else(|| oms_panel_core::metrics::stamp(&call["updated_at"]))
            .is_none_or(|finished| (oms_panel_core::timestamp() as f64 - finished) >= 300.0)
}

fn visible_from(all: Vec<Value>, nav: &navigation::Navigation) -> Vec<Value> {
    let groups = provider_groups(&all);
    let main = unique_live_main(&all).cloned();
    let words = navigation::words(&nav.filter);
    let mut filtered: Vec<_> = all
        .into_iter()
        .filter(|v| nav.view != View::Auto || v["role"] != "main")
        .filter(|v| !collapsed_history(nav, v))
        .filter(|v| navigation::matches_words(v, &words))
        .filter(|v| {
            !nav.attention_only
                || [
                    "waiting_input",
                    "waiting_approval",
                    "blocked",
                    "review",
                    "orphaned",
                    "failed",
                    "timed_out",
                    "abandoned",
                ]
                .contains(&text(v, "state"))
        })
        .collect();
    if nav.view == View::Auto {
        filtered.sort_by_key(|call| {
            (
                !direct_main_work(call, main.as_ref()),
                groups
                    .iter()
                    .position(|group| *group == provider_bucket(call)),
            )
        });
    }
    filtered
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

pub(crate) struct BoardFrame {
    pub lines: Vec<String>,
    pub provider_rows: [Option<usize>; 3],
    pub worker_rows: BTreeMap<usize, (String, String)>,
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
    frame_with_capacity(
        report,
        nav,
        columns,
        height,
        read,
        colored,
        fill_height,
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
    let mut capacity = 1;
    frame_with_capacity(
        report,
        nav,
        columns,
        height,
        read,
        false,
        false,
        &mut capacity,
    );
    capacity
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
    page_capacity: &mut usize,
) -> BoardFrame {
    let columns = columns.max(1);
    let height = height.max(3);
    let mut provider_rows = [None; 3];
    let mut worker_rows = BTreeMap::new();
    let use_cards = columns >= 44;
    let inner_width = if use_cards {
        columns.saturating_sub(4)
    } else {
        columns
    };
    let all = calls(report);
    let total = all
        .iter()
        .filter(|call| nav.view != View::Auto || call["role"] != "main")
        .count();
    let history = all
        .iter()
        .filter(|call| collapsed_history(nav, call))
        .count();
    let filtered = visible_from(all.clone(), nav);
    let filtered_count = filtered.len();
    let calls = if matches!(nav.view, View::Tree | View::Graph) {
        collect_tree(&filtered, nav, nav.view == View::Graph, usize::MAX)
            .into_iter()
            .map(|entry| entry.call.clone())
            .collect()
    } else {
        filtered
    };
    let attention = attention::items(report, read);
    let view = nav.view.resolved(columns, height);

    let mut lines = Vec::new();
    lines.push(header_line(
        report,
        &calls,
        &attention,
        if nav.view == View::Auto && columns >= 44 && height >= 20 {
            View::Auto
        } else {
            view
        },
        columns,
        colored,
    ));
    let main = render_main_section(report, &all, columns, height, colored);
    let mut optional = Vec::new();
    if height >= 14 {
        optional.push(provider_line(report, columns, colored));
        if let Some(meta) = status_subheader(report, columns, colored) {
            optional.push(meta);
        }
    }
    if columns >= 68 && height >= 18 && !nav.help && !nav.health && !nav.detail {
        optional.push(legend_line(colored, columns));
    }

    if report["collection"]["ok"] != true || report["capacity_stale"] == true {
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

    if nav.help || nav.health || nav.detail {
        lines.extend(optional.iter().cloned());
        lines.extend(notices.iter().cloned());
        lines.extend(main.iter().cloned());
    }
    if nav.help {
        lines.extend(render_help_card(columns, inner_width, use_cards, colored));
    } else if nav.health {
        lines.extend(render_health_card(
            report,
            columns,
            inner_width,
            use_cards,
            colored,
        ));
    } else if nav.detail {
        lines.extend(render_detail_card(
            calls.get(nav.selected),
            columns,
            inner_width,
            use_cards,
            colored,
        ));
    } else {
        if !nav.filter.is_empty() || nav.editing {
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

        let groups = provider_groups(&all);
        let main_identity = unique_live_main(&all);
        let direct_work = calls
            .iter()
            .any(|call| direct_main_work(call, main_identity));
        let provider_chrome = groups.len() + 1 + 2 * usize::from(direct_work);
        // Reserve the footer, omission count, and at least one selectable row.
        let provider_view = nav.view == View::Auto
            && use_cards
            && height.saturating_sub(lines.len() + main.len()) >= provider_chrome + 3;
        let content_start = lines.len() + main.len();
        let activity_chrome = usize::from(!provider_view && content_start + 4 < height);
        let bottom_chrome =
            !provider_view && use_cards && content_start + activity_chrome + 4 < height;
        let chrome = if provider_view {
            provider_chrome
        } else {
            activity_chrome + usize::from(bottom_chrome)
        };
        let reserved = 3 + chrome;
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
        lines.extend(main);
        let row_capacity = height.saturating_sub(lines.len() + chrome + 2).max(1);
        let detail_rows = !provider_view && view == View::Detail && row_capacity >= 2;
        let capacity = if detail_rows {
            row_capacity / 2
        } else {
            row_capacity
        };
        *page_capacity = capacity;
        let selected = nav.selected.min(calls.len().saturating_sub(1));
        let offset = nav
            .offset
            .min(selected)
            .max(selected.saturating_add(1).saturating_sub(capacity))
            .min(calls.len().saturating_sub(capacity));
        if activity_chrome > 0 {
            lines.push(card_top(
                &format!("Activity · {} calls", calls.len()),
                Some(view_label(view)),
                columns,
                colored,
            ));
        }
        if provider_view {
            let base = lines.len();
            lines.extend(render_provider_rows(
                &groups,
                &all,
                &calls,
                nav,
                offset,
                capacity,
                columns,
                colored,
                &report["provider_sessions"],
                &report["provider_models"],
                &mut provider_rows,
                &mut worker_rows,
                base,
            ));
            for row in provider_rows.iter_mut().flatten() {
                *row += base;
            }
        } else if calls.is_empty() {
            lines.push(card_row_lr(
                vec![Span::literal(
                    "  No recorded calls — press ? for shortcuts or dispatch a worker",
                    Some(SGR_MUTED),
                )],
                vec![],
                columns,
                use_cards,
                None,
                colored,
            ));
        } else {
            match view {
                View::Summary if nav.view == View::Summary => {
                    lines.extend(render_summary_rows(
                        &calls, capacity, columns, use_cards, colored,
                    ));
                }
                View::Graph | View::Tree => lines.extend(render_tree_rows(
                    &calls,
                    nav,
                    offset,
                    view == View::Graph,
                    capacity,
                    columns,
                    use_cards,
                    colored,
                )),
                _ => {
                    let mut used = 0usize;
                    for (index, call) in calls.iter().enumerate().skip(offset) {
                        if used >= capacity {
                            break;
                        }
                        let selected = index == nav.selected;
                        register_worker(&mut worker_rows, lines.len(), call);
                        let bg = (colored && selected).then_some(SGR_SEL_BG);
                        let marker = if selected { "▸" } else { " " };
                        let role = text(call, "role");
                        let (glyph, role_sgr) = role_style(role);
                        let alias = call_label(call);
                        let title = text(call, "title");
                        let (badge, badge_sgr) = state_badge(text(call, "state"));
                        if columns < 44 {
                            let task = [title, text(call, "purpose"), alias.as_str()]
                                .into_iter()
                                .find(|v| !v.is_empty())
                                .unwrap_or("call");
                            let state = text(call, "state");
                            let reason = text(call, "ending");
                            lines.push(card_row_lr(
                                vec![Span::raw(
                                    format!("{marker} {task}"),
                                    columns,
                                    Some(SGR_TEXT),
                                )],
                                vec![Span::raw(
                                    format!("{state} {reason}"),
                                    columns / 2,
                                    Some(badge_sgr),
                                )],
                                columns,
                                use_cards,
                                bg,
                                colored,
                            ));
                            used += 1;
                            if detail_rows {
                                lines.push(card_row_lr(
                                    vec![Span::raw(
                                        call_detail_meta(call),
                                        inner_width,
                                        Some(SGR_MUTED),
                                    )],
                                    vec![],
                                    columns,
                                    use_cards,
                                    bg,
                                    colored,
                                ));
                            }
                            continue;
                        }
                        let left = vec![
                            Span::literal(
                                marker,
                                Some(if selected { SGR_AMBER_BOLD } else { SGR_MUTED }),
                            ),
                            Span::literal(" ", None),
                            Span::literal(glyph, Some(role_sgr)),
                            Span::literal(" ", None),
                            Span::raw(alias, 36, Some(SGR_BOLD)),
                            Span::literal(" ", None),
                            Span::raw(role, 24, Some(role_sgr)),
                            Span::literal(
                                if title.is_empty() { "" } else { " · " },
                                Some(SGR_MUTED),
                            ),
                            Span::raw(title, inner_width, Some(SGR_TEXT)),
                        ];
                        let right = model_and_state(call, columns, badge, badge_sgr);
                        lines.push(card_row_lr(left, right, columns, use_cards, bg, colored));
                        used += 1;
                        if detail_rows {
                            lines.push(card_row_lr(
                                vec![
                                    Span::literal("    ↳ ", Some(SGR_MUTED)),
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
        let filtered_out = total.saturating_sub(history).saturating_sub(filtered_count);
        let folded = filtered_count.saturating_sub(calls.len());
        let offscreen = calls.len().saturating_sub(capacity.min(calls.len()));
        if filtered_out > 0 || folded > 0 || offscreen > 0 || history > 0 {
            lines.push(render_lr(
                vec![Span::raw(
                    [
                        (offscreen, "hidden"),
                        (filtered_out, "filt"),
                        (folded, "fold"),
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
    if nav.help || nav.health || nav.detail || height < 8 {
        lines.truncate(body_cap);
    }
    if fill_height {
        while lines.len() < body_cap {
            lines.push(String::new());
        }
    }
    for row in &mut provider_rows {
        if row.is_some_and(|row| row >= lines.len()) {
            *row = None;
        }
    }
    let hint = [
        navigation::BOARD_HINT,
        "↑↓ Move  Enter Open  Tab View  / Filter  b Attention  u Refresh  ? Keys  q Quit",
        "↑↓ Move  Enter Open  Tab View  / Filter  ? Keys  q Quit",
        "↑↓ Move  Enter Open  ? Keys  q Quit",
        "↑↓ Enter ? q",
    ]
    .into_iter()
    .find(|hint| width(hint) <= columns)
    .map_or_else(|| clip("↑↓ Enter ? q", columns), str::to_owned);
    lines.push(if colored {
        format!("{SGR_DIM}{hint}{SGR_RESET}")
    } else {
        hint
    });
    BoardFrame {
        lines,
        provider_rows,
        worker_rows,
    }
}

fn call_label(call: &Value) -> String {
    let raw = [
        text(call, "alias"),
        text(call, "participant"),
        text(call, "attempt"),
    ]
    .into_iter()
    .find(|v| !v.is_empty())
    .unwrap_or("call");
    if (raw.starts_with("att_") || raw.starts_with("call-")) && raw.chars().count() > 16 {
        raw.chars().take(12).collect()
    } else {
        raw.to_owned()
    }
}

fn call_detail_meta(call: &Value) -> String {
    let provider = text(call, "provider");
    let model = text(call, "model");
    let task = text(call, "task_id");
    [provider, model, task]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn header_line(
    report: &Value,
    calls: &[Value],
    attention: &[attention::Item],
    view: View,
    columns: usize,
    colored: bool,
) -> String {
    let room = &report["room"];
    let title = [text(room, "title"), text(room, "id"), "workspace"]
        .into_iter()
        .find(|v| !v.trim().is_empty())
        .unwrap_or("workspace");
    if columns < 44 {
        return render_lr(
            vec![Span::raw(
                if title.starts_with("OMS") {
                    title.to_owned()
                } else {
                    format!("OMS {title}")
                },
                columns,
                Some(SGR_AMBER_BOLD),
            )],
            if attention.is_empty() {
                vec![]
            } else {
                vec![Span::raw(
                    format!("!{}", attention.len()),
                    columns,
                    Some(SGR_AMBER_BOLD),
                )]
            },
            columns,
            false,
            None,
            colored,
        );
    }
    let running = calls
        .iter()
        .filter(|c| {
            matches!(
                text(c, "state"),
                "working" | "running" | "starting" | "queued" | "verifying"
            )
        })
        .count();
    let review = calls
        .iter()
        .filter(|c| text(c, "state") == "review")
        .count();
    let mut parts = Vec::new();
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if review > 0 {
        parts.push(format!("{review} review"));
    }
    if !attention.is_empty() {
        parts.push(format!("{} attention", attention.len()));
    }
    let summary = if parts.is_empty() {
        format!("{} calls", calls.len())
    } else {
        parts.join(" · ")
    };
    let left = vec![
        Span::literal("OMS", Some(SGR_AMBER_BOLD)),
        Span::literal(" · ", Some(SGR_MUTED)),
        Span::raw(title, columns / 2, Some(SGR_BOLD)),
    ];
    let right = vec![
        Span::raw(summary, columns / 2, Some(SGR_CYAN)),
        Span::literal("  ", None),
        Span::raw(format!("[{}]", view_label(view)), 16, Some(SGR_MUTED)),
    ];
    render_lr(left, right, columns, false, None, colored)
}

#[allow(clippy::too_many_arguments)]
fn render_provider_rows(
    groups: &[usize],
    all: &[Value],
    calls: &[Value],
    nav: &navigation::Navigation,
    offset: usize,
    capacity: usize,
    columns: usize,
    colored: bool,
    sessions: &Value,
    models: &Value,
    provider_rows: &mut [Option<usize>; 3],
    worker_rows: &mut BTreeMap<usize, (String, String)>,
    base: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let main = unique_live_main(all);
    let direct: Vec<_> = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| direct_main_work(call, main))
        .collect();
    if !direct.is_empty() {
        lines.push(card_top_styled(
            "Overall Main · Direct work",
            None,
            columns,
            colored,
            SGR_AMBER_BOLD,
        ));
        for (index, call) in direct
            .into_iter()
            .filter(|(index, _)| *index >= offset && *index < offset + capacity)
        {
            register_worker(worker_rows, base + lines.len(), call);
            lines.push(activity_row(
                call,
                all,
                index == nav.selected,
                columns,
                colored,
                true,
            ));
        }
        lines.extend(card_bottom(columns, true, colored));
    }
    let provider_start = lines.len();
    for &group in groups {
        let label = ["Codex", "Claude", "Antigravity", "Other providers"][group];
        let members: Vec<_> = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| provider_bucket(call) == group && !direct_main_work(call, main))
            .collect();
        let history = all
            .iter()
            .filter(|call| {
                provider_bucket(call) == group
                    && !direct_main_work(call, main)
                    && collapsed_history(nav, call)
            })
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
        let mut summary = if working > 0 {
            format!("{working} working")
        } else if members.is_empty() {
            "no visible work".into()
        } else {
            format!("{} recorded", members.len())
        };
        let awaiting = members
            .iter()
            .filter(|(_, call)| call["state"] == "review" && !call["returned_at"].is_string())
            .count();
        if awaiting > 0 {
            summary.push_str(&format!(" · {awaiting} review"));
        }
        if history > 0 {
            summary.push_str(&format!(" · {history} past"));
        }
        let model = crate::orchestrator::PROVIDERS
            .get(group)
            .and_then(|provider| models[*provider].as_str())
            .unwrap_or("model —");
        let full_title = if connected {
            format!("{label} · {session} · {model}")
        } else {
            format!("{label} · {session}")
        };
        let title = if connected && width(&full_title) + 6 > columns {
            let label = ["Codex", "Claude", "AGY"][group];
            format!("{label} · {session}")
        } else {
            full_title
        };
        let tag = (width(&title) + width(&summary) + 8 <= columns).then_some(summary.as_str());
        let header = card_top_styled(&title, tag, columns, colored, provider_style(group));
        lines.push(if lines.len() > provider_start {
            header.replacen('╭', "├", 1).replacen('╮', "┤", 1)
        } else {
            header
        });
        for (index, call) in members
            .into_iter()
            .filter(|(index, _)| *index >= offset && *index < offset + capacity)
        {
            register_worker(worker_rows, base + lines.len(), call);
            lines.push(activity_row(
                call,
                all,
                index == nav.selected,
                columns,
                colored,
                false,
            ));
        }
    }
    lines.extend(card_bottom(columns, true, colored));
    lines
}

fn activity_row(
    call: &Value,
    all: &[Value],
    selected: bool,
    columns: usize,
    colored: bool,
    show_provider: bool,
) -> String {
    let (glyph, role_sgr) = role_style(text(call, "role"));
    let (badge, badge_sgr) = if call["returned_at"].is_string() {
        ("[✓ returned]".into(), SGR_SAGE)
    } else {
        state_badge(text(call, "state"))
    };
    let mut parents = all.iter().filter(|parent| {
        let id = text(call, "parent");
        !id.is_empty() && (text(parent, "participant") == id || text(parent, "attempt") == id)
    });
    let parent = parents.next().filter(|_| parents.next().is_none());
    let parent_label = parent
        .map(|parent| format!(" ← {}", call_label(parent)))
        .unwrap_or_default();
    let mut left = vec![
        Span::literal(if selected { "▸ " } else { "  " }, Some(SGR_AMBER_BOLD)),
        Span::literal(glyph, Some(role_sgr)),
        Span::literal(" ", None),
        Span::raw(call_label(call), 24, Some(SGR_BOLD)),
        Span::literal(" ", None),
        Span::raw(text(call, "role"), 24, Some(role_sgr)),
    ];
    if show_provider {
        left.extend([
            Span::literal(" · ", Some(SGR_MUTED)),
            Span::raw(
                text(call, "provider"),
                24,
                Some(provider_style(provider_bucket(call))),
            ),
        ]);
    }
    left.extend([
        Span::literal(" · ", Some(SGR_MUTED)),
        Span::raw(text(call, "title"), columns, Some(SGR_TEXT)),
        Span::raw(parent_label, 32, Some(SGR_MUTED)),
    ]);
    card_row_lr(
        left,
        model_and_state(call, columns, badge, badge_sgr),
        columns,
        true,
        (colored && selected).then_some(SGR_SEL_BG),
        colored,
    )
}

fn register_worker(hits: &mut BTreeMap<usize, (String, String)>, row: usize, call: &Value) {
    let participant = text(call, "participant");
    if call["role"] != "main" && !participant.is_empty() {
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

fn provider_line(report: &Value, columns: usize, colored: bool) -> String {
    let snapshot = crate::usage::snapshot(report, oms_panel_core::timestamp() as f64);
    let compact = columns < 56;
    let tiny = columns < 28;
    let labels = if tiny {
        ["C", "L", "A"]
    } else if compact {
        ["Cdx", "Cld", "AGY"]
    } else {
        ["Codex", "Claude", "AGY"]
    };
    let values = ["codex", "claude", "antigravity"]
        .into_iter()
        .zip(labels)
        .map(|(name, label)| {
            let provider = &snapshot["providers"][name];
            let label = if name == "antigravity"
                && !compact
                && provider["reading"]["source"] == "AGY Gemini quota"
            {
                "AGY Gemini"
            } else {
                label
            };
            let left = if provider["reading"]["stale"] == true {
                if tiny { "!" } else { "stale" }.into()
            } else {
                rows(&provider["windows"])
                    .iter()
                    .find(|window| text(window, "name") == "weekly")
                    .and_then(|window| window["left"].as_u64())
                    .map(|left| format!("{left}%"))
                    .unwrap_or_else(|| "—".into())
            };
            format!("{label}{}{left}", if compact { "" } else { " " })
        })
        .collect::<Vec<_>>();
    render_lr(
        vec![Span::raw(
            format!(
                "{}{}",
                if tiny {
                    "W "
                } else if compact {
                    "Wk "
                } else {
                    "Weekly left · "
                },
                values.join(if compact { " " } else { " │ " })
            ),
            1_048_576,
            Some(SGR_MUTED),
        )],
        vec![],
        columns,
        false,
        None,
        colored,
    )
}

fn status_subheader(report: &Value, columns: usize, colored: bool) -> Option<String> {
    let mut segments: Vec<Span> = Vec::new();
    if let Some(tasks) = report["plan"]["tasks"].as_object()
        && !tasks.is_empty()
    {
        let total = tasks.len();
        let done = tasks.values().filter(|t| t["state"] == "done").count();
        segments.push(Span::raw(
            format!("Plan {done}/{total} done"),
            28,
            Some(if done == total { SGR_SAGE } else { SGR_MUTED }),
        ));
    }
    if segments.is_empty() {
        return None;
    }
    Some(render_lr(segments, vec![], columns, false, None, colored))
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

fn render_main_section(
    report: &Value,
    calls: &[Value],
    columns: usize,
    height: usize,
    colored: bool,
) -> Vec<String> {
    let main = calls.iter().find(|call| {
        text(call, "role") == "main"
            && oms_panel_core::state::LIVE_STATES.contains(&text(call, "state"))
    });
    let goal = text(&report["plan"], "goal");
    if main.is_none() && goal.is_empty() {
        return vec![];
    }
    let model = main.map(|call| text(call, "model")).unwrap_or("");
    let provider = main.map(|call| text(call, "provider")).unwrap_or("");
    let state = main.map(|call| text(call, "state")).unwrap_or("");
    let use_cards = columns >= 44 && height >= 12;
    let mut out = Vec::new();
    if use_cards {
        out.push(card_top_styled(
            "Overall Main",
            Some(state),
            columns,
            colored,
            SGR_MAIN_TITLE,
        ));
        let identity = if main.is_some() {
            [provider, model]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        } else {
            "No active Main".into()
        };
        out.push(card_row_lr(
            vec![Span::raw(identity, columns, Some(SGR_AMBER_BOLD))],
            vec![],
            columns,
            true,
            None,
            colored,
        ));
    } else {
        let identity = if main.is_none() {
            "Main not active".into()
        } else if model.is_empty() {
            format!("Main {provider}")
        } else {
            format!("Main {model}")
        };
        out.push(render_lr(
            vec![Span::raw(identity, columns, Some(SGR_CYAN))],
            vec![Span::raw(state, columns / 2, Some(SGR_CYAN))],
            columns,
            false,
            None,
            colored,
        ));
    }
    if !goal.is_empty() {
        out.push(card_row_lr(
            vec![Span::raw(
                format!("Goal: {goal}"),
                1_048_576,
                Some(SGR_TEXT),
            )],
            vec![],
            columns,
            use_cards,
            None,
            colored,
        ));
    }
    if let Some(bottom) = card_bottom(columns, use_cards, colored) {
        out.push(bottom);
    }
    out
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
        ("f", "Show selected call details"),
        ("Click title · 1/2/3", "Read Codex, Claude or AGY chat"),
        ("Click worker · c", "Read worker details or latest council"),
        ("U", "Refresh Claude and AGY weekly usage"),
        ("R · r", "All results / mark attention read"),
        ("Tab · g · t", "Cycle view / Graph view / Tree view"),
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

fn render_health_card(
    report: &Value,
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
            report["room"]["participants"]
                .as_array()
                .map(|v| v.len() as u64),
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

fn render_detail_card(
    selected: Option<&Value>,
    columns: usize,
    inner_width: usize,
    use_cards: bool,
    colored: bool,
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
    let (badge, badge_sgr) = state_badge(text(call, "state"));
    let mut out = vec![card_top(
        &format!("Call Detail · {}", call_label(call)),
        Some("Esc back"),
        columns,
        colored,
    )];
    let role = text(call, "role");
    let (glyph, role_sgr) = role_style(role);
    out.push(card_row_lr(
        vec![
            Span::literal(glyph, Some(role_sgr)),
            Span::literal(" ", None),
            Span::raw(
                format!("{} ({})", role, text(call, "purpose")),
                inner_width,
                Some(role_sgr),
            ),
        ],
        vec![Span::raw(badge, 20, Some(badge_sgr))],
        columns,
        use_cards,
        None,
        colored,
    ));
    for (label, value) in [
        ("Participant", text(call, "participant")),
        ("Attempt", text(call, "attempt")),
        ("Provider", text(call, "provider")),
        ("Model", text(call, "model")),
        ("Task", text(call, "task_id")),
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
    let title = text(call, "title");
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
    out
}

fn render_summary_rows(
    calls: &[Value],
    capacity: usize,
    columns: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let mut states = BTreeMap::new();
    for call in calls {
        *states.entry(text(call, "state")).or_insert(0usize) += 1;
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
    collect_tree(calls, nav, graph, maximum)
        .into_iter()
        .map(|entry| {
            format!(
                "{}{} {} [{}] {}",
                entry.prefix,
                call_label(entry.call),
                text(entry.call, "role"),
                text(entry.call, "state"),
                text(entry.call, "title")
            )
        })
        .collect()
}

struct TreeEntry<'a> {
    prefix: String,
    call: &'a Value,
}

fn collect_tree<'a>(
    calls: &'a [Value],
    nav: &navigation::Navigation,
    graph: bool,
    maximum: usize,
) -> Vec<TreeEntry<'a>> {
    let by_id: BTreeMap<_, _> = calls.iter().map(|v| (text(v, "participant"), v)).collect();
    let mut children: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for call in calls {
        children.entry(text(call, "parent")).or_default().push(call);
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
        .filter(|v| !by_id.contains_key(text(v, "parent")))
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
    for call in calls {
        let mut parent = text(call, "parent");
        let mut ancestors = BTreeSet::new();
        let mut hidden = false;
        while let Some(node) = by_id.get(parent) {
            if !ancestors.insert(parent) {
                break;
            }
            if nav.folded.contains(parent) {
                hidden = true;
                break;
            }
            parent = text(node, "parent");
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

#[allow(clippy::too_many_arguments)]
fn render_tree_rows(
    calls: &[Value],
    nav: &navigation::Navigation,
    offset: usize,
    graph: bool,
    capacity: usize,
    columns: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let entries = collect_tree(calls, nav, graph, usize::MAX);
    let inner = if use_cards {
        columns.saturating_sub(4)
    } else {
        columns
    };
    entries
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(capacity)
        .map(|(index, entry)| {
            let role = text(entry.call, "role");
            let (glyph, role_sgr) = role_style(role);
            let alias = call_label(entry.call);
            let title = text(entry.call, "title");
            let (badge, badge_sgr) = state_badge(text(entry.call, "state"));
            let left = vec![
                Span::literal(
                    if index == nav.selected { "▸" } else { " " },
                    Some(SGR_AMBER_BOLD),
                ),
                Span::raw(entry.prefix, 64, Some(SGR_BORDER)),
                Span::literal(glyph, Some(role_sgr)),
                Span::literal(" ", None),
                Span::raw(alias, 36, Some(SGR_BOLD)),
                Span::literal(" ", None),
                Span::raw(role, 24, Some(role_sgr)),
                Span::literal(if title.is_empty() { "" } else { " · " }, Some(SGR_MUTED)),
                Span::raw(title, inner, Some(SGR_TEXT)),
            ];
            let right = model_and_state(entry.call, columns, badge, badge_sgr);
            card_row_lr(
                left,
                right,
                columns,
                use_cards,
                (colored && index == nav.selected).then_some(SGR_SEL_BG),
                colored,
            )
        })
        .collect()
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
            lines.push(format!(
                "# {role} · tier {}",
                value(&report["roles"][role]["tier"])
            ));
            if let Some(error) = choice["error"].as_str() {
                lines.push(format!("Unavailable: {error}"));
                continue;
            }
            let snapshot = &choice["snapshot"];
            lines.push(format!(
                "Harness/provider: {} · family {} ({}) · execution profile {}",
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
                "Account: requested {} · selected {} · pinned {} · served {}",
                value(&snapshot["requested_account"]),
                value(&snapshot["selected_account"]),
                value(&snapshot["account_pinned"]),
                value(&snapshot["served_account"])
            ));
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
    fn finished_workers_wait_five_minutes_before_leaving_overview() {
        let now = oms_panel_core::timestamp();
        let nav = navigation::Navigation::default();
        let recent = json!({"state":"done","updated_at":now.saturating_sub(240)});
        let old = json!({"state":"done","updated_at":now.saturating_sub(360)});
        let review = json!({"state":"review","updated_at":now.saturating_sub(360)});
        let returned = json!({"state":"review","returned_at":"2024-01-01T00:00:00Z"});
        assert!(!collapsed_history(&nav, &recent));
        assert!(collapsed_history(&nav, &old));
        assert!(!collapsed_history(&nav, &review));
        assert!(collapsed_history(&nav, &returned));
        let search = navigation::Navigation {
            filter: "worker".into(),
            ..Default::default()
        };
        assert!(!collapsed_history(&search, &old));
    }

    #[test]
    fn board_footer_preserves_essential_keys() {
        let report = json!({});
        for columns in [80, 19, 28, 40, 55, 56, 120] {
            let frame = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                24,
                &BTreeMap::new(),
                false,
                false,
            );
            let footer = frame.last().unwrap();
            for key in ["?", "q", "Enter"] {
                assert!(footer.contains(key), "{columns}: missing {key}: {footer}");
            }
            assert!(
                footer.contains("↑↓") || footer.contains("Up/Down"),
                "{columns}: {footer}"
            );
            assert!(width(footer) <= columns);
            if columns == 120 {
                assert_eq!(footer, navigation::BOARD_HINT);
            }
            let colored = frame_styled(
                &report,
                &navigation::Navigation::default(),
                columns,
                24,
                &BTreeMap::new(),
                true,
                false,
            );
            assert_eq!(strip_sgr(colored.last().unwrap()), *footer);
        }
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
    fn viewport_detail_attention_and_geometry() {
        let report = json!({"collection":{"ok":true}, "room":{"title":"room", "participants":[
            {"participant":"main", "role":"main", "state":"working", "model":"long-model", "title":"Live"},
            {"participant":"old", "role":"main", "state":"done", "title":"History"},
            {"participant":"review", "state":"review", "title":"Review"},
            {"participant":"blocked", "state":"blocked", "title":"Blocked", "provider":"codex", "model":"m1", "task_id":"task-1"}
        ]}, "plan":{"goal":"Ship", "tasks":{"one":{"state":"done"}}}, "provider_status":{"codex":{"weekly_used":25}}});
        let mut observed = report.clone();
        observed["room"]["participants"][0]["attempt"] = json!("attempt-main");
        observed["room"]["participants"][0]["provider"] = json!("codex");
        observed["room_attempts"]["attempt-main"] = json!({
            "attempt_id":"attempt-main", "provider":"codex", "role":"main", "state":"working"
        });
        observed["main_model_observation"] =
            json!({"attempt":"attempt-main","model":"observed-model"});
        assert_eq!(calls(&observed)[0]["model"], "observed-model");
        observed["main_model_observation"]["attempt"] = json!("unrelated");
        assert_eq!(calls(&observed)[0]["model"], "long-model");
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
                    .any(|v| v.contains("▸") && v.contains("Blocked")),
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
        let usage = provider_line(&stale_usage, 80, false);
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
                .any(|line| line.contains("▸") && line.contains("Blocked")),
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
            {"participant":"M", "role":"main", "provider":"codex", "state":"working", "title":"Main task"},
            {"participant":"C", "parent":"M", "state":"blocked", "title":"長い注意長い注意長い注意長い注意長い注意長い注意"},
            {"participant":"X", "state":"done", "title":"Other"}
        ]}, "plan":{"goal":"長い目標長い目標長い目標長い目標長い目標長い目標", "tasks":{"a":{"state":"queued"},"b":{"state":"queued"}}},
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
                    && frame.contains("Main codex")
                    && frame.contains("!1"),
                "{frame}"
            );
            assert!(frame.contains("Plan 0/2"), "{columns}: {frame}");
            assert!(
                frame.contains("82%") && !frame.contains("89%"),
                "{columns}: {frame}"
            );
            assert!(
                frame
                    .lines()
                    .any(|s| s.starts_with("Goal:") && s.contains('…')),
                "{frame}"
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
                frame.contains("Plan 0/2")
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
            frame.contains("2 filt") && !frame.contains("1 fold"),
            "{frame}"
        );
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
                plain.iter().any(|row| row.contains("Live ta")),
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
        assert!(selected_frame.contains("▸ Task19"), "{selected_frame}");
        nav.selected = 20;
        nav.offset = 20;
        let resized = frame(&report, &nav, 28, 40, &read).join("\n");
        assert!(resized.contains("▸ Task9"), "{resized}");
        assert!(resized.contains("Live ta"), "{resized}");
        assert!(!resized.contains("hidden"), "{resized}");
        nav.filter = "Task19".into();
        nav.selected = 0;
        let frame = frame(&report, &nav, 28, 14, &read).join("\n");
        assert!(frame.contains("20 filt"), "{frame}");
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
            {"participant":"M", "attempt_id":"att-M", "provider":"codex", "role":"main", "state":"working", "title":"Own goal"},
            {"participant":"CW", "provider":"codex", "role":"worker", "parent":"M", "state":"working", "title":"Codex task"},
            {"participant":"L", "provider":"claude", "role":"lead", "parent":"M", "state":"working", "title":"Coordinate review"},
            {"participant":"LW", "provider":"claude", "role":"worker", "parent":"L", "state":"review", "title":"Claude task"},
            {"participant":"AW", "provider":"antigravity", "role":"worker", "parent":"M", "state":"working", "title":"AGY task"},
            {"participant":"DW", "provider":"claude", "role":"worker", "parent":"att-M", "state":"working", "title":"Direct Claude work"}
        ]}});
        let mut nav = navigation::Navigation::default();
        let visible_calls = visible(&report, &nav);
        assert_eq!(
            visible_calls
                .iter()
                .map(|call| text(call, "participant"))
                .collect::<Vec<_>>(),
            ["CW", "DW", "AW", "L", "LW"]
        );
        let read = BTreeMap::new();
        for (columns, height) in [(200, 50), (120, 30), (80, 24), (56, 20), (40, 12), (19, 14)] {
            for (index, call) in visible_calls.iter().enumerate() {
                nav.selected = index;
                let board = board_frame(&report, &nav, columns, height, &read, false, true);
                let plain = &board.lines;
                let color = frame_styled(&report, &nav, columns, height, &read, true, true);
                assert_eq!(plain.len(), height, "{columns}x{height}: {plain:?}");
                assert!(plain.iter().all(|line| width(line) <= columns));
                assert!(
                    plain
                        .iter()
                        .zip(color)
                        .all(|(plain, color)| *plain == strip_sgr(&color))
                );
                let selected_label = if columns < 44 {
                    text(call, "title").chars().take(4).collect::<String>()
                } else {
                    text(call, "participant").to_owned()
                };
                assert!(
                    plain
                        .iter()
                        .any(|line| line.contains('▸') && line.contains(&selected_label)),
                    "{columns}x{height} {index}: {plain:?}"
                );
                let selected_row = plain
                    .iter()
                    .position(|line| line.contains('▸') && line.contains(&selected_label))
                    .unwrap();
                assert_eq!(
                    board.worker_rows.get(&selected_row),
                    Some(&(
                        text(call, "participant").to_owned(),
                        text(call, "attempt").to_owned()
                    ))
                );
                if columns >= 56 {
                    let joined = plain.join("\n");
                    for required in [
                        "Overall Main · Direct work",
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
                let board = board_frame(&paged, &page_nav, columns, height, &read, false, true);
                let frame = &board.lines;
                let identity = format!(" {} ", text(call, "participant"));
                assert!(
                    frame
                        .iter()
                        .any(|line| line.contains('▸') && line.contains(&identity)),
                    "{columns}x{height} {index}: {frame:?}"
                );
                let selected_row = frame
                    .iter()
                    .position(|line| line.contains('▸') && line.contains(&identity))
                    .unwrap();
                assert_eq!(
                    board.worker_rows.get(&selected_row),
                    Some(&(
                        text(call, "participant").to_owned(),
                        text(call, "attempt").to_owned()
                    ))
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
        assert_eq!(visible(&historical, &legacy).len(), 6);
        nav.filter = "AGY task".into();
        assert_eq!(visible(&historical, &nav).len(), 1);
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
        assert!(
            empty.contains("no visible work") && !empty.contains("idle"),
            "{empty}"
        );
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
        assert!(!board.worker_rows.contains_key(&codex));
        assert!(!board.worker_rows.contains_key(&agy));
        for columns in [56, 44, 40] {
            let board = board_frame(
                &connected,
                &navigation::Navigation::default(),
                columns,
                24,
                &read,
                false,
                true,
            );
            assert_eq!(board.lines.len(), 24);
            assert!(board.lines.iter().all(|line| width(line) <= columns));
            if columns >= 44 {
                let [Some(codex), None, Some(agy)] = board.provider_rows else {
                    panic!("connected session targets must survive compact card titles");
                };
                assert!(board.lines[codex].contains("Lead chat [1]"));
                assert!(board.lines[agy].contains("Lead chat [3]"));
            } else {
                assert_eq!(board.provider_rows, [None; 3]);
            }
        }

        let mut classified = report.clone();
        classified["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"participant":"OLD", "role":"main", "state":"done"}),
                json!({"participant":"PAST", "provider":"claude", "role":"worker", "parent":"OLD", "state":"working"}),
                json!({"participant":"EMPTY", "provider":"claude", "role":"worker", "parent":"", "state":"working"}),
                json!({"participant":"LOST", "provider":"claude", "role":"worker", "parent":"missing", "state":"working"}),
                json!({"participant":"PREFIX", "provider":"claude", "role":"worker", "parent":"att-M-more", "state":"working"}),
            ]);
        classified["native_children"] = json!([
            {"participant":"NATIVE", "provider":"claude", "role":"subagent", "parent":"M", "native_child":true, "state":"working"}
        ]);
        let board = board_frame(
            &classified,
            &navigation::Navigation::default(),
            200,
            50,
            &read,
            false,
            true,
        );
        let direct_top = board
            .lines
            .iter()
            .position(|line| line.contains("Overall Main · Direct work"))
            .unwrap();
        let direct_bottom = board
            .lines
            .iter()
            .enumerate()
            .skip(direct_top + 1)
            .find(|(_, line)| line.starts_with('╰'))
            .unwrap()
            .0;
        assert!(board.lines[direct_top - 1].starts_with('╰'));
        assert!(board.lines[direct_bottom + 1].starts_with("╭─ Codex · Activity"));
        assert_eq!(board.worker_rows.len(), 10);
        for (row, (participant, _)) in &board.worker_rows {
            assert_eq!(
                *row > direct_top && *row < direct_bottom,
                ["CW", "DW", "AW"].contains(&participant.as_str()),
                "{participant}: {:?}",
                board.lines
            );
            assert!(!board.lines[*row].contains("├─") && !board.lines[*row].contains("└─"));
            if participant == "DW" {
                assert!(board.lines[*row].contains("claude") && board.lines[*row].contains("← M"));
            }
            if participant == "LW" {
                assert!(board.lines[*row].contains("← L"));
            }
        }
        for variant in 0..4 {
            let mut uncertain = classified.clone();
            let members = uncertain["room"]["participants"].as_array_mut().unwrap();
            match variant {
                0 => members.retain(|call| call["participant"] != "M"),
                1 => members[0]["state"] = json!("done"),
                2 => {
                    members.push(json!({"participant":"SECOND", "role":"main", "state":"working"}));
                }
                _ => {
                    members
                        .push(json!({"participant":"att-M", "role":"worker", "state":"working"}));
                }
            }
            let board = board_frame(
                &uncertain,
                &navigation::Navigation::default(),
                200,
                50,
                &read,
                false,
                true,
            );
            assert!(
                !board
                    .lines
                    .iter()
                    .any(|line| line.contains("Overall Main · Direct work")),
                "variant {variant}: {:?}",
                board.lines
            );
            if variant == 3 {
                let (row, _) = board
                    .worker_rows
                    .iter()
                    .find(|(_, (participant, _))| participant == "DW")
                    .unwrap();
                assert!(!board.lines[*row].contains('←'));
            }
        }
        nav.filter = "Claude task".into();
        nav.selected = 0;
        assert_eq!(visible(&report, &nav).len(), 1);
        let filtered = frame(&report, &nav, 80, 24, &read).join("\n");
        assert!(
            filtered
                .lines()
                .any(|line| line.contains('▸') && line.contains("LW")),
            "{filtered}"
        );
        assert!(filtered.contains("4 filt"), "{filtered}");
        assert!(filtered.contains("Claude · Activity"), "{filtered}");
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
                        "created_at": "2026-11-17T00:00:00Z",
                        "refs": {
                            "panel_role": "main",
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
                "tasks": {
                    "t1": {"state": "done"},
                    "t2": {"state": "running"}
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
        assert!(plain[0].starts_with("OMS · room-demo"), "{}", plain[0]);
        assert!(!plain[0].contains("·  ·"), "{}", plain[0]);
        assert!(
            plain
                .iter()
                .any(|l| l.contains("Weekly left · Codex — │ Claude 72% │ AGY —")),
            "{plain:?}"
        );
        assert!(plain.iter().any(|l| l.contains("Plan 1/2 done")));
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
        assert!(tree_joined.contains("├─ ● W1 builder"), "{tree_joined}");
        assert!(tree_joined.contains("└─ ◆ R1 reviewer"), "{tree_joined}");

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
        assert!(joined.contains("att_8aa76df1 main"), "{joined}");
        assert!(joined.contains("Main Tower"), "{joined}");
        assert!(joined.contains("W1 worker · Worker label"), "{joined}");
        assert!(joined.contains("[✓ exited]"), "{joined}");
        assert!(!joined.contains(": blocked:"), "{joined}");
        assert!(!joined.contains("blocked:\n"), "{joined}");
        assert!(joined.contains("blocked"), "{joined}");
    }
}
