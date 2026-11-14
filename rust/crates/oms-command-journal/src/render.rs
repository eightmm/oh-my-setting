#![forbid(unsafe_code)]

use crate::json::{JsonExt, Value};
use crate::labels::label;
use crate::sanitize::sanitize_text;
use crate::tz::TimeZoneSpec;
use regex::Regex;
use std::collections::{BTreeMap, HashSet};

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
    let mut parts = vec![text(&ev.at("event_id"))];
    if truth(&ev.at("source").at("type")) && truth(&ev.at("source").at("id")) {
        parts.push(format!(
            "source {}:{}",
            text(&ev.at("source").at("type")),
            text(&ev.at("source").at("id"))
        ));
    }
    for item in ev.at("evidence").as_array().into_iter().flatten().take(3) {
        if truth(&item.at("type")) && truth(&item.at("ref")) {
            parts.push(format!(
                "evidence {}:{}",
                text(&item.at("type")),
                text(&item.at("ref"))
            ));
        }
    }
    format!("[{}]", parts.join("; "))
}
fn display(value: &str) -> String {
    let value = value.trim();
    if !value.ends_with('…') && value.len() >= 1997 {
        format!("{value}…")
    } else {
        value.into()
    }
}
fn text(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        crate::schema::python_str(value)
    }
}
fn truth(value: &Value) -> bool {
    crate::schema::json_truthy(value)
}
fn string(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
fn order(value: &Value) -> (String, String) {
    (string(value, "occurred_at"), string(value, "event_id"))
}
fn sorted(events: &[Value]) -> Vec<&Value> {
    let mut rows: Vec<_> = events.iter().collect();
    rows.sort_by_key(|r| order(r));
    rows
}

pub fn event_work_label(ev: &Value) -> String {
    if truth(&ev.at("outcome").at("summary")) {
        display(&text(&ev.at("outcome").at("summary")))
    } else {
        string(ev, "event_type").replace('_', " ")
    }
}

pub fn judgment_summary(ev: &Value, field: &str, h: &Headings) -> String {
    let raw = text(&ev.at(field));
    let raw = raw.trim();
    let mut rendered = display(raw);
    let context = text(&ev.at("outcome").at("summary"));
    let context = context.trim();
    if !context.is_empty()
        && context != raw
        && raw.chars().count() <= 96
        && context.chars().count() <= 180
    {
        rendered.push_str(&format!(" — {}: {context}", label(h.lang, "context")));
    }
    rendered
}

const FAILED: &[&str] = &[
    "failure",
    "failed",
    "rejected",
    "deferred",
    "cancelled",
    "aborted",
    "parked",
];
fn event_failed(event: &Value) -> bool {
    truth(&event.at("blocker"))
        || matches!(
            string(event, "verification_status").as_str(),
            "failed" | "skipped"
        )
        || FAILED.contains(
            &text(&event.at("outcome").at("status"))
                .to_lowercase()
                .as_str(),
        )
}
fn event_commit_sha(event: &Value) -> String {
    if event.at("source").at("type").as_str().as_deref() == Some("git-commit") {
        return text(&event.at("source").at("id"));
    }
    if truth(&event.at("provenance").at("code_commit")) {
        return text(&event.at("provenance").at("code_commit"));
    }
    event
        .at("refs")
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r.at("type").as_str().as_deref() == Some("commit") && truth(&r.at("id")))
        .map(|r| text(&r.at("id")))
        .unwrap_or_default()
}
fn ci_run_identity(event: &Value) -> String {
    let id = text(&event.at("provenance").at("run_id"));
    let id = id.trim();
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        return format!("run:{id}");
    }
    let urls: Vec<_> = event
        .at("refs")
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r.at("type").as_str().as_deref() == Some("ci-run") && truth(&r.at("url")))
        .map(|r| text(&r.at("url")))
        .collect();
    let source_id = text(&event.at("source").at("id"));
    let run = Regex::new(r"/actions/runs/(\d+)(?:/|$)").expect("pattern");
    let attempt = Regex::new(r"^(.*):attempt:[^:]+:[^:]*$").expect("pattern");
    for (marker, prefix) in [(":run-url:", "url:"), (":run:", "run:")] {
        if let Some((_, tail)) = source_id.split_once(marker) {
            let id = attempt
                .captures(tail)
                .map(|c| c[1].to_owned())
                .unwrap_or_else(|| {
                    tail.rsplit_once(':')
                        .map(|(s, _)| s)
                        .unwrap_or(tail)
                        .to_owned()
                });
            if !id.is_empty() {
                if prefix == "run:" && id.chars().all(|c| c.is_ascii_digit()) {
                    return format!("run:{id}");
                }
                if prefix == "url:" {
                    return run
                        .captures(&id)
                        .map(|c| format!("run:{}", &c[1]))
                        .unwrap_or_else(|| format!("url:{id}"));
                }
            }
        }
    }
    for url in &urls {
        if let Some(c) = run.captures(url) {
            return format!("run:{}", &c[1]);
        }
    }
    if let Some(url) = urls.first() {
        return format!("url:{url}");
    }
    format!(
        "event:{}",
        if truth(&event.at("event_id")) {
            text(&event.at("event_id"))
        } else {
            source_id
        }
    )
}
fn attempt_order(value: &Value) -> (bool, String) {
    let raw = match value {
        Value::Integer(n) => n.as_decimal().to_owned(),
        Value::Float(n) if n.is_finite() => format!("{:.0}", n.trunc()),
        Value::Bool(b) => if *b { "1" } else { "0" }.into(),
        Value::String(s) => s.to_str_checked().unwrap_or_default(),
        _ => String::new(),
    };
    let raw = raw.trim();
    let negative = raw.starts_with('-');
    let digits = raw
        .strip_prefix('-')
        .or_else(|| raw.strip_prefix('+'))
        .unwrap_or(raw);
    if !digits
        .split('_')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return (false, "0".into());
    }
    let digits = digits.replace('_', "");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        (false, "0".into())
    } else {
        (negative, digits.into())
    }
}
fn compare_attempts(a: &(bool, String), b: &(bool, String)) -> std::cmp::Ordering {
    match (a.0, b.0) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => {
            let magnitude = a.1.len().cmp(&b.1.len()).then_with(|| a.1.cmp(&b.1));
            if a.0 { magnitude.reverse() } else { magnitude }
        }
    }
}
fn latest_ci_events(events: &[Value]) -> Vec<&Value> {
    let mut ordered: Vec<_> = sorted(events)
        .into_iter()
        .map(|event| (attempt_order(event.at("provenance").at("attempt")), event))
        .collect();
    ordered.sort_by(|(a, _), (b, _)| compare_attempts(a, b));
    let mut latest: Vec<((String, String), &Value)> = Vec::new();
    for (_, event) in ordered {
        if event.at("source").at("type").as_str().as_deref() != Some("ci-status") {
            continue;
        }
        let sha = event_commit_sha(event);
        let sha = if sha.is_empty() {
            format!("@{}", text(&event.at("event_id")))
        } else {
            sha
        };
        let key = (sha, ci_run_identity(event));
        if let Some((_, value)) = latest.iter_mut().find(|(k, _)| *k == key) {
            *value = event;
        } else {
            latest.push((key, event));
        }
    }
    latest.into_iter().map(|(_, v)| v).collect()
}
fn failed_digest_events(events: &[Value]) -> Vec<&Value> {
    let mut latest: Vec<(Vec<String>, &Value, bool)> = Vec::new();
    for event in sorted(events) {
        let kind = text(&event.at("source").at("type"));
        let id = if truth(&event.at("source").at("id")) {
            text(&event.at("source").at("id"))
        } else {
            text(&event.at("event_id"))
        };
        let task = text(&event.at("correlation").at("task_id"));
        let task = task.trim();
        let operation = text(&event.at("correlation").at("operation_id"));
        let verify = kind == "agent-task"
            && (event.at("event_type").as_str().as_deref() == Some("validation")
                || operation.contains(":verify:"));
        if kind == "ci-status" {
            continue;
        }
        let target = if task.is_empty() {
            id.clone()
        } else {
            task.into()
        };
        let key = if kind == "peer-review" {
            vec!["review".into(), target]
        } else if kind == "patch-land" {
            let sha = event_commit_sha(event);
            vec!["land".into(), if sha.is_empty() { target } else { sha }]
        } else if verify {
            vec!["verify".into(), target]
        } else if !task.is_empty() {
            vec!["worker".into(), kind, task.into()]
        } else {
            vec!["event".into(), kind, id]
        };
        if let Some((_, value, verification)) = latest.iter_mut().find(|(k, _, _)| *k == key) {
            *value = event;
            *verification = verify;
        } else {
            latest.push((key, event, verify));
        }
    }
    for event in latest_ci_events(events) {
        latest.push((vec![], event, false));
    }
    let ids: HashSet<_> = latest
        .into_iter()
        .filter(|(_, e, v)| {
            if *v {
                matches!(
                    string(e, "verification_status").as_str(),
                    "failed" | "skipped"
                )
            } else {
                event_failed(e)
            }
        })
        .map(|(_, e, _)| string(e, "event_id"))
        .collect();
    events
        .iter()
        .filter(|e| ids.contains(&string(e, "event_id")))
        .collect()
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
    let mut aggregate = PeriodAggregates::default();
    for event in events {
        aggregate.event_count += 1;
        aggregate.verified += usize::from(string(event, "verification_status") == "passed");
        aggregate.unverified += usize::from(string(event, "verification_status") == "not_verified");
        aggregate.blockers += usize::from(truth(&event.at("blocker")));
        aggregate.commits += usize::from(string(event, "event_type") == "commit");
        let sid = text(&event.at("correlation").at("session_id"));
        let sid = sid.trim();
        if !sid.is_empty() && !aggregate.sessions.iter().any(|v| v == sid) {
            aggregate.sessions.push(sid.into());
        }
    }
    aggregate.failed = failed_digest_events(events).len();
    aggregate
}
pub fn has_blocker(events: &[Value]) -> bool {
    events.iter().any(|e| truth(&e.at("blocker")))
}
fn overview(events: &[Value], lang: &str) -> String {
    let a = compute_period_aggregates(events);
    let count = |field: &str| {
        events
            .iter()
            .filter(|e| truth(&e.at(field)))
            .map(|e| text(&e.at(field)).trim().to_owned())
            .collect::<HashSet<_>>()
            .len()
    };
    crate::labels::format(
        label(lang, "overview_line"),
        &[
            a.event_count,
            a.verified,
            a.unverified,
            a.failed,
            count("decision"),
            count("blocker"),
            count("next_action"),
        ]
        .map(|v| v.to_string()),
    )
}
fn section(lines: &mut Vec<String>, heading: &str, body: Vec<String>, lang: &str) {
    lines.push(format!("## {heading}"));
    lines.push(String::new());
    if body.is_empty() {
        lines.push(label(lang, "empty").into());
    } else {
        lines.extend(body);
    }
    lines.push(String::new());
}
fn cited(event: &Value) -> String {
    format!("- {} {}", event_work_label(event), event_citation(event))
}

