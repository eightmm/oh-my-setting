use super::*;
use crate::{
    Options,
    value::{self, V, display, equal, obj},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write;

struct Error {
    code: i32,
    message: String,
}
impl From<StoreError> for Error {
    fn from(e: StoreError) -> Self {
        Self {
            code: e.exit_code(),
            message: e.to_string(),
        }
    }
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self { code: 2, message }
    }
}
type Result<T> = std::result::Result<T, Error>;
fn outcome_error(message: impl Into<String>) -> Error {
    Error {
        code: 1,
        message: message.into(),
    }
}
fn invalid(message: impl Into<String>) -> Error {
    message.into().into()
}
fn output(result: Result<(i32, Vec<u8>)>) -> (i32, Vec<u8>, Vec<u8>) {
    match result {
        Ok((code, bytes)) => (code, bytes, Vec::new()),
        Err(e) => (
            e.code,
            Vec::new(),
            format!("error: {}\n", e.message).into_bytes(),
        ),
    }
}
pub(crate) fn run(o: &Options, caller: &Path, interrupted: &AtomicBool) -> (i32, Vec<u8>, Vec<u8>) {
    output(execute(o, caller, interrupted))
}
fn execute(o: &Options, caller: &Path, interrupted: &AtomicBool) -> Result<(i32, Vec<u8>)> {
    let (repo, logical) = crate::repository(o, caller)?;
    let root = Root::open(&repo).map_err(StoreError::from)?;
    let supplied = o
        .index
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new(DEFAULT_INDEX));
    let target = canonical_index(&logical, Some(supplied), false, false)?;
    if o.action == "salvage" {
        let r = salvage_index(&repo, Some(&target), o.apply, interrupted)?;
        let text = if r.healthy {
            "artifact-index: already healthy; salvage no-op\n".into()
        } else {
            format!(
                "artifact-index: salvage {}; recovered={} dropped={} compacted={} quarantine={} sha256={}{}\n",
                if r.applied { "applied" } else { "plan only" },
                r.recovered_rows,
                r.dropped_rows,
                r.compacted_rows,
                r.quarantine.unwrap_or_default(),
                r.digest.unwrap_or_default(),
                if r.applied {
                    ""
                } else {
                    "; rerun with --apply"
                }
            )
        };
        return Ok((0, text.into_bytes()));
    }
    let writer = [
        "resolve",
        "resolve-superseded",
        "resolve-recovered",
        "migrate",
        "prune",
    ]
    .contains(&o.action.as_str());
    let guard = if writer {
        Some(acquire(&target, interrupted)?)
    } else {
        None
    };
    let snapshot = root
        .read_no_follow(
            &target,
            ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_INPUT_BYTES,
            },
        )
        .map_err(StoreError::from)?;
    if snapshot.is_empty() {
        if let Some(label) = o.action.strip_prefix("resolve-") {
            return Ok((
                0,
                format!(
                    "artifact-index: 0 {label} failure(s) {}\n",
                    if o.dry {
                        "would be resolved (dry run)"
                    } else {
                        "resolved"
                    }
                )
                .into_bytes(),
            ));
        }
        if o.json {
            return Ok((
                0,
                value::encode(&obj!("schema":1,"action":o.action.clone(),"rows":Vec::<V>::new()))?,
            ));
        }
        return Err(invalid(format!(
            "no artifact index at {}",
            target.display()
        )));
    }
    if o.action == "validate" {
        return validate(&root, &snapshot);
    }
    // Keep the canonical public diagnostics used by the native telemetry view.
    let rows = crate::parse_snapshot(&snapshot, interrupted)?;
    let text = match o.action.as_str() {
        "resolve" | "resolve-superseded" | "resolve-recovered" => {
            resolve(&root, &target, &snapshot, &rows, o, interrupted)?
        }
        "migrate" => migrate(&root, &target, &snapshot, &rows, interrupted)?,
        "prune" => {
            let (text, kept) = prune(&root, &target, &snapshot, &rows, o, interrupted)?;
            drop(guard);
            if o.files && !o.stale {
                let grace = std::env::var("OMS_ARTIFACT_ORPHAN_GRACE")
                    .unwrap_or_else(|_| "86400".into())
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| {
                        invalid("OMS_ARTIFACT_ORPHAN_GRACE must be a non-negative integer")
                    })?;
                // A dry run's reference set is hypothetical; deletion still validates
                // the actual snapshot before accepting that reference set.
                let r = if o.dry {
                    recovery::plan_orphans(
                        &repo,
                        Some(&target),
                        &snapshot,
                        &kept,
                        grace,
                        interrupted,
                    )?
                } else {
                    delete_orphans(&repo, Some(&target), &kept, false, grace, interrupted)?
                };
                let mut text = text;
                for p in &r.paths {
                    writeln!(
                        text,
                        "{}: {p}",
                        if o.dry { "would delete" } else { "deleted" }
                    )
                    .unwrap();
                }
                writeln!(
                    text,
                    "artifact-index: {} {} orphan file(s)",
                    if o.dry { "would delete" } else { "deleted" },
                    r.paths.len()
                )
                .unwrap();
                if r.fresh > 0 {
                    writeln!(text, "artifact-index: kept {} recent unindexed file(s) inside the {}s grace window (in-flight writers; they age out or get indexed)", r.fresh, grace).unwrap();
                }
                return Ok((0, text.into_bytes()));
            }
            return Ok((0, text.into_bytes()));
        }
        _ => return Ok((0, view(&rows, o)?)),
    };
    Ok((0, text.into_bytes()))
}

