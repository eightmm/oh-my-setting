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
    let inner = columns.saturating_sub(4);
    if columns < 44 {
        let mut left = vec![Span::raw(title, inner, Some(SGR_BOLD))];
        if let Some(t) = tag.filter(|v| !v.is_empty()) {
            left.push(Span::literal(" · ", Some(SGR_MUTED)));
            left.push(Span::raw(t, inner, Some(SGR_MUTED)));
        }
        return render_lr(left, vec![], columns, false, None, colored);
    }
    let clean_title = clip(title, columns.saturating_sub(8));
    let clean_tag = tag
        .filter(|v| !v.is_empty())
        .map(|v| clip(v, (columns / 3).max(6)))
        .filter(|v| !v.is_empty());
    let title_w = width(&clean_title);
    let right_len = clean_tag.as_ref().map_or(0, |t| width(t) + 3);
    let prefix_len = 3usize; // "╭─ "
    let title_space = 1usize; // " "
    let end_cap = 1usize; // "╮"
    let used = prefix_len + title_w + title_space + right_len + end_cap;
    let fill = columns.saturating_sub(used);
    let mut spans = vec![
        Span::literal("╭─ ", Some(SGR_BORDER)),
        Span {
            plain: clean_title,
            sgr: Some(SGR_BOLD),
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
        "done" | "accepted" | "completed" => ("[✓ done]".into(), SGR_SAGE),
        "failed" | "guard_stop" | "orphaned" => ("[× failed]".into(), SGR_CORAL_BOLD),
        "timed_out" => ("[◷ timeout]".into(), SGR_CORAL_BOLD),
        "waiting_input" | "waiting_approval" | "blocked" => ("[! wait]".into(), SGR_AMBER_BOLD),
        "" => ("[· -]".into(), SGR_MUTED),
        other => (format!("[· {}]", clean(other, 14)), SGR_MUTED),
    }
}

