//! Terminal text is untrusted. Only renderer-owned escape sequences reach the TTY.
use crate::layout::View;
use crate::{attention, navigation, rows, text};
use oms_panel_core::Value;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use unicode_general_category::{GeneralCategory, get_general_category};

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
pub fn calls(report: &Value) -> Vec<Value> {
    let mut attempts = BTreeMap::new();
    for key in ["recent", "active_recent", "active"] {
        for row in rows(&report["attempts"][key]) {
            attempts.insert(text(row, "attempt_id").to_owned(), row);
        }
    }
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for member in rows(&report["room"]["participants"]) {
        let attempt = text(member, "attempt");
        let source = attempts.get(attempt).copied().unwrap_or(member);
        let mut call = normalize(source);
        for key in [
            "participant",
            "parent",
            "provider",
            "model",
            "role",
            "state",
            "title",
            "task_id",
            "alias",
            "purpose",
        ] {
            if let Some(v) = member.get(key).filter(|v| !v.is_null() && *v != "") {
                call[key] = v.clone();
            }
        }
        output.push(call);
        seen.insert(attempt.to_owned());
    }
    for (id, attempt) in attempts {
        if !id.is_empty() && !seen.contains(&id) {
            output.push(normalize(attempt));
        }
    }
    output.sort_by_key(|v| {
        (
            text(v, "created_at").to_owned(),
            text(v, "participant").to_owned(),
        )
    });
    output
}
fn normalize(row: &Value) -> Value {
    let refs = &row["refs"];
    let choose = |key, reference| {
        row[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .or_else(|| refs[reference].as_str())
            .unwrap_or("")
            .to_owned()
    };
    let attempt = text(row, "attempt_id");
    let participant = refs["panel_room_participant"].as_str().unwrap_or(attempt);
    json!({"participant":participant,"attempt":attempt,"task_id":text(row,"task_id"),
        "role":choose("role","panel_role"),"provider":choose("provider","panel_provider"),
        "model":choose("model","panel_model"),"purpose":choose("purpose","panel_purpose"),
        "alias":choose("alias","panel_alias"),"title":choose("title","panel_label"),
        "parent":refs["panel_room_parent"].as_str().unwrap_or(text(row,"parent_attempt_id")),
        "state":row["state"],"ending":row["reason_code"],"updated_at":row["updated_at"],"created_at":row["created_at"],"refs":refs})
}
pub fn visible(report: &Value, nav: &navigation::Navigation) -> Vec<Value> {
    calls(report)
        .into_iter()
        .filter(|v| navigation::matches(v, &nav.filter))
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
    let columns = columns.max(1);
    let calls = visible(report, nav);
    let attention = attention::items(report, read);
    let room = &report["room"];
    let mut lines = vec![format!(
        "OMS · {} · {} calls · {} waiting",
        text(room, "title"),
        calls.len(),
        attention.len()
    )];
    if report["collection"]["ok"] != true || report["capacity_stale"] == true {
        lines.push("STALE: refresh failed; retained evidence cannot authorize actions".into());
    }
    if let Some(notice) = &nav.notice {
        lines.extend(wrap(notice, columns));
    }
    if nav.help {
        lines.extend(wrap(navigation::BOARD_HINT, columns));
        lines.extend(wrap(navigation::CONTROL_HINT, columns));
    } else if nav.health {
        let health = oms_panel_core::health::project(report, oms_panel_core::timestamp() as f64);
        lines.extend(value_lines(&health, columns));
    } else if nav.detail {
        if let Some(call) = calls.get(nav.selected) {
            lines.extend(value_lines(call, columns));
        }
    } else {
        if !attention.is_empty() {
            lines.push("Attention".into());
            for item in attention
                .iter()
                .take(attention::LIMIT.min(height.saturating_sub(7) / 3 + 1))
            {
                lines.push(format!(
                    " {} [{}] {}: {}",
                    if item.unread { '*' } else { ' ' },
                    item.cls,
                    item.who,
                    item.what
                ));
            }
        }
        if !nav.filter.is_empty() || nav.editing {
            lines.push(format!(
                "Filter /{}{} · shown {} of {} · Esc clears",
                nav.filter,
                if nav.editing { "_" } else { "" },
                calls.len(),
                self::calls(report).len()
            ));
        }
        let view = nav.view.resolved(columns, height);
        let capacity = height.saturating_sub(lines.len() + 2).max(1);
        match view {
            View::Graph | View::Tree => {
                lines.extend(tree(&calls, nav, view == View::Graph, capacity))
            }
            View::Summary => {
                let mut states = BTreeMap::new();
                for call in &calls {
                    *states.entry(text(call, "state")).or_insert(0usize) += 1;
                }
                for (state, count) in states {
                    lines.push(format!("{state}: {count}"));
                }
            }
            _ => {
                for (index, call) in calls.iter().enumerate().skip(nav.offset).take(capacity) {
                    let marker = if index == nav.selected { '>' } else { ' ' };
                    lines.push(format!(
                        "{marker} {} {} · {} · {}",
                        text(call, "alias"),
                        text(call, "role"),
                        text(call, "state"),
                        text(call, "title")
                    ));
                    if view == View::Detail {
                        lines.push(format!(
                            "    {} {} · {}",
                            text(call, "provider"),
                            text(call, "model"),
                            text(call, "task_id")
                        ));
                    }
                }
            }
        }
        if calls.is_empty() {
            lines.push("No recorded calls".into());
        }
    }
    lines.truncate(height.saturating_sub(1));
    lines.push(navigation::BOARD_HINT.into());
    lines.into_iter().map(|line| clip(&line, columns)).collect()
}
pub fn tree(
    calls: &[Value],
    nav: &navigation::Navigation,
    graph: bool,
    maximum: usize,
) -> Vec<String> {
    let by_id: BTreeMap<_, _> = calls.iter().map(|v| (text(v, "participant"), v)).collect();
    let mut children: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for call in calls {
        children.entry(text(call, "parent")).or_default().push(call);
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    fn visit<'a>(
        call: &'a Value,
        level: usize,
        children: &BTreeMap<&str, Vec<&'a Value>>,
        seen: &mut BTreeSet<&'a str>,
        out: &mut Vec<String>,
        nav: &navigation::Navigation,
        graph: bool,
        maximum: usize,
    ) {
        let id = text(call, "participant");
        if !seen.insert(id) || out.len() >= maximum {
            return;
        }
        out.push(format!(
            "{}{}{} {} [{}] {}",
            "  ".repeat(level.min(32)),
            if level == 0 {
                ""
            } else if graph {
                "└→ "
            } else {
                "└─ "
            },
            text(call, "alias"),
            text(call, "role"),
            text(call, "state"),
            text(call, "title")
        ));
        if nav.folded.contains(id) {
            return;
        }
        if let Some(rows) = children.get(id) {
            for child in rows {
                visit(child, level + 1, children, seen, out, nav, graph, maximum);
            }
        }
    }
    for call in calls
        .iter()
        .filter(|v| !by_id.contains_key(text(v, "parent")))
    {
        visit(call, 0, &children, &mut seen, &mut out, nav, graph, maximum);
    }
    for call in calls {
        visit(call, 0, &children, &mut seen, &mut out, nav, graph, maximum);
    }
    out
}
pub fn value_lines(value: &Value, columns: usize) -> Vec<String> {
    serde_json::to_string_pretty(value)
        .map(|v| wrap(&v, columns))
        .unwrap_or_default()
}
pub fn results(report: &Value, columns: usize) -> String {
    let mut lines = vec!["OMS · Results".into()];
    for row in rows(&report["rows"]) {
        lines.push(format!(
            "{} · {} · {}",
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
                "  {} · {} · {}",
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
    let mut lines = vec!["Council · recorded positions".into()];
    for task in rows(&report["rows"]) {
        for call in rows(&task["calls"]) {
            lines.push(format!(
                "{} · {}",
                text(call, "kind"),
                text(call, "selected_model")
            ));
            lines.extend(wrap(text(call, "answer_excerpt"), columns));
        }
    }
    if let Some(synthesis) = report["synthesis"]["text"].as_str() {
        lines.push("Synthesis".into());
        lines.extend(wrap(synthesis, columns));
    }
    lines.push("Agreement records identical positions; it does not prove correctness.".into());
    lines.join("\n") + "\n"
}
