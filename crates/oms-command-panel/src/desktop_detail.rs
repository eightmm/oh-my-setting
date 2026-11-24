//! On-demand projections for the desktop reader, outside the compact dashboard.
use crate::{Error, Result, Services, cli::Options, commands::query, parent_only, rows};
use oms_panel_core::{
    NativeCommand, Store, Value, metrics::desktop_receipts, native::room, results,
};
use serde_json::json;
use std::{collections::BTreeSet, path::Path, sync::atomic::AtomicBool};

pub(super) fn usage(repo: &Path, report: &Value, now: f64, stop: &AtomicBool) -> Value {
    let snapshot = crate::usage::snapshot(report, now);
    let recent = rows(&report["attempts"]["recent"]);
    let totals = desktop_receipts(
        repo,
        recent
            .iter()
            .take(16)
            .filter_map(|r| r["attempt_id"].as_str()),
        stop,
    );
    let mut value = usage_projection(
        &snapshot,
        recent,
        totals.as_ref().map(|(attempts, _)| attempts),
    );
    let mut lifetime = totals.map(|(_, lifetime)| lifetime).unwrap_or(Value::Null);
    if let Some(groups) = lifetime.as_array_mut() {
        value["providerTotalsOmitted"] = groups.len().saturating_sub(32).into();
        groups.truncate(32);
    }
    value["providerTotals"] = lifetime;
    value
}

fn usage_projection(
    snapshot: &Value,
    recent: &[Value],
    receipts: Option<&serde_json::Map<String, Value>>,
) -> Value {
    let providers = ["codex", "claude", "antigravity", "pi"]
        .into_iter()
        .filter_map(|provider| quota_provider(&snapshot["providers"], provider))
        .collect::<Vec<_>>();
    let attempts: Vec<_> = recent
        .iter()
        .take(16)
        .filter_map(|attempt| {
            let id = attempt["attempt_id"].as_str()?;
            let receipt = receipts.and_then(|r| r.get(id)).unwrap_or(&Value::Null);
            Some(attempt_usage(attempt, receipt))
        })
        .collect();
    json!({
        "providers": providers, "attempts": attempts,
        "scope":"repository-recent-attempts", "attemptsOmitted": recent.len().saturating_sub(16)
    })
}

fn quota_provider(providers: &Value, provider: &str) -> Option<Value> {
    let p = providers.get(provider)?;
    let windows: Vec<_> = rows(&p["windows"])
        .iter()
        .take(8)
        .map(|w| {
            json!({
                "name":w["name"], "used":w["used"], "source":w["source"],
                "stale":w["stale"], "resets_at":w["resets_at"], "age_seconds":w["age_seconds"]
            })
        })
        .collect();
    let seven = &p["tokens"]["recent_7d"];
    Some(json!({
        "provider":provider, "windows":windows,"windowsOmitted":rows(&p["windows"]).len().saturating_sub(8),
        "contextUsed":p["context_used"].as_u64(), "cacheReadShare":p["cache_read_share"]["percent"].as_u64(),
        "recent7d": seven["calls"].as_u64().map(|calls|json!({
            "calls":calls,"input":seven["input_tokens"],"output":seven["output_tokens"],"cacheRead":seven["cache_read_tokens"]
        }))
    }))
}

fn attempt_usage(attempt: &Value, receipt: &Value) -> Value {
    let event = |key: &str| {
        attempt["usage_reports"][key]
            .as_u64()
            .filter(|reports| *reports > 0)
            .and_then(|_| attempt["usage"][key].as_u64())
    };
    let (tokens, source) = match (receipt["total"].as_u64(), event("tokens")) {
        (Some(total), _) => (Some(total), Some("receipt")),
        (None, Some(total)) => (Some(total), Some("events")),
        (None, None) => (None, None),
    };
    let breakdown: Vec<_> = receipt["providers"]
        .as_object()
        .into_iter()
        .flatten()
        .take(16)
        .map(|(provider, r)| {
            json!({
                "provider":provider,"calls":r["calls"],"input":r["input"],"output":r["output"],
                "cacheRead":r["cache_read"],"cacheWrite":r["cache_write"]
            })
        })
        .collect();
    let cost = event("cost_microusd");
    json!({"id":attempt["attempt_id"],"provider":attempt["provider"],"tokens":tokens,"source":source,
        "reportedCalls":receipt["reported"],"calls":receipt["calls"],"breakdown":breakdown,
        "costMicrousd":cost,"costSource":cost.map(|_|"events")})
}