pub fn render_daily(period: &str, events: &[Value], _tz: &TimeZoneSpec, lang: &str) -> String {
    let rows = sorted(events);
    let h = headings_for(lang);
    let project = rows
        .first()
        .map(|r| text(&r.at("project_name")))
        .unwrap_or("project".into());
    let mut lines = vec![format!("# Daily Work Journal — {period}"), String::new()];
    section(
        &mut lines,
        label(lang, "overview"),
        vec![overview(events, lang)],
        lang,
    );
    section(
        &mut lines,
        label(lang, "daily_progress"),
        rows.iter()
            .filter(|e| string(e, "event_type") != "handoff")
            .map(|e| cited(e))
            .collect(),
        lang,
    );
    let mut work = vec![format!("### {project}"), String::new()];
    if rows.is_empty() {
        work.push(label(lang, "empty").into());
    }
    for event in &rows {
        work.push(format!(
            "- {}: {} {}",
            label(lang, "work"),
            event_work_label(event),
            event_citation(event)
        ));
        let refs: Vec<_> = event
            .at("evidence")
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| truth(&r.at("type")) && truth(&r.at("ref")))
            .map(|r| format!("{}:{}", text(&r.at("type")), text(&r.at("ref"))))
            .collect();
        if !refs.is_empty() {
            work.push(format!(
                "  - {}: {}",
                label(lang, "evidence"),
                refs.join(", ")
            ));
        }
        for (key, title) in [("status", "result"), ("interpretation", "interpretation")] {
            if truth(&event.at("outcome").at(key)) {
                work.push(format!(
                    "  - {}: {}",
                    label(lang, title),
                    text(&event.at("outcome").at(key))
                ));
            }
        }
    }
    section(&mut lines, label(lang, "daily_by_project"), work, lang);
    for (heading, status) in [("verified", "passed"), ("unverified", "not_verified")] {
        section(
            &mut lines,
            label(lang, heading),
            rows.iter()
                .filter(|e| string(e, "verification_status") == status)
                .map(|e| cited(e))
                .collect(),
            lang,
        );
    }
    section(
        &mut lines,
        label(lang, "failed"),
        rows.iter()
            .filter(|e| event_failed(e))
            .map(|e| cited(e))
            .collect(),
        lang,
    );
    for (field, heading) in [
        ("decision", "decisions"),
        ("blocker", "blockers"),
        ("next_action", "next"),
    ] {
        section(
            &mut lines,
            label(lang, heading),
            rows.iter()
                .filter(|e| truth(&e.at(field)))
                .map(|e| format!("- {} {}", judgment_summary(e, field, h), event_citation(e)))
                .collect(),
            lang,
        );
    }
    let mut ids = Vec::new();
    for event in &rows {
        let id = text(&event.at("correlation").at("session_id"));
        let id = id.trim();
        if !id.is_empty() && !ids.iter().any(|v| v == id) {
            ids.push(id.to_owned());
        }
    }
    let mut sessions: Vec<_> = ids
        .iter()
        .map(|id| {
            format!(
                "- {id} ({} event(s))",
                rows.iter()
                    .filter(|e| text(&e.at("correlation").at("session_id")) == *id)
                    .count()
            )
        })
        .collect();
    if !sessions.is_empty() {
        sessions.push(format!("- {}", label(lang, "sessions_restore")));
    }
    section(&mut lines, label(lang, "sessions"), sessions, lang);
    lines.join("\n")
}