fn view_label(view: View) -> &'static str {
    match view {
        View::Auto | View::Compact => "Compact",
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
    output.sort_by_key(|v| {
        (
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
    visible_from(calls(report), nav)
}

fn visible_from(all: Vec<Value>, nav: &navigation::Navigation) -> Vec<Value> {
    let words = navigation::words(&nav.filter);
    all.into_iter()
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
        .collect()
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
    let columns = columns.max(1);
    let height = height.max(3);
    let use_cards = columns >= 44;
    let inner_width = if use_cards {
        columns.saturating_sub(4)
    } else {
        columns
    };
    let all_calls = calls(report);
    let total = all_calls.len();
    let calls = visible_from(all_calls, nav);
    let attention = attention::items(report, read);
    let view = nav.view.resolved(columns, height);

    let mut lines = Vec::new();
    lines.push(header_line(
        report, &calls, &attention, view, columns, colored,
    ));
    if columns >= 56
        && height >= 14
        && let Some(meta) = status_subheader(report, columns, colored)
    {
        lines.push(meta);
    }
    if columns >= 68 && height >= 18 && !nav.help && !nav.health && !nav.detail {
        lines.push(legend_line(colored, columns));
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
    if let Some(notice) = &nav.notice {
        for part in wrap(notice, columns.saturating_sub(2).max(1))
            .into_iter()
            .take(2)
        {
            lines.push(render_lr(
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
        if !attention.is_empty() {
            lines.extend(render_attention_section(
                &attention,
                columns,
                inner_width,
                height,
                use_cards,
                colored,
            ));
        }
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

        let card_chrome = usize::from(use_cards) * 2 + 1;
        let capacity = height.saturating_sub(lines.len() + card_chrome).max(1);
        lines.push(card_top(
            &format!("Activity · {} calls", calls.len()),
            Some(view_label(view)),
            columns,
            colored,
        ));
        if calls.is_empty() {
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
                View::Graph | View::Tree => lines.extend(render_tree_rows(
                    &calls,
                    nav,
                    view == View::Graph,
                    capacity,
                    columns,
                    use_cards,
                    colored,
                )),
                View::Summary => {
                    lines.extend(render_summary_rows(
                        &calls, capacity, columns, use_cards, colored,
                    ));
                }
                _ => {
                    let mut used = 0usize;
                    for (index, call) in calls.iter().enumerate().skip(nav.offset) {
                        if used >= capacity {
                            break;
                        }
                        let selected = index == nav.selected;
                        let bg = (colored && selected).then_some(SGR_SEL_BG);
                        let marker = if selected { "▸" } else { " " };
                        let role = text(call, "role");
                        let (glyph, role_sgr) = role_style(role);
                        let alias = call_label(call);
                        let title = text(call, "title");
                        let (badge, badge_sgr) = state_badge(text(call, "state"));
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
                        let right = vec![Span::raw(badge, 20, Some(badge_sgr))];
                        lines.push(card_row_lr(left, right, columns, use_cards, bg, colored));
                        used += 1;

                        if view == View::Detail && used < capacity {
                            let meta = call_detail_meta(call);
                            if !meta.is_empty() {
                                lines.push(card_row_lr(
                                    vec![
                                        Span::literal("    ↳ ", Some(SGR_MUTED)),
                                        Span::raw(meta, inner_width, Some(SGR_MUTED)),
                                    ],
                                    vec![],
                                    columns,
                                    use_cards,
                                    bg,
                                    colored,
                                ));
                                used += 1;
                            }
                        }
                    }
                }
            }
        }
        if let Some(bottom) = card_bottom(columns, use_cards, colored) {
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
    let hint = clip(navigation::BOARD_HINT, columns);
    lines.push(if colored {
        format!("{SGR_DIM}{hint}{SGR_RESET}")
    } else {
        hint
    });
    lines
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
    let provider = clean(text(call, "provider"), 24);
    let model = clean(text(call, "model"), 40);
    let task = clean(text(call, "task_id"), 48);
    let pm = match (provider.is_empty(), model.is_empty()) {
        (false, false) => format!("{provider}/{model}"),
        (false, true) => provider,
        (true, false) => model,
        (true, true) => String::new(),
    };
    match (pm.is_empty(), task.is_empty()) {
        (false, false) => format!("{pm} · {task}"),
        (false, true) => pm,
        (true, false) => format!("task {task}"),
        (true, true) => String::new(),
    }
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
        parts.push(format!("{} waiting", attention.len()));
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

fn status_subheader(report: &Value, columns: usize, colored: bool) -> Option<String> {
    let mut segments: Vec<Span> = Vec::new();
    let push_sep = |segs: &mut Vec<Span>| {
        if !segs.is_empty() {
            segs.push(Span::literal(" │ ", Some(SGR_BORDER)));
        }
    };
    for (name, label) in [("claude", "Claude"), ("codex", "Codex")] {
        let entry = &report["provider_status"][name];
        if !entry.is_object() {
            continue;
        }
        let mut metrics = Vec::new();
        if let Some(used) = entry["weekly_used"].as_u64().filter(|v| *v <= 100) {
            metrics.push(format!("wk {}% left", 100 - used));
        }
        if let Some(used) = entry["five_hour_used"].as_u64().filter(|v| *v <= 100) {
            metrics.push(format!("5h {}% left", 100 - used));
        }
        if !metrics.is_empty() {
            push_sep(&mut segments);
            segments.push(Span::raw(
                format!("{label} {}", metrics.join(" · ")),
                40,
                Some(SGR_MUTED),
            ));
        }
    }
    if let Some(tasks) = report["plan"]["tasks"].as_object()
        && !tasks.is_empty()
    {
        let total = tasks.len();
        let done = tasks.values().filter(|t| t["state"] == "done").count();
        push_sep(&mut segments);
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

fn render_attention_section(
    attention: &[attention::Item],
    columns: usize,
    inner_width: usize,
    height: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let max_items = attention::LIMIT
        .min(height.saturating_sub(8) / 3 + 1)
        .max(1);
    out.push(card_top(
        &format!("! Attention · {}", attention.len()),
        Some("b filter · r read"),
        columns,
        colored,
    ));
    for item in attention.iter().take(max_items) {
        let marker = if item.unread { "●" } else { "·" };
        let marker_sgr = if item.unread {
            SGR_AMBER_BOLD
        } else {
            SGR_MUTED
        };
        let cls_sgr = if matches!(
            item.cls.as_str(),
            "failed"
                | "guard_stop"
                | "timed_out"
                | "verify_failed"
                | "verify_timed_out"
                | "provider_error"
        ) {
            SGR_CORAL_BOLD
        } else if matches!(item.cls.as_str(), "needs" | "decide" | "stale") {
            SGR_GOLD_BOLD
        } else {
            SGR_CYAN
        };
        let mut left = vec![
            Span::literal(marker, Some(marker_sgr)),
            Span::literal(" ", None),
            Span::raw(format!("[{}]", item.cls), 20, Some(cls_sgr)),
            Span::literal(" ", None),
        ];
        let who = item.who.trim();
        if !who.is_empty() {
            left.push(Span::raw(who, 28, Some(SGR_BOLD)));
            left.push(Span::literal(": ", Some(SGR_MUTED)));
        }
        left.push(Span::raw(&item.what, inner_width, Some(SGR_TEXT)));
        out.push(card_row_lr(left, vec![], columns, use_cards, None, colored));
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
        ("Enter · f", "Open call results & diff reader"),
        ("Tab · g · t", "Cycle view / Graph view / Tree view"),
        ("/ · Esc", "Filter calls / clear or return to board"),
        ("b · r", "Toggle attention filter / mark visible read"),
        ("k · ? · u · q", "Health & capacity / Help / Refresh / Quit"),
        ("1 · 2 · 0 · s", "Focus Claude / Pi / Codex / Swap main"),
        ("o · h · j · R", "Rooms / Joined chats / Controls / Results"),
        (
            "5 · 6 · e · a",
            "Dispatch Explain / Implement / Researcher / Advisor",
        ),
        ("7 · c · n", "Review gate / Council debate / Retry delivery"),
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
        let mut stack = Vec::new();
        visit(
            call, &mut stack, true, &children, &mut seen, &mut out, nav, graph, maximum,
        );
    }
    out
}

fn render_tree_rows(
    calls: &[Value],
    nav: &navigation::Navigation,
    graph: bool,
    capacity: usize,
    columns: usize,
    use_cards: bool,
    colored: bool,
) -> Vec<String> {
    let entries = collect_tree(calls, nav, graph, capacity);
    let inner = if use_cards {
        columns.saturating_sub(4)
    } else {
        columns
    };
    entries
        .into_iter()
        .map(|entry| {
            let role = text(entry.call, "role");
            let (glyph, role_sgr) = role_style(role);
            let alias = call_label(entry.call);
            let title = text(entry.call, "title");
            let (badge, badge_sgr) = state_badge(text(entry.call, "state"));
            let left = vec![
                Span::raw(entry.prefix, 64, Some(SGR_BORDER)),
                Span::literal(glyph, Some(role_sgr)),
                Span::literal(" ", None),
                Span::raw(alias, 36, Some(SGR_BOLD)),
                Span::literal(" ", None),
                Span::raw(role, 24, Some(role_sgr)),
                Span::literal(if title.is_empty() { "" } else { " · " }, Some(SGR_MUTED)),
                Span::raw(title, inner, Some(SGR_TEXT)),
            ];
            let right = vec![Span::raw(badge, 20, Some(badge_sgr))];
            card_row_lr(left, right, columns, use_cards, None, colored)
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
                "claude": {"weekly_used": 28, "five_hour_used": 23}
            },
            "plan": {
                "tasks": {
                    "t1": {"state": "done"},
                    "t2": {"state": "running"}
                }
            }
        });
        let nav = navigation::Navigation::default();
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
                .any(|l| l.contains("Claude wk 72% left · 5h 77% left")
                    && l.contains("Plan 1/2 done")),
            "{plain:?}"
        );
        assert!(plain.iter().any(|l| l.contains("[⚑ review]")));
        assert!(plain.iter().any(|l| l.contains("[✓ done]")));
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

        let nav = navigation::Navigation::default();
        let read = BTreeMap::new();
        let lines = frame_styled(&report, &nav, 90, 24, &read, false, false);
        let joined = lines.join("\n");
        assert!(joined.contains("att_8aa76df1 main"), "{joined}");
        assert!(joined.contains("Main Tower"), "{joined}");
        assert!(joined.contains("W1 worker · Worker label"), "{joined}");
        assert!(joined.contains("[✓ done]"), "{joined}");
        assert!(!joined.contains(": blocked:"), "{joined}");
        assert!(!joined.contains("blocked:\n"), "{joined}");
        assert!(joined.contains("blocked"), "{joined}");
    }
}
