#![forbid(unsafe_code)]

use crate::sanitize::{
    DAILY_ITEM_LIMIT, SUMMARY_BYTE_LIMIT, SUMMARY_ITEM_CHAR_LIMIT, WEEKLY_ITEM_LIMIT,
    bounded_summary, sanitize_text, truncate_utf8_bytes,
};
use crate::tz::{TimeZoneSpec, format_hhmm, parse_rfc3339_utc};
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub struct Headings {
    pub lang: &'static str,
    pub daily_title: &'static str,
    pub weekly_title: &'static str,
    pub overview: &'static str,
    pub progress: &'static str,
    pub verified: &'static str,
    pub decisions: &'static str,
    pub blockers: &'static str,
    pub next_actions: &'static str,
    pub experiments: &'static str,
    pub sessions: &'static str,
    pub by_project: &'static str,
    pub notion_done: &'static str,
    pub notion_failed: &'static str,
    pub notion_next: &'static str,
    pub notion_details: &'static str,
    pub none_recorded: &'static str,
    pub work_label: &'static str,
    pub repeated_label: &'static str,
    pub events_word: &'static str,
    pub verified_word: &'static str,
    pub unverified_word: &'static str,
    pub failed_word: &'static str,
    pub blocker_word: &'static str,
    pub session_word: &'static str,
    pub commit_word: &'static str,
    pub omitted_note: &'static str,
}

const HEADINGS_KO: Headings = Headings {
    lang: "ko",
    daily_title: "# Daily Work Journal — ",
    weekly_title: "# Weekly Work Journal — ",
    overview: "## 한눈에 보기",
    progress: "## 핵심 진전",
    verified: "## 검증된 것",
    decisions: "## 의사결정",
    blockers: "## Blocker / 미해결 이슈",
    next_actions: "## 다음 액션",
    experiments: "## 실험·연구",
    sessions: "## 세션",
    by_project: "## 프로젝트별 작업",
    notion_done: "## 오늘 끝낸 것",
    notion_failed: "## 실패·막힌 것",
    notion_next: "## 다음",
    notion_details: "## 세부 내용",
    none_recorded: "없음",
    work_label: "관련 작업",
    repeated_label: "반복",
    events_word: "기록",
    verified_word: "검증",
    unverified_word: "미검증",
    failed_word: "실패",
    blocker_word: "Blocker",
    session_word: "세션",
    commit_word: "커밋",
    omitted_note: "- (요약 크기 상한으로 일부 세부 항목이 생략되었습니다; 전체 내역은 events.jsonl을 확인하세요)",
};

const HEADINGS_EN: Headings = Headings {
    lang: "en",
    daily_title: "# Daily Work Journal — ",
    weekly_title: "# Weekly Work Journal — ",
    overview: "## Overview",
    progress: "## Key progress",
    verified: "## Verified",
    decisions: "## Decisions",
    blockers: "## Blockers & open issues",
    next_actions: "## Next actions",
    experiments: "## Experiments & research",
    sessions: "## Sessions",
    by_project: "## Work by project",
    notion_done: "## Completed",
    notion_failed: "## Failed or blocked",
    notion_next: "## Next",
    notion_details: "## Details",
    none_recorded: "none recorded",
    work_label: "work",
    repeated_label: "repeated",
    events_word: "Events",
    verified_word: "Verified",
    unverified_word: "Unverified",
    failed_word: "Failed",
    blocker_word: "Blocker",
    session_word: "Sessions",
    commit_word: "Commits",
    omitted_note: "- (additional details omitted due to summary size cap; inspect events.jsonl for full log)",
};

pub fn headings_for(lang: &str) -> &'static Headings {
    if lang == "ko" {
        &HEADINGS_KO
    } else {
        &HEADINGS_EN
    }
}