struct Joins<'a> {
    rows: &'a [V],
    by_id: BTreeMap<&'a str, &'a V>,
    counts: BTreeMap<String, usize>,
    resolved: BTreeSet<Vec<u8>>,
    superseded: BTreeMap<String, String>,
    recovered: BTreeMap<String, String>,
    claimed: BTreeSet<String>,
}
fn text<'a>(r: &'a V, key: &str) -> &'a str {
    r[key].text().unwrap_or("")
}
fn schema(r: &V) -> bool {
    equal(&r["schema"], &V::from(1))
}
impl<'a> Joins<'a> {
    fn new(rows: &'a [V]) -> Self {
        let mut j = Self {
            rows,
            by_id: BTreeMap::new(),
            counts: BTreeMap::new(),
            resolved: crate::report::resolved(rows),
            superseded: BTreeMap::new(),
            recovered: BTreeMap::new(),
            claimed: BTreeSet::new(),
        };
        for r in rows {
            j.by_id.entry(text(r, "event_id")).or_insert(r);
            if let Some(id) = r["event_id"].text() {
                *j.counts.entry(id.into()).or_default() += 1;
            }
            if text(r, "kind") == "artifact-resolution" {
                for key in ["resolves_event_id", "parent_event_id"] {
                    if let Some(id) = r[key].text().filter(|s| !s.is_empty()) {
                        j.claimed.insert(id.into());
                    }
                }
            }
        }
        let mut successes = BTreeMap::new();
        let mut recoveries = BTreeMap::new();
        for r in rows.iter().rev() {
            let id = text(r, "event_id");
            let kind = text(r, "kind");
            if let Some(digest) = r["patch_sha256"]
                .text()
                .filter(|s| valid_digest(s) && *s != crate::report::EMPTY_DIGEST)
            {
                if ["patch-admit", "patch-land"].contains(&kind) && r["exit"].int() == Some("0") {
                    if !id.is_empty() {
                        successes.insert(digest, id);
                    }
                } else if ["patch-admit", "delegate"].contains(&kind)
                    && j.status(r) == "unresolved"
                    && !id.is_empty()
                    && let Some(winner) = successes.get(digest)
                {
                    j.superseded.insert(id.into(), (*winner).into());
                }
            }
            if !j.complete(r) {
                continue;
            }
            let key = (text(r, "provider"), kind);
            if r["exit"].int() == Some("0") {
                recoveries.insert(key, id);
            } else if r["exit"].int() == Some("124")
                && j.status(r) == "unresolved"
                && !j.claimed.contains(id)
                && let Some(winner) = recoveries.get(&key)
            {
                j.recovered.insert(id.into(), (*winner).into());
            }
        }
        j
    }
    fn complete(&self, r: &V) -> bool {
        let id = text(r, "event_id");
        schema(r)
            && !id.is_empty()
            && self.counts.get(id) == Some(&1)
            && ["operation_id", "artifact_id", "ts", "kind"]
                .iter()
                .all(|k| !matches!(r[k], V::Null) && r[k] != V::from(""))
            && r["provider"].bytes().is_some()
            && r["exit"].int().is_some()
    }
    fn status(&self, r: &V) -> &'static str {
        if text(r, "kind") == "artifact-resolution" {
            return "resolution";
        }
        match r["exit"].int() {
            Some("0") => "success",
            Some(_)
                if r["event_id"]
                    .bytes()
                    .is_some_and(|id| self.resolved.contains(id)) =>
            {
                "resolved"
            }
            _ => "unresolved",
        }
    }
    fn row(&self, r: &V) -> V {
        let mut out = r.clone();
        out["status"] = self.status(r).into();
        let id = text(r, "event_id");
        if let Some(w) = self.superseded.get(id) {
            out["superseded_by"] = w.clone().into();
        }
        if let Some(w) = self.recovered.get(id) {
            out["recovered_by"] = w.clone().into();
        }
        out
    }
    fn format(&self, r: &V) -> String {
        let get = |k| r.get(k).map(shown).unwrap_or_default();
        let mut parts = vec![
            get("ts"),
            get("kind"),
            get("provider"),
            format!("exit={}", get("exit")),
        ];
        if r.get("verify_exit").is_some() {
            parts.push(format!("verify={}", get("verify_exit")));
        }
        for (key, label) in [
            ("task_id", "task"),
            ("base_sha", "base"),
            ("artifact", "artifact"),
            ("patch", "patch"),
            ("task_goal", "goal"),
            ("event_id", "event"),
        ] {
            if r[key].truth() {
                parts.push(format!(
                    "{label}={}",
                    if key == "task_goal" {
                        get(key).chars().take(80).collect()
                    } else {
                        get(key)
                    }
                ));
            }
        }
        parts.push(format!("status={}", self.status(r)));
        parts.join("  ")
    }
    fn triage(&self, r: &V) -> String {
        let id = text(r, "event_id");
        if id.is_empty() || !schema(r) {
            return "  next: oms artifact-index migrate  (row is not a resolvable schema-1 event)"
                .into();
        }
        if let Some(w) = self.recovered.get(id) {
            let mut s = format!(
                "  recovered-by: {w}  next: oms artifact-index resolve --event-id {id} --reason \"recovered by {w}\""
            );
            if let Some(w) = self.superseded.get(id) {
                write!(s, "\n  superseded-by: {w}").unwrap();
            }
            return s;
        }
        if let Some(w) = self.superseded.get(id) {
            return format!(
                "  superseded-by: {w}  next: oms artifact-index resolve --event-id {id} --reason \"superseded by {w}\""
            );
        }
        format!(
            "  next: oms artifact-index resolve --event-id {id} --reason \"<why this failure is no longer open>\""
        )
    }
}
fn shown(v: &V) -> String {
    match v {
        V::Bool(true) => "True".into(),
        V::Bool(false) => "False".into(),
        V::Array(_) | V::Object(_) => repr(v),
        _ => display(v),
    }
}
fn repr(v: &V) -> String {
    match v {
        V::Bool(true) => "True".into(),
        V::Bool(false) => "False".into(),
        V::Str(raw) => {
            let s = String::from_utf8_lossy(raw);
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::new();
            out.push(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        V::Array(values) => format!(
            "[{}]",
            values.iter().map(repr).collect::<Vec<_>>().join(", ")
        ),
        V::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(k, v)| format!("{}: {}", repr(&V::Str(k.clone())), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => display(v),
    }
}
fn valid_digest(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn view(rows: &[V], o: &Options) -> Result<Vec<u8>> {
    let joins = Joins::new(rows);
    if o.action == "latest-run" {
        return Ok(latest_run(&joins).into_bytes());
    }
    let selected: Vec<_> = rows
        .iter()
        .filter(|r| match o.action.as_str() {
            "failures" => ["resolved", "unresolved"].contains(&joins.status(r)),
            "unresolved" => joins.status(r) == "unresolved",
            _ => true,
        })
        .collect();
    let limit = if o.action == "latest" { 1 } else { o.limit };
    let selected = &selected[selected.len().saturating_sub(limit)..];
    if o.json {
        return Ok(value::encode(
            &obj!("schema":1,"action":o.action.clone(),"rows":selected.iter().map(|r| joins.row(r)).collect::<Vec<_>>()),
        )?);
    }
    let mut out = String::new();
    for r in selected {
        writeln!(out, "{}", joins.format(r)).unwrap();
        if o.action == "unresolved" {
            writeln!(out, "{}", joins.triage(r)).unwrap();
        }
    }
    Ok(out.into_bytes())
}
fn run_info(r: &V) -> Option<(String, usize)> {
    let artifact = Path::new(text(r, "artifact"))
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let pattern = regex::Regex::new(r".*-([0-9]{8}T[0-9]{6}Z-[0-9]+)(?:-r([0-9]+))?\.md$").unwrap();
    let c = pattern.captures(artifact);
    let round = c
        .as_ref()
        .and_then(|c| c.get(2))
        .and_then(|v| v.as_str().parse().ok())
        .unwrap_or(0);
    let operation = text(r, "operation_id");
    if !operation.is_empty() {
        return Some((operation.into(), round));
    }
    c.map(|c| (c[1].into(), round))
}
fn latest_run(j: &Joins<'_>) -> String {
    struct Group<'a> {
        id: Option<String>,
        sort: (String, usize),
        rows: Vec<(usize, usize, &'a V)>,
    }
    let mut groups: Vec<Group<'_>> = Vec::new();
    for (order, r) in j
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| text(r, "kind") != "artifact-resolution")
    {
        let info = run_info(r);
        let id = info.as_ref().map(|v| v.0.clone());
        let round = info.as_ref().map_or(0, |v| v.1);
        let stamp = id
            .as_ref()
            .and_then(|s| s.split('-').next())
            .and_then(|s| {
                let (date, time) = s.split_once('T')?;
                let time = time.trim_end_matches('Z');
                let part = |s: &str, start, end| {
                    s.chars().skip(start).take(end - start).collect::<String>()
                };
                Some(format!(
                    "{}-{}-{}T{}:{}:{}Z",
                    part(date, 0, 4),
                    part(date, 4, 6),
                    part(date, 6, 8),
                    part(time, 0, 2),
                    part(time, 2, 4),
                    part(time, 4, 6)
                ))
            })
            .unwrap_or_else(|| display(&r["ts"]));
        let found = id
            .as_ref()
            .and_then(|id| groups.iter().position(|g| g.id.as_ref() == Some(id)));
        let position = found.unwrap_or_else(|| {
            groups.push(Group {
                id,
                sort: (stamp.clone(), order),
                rows: Vec::new(),
            });
            groups.len() - 1
        });
        let g = &mut groups[position];
        g.sort = g.sort.clone().max((stamp, order));
        g.rows.push((round, order, r));
    }
    let Some(g) = groups.into_iter().max_by(|a, b| a.sort.cmp(&b.sort)) else {
        return String::new();
    };
    let mut out = String::new();
    if let Some(id) = g.id {
        let mut selected = BTreeMap::new();
        let mut kinds = BTreeSet::new();
        let mut round = 0;
        for (n, order, r) in g.rows {
            round = round.max(n);
            if !text(r, "kind").is_empty() {
                kinds.insert(text(r, "kind"));
            }
            let key = (text(r, "kind"), text(r, "provider"));
            let old = selected.entry(key).or_insert((n, order, r));
            if (n, order) >= (old.0, old.1) {
                *old = (n, order, r);
            }
        }
        writeln!(
            out,
            "run: {id}  kind={}  debate_round={round}",
            kinds.into_iter().collect::<Vec<_>>().join(",")
        )
        .unwrap();
        for (_, (_, _, r)) in selected {
            writeln!(out, "{}", j.format(r)).unwrap();
        }
    } else {
        for (_, _, r) in g.rows {
            writeln!(out, "{}", j.format(r)).unwrap();
        }
    }
    out
}
fn resolve(
    root: &Root,
    target: &Path,
    snapshot: &[u8],
    rows: &[V],
    o: &Options,
    interrupted: &AtomicBool,
) -> Result<String> {
    let j = Joins::new(rows);
    let mut out = String::new();
    let mut pending = Vec::new();
    let label = o.action.strip_prefix("resolve-");
    if label.is_none() {
        if o.reason.chars().count() > 200 {
            return Err(invalid("--reason must be at most 200 characters"));
        }
        if sensitive(&o.reason) {
            return Err(invalid("--reason contains sensitive-looking content"));
        }
        if let Some((id, _)) = j.counts.iter().find(|(_, n)| **n > 1) {
            return Err(outcome_error(format!("duplicate artifact event id: {id}")));
        }
        let mut ids = BTreeSet::new();
        let mut targets = Vec::new();
        for id in &o.events {
            if !ids.insert(id) {
                continue;
            }
            let r = j
                .by_id
                .get(id.as_str())
                .copied()
                .ok_or_else(|| outcome_error(format!("unknown artifact event: {id}")))?;
            if !j.complete(r) {
                return Err(outcome_error(format!(
                    "resolve target {id} must be a complete schema-1 event; run migrate first"
                )));
            }
            if text(r, "kind") == "artifact-resolution" {
                return Err(outcome_error(format!(
                    "a resolution event cannot be resolved: {id}"
                )));
            }
            if r["exit"].int() == Some("0") {
                return Err(outcome_error(format!(
                    "only non-zero-exit artifact events can be resolved: {id}"
                )));
            }
            targets.push((id, r));
        }
        for (id, r) in targets {
            if j.claimed.contains(id) {
                writeln!(out, "artifact-index: already resolved {id}").unwrap();
            } else {
                pending.push((id.as_str(), r, String::new(), o.reason.clone()));
            }
        }
    } else {
        for r in rows {
            let id = text(r, "event_id");
            let map = if label == Some("superseded") {
                &j.superseded
            } else {
                &j.recovered
            };
            let Some(winner) = map.get(id) else {
                continue;
            };
            if label == Some("superseded")
                && (!j.complete(r)
                    || j.claimed.contains(id)
                    || j.counts.get(winner) != Some(&1)
                    || !j
                        .by_id
                        .get(winner.as_str())
                        .is_some_and(|candidate| candidate["provider"] == r["provider"]))
            {
                continue;
            }
            let reason = if label == Some("superseded") {
                format!("superseded by {winner} (same patch bytes later succeeded)")
            } else {
                format!("recovered {id} by {winner} (same provider and kind later succeeded)")
            };
            pending.push((id, r, winner.clone(), reason));
        }
    }
    if !o.dry && !pending.is_empty() {
        let now = producer::now_text()?;
        let writer = std::env::var("OMS_AGENT").unwrap_or_else(|_| "unknown".into());
        let mut encoded = Vec::new();
        let mut ids = Vec::new();
        for (id, r, _, reason) in &pending {
            let event = format!("evt_resolve_{}", producer::random_hex::<16>()?);
            let mut row = obj!("schema":1,"event_id":event.clone(),"operation_id":r["operation_id"].clone(),"artifact_id":r["artifact_id"].clone(),"ts":now.clone(),"kind":"artifact-resolution","provider":writer.clone(),"exit":0,"parent_event_id":*id,"resolves_event_id":*id,"resolution":"resolved");
            if !reason.is_empty() {
                row["reason"] = reason.clone().into();
            }
            encoded.push(value::encode(&row)?);
            ids.push(event);
        }
        let refs: Vec<_> = encoded.iter().map(Vec::as_slice).collect();
        let (keep, high) = retention_limits(None, None)?;
        append_locked(
            root,
            target,
            &refs,
            keep,
            high,
            Some(snapshot),
            &ids,
            interrupted,
        )?;
    }
    for (id, _, winner, _) in &pending {
        write!(
            out,
            "artifact-index: {} {id}",
            if o.dry { "would resolve" } else { "resolved" }
        )
        .unwrap();
        if let Some(label) = label {
            write!(out, " ({label} by {winner})").unwrap();
        }
        out.push('\n');
    }
    if let Some(label) = label {
        writeln!(
            out,
            "artifact-index: {} {label} failure(s) {}",
            pending.len(),
            if o.dry {
                "would be resolved (dry run)"
            } else {
                "resolved"
            }
        )
        .unwrap();
    }
    Ok(out)
}
fn validate(root: &Root, raw: &[u8]) -> Result<(i32, Vec<u8>)> {
    let mut out = String::new();
    let mut errors = 0;
    let mut warnings = 0;
    let mut total = 0;
    let mut ids = BTreeSet::new();
    let mut parsed = Vec::new();
    let required = [
        "artifact_id",
        "event_id",
        "exit",
        "kind",
        "operation_id",
        "provider",
        "schema",
        "ts",
    ];
    for (pos, line) in raw.split_inclusive(|b| *b == b'\n').enumerate() {
        let n = pos + 1;
        let mut bad = |message: String| {
            writeln!(out, "BAD line {n}: {message}").unwrap();
            errors += 1;
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            bad("blank JSONL row".into());
            continue;
        }
        total += 1;
        if !line.ends_with(b"\n") {
            bad("partial final JSONL row".into());
        }
        let mut complete = line.to_vec();
        if !complete.ends_with(b"\n") {
            complete.push(b'\n');
        }
        let r = match crate::parse_row(&complete, n) {
            Ok(r) => r,
            Err(message) => {
                bad(message
                    .strip_prefix("artifact index: ")
                    .unwrap_or(&message)
                    .into());
                continue;
            }
        };
        let missing: Vec<_> = required
            .iter()
            .filter(|k| r.get(k).is_none())
            .copied()
            .collect();
        if !missing.is_empty() {
            bad(format!("missing {}", missing.join(",")));
        }
        if !schema(&r) {
            bad(format!("schema={}, expected 1", repr(&r["schema"])));
        }
        if r["exit"].int().is_none() {
            bad("exit must be a non-negative integer".into());
        }
        if r["executor_id"] != V::Null
            && !r["executor_id"].text().is_some_and(|s| {
                !s.is_empty()
                    && s.len() <= 160
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            bad("invalid executor_id".into());
        }
        if r["soul_sha256"] != V::Null && !r["soul_sha256"].text().is_some_and(valid_digest) {
            bad("invalid soul_sha256".into());
        }
        if ["traceparent", "tracestate", "baggage"]
            .iter()
            .any(|k| r.get(k).is_some())
        {
            bad("raw trace propagation headers must not be persisted".into());
        }
        if ["trace_id", "span_id", "parent_span_id", "trace_flags"]
            .iter()
            .any(|k| r.get(k).is_some())
        {
            let hex = |k: &str, n: usize, nonzero: bool| {
                r[k].text().is_some_and(|s| {
                    s.len() == n
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        && (!nonzero || s.bytes().any(|b| b != b'0'))
                })
            };
            if !(hex("trace_id", 32, true)
                && hex("span_id", 16, true)
                && hex("trace_flags", 2, false)
                && (r["parent_span_id"] == V::Null || hex("parent_span_id", 16, true)))
            {
                bad("invalid persisted trace context".into());
            }
        }
        let id = text(&r, "event_id");
        if id.is_empty() {
            bad("invalid event_id".into());
        } else if !ids.insert(id.to_string()) {
            bad(format!("duplicate event_id {id}"));
        }
        for key in ["artifact", "patch", "source"] {
            if matches!(r[key], V::Null) || r[key] == V::from("") {
                continue;
            }
            let Some(value) = r[key]
                .text()
                .filter(|s| !Path::new(s).is_absolute() && *s != ".." && !s.starts_with("../"))
            else {
                writeln!(out, "BAD line {n}: unsafe {key} path").unwrap();
                errors += 1;
                continue;
            };
            match path_state(root, value) {
                PathState::Unsafe => {
                    writeln!(out, "BAD line {n}: escaping {key} path").unwrap();
                    errors += 1;
                }
                PathState::Missing => {
                    writeln!(out, "STALE line {n}: missing {key}={value}").unwrap();
                    warnings += 1;
                }
                PathState::Present => (),
            }
        }
        parsed.push((n, r));
    }
    let by_id: BTreeMap<_, _> = parsed
        .iter()
        .filter_map(|(n, r)| r["event_id"].text().map(|id| (id, (*n, r))))
        .collect();
    let mut targets = BTreeSet::new();
    for (n, r) in &parsed {
        if text(r, "kind") != "artifact-resolution" {
            continue;
        }
        let mut bad = |message: String| {
            writeln!(out, "BAD line {n}: {message}").unwrap();
            errors += 1;
        };
        let id = text(r, "resolves_event_id");
        if id.is_empty() {
            bad("resolution missing target event id".into());
            continue;
        }
        if !targets.insert(id) {
            bad(format!("duplicate resolution for {id}"));
        }
        if r["parent_event_id"] != r["resolves_event_id"] || text(r, "resolution") != "resolved" {
            bad("invalid resolution lineage".into());
        }
        if r["exit"].int() != Some("0") {
            bad("resolution exit must be 0".into());
        }
        match by_id.get(id) {
            None => bad(format!("resolution target {id} is missing")),
            Some((oldn, old)) => {
                if text(old, "kind") == "artifact-resolution"
                    || !old["exit"].int().is_some_and(|s| s != "0")
                {
                    bad("resolution target is not a failed outcome".into());
                }
                if oldn >= n {
                    bad("resolution must follow its target".into());
                }
                if r["operation_id"] != old["operation_id"]
                    || r["artifact_id"] != old["artifact_id"]
                {
                    bad("resolution target lineage mismatch".into());
                }
            }
        }
    }
    writeln!(
        out,
        "artifact-index: {total} row(s), {errors} error(s), {warnings} stale reference(s)"
    )
    .unwrap();
    Ok((i32::from(errors + warnings > 0), out.into_bytes()))
}
enum PathState {
    Present,
    Missing,
    Unsafe,
}
fn path_state(root: &Root, value: &str) -> PathState {
    let candidate = crate::normalize(&root.physical_root().join(value));
    let candidate = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    if !candidate.starts_with(root.physical_root()) {
        return PathState::Unsafe;
    }
    let relative = candidate.strip_prefix(root.physical_root()).unwrap();
    // Descriptor-relative reads distinguish absence from a symlink or unsafe
    // component; no content is accepted from beyond the repository boundary.
    match root.read_path(relative, 0, oms_safe_fs::Policy::source()) {
        Ok(_) => PathState::Present,
        Err(oms_safe_fs::Error::Missing) => PathState::Missing,
        Err(oms_safe_fs::Error::Oversize) => PathState::Present,
        Err(_) => PathState::Unsafe,
    }
}
fn prune(
    root: &Root,
    target: &Path,
    snapshot: &[u8],
    rows: &[V],
    o: &Options,
    interrupted: &AtomicBool,
) -> Result<(String, Vec<u8>)> {
    let source = lines(snapshot)?;
    let before = source.len();
    let pins = completion_pins(root.physical_root());
    let mut out = String::new();
    let keep = o.limit;
    let body = if o.stale {
        let decisions = decision_pin_indices(&source, std::time::SystemTime::now(), 3000);
        let mut kept: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                pins.as_ref()
                    .is_none_or(|p| p.contains(text(r, "event_id")))
                    || decisions.contains(i)
                    || ["artifact", "patch", "source"].iter().all(|k| {
                        r[k].text().is_none_or(|s| {
                            s.is_empty()
                                || Path::new(s).is_absolute()
                                || s == ".."
                                || s.starts_with("../")
                                || !matches!(path_state(root, s), PathState::Missing)
                        })
                    })
            })
            .map(|(i, _)| i)
            .collect();
        loop {
            let mut ids = BTreeMap::<&str, Vec<usize>>::new();
            for (pos, i) in kept.iter().enumerate() {
                if let Some(id) = rows[*i]["event_id"].text() {
                    ids.entry(id).or_default().push(pos);
                }
            }
            let mut filtered = Vec::new();
            let mut reset = false;
            for (pos, i) in kept.iter().enumerate() {
                let r = &rows[*i];
                if text(r, "kind") == "artifact-resolution"
                    && !ids
                        .get(text(r, "resolves_event_id"))
                        .is_some_and(|v| v.len() == 1 && v[0] < pos)
                {
                    if pins
                        .as_ref()
                        .is_none_or(|p| p.contains(text(r, "event_id")))
                    {
                        reset = true;
                        break;
                    }
                    continue;
                }
                filtered.push(*i);
            }
            if reset {
                kept = (0..rows.len()).collect();
                break;
            }
            if kept == filtered {
                break;
            }
            kept = filtered;
        }
        let survivors = kept.len();
        let keep = survivors.max(1);
        let refs: Vec<_> = kept.iter().map(|i| source[*i]).collect();
        let body = if survivors == before {
            snapshot.to_vec()
        } else {
            bounded(root.physical_root(), &refs, keep, &[])?
        };
        let after = lines(&body)?.len();
        let stale = before - survivors;
        if stale == 0 {
            writeln!(out, "artifact-index: {before} rows, no stale references").unwrap();
        } else {
            writeln!(
                out,
                "artifact-index: {} {stale} stale row(s), {before} -> {after}",
                if o.dry { "would drop" } else { "dropped" }
            )
            .unwrap();
            if survivors > after {
                writeln!(
                    out,
                    "artifact-index: byte retention {} {} additional row(s)",
                    if o.dry { "would compact" } else { "compacted" },
                    survivors - after
                )
                .unwrap();
            }
        }
        body
    } else {
        let body = if before > keep || snapshot.len() > MAX_INDEX_BYTES {
            bounded(root.physical_root(), &source, keep, &[])?
        } else {
            snapshot.to_vec()
        };
        let after = lines(&body)?.len();
        if body == snapshot {
            writeln!(
                out,
                "artifact-index: {before} rows, {}; nothing pruned",
                if before > keep {
                    format!("completion evidence retained above keep={keep}")
                } else {
                    format!("within keep={keep}")
                }
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "artifact-index: {} {before} -> {after} rows",
                if o.dry { "would prune" } else { "pruned" }
            )
            .unwrap();
        }
        body
    };
    if !o.dry && body != snapshot {
        mutate(root, target, false, Some(snapshot), interrupted, |_| {
            Ok(body.clone())
        })?;
    }
    Ok((out, body))
}
fn deterministic(prefix: &str, row: &V, n: usize) -> Result<String> {
    let encoded = value::encode(row)?;
    let mut quoted = false;
    let mut escaped = false;
    let mut compact = Vec::new();
    for b in &encoded[..encoded.len() - 1] {
        if quoted || *b != b' ' {
            compact.push(*b);
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                quoted = false;
            }
        } else if *b == b'"' {
            quoted = true;
        }
    }
    compact.extend_from_slice(format!("#{n}").as_bytes());
    Ok(format!(
        "{prefix}{}",
        &format!("{:x}", Sha256::digest(compact))[..32]
    ))
}
fn artifact_files(root: &Root) -> Result<Vec<PathBuf>> {
    let mut queue = vec![(root.physical_root().join(".oms/artifacts"), 0)];
    let mut result = Vec::new();
    let mut count = 0;
    while let Some((path, depth)) = queue.pop() {
        if depth > 64 {
            return Err(invalid("artifact scan depth exceeded"));
        }
        let dir = match paths::Directory::open(root, &path, false) {
            Err(e) if paths::missing(&e) => continue,
            other => other?,
        };
        let names = dir.names(65536 - count)?;
        count += names.len();
        for name in names {
            let stat = dir.stat(&name)?;
            let child = path.join(name);
            if rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory
            {
                queue.push((child, depth + 1));
            } else if paths::regular(&stat) {
                result.push(child);
            }
        }
    }
    Ok(result)
}
fn digest(root: &Root, path: &Path) -> Option<String> {
    producer::payload(root, root.physical_root(), path)
        .ok()
        .map(|raw| format!("{:x}", Sha256::digest(raw)))
}
fn migrate(
    root: &Root,
    target: &Path,
    snapshot: &[u8],
    rows: &[V],
    interrupted: &AtomicBool,
) -> Result<String> {
    let files = artifact_files(root)?;
    let mut by_name = BTreeMap::<_, Vec<_>>::new();
    for path in &files {
        by_name.entry(path.file_name()).or_default().push(path);
    }
    let mut digests = BTreeMap::new();
    let mut digest_path = |path: &Path| {
        digests
            .entry(path.to_path_buf())
            .or_insert_with(|| digest(root, path))
            .clone()
    };
    let mut changed = 0;
    let mut legacy = 0;
    let mut body = Vec::new();
    let pattern = regex::Regex::new(r"([0-9]{8}T[0-9]{6}Z-[0-9]+)").unwrap();
    for (pos, original) in rows.iter().enumerate() {
        cancelled(interrupted)?;
        let mut r = original.clone();
        if !schema(&r) {
            legacy += 1;
        }
        for key in ["artifact", "patch", "source"] {
            let Some(value) = r[key].text().filter(|s| !s.is_empty()).map(str::to_string) else {
                continue;
            };
            let path = Path::new(&value);
            let candidate = root.physical_root().join(path);
            // Presence, not digestibility: published payloads may be hard
            // links that the single-link digest refuses (#589).
            if let Ok(canonical) = std::fs::canonicalize(&candidate)
                && canonical.starts_with(root.physical_root())
                && matches!(
                    path_state(
                        root,
                        &canonical
                            .strip_prefix(root.physical_root())
                            .unwrap()
                            .to_string_lossy()
                    ),
                    PathState::Present
                )
            {
                r[key] = canonical
                    .strip_prefix(root.physical_root())
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
                    .into();
                continue;
            }
            let matches = by_name
                .get(&path.file_name())
                .map(Vec::as_slice)
                .unwrap_or_default();
            if matches.len() == 1 {
                r[key] = matches[0]
                    .strip_prefix(root.physical_root())
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
                    .into();
            } else {
                let mut ext = obj!("name":path.file_name().unwrap_or_default().to_string_lossy().to_string(),"owned":false,"legacy_unresolved":true);
                if let Some(h) = digest_path(&candidate) {
                    ext["sha256"] = h.into();
                    ext.remove("legacy_unresolved");
                }
                r.remove(key);
                r[&format!("{key}_external")] = ext;
            }
        }
        r["schema"] = 1.into();
        if r.get("provider").is_none()
            && matches!(
                text(&r, "kind"),
                "review-outcome" | "review-verify" | "patch-admit" | "patch-land"
            )
        {
            // These receipts are produced by the owner, not a provider seat.
            r["provider"] = "local".into();
        }
        if r.get("event_id").is_none() {
            r["event_id"] = deterministic("evt_legacy_", &r, pos + 1)?.into();
        }
        if r.get("operation_id").is_none() {
            let id = pattern
                .captures(text(&r, "artifact"))
                .map(|c| format!("op_legacy_{}", &c[1]))
                .map(Ok)
                .unwrap_or_else(|| deterministic("op_legacy_", &r, pos + 1))?;
            r["operation_id"] = id.into();
        }
        if r.get("artifact_id").is_none() {
            let mut primary = None;
            for key in ["artifact", "patch"] {
                if let Some(p) = r[key].text().filter(|s| !s.is_empty()) {
                    let recorded = r[&format!("{key}_sha256")]
                        .text()
                        .filter(|s| valid_digest(s))
                        .map(str::to_string);
                    primary = recorded.or_else(|| digest_path(Path::new(p))).or(primary);
                }
            }
            r["artifact_id"] = format!(
                "sha256:{}",
                primary.unwrap_or_else(|| format!(
                    "{:x}",
                    Sha256::digest(text(&r, "event_id").as_bytes())
                ))
            )
            .into();
        }
        if r != *original {
            changed += 1;
        }
        body.extend(value::encode(&r)?);
    }
    let (keep, high) = retention_limits(None, None)?;
    let keep = if rows.len() <= high {
        rows.len().max(1)
    } else {
        keep
    };
    let body = bounded(root.physical_root(), &lines(&body)?, keep, &[])?;
    // Serialization alone is not a migration: retain exact bytes when no row changed.
    if changed != 0 || lines(&body)?.len() != rows.len() {
        mutate(root, target, false, Some(snapshot), interrupted, |_| {
            Ok(body.clone())
        })?;
    }
    Ok(format!(
        "artifact-index: migrated {changed} row(s); legacy={legacy}; total={}\n",
        lines(&body)?.len()
    ))
}

fn sensitive(value: &str) -> bool {
    if regex::RegexBuilder::new(r###"((^|[^A-Za-z0-9])[A-Za-z0-9_]*(t[o]ken|s[e]cret|passw(or)?[d]|credentia[l]s?|(ap[i]|s[e]cret|privat[e])[-_ ]?(ke[y]|t[o]ken)|aws_s[e]cret_access_[k]ey)["']?[[:space:]]*[:=]|auth[o]rization:[[:space:]]+[^[:space:]]+|bear[e]r[[:space:]]+[A-Za-z0-9._-]{10,}|[a-z][a-z0-9+.-]*://[^[:space:]/:@]+:[^*[:space:]@/][^[:space:]@/]*@[^[:space:]/]+|(^|[^A-Za-z0-9_-])ey[J][A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}($|[^A-Za-z0-9_-])|gh[pousr]_[A-Za-z0-9_]{20,}|githu[b]_pa[t]_[A-Za-z0-9_]{20,}|npm_[A-Za-z0-9]{36,}|h[f]_[A-Za-z0-9]{34,}|glpa[t]-[A-Za-z0-9_-]{20,}|[sr]k_(liv[e]|tes[t])_[A-Za-z0-9]{16,}|AI[z]a[0-9A-Za-z_-]{35}|hook[s]\.slac[k]\.com/service[s]/|disc[o]rd(app)?\.com/api/webhook[s]/|machin[e][[:space:]]+[^[:space:]]+[[:space:]]+logi[n][[:space:]]+[^[:space:]]+[[:space:]]+passwor[d][[:space:]]+[^[:space:]]+|(^|[^A-Za-z0-9_])s[k]-[A-Za-z0-9_-]{10,}|xox[bap]-[A-Za-z0-9-]{10,}|AK[I]A[0-9A-Z]{16}|(^|[^A-Za-z0-9_])n[t]n_[0-9]{11}[A-Za-z0-9]{35}($|[^A-Za-z0-9])|-----BE[G]IN)"###).case_insensitive(true).build().map_or(true, |r| r.is_match(value)) { return true; }
    if regex::RegexBuilder::new(r###"((^[+]?|[[:space:]=(:<>]|\[|[a-z][a-z0-9+.-]*://|\$\{[A-Za-z_][A-Za-z0-9_]*:?[=+-])["'`]?/(hom[e]|User[s])/[^/$[:space:]"'`{}]+|(^[+]?|[[:space:]=(:<>]|\[|[a-z][a-z0-9+.-]*://|\$\{[A-Za-z_][A-Za-z0-9_]*:?[=+-])["'`]?/(scratc[h]|lustr[e]|gpf[s]|beegf[s])/[^[:space:]"'`{}]+|\.ss[h]/|\.aw[s]/|clust[e]r[[:space:]]*[:=]|partiti[o]n[[:space:]]*[:=]|nodelis[t][[:space:]]*[:=]|sbatc[h][[:space:]]+--partition)"###).case_insensitive(true).build().map_or(true, |r| r.is_match(value)) { return true; }
    for (name, raw) in std::env::vars_os() {
        let name = name.to_string_lossy();
        let raw = raw.to_string_lossy();
        let v = raw.trim();
        if !usable(v) {
            continue;
        }
        let parts: Vec<_> = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_uppercase)
            .collect();
        let words = [
            "KE".to_string() + "Y",
            "APIKE".to_string() + "Y",
            "TO".to_string() + "KEN",
            "SEC".to_string() + "RET",
            "PASS".to_string() + "WORD",
            "PASS".to_string() + "WD",
            "PASS".to_string() + "PHRASE",
            "CRED".to_string() + "ENTIAL",
            "CRED".to_string() + "ENTIALS",
            "AU".to_string() + "THORIZATION",
        ];
        let metadata = [
            "NAME", "METHOD", "TYPE", "KIND", "MODE", "FILE", "PATH", "DIR", "ID", "URL", "VAR",
            "ENV", "FORMAT", "HEADER", "LENGTH",
        ];
        let exempt = name
            .strip_prefix("GIT_CONFIG_KEY_")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            && !v.contains('@');
        if !exempt
            && parts
                .last()
                .is_some_and(|p| !metadata.contains(&p.as_str()))
            && parts.iter().any(|p| words.contains(p))
            && value.contains(v)
        {
            return true;
        }
        if let Some((_, authority)) = v.split_once("://")
            && let Some((userinfo, _)) = authority.split_once('@')
            && let Some((_, part)) = userinfo.split_once(':')
            && part.len() >= 8
            && !["true", "false", "yes", "no", "0", "1"].contains(&part.to_lowercase().as_str())
            && value.contains(part)
        {
            return true;
        }
    }
    false
}
fn usable(v: &str) -> bool {
    v.len() >= 8
        && !v.bytes().all(|b| b.is_ascii_digit())
        && !["true", "false", "yes", "no", "0", "1"].contains(&v.to_ascii_lowercase().as_str())
        && !v.starts_with(['/', '~'])
        && !(v.as_bytes().get(1) == Some(&b':')
            && matches!(v.as_bytes().get(2), Some(b'/' | b'\\')))
        && (v.len() >= 16 || v.bytes().any(|b| !b.is_ascii_alphabetic()))
}
#[path = "import.rs"]
mod importing;
pub(crate) fn import(
    args: &[OsString],
    caller: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    importing::run(args, caller, interrupted)
}
