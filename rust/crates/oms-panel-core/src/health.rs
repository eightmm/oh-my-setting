//! Advisory pressure and work health; these observations never authorize a mutation.
use crate::Value;
use crate::value::{array, text};
fn item(
    items: &mut Vec<Value>,
    kind: &str,
    label: &str,
    used: Option<u64>,
    limit: u64,
    lower: bool,
    stale: bool,
    action: &str,
) {
    let status = if stale || used.is_none() || limit == 0 {
        "unknown"
    } else if used.is_some_and(|v| if lower { v == 0 } else { v >= limit }) {
        "full"
    } else if used.is_some_and(|v| {
        if lower {
            v <= 10
        } else {
            v.saturating_mul(10) >= limit.saturating_mul(9)
        }
    }) {
        "warning"
    } else {
        "ok"
    };
    items.push(serde_json::json!({"kind":kind,"label":label,"used":if stale { None } else {used},"limit":limit,"status":status,"action":action}));
}
pub fn project(report: &Value, now: f64) -> Value {
    let stale = report["capacity_stale"] == true || report["collection"]["ok"] != true;
    let mut items = vec![];
    for provider in ["codex", "claude"] {
        let row = &report["provider_status"][provider];
        let stale = stale || ["cache stale", "unavailable"].contains(&text(row, "source").as_str());
        item(
            &mut items,
            "context",
            &format!("{provider} context used"),
            crate::metrics::percent(&row["context_left"]).map(|v| u64::from(100 - v)),
            100,
            false,
            stale,
            "Use native compaction or an explicit handoff.",
        );
        item(
            &mut items,
            "quota",
            &format!("{provider} weekly left"),
            crate::metrics::percent(&row["weekly_used"]).map(|v| u64::from(100 - v)),
            100,
            true,
            stale,
            "Check the native quota/reset.",
        );
        if !stale && let Some((left, _)) = crate::metrics::five_hour(row, now) {
            item(
                &mut items,
                "quota",
                &format!("{provider} 5-hour left"),
                Some(u64::from(left)),
                100,
                true,
                false,
                "Wait for reset or check native quota.",
            );
            if let Some(v) = items.last_mut() {
                v["advisory"] = true.into();
            }
        }
    }
    let room = &report["room"];
    if room["closed"] == true {
        item(
            &mut items,
            "room",
            "Room closed",
            Some(1),
            1,
            false,
            stale,
            "Select a fresh room.",
        );
    }
    if room["participants"].is_array() {
        let calls = array(&room["participants"])
            .iter()
            .filter(|v| !text(v, "parent").is_empty())
            .count() as u64;
        item(
            &mut items,
            "room",
            "Room calls",
            Some(calls),
            oms_command_room::MAX_CALLS as u64,
            false,
            stale,
            "Continue as a linked successor near capacity.",
        );
        item(
            &mut items,
            "room",
            "Room mains",
            Some(array(&room["participants"]).len() as u64 - calls),
            oms_command_room::MAX_PARTICIPANTS as u64,
            false,
            stale,
            "New mains use a bounded room.",
        );
        item(
            &mut items,
            "room",
            "Room messages",
            room["message_count"].as_u64(),
            oms_command_room::MAX_MESSAGES as u64,
            false,
            stale,
            "The next dispatch continues near capacity.",
        );
    }
    if let Some(capacities) = report["private_capacity"].as_object() {
        for (kind, row) in capacities {
            item(
                &mut items,
                "slots",
                &format!("{kind} slots"),
                row["used"].as_u64(),
                row["limit"].as_u64().unwrap_or(0),
                false,
                stale,
                "Archive completed or positively gone records.",
            );
            item(
                &mut items,
                "history",
                &format!("{kind} history"),
                row["archived"].as_u64(),
                row["archive_limit"].as_u64().unwrap_or(0),
                false,
                stale,
                "Review retained history before explicitly forgetting records.",
            );
        }
    }
    let level = ["full", "urgent", "warning", "unknown"]
        .into_iter()
        .find(|s| {
            items
                .iter()
                .any(|v| v["status"] == *s && v["advisory"] != true)
        })
        .unwrap_or("ok");
    serde_json::json!({"schema":1,"level":level,"items":items})
}
pub fn work_health(report: &Value) -> Value {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for row in array(&report["attempts"]["active"]) {
        *counts.entry(text(row, "state")).or_default() += 1;
    }
    serde_json::json!({"schema":1,"states":counts,"collection_ok":report["collection"]["ok"] == true})
}
pub fn alerts(report: &Value, now: f64) -> Vec<String> {
    array(&project(report, now)["items"])
        .iter()
        .filter(|v| {
            ["full", "urgent", "warning"].contains(&text(v, "status").as_str())
                && v["advisory"] != true
        })
        .map(|v| {
            format!(
                "! Capacity: {} {}/{}",
                text(v, "label"),
                v["used"],
                v["limit"]
            )
        })
        .collect()
}

/// Probe the registered catalog through the same bounded native service as
/// dispatch/council. This lists cached models and does not spend model calls.
pub fn provider_catalog(
    repo: &std::path::Path,
    selection: &str,
    service: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
) -> crate::Result<Value> {
    crate::require(
        !selection.is_empty()
            && selection.len() <= 4096
            && selection
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"._,-".contains(&v)),
        "invalid provider selection",
    )?;
    let catalog = crate::services::query(
        service,
        crate::NativeCommand::ProviderCatalog,
        repo,
        &["--providers", selection, "--json"],
        stop,
    )?;
    let rows = catalog.get("providers").unwrap_or(&catalog);
    crate::require(rows.is_array(), "provider catalog projection unavailable")?;
    let mut normalized = Vec::new();
    for row in array(rows) {
        crate::identifier(&text(row, "provider"))?;
        let mut row = row.clone();
        if row.get("installed").is_none() {
            row["installed"] = row["present"].clone();
        }
        crate::require(
            row["installed"].is_boolean(),
            "provider installation proof unavailable",
        )?;
        normalized.push(row);
    }
    Ok(serde_json::json!({"schema":1,"providers":normalized}))
}