pub(super) fn chat(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    parent_only()?;
    let id = options.required("room")?;
    let participant = options.required("participant")?;
    let digest = options.required("conversation-digest")?;
    let before = options
        .get("before")
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| Error::new("invalid chat cursor"))
        })
        .transpose()?;
    let reader = room::Room::open(
        repo,
        room::Session::from_env(),
        &room::LocalServices,
        stop,
        false,
        false,
    )
    .map_err(|_| Error::new("Room unavailable"))?;
    // Recheck the same positive binding used by the dashboard on every click.
    // An old model response or widget cannot reopen a superseded/terminal Main.
    let attempts = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let attempts = attempts
        .as_array()
        .ok_or_else(|| Error::new("invalid attempt collection"))?;
    if crate::desktop_history::bound_room_from_events(&reader, &digest, attempts)?.as_deref()
        != Some(&id)
    {
        return Err(Error::new("Room chat unavailable for this conversation"));
    }
    let read_scope = || {
        reader
            .desktop_chat(&id, &participant, &digest, before)
            .map_err(|_| Error::new("Room chat unavailable for this conversation"))
    };
    let page = read_scope()?;
    let body =
        room::value::encode(&page, false, true).map_err(|_| Error::new("invalid Room page"))?;
    let mut response: Value = serde_json::from_slice(&body).map_err(Error::new)?;
    // Scan the actual message bytes: a credential containing quotes or newlines
    // cannot match the escaped representation of the enclosing JSON document.
    for message in response["messages"].as_array_mut().into_iter().flatten() {
        if let Some(text) = message["text"].as_str() {
            message["text"] = oms_panel_core::redact(text.as_bytes())?.into();
        }
    }
    response["result"] = Value::Null;
    let selected: Vec<_> = attempts
        .iter()
        .filter(|row| attempt_belongs(row, &id, &participant))
        .collect();
    if let Some(section) = options.get("evidence-section") {
        let attempt = options.required("evidence-attempt")?;
        let matches: Vec<_> = selected
            .iter()
            .copied()
            .filter(|a| a["attempt_id"] == attempt)
            .collect();
        let [record] = matches.as_slice() else {
            return Err(Error::new("attempt unavailable for this Room participant"));
        };
        response["evidence"] = crate::desktop_evidence::read(
            repo,
            record,
            section,
            options.integer("page", 1, 1, 10000)?,
            services,
            stop,
        )?;
        // Evidence collection can perform Git or artifact I/O. Re-read the
        // producer lifecycle and Room custody before returning its private body.
        let current = query(
            services,
            NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?;
        let current = current
            .as_array()
            .ok_or_else(|| Error::new("invalid attempt collection"))?;
        let consistent = current
            .iter()
            .find(|row| row["attempt_id"] == attempt)
            .is_some_and(|row| {
                attempt_belongs(row, &id, &participant)
                    && row["refs"] == record["refs"]
                    && row["provider"] == record["provider"]
                    && row["tool"] == record["tool"]
                    && row["task_id"] == record["task_id"]
            });
        if !consistent
            || crate::desktop_history::bound_room_from_events(&reader, &digest, current)?.as_deref()
                != Some(&id)
        {
            return Err(Error::new("evidence binding changed during collection"));
        }
        response["messages"] = json!([]);
        response["nextBefore"] = Value::Null;
        read_scope()?;
    } else if before.is_none() {
        response["attempts"]=json!(selected.iter().rev().take(16).map(|a|json!({
            "id":a["attempt_id"],"tool":a["tool"],"state":a["state"],"provider":a["provider"],"sections":crate::desktop_evidence::sections(a)
        })).collect::<Vec<_>>());
        response["attemptsOmitted"] = json!(selected.len().saturating_sub(16));
        if let Some(result) = recorded_result(repo, &selected) {
            read_scope()?;
            response["result"] = result;
        }
    }
    Ok(response)
}

fn attempt_belongs(row: &Value, room: &str, participant: &str) -> bool {
    let refs = &row["refs"];
    refs["panel_room_id"] == room
        && refs["panel_room"]
            .as_str()
            .is_none_or(|id| id.is_empty() || id == room)
        && (refs["panel_room_participant"] == participant
            || (row["tool"] == "panel-main"
                && oms_panel_core::state::main_participant(row) == participant)
            || (row["tool"] == "panel-native-assignment"
                && refs.get("native_return_schema").is_some()
                && row["attempt_id"] == participant))
}