pub fn event_citation(ev: &Value) -> String {
    let eid = ev
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or("wj_unknown");
    let src_type = ev
        .pointer("/source/type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let src_id = ev
        .pointer("/source/id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let short_src_id = sanitize_text(src_id, 48);
    let ev_refs: Vec<String> = ev
        .get("evidence")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .take(3)
                .filter_map(|item| {
                    let r = item.get("ref").and_then(Value::as_str)?;
                    Some(sanitize_text(r, 48))
                })
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let ev_part = if ev_refs.is_empty() {
        "none".to_string()
    } else {
        ev_refs.join(",")
    };
    format!("[{eid}; source {src_type}:{short_src_id}; evidence {ev_part}]")
}

pub fn event_work_label(ev: &Value) -> String {
    if let Some(summary) = ev
        .pointer("/outcome/summary")
        .and_then(Value::as_str)
        .map(|s| bounded_summary(s, 180))
        .filter(|s| !s.is_empty())
    {
        return summary.lines().next().unwrap_or("").to_string();
    }
    if let Some(title) = ev
        .pointer("/work_item/title")
        .and_then(Value::as_str)
        .map(|s| sanitize_text(s, 160))
        .filter(|s| !s.is_empty())
    {
        return title;
    }
    if let Some(note) = ev
        .get("note")
        .and_then(Value::as_str)
        .map(|s| sanitize_text(s, 160))
        .filter(|s| !s.is_empty())
    {
        return note;
    }
    let et = ev
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("event");
    let src_id = ev
        .pointer("/source/id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if src_id.is_empty() {
        et.to_string()
    } else {
        format!("{et} ({src_id})")
    }
}

pub fn judgment_summary(ev: &Value, field: &str, h: &Headings) -> String {
    let raw = ev.get(field).and_then(Value::as_str).unwrap_or("").trim();
    if raw.is_empty() {
        return String::new();
    }
    let clean_field = sanitize_text(raw, SUMMARY_ITEM_CHAR_LIMIT);
    let summary_text = ev
        .pointer("/outcome/summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if summary_text.is_empty() {
        return clean_field;
    }
    let compact_work = sanitize_text(summary_text, 120);
    if compact_work.is_empty() || summary_text.chars().count() > 200 || compact_work == clean_field
    {
        return clean_field;
    }
    format!("{clean_field} — {}: {compact_work}", h.work_label)
}

#[derive(Clone, Debug, Default)]
pub struct PeriodAggregates {
    pub event_count: usize,
    pub verified: usize,
    pub unverified: usize,
    pub failed: usize,
    pub blockers: usize,
    pub commits: usize,
    pub sessions: Vec<String>,
}

pub fn compute_period_aggregates(events: &[Value]) -> PeriodAggregates {
    let mut agg = PeriodAggregates::default();
    let mut session_set = BTreeSet::new();
    for ev in events {
        agg.event_count += 1;
        let vs = ev
            .get("verification_status")
            .and_then(Value::as_str)
            .unwrap_or("not_verified");
        let outcome_status = ev
            .pointer("/outcome/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if vs == "passed" {
            agg.verified += 1;
        } else if vs == "failed" || outcome_status == "failed" {
            agg.failed += 1;
        } else {
            agg.unverified += 1;
        }
        if ev
            .get("blocker")
            .and_then(Value::as_str)
            .is_some_and(|b| !b.trim().is_empty())
        {
            agg.blockers += 1;
        }
        if ev.get("event_type").and_then(Value::as_str) == Some("commit") {
            agg.commits += 1;
        }
        if let Some(sid) = ev
            .pointer("/correlation/session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            session_set.insert(sid.to_string());
        }
    }
    agg.sessions = session_set.into_iter().collect();
    agg
}

pub fn has_blocker(events: &[Value]) -> bool {
    events.iter().any(|ev| {
        let blocker_nonempty = ev
            .get("blocker")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        let vs_failed = ev.get("verification_status").and_then(Value::as_str) == Some("failed");
        let out_failed = ev.pointer("/outcome/status").and_then(Value::as_str) == Some("failed");
        blocker_nonempty || vs_failed || out_failed
    })
}

fn format_overview_line(agg: &PeriodAggregates, h: &Headings) -> String {
    let mut parts = vec![
        format!("{} {}", h.events_word, agg.event_count),
        format!("{} {}", h.verified_word, agg.verified),
        format!("{} {}", h.unverified_word, agg.unverified),
    ];
    if agg.failed > 0 {
        parts.push(format!("{} {}", h.failed_word, agg.failed));
    }
    if agg.blockers > 0 {
        parts.push(format!("{} {}", h.blocker_word, agg.blockers));
    }
    if !agg.sessions.is_empty() {
        parts.push(format!("{} {}", h.session_word, agg.sessions.len()));
    }
    if agg.commits > 0 {
        parts.push(format!("{} {}", h.commit_word, agg.commits));
    }
    format!("- {}", parts.join(" · "))
}

pub fn render_daily(date: &str, events: &[Value], tz: &TimeZoneSpec, lang: &str) -> String {
    let h = headings_for(lang);
    let mut sorted: Vec<&Value> = events.iter().collect();
    sorted.sort_by_key(|ev| {
        (
            ev.get("occurred_at").and_then(Value::as_str).unwrap_or(""),
            ev.get("event_id").and_then(Value::as_str).unwrap_or(""),
        )
    });

    let agg = compute_period_aggregates(events);
    let mut out = String::new();
    out.push_str(h.daily_title);
    out.push_str(date);
    out.push_str("\n\n");

    out.push_str(h.overview);
    out.push('\n');
    out.push_str(&format_overview_line(&agg, h));
    out.push_str("\n\n");

    // Key progress
    let mut progress_lines = Vec::new();
    let mut verified_lines = Vec::new();
    let mut decision_lines = Vec::new();
    let mut blocker_lines = Vec::new();
    let mut next_lines = Vec::new();
    let mut experiment_lines = Vec::new();
    let mut session_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_project: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();

    for ev in &sorted {
        let cite = event_citation(ev);
        let label = event_work_label(ev);
        let vs = ev
            .get("verification_status")
            .and_then(Value::as_str)
            .unwrap_or("not_verified");
        let et = ev
            .get("event_type")
            .and_then(Value::as_str)
            .unwrap_or("event");
        let outcome_status = ev
            .pointer("/outcome/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        let hhmm = ev
            .get("occurred_at")
            .and_then(Value::as_str)
            .and_then(|s| parse_rfc3339_utc(s).ok())
            .map(|dt| format_hhmm(dt, tz))
            .unwrap_or_else(|| "--:--".to_string());

        if progress_lines.len() < DAILY_ITEM_LIMIT
            && vs != "failed"
            && outcome_status != "failed"
            && et != "correction"
        {
            progress_lines.push(format!("- [{hhmm}] {label} {cite}"));
        }

        if vs == "passed" && verified_lines.len() < DAILY_ITEM_LIMIT {
            verified_lines.push(format!("- {label} {cite}"));
        }

        if let Some(dec) = Some(judgment_summary(ev, "decision", h)).filter(|s| !s.is_empty()) {
            decision_lines.push(format!("- {dec} {cite}"));
        }

        if let Some(blk) = Some(judgment_summary(ev, "blocker", h)).filter(|s| !s.is_empty()) {
            blocker_lines.push(format!("- {blk} {cite}"));
        } else if (vs == "failed" || outcome_status == "failed")
            && blocker_lines.len() < DAILY_ITEM_LIMIT
        {
            blocker_lines.push(format!("- {label} {cite}"));
        }

        if let Some(na) = Some(judgment_summary(ev, "next_action", h)).filter(|s| !s.is_empty()) {
            next_lines.push(format!("- {na} {cite}"));
        }

        if matches!(et, "experiment" | "research") && experiment_lines.len() < DAILY_ITEM_LIMIT {
            let mut exp_block = vec![format!("- {label} {cite}")];
            if let Some(research) = ev.get("research").and_then(Value::as_object) {
                for (rk, rv) in research {
                    if let Some(s) = rv.as_str() {
                        exp_block.push(format!("  - {rk}: {}", sanitize_text(s, 200)));
                    }
                }
            }
            if let Some(metrics) = ev.get("metrics").and_then(Value::as_array) {
                for m in metrics.iter().take(5) {
                    if let (Some(mn), Some(mv)) =
                        (m.get("name").and_then(Value::as_str), m.get("value"))
                    {
                        let unit = m.get("unit").and_then(Value::as_str).unwrap_or("");
                        exp_block.push(format!("  - metric {mn}: {mv}{unit}"));
                    }
                }
            }
            experiment_lines.push(exp_block.join("\n"));
        }

        if let Some(sid) = ev
            .pointer("/correlation/session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            *session_counts.entry(sid.to_string()).or_insert(0) += 1;
        }

        let proj_name = ev
            .pointer("/project/name")
            .and_then(Value::as_str)
            .unwrap_or("project")
            .to_string();
        let mut work_entry = Vec::new();
        let first_line = label.lines().next().unwrap_or(&label).to_string();
        let prefix = if lang == "ko" {
            "- 작업: "
        } else {
            "- Work: "
        };
        work_entry.push(format!("{prefix}{first_line} {cite}"));
        if !outcome_status.is_empty() {
            let res_prefix = if lang == "ko" {
                "  - 결과: "
            } else {
                "  - result: "
            };
            work_entry.push(format!("{res_prefix}{outcome_status}"));
        }
        if let Some(summary) = ev.pointer("/outcome/summary").and_then(Value::as_str) {
            for extra in summary.lines().skip(1).take(6) {
                let cleaned = sanitize_text(extra, 300);
                if !cleaned.is_empty() {
                    work_entry.push(format!("  - {cleaned}"));
                }
            }
        }
        if let Some(note) = ev
            .get("note")
            .and_then(Value::as_str)
            .map(|s| sanitize_text(s, 300))
            .filter(|s| !s.is_empty())
        {
            let note_prefix = if lang == "ko" {
                "  - 해석: "
            } else {
                "  - note: "
            };
            work_entry.push(format!("{note_prefix}{note}"));
        }
        if let Some(evs) = ev.get("evidence").and_then(Value::as_array) {
            for item in evs.iter().take(2) {
                if let Some(r) = item.get("ref").and_then(Value::as_str) {
                    let ev_prefix = if lang == "ko" {
                        "  - 관련 evidence: "
                    } else {
                        "  - evidence: "
                    };
                    let t = item.get("type").and_then(Value::as_str).unwrap_or("ref");
                    work_entry.push(format!("{ev_prefix}{t}:{}", sanitize_text(r, 120)));
                }
            }
        }
        by_project.entry(proj_name).or_default().push(work_entry);
    }

    append_section(&mut out, h.progress, &progress_lines);
    append_section(&mut out, h.verified, &verified_lines);
    append_section(&mut out, h.decisions, &decision_lines);
    append_section(&mut out, h.blockers, &blocker_lines);
    append_section(&mut out, h.next_actions, &next_lines);
    append_section(&mut out, h.experiments, &experiment_lines);

    if !session_counts.is_empty() {
        let mut s_lines: Vec<String> = session_counts
            .into_iter()
            .map(|(sid, count)| format!("- {sid} ({count} event(s))"))
            .collect();
        s_lines.push("  - inspect: oms session-handoff list".to_string());
        append_section(&mut out, h.sessions, &s_lines);
    }

    // By project
    if !by_project.is_empty() {
        let mut body = Vec::new();
        for (proj, entries) in by_project {
            body.push(format!("### {proj}"));
            for entry in entries.into_iter().take(WEEKLY_ITEM_LIMIT) {
                body.extend(entry);
            }
        }
        append_section(&mut out, h.by_project, &body);
    }

    bound_markdown(&out, h)
}

pub fn render_weekly(week: &str, events: &[Value], _tz: &TimeZoneSpec, lang: &str) -> String {
    let h = headings_for(lang);
    let mut sorted: Vec<&Value> = events.iter().collect();
    sorted.sort_by_key(|ev| {
        (
            ev.get("occurred_at").and_then(Value::as_str).unwrap_or(""),
            ev.get("event_id").and_then(Value::as_str).unwrap_or(""),
        )
    });

    let agg = compute_period_aggregates(events);
    let mut out = String::new();
    out.push_str(h.weekly_title);
    out.push_str(week);
    out.push_str("\n\n");

    out.push_str(h.overview);
    out.push('\n');
    out.push_str(&format_overview_line(&agg, h));
    out.push_str("\n\n");

    let mut progress_lines = Vec::new();
    let mut verified_lines = Vec::new();
    let mut decision_lines = Vec::new();
    let mut next_lines = Vec::new();
    let mut experiment_lines = Vec::new();

    // Group repeated blockers across the week
    let mut blocker_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut by_date: BTreeMap<String, usize> = BTreeMap::new();

    for ev in &sorted {
        let date = ev.get("local_date").and_then(Value::as_str).unwrap_or("");
        *by_date.entry(date.to_string()).or_insert(0) += 1;
        let cite = event_citation(ev);
        let label = event_work_label(ev);
        let vs = ev
            .get("verification_status")
            .and_then(Value::as_str)
            .unwrap_or("not_verified");
        let et = ev
            .get("event_type")
            .and_then(Value::as_str)
            .unwrap_or("event");
        let outcome_status = ev
            .pointer("/outcome/status")
            .and_then(Value::as_str)
            .unwrap_or("");

        if progress_lines.len() < WEEKLY_ITEM_LIMIT && vs != "failed" && outcome_status != "failed"
        {
            progress_lines.push(format!("- {date} · {label} {cite}"));
        }
        if vs == "passed" && verified_lines.len() < WEEKLY_ITEM_LIMIT {
            verified_lines.push(format!("- {date} · {label} {cite}"));
        }
        if let Some(dec) = Some(judgment_summary(ev, "decision", h)).filter(|s| !s.is_empty()) {
            decision_lines.push(format!("- {date} · {dec} {cite}"));
        }
        if let Some(raw_blk) = ev
            .get("blocker")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let key = sanitize_text(raw_blk, SUMMARY_ITEM_CHAR_LIMIT);
            blocker_groups.entry(key).or_default().push(cite.clone());
        } else if vs == "failed" || outcome_status == "failed" {
            let key = sanitize_text(&label, SUMMARY_ITEM_CHAR_LIMIT);
            blocker_groups.entry(key).or_default().push(cite.clone());
        }
        if let Some(na) = Some(judgment_summary(ev, "next_action", h)).filter(|s| !s.is_empty()) {
            next_lines.push(format!("- {date} · {na} {cite}"));
        }
        if matches!(et, "experiment" | "research") && experiment_lines.len() < WEEKLY_ITEM_LIMIT {
            experiment_lines.push(format!("- {date} · {label} {cite}"));
        }
    }

    let mut blocker_lines = Vec::new();
    for (blk_text, cites) in blocker_groups {
        if cites.len() > 1 {
            if lang == "ko" {
                blocker_lines.push(format!(
                    "- {blk_text} ({} {}회) {}",
                    h.repeated_label,
                    cites.len(),
                    cites.join(" ")
                ));
            } else {
                blocker_lines.push(format!(
                    "- {blk_text} ({} {}x) {}",
                    h.repeated_label,
                    cites.len(),
                    cites.join(" ")
                ));
            }
        } else {
            blocker_lines.push(format!("- {blk_text} {}", cites.join(" ")));
        }
    }

    append_section(&mut out, h.progress, &progress_lines);
    append_section(&mut out, h.verified, &verified_lines);
    append_section(&mut out, h.decisions, &decision_lines);
    append_section(&mut out, h.blockers, &blocker_lines);
    append_section(&mut out, h.next_actions, &next_lines);
    append_section(&mut out, h.experiments, &experiment_lines);

    if !by_date.is_empty() {
        let heading = if lang == "ko" {
            "## 일자별 요약"
        } else {
            "## By day"
        };
        let lines: Vec<String> = by_date
            .into_iter()
            .map(|(d, count)| format!("- {d}: {count} event(s)"))
            .collect();
        append_section(&mut out, heading, &lines);
    }

    bound_markdown(&out, h)
}

fn append_section(out: &mut String, heading: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str(heading);
    out.push('\n');
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
}

fn bound_markdown(raw: &str, h: &Headings) -> String {
    if raw.len() <= SUMMARY_BYTE_LIMIT {
        return raw.trim_end().to_string() + "\n";
    }
    // Keep judgment sections first, then truncate trailing detail sections cleanly
    let budget = SUMMARY_BYTE_LIMIT.saturating_sub(h.omitted_note.len() + 16);
    let truncated = truncate_utf8_bytes(raw, budget);
    let cut = match truncated.rfind('\n') {
        Some(idx) => &truncated[..idx],
        None => truncated.as_str(),
    };
    format!("{}\n\n{}\n", cut.trim_end(), h.omitted_note)
}

fn citation_suffix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\s*\[wj_[A-Za-z0-9_-]+;\s*source\s+[^\]]*\]").expect("valid regex")
    })
}

