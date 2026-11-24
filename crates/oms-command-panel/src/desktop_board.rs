//! Read-only desktop TUI projection over canonical OMS panel producers.
//!
//! Each view returns a bounded flat JSON table (`<= 20` rows per page) with
//! explicit field allowlists, natural Korean column labels, and strict
//! distinction between missing (`null`), zero (`0`), and empty (`""`).
//! No mutations, provider calls, window focusing, shell output, paths,
//! arbitrary command strings, prompts, or secrets leave this module.

use crate::cli::Options;
use crate::commands::{self, Session};
use crate::{Error, Output, Result, Services, rows, text};
use oms_panel_core::{
    NativeCommand, Store, Value, health, jobs, metrics, results, state, teams, workspaces,
};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub const VIEWS: &[&str] = &[
    "overview",
    "health",
    "providers",
    "connections",
    "modelcatalog",
    "attribution",
    "teams",
    "sessions",
    "plan",
    "attention",
    "results",
    "metrics",
    "routes",
    "workspaces",
    "jobs",
    "past",
    "debate",
    "capacity",
    "log",
];

pub const MAX_PAGE_ROWS: usize = 20;
pub const MAX_PAGE: usize = 1000;
const MAX_INPUT_ROWS: usize = 512;
const MAX_TEXT_CHARS: usize = 160;
const MAX_OUTPUT_BYTES: usize = 48 * 1024;
const MAX_WORK_ITEMS: usize = 64;
const MAX_WORK_ITEM_DEPENDS: usize = 32;
// Reserve envelope headroom so structuredContent + escaped MCP text stay <= 60_000 bytes.
const WORK_ITEM_HOST_BUDGET: usize = 56_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopView {
    Overview,
    Health,
    Providers,
    Connections,
    ModelCatalog,
    Attribution,
    Teams,
    Sessions,
    Plan,
    Attention,
    Results,
    Metrics,
    Routes,
    Workspaces,
    Jobs,
}

impl DesktopView {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "overview" => Some(Self::Overview),
            "health" => Some(Self::Health),
            "providers" => Some(Self::Providers),
            "connections" => Some(Self::Connections),
            "modelcatalog" => Some(Self::ModelCatalog),
            "attribution" => Some(Self::Attribution),
            "teams" => Some(Self::Teams),
            "sessions" => Some(Self::Sessions),
            "plan" => Some(Self::Plan),
            "attention" => Some(Self::Attention),
            "results" => Some(Self::Results),
            "metrics" => Some(Self::Metrics),
            "routes" => Some(Self::Routes),
            "workspaces" => Some(Self::Workspaces),
            "jobs" => Some(Self::Jobs),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Health => "health",
            Self::Providers => "providers",
            Self::Connections => "connections",
            Self::ModelCatalog => "modelcatalog",
            Self::Attribution => "attribution",
            Self::Teams => "teams",
            Self::Sessions => "sessions",
            Self::Plan => "plan",
            Self::Attention => "attention",
            Self::Results => "results",
            Self::Metrics => "metrics",
            Self::Routes => "routes",
            Self::Workspaces => "workspaces",
            Self::Jobs => "jobs",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
struct Column {
    key: &'static str,
    label: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct BoardResponse {
    view: &'static str,
    page: usize,
    #[serde(rename = "nextPage")]
    next_page: Option<usize>,
    columns: Vec<Column>,
    rows: Vec<Map<String, Value>>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    incomplete: Option<bool>,
}

fn col(key: &'static str, label: &'static str) -> Column {
    Column { key, label }
}

fn columns_for(view: DesktopView) -> Vec<Column> {
    match view {
        DesktopView::Overview => vec![
            col("section", "영역"),
            col("count", "건수"),
            col("active", "진행 중"),
        ],
        DesktopView::Health => vec![
            col("kind", "구분"),
            col("label", "항목"),
            col("status", "상태"),
            col("used", "사용량"),
            col("limit", "한도"),
            col("advisory", "참고용"),
            col("action", "권장 조치"),
        ],
        DesktopView::Connections => vec![
            col("harness", "하네스"),
            col("owner", "디스패치 소유자"),
            col("transport", "Main 전송 경로"),
            col("installed", "CLI 설치"),
            col("dependencies", "실행에 필요한 도구"),
            col("dependenciesPresent", "도구 모두 발견"),
            col("resumePolicy", "재개 계약"),
            col("accountSelection", "계정 선택 계약"),
            col("version", "확인된 CLI 버전"),
            col("auth", "인증 준비"),
            col("observedAt", "PATH 조회 시각"),
        ],
        DesktopView::ModelCatalog => vec![
            col("provider", "공급자"),
            col("model", "보고된 모델"),
            col("efforts", "이 모델의 강도"),
            col("providerEfforts", "공급자 단위 강도"),
            col("routable", "라우팅 후보"),
            col("source", "출처"),
            col("probe", "캐시 탐지 결과"),
            col("age", "캐시 경과(초)"),
        ],
        DesktopView::Attribution => vec![
            col("attempt", "호출 시도"),
            col("recorded", "기록 시각"),
            col("provider", "호출 공급자"),
            col("requestedModel", "요청 모델"),
            col("selectedModel", "선택 모델"),
            col("resolvedEffort", "결정된 강도"),
            col("selectedEffort", "선택한 강도"),
            col("servedModel", "응답 모델"),
            col("modelEvidence", "응답 근거"),
        ],
        DesktopView::Providers => vec![
            col("provider", "공급자"),
            col("installed", "설치됨"),
            col("usable", "사용 가능"),
            col("modelCount", "모델 수"),
            col("routableCount", "라우팅 가능"),
            col("defaultModel", "기본 모델"),
            col("catalogProbe", "카탈로그 상태"),
            col("catalogAgeSec", "카탈로그 경과(초)"),
        ],
        DesktopView::Teams => vec![
            col("teamId", "팀 ID"),
            col("owner", "루트 소유자"),
            col("taskId", "작업 ID"),
            col("mode", "모드"),
            col("phase", "계약 단계"),
            col("memberCount", "구성원 수"),
            col("intentCount", "물리 인텐트"),
            col("verified", "검증됨"),
            col("admitted", "승인됨"),
            col("landed", "반영됨"),
            col("reason", "사유"),
        ],
        DesktopView::Sessions => vec![
            col("id", "세션 ID"),
            col("provider", "공급자"),
            col("role", "역할"),
            col("model", "모델"),
            col("state", "상태"),
            col("waitingFor", "대기 사유"),
            col("taskId", "작업 ID"),
            col("title", "제목"),
            col("elapsed", "경과"),
        ],
        DesktopView::Plan => vec![
            col("taskId", "작업 ID"),
            col("title", "제목"),
            col("state", "상태"),
            col("provider", "담당 공급자"),
            col("role", "역할"),
            col("dependsCount", "선행 작업 수"),
            col("claimExpired", "점유 만료"),
            col("contractStale", "계약 만료"),
            col("verified", "완료 검증"),
            col("reason", "사유"),
        ],
        DesktopView::Attention => vec![
            col("kind", "유형"),
            col("source", "출처"),
            col("who", "대상"),
            col("taskId", "작업 ID"),
            col("summary", "내용"),
            col("unread", "미확인"),
            col("stamp", "시각"),
        ],
        DesktopView::Results => vec![
            col("taskId", "작업 ID"),
            col("title", "제목"),
            col("state", "대표 상태"),
            col("outcome", "결정"),
            col("implement", "구현"),
            col("verify", "검증"),
            col("review", "리뷰"),
            col("admitted", "승인 대기"),
            col("evidenceCount", "증거 수"),
            col("delivery", "전달 상태"),
            col("issue", "이슈"),
        ],
        DesktopView::Metrics => vec![
            col("category", "분류"),
            col("metric", "지표"),
            col("value", "값"),
            col("unit", "단위"),
            col("samples", "표본 수"),
        ],
        DesktopView::Routes => vec![
            col("role", "역할"),
            col("access", "권한"),
            col("tier", "등급"),
            col("status", "상태"),
            col("provider", "예정 공급자"),
            col("model", "예정 모델"),
            col("effort", "추론 강도"),
            col("requestedModel", "요청 모델"),
            col("requestedEffort", "요청 강도"),
            col("requestedAccount", "요청 별칭"),
            col("selectedAccount", "예정 별칭"),
            col("versionEvidence", "캐시 CLI 버전"),
            col("probeAge", "근거 경과(초)"),
            col("inFlight", "실행 중"),
            col("recentAttempts", "최근 호출"),
            col("issue", "이슈"),
        ],
        DesktopView::Workspaces => vec![
            col("id", "워크스페이스 ID"),
            col("label", "이름"),
            col("state", "상태"),
            col("archived", "보관됨"),
            col("canFocus", "전환 가능"),
        ],
        DesktopView::Jobs => vec![
            col("id", "작업 ID"),
            col("label", "이름"),
            col("state", "상태"),
            col("generation", "세대"),
            col("exitCode", "종료 코드"),
            col("archived", "보관됨"),
            col("logTruncated", "로그 잘림"),
        ],
    }
}

/// Resolve the exact selected Git repository for desktop read projections.
/// Never falls back to a non-Git directory or an unrelated `OMS_ROOM_REPO`.
/// Explicit early read-only entrypoint for `oms panel --desktop-view VIEW [--page N] --json`.
pub(crate) fn run(
    repo: &Path,
    execution: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    checkpoint(stop)?;
    let raw_view = options.required("desktop-view")?;
    let page = options.integer("page", 1, 1, MAX_PAGE)?;
    let private = matches!(
        raw_view.as_str(),
        "sessions" | "attention" | "past" | "debate" | "log"
    );
    let digest = options.get("conversation-digest");
    let bound = if private {
        crate::desktop_history::bound_room(repo, digest, services, stop)?
    } else {
        None
    };
    let binding = bound.as_deref().zip(digest);
    if matches!(raw_view.as_str(), "past" | "debate" | "log" | "capacity") {
        let snapshot = if binding.is_some() {
            read_snapshot(repo, binding, services, stop)?
        } else {
            None
        };
        let report = snapshot.as_ref().map(|s| &s.report).unwrap_or(&Value::Null);
        let value =
            crate::desktop_history::page(repo, report, bound.as_deref(), &raw_view, page, stop)?;
        let bytes = crate::json(&value)?;
        oms_panel_core::outbound(&bytes)?;
        return Ok((0, bytes, vec![]));
    }
    let view = DesktopView::parse(&raw_view).ok_or_else(|| Error::new("unknown desktop view"))?;
    if private && binding.is_none() {
        return finalize_response(unavailable(view, page));
    }
    let room_id = binding;
    let response = match view {
        DesktopView::Overview => project_overview(repo, execution, room_id, page, services, stop)?,
        DesktopView::Health => project_health(repo, room_id, page, services, stop)?,
        DesktopView::Connections => project_connections(page),
        DesktopView::ModelCatalog => project_catalog(repo, page, services, stop)?,
        DesktopView::Attribution => project_attribution(repo, page)?,
        DesktopView::Providers => project_providers(repo, page, services, stop)?,
        DesktopView::Teams => project_teams(repo, execution, page, services, stop)?,
        DesktopView::Sessions => project_sessions(repo, room_id, page, services, stop)?,
        DesktopView::Plan => project_plan(repo, page, services, stop)?,
        DesktopView::Attention => project_attention(repo, room_id, page, services, stop)?,
        DesktopView::Results => project_results(repo, page, services, stop)?,
        DesktopView::Metrics => project_metrics(repo, room_id, page, services, stop)?,
        DesktopView::Routes => {
            project_routes(repo, execution, source, options, None, page, services, stop)?
        }
        DesktopView::Workspaces => project_workspaces(repo, page, services, stop)?,
        DesktopView::Jobs => project_jobs(repo, page, stop)?,
    };
    finalize_response(response)
}

fn checkpoint(stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        Err(Error {
            code: 130,
            ..Error::new("panel operation interrupted")
        })
    } else {
        Ok(())
    }
}

fn unavailable(view: DesktopView, page: usize) -> BoardResponse {
    BoardResponse {
        view: view.as_str(),
        page,
        next_page: None,
        columns: columns_for(view),
        rows: Vec::new(),
        status: "unavailable",
        total: None,
        incomplete: Some(true),
    }
}

fn paginate(
    view: DesktopView,
    page: usize,
    all_rows: Vec<Map<String, Value>>,
    total_known: bool,
    incomplete: bool,
) -> BoardResponse {
    let capped = all_rows.len() > MAX_INPUT_ROWS;
    let bounded_len = all_rows.len().min(MAX_INPUT_ROWS);
    let offset = (page - 1).saturating_mul(MAX_PAGE_ROWS);
    let page_rows: Vec<Map<String, Value>> = all_rows
        .into_iter()
        .take(MAX_INPUT_ROWS)
        .skip(offset)
        .take(MAX_PAGE_ROWS)
        .collect();
    let has_more = offset.saturating_add(page_rows.len()) < bounded_len;
    let next_page = (has_more && page < MAX_PAGE).then_some(page + 1);
    let is_incomplete = incomplete || capped;
    let total = (total_known && !capped).then_some(bounded_len);
    BoardResponse {
        view: view.as_str(),
        page,
        next_page,
        columns: columns_for(view),
        rows: page_rows,
        status: "ready",
        total,
        incomplete: is_incomplete.then_some(true),
    }
}

fn finalize_response(response: BoardResponse) -> Result<Output> {
    let view_name = response.view;
    let page = response.page;
    let columns = response.columns.clone();
    let mut response = response;
    let mut bytes = serde_json::to_vec(&response).map_err(Error::new)?;
    // Multibyte labels must not make rows disappear from a fixed-size page.
    // Retain row identities and counts; shorten only long presentation cells.
    for cap in [96, 48, 24] {
        if bytes.len() <= MAX_OUTPUT_BYTES {
            break;
        }
        for row in &mut response.rows {
            for value in row.values_mut() {
                if let Value::String(text) = value
                    && text.len() > cap
                {
                    let end = text.floor_char_boundary(cap - 3);
                    text.truncate(end);
                    text.push('…');
                }
            }
        }
        response.incomplete = Some(true);
        bytes = serde_json::to_vec(&response).map_err(Error::new)?;
    }
    if bytes.len() <= MAX_OUTPUT_BYTES && oms_panel_core::outbound(&bytes).is_ok() {
        return Ok((0, bytes, vec![]));
    }
    let fallback = BoardResponse {
        view: view_name,
        page,
        next_page: None,
        columns,
        rows: Vec::new(),
        status: "unavailable",
        total: None,
        incomplete: Some(true),
    };
    let fallback_value = serde_json::to_value(&fallback).map_err(Error::new)?;
    let fallback_bytes = crate::json(&fallback_value)?;
    Ok((0, fallback_bytes, vec![]))
}

fn scrub_paths(input: &str) -> String {
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    let re = PATH_RE.get_or_init(|| {
        Regex::new(
            r#"(?P<pre>^|[a-zA-Z][a-zA-Z0-9+.-]*://|[^A-Za-z0-9._/~\\])(?P<path>/(?:[^\s"'`<>|{}()\[\],;:/*]+/)+[^\s"'`<>|{}()\[\],;:/*]*|(?:~/|\.{1,2}/|\.oms/)(?:[^\s"'`<>|{}()\[\],;:/*]+/)*[^\s"'`<>|{}()\[\],;:/*]*|[A-Za-z]:[\\/][^\s"'`<>|{}()\[\],;*]+)"#,
        )
        .expect("valid static path scrubber regex")
    });
    re.replace_all(input, |caps: &regex::Captures<'_>| {
        let pre = caps.name("pre").map_or("", |m| m.as_str());
        format!("{pre}[path]")
    })
    .into_owned()
}