fn recorded_result(repo: &Path, attempts: &[&Value]) -> Option<Value> {
    let matches: BTreeSet<_> = attempts
        .iter()
        .filter_map(|row| row["attempt_id"].as_str())
        .collect();
    if matches.is_empty() {
        return None;
    }
    let store = Store::open(repo).ok()?;
    if let Some(attempt) = attempts.iter().rev().find(|a| {
        a["tool"] == "panel-native-assignment"
            && a["attempt_id"]
                .as_str()
                .is_some_and(|id| matches.contains(id))
    }) && let Ok(Some(report)) = oms_panel_core::native_results::read(&store, attempt)
        && let Some(summary) = report["summary"].as_str()
        && let Ok(full) = oms_panel_core::redact(summary.as_bytes())
    {
        return result_excerpt(
            attempt["attempt_id"].clone(),
            "native-report".into(),
            attempt["updated_at"].clone(),
            &full,
        );
    }
    let index = results::index_rows(repo).ok()?;
    let row = index.iter().rev().find(|r| {
        r["attempt_id"]
            .as_str()
            .is_some_and(|id| matches.contains(id))
            && matches!(
                r["kind"].as_str(),
                Some("ask" | "call" | "consult" | "delegate" | "review")
            )
            && !matches!(r["answer_quality"].as_str(), Some("empty" | "no_answer"))
            && r["artifact"].is_string()
    })?;
    let full = crate::desktop_evidence::indexed_answer(&store, row)?;
    result_excerpt(
        row["attempt_id"].clone(),
        row["kind"].clone(),
        row["ts"].clone(),
        &full,
    )
}

fn result_excerpt(attempt: Value, kind: Value, at: Value, full: &str) -> Option<Value> {
    if full.is_empty() {
        return None;
    }
    let text: String = full.chars().take(2000).collect();
    Some(
        json!({"attempt":attempt,"kind":kind,"at":at,"truncated":text.len()!=full.len(),"text":text}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_needs_one_consistent_room_and_member_binding() {
        let mut record = json!({"attempt_id":"a","tool":"delegate","refs":{"panel_room_id":"one","panel_room_participant":"member"}});
        assert!(attempt_belongs(&record, "one", "member"));
        assert!(!attempt_belongs(&record, "two", "member"));
        assert!(!attempt_belongs(&record, "one", "other"));
        record["refs"]["panel_room"] = "two".into();
        assert!(!attempt_belongs(&record, "one", "member"));
    }

    #[test]
    fn records_keep_receipt_precedence_missing_values_and_provider_cache_conventions() {
        let report = json!({"providers":{"claude":{"windows":[{"name":"5h","used":0,"source":"HUD","stale":false,"resets_at":1000,"age_seconds":0}],"account":{"label":"private-account"},"context_used":0,"cache_read_share":{"percent":0}}}});
        let events = json!([
            {"attempt_id":"one","provider":"codex","usage":{"tokens":80,"cost_microusd":0},"usage_reports":{"tokens":1,"cost_microusd":1}},
            {"attempt_id":"two","provider":"claude","usage":{"tokens":0,"cost_microusd":0},"usage_reports":{"tokens":1,"cost_microusd":0}},
            {"attempt_id":"three","provider":"antigravity","usage":{"tokens":0},"usage_reports":{"tokens":0}}
        ]);
        let receipts = json!({"one":{"total":11,"reported":1,"calls":1,"providers":{"codex":{"input":7,"output":4,"cache_read":2,"cache_write":null,"calls":1,"launch_accounts":[{"alias":"private-account"}]}}},"two":{"total":null,"reported":0,"calls":1}});
        let v = usage_projection(&report, events.as_array().unwrap(), receipts.as_object());
        assert!(!v.to_string().contains("private-account"));
        assert_eq!(v["attempts"][0]["source"], "receipt");
        assert_eq!(v["attempts"][0]["tokens"], 11);
        assert_eq!(v["attempts"][0]["costMicrousd"], 0);
        assert_eq!(v["attempts"][0]["breakdown"][0]["input"], 7);
        assert!(v["attempts"][0]["breakdown"][0]["cacheWrite"].is_null());
        assert_eq!(v["attempts"][1]["source"], "events");
        assert_eq!(v["attempts"][1]["tokens"], 0);
        assert!(v["attempts"][1]["costMicrousd"].is_null());
        assert!(v["attempts"][2]["tokens"].is_null());
        assert_eq!(v["providers"][0]["contextUsed"], 0);
    }
}