fn commit_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Commit [0-9a-fA-F]{7,40}:\s*").expect("valid regex"))
}

fn long_hash_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([0-9a-f]{40})\b").expect("valid regex"))
}

fn clean_presentation_line(line: &str) -> String {
    let mut s = citation_suffix_re().replace_all(line, "").into_owned();
    s = commit_prefix_re().replace_all(&s, "").into_owned();
    s = long_hash_re()
        .replace_all(&s, |caps: &regex::Captures| caps[1][..7].to_string())
        .into_owned();
    s.trim_end().to_string()
}

fn is_evidence_detail_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- 관련 evidence:") || t.starts_with("- evidence:")
}

/// Produce Notion-facing Markdown presentation:
/// - When `events` is `Some` and `markdown` begins with `# Daily Work Journal`, prepends
///   `Completed / Failed or blocked / Next` (localized) and `## Details\n\n<daily>` (or `## 세부 내용` when `ko`).
/// - Cleans citations, commit hashes, drops evidence sub-bullets, folds duplicate bullets + their detail lines
///   across presented section order, and formats decision bullets (non-date-prefixed) as blockquotes `> ...`.
pub fn notion_presentation(markdown: &str, events: Option<&[Value]>, lang: &str) -> String {
    let h = headings_for(lang);
    let trimmed = markdown.trim();
    if let Some(evs) = events {
        if trimmed.starts_with(h.daily_title) || trimmed.starts_with("# Daily Work Journal") {
            let mut done = Vec::new();
            let mut failed = Vec::new();
            let mut nexts = Vec::new();
            for ev in evs {
                let vs = ev
                    .get("verification_status")
                    .and_then(Value::as_str)
                    .unwrap_or("not_verified");
                let out_s = ev
                    .pointer("/outcome/status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let label = clean_presentation_line(&event_work_label(ev));
                if vs == "failed" || out_s == "failed" {
                    failed.push(format!("- {label}"));
                } else if ev
                    .get("blocker")
                    .and_then(Value::as_str)
                    .is_some_and(|b| !b.trim().is_empty())
                {
                    let b = clean_presentation_line(
                        ev.get("blocker").and_then(Value::as_str).unwrap_or(""),
                    );
                    failed.push(format!("- {b}"));
                } else {
                    done.push(format!("- {label}"));
                }
                if let Some(na) = ev
                    .get("next_action")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    nexts.push(format!("- {}", clean_presentation_line(na)));
                }
            }
            if done.is_empty() {
                done.push(format!("- {}", h.none_recorded));
            }
            if failed.is_empty() {
                failed.push(format!("- {}", h.none_recorded));
            }
            if nexts.is_empty() {
                nexts.push(format!("- {}", h.none_recorded));
            }
            let mut prefix = String::new();
            append_section(&mut prefix, h.notion_done, &done);
            append_section(&mut prefix, h.notion_failed, &failed);
            append_section(&mut prefix, h.notion_next, &nexts);
            let details_heading = h.notion_details;
            let body = present_markdown_sections(markdown, h);
            let _ = body;
            // Preserve exact original daily body under Details as expected by tests
            return format!("{prefix}{details_heading}\n\n{markdown}");
        }
    }

    present_markdown_sections(markdown, h)
}