pub(crate) fn redact_and_clean(raw: &str, max_chars: usize) -> Option<String> {
    if raw.is_empty() {
        return Some(String::new());
    }
    let redacted = oms_panel_core::redact(raw.as_bytes()).ok()?;
    let scrubbed = scrub_paths(&redacted);
    let cleaned = crate::render::clean(&scrubbed, usize::MAX);
    if cleaned == scrubbed {
        return Some(cleaned.chars().take(max_chars).collect());
    }
    let re_redacted = oms_panel_core::redact(cleaned.as_bytes()).ok()?;
    let re_scrubbed = scrub_paths(&re_redacted);
    Some(crate::render::clean(&re_scrubbed, max_chars))
}

fn is_outbound_safe(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    if oms_panel_core::outbound(text.as_bytes()).is_err() {
        return false;
    }
    serde_json::to_vec(text)
        .ok()
        .is_some_and(|encoded| oms_panel_core::outbound(&encoded).is_ok())
}

fn sanitize_text(raw: &str, max_chars: usize) -> Option<String> {
    let cleaned = redact_and_clean(raw, max_chars)?;
    is_outbound_safe(&cleaned).then_some(cleaned)
}

fn opt_str(value: &Value, key: &str, max_chars: usize) -> Value {
    match value.get(key) {
        Some(Value::String(s)) => sanitize_text(s, max_chars)
            .map(Value::String)
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn opt_nonempty_str(value: &Value, key: &str, max_chars: usize) -> Value {
    match value.get(key) {
        Some(Value::String(s)) if !s.is_empty() => sanitize_text(s, max_chars)
            .map(Value::String)
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn opt_text_value(raw: Option<&str>, max_chars: usize) -> Value {
    match raw {
        Some(s) => sanitize_text(s, max_chars)
            .map(Value::String)
            .unwrap_or(Value::Null),
        None => Value::Null,
    }
}

fn opt_u64(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn opt_i64(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .and_then(Value::as_i64)
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn opt_bool(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .and_then(Value::as_bool)
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn opt_array_len(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| Value::from(items.len()))
        .unwrap_or(Value::Null)
}

fn round1(value: Option<f64>) -> Value {
    match value.filter(|v| v.is_finite()) {
        Some(v) => json!((v * 10.0).round() / 10.0),
        None => Value::Null,
    }
}

fn row_map(pairs: Vec<(&'static str, Value)>) -> Map<String, Value> {
    let mut map = Map::with_capacity(pairs.len());
    for (k, v) in pairs {
        let scalar = match v {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => v,
            Value::Array(_) | Value::Object(_) => Value::Null,
        };
        map.insert(k.to_owned(), scalar);
    }
    map
}

fn read_snapshot(
    repo: &Path,
    room_id: Option<(&str, &str)>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<state::Snapshot>> {
    checkpoint(stop)?;
    let session = Session::new();
    match commands::snapshot_for(
        &session,
        repo,
        room_id.map(|(id, _)| id),
        None,
        services,
        stop,
        false,
    ) {
        Ok((mut snap, _, _)) => {
            if let Some((id, digest)) = room_id {
                if !crate::desktop_history::confirms_room(&snap.report, id, digest) {
                    return Ok(None);
                }
                let room = &mut snap.report["room"];
                let owner = room["owner"].as_str().unwrap_or_default().to_owned();
                if let Some(messages) = room["messages"].as_array_mut() {
                    messages.retain(|m| crate::desktop_history::visible(m, &owner));
                }
                for key in ["recent", "active"] {
                    if let Some(attempts) = snap.report["attempts"][key].as_array_mut() {
                        attempts.retain(|a| crate::desktop_history::in_room(a, id));
                    }
                }
            }
            Ok(Some(snap))
        }
        Err(err) if err.code == 130 => Err(err),
        Err(_) => Ok(None),
    }
}

fn read_plan_tasks(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<Vec<Value>>> {
    checkpoint(stop)?;
    match commands::query(
        services,
        NativeCommand::Plan,
        repo,
        &["list", "--json"],
        stop,
    ) {
        Ok(value) => {
            if let Some(arr) = value["tasks"].as_array() {
                Ok(Some(arr.clone()))
            } else if let Some(obj) = value["tasks"].as_object() {
                let mut items: Vec<_> = obj.iter().collect();
                items.sort_by_key(|(key, _)| *key);
                Ok(Some(items.into_iter().map(|(_, v)| v.clone()).collect()))
            } else {
                Ok(None)
            }
        }
        Err(err) if err.code == 130 => Err(err),
        Err(_) => Ok(None),
    }
}

fn read_providers(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<Vec<Value>>> {
    checkpoint(stop)?;
    match commands::query(
        services,
        NativeCommand::ProviderCatalog,
        repo,
        &["--json"],
        stop,
    ) {
        Ok(raw) => {
            let list = raw.get("providers").unwrap_or(&raw);
            Ok(list.as_array().cloned())
        }
        Err(err) if err.code == 130 => Err(err),
        Err(_) => Ok(None),
    }
}

fn normalize_session_state(raw: &str) -> (&'static str, &'static str) {
    match raw {
        "queued" | "idle" => ("waiting", "none"),
        "starting" | "working" | "running" | "verifying" | "manual" | "live marker" => {
            ("working", "none")
        }
        "waiting_input" => ("waiting", "input"),
        "waiting_approval" => ("waiting", "approval"),
        "review" => ("waiting", "review"),
        "blocked" => ("blocked", "owner"),
        "done" => ("done", "none"),
        "exited" => ("exited", "none"),
        "cancelled" => ("cancelled", "none"),
        "failed" | "timed_out" | "abandoned" => ("failed", "none"),
        _ => ("unknown", "unknown"),
    }
}

fn project_overview(
    repo: &Path,
    _execution: &Path,
    room_id: Option<(&str, &str)>,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(snap) = read_snapshot(repo, room_id, services, stop)? else {
        return Ok(unavailable(DesktopView::Overview, page));
    };
    let report = &snap.report;
    let tasks = report["plan"]["tasks"].as_object();
    let active = report["attempts"]["active"].as_array();
    let recent = report["attempts"]["recent"].as_array();
    let items = vec![
        row_map(vec![
            ("section", json!("프로젝트 계획")),
            ("count", json!(tasks.map(|t| t.len()))),
        ]),
        row_map(vec![
            ("section", json!("최근 실행")),
            ("count", json!(recent.map(Vec::len))),
            ("active", json!(active.map(Vec::len))),
        ]),
    ];
    Ok(paginate(
        DesktopView::Overview,
        page,
        items,
        true,
        report["collection"]["ok"] != true,
    ))
}
fn project_health(
    repo: &Path,
    room_id: Option<(&str, &str)>,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(snap) = read_snapshot(repo, room_id, services, stop)? else {
        return Ok(unavailable(DesktopView::Health, page));
    };
    let report = &snap.report;
    let projected = health::project(report, oms_panel_core::timestamp() as f64);
    let work = health::work_health(report);
    let collection_ok = work["collection_ok"] == true;
    let incomplete = report["capacity_stale"] == true || !collection_ok;

    let mut out = Vec::new();
    for item in rows(&projected["items"]).iter().take(MAX_INPUT_ROWS + 1) {
        out.push(row_map(vec![
            ("kind", opt_nonempty_str(item, "kind", 32)),
            ("label", opt_str(item, "label", 80)),
            ("status", opt_nonempty_str(item, "status", 32)),
            ("used", opt_u64(item, "used")),
            ("limit", opt_u64(item, "limit")),
            (
                "advisory",
                item.get("advisory")
                    .and_then(Value::as_bool)
                    .map(Value::from)
                    .unwrap_or(Value::Bool(false)),
            ),
            ("action", opt_str(item, "action", MAX_TEXT_CHARS)),
        ]));
    }

    let active_total: Option<u64> = if collection_ok {
        work["states"].as_object().map(|states| {
            states
                .values()
                .filter_map(Value::as_u64)
                .fold(0u64, u64::saturating_add)
        })
    } else {
        None
    };
    out.push(row_map(vec![
        ("kind", json!("work")),
        ("label", json!("활성 작업 시도")),
        (
            "status",
            json!(if collection_ok { "ok" } else { "unknown" }),
        ),
        ("used", active_total.map(Value::from).unwrap_or(Value::Null)),
        ("limit", Value::Null),
        ("advisory", json!(false)),
        (
            "action",
            json!(if collection_ok {
                "수집 정상"
            } else {
                "수집 상태를 확인하세요."
            }),
        ),
    ]));

    Ok(paginate(DesktopView::Health, page, out, true, incomplete))
}

fn account_alias(v: &Value, key: &str) -> Value {
    let safe = v[key].as_str().filter(|s| {
        !s.is_empty()
            && s.len() <= 80
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    });
    opt_text_value(safe, 80)
}

fn project_connections(page: usize) -> BoardResponse {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|t| t.as_secs());
    let data = crate::native_panel::desktop_connections()
        .into_iter()
        .filter_map(|row| {
            let mut cells = row.as_object()?.clone();
            cells.insert("observedAt".into(), json!(now));
            // Cache-only provider reads do not authenticate or execute --version.
            cells.insert("auth".into(), Value::Null);
            cells.insert("version".into(), Value::Null);
            Some(cells)
        })
        .collect();
    paginate(DesktopView::Connections, page, data, true, false)
}

fn effort_label(value: &Value) -> Value {
    let Some(list) = value.as_array() else {
        return Value::Null;
    };
    if list.len() > 32 {
        return Value::Null;
    }
    let Some(terms) = list
        .iter()
        .map(|v| v.as_str().filter(|s| !s.is_empty() && s.len() <= 40))
        .collect::<Option<Vec<_>>>()
    else {
        return Value::Null;
    };
    if terms.is_empty() {
        return Value::Null;
    }
    opt_text_value(Some(&terms.join(", ")), 180)
}

fn project_catalog(
    repo: &Path,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(providers) = read_providers(repo, services, stop)? else {
        return Ok(unavailable(DesktopView::ModelCatalog, page));
    };
    let mut entries = vec![];
    let mut incomplete = false;
    'providers: for provider in providers {
        let Some(catalog) = provider["catalog_models"].as_array() else {
            incomplete = true;
            continue;
        };
        if catalog.is_empty() && provider["catalog_probe"] != "ok" {
            incomplete = true;
        }
        for model in catalog {
            if entries.len() > MAX_INPUT_ROWS {
                break 'providers;
            }
            let Some(name) = model.as_str().filter(|s| !s.is_empty()) else {
                incomplete = true;
                continue;
            };
            entries.push(row_map(vec![
                ("provider", opt_nonempty_str(&provider, "provider", 48)),
                ("model", opt_text_value(Some(name), 128)),
                (
                    "efforts",
                    effort_label(&provider["model_effort_scales"][name]),
                ),
                ("providerEfforts", effort_label(&provider["effort_values"])),
                (
                    "routable",
                    json!(
                        provider["routable"]
                            .as_array()
                            .map(|values| values.contains(model))
                    ),
                ),
                ("source", json!("oms models · cache")),
                ("probe", opt_nonempty_str(&provider, "catalog_probe", 32)),
                ("age", opt_u64(&provider, "catalog_age_seconds")),
            ]));
        }
    }
    Ok(paginate(
        DesktopView::ModelCatalog,
        page,
        entries,
        true,
        incomplete,
    ))
}

fn project_attribution(repo: &Path, page: usize) -> Result<BoardResponse> {
    let Ok(index) = results::index_rows(repo) else {
        return Ok(unavailable(DesktopView::Attribution, page));
    };
    let rows = index
        .iter()
        .rev()
        .filter(|r| {
            matches!(
                r["kind"].as_str(),
                Some("ask" | "review" | "call" | "consult" | "delegate")
            )
        })
        .take(MAX_INPUT_ROWS + 1)
        .map(|r| {
            let verified = r["model_attribution"] == "transport";
            row_map(vec![
                ("attempt", opt_nonempty_str(r, "attempt_id", 96)),
                ("recorded", opt_nonempty_str(r, "ts", 48)),
                ("provider", opt_nonempty_str(r, "provider", 48)),
                ("requestedModel", opt_nonempty_str(r, "requested_model", 96)),
                ("selectedModel", opt_nonempty_str(r, "selected_model", 96)),
                (
                    "resolvedEffort",
                    opt_nonempty_str(r, "reasoning_effort", 32),
                ),
                (
                    "selectedEffort",
                    opt_nonempty_str(r, "selected_reasoning_effort", 32),
                ),
                (
                    "servedModel",
                    if verified {
                        opt_nonempty_str(r, "served_model", 96)
                    } else {
                        Value::Null
                    },
                ),
                (
                    "modelEvidence",
                    opt_nonempty_str(r, "model_attribution", 32),
                ),
            ])
        })
        .collect();
    Ok(paginate(DesktopView::Attribution, page, rows, true, false))
}

fn project_providers(
    repo: &Path,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(list) = read_providers(repo, services, stop)? else {
        return Ok(unavailable(DesktopView::Providers, page));
    };
    let mut out = Vec::new();
    for item in list.iter().take(MAX_INPUT_ROWS + 1) {
        let installed = item
            .get("installed")
            .and_then(Value::as_bool)
            .or_else(|| item.get("present").and_then(Value::as_bool))
            .map(Value::from)
            .unwrap_or(Value::Null);
        let usable = opt_bool(item, "usable");
        let model_count = item
            .get("catalog_models")
            .and_then(Value::as_array)
            .or_else(|| item.get("models").and_then(Value::as_array))
            .map(|models| Value::from(models.len()))
            .unwrap_or(Value::Null);
        let routable_count = opt_array_len(item, "routable");
        let default_model = item["configured_default"]
            .as_str()
            .filter(|s| !s.is_empty());
        out.push(row_map(vec![
            ("provider", opt_nonempty_str(item, "provider", 48)),
            ("installed", installed),
            ("usable", usable),
            ("modelCount", model_count),
            ("routableCount", routable_count),
            ("defaultModel", opt_text_value(default_model, 96)),
            ("catalogProbe", opt_nonempty_str(item, "catalog_probe", 32)),
            ("catalogAgeSec", opt_u64(item, "catalog_age_seconds")),
        ]));
    }
    Ok(paginate(DesktopView::Providers, page, out, true, false))
}

fn project_teams(
    repo: &Path,
    execution: &Path,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    checkpoint(stop)?;
    let report = match teams::read_at(execution, repo, services, stop) {
        Ok(report) => report,
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => return Ok(unavailable(DesktopView::Teams, page)),
    };
    let Some(roots) = report["roots"].as_array() else {
        return Ok(unavailable(DesktopView::Teams, page));
    };
    let omitted = report["omitted_teams"].as_u64().unwrap_or(0);
    let incomplete = omitted > 0
        || report["sources"]["teams"]["status"] == "partial"
        || report["sources"]["resources"]["status"] == "unavailable"
        || report["sources"]["physical_intents"]["status"] == "unavailable";

    let mut out = Vec::new();
    for root in roots {
        let owner = opt_nonempty_str(root, "id", 80);
        for team in rows(&root["teams"]) {
            let members = team.get("members").and_then(Value::as_array);
            let member_count = members.map(|m| Value::from(m.len())).unwrap_or(Value::Null);
            let intent_count = members
                .and_then(|m| {
                    let mut sum = 0usize;
                    for member in m {
                        let intents = member.get("physical_intents").and_then(Value::as_array)?;
                        sum = sum.saturating_add(intents.len());
                    }
                    Some(Value::from(sum))
                })
                .unwrap_or(Value::Null);
            out.push(row_map(vec![
                ("teamId", opt_nonempty_str(team, "id", 96)),
                ("owner", owner.clone()),
                ("taskId", opt_nonempty_str(team, "task_id", 96)),
                ("mode", opt_nonempty_str(team, "mode", 48)),
                ("phase", opt_nonempty_str(team, "contract_phase", 32)),
                ("memberCount", member_count),
                ("intentCount", intent_count),
                ("verified", opt_bool(team, "verified")),
                ("admitted", opt_bool(team, "admitted")),
                ("landed", opt_bool(team, "landed")),
                ("reason", opt_str(team, "reason", MAX_TEXT_CHARS)),
            ]));
        }
    }
    for team in rows(&report["unbound_teams"]) {
        out.push(row_map(vec![
            ("teamId", opt_nonempty_str(team, "id", 96)),
            ("owner", Value::Null),
            ("taskId", Value::Null),
            ("mode", Value::Null),
            ("phase", opt_nonempty_str(team, "contract_phase", 32)),
            ("memberCount", Value::Null),
            ("intentCount", Value::Null),
            ("verified", Value::Null),
            ("admitted", Value::Null),
            ("landed", Value::Null),
            ("reason", opt_str(team, "reason", MAX_TEXT_CHARS)),
        ]));
    }
    Ok(paginate(
        DesktopView::Teams,
        page,
        out,
        omitted == 0,
        incomplete,
    ))
}

fn sessions_response(report: &Value, page: usize) -> BoardResponse {
    let incomplete = report["capacity_stale"] == true || report["collection"]["ok"] != true;
    let calls = crate::render::calls(report);

    let mut out = Vec::new();
    for call in calls.iter().take(MAX_INPUT_ROWS + 1) {
        let id = call
            .get("attempt")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                call.get("participant")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            });
        let role = call
            .get("display_role")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                call.get("role")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            });
        let model = if call["model_state"] == "unverified" {
            Value::Null
        } else {
            opt_nonempty_str(call, "model", 96)
        };
        let (state, waiting) = normalize_session_state(text(call, "state"));
        let elapsed = call
            .get("board_elapsed")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "—");
        out.push(row_map(vec![
            ("id", opt_text_value(id, 96)),
            ("provider", opt_nonempty_str(call, "provider", 48)),
            ("role", opt_text_value(role, 48)),
            ("model", model),
            ("state", json!(state)),
            ("waitingFor", json!(waiting)),
            ("taskId", opt_nonempty_str(call, "task_id", 96)),
            ("title", opt_str(call, "title", MAX_TEXT_CHARS)),
            ("elapsed", opt_text_value(elapsed, 32)),
        ]));
    }
    paginate(DesktopView::Sessions, page, out, true, incomplete)
}

fn project_sessions(
    repo: &Path,
    room_id: Option<(&str, &str)>,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(snap) = read_snapshot(repo, room_id, services, stop)? else {
        return Ok(unavailable(DesktopView::Sessions, page));
    };
    Ok(sessions_response(&snap.report, page))
}

fn project_plan(
    repo: &Path,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(tasks) = read_plan_tasks(repo, services, stop)? else {
        return Ok(unavailable(DesktopView::Plan, page));
    };
    let mut out = Vec::new();
    for task in tasks.iter().take(MAX_INPUT_ROWS + 1) {
        let state = task
            .get("state")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                task.get("status")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            });
        out.push(row_map(vec![
            ("taskId", opt_nonempty_str(task, "id", 96)),
            ("title", opt_str(task, "title", MAX_TEXT_CHARS)),
            ("state", opt_text_value(state, 32)),
            ("provider", opt_nonempty_str(task, "provider", 48)),
            ("role", opt_nonempty_str(task, "role", 48)),
            ("dependsCount", opt_array_len(task, "depends")),
            ("claimExpired", opt_bool(task, "claim_expired")),
            ("contractStale", opt_bool(task, "contract_stale")),
            ("verified", opt_bool(task, "completion_verified")),
            ("reason", opt_str(task, "reason", MAX_TEXT_CHARS)),
        ]));
    }
    Ok(paginate(DesktopView::Plan, page, out, true, false))
}

fn attention_fallback(source: &str, kind: &str) -> Value {
    match (source, kind) {
        ("message", "needs") => json!("작업방 질문"),
        ("message", _) => json!("작업방 메시지"),
        ("task", _) => json!("계획 작업 확인"),
        ("call", _) => json!("실행 상태 확인"),
        _ => Value::Null,
    }
}

fn attention_summary(source: &str, kind: &str, what: &str) -> Value {
    // Desktop Board rows reach the model. Room bodies belong to the private
    // conversation reader even when their text passes the outbound scrubber.
    if source == "message" {
        return attention_fallback(source, kind);
    }
    let candidate = what.trim();
    if !candidate.is_empty()
        && let Some(cleaned) = sanitize_text(candidate, MAX_TEXT_CHARS)
    {
        let trimmed = cleaned.trim();
        if !trimmed.is_empty() {
            return Value::String(trimmed.to_owned());
        }
    }
    attention_fallback(source, kind)
}

fn attention_response(
    report: &Value,
    read_markers: &BTreeMap<String, String>,
    page: usize,
) -> BoardResponse {
    let incomplete = report["capacity_stale"] == true || report["collection"]["ok"] != true;
    let items = crate::attention::items(report, read_markers);

    let mut out = Vec::new();
    for item in items.iter().take(MAX_INPUT_ROWS + 1) {
        let stamp = (!item.stamp.is_empty()).then_some(item.stamp.as_str());
        out.push(row_map(vec![
            ("kind", opt_text_value(Some(item.cls.as_str()), 32)),
            ("source", opt_text_value(Some(item.row_id.0.as_str()), 32)),
            ("who", opt_text_value(Some(item.who.as_str()), 96)),
            ("taskId", opt_text_value(item.task.as_deref(), 96)),
            (
                "summary",
                attention_summary(&item.row_id.0, &item.cls, &item.what),
            ),
            ("unread", json!(item.unread)),
            ("stamp", opt_text_value(stamp, 40)),
        ]));
    }
    paginate(DesktopView::Attention, page, out, true, incomplete)
}

fn project_attention(
    repo: &Path,
    room_id: Option<(&str, &str)>,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(snap) = read_snapshot(repo, room_id, services, stop)? else {
        return Ok(unavailable(DesktopView::Attention, page));
    };
    let report = &snap.report;
    let read_markers = crate::attention::load(repo, text(&report["room"], "id"));
    Ok(attention_response(report, &read_markers, page))
}

fn project_results(
    repo: &Path,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    checkpoint(stop)?;
    let offset = (page - 1).saturating_mul(MAX_PAGE_ROWS);
    let report = match results::results_page(
        repo,
        &results::ResultsRequest {
            offset,
            limit: MAX_PAGE_ROWS,
            ..Default::default()
        },
        services,
        stop,
    ) {
        Ok(v) => v,
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => return Ok(unavailable(DesktopView::Results, page)),
    };
    let Some(raw_rows) = report["rows"].as_array() else {
        return Ok(unavailable(DesktopView::Results, page));
    };
    let has_more = report["truncated"] == true && report["omitted"].is_null();
    let mut has_issue = false;
    let mut out = Vec::new();
    for row in raw_rows.iter().take(MAX_PAGE_ROWS) {
        let issue = opt_str(row, "issue", MAX_TEXT_CHARS);
        if !issue.is_null() || row["delivery_unknown"] == true {
            has_issue = true;
        }
        out.push(row_map(vec![
            ("taskId", opt_nonempty_str(row, "task_id", 96)),
            ("title", opt_str(row, "title", MAX_TEXT_CHARS)),
            ("state", opt_nonempty_str(row, "representative_state", 64)),
            ("outcome", opt_nonempty_str(row, "outcome", 32)),
            (
                "implement",
                opt_nonempty_str(&row["stages"]["implement"], "state", 32),
            ),
            (
                "verify",
                opt_nonempty_str(&row["stages"]["verify"], "state", 32),
            ),
            (
                "review",
                opt_nonempty_str(&row["stages"]["review"], "state", 32),
            ),
            ("admitted", opt_bool(row, "admitted")),
            ("evidenceCount", opt_array_len(row, "evidence")),
            ("delivery", opt_nonempty_str(row, "delivery", 32)),
            ("issue", issue),
        ]));
    }
    let next_page = (has_more && page < MAX_PAGE).then_some(page + 1);
    let total = (!has_more && (page == 1 || !out.is_empty())).then_some(offset + out.len());
    let incomplete = has_more || has_issue;
    Ok(BoardResponse {
        view: DesktopView::Results.as_str(),
        page,
        next_page,
        columns: columns_for(DesktopView::Results),
        rows: out,
        status: "ready",
        total,
        incomplete: incomplete.then_some(true),
    })
}

fn project_metrics(
    repo: &Path,
    room_id: Option<(&str, &str)>,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    let Some(snap) = read_snapshot(repo, room_id, services, stop)? else {
        return Ok(unavailable(DesktopView::Metrics, page));
    };
    checkpoint(stop)?;
    let artifacts = match commands::query(
        services,
        NativeCommand::Telemetry,
        repo,
        &["list", "--json"],
        stop,
    ) {
        Ok(v) if commands::artifact_rows(&v).is_ok() => v,
        Err(err) if err.code == 130 => return Err(err),
        _ => return Ok(unavailable(DesktopView::Metrics, page)),
    };
    let report = &snap.report;
    let attempts = rows(&report["attempts"]["recent"]).to_vec();
    let tasks: Vec<Value> = report["plan"]["tasks"]
        .as_object()
        .map(|v| v.values().cloned().collect())
        .unwrap_or_else(|| rows(&report["plan"]["tasks"]).to_vec());
    let aggregated = match metrics::aggregate(&metrics::MetricInputs {
        attempts: &attempts,
        artifacts: commands::artifact_rows(&artifacts)?,
        tasks: &tasks,
        since: None,
        now: oms_panel_core::timestamp() as f64,
        min_samples: 3,
        incomplete_reasons: vec![],
    }) {
        Ok(v) => v,
        Err(_) => return Ok(unavailable(DesktopView::Metrics, page)),
    };

    let imp = &aggregated["implementation"];
    let dur = &aggregated["duration"];
    let imp_samples = opt_u64(imp, "samples");
    let dur_samples = opt_u64(dur, "samples");
    let success_pct = round1(imp["success_rate"].as_f64().map(|r| r * 100.0));
    let mut out = vec![
        row_map(vec![
            ("category", json!("구현")),
            ("metric", json!("구현 호출")),
            ("value", opt_u64(imp, "calls")),
            ("unit", json!("건")),
            ("samples", imp_samples.clone()),
        ]),
        row_map(vec![
            ("category", json!("구현")),
            ("metric", json!("대상 작업")),
            ("value", opt_u64(imp, "tasks")),
            ("unit", json!("건")),
            ("samples", imp_samples.clone()),
        ]),
        row_map(vec![
            ("category", json!("구현")),
            ("metric", json!("완료 호출")),
            ("value", opt_u64(imp, "completed")),
            ("unit", json!("건")),
            ("samples", imp_samples.clone()),
        ]),
        row_map(vec![
            ("category", json!("구현")),
            ("metric", json!("실패 호출")),
            ("value", opt_u64(imp, "failed")),
            ("unit", json!("건")),
            ("samples", imp_samples.clone()),
        ]),
        row_map(vec![
            ("category", json!("구현")),
            ("metric", json!("재작업 라운드")),
            ("value", opt_u64(imp, "rework_rounds")),
            ("unit", json!("회")),
            ("samples", imp_samples.clone()),
        ]),
        row_map(vec![
            ("category", json!("구현")),
            ("metric", json!("성공률")),
            ("value", success_pct),
            ("unit", json!("%")),
            ("samples", imp_samples),
        ]),
        row_map(vec![
            ("category", json!("리뷰")),
            ("metric", json!("리뷰 기록 작업")),
            (
                "value",
                opt_u64(&aggregated["review"], "tasks_with_recorded_review"),
            ),
            ("unit", json!("건")),
            ("samples", Value::Null),
        ]),
        row_map(vec![
            ("category", json!("계획")),
            ("metric", json!("완료된 계획 작업")),
            ("value", opt_u64(&aggregated["plan"], "completed")),
            ("unit", json!("건")),
            ("samples", Value::Null),
        ]),
        row_map(vec![
            ("category", json!("소요 시간")),
            ("metric", json!("중앙값 (p50)")),
            ("value", round1(dur["p50"].as_f64())),
            ("unit", json!("초")),
            ("samples", dur_samples.clone()),
        ]),
        row_map(vec![
            ("category", json!("소요 시간")),
            ("metric", json!("90백분위 (p90)")),
            ("value", round1(dur["p90"].as_f64())),
            ("unit", json!("초")),
            ("samples", dur_samples),
        ]),
    ];

    if let Some(by_provider) = imp["by_provider"].as_object() {
        let mut sorted: BTreeMap<&str, u64> = BTreeMap::new();
        for (provider, count) in by_provider {
            if let Some(n) = count.as_u64() {
                sorted.insert(provider.as_str(), n);
            }
        }
        let total_calls = opt_u64(imp, "calls");
        for (provider, count) in sorted.into_iter().take(64) {
            out.push(row_map(vec![
                ("category", json!("공급자별 호출")),
                ("metric", opt_text_value(Some(provider), 48)),
                ("value", json!(count)),
                ("unit", json!("건")),
                ("samples", total_calls.clone()),
            ]));
        }
    }

    let incomplete = aggregated["incomplete"] == true
        || report["capacity_stale"] == true
        || report["collection"]["ok"] != true;
    Ok(paginate(DesktopView::Metrics, page, out, true, incomplete))
}

#[allow(clippy::too_many_arguments)]
fn project_routes(
    repo: &Path,
    execution: &Path,
    source: &Path,
    options: &Options,
    room_id: Option<&str>,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    checkpoint(stop)?;
    let report = match commands::routes_report(
        Some(repo),
        true,
        execution,
        source,
        options,
        room_id,
        services,
        stop,
    ) {
        Ok(v) => v,
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => return Ok(unavailable(DesktopView::Routes, page)),
    };
    let Some(chosen) = report["chosen"].as_object() else {
        return Ok(unavailable(DesktopView::Routes, page));
    };
    let writers = rows(&report["writers"]);
    let mut incomplete = false;
    let mut out = Vec::new();
    for (role, choice) in chosen.iter().take(MAX_INPUT_ROWS + 1) {
        let access = if writers.iter().any(|w| w.as_str() == Some(role.as_str())) {
            "write"
        } else {
            "read"
        };
        let evidence = &choice["evidence"];
        if evidence["events_source"].as_str() != Some("repository_events") {
            incomplete = true;
        }
        let tier = evidence
            .get("tier")
            .and_then(Value::as_str)
            .or_else(|| report["roles"][role]["tier"].as_str());
        if let Some(error) = choice["error"].as_str() {
            out.push(row_map(vec![
                ("role", opt_text_value(Some(role), 48)),
                ("access", json!(access)),
                ("tier", opt_text_value(tier, 32)),
                ("status", json!("unavailable")),
                ("provider", Value::Null),
                ("model", Value::Null),
                ("effort", Value::Null),
                ("inFlight", Value::Null),
                ("recentAttempts", Value::Null),
                ("issue", opt_text_value(Some(error), MAX_TEXT_CHARS)),
            ]));
        } else {
            out.push(row_map(vec![
                ("role", opt_text_value(Some(role), 48)),
                ("access", json!(access)),
                ("tier", opt_text_value(tier, 32)),
                ("status", json!("ready")),
                ("provider", opt_nonempty_str(choice, "provider", 48)),
                ("model", opt_nonempty_str(choice, "model", 96)),
                ("effort", opt_nonempty_str(choice, "effort", 32)),
                (
                    "requestedModel",
                    opt_nonempty_str(&choice["snapshot"], "requested_model", 96),
                ),
                (
                    "requestedEffort",
                    opt_nonempty_str(&choice["snapshot"], "requested_effort", 32),
                ),
                (
                    "requestedAccount",
                    account_alias(&choice["snapshot"], "requested_account"),
                ),
                (
                    "selectedAccount",
                    account_alias(&choice["snapshot"], "selected_account"),
                ),
                (
                    "versionEvidence",
                    opt_nonempty_str(&choice["snapshot"]["capabilities"], "harness_version", 48),
                ),
                (
                    "probeAge",
                    opt_u64(&choice["snapshot"]["capabilities"], "probe_age_seconds"),
                ),
                ("inFlight", opt_u64(&evidence["recent"], "in_flight")),
                (
                    "recentAttempts",
                    opt_u64(&evidence["recent"], "non_main_attempts"),
                ),
                ("issue", Value::Null),
            ]));
        }
    }
    Ok(paginate(DesktopView::Routes, page, out, true, incomplete))
}

fn project_workspaces(
    repo: &Path,
    page: usize,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BoardResponse> {
    checkpoint(stop)?;
    let active = match workspaces::list_workspaces(repo, false, services, stop) {
        Ok(list) => list,
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => return Ok(unavailable(DesktopView::Workspaces, page)),
    };
    checkpoint(stop)?;
    let (archived, archived_ok) = match workspaces::list_workspaces(repo, true, services, stop) {
        Ok(list) => (list, true),
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => (Vec::new(), false),
    };
    let mut out = Vec::new();
    for item in active
        .iter()
        .chain(archived.iter())
        .take(MAX_INPUT_ROWS + 1)
    {
        out.push(row_map(vec![
            ("id", opt_nonempty_str(item, "id", 64)),
            ("label", opt_str(item, "label", 120)),
            ("state", opt_nonempty_str(item, "state", 32)),
            ("archived", opt_bool(item, "archived")),
            ("canFocus", opt_bool(&item["capabilities"], "focus")),
        ]));
    }
    Ok(paginate(
        DesktopView::Workspaces,
        page,
        out,
        archived_ok,
        !archived_ok,
    ))
}

fn project_jobs(repo: &Path, page: usize, stop: &AtomicBool) -> Result<BoardResponse> {
    checkpoint(stop)?;
    let active = match jobs::list_public_jobs(repo, false) {
        Ok(list) => list,
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => return Ok(unavailable(DesktopView::Jobs, page)),
    };
    checkpoint(stop)?;
    let (archived, archived_ok) = match jobs::list_public_jobs(repo, true) {
        Ok(list) => (list, true),
        Err(err) if err.code == 130 => return Err(err),
        Err(_) => (Vec::new(), false),
    };
    let mut out = Vec::new();
    for item in active
        .iter()
        .chain(archived.iter())
        .take(MAX_INPUT_ROWS + 1)
    {
        out.push(row_map(vec![
            ("id", opt_nonempty_str(item, "id", 64)),
            ("label", opt_str(item, "label", 120)),
            ("state", opt_nonempty_str(item, "state", 32)),
            ("generation", opt_u64(item, "generation")),
            ("exitCode", opt_i64(item, "exit_code")),
            ("archived", opt_bool(item, "archived")),
            ("logTruncated", opt_bool(item, "log_truncated")),
        ]));
    }
    Ok(paginate(
        DesktopView::Jobs,
        page,
        out,
        archived_ok,
        !archived_ok,
    ))
}

fn valid_work_item_id(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn work_issue(task: &Value) -> Value {
    let Some((slug, number)) = task["issue"].as_str().and_then(|s| s.split_once('#')) else {
        return Value::Null;
    };
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if slug.len() <= 128
        && slug
            .split_once('/')
            .is_some_and(|(o, r)| segment(o) && segment(r))
        && number.len() <= 10
        && !number.starts_with('0')
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
    {
        task["issue"].clone()
    } else {
        Value::Null
    }
}

fn work_response(items: Vec<Value>, omitted: usize, availability: &str) -> Result<Output> {
    let payload = json!({"schema":1,"items":items,"omitted":omitted,"availability":availability});
    let bytes = crate::json(&payload)?;
    if oms_panel_core::outbound(&bytes).is_err() {
        return Ok((
            0,
            crate::json(&json!({"schema":1,"items":[],"omitted":0,"availability":"unavailable"}))?,
            vec![],
        ));
    }
    Ok((0, bytes, vec![]))
}

fn work_verification(task: &Value, result: &Value) -> Option<&'static str> {
    if task["completion_verified"] == false {
        return Some("unverified");
    }
    let v = &result["verification"];
    let verified = v["command_sha256"]
        .as_str()
        .is_some_and(|s| oms_command_plan::json::hex(s, 64));
    match (v["status"].as_str(), v["exit"].as_i64()) {
        (Some("passed"), Some(0)) if verified => Some("passed"),
        (Some("failed"), Some(n)) if n != 0 && verified => Some("failed"),
        (Some("not_run"), None) => Some("not_run"),
        _ if v.is_null()
            && task["state"] == "done"
            && task["completion_kind"].is_string()
            && task["completion_receipt"]["sha256"]
                .as_str()
                .is_some_and(|s| oms_command_plan::json::hex(s, 64)) =>
        {
            Some("verified")
        }
        _ => None,
    }
}

/// A Plan view's task hash and a result's stored binding use different canonical
/// encodings. Check both against the same disk generation before displaying a result.
pub(crate) fn work_items(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    checkpoint(stop)?;
    let report = match commands::query(
        services,
        NativeCommand::Plan,
        repo,
        &["list", "--json"],
        stop,
    ) {
        Ok(v) if v["schema"] == 1 && v["tasks"].is_array() => v,
        Err(e) if e.code == 130 => return Err(e),
        _ => return work_response(vec![], 0, "unavailable"),
    };
    let tasks = report["tasks"].as_array().expect("checked task array");
    if tasks.is_empty() {
        return work_response(vec![], 0, "ready");
    }
    let plan_id = report["plan_id"].as_str().filter(|s| {
        s.strip_prefix("plan_")
            .is_some_and(|id| oms_command_plan::json::hex(id, 32))
    });
    let store = Store::open(repo).ok();
    let raw_plan = store.as_ref().and_then(|store| {
        store
            .json(Path::new(".oms/plan/tasks.json"), 4 * 1024 * 1024, true)
            .ok()
            .flatten()
    });
    let mut partial = plan_id.is_none()
        || raw_plan.is_none()
        || raw_plan
            .as_ref()
            .is_some_and(|v| v["plan_id"] != report["plan_id"]);
    let indexed = results::index_rows(repo).unwrap_or_else(|_| {
        partial = true;
        vec![]
    });
    let mut latest = BTreeMap::new();
    for row in &indexed {
        if row["kind"] == "panel-result"
            && let Some(id) = row["task_id"].as_str()
        {
            latest.insert(id, row);
        }
    }
    let mut ids = BTreeMap::new();
    for task in tasks {
        if let Some(id) = task["id"].as_str().filter(|id| valid_work_item_id(id)) {
            *ids.entry(id).or_insert(0usize) += 1;
        }
    }
    let mut items = Vec::new();
    let mut size = 512;
    for task in tasks {
        checkpoint(stop)?;
        if items.len() == MAX_WORK_ITEMS {
            break;
        }
        let Some(id) = task["id"].as_str().filter(|id| ids.get(id) == Some(&1)) else {
            partial = true;
            continue;
        };
        let state = task["state"].as_str().filter(|s| {
            matches!(
                *s,
                "ready"
                    | "claimed"
                    | "running"
                    | "review"
                    | "landing"
                    | "blocked"
                    | "done"
                    | "cancelled"
            )
        });
        let title = opt_nonempty_str(task, "title", MAX_TEXT_CHARS);
        let owner = ["claimed_by_participant", "owner", "provider"]
            .iter()
            .find_map(|key| task[*key].as_str().filter(|s| valid_work_item_id(s)));
        let depends = task["depends"].as_array().filter(|deps| {
            deps.len() <= MAX_WORK_ITEM_DEPENDS
                && deps.iter().all(|d| {
                    d.as_str()
                        .is_some_and(|dep| dep != id && ids.get(dep) == Some(&1))
                })
        });
        let issue = work_issue(task);
        let task_hash = task["task_sha256"]
            .as_str()
            .filter(|s| oms_command_plan::json::hex(s, 64));
        let mut result = Value::Null;
        if let Some(row) = latest.get(id) {
            let recorded = raw_plan.as_ref().filter(|plan| {
                plan_id.is_some()
                    && plan["plan_id"] == report["plan_id"]
                    && plan["tasks"][id]["id"] == id
            });
            if let (Some(store), Some(plan), Some(hash)) = (&store, recorded, task_hash)
                && oms_command_plan::stored_task_revision(
                    &serde_json::to_vec(&plan["tasks"][id]).map_err(Error::new)?,
                )
                .is_ok_and(|v| v == hash)
            {
                match results::verified_bound_result(store, plan, row) {
                    Ok(Some(v)) => result = v,
                    Ok(None) => (),
                    Err(_) => partial = true,
                }
            } else {
                partial = true;
            }
        }
        let outcome = result["outcome"]
            .as_str()
            .filter(|s| matches!(*s, "accepted" | "completed" | "failed" | "rejected"));
        if !result.is_null() && outcome.is_none() {
            partial = true;
            result = Value::Null;
        }
        let verification = work_verification(task, &result);
        partial |= state.is_none()
            || title.is_null()
            || depends.is_none()
            || verification == Some("unverified")
            || (!result["verification"].is_null() && verification.is_none());
        let revision = plan_id.zip(task_hash).map(|(lineage, hash)| {
            oms_panel_core::digest(
                json!([lineage, id, hash, result["revision"], verification])
                    .to_string()
                    .as_bytes(),
            )
        });
        partial |= revision.is_none();
        let item = json!({"id":id,"title":title,"owner":owner,"state":state,"depends":depends,"issue":issue,
            "result":result["outcome"],"verification":verification,"revision":revision});
        let encoded = item.to_string();
        size += encoded.len() + json!(encoded).to_string().len() + 4;
        if size > WORK_ITEM_HOST_BUDGET {
            break;
        }
        items.push(item);
    }
    let omitted = tasks.len() - items.len();
    work_response(
        items,
        omitted,
        if partial || omitted > 0 {
            "partial"
        } else {
            "ready"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::account_binding::test_support::TempDir;
    use oms_panel_core::{CommandOutput, PeerExecutor, services::ExecutionRequest};
    use std::{ffi::OsString, process::Command};
    struct Fixture;
    impl PeerExecutor for Fixture {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            Err(Error::new("no external commands in desktop fixture"))
        }
    }
    impl Services for Fixture {
        fn command(
            &self,
            cmd: NativeCommand,
            _: &Path,
            _: &[OsString],
            _: &AtomicBool,
        ) -> Result<CommandOutput> {
            let data = match cmd {
                NativeCommand::Events => json!([]),
                NativeCommand::Telemetry => json!({"schema":1,"action":"list","rows":[]}),
                NativeCommand::Plan => {
                    json!({"tasks":(1..=23).map(|i|json!({"id":format!("task-{i}"),"title":format!("Task {i}"),"state":"ready"})).collect::<Vec<_>>()})
                }
                NativeCommand::ProviderCatalog => {
                    json!({"providers":[
                        {"provider":"codex","installed":true,"catalog_models":[],"routable":["sample-model"]},
                        {"provider":"claude","catalog_models":(1..=23).map(|i|format!("model-{i}")).collect::<Vec<_>>(),
                         "effort_values":["low","high"],"model_effort_scales":{"model-1":["high"]},"routable":["model-1"]}
                    ]})
                }
                _ => return Err(Error::new("fixture source unavailable")),
            };
            Ok(CommandOutput {
                code: 0,
                stdout: serde_json::to_vec(&data).unwrap(),
                stderr: vec![],
                output_complete: true,
                cleanup_complete: true,
                limited: false,
            })
        }
    }

    #[test]
    fn desktop_views_use_read_only_sources_and_keep_page_identity() {
        let tmp = TempDir::new("desktop-board");
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&tmp.0)
                .status()
                .unwrap()
                .success()
        );
        crate::orchestrator::account_binding::test_support::private_dir(&tmp.0.join(".oms"));
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap();
        let run = |view: &str, page: &str| {
            let args = commands::strings(&[
                "--desktop-view",
                view,
                "--json",
                "--page",
                page,
                "--repo",
                tmp.0.to_str().unwrap(),
            ]);
            let (code, out, err) = crate::run_with_services(
                "panel",
                &args,
                &tmp.0,
                source,
                &AtomicBool::new(false),
                &Fixture,
            );
            assert_eq!(code, 0, "{view}: {}", String::from_utf8_lossy(&err));
            serde_json::from_slice::<Value>(&out).unwrap()
        };
        let messages = json!({"room":{"id":"x","messages":[{"id":"private","sender":"lead","recipient":"main","message_kind":"question","pending_for":["main"],"text":"private-message-sentinel"}]}});
        let items = crate::attention::items(&messages, &Default::default());
        let msg = items.iter().find(|m| m.row_id.0 == "message").unwrap();
        assert!(msg.what.contains("private-message-sentinel"));
        assert_eq!(
            attention_summary(&msg.row_id.0, &msg.cls, &msg.what),
            "작업방 질문"
        );
        let (_, board, _) =
            finalize_response(attention_response(&messages, &BTreeMap::new(), 1)).unwrap();
        assert!(
            !String::from_utf8(board)
                .unwrap()
                .contains("private-message-sentinel")
        );
        assert_eq!(
            attention_summary(&msg.row_id.0, &msg.cls, ""),
            "작업방 질문"
        );
        for &view in VIEWS {
            let v = run(view, "1");
            assert_eq!(v["view"], view);
            assert_eq!(v["page"], 1);
            let rows = v["rows"].as_array().unwrap();
            assert!(rows.len() <= MAX_PAGE_ROWS);
            if matches!(view, "sessions" | "attention" | "past" | "debate" | "log") {
                assert_eq!(v["status"], "unavailable");
                assert!(rows.is_empty());
            }
        }
        let a = run("plan", "1");
        let b = run("plan", "2");
        assert_eq!(a["rows"].as_array().unwrap().len(), 20);
        assert_eq!(a["nextPage"], 2);
        assert_eq!(b["rows"].as_array().unwrap().len(), 3);
        assert!(b["nextPage"].is_null());
        assert_eq!(run("metrics", "1")["status"], "ready");
        let providers = run("providers", "1");
        assert!(providers["rows"][0]["defaultModel"].is_null());
        assert_eq!(providers["rows"][0]["modelCount"], 0);
        assert!(providers["rows"][0]["usable"].is_null());
        let connections = run("connections", "1");
        let harnesses = connections["rows"].as_array().unwrap();
        assert_eq!(harnesses.len(), 4);
        let pi = harnesses.iter().find(|r| r["harness"] == "pi").unwrap();
        assert_eq!(pi["transport"], "CodexGateway");
        assert_eq!(pi["owner"], "codex");
        assert_eq!(pi["resumePolicy"], "unsupported");
        assert!(pi["auth"].is_null());
        assert!(
            harnesses
                .iter()
                .any(|r| r["harness"] == "agy" && r["resumePolicy"] == "unsupported")
        );
        let catalog = run("modelcatalog", "1");
        assert_eq!(catalog["nextPage"], 2);
        assert_eq!(catalog["rows"][0]["efforts"], "high");
        assert_eq!(catalog["rows"][1]["routable"], false);
        assert!(catalog["rows"][1]["efforts"].is_null());
        assert_eq!(catalog["rows"][1]["providerEfforts"], "low, high");
        assert_eq!(run("modelcatalog", "2")["rows"][2]["model"], "model-23");
        let index = tmp.0.join(".oms/artifacts");
        crate::orchestrator::account_binding::test_support::private_dir(&index);
        let receipt = |source: &str| {
            json!({"kind":"ask","attempt_id":"a","requested_model":"request",
            "selected_model":"selected","selected_reasoning_effort":"high","served_model":"observed","model_attribution":source,"body":"private-text-sentinel"})
        };
        std::fs::write(
            index.join("index.jsonl"),
            format!("{}\n{}\n", receipt("transport"), receipt("ambiguous")),
        )
        .unwrap();
        let attribution = run("attribution", "1");
        assert!(attribution["rows"][0]["servedModel"].is_null());
        assert_eq!(attribution["rows"][1]["servedModel"], "observed");
        assert_eq!(attribution["rows"][1]["requestedModel"], "request");
        assert_eq!(attribution["rows"][1]["selectedEffort"], "high");
        assert!(account_alias(&json!({"a":"invalid/user"}), "a").is_null());
        assert_eq!(account_alias(&json!({"a":"team-a"}), "a"), "team-a");
        assert!(!attribution.to_string().contains("private-text-sentinel"));
        for args in [
            ["--desktop-view", "sessions", "--room", "other", "--json"],
            ["--desktop-view", "plan", "--page", "1001", "--json"],
        ] {
            assert!(Options::parse(&commands::strings(&args)).is_err());
        }
    }

    #[test]
    fn wide_multibyte_rows_fit_without_losing_a_row() {
        let rows = (0..20)
            .map(|_| {
                columns_for(DesktopView::Results)
                    .into_iter()
                    .map(|c| (c.key.to_string(), json!("🛰".repeat(160))))
                    .collect()
            })
            .collect();
        let response = paginate(DesktopView::Results, 1, rows, true, false);
        let (_, bytes, _) = finalize_response(response).unwrap();
        assert!(bytes.len() <= MAX_OUTPUT_BYTES);
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["rows"].as_array().unwrap().len(), 20);
        assert_eq!(report["incomplete"], true);
    }

    #[test]
    fn session_states_keep_exited_and_cancelled_distinct_from_done() {
        assert_eq!(normalize_session_state("done"), ("done", "none"));
        assert_eq!(normalize_session_state("exited"), ("exited", "none"));
        assert_eq!(normalize_session_state("cancelled"), ("cancelled", "none"));
        assert_eq!(normalize_session_state("failed"), ("failed", "none"));
        assert_eq!(normalize_session_state("timed_out"), ("failed", "none"));
        assert_eq!(normalize_session_state("abandoned"), ("failed", "none"));
        assert_eq!(
            normalize_session_state("waiting_approval"),
            ("waiting", "approval")
        );
        assert_eq!(
            normalize_session_state("waiting_input"),
            ("waiting", "input")
        );
        assert_eq!(normalize_session_state("review"), ("waiting", "review"));
        assert_eq!(normalize_session_state("blocked"), ("blocked", "owner"));
        assert_eq!(normalize_session_state("working"), ("working", "none"));
        assert_eq!(normalize_session_state("queued"), ("waiting", "none"));
        assert_eq!(normalize_session_state("other"), ("unknown", "unknown"));

        let report = json!({
            "collection": {"ok": true},
            "attempts": {
                "recent": [
                    {"attempt_id": "att-done", "provider": "codex", "role": "worker", "state": "done", "task_id": "t-done", "title": "Verified work"},
                    {"attempt_id": "att-exited", "provider": "claude", "role": "worker", "state": "exited", "task_id": "t-exited", "title": "Process exited"},
                    {"attempt_id": "att-cancelled", "provider": "agy", "role": "reviewer", "state": "cancelled", "task_id": "t-cancelled", "title": "Cancelled work"}
                ]
            }
        });
        let (_, bytes, _) = finalize_response(sessions_response(&report, 1)).unwrap();
        let projected: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(projected["status"], "ready");
        let state_col = projected["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["key"] == "state")
            .unwrap();
        assert_eq!(state_col["label"], "상태");
        let rows = projected["rows"].as_array().unwrap();
        let state_for = |id: &str| {
            rows.iter()
                .find(|r| r["id"] == id)
                .map(|r| {
                    (
                        r["state"].as_str().unwrap(),
                        r["waitingFor"].as_str().unwrap(),
                    )
                })
                .unwrap()
        };
        assert_eq!(state_for("att-done"), ("done", "none"));
        assert_eq!(state_for("att-exited"), ("exited", "none"));
        assert_eq!(state_for("att-cancelled"), ("cancelled", "none"));
    }

    #[test]
    fn attention_projects_sanitized_bounded_what_and_falls_back_when_empty() {
        let posix_path = concat!("/ho", "me/alice/project/src/secret.rs");
        let tilde_path = concat!("~/", "private/key.pem");
        let win_path = concat!("C:\\", "Users\\alice\\notes.txt");
        let secret_like = concat!("ghp_", "1234567890abcdef1234567890abcdef1234");

        let scrubbed = attention_summary(
            "call",
            "needs",
            &format!("waiting for approval: check {posix_path} and {tilde_path} and {win_path}"),
        );
        let scrubbed_str = scrubbed.as_str().unwrap();
        assert!(scrubbed_str.contains("waiting for approval"));
        assert!(scrubbed_str.contains("[path]"));
        assert!(!scrubbed_str.contains(posix_path));
        assert!(!scrubbed_str.contains(tilde_path));
        assert!(!scrubbed_str.contains(win_path));

        let redacted = attention_summary(
            "call",
            "failed",
            &format!("provider error with {secret_like}"),
        );
        let redacted_str = redacted.as_str().unwrap();
        assert!(!redacted_str.contains(secret_like));
        assert!(!redacted_str.is_empty());

        let long_what = format!("\u{1b}[31m{}\n\t", "가".repeat(MAX_TEXT_CHARS + 50));
        let bounded = attention_summary("call", "needs", &long_what);
        let bounded_str = bounded.as_str().unwrap();
        assert_eq!(bounded_str.chars().count(), MAX_TEXT_CHARS);
        assert!(!bounded_str.contains('\u{1b}'));

        assert_eq!(attention_summary("message", "needs", ""), "작업방 질문");
        assert_eq!(
            attention_summary("message", "mail", "   \n\t "),
            "작업방 메시지"
        );
        assert_eq!(
            attention_summary("task", "stale", "\u{1b}[31m\n"),
            "계획 작업 확인"
        );
        assert_eq!(attention_summary("call", "needs", ""), "실행 상태 확인");
        assert!(attention_summary("other", "info", "").is_null());

        let report = json!({
            "collection": {"ok": true},
            "room": {
                "id": "room-1",
                "messages": [
                    {
                        "id": "msg-raw-id-1",
                        "sender": "lead",
                        "recipient": "main",
                        "message_kind": "question",
                        "pending_for": ["main"],
                        "text": format!("Need decision on {posix_path}"),
                        "ts": "2026-11-24T12:00:00Z"
                    },
                    {
                        "id": "msg-raw-id-empty",
                        "sender": "lead",
                        "recipient": "main",
                        "message_kind": "question",
                        "pending_for": ["main"],
                        "text": "   ",
                        "ts": "2026-11-24T11:59:00Z"
                    }
                ]
            },
            "attempts": {
                "recent": [
                    {
                        "attempt_id": "att-raw-id-99",
                        "provider": "codex",
                        "role": "worker",
                        "state": "waiting_approval",
                        "task_id": "task-42",
                        "title": "Approve release schema",
                        "updated_at": "2026-11-24T12:01:00Z",
                        "refs": {"panel_room_participant": "participant-raw-99"}
                    },
                    {
                        "attempt_id": "att-done-1",
                        "provider": "claude",
                        "role": "worker",
                        "state": "done",
                        "task_id": "task-held",
                        "updated_at": "2026-11-24T11:00:00Z"
                    }
                ]
            },
            "plan": {
                "tasks": {
                    "task-held": {
                        "id": "task-held",
                        "status": "held",
                        "claimed_at": "2026-11-24T10:00:00Z"
                    }
                }
            }
        });
        let (_, bytes, _) =
            finalize_response(attention_response(&report, &BTreeMap::new(), 1)).unwrap();
        let projected: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(projected["status"], "ready");
        let rows = projected["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4);

        let call_row = rows.iter().find(|r| r["source"] == "call").unwrap();
        assert_eq!(
            call_row["summary"],
            "waiting for approval: Approve release schema"
        );
        assert!(
            !call_row["summary"]
                .as_str()
                .unwrap()
                .contains("att-raw-id-99")
        );
        assert!(
            !call_row["summary"]
                .as_str()
                .unwrap()
                .contains("participant-raw-99")
        );

        let task_row = rows.iter().find(|r| r["source"] == "task").unwrap();
        assert_eq!(task_row["summary"], "workers ended, still held");

        let msg_rows: Vec<_> = rows.iter().filter(|r| r["source"] == "message").collect();
        assert_eq!(msg_rows.len(), 2);
        assert!(msg_rows.iter().all(|row| row["summary"] == "작업방 질문"));
        for private in [
            "Need decision",
            posix_path,
            "msg-raw-id-1",
            "msg-raw-id-empty",
        ] {
            assert!(!projected.to_string().contains(private));
        }
    }

    #[test]
    fn attention_redacts_before_truncation_scrubs_delimited_paths_and_isolates_outbound_failures() {
        let posix_path = concat!("/ho", "me/alice/project/src/sample.rs");
        let unicode_path = concat!("/ho", "me/사용자/문서.txt");
        let tilde_path = concat!("~/", "private/key.pem");
        let win_path = concat!("C:\\", "Users\\alice\\notes.txt");
        let sample = concat!("ghp_", "1234567890abcdef1234567890abcdef1234");
        let cluster_text = format!("Verify {} before deploy", concat!("clus", "ter=hpc-prod"));
        let ssh_text = format!("Inspect {} key", concat!(".s", "sh/id_rsa"));

        for (wrapped, expected) in [
            (format!("check `{posix_path}` now"), "check `[path]` now"),
            (format!("check <{posix_path}> now"), "check <[path]> now"),
            (format!("check {{{posix_path}}} now"), "check {[path]} now"),
            (format!("check *{posix_path}* now"), "check *[path]* now"),
            (format!("check `{unicode_path}` now"), "check `[path]` now"),
            (
                format!("check file://{posix_path} now"),
                "check file://[path] now",
            ),
            (format!("check `{tilde_path}` now"), "check `[path]` now"),
            (format!("check `{win_path}` now"), "check `[path]` now"),
            (
                "check `.oms/plan/작업.json` now".into(),
                "check `[path]` now",
            ),
            (
                "check <../private/report.md> now".into(),
                "check <[path]> now",
            ),
            ("check ./private/notes now".into(), "check [path] now"),
        ] {
            let scrubbed = scrub_paths(&wrapped);
            assert_eq!(scrubbed, expected, "input: {wrapped}");
            assert!(!scrubbed.contains("alice"));
        }
        assert_eq!(
            scrub_paths(
                "keep crates/oms-command-panel/src/desktop_board.rs and https://example.com/a/b"
            ),
            "keep crates/oms-command-panel/src/desktop_board.rs and https://example.com/a/b"
        );

        let report = json!({
            "collection": {"ok": true},
            "room": {
                "id": "room-1",
                "messages": [
                    {
                        "id": "msg-straddling-sample",
                        "sender": "lead",
                        "message_kind": "question",
                        "pending_for": ["main"],
                        "text": format!("{} {}", "a".repeat(69), sample),
                        "ts": "2026-11-24T12:04:00Z"
                    },
                    {
                        "id": "msg-backtick-path",
                        "sender": "lead",
                        "message_kind": "question",
                        "pending_for": ["main"],
                        "text": format!("Check `{posix_path}` now"),
                        "ts": "2026-11-24T12:03:00Z"
                    },
                    {
                        "id": "msg-outbound-question",
                        "sender": "lead",
                        "message_kind": "question",
                        "pending_for": ["main"],
                        "text": cluster_text,
                        "ts": "2026-11-24T12:02:00Z"
                    },
                    {
                        "id": "msg-outbound-mail",
                        "sender": "lead",
                        "message_kind": "update",
                        "pending_for": ["main"],
                        "text": ssh_text,
                        "ts": "2026-11-24T12:01:00Z"
                    }
                ]
            },
            "attempts": {
                "recent": [
                    {
                        "attempt_id": "att-approval-1",
                        "provider": "codex",
                        "role": "worker",
                        "state": "waiting_approval",
                        "task_id": "task-approval",
                        "title": "Approve release schema",
                        "updated_at": "2026-11-24T12:05:00Z"
                    }
                ]
            }
        });

        let (_, bytes, _) =
            finalize_response(attention_response(&report, &BTreeMap::new(), 1)).unwrap();
        let raw_json = String::from_utf8(bytes.clone()).unwrap();
        let projected: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(projected["status"], "ready");
        assert!(!raw_json.contains("ghp_"));
        assert!(!raw_json.contains("123456"));
        assert!(!raw_json.contains("alice"));
        assert!(!raw_json.contains("hpc-prod"));
        assert!(!raw_json.contains("id_rsa"));

        let rows = projected["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5);

        let call_row = rows.iter().find(|r| r["source"] == "call").unwrap();
        assert_eq!(
            call_row["summary"],
            "waiting for approval: Approve release schema"
        );

        let by_stamp = |stamp: &str| rows.iter().find(|r| r["stamp"] == stamp).unwrap();
        assert_eq!(by_stamp("2026-11-24T12:04:00Z")["summary"], "작업방 질문");
        assert_eq!(by_stamp("2026-11-24T12:03:00Z")["summary"], "작업방 질문");
        assert_eq!(by_stamp("2026-11-24T12:02:00Z")["summary"], "작업방 질문");
        assert_eq!(by_stamp("2026-11-24T12:01:00Z")["summary"], "작업방 메시지");
    }

    #[test]
    fn work_items_projection_verifies_bindings_rejects_false_joins_and_enforces_budget() {
        struct PlanFixture {
            plan: Result<Value>,
        }
        impl PeerExecutor for PlanFixture {
            fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                Err(Error::new("no external commands"))
            }
        }
        impl Services for PlanFixture {
            fn command(
                &self,
                cmd: NativeCommand,
                _: &Path,
                _: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                match cmd {
                    NativeCommand::Plan => match &self.plan {
                        Ok(v) => Ok(CommandOutput {
                            code: 0,
                            stdout: serde_json::to_vec(v).unwrap(),
                            stderr: vec![],
                            output_complete: true,
                            cleanup_complete: true,
                            limited: false,
                        }),
                        Err(e) => Err(Error::new(&e.message)),
                    },
                    _ => Err(Error::new("unused")),
                }
            }
        }

        let tmp = TempDir::new("desktop-work-items");
        let repo = &tmp.0;
        let run = |plan: Result<Value>| {
            let (_, out, _) =
                work_items(repo, &PlanFixture { plan }, &AtomicBool::new(false)).unwrap();
            serde_json::from_slice::<Value>(&out).unwrap()
        };
        let read = |plan: &Value, tasks: Vec<Value>| {
            run(Ok(
                json!({"schema":1,"plan_id":plan["plan_id"],"tasks":tasks}),
            ))
        };
        assert_eq!(
            run(Err(Error::new("unavailable")))["availability"],
            "unavailable"
        );
        assert_eq!(read(&Value::Null, vec![])["availability"], "ready");
        assert!(!repo.join(".oms").exists());
        let source = json!({"id":"task-a","title":"한글 작업","state":"running","depends":[],"provider":"claude",
                          "verify":"private verifier","issue":"org/project#433"});
        let plan = json!({"plan_id":format!("plan_{}","a".repeat(32)),"tasks":{"task-a":source}});
        let report_row = |record: &Value| {
            let mut row = record.clone();
            row["task_sha256"] =
                oms_command_plan::stored_task_revision(&serde_json::to_vec(record).unwrap())
                    .unwrap()
                    .into();
            row
        };
        for d in [
            ".oms",
            ".oms/plan",
            ".oms/artifacts",
            ".oms/artifacts/panel-results",
        ] {
            crate::orchestrator::account_binding::test_support::private_dir(&repo.join(d));
        }
        std::fs::write(repo.join(".oms/plan/tasks.json"), plan.to_string()).unwrap();
        let query = || read(&plan, vec![report_row(&source)]);
        let unbound = query();
        let current = unbound["items"][0]["revision"].clone();
        assert_eq!(unbound["availability"], "ready");
        assert!(oms_command_plan::json::hex(current.as_str().unwrap(), 64));
        assert!(unbound["items"][0]["result"].is_null());
        assert!(!unbound.to_string().contains("private verifier"));
        let raw = oms_json::parse_strict(&serde_json::to_vec(&source).unwrap()).unwrap();
        let binding = json!({"plan_id":plan["plan_id"],"task_sha256":oms_json::projection_cache_digest(&raw).unwrap()});
        let mut result = json!({"kind":"oms-panel-result","schema":1,"task_id":"task-a","outcome":"completed",
             "revision":"1a".repeat(12),"summary":"private answer","summary_sha256":oms_panel_core::digest(b"private answer"),
             "verification":{"status":"passed","exit":0,"command_sha256":"f".repeat(64)},"plan_binding":binding});
        let write = |result: &Value| {
            let path = ".oms/artifacts/panel-results/result.json";
            let raw = result.to_string();
            std::fs::write(repo.join(path), &raw).unwrap();
            let row = json!({"kind":"panel-result","task_id":"task-a","artifact":path,"artifact_sha256":oms_panel_core::digest(raw.as_bytes())});
            std::fs::write(repo.join(".oms/artifacts/index.jsonl"), format!("{row}\n")).unwrap();
        };
        write(&result);
        let good = query();
        let row = &good["items"][0];
        assert_eq!(good["availability"], "ready");
        assert_eq!(row["result"], "completed");
        assert_eq!(row["verification"], "passed");
        assert_ne!(row["revision"], current);
        assert_eq!(row["depends"], json!([]));
        assert_eq!(row["issue"], "org/project#433");
        assert!(!good.to_string().contains("private answer"));
        let mut changed = source.clone();
        changed["title"] = json!("New generation");
        let changed_view = read(&plan, vec![report_row(&changed)]);
        let row = &changed_view["items"][0];
        assert_eq!(changed_view["availability"], "partial");
        assert!(row["result"].is_null());
        assert_ne!(row["revision"], current);
        for invalid in [
            Value::Null,
            json!({"plan_id":"other","task_sha256":binding["task_sha256"]}),
            json!({"plan_id":plan["plan_id"],"task_sha256":"b".repeat(64)}),
        ] {
            result["plan_binding"] = invalid;
            write(&result);
            assert!(query()["items"][0]["verification"].is_null());
        }
        result["plan_binding"] = binding;
        result["verification"]["exit"] = 2.into();
        write(&result);
        assert_eq!(query()["availability"], "partial");
        assert!(query()["items"][0]["verification"].is_null());
        std::fs::write(
            repo.join(".oms/artifacts/panel-results/result.json"),
            b"altered",
        )
        .unwrap();
        assert_eq!(query()["availability"], "partial");
        assert!(query()["items"][0]["result"].is_null());
        let mut changed = report_row(&source);
        changed["completion_verified"] = false.into();
        changed["depends"] = json!(["foreign"]);
        let bad = read(&plan, vec![changed]);
        assert_eq!(bad["items"][0]["verification"], "unverified");
        assert!(bad["items"][0]["depends"].is_null());
        let many = (0..90)
            .map(|i| {
                json!({"id":format!("t{i}"),"title":"줄🌐".repeat(80),"state":"ready",
            "depends":[],"task_sha256":"b".repeat(64)})
            })
            .collect();
        let bounded = read(&plan, many);
        let shown = bounded["items"].as_array().unwrap().len();
        assert!(shown > 0 && shown <= MAX_WORK_ITEMS);
        assert_eq!(shown + bounded["omitted"].as_u64().unwrap() as usize, 90);
        let encoded = bounded.to_string();
        assert!(encoded.len() + json!(encoded).to_string().len() + 1024 < 60_000);
    }
}
