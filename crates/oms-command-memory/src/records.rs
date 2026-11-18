use crate::*;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
pub(crate) const START: &str = "<!-- oms-memory";
pub(crate) const COMPLETE: &str = "<!-- oms-memory-link-complete -->";
pub(crate) fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub event_id: String,
    pub source: String,
    pub ordinal: usize,
    pub occurred_at: String,
    pub agent: String,
    pub kind: String,
    pub task_id: String,
    pub session_hash: String,
    pub git_sha: String,
    pub git_dirty: Option<i64>,
    pub git_state: String,
    pub body: String,
    pub rendered: String,
    pub metadata: BTreeMap<String, String>,
    pub start: usize,
    pub end: usize,
    pub raw_source_path: Option<std::ffi::OsString>,
}
impl Entry {
    pub(crate) fn source_path(&self) -> &std::ffi::OsStr {
        self.raw_source_path
            .as_deref()
            .unwrap_or_else(|| std::ffi::OsStr::new(self.field("source_path")))
    }
    pub(crate) fn field(&self, key: &str) -> &str {
        self.metadata.get(key).map(String::as_str).unwrap_or("")
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make(
        source: &str,
        ordinal: usize,
        occurred: &str,
        agent: &str,
        body: String,
        rendered: String,
        m: BTreeMap<String, String>,
        start: usize,
        end: usize,
    ) -> Self {
        let get = |key: &str| m.get(key).cloned().unwrap_or_default();
        let supplied = get("event_id");
        let event_id = if valid_id(&supplied) {
            supplied
        } else {
            format!(
                "legacy:{source}:{ordinal}:{}",
                &hash(rendered.as_bytes())[..16]
            )
        };
        let kind = get("kind");
        Self {
            event_id,
            source: source.into(),
            ordinal,
            occurred_at: occurred.into(),
            agent: agent.trim().into(),
            kind: if kind.is_empty() {
                if source == "pins" { "pin" } else { "note" }.into()
            } else {
                kind
            },
            task_id: get("task_id"),
            session_hash: get("session_hash"),
            git_sha: get("git_sha"),
            git_dirty: match get("git_dirty").as_str() {
                "0" => Some(0),
                "1" => Some(1),
                _ => None,
            },
            git_state: get("git_state"),
            body,
            rendered,
            metadata: m,
            start,
            end,
            raw_source_path: None,
        }
    }
    pub fn json(&self) -> Value {
        json!({"event_id":self.event_id,"source":self.source,"ordinal":self.ordinal,"occurred_at":self.occurred_at,
        "agent":self.agent,"kind":self.kind,"task_id":self.task_id,"session_hash":self.session_hash,"git_sha":self.git_sha,
        "git_dirty":self.git_dirty.map(|v|v!=0),"git_state":self.git_state,"body":self.body})
    }
}
fn header(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix("## ")?;
    let (when, agent) = s.split_once(' ')?;
    if when.is_empty() || when.chars().any(char::is_whitespace) {
        None
    } else {
        Some((when, agent))
    }
}
fn pin(s: &str) -> Option<(&str, &str, &str)> {
    let (when, rest) = s.strip_prefix("- ")?.split_once(" [")?;
    let (agent, body) = rest.split_once("] ")?;
    if when.is_empty() || when.chars().any(char::is_whitespace) || agent.is_empty() {
        None
    } else {
        Some((when, agent, body))
    }
}
/// Parses canonical logs, discarding interrupted link records. Byte spans retain
/// untouched source bytes for explicit redaction, including non-UTF-8 neighbors.
/// Timestamp headings delimit metadata-free legacy records; metadata-framed
/// bodies extend to the next reserved metadata marker, even across such headings.
pub fn parse(raw: &[u8], source: &str) -> Vec<Entry> {
    let ranges = line_ranges(raw);
    let decoded: Vec<_> = ranges
        .iter()
        .map(|(start, end)| String::from_utf8_lossy(&raw[*start..*end]))
        .collect();
    let lines: Vec<_> = decoded.iter().map(|s| s.as_ref()).collect();
    let mut offsets: Vec<_> = ranges.iter().map(|(start, _)| *start).collect();
    offsets.push(raw.len());
    let mut pending = BTreeMap::new();
    let mut block = None;
    let mut out = Vec::new();
    type PendingRecord = (usize, String, String, BTreeMap<String, String>, Vec<String>);
    let mut current: Option<PendingRecord> = None;
    fn finish(out: &mut Vec<Entry>, c: &mut Option<PendingRecord>, end: usize, offsets: &[usize]) {
        if let Some((start, when, agent, m, mut body)) = c.take() {
            if m.get("link_complete").map(String::as_str) == Some("required") {
                while body.last().is_some_and(|l| l.trim().is_empty()) {
                    body.pop();
                }
                if body.last().map(String::as_str) != Some(COMPLETE) {
                    return;
                }
                body.pop();
            }
            let body = body.join("\n").trim().to_owned();
            let rendered = format!(
                "## {when} {agent}{}",
                if body.is_empty() {
                    String::new()
                } else {
                    format!("\n\n{body}")
                }
            );
            out.push(Entry::make(
                "shared",
                out.len(),
                &when,
                &agent,
                body,
                rendered,
                m,
                offsets[start],
                offsets[end],
            ));
        }
    }
    let iso_header_re = regex::Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d").unwrap();
    for (i, line) in lines.iter().enumerate() {
        let l = *line;
        if l.trim() == START {
            if source == "shared" {
                finish(&mut out, &mut current, i, &offsets);
            }
            block = Some(BTreeMap::new());
            continue;
        }
        if let Some(ref mut m) = block {
            if l.trim() == "-->" {
                pending = block.take().unwrap();
            } else if let Some((k, v)) = l.split_once(':') {
                m.insert(k.trim().into(), v.trim().into());
            }
            continue;
        }
        if source == "pins" {
            if let Some((when, agent, body)) = pin(l) {
                out.push(Entry::make(
                    source,
                    out.len(),
                    when,
                    agent,
                    body.into(),
                    l.into(),
                    std::mem::take(&mut pending),
                    offsets[i],
                    offsets[i + 1],
                ));
            }
            continue;
        }
        if let Some((when, agent)) = header(l)
            && (current.is_none()
                || !pending.is_empty()
                || (iso_header_re.is_match(when)
                    && current.as_ref().is_some_and(|(_, _, _, m, _)| m.is_empty())))
        {
            finish(&mut out, &mut current, i, &offsets);
            current = Some((
                i,
                when.into(),
                agent.into(),
                std::mem::take(&mut pending),
                Vec::new(),
            ));
            continue;
        }
        if let Some((_, _, _, _, ref mut body)) = current {
            body.push(l.into());
        }
    }
    if source == "shared" {
        finish(&mut out, &mut current, lines.len(), &offsets);
    }
    for entry in &mut out {
        if !entry.field("source_path").is_empty()
            && let Some(start) = raw[..entry.start]
                .windows(START.len())
                .rposition(|w| w == START.as_bytes())
        {
            for (a, b) in line_ranges(&raw[start..entry.start]) {
                if let Some(value) = raw[start + a..start + b].strip_prefix(b"source_path:") {
                    entry.raw_source_path = Some(os::from_bytes(value.trim_ascii().to_vec()));
                    break;
                }
            }
        }
    }
    out
}
pub(crate) fn redacted(s: &str) -> bool {
    regex::Regex::new(
        r"^\[redacted \S+ by \S+: text removed with `oms agent-memory redact`; \d+ bytes\]$",
    )
    .unwrap()
    .is_match(s.trim())
}
pub(crate) fn states(notes: &[Entry], pins: &[Entry]) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    for e in notes {
        for (field, status) in [("supersedes", "superseded"), ("retires", "retired")] {
            if valid_id(e.field(field)) {
                out.insert(
                    e.field(field).into(),
                    json!({"status":status,"by":e.event_id}),
                );
            }
        }
    }
    for e in notes.iter().chain(pins) {
        if redacted(&e.body) {
            out.insert(e.event_id.clone(), json!({"status":"redacted","by":""}));
        }
    }
    out
}
pub(crate) fn failure_rows(raw: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|l| parse_json(l.as_bytes()))
        .filter(Value::is_object)
        .collect()
}
pub(crate) fn failures(raw: &[u8]) -> Vec<Entry> {
    let mut rows = Vec::new();
    let mut resolved = HashMap::new();
    let mut fixes = HashMap::new();
    let mut newest = HashMap::new();
    for row in failure_rows(raw) {
        let fp = text(&row, "fingerprint");
        if text(&row, "event") == "resolved" {
            if !fp.is_empty() {
                resolved.insert(fp.clone(), true);
                fixes.insert(fp, text(&row, "how"));
            }
            continue;
        }
        if !fp.is_empty() {
            resolved.insert(fp.clone(), false);
            fixes.remove(&fp);
            newest.insert(fp, rows.len());
        }
        rows.push(row);
    }
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let fp = text(row, "fingerprint");
        let mut body = [text(row, "summary"), text(row, "cmd")]
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");
        if let Some(exit) = row.get("exit").filter(|v| !v.is_null()) {
            let val = exit
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| exit.to_string());
            body = if body.is_empty() {
                format!("exit {val}")
            } else {
                format!("{body} (exit {val})")
            };
        }
        if body.is_empty() {
            continue;
        }
        let kind = text(row, "kind");
        if !kind.is_empty() && kind != "cmd" {
            body = format!("{kind} failure: {body}");
        }
        let done = resolved.get(&fp) == Some(&true);
        if done {
            body = format!("resolved — {body}");
        }
        let addition = if done && newest.get(&fp) == Some(&i) {
            fixes.get(&fp).cloned().unwrap_or_default()
        } else if !done {
            text(row, "next")
        } else {
            String::new()
        };
        let collapsed = flat(&addition);
        if !addition.is_empty()
            && addition.len() <= 8192
            && collapsed.len() <= 8192
            && store::clean(&addition, false)
            && store::clean(&collapsed, false)
        {
            let bounded = if collapsed.chars().count() > 240 {
                format!("{}...", clip(&collapsed, 237))
            } else {
                collapsed
            };
            body.push_str(&format!(
                " | {}: {bounded}",
                if done { "fixed" } else { "next" }
            ));
        }
        let mut m = BTreeMap::new();
        m.insert("event_id".into(), format!("fail:{fp}:{}", out.len()));
        m.insert(
            "kind".into(),
            if done { "failure-resolved" } else { "failure" }.into(),
        );
        let task = text(row, "task");
        m.insert(
            "task_id".into(),
            if task.is_empty() {
                text(row, "task_id")
            } else {
                task
            },
        );
        m.insert("git_state".into(), text(row, "state_fingerprint"));
        let when = text(row, "ts");
        let rendered = format!("## {} {}\n\n{body}", when, text(row, "agent"));
        let when = if when.is_empty() {
            text(row, "timestamp")
        } else {
            when
        };
        out.push(Entry::make(
            "failures",
            out.len(),
            &when,
            &text(row, "agent"),
            body,
            rendered,
            m,
            0,
            0,
        ));
    }
    out
}
pub(crate) fn scope_match(glob: &str, target: &str) -> bool {
    let mut pattern = String::new();
    let mut rest = glob;
    while !rest.is_empty() {
        if rest.starts_with("**/") {
            pattern.push_str("(?:.*/)?");
            rest = &rest[3..];
        } else if rest.starts_with("**") {
            pattern.push_str(".*");
            rest = &rest[2..];
        } else {
            let c = rest.chars().next().unwrap();
            pattern.push_str(&match c {
                '*' => "[^/]*".into(),
                '?' => "[^/]".into(),
                _ => regex::escape(&c.to_string()),
            });
            rest = &rest[c.len_utf8()..];
        }
    }
    regex::Regex::new(&format!("^{pattern}(?:/.*)?$")).is_ok_and(|r| r.is_match(target))
}

/// Byte ranges for Python str.splitlines semantics, retaining original byte
/// offsets so a redaction never re-encodes neighboring invalid UTF-8 data.
pub(crate) fn line_ranges(raw: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < raw.len() {
        let width = if raw[at] == b'\r' {
            if raw.get(at + 1) == Some(&b'\n') {
                2
            } else {
                1
            }
        } else if matches!(raw[at], b'\n' | 0x0b | 0x0c | 0x1c | 0x1d | 0x1e) {
            1
        } else if raw[at..].starts_with(&[0xc2, 0x85]) {
            2
        } else if raw[at..].starts_with(&[0xe2, 0x80, 0xa8])
            || raw[at..].starts_with(&[0xe2, 0x80, 0xa9])
        {
            3
        } else {
            0
        };
        if width > 0 {
            out.push((start, at));
            at += width;
            start = at;
        } else {
            at += 1;
        }
    }
    if start < raw.len() {
        out.push((start, raw.len()));
    }
    out
}
