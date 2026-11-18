//! Recorded byte provenance is independent of provider usage and authority.
use serde_json::{Value, json};

pub const LAYERS: [&str; 7] = [
    "contract",
    "task",
    "retrieved",
    "history",
    "tool_schema",
    "tool_result",
    "framing",
];
const MAX_SPANS: usize = 4096;

#[derive(Clone, Debug)]
pub struct Span {
    pub layer: &'static str,
    pub start: usize,
    pub end: usize,
    pub sha256: String,
    pub prefix_sha256: String,
    pub source_sha256: Option<String>,
    pub source_bytes: Option<usize>,
    pub source_range: Option<(usize, usize)>,
}

/// Describe only ranges identified by the producer; gaps remain unmeasured.
/// Hashing is supplied by the producer that has the actual serialized bytes.
pub fn describe(
    payload: &[u8],
    renderer: &str,
    spans: &[Span],
    hash: impl Fn(&[u8]) -> String,
    policy: Option<&str>,
    manifest: Option<&str>,
) -> Result<Value, String> {
    let rows: Vec<_> = spans
        .iter()
        .map(|s| {
            json!({
                "layer":s.layer,"start":s.start,"end":s.end,"sha256":s.sha256,
                "prefix_sha256":s.prefix_sha256,"source_sha256":s.source_sha256,
                "source_bytes":s.source_bytes,"source_range":s.source_range.map(|(a,b)|[a,b]),
            })
        })
        .collect();
    let mut layers = serde_json::Map::new();
    for layer in LAYERS {
        let bytes: usize = spans
            .iter()
            .filter(|s| s.layer == layer)
            .map(|s| s.end.saturating_sub(s.start))
            .sum();
        layers.insert(layer.into(), json!({"bytes":bytes,"tokens":null}));
    }
    let measured: usize = spans.iter().map(|s| s.end.saturating_sub(s.start)).sum();
    let value = json!({"schema":1,"renderer_version":renderer,"request_sha256":hash(payload),
        "request_bytes":payload.len(),"phase_policy_sha256":policy,"context_manifest_sha256":manifest,
        "measured_bytes":measured,"unattributed_bytes":payload.len().saturating_sub(measured),
        "layers":layers,"spans":rows,"tokenizer":null,
        "provider_added_unknown":{"bytes":null,"tokens":null},
        "cache_hit":null});
    validate(&value, payload.len() as u64, &hash(payload))?;
    Ok(value)
}

fn invalid() -> String {
    "invalid context request attribution".into()
}
fn digest(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn optional_digest(v: &Value) -> bool {
    v.is_null() || digest(v)
}
fn count(v: &Value) -> Result<u64, String> {
    v.as_u64().filter(|n| *n < (1u64 << 53)).ok_or_else(invalid)
}
fn label(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 160
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    })
}
fn keys(v: &Value, allowed: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == allowed.len() && allowed.iter().all(|s| o.contains_key(*s)))
}

/// Validate bounded, nonoverlapping recorded ranges without opening their sources.
pub fn validate(v: &Value, bytes: u64, sha256: &str) -> Result<(), String> {
    if !keys(
        v,
        &[
            "schema",
            "renderer_version",
            "request_sha256",
            "request_bytes",
            "phase_policy_sha256",
            "context_manifest_sha256",
            "measured_bytes",
            "unattributed_bytes",
            "layers",
            "spans",
            "tokenizer",
            "provider_added_unknown",
            "cache_hit",
        ],
    ) || v["schema"] != 1
        || !label(&v["renderer_version"])
        || !digest(&v["request_sha256"])
        || v["request_sha256"] != sha256
        || count(&v["request_bytes"])? != bytes
        || !optional_digest(&v["phase_policy_sha256"])
        || !optional_digest(&v["context_manifest_sha256"])
        || !v["tokenizer"].is_null()
        || !v["cache_hit"].is_null()
        || v["provider_added_unknown"] != json!({"bytes":null,"tokens":null})
        || !keys(&v["layers"], &LAYERS)
    {
        return Err(invalid());
    }
    let spans = v["spans"]
        .as_array()
        .filter(|s| s.len() <= MAX_SPANS)
        .ok_or_else(invalid)?;
    let mut end = 0;
    let mut totals = [0u64; 7];
    for span in spans {
        let start = count(&span["start"])?;
        let next = count(&span["end"])?;
        let layer = LAYERS
            .iter()
            .position(|l| span["layer"] == *l)
            .ok_or_else(invalid)?;
        if !keys(
            span,
            &[
                "layer",
                "start",
                "end",
                "sha256",
                "prefix_sha256",
                "source_sha256",
                "source_bytes",
                "source_range",
            ],
        ) || start < end
            || next <= start
            || next > bytes
            || !digest(&span["sha256"])
            || !digest(&span["prefix_sha256"])
            || !optional_digest(&span["source_sha256"])
        {
            return Err(invalid());
        }
        if !span["source_bytes"].is_null() {
            count(&span["source_bytes"])?;
        }
        if !span["source_range"].is_null() {
            let r = span["source_range"]
                .as_array()
                .filter(|r| r.len() == 2)
                .ok_or_else(invalid)?;
            let a = count(&r[0])?;
            let b = count(&r[1])?;
            if b < a
                || b - a != next - start
                || b > count(&span["source_bytes"])?
                || !digest(&span["source_sha256"])
            {
                return Err(invalid());
            }
        }
        totals[layer] = totals[layer]
            .checked_add(next - start)
            .ok_or_else(invalid)?;
        end = next;
    }
    let measured = totals.iter().sum::<u64>();
    if measured > bytes
        || count(&v["measured_bytes"])? != measured
        || count(&v["unattributed_bytes"])? != bytes - measured
    {
        return Err(invalid());
    }
    for (i, layer) in LAYERS.iter().enumerate() {
        if v["layers"][layer] != json!({"bytes":totals[i],"tokens":null}) {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Inspect an explicit physical-call manifest, retaining unknown delivery and usage.
pub(crate) fn inspect_request(v: &Value, limit: usize) -> Result<Value, String> {
    if v["schema"] != 1
        || v["kind"] != "oms-context-request"
        || !label(&v["operation_id"])
        || !label(&v["call_id"])
        || !label(&v["provider"])
        || !matches!(
            v["dispatch"].as_str(),
            Some("prepared" | "executor-returned" | "executor-error")
        )
        || !v["provider_delivery"].is_null()
    {
        return Err(invalid());
    }
    let a = &v["attribution"];
    validate(
        a,
        count(&a["request_bytes"])?,
        a["request_sha256"].as_str().ok_or_else(invalid)?,
    )?;
    let mut a = a.clone();
    let spans = a["spans"].as_array().ok_or_else(invalid)?;
    a["spans"] = json!({"total":spans.len(),"items":&spans[..spans.len().min(limit)],"omitted":spans.len().saturating_sub(limit)});
    Ok(
        json!({"schema":1,"kind":"oms-context-request-inspection","validation":"recorded-metadata-only",
        "operation_id":v["operation_id"],"call_id":v["call_id"],"provider":v["provider"],
        "dispatch":v["dispatch"],"provider_delivery":null,"usage_basis":"canonical-provider-usage-receipt",
        "usage_receipt":".oms/usage/receipts.jsonl","attribution":a,
        "note":"Prepared primary prompt bytes are not proof of provider delivery, model use, tokens, cost or cache hits. Usage revisions remain in the canonical receipt."}),
    )
}
