//! Bounded provider event summaries; private provider homes are not traversed.
use crate::value::{array, text};
use crate::{Result, Store, Value, require};
use std::path::Path;
use std::time::Duration;
pub const MAX_STREAM: usize = 8 * 1024 * 1024;
pub const MAX_LINE: usize = 1024 * 1024;
#[derive(Clone, Debug, serde::Serialize)]
pub struct Event {
    pub kind: String,
    pub text: String,
    pub stamp: Option<String>,
}
pub fn events(provider: &str, bytes: &[u8], maximum: usize) -> Result<Vec<Event>> {
    require(
        bytes.len() <= MAX_STREAM && maximum <= 4096,
        "activity stream exceeds bounds",
    )?;
    let mut events = Vec::new();
    for line in bytes.split(|&v| v == b'\n').filter(|v| !v.is_empty()) {
        require(line.len() <= MAX_LINE, "activity line exceeds byte limit")?;
        let Ok(v) = crate::decode(line, MAX_LINE) else {
            continue;
        };
        let payload = v.get("payload").unwrap_or(&v);
        let kind = payload["type"]
            .as_str()
            .or_else(|| v["type"].as_str())
            .unwrap_or_default();
        let content = if ["agent_message", "assistant", "message", "text"].contains(&kind) {
            payload["message"]
                .as_str()
                .or_else(|| payload["text"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    array(&payload["content"])
                        .iter()
                        .filter_map(|v| v["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        } else if [
            "function_call",
            "tool_use",
            "tool_result",
            "command_execution",
        ]
        .contains(&kind)
        {
            format!("{} {}", kind, payload["name"].as_str().unwrap_or_default())
        } else {
            continue;
        };
        let safe = crate::value::redact(content.as_bytes())?;
        let safe = crate::eventlog::clean(&safe, 2000);
        if !safe.is_empty() {
            events.push(Event {
                kind: format!("{provider}:{kind}"),
                text: safe,
                stamp: v["timestamp"].as_str().map(str::to_owned),
            });
        }
    }
    if events.len() > maximum {
        events.drain(..events.len() - maximum);
    }
    Ok(events)
}
pub fn summarize(provider: &str, bytes: &[u8]) -> Result<Value> {
    let events = events(provider, bytes, 256)?;
    let final_text = events
        .iter()
        .rev()
        .find(|v| {
            v.kind.ends_with("agent_message")
                || v.kind.ends_with("assistant")
                || v.kind.ends_with("message")
        })
        .map(|v| v.text.clone());
    Ok(
        serde_json::json!({"schema":1,"provider":provider,"events":events,"final_text":final_text,"generated_at":crate::now()}),
    )
}
pub fn record_summary(store: &Store, member: &str, provider: &str, bytes: &[u8]) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(member)?;
    let summary = summarize(provider, bytes)?;
    crate::outbound(&crate::value::encoded(&summary)?)?;
    let tx = store.transaction("panel-activity", Duration::from_secs(10))?;
    tx.write_json(
        Path::new(&format!(
            ".oms/artifacts/panel-dispatch/{member}-summary.json"
        )),
        &summary,
        true,
        MAX_STREAM,
    )?;
    Ok(summary)
}
pub fn read_summary(store: &Store, member: &str) -> Result<Option<Value>> {
    crate::identifier(member)?;
    store.json(
        Path::new(&format!(
            ".oms/artifacts/panel-dispatch/{member}-summary.json"
        )),
        MAX_STREAM,
        true,
    )
}
pub fn summary_lines(summary: &Value, maximum: usize) -> Vec<String> {
    array(&summary["events"])
        .iter()
        .rev()
        .take(maximum)
        .rev()
        .map(|v| crate::eventlog::clean(&text(v, "text"), 256))
        .collect()
}
pub fn background(report: &Value) -> Value {
    let mut rows = vec![];
    for key in ["autopilot", "graphs", "queue", "runs"] {
        for row in array(&report[key]) {
            rows.push(serde_json::json!({"kind":key,"state":row["state"],"id":row["id"],"summary":crate::eventlog::clean(&text(row,"summary"),240)}));
        }
    }
    serde_json::json!({"schema":1,"rows":rows,"read_only":true})
}