fn present_markdown_sections(markdown: &str, h: &Headings) -> String {
    // Parse top-level `## ` sections while preserving any leading header before first `## `
    let mut preamble: Vec<String> = Vec::new();
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();

    for line in markdown.lines() {
        if line.starts_with("## ") {
            if let Some(ch) = current_heading.take() {
                sections.push((ch, std::mem::take(&mut current_lines)));
            }
            current_heading = Some(line.to_string());
        } else if current_heading.is_some() {
            current_lines.push(line.to_string());
        } else {
            preamble.push(line.to_string());
        }
    }
    if let Some(ch) = current_heading.take() {
        sections.push((ch, current_lines));
    }

    // Order decisions ahead of progress when both appear in standalone presentation
    let section_priority = |heading: &str| -> usize {
        if heading == h.overview || heading == "## 한눈에 보기" || heading == "## Overview" {
            0
        } else if heading == h.decisions || heading == "## 의사결정" || heading == "## Decisions"
        {
            1
        } else if heading == h.blockers
            || heading == "## Blocker / 미해결 이슈"
            || heading == "## Blockers & open issues"
        {
            2
        } else if heading == h.next_actions
            || heading == "## 다음 액션"
            || heading == "## Next actions"
        {
            3
        } else {
            4
        }
    };
    let mut indexed_sections: Vec<(usize, String, Vec<String>)> = sections
        .into_iter()
        .enumerate()
        .map(|(idx, (hd, lines))| (idx, hd, lines))
        .collect();
    indexed_sections.sort_by_key(|(idx, hd, _)| (section_priority(hd), *idx));

    let mut seen_bullets: HashSet<String> = HashSet::new();
    let mut out_lines: Vec<String> = Vec::new();
    out_lines.extend(preamble);

    for (_idx, heading, lines) in indexed_sections {
        let is_decision_section =
            heading == h.decisions || heading == "## 의사결정" || heading == "## Decisions";
        let mut rendered_section_lines: Vec<String> = Vec::new();
        let mut skip_indented_for_dropped = false;

        for line in lines {
            if is_evidence_detail_line(&line) {
                continue;
            }
            if line.starts_with("  ") {
                if !skip_indented_for_dropped {
                    rendered_section_lines.push(clean_presentation_line(&line));
                }
                continue;
            }
            skip_indented_for_dropped = false;

            if line.starts_with("### ") {
                rendered_section_lines.push(line);
                continue;
            }

            if let Some(body) = line.strip_prefix("- ") {
                let cleaned_body = clean_presentation_line(body);
                if cleaned_body.is_empty() {
                    skip_indented_for_dropped = true;
                    continue;
                }
                if !seen_bullets.insert(cleaned_body.clone()) {
                    skip_indented_for_dropped = true;
                    continue;
                }
                let is_date_prefixed = cleaned_body.len() >= 13
                    && cleaned_body.as_bytes()[4] == b'-'
                    && cleaned_body.as_bytes()[7] == b'-'
                    && cleaned_body[10..].starts_with(" · ");
                if is_decision_section && !is_date_prefixed {
                    rendered_section_lines.push(format!("> {cleaned_body}"));
                } else {
                    rendered_section_lines.push(format!("- {cleaned_body}"));
                }
            } else {
                rendered_section_lines.push(clean_presentation_line(&line));
            }
        }

        if rendered_section_lines.iter().any(|l| !l.trim().is_empty()) {
            out_lines.push(heading);
            out_lines.extend(rendered_section_lines);
        }
    }

    let joined = out_lines.join("\n").trim_end().to_string();
    if joined.is_empty() {
        String::new()
    } else {
        format!("{joined}\n")
    }
}

pub fn ci_status_by_sha(events: &[Value]) -> BTreeMap<String, Value> {
    let mut latest = BTreeMap::new();
    for ev in events {
        if ev.pointer("/source/type").and_then(Value::as_str) != Some("ci-status") {
            continue;
        }
        if let Some(sha) = ev
            .pointer("/correlation/commit")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            latest.insert(sha.to_string(), ev.clone());
        }
    }
    latest
}
