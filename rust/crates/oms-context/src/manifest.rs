use crate::{Json, discard_json, parse_json};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use unicode_general_category::{GeneralCategory as G, get_general_category};

const MAX_NUMBER: u64 = (1 << 53) - 1;
const MAX_ROWS: usize = 4096;
type Result<T> = std::result::Result<T, String>;

fn manifest_json(raw: &[u8]) -> Result<Json> {
    parse_json(raw).map_err(str::to_owned)
}
fn error(name: &str) -> String {
    format!("invalid context manifest: {name}")
}
fn get<'a>(v: &'a Json, key: &str) -> Option<&'a Json> {
    if let Json::Object(fields) = v {
        fields
            .iter()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v)
    } else {
        None
    }
}
fn integer(v: Option<&Json>, name: &str) -> Result<u64> {
    if let Some(Json::Number(s)) = v
        && !s.contains(['.', 'e', 'E'])
    {
        if s == "-0" {
            return Ok(0);
        }
        if let Ok(n) = s.parse::<u64>()
            && n <= MAX_NUMBER
        {
            return Ok(n);
        }
    }
    Err(error(name))
}
fn sanitized(v: Option<&Json>, name: &str, limit: usize) -> Result<String> {
    let Some(Json::String(bytes)) = v else {
        return Err(error(name));
    };
    if bytes.len() > limit * 4 {
        return Err(error(name));
    }
    let mut out = String::new();
    let mut left = bytes.as_slice();
    let mut count = 0;
    while !left.is_empty() {
        let (valid, skip) = match std::str::from_utf8(left) {
            Ok(s) => (s, 0),
            Err(e) => (
                std::str::from_utf8(&left[..e.valid_up_to()]).expect("valid prefix"),
                3,
            ),
        };
        for ch in valid.chars() {
            count += 1;
            if count > limit {
                return Err(error(name));
            }
            out.push(if matches!(ch, '\t' | '\r' | '\n') {
                ' '
            } else if matches!(
                get_general_category(ch),
                G::Control | G::Format | G::PrivateUse | G::Surrogate | G::Unassigned
            ) {
                '?'
            } else {
                ch
            });
        }
        left = &left[valid.len()..];
        if skip != 0 {
            count += 1;
            if count > limit {
                return Err(error(name));
            }
            out.push('?');
            left = left.get(skip..).ok_or_else(|| error(name))?;
        }
    }
    if count > limit {
        return Err(error(name));
    }
    Ok(out)
}
fn path(v: Option<&Json>) -> Result<String> {
    let text = sanitized(v, "source path", 4096)?;
    let Some(Json::String(raw)) = v else {
        unreachable!()
    };
    if text.is_empty()
        || text.as_bytes() != raw
        || text.contains(['\\', ':'])
        || text.starts_with('~')
        || text.starts_with('/')
        || text.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return Err(error("source path must be a relative label"));
    }
    Ok(text)
}
fn rows<'a>(v: Option<&'a Json>, name: &str) -> Result<&'a [Json]> {
    if let Some(Json::Array(v)) = v
        && v.len() <= MAX_ROWS
    {
        return Ok(v);
    }
    Err(error(name))
}
fn paths(v: Option<&Json>, name: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for v in rows(v, name)? {
        let p = path(Some(v))?;
        if !seen.insert(p.clone()) {
            return Err(error(&format!("duplicate {name}")));
        }
        out.push(p);
    }
    Ok(out)
}
fn digest(v: Option<&Json>, name: &str, optional: bool) -> Result<Option<String>> {
    if optional && matches!(v, None | Some(Json::Null)) {
        return Ok(None);
    }
    if let Some(Json::String(v)) = v
        && v.len() == 64
        && v.iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Ok(Some(String::from_utf8(v.clone()).expect("ASCII")));
    }
    Err(error(name))
}
fn required_digest(v: Option<&Json>, name: &str) -> Result<String> {
    Ok(digest(v, name, false)?.expect("required digest"))
}
#[derive(Clone)]
struct Source {
    path: String,
    reason: String,
    before: u64,
    payload: u64,
    omitted: u64,
    selection: String,
    digest: String,
}
impl Source {
    fn value(&self) -> Value {
        json!({"path":self.path,"reason":self.reason,"bytes_before":self.before,"bytes_selected":self.payload,"omitted_bytes":self.omitted,"selection":self.selection,"sha256":self.digest})
    }
}
struct Manifest {
    phase: String,
    generated: String,
    budget: u64,
    used: u64,
    remaining: u64,
    targets: Vec<String>,
    missing: Vec<String>,
    truncated: Vec<String>,
    debt: u64,
    sufficient: bool,
    selected: Vec<Source>,
    omitted: Vec<Value>,
    bundle: String,
    contract: Option<String>,
    rules: Option<String>,
}
fn validate(v: &Json) -> Result<Manifest> {
    if !matches!(v, Json::Object(_)) || integer(get(v, "schema"), "schema").ok() != Some(1) {
        return Err("unsupported context manifest schema".into());
    }
    let phase = match get(v, "phase") {
        Some(Json::String(s)) => std::str::from_utf8(s).unwrap_or(""),
        _ => "",
    };
    if ![
        "orientation",
        "implementation",
        "verification",
        "review",
        "research",
    ]
    .contains(&phase)
    {
        return Err(error("phase"));
    }
    let budget = integer(get(v, "budget_bytes"), "budget_bytes")?;
    let used = integer(get(v, "selected_bytes"), "selected_bytes")?;
    let remaining = integer(get(v, "remaining_bytes"), "remaining_bytes")?;
    if budget == 0 || used > budget || used + remaining != budget {
        return Err(error("inconsistent budget"));
    }
    let targets = paths(get(v, "targets"), "targets")?;
    let missing = paths(get(v, "missing_required"), "missing_required")?;
    let truncated = paths(get(v, "truncated_required"), "truncated_required")?;
    let debt = integer(get(v, "context_debt"), "context_debt")?;
    let sufficient = match get(v, "sufficient") {
        Some(Json::Bool(b)) => *b,
        _ => return Err(error("inconsistent context debt")),
    };
    if debt != missing.len() as u64
        || sufficient != (debt == 0)
        || truncated.iter().any(|x| !missing.contains(x))
    {
        return Err(error("inconsistent context debt"));
    }
    let mut selected = Vec::new();
    for row in rows(get(v, "selected"), "selected")? {
        if !matches!(row, Json::Object(_)) {
            return Err(error("selected row"));
        }
        let before = integer(get(row, "bytes_before"), "bytes_before")?;
        let payload = integer(get(row, "bytes_selected"), "bytes_selected")?;
        let omitted = integer(get(row, "omitted_bytes"), "omitted_bytes")?;
        let selection = match get(row, "selection") {
            Some(Json::String(s)) => std::str::from_utf8(s).unwrap_or(""),
            _ => "",
        };
        if !["full", "head", "middle", "tail"].contains(&selection)
            || omitted > before
            || payload > before
            || (selection == "full" && (omitted != 0 || payload != before))
            || (selection != "full" && omitted == 0)
            || (omitted != 0
                && before - omitted > 0
                && before - omitted + 29 + (before - payload).to_string().len() as u64 > payload)
        {
            return Err(error("inconsistent source selection"));
        }
        selected.push(Source {
            path: path(get(row, "path"))?,
            reason: sanitized(get(row, "reason"), "reason", 4096)?,
            before,
            payload,
            omitted,
            selection: selection.into(),
            digest: required_digest(get(row, "sha256"), "source digest")?,
        });
    }
    let mut index = BTreeMap::new();
    let mut total = 0u128;
    for row in &selected {
        index.insert(row.path.as_str(), row);
        total += u128::from(row.payload);
    }
    if index.len() != selected.len() || total > u128::from(used) {
        return Err(error("duplicate source or payload exceeds bundle"));
    }
    if truncated
        .iter()
        .any(|p| index.get(p.as_str()).is_none_or(|r| r.omitted == 0))
        || missing.iter().any(|p| {
            index.get(p.as_str()).is_some_and(|r| r.omitted != 0) && !truncated.contains(p)
        })
    {
        return Err(error("truncated required source"));
    }
    if targets
        .iter()
        .any(|p| index.get(p.as_str()).is_none_or(|r| r.omitted != 0) && !missing.contains(p))
    {
        return Err(error("target debt is missing"));
    }
    if missing
        .iter()
        .any(|p| index.get(p.as_str()).is_some_and(|r| r.omitted == 0))
    {
        return Err(error("missing source was fully selected"));
    }
    let mut omitted = Vec::new();
    let mut omitted_paths = HashSet::new();
    for row in rows(get(v, "omitted"), "omitted")? {
        if !matches!(row, Json::Object(_)) {
            return Err(error("omitted row"));
        }
        let p = path(get(row, "path"))?;
        let reason = sanitized(get(row, "reason"), "omission reason", 4096)?;
        let bytes = if get(row, "bytes").is_some() {
            Some(integer(get(row, "bytes"), "omitted bytes")?)
        } else {
            None
        };
        omitted_paths.insert(p.clone());
        omitted.push(json!({"path":p,"reason":reason,"bytes":bytes}));
    }
    if omitted_paths.len() != omitted.len()
        || omitted_paths.iter().any(|p| index.contains_key(p.as_str()))
    {
        return Err(error("duplicate omitted source"));
    }
    let mut sizes = BTreeMap::new();
    for row in &selected {
        if sizes
            .insert(&row.digest, row.before)
            .is_some_and(|n| n != row.before)
        {
            return Err(error("digest has conflicting source sizes"));
        }
    }
    Ok(Manifest {
        phase: phase.into(),
        generated: sanitized(get(v, "generated_at"), "generated_at", 80)?,
        budget,
        used,
        remaining,
        targets,
        missing,
        truncated,
        debt,
        sufficient,
        selected,
        omitted,
        bundle: required_digest(get(v, "bundle_sha256"), "bundle digest")?,
        contract: digest(get(v, "contract_state_digest"), "contract digest", true)?,
        rules: if get(v, "phase_rules_sha256").is_some() {
            Some(required_digest(
                get(v, "phase_rules_sha256"),
                "phase rules digest",
            )?)
        } else {
            None
        },
    })
}
fn bounded<T: serde::Serialize>(items: &[T], limit: usize) -> Value {
    json!({"total":items.len(),"items":&items[..items.len().min(limit)],"omitted":items.len().saturating_sub(limit)})
}
fn check_limit(limit: usize) -> Result<()> {
    if (1..=100).contains(&limit) {
        Ok(())
    } else {
        Err("limit must be an integer from 1 to 100".into())
    }
}
/// Validate a JSON inspection limit before accessing a manifest.
pub fn inspection_limit(value: &Value) -> Result<usize> {
    let limit = value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| "limit must be an integer from 1 to 100".to_string())?;
    check_limit(limit)?;
    Ok(limit)
}
/// Validate and project a raw JSON manifest without filesystem access.
pub fn inspect_manifest(raw: &[u8], limit: usize) -> Result<Value> {
    check_limit(limit)?;
    let parsed = manifest_json(raw)?;
    let validated = validate(&parsed);
    discard_json(parsed);
    let m = validated?;
    Ok(inspect(&m, limit))
}
fn inspect(m: &Manifest, limit: usize) -> Value {
    let mut rows = m.selected.clone();
    rows.sort_by(|a, b| b.payload.cmp(&a.payload).then_with(|| a.path.cmp(&b.path)));
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in &rows {
        if row.before != 0 {
            groups.entry(&row.digest).or_default().push(&row.path);
        }
    }
    let repeated: Vec<Value> = groups
        .into_iter()
        .filter_map(|(digest, mut paths)| {
            if paths.len() < 2 {
                None
            } else {
                paths.sort();
                Some(json!({"sha256":digest,"paths":bounded(&paths,limit)}))
            }
        })
        .collect();
    let payload: u64 = rows.iter().map(|r| r.payload).sum();
    let sources: Vec<Value> = rows.iter().map(Source::value).collect();
    let mut result = json!({"schema":1,"kind":"oms-context-inspection","validation":"recorded-metadata-only","sources_current":null,"bundle_verified":false,"note":"Not the provider's full context; no source or bundle files were read. Repeated source hashes do not prove duplicate selected bytes. No token or billing savings are inferred.","phase":m.phase,"generated_at":m.generated,"budget":{"bytes":m.budget,"used_bytes":m.used,"remaining_bytes":m.remaining,"source_payload_bytes":payload,"framing_bytes":m.used-payload},"debt":{"reported":m.debt,"reported_sufficient":m.sufficient,"missing_required":bounded(&m.missing,limit),"truncated_required":bounded(&m.truncated,limit)},"targets":bounded(&m.targets,limit),"sources":bounded(&sources,limit),"omissions":bounded(&m.omitted,limit),"repeated_source_digests":bounded(&repeated,limit),"bundle_sha256":m.bundle,"contract_state_digest":m.contract});
    if let Some(rules) = &m.rules {
        result["phase_rules_sha256"] = json!(rules);
    }
    result
}
/// Compare two raw manifests; deltas describe recorded metadata only.
pub fn compare_manifests(left: &[u8], right: &[u8], limit: usize) -> Result<Value> {
    fn parse_and_validate(raw: &[u8]) -> Result<Manifest> {
        let parsed = manifest_json(raw)?;
        let validated = validate(&parsed);
        discard_json(parsed);
        validated
    }
    let before = parse_and_validate(left)?;
    let after = parse_and_validate(right)?;
    check_limit(limit)?;
    let mut reasons = Vec::new();
    if before.phase != after.phase {
        reasons.push("different-phase");
    }
    let old_targets: BTreeSet<_> = before.targets.iter().collect();
    let new_targets: BTreeSet<_> = after.targets.iter().collect();
    if old_targets != new_targets {
        reasons.push("different-targets");
    }
    if before.contract.is_none() || after.contract.is_none() {
        reasons.push("contract-identity-unavailable");
    } else if before.contract != after.contract {
        reasons.push("different-contract-state");
    }
    let rules = if before.rules.is_some() || after.rules.is_some() {
        let status = if before.rules.is_none() || after.rules.is_none() {
            reasons.push("phase-rules-identity-unavailable");
            "unknown"
        } else if before.rules != after.rules {
            reasons.push("different-phase-rules");
            "different-recorded-digest"
        } else {
            "same-recorded-digest"
        };
        Some(json!({"left":before.rules,"right":after.rules,"status":status}))
    } else {
        None
    };
    let old: BTreeMap<_, _> = before.selected.iter().map(|r| (&r.path, r)).collect();
    let new: BTreeMap<_, _> = after.selected.iter().map(|r| (&r.path, r)).collect();
    let added: Vec<_> = new.keys().filter(|p| !old.contains_key(*p)).collect();
    let removed: Vec<_> = old.keys().filter(|p| !new.contains_key(*p)).collect();
    let mut changed = Vec::new();
    for (p, a) in &old {
        if let Some(b) = new.get(p) {
            let mut fields = Vec::new();
            if a.digest != b.digest {
                fields.push("sha256");
            }
            if a.payload != b.payload {
                fields.push("bytes_selected");
            }
            if a.selection != b.selection {
                fields.push("selection");
            }
            if a.omitted != b.omitted {
                fields.push("omitted_bytes");
            }
            if a.reason != b.reason {
                fields.push("reason");
            }
            if !fields.is_empty() {
                changed.push(json!({"path":p,"changed_fields":fields}));
            }
        }
    }
    let mut result = json!({"schema":1,"kind":"oms-context-comparison","validation":"recorded-metadata-only","recorded_scope_matches":reasons.is_empty(),"scope_mismatch_reasons":reasons,"note":"Deltas describe saved metadata, not quality, current validity, cost savings or a winning run.","delta":{"budget_bytes":after.budget as i64-before.budget as i64,"selected_bytes":after.used as i64-before.used as i64,"remaining_bytes":after.remaining as i64-before.remaining as i64,"context_debt":after.debt as i64-before.debt as i64},"added":bounded(&added,limit),"removed":bounded(&removed,limit),"changed":bounded(&changed,limit)});
    if let Some(rules) = rules {
        result["phase_rules"] = rules;
    }
    Ok(result)
}