pub fn render_weekly(period: &str, events: &[Value], _tz: &TimeZoneSpec, lang: &str) -> String {
    let rows = sorted(events);
    let h = headings_for(lang);
    let project = rows
        .first()
        .map(|r| text(&r.at("project_name")))
        .unwrap_or("project".into());
    let mut lines = vec![format!("# Weekly Work Journal — {period}"), String::new()];
    section(
        &mut lines,
        label(lang, "overview"),
        vec![overview(events, lang)],
        lang,
    );
    let mut progress = vec![format!("### {project}"), String::new()];
    if rows.is_empty() {
        progress.push(label(lang, "empty").into());
    } else {
        progress.extend(rows.iter().map(|e| cited(e)));
    }
    section(&mut lines, label(lang, "weekly_by_project"), progress, lang);
    section(
        &mut lines,
        label(lang, "weekly_verified"),
        rows.iter()
            .filter(|e| string(e, "verification_status") == "passed")
            .map(|e| cited(e))
            .collect(),
        lang,
    );
    let mut blockers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for event in &rows {
        if truth(&event.at("blocker")) {
            blockers
                .entry(text(&event.at("blocker")))
                .or_default()
                .push(event_citation(event));
        }
    }
    let recurring = blockers
        .into_iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|(b, c)| {
            format!(
                "- {b} ({}) {}",
                crate::labels::format(label(lang, "repeated"), &[c.len().to_string()]),
                c.join(" ")
            )
        })
        .collect();
    section(
        &mut lines,
        label(lang, "recurring_blockers"),
        recurring,
        lang,
    );
    section(
        &mut lines,
        label(lang, "weekly_decisions"),
        rows.iter()
            .filter(|e| truth(&e.at("decision")))
            .map(|e| {
                format!(
                    "- {} · {} {}",
                    text(&e.at("local_date")),
                    judgment_summary(e, "decision", h),
                    event_citation(e)
                )
            })
            .collect(),
        lang,
    );
    let mut groups: BTreeMap<Vec<String>, Vec<(String, String)>> = BTreeMap::new();
    for event in &rows {
        for metric in event.at("metrics").as_array().into_iter().flatten() {
            let signature = ["name", "unit", "dataset", "split", "conditions"]
                .iter()
                .map(|k| {
                    metric
                        .get(k)
                        .map(crate::schema::python_str)
                        .unwrap_or_default()
                })
                .collect();
            groups
                .entry(signature)
                .or_default()
                .push((text(&metric.at("value")), event_citation(event)));
        }
    }
    let trends = groups
        .into_iter()
        .filter(|(s, p)| p.len() >= 2 && s.iter().all(|v| !v.is_empty()))
        .map(|(s, p)| {
            format!(
                "- {} (unit={}, dataset={}, split={}, conditions={}): {} {}",
                s[0],
                s[1],
                s[2],
                s[3],
                s[4],
                p.iter()
                    .map(|(v, _)| v.clone())
                    .collect::<Vec<_>>()
                    .join(" → "),
                p.iter()
                    .map(|(_, c)| c.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    section(&mut lines, label(lang, "trends"), trends, lang);
    section(
        &mut lines,
        label(lang, "weekly_next"),
        rows.iter()
            .filter(|e| truth(&e.at("next_action")))
            .map(|e| {
                format!(
                    "- {} · {} {}",
                    text(&e.at("local_date")),
                    judgment_summary(e, "next_action", h),
                    event_citation(e)
                )
            })
            .collect(),
        lang,
    );
    lines.join("\n")
}

fn status(value: &str, lang: &str) -> String {
    let normalized = value.trim().to_lowercase();
    let value = if normalized.is_empty() {
        "unknown"
    } else {
        &normalized
    };
    let translated = label(lang, &format!("digest_status_{value}"));
    if translated.is_empty() {
        value.into()
    } else {
        translated.into()
    }
}
fn templ(lang: &str, key: &str, args: &[String]) -> String {
    crate::labels::format(label(lang, key), args)
}
fn latest_by<'a>(
    events: impl Iterator<Item = &'a Value>,
    key: impl Fn(&Value) -> String,
) -> Vec<(String, &'a Value)> {
    let mut latest: Vec<(String, &Value)> = Vec::new();
    for event in events {
        let k = key(event);
        if k.is_empty() {
            continue;
        }
        if let Some((_, v)) = latest.iter_mut().find(|(id, _)| *id == k) {
            *v = event;
        } else {
            latest.push((k, event));
        }
    }
    latest
}
pub fn ci_status_by_sha(events: &[Value]) -> BTreeMap<String, Value> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for event in events {
        let sha = event_commit_sha(event);
        if !sha.is_empty() {
            let state = text(&event.at("outcome").at("status"));
            groups.entry(sha).or_default().push(if state.is_empty() {
                "pending".into()
            } else {
                state.to_lowercase()
            });
        }
    }
    groups
        .into_iter()
        .map(|(sha, values)| {
            let result = if values.iter().any(|v| {
                [
                    "failure",
                    "failed",
                    "cancelled",
                    "timed_out",
                    "startup_failure",
                ]
                .contains(&v.as_str())
            }) {
                "failure"
            } else if values.iter().any(|v| v != "success") {
                "pending"
            } else {
                "success"
            };
            (sha, Value::String(result.into()))
        })
        .collect()
}
fn daily_notion_sections(events: &[Value], lang: &str) -> Vec<(String, Vec<String>)> {
    let ordered = sorted(events);
    let h = headings_for(lang);
    let mut reviews: Vec<(String, Vec<&Value>)> = Vec::new();
    let mut unlinked = Vec::new();
    for event in &ordered {
        if event.at("source").at("type").as_str().as_deref() != Some("peer-review") {
            continue;
        }
        let task = text(&event.at("correlation").at("task_id"));
        let task = task.trim();
        if task.is_empty() {
            unlinked.push(*event);
        } else if let Some((_, rows)) = reviews.iter_mut().find(|(id, _)| id == task) {
            rows.push(*event);
        } else {
            reviews.push((task.into(), vec![*event]));
        }
    }
    let tasks = latest_by(
        ordered
            .iter()
            .copied()
            .filter(|e| e.at("source").at("type").as_str().as_deref() == Some("panel-result")),
        |e| text(&e.at("correlation").at("task_id")),
    );
    let commits = latest_by(
        ordered
            .iter()
            .copied()
            .filter(|e| string(e, "event_type") == "commit"),
        event_commit_sha,
    );
    let ci_events = latest_ci_events(events);
    let ci_values: Vec<_> = ci_events.iter().map(|e| (*e).copy()).collect();
    let mut ci = ci_status_by_sha(&ci_values);
    for (sha, value) in ci
        .iter()
        .map(|(k, v)| (k.clone(), v.copy()))
        .collect::<Vec<_>>()
    {
        ci.entry(sha.chars().take(7).collect()).or_insert(value);
    }
    let failures = failed_digest_events(events);
    let failure_ids: HashSet<_> = failures.iter().map(|e| string(e, "event_id")).collect();
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    let mut decisions = Vec::new();
    let mut represented = HashSet::new();
    for event in &ordered {
        if truth(&event.at("decision")) {
            decisions.push(format!("- {}", judgment_summary(event, "decision", h)));
        }
        let id = string(event, "event_id");
        if truth(&event.at("blocker")) && failure_ids.contains(&id) {
            failed.push(format!("- {}", judgment_summary(event, "blocker", h)));
            represented.insert(id.to_owned());
        }
    }
    let task_prefix =
        Regex::new(r"^Task [^ ]+ (?:completed|accepted|failed|rejected):\s*").expect("pattern");
    let landed = Regex::new(r"landed commit ([0-9a-f]{7,40})").expect("pattern");
    let landed_suffix = Regex::new(r"\s*\(landed commit [0-9a-f]{7,40}\)").expect("pattern");
    let mut linked = HashSet::new();
    let ci_lookup = |sha: &str| {
        ci.get(sha)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    for (task, event) in tasks {
        let id = string(event, "event_id");
        let raw = text(&event.at("outcome").at("status")).to_lowercase();
        let state = status(&raw, lang);
        let summary = text(&event.at("outcome").at("summary"));
        let mut summary = task_prefix
            .replace(summary.lines().next().unwrap_or(""), "")
            .into_owned();
        let mut note = String::new();
        if let Some(c) = landed.captures(&summary) {
            let sha = &c[1];
            let short: String = sha.chars().take(7).collect();
            linked.insert(short.clone());
            let ci_state = ci_lookup(sha);
            let ci_state = if ci_state.is_empty() {
                ci_lookup(&short)
            } else {
                ci_state
            };
            note = templ(
                lang,
                "digest_commit_note",
                &[
                    short,
                    if ci_state.is_empty() {
                        String::new()
                    } else {
                        format!(", CI {}", status(&ci_state, lang))
                    },
                ],
            );
            summary = landed_suffix.replace_all(&summary, "").into_owned();
        }
        let task = crate::sanitize::canonical_truncate(&task, 120);
        let line = format!("- {task}: {state} — {summary}{note}");
        let is_failed = failure_ids.contains(&id);
        let outcome_failed = FAILED.contains(&raw.as_str());
        if is_failed && outcome_failed {
            let reason = if truth(&event.at("blocker")) {
                &event.at("blocker")
            } else {
                &event.at("outcome").at("interpretation")
            };
            failed.push(format!(
                "{line}{}",
                if truth(reason) {
                    format!(" — {}", sanitize_text(&text(reason), 120))
                } else {
                    String::new()
                }
            ));
        } else if !outcome_failed {
            completed.push(line.clone());
        }
        if is_failed
            && !outcome_failed
            && matches!(
                string(event, "verification_status").as_str(),
                "failed" | "skipped"
            )
        {
            failed.push(format!(
                "{line} — {}",
                label(lang, "digest_verification_failed")
            ));
        }
        if is_failed {
            represented.insert(id.to_owned());
        }
    }
    let commit_prefix = Regex::new(r"^Commit [0-9a-f]{7,40}:\s*").expect("pattern");
    let mut commit_details = Vec::new();
    for (sha, event) in &commits {
        let subject = text(&event.at("outcome").at("summary"));
        let subject = commit_prefix.replace(&subject, "");
        let mut subject_short: String = subject
            .chars()
            .take(80)
            .collect::<String>()
            .trim_end()
            .into();
        if subject.chars().count() > 80 {
            subject_short.push('…');
        }
        let raw = ci_lookup(sha);
        let state = status(&raw, lang);
        let short: String = sha.chars().take(7).collect();
        let suffix = if raw.is_empty() {
            String::new()
        } else {
            templ(
                lang,
                "digest_ci_status_suffix",
                std::slice::from_ref(&state),
            )
        };
        let line = templ(
            lang,
            "digest_commit_line",
            &[subject_short.clone(), short.clone(), suffix],
        );
        commit_details.push(line.clone());
        if raw == "success" && !linked.contains(&short) {
            completed.push(templ(
                lang,
                "digest_ci_success_line",
                &[subject_short, short],
            ));
        } else if [
            "failure",
            "failed",
            "cancelled",
            "timed_out",
            "startup_failure",
        ]
        .contains(&raw.as_str())
        {
            failed.push(line);
            for event in &ci_events {
                if event_commit_sha(event) == *sha
                    && [
                        "failure",
                        "failed",
                        "cancelled",
                        "timed_out",
                        "startup_failure",
                    ]
                    .contains(
                        &text(&event.at("outcome").at("status"))
                            .to_lowercase()
                            .as_str(),
                    )
                {
                    represented.insert(string(event, "event_id").to_owned());
                }
            }
        }
    }
    let known: HashSet<String> = commits
        .iter()
        .map(|(sha, _)| sha.clone())
        .chain(linked)
        .collect();
    let mut unmatched = Vec::new();
    let mut completed_ci = HashSet::new();
    for event in ci_events {
        let sha = event_commit_sha(event);
        let short: String = sha.chars().take(7).collect();
        let raw = text(&event.at("outcome").at("status"));
        let raw = if raw.is_empty() {
            "unknown".into()
        } else {
            raw.to_lowercase()
        };
        let aggregate = if sha.is_empty() {
            raw.clone()
        } else {
            let state = ci_lookup(&sha);
            if state.is_empty() { raw.clone() } else { state }
        };
        let state = status(&raw, lang);
        let matched = known
            .iter()
            .any(|s| s == &sha || s.chars().take(7).collect::<String>() == short);
        let covered = commits
            .iter()
            .any(|(s, _)| s == &sha || s.chars().take(7).collect::<String>() == short);
        let line = |key: &str, state: &str| {
            if short.is_empty() {
                templ(lang, "digest_ci_unlinked", &[state.into()])
            } else {
                templ(lang, key, &[state.into(), short.clone()])
            }
        };
        if [
            "failure",
            "failed",
            "cancelled",
            "timed_out",
            "startup_failure",
        ]
        .contains(&raw.as_str())
            && !covered
        {
            failed.push(line("digest_ci_for_commit", &state));
            represented.insert(string(event, "event_id").to_owned());
        }
        if !matched {
            if aggregate == "success" && !completed_ci.contains(&sha) {
                completed.push(line("digest_ci_for_commit", &status(&aggregate, lang)));
                if !sha.is_empty() {
                    completed_ci.insert(sha);
                }
            }
            unmatched.push(line("digest_ci_detail", &state));
        }
    }
    let mut review_details = Vec::new();
    for (task, rows) in reviews {
        let event = rows[rows.len() - 1];
        let passed = string(event, "verification_status") == "passed"
            || ["success", "passed"].contains(
                &text(&event.at("outcome").at("status"))
                    .to_lowercase()
                    .as_str(),
            );
        let verdict = label(
            lang,
            if passed {
                "digest_review_passed"
            } else {
                "digest_review_failed"
            },
        );
        let count = if rows.len() > 1 {
            format!(
                " ({})",
                templ(lang, "digest_review_count", &[rows.len().to_string()])
            )
        } else {
            String::new()
        };
        let task = crate::sanitize::canonical_truncate(&task, 120);
        review_details.push(format!(
            "- {}",
            templ(
                lang,
                "digest_review_linked",
                &[task.clone(), verdict.into(), count.clone()]
            )
        ));
        if !passed && failure_ids.contains(&string(event, "event_id")) {
            let reason = if truth(&event.at("blocker")) {
                &event.at("blocker")
            } else {
                &event.at("outcome").at("interpretation")
            };
            let suffix = if truth(reason) {
                format!(" — {}", sanitize_text(&text(reason), 120))
            } else {
                String::new()
            };
            failed.push(format!(
                "- {}{count}",
                templ(lang, "digest_review_linked_failure", &[task, suffix])
            ));
            represented.insert(string(event, "event_id").to_owned());
        }
    }
    for event in unlinked {
        let id = if truth(&event.at("source").at("id")) {
            text(&event.at("source").at("id"))
        } else if truth(&event.at("event_id")) {
            text(&event.at("event_id"))
        } else {
            "unknown review".into()
        };
        let passed = string(event, "verification_status") == "passed"
            || ["success", "passed"].contains(
                &text(&event.at("outcome").at("status"))
                    .to_lowercase()
                    .as_str(),
            );
        let verdict = label(
            lang,
            if passed {
                "digest_review_passed"
            } else {
                "digest_review_failed"
            },
        );
        review_details.push(format!(
            "- {}",
            templ(
                lang,
                "digest_review_unlinked",
                &[id.clone(), verdict.into()]
            )
        ));
        if !passed && failure_ids.contains(&string(event, "event_id")) {
            let reason = if truth(&event.at("blocker")) {
                &event.at("blocker")
            } else {
                &event.at("outcome").at("interpretation")
            };
            let suffix = if truth(reason) {
                format!(" — {}", sanitize_text(&text(reason), 120))
            } else {
                String::new()
            };
            failed.push(format!(
                "- {}",
                templ(lang, "digest_review_unlinked_failure", &[id, suffix])
            ));
            represented.insert(string(event, "event_id").to_owned());
        }
    }
    for event in failures {
        let id = string(event, "event_id");
        if represented.contains(&id) {
            continue;
        }
        let summary = sanitize_text(&text(&event.at("outcome").at("summary")), 120);
        let summary = if summary.is_empty() {
            let kind = text(&event.at("source").at("type"));
            if kind.is_empty() {
                "Event failed".into()
            } else {
                kind
            }
        } else {
            summary
        };
        let raw = text(&event.at("outcome").at("status"));
        let raw = if raw.is_empty() {
            "unknown".into()
        } else {
            raw.to_lowercase()
        };
        let state = if matches!(
            string(event, "verification_status").as_str(),
            "failed" | "skipped"
        ) && !FAILED.contains(&raw.as_str())
        {
            label(lang, "digest_verification_failed").into()
        } else {
            status(&raw, lang)
        };
        let mut details = vec![templ(lang, "digest_failure_status", &[state])];
        if !event.at("provenance").at("exit_code").is_null() {
            details.push(templ(
                lang,
                "digest_failure_exit",
                &[text(&event.at("provenance").at("exit_code"))],
            ));
        }
        failed.push(format!("- {summary} ({})", details.join("; ")));
        represented.insert(id.into());
    }
    let mut seen = HashSet::new();
    let mut next = Vec::new();
    for event in ordered {
        let action = text(&event.at("next_action"));
        let action = action.trim();
        if !action.is_empty() && seen.insert(action.to_owned()) {
            next.push(format!("- {}", sanitize_text(action, 120)));
        }
    }
    let mut sections = Vec::new();
    for (key, mut lines, required) in [
        ("digest_completed", completed, true),
        ("digest_failed", failed, true),
        ("digest_next", next, true),
        ("digest_decisions", decisions, false),
        ("digest_reviews", review_details, false),
        ("digest_commits", commit_details, false),
        ("digest_ci", unmatched, false),
    ] {
        if lines.is_empty() {
            if required {
                lines.push(label(lang, "digest_no_records").into());
            } else {
                continue;
            }
        }
        sections.push((label(lang, key).into(), lines));
    }
    sections
}
fn rank(title: &str) -> i32 {
    match title {
        "한눈에 보기" | "At a glance" => -1,
        "의사결정" | "Decisions" | "주요 의사결정" | "Key decisions" => 0,
        "Blockers" | "반복 Blockers" | "Recurring blockers" => 1,
        "다음 우선순위" | "Next priorities" | "다음 주 우선순위" | "Next week priorities" => {
            2
        }
        "실패하거나 보류한 접근" | "Failed or parked approaches" => 4,
        "핵심 진전"
        | "Key progress"
        | "프로젝트별 작업"
        | "Work by project"
        | "프로젝트별 진전"
        | "Progress by project" => 5,
        "세션" | "Sessions" => 6,
        _ => 3,
    }
}
fn empty(line: &str) -> bool {
    ["- 기록 없음", "- none", "- none recorded"].contains(&line)
}
fn partition(content: &str) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let mut preamble = Vec::new();
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in crate::unicode::text_lines(content) {
        if let Some(title) = line.strip_prefix("## ") {
            sections.push((title.trim().into(), vec![line.into()]));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push(line.into());
        } else {
            preamble.push(line.into());
        }
    }
    (preamble, sections)
}
pub fn notion_presentation(content: &str, events: Option<&[Value]>, lang: &str) -> String {
    if content.trim_start().starts_with("# Daily Work Journal") {
        let Some(events) = events else {
            return content.into();
        };
        let work: Vec<_> = events
            .iter()
            .filter(|e| e.at("source").at("type").as_str().as_deref() != Some("session-handoff"))
            .map(|v| v.copy())
            .collect();
        let mut sections = daily_notion_sections(events, lang);
        sections.insert(
            0,
            (label(lang, "overview").into(), vec![overview(&work, lang)]),
        );
        let mut lines = Vec::new();
        for (title, body) in sections {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("## {title}"));
            lines.extend(body.into_iter().map(|line| {
                if rank(&title) == 0 {
                    line.strip_prefix("- ")
                        .map(|v| format!("> {v}"))
                        .unwrap_or(line)
                } else {
                    line
                }
            }));
        }
        return format!(
            "{}\n\n## {}\n\n{content}",
            lines.join("\n"),
            label(lang, "digest_details")
        );
    }
    let citation = Regex::new(r"\s*\[wj_[0-9a-f]{8,}.*\]\s*$").expect("pattern");
    let prefix = Regex::new(r"^- ((?:작업|work): )?Commit [0-9a-f]{7,40}: ").expect("pattern");
    let hash = Regex::new(r"\b([0-9a-f]{7})[0-9a-f]{33}\b").expect("pattern");
    let evidence = Regex::new(r"^\s*- (관련 evidence|related evidence): ").expect("pattern");
    let long = Regex::new(r"(Commit [0-9a-f]{7,40}: )(.{80})[^\s]*").expect("pattern");
    let mut lines = Vec::new();
    for raw in crate::unicode::text_lines(content) {
        if evidence.is_match(raw) {
            continue;
        }
        let mut line = raw.to_owned();
        if line.starts_with("- ") {
            line = long
                .replace_all(&line, |c: &regex::Captures| {
                    format!("{}{}…", &c[1], c[2].trim_end())
                })
                .into_owned();
            line = citation.replace_all(&line, "").into_owned();
            line = prefix
                .replace_all(&line, |c: &regex::Captures| {
                    format!("- {}", c.get(1).map(|m| m.as_str()).unwrap_or(""))
                })
                .into_owned();
        }
        lines.push(hash.replace_all(&line, "${1}").into_owned());
    }
    let (mut preamble, mut sections) = partition(&lines.join("\n"));
    sections.sort_by_key(|(title, _)| rank(title));
    let weekly = preamble
        .iter()
        .any(|l| l.starts_with("# Weekly Work Journal"));
    let mut seen = HashSet::new();
    let mut fold = |body: Vec<String>| {
        let mut kept = Vec::new();
        let mut dropping = false;
        for line in body {
            if line.starts_with("- ") && !empty(&line) {
                dropping = !seen.insert(line.clone());
                if dropping {
                    continue;
                }
            } else if dropping {
                if line.starts_with([' ', '\t']) && !line.trim().is_empty() {
                    continue;
                }
                dropping = false;
            }
            kept.push(line);
        }
        kept
    };
    preamble = fold(preamble);
    let mut output = preamble;
    for (title, body) in sections {
        let mut body = fold(body);
        let meaningful: Vec<_> = body
            .iter()
            .skip(1)
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .collect();
        if meaningful.is_empty() || meaningful.iter().all(|l| empty(l)) {
            continue;
        }
        if !weekly && rank(&title) == 0 {
            for line in &mut body {
                if line.starts_with('-') && !empty(line) {
                    *line = format!("> {}", line.get(2..).unwrap_or(""));
                }
            }
        }
        while output.last().is_some_and(|l| l.trim().is_empty()) {
            output.pop();
        }
        output.push(String::new());
        output.extend(body);
    }
    output.join("\n").trim_matches('\n').into()
}

pub(crate) fn bounded_export(content: &str, lang: &str) -> String {
    let bound = 65536;
    let sanitized = crate::sanitize::sanitize_multiline(
        content,
        content
            .len()
            .saturating_add(8 * (content.bytes().filter(|b| *b == b'\n').count() + 1))
            .max(bound),
    );
    if sanitized.len() <= bound {
        return sanitized;
    }
    let (mut lines, mut sections) = partition(&sanitized);
    sections.sort_by_key(|(title, _)| rank(title));
    for (_, body) in sections {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.push(String::new());
        lines.extend(body);
    }
    let marker = label(lang, "truncated");
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    for line in lines {
        let separator = usize::from(!kept.is_empty());
        let marker_separator = usize::from(!kept.is_empty() || !line.is_empty());
        if used + separator + line.len() + marker_separator + marker.len() > bound {
            break;
        }
        used += separator + line.len();
        kept.push(line);
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    if kept.is_empty() {
        return crate::sanitize::canonical_truncate(marker, bound);
    }
    kept.push(marker.into());
    kept.join("\n")
}

#[cfg(test)]
#[path = "../tests/unit/render_json.rs"]
mod json_tests;
