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
            options.has("evidence-full"),
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

/// Only the verified conversation owner may inspect a worker's retained output.
/// A detached worker can outlive Room membership; its recorded parent still has to be this exact Lead.
pub(super) fn worker_output(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    parent_only()?;
    let session = room::Session::from_env();
    if session.child || session.participant.is_some() {
        return Err(Error::new(
            "worker output requires a bound app conversation",
        ));
    }
    let reader = room::Room::open(repo, session, &room::LocalServices, stop, false, false)
        .map_err(|_| Error::new("Room unavailable"))?;
    let id = options.required("room")?;
    let lead = options.required("lead-attempt")?;
    let worker = options.required("worker-attempt")?;
    let digest = options.required("conversation-digest")?;
    let select = || {
        let events = query(
            services,
            NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?;
        let events = events
            .as_array()
            .ok_or_else(|| Error::new("invalid lifecycle attempts"))?;
        let projection = crate::desktop_history::bound_main_from_events(&reader, &digest, events)?;
        let main: Value = serde_json::from_slice(
            &room::value::encode(&projection, false, true).map_err(Error::new)?,
        )
        .map_err(Error::new)?;
        let proof = worker_scope(events, &main, &id, &lead, &worker)
            .ok_or_else(|| Error::new("worker output outside the bound hierarchy"))?;
        Ok::<_, Error>((
            main["participant"].clone(),
            proof.0.clone(),
            proof.1.clone(),
        ))
    };
    let proof = select()?;
    let attempt = &proof.2;
    if options.has("page") {
        let number = options.integer("page", 1, 1, 10000)?;
        if let Some(outcome) = crate::desktop_evidence::worker_answer_page(repo, attempt, number)? {
            if stop.load(std::sync::atomic::Ordering::Relaxed) || select()? != proof {
                return Err(Error::new("worker output binding changed"));
            }
            return match outcome {
                Ok(mut page) => {
                    let text = page.as_object_mut().and_then(|o| o.remove("text"));
                    Ok(
                        json!({"room":id,"leadAttempt":lead,"workerAttempt":worker,"status":"complete",
                        "text":text,"source":page["source"],"truncated":page["hasMore"],"page":page}),
                    )
                }
                Err(oms_scrub::OutboundWithheld) => Ok(
                    json!({"room":id,"leadAttempt":lead,"workerAttempt":worker,"status":"outbound_withheld",
                    "text":Value::Null,"source":Value::Null,"truncated":false}),
                ),
            };
        }
    }
    let (status, text, source, truncated) =
        match crate::desktop_evidence::worker_answer(repo, attempt) {
            Ok(Some(Ok(preview))) => (
                "complete",
                Some(preview.text),
                Some(preview.source),
                preview.truncated,
            ),
            Ok(Some(Err(oms_scrub::OutboundWithheld))) => ("outbound_withheld", None, None, false),
            Ok(None) if attempt["terminal"] == true => ("not_recorded", None, None, false),
            Ok(None)
                if attempt["terminal"] == false
                    && matches!(
                        attempt["state"].as_str(),
                        Some("working" | "running" | "verifying")
                    ) =>
            {
                ("running", None, None, false)
            }
            _ => ("unavailable", None, None, false),
        };
    if stop.load(std::sync::atomic::Ordering::Relaxed) || select()? != proof {
        return Err(Error::new("worker output binding changed"));
    }
    Ok(
        json!({"room":id,"leadAttempt":lead,"workerAttempt":worker,"status":status,"text":text,"source":source,"truncated":truncated}),
    )
}

fn worker_scope<'a>(
    attempts: &'a [Value],
    main: &Value,
    room: &str,
    lead_id: &str,
    worker_id: &str,
) -> Option<(&'a Value, &'a Value)> {
    use crate::render::attempt_room;
    use oms_command_room::AttemptRoom;
    if main["room"] != room
        || main["state"] != "joined"
        || main["provider"] != "codex"
        || main["participantsOmitted"] != 0
        || lead_id == worker_id
    {
        return None;
    }
    let owner = main["participant"].as_str()?;
    let members = main["participants"].as_array()?;
    let unique = |id: &str| {
        let mut xs = attempts.iter().filter(|v| v["attempt_id"] == id);
        let row = xs.next()?;
        xs.next().is_none().then_some(row)
    };
    let lead = unique(lead_id)?;
    let worker = unique(worker_id)?;
    // Native lifecycle encodes absent parents as empty; malformed references cannot inherit.
    let parent = |row: &'a Value| match row.get("parent_attempt_id") {
        None | Some(Value::Null) => Some(None),
        Some(v) if v.as_str() == Some("") => Some(None),
        Some(v) => v
            .as_str()
            .filter(|s| room::identifier(s, "parent").is_ok())
            .map(Some),
    };
    let member = |id: &str| {
        members
            .iter()
            .find(|m| m["participant"] == id && m["room"] == room)
    };
    let participant = |row: &'a Value| match row["refs"].get("panel_room_participant") {
        None => Some(None),
        Some(v) => v
            .as_str()
            .filter(|s| room::identifier(s, "participant").is_ok())
            .map(Some),
    };
    // A retained Room membership cannot revive a terminal Main or choose between
    // competing Main attempts. Recognize both producer Room spellings here.
    let mut active_owner = None;
    for event in attempts.iter().filter(|r| r["tool"] == "panel-main") {
        let refs = &event["refs"];
        if refs["panel_room_id"] != room && refs["panel_room"] != room {
            continue;
        }
        let principal = participant(event)?.or_else(|| event["attempt_id"].as_str())?;
        if principal != owner {
            continue;
        }
        if attempt_room(event) != AttemptRoom::Named(room)
            || event["terminal"] != false
            || event["provider"] != main["provider"]
            || refs["panel_role"] != "main"
            || active_owner
                .replace(event["attempt_id"].as_str()?)
                .is_some()
        {
            return None;
        }
    }
    let provider = lead["provider"].as_str()?;
    let role = lead["refs"].get("panel_role");
    let lead_member_id = if lead["tool"] == "panel-native-assignment" {
        if provider != "codex"
            || lead["refs"]["native_assignment_schema"] != "1"
            || role.is_some_and(|r| r != "lead")
            || participant(lead)?.is_some_and(|p| p != lead_id)
        {
            return None;
        }
        Some(lead_id)
    } else {
        if role?.as_str()? != "lead" || lead["tool"] == "panel-main" {
            return None;
        }
        participant(lead)?
    };
    if attempt_room(lead) != AttemptRoom::Named(room) || lead_member_id == Some(owner) {
        return None;
    }
    let lm = lead_member_id.and_then(member);
    if lm.is_some_and(|m| {
        m["joined"] != true
            || m["parent"] != owner
            || m["provider"] != provider
            || !matches!(m["role"].as_str(), Some("lead" | "worker"))
    }) {
        return None;
    }
    if let Some(parent_id) = parent(lead)? {
        if parent_id == lead_id || parent_id == worker_id || active_owner != Some(parent_id) {
            return None;
        }
    } else if lm.is_none() {
        return None;
    }
    let worker_room = attempt_room(worker);
    if !matches!(worker_room, AttemptRoom::Unspecified) && worker_room != AttemptRoom::Named(room) {
        return None;
    }
    let worker_member = participant(worker)?;
    if worker_member == Some(owner)
        || worker_member.is_some() && worker_member == lead_member_id
        || worker["tool"] == "panel-main"
    {
        return None;
    }
    let wm = worker_member.and_then(member);
    let explicit = parent(worker)?;
    if let Some(p) = explicit {
        if p != lead_id {
            return None;
        }
    } else if worker_room != AttemptRoom::Named(room) || wm.is_none_or(|m| m["joined"] != true) {
        return None;
    }
    let worker_role = worker["refs"].get("panel_role");
    if worker_role.is_some_and(|r| r != "worker")
        || worker_role.is_none() && (wm.is_none() || worker["tool"] == "panel-native-assignment")
    {
        return None;
    }
    if wm.is_some_and(|m| {
        m["role"] != "worker"
            || m["provider"] != worker["provider"]
            || lead_member_id.is_none()
            || m["parent"].as_str() != lead_member_id
    }) {
        return None;
    }
    Some((lead, worker))
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
    {
        return result_excerpt(
            attempt["attempt_id"].clone(),
            "native-report".into(),
            attempt["updated_at"].clone(),
            summary,
            results::MAX_DRAFT,
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
        results::MAX_RESULT,
    )
}

fn result_excerpt(
    attempt: Value,
    kind: Value,
    at: Value,
    full: &str,
    max_bytes: usize,
) -> Option<Value> {
    oms_scrub::outbound_page(full, max_bytes, oms_scrub::PageSelector::Offset(0)).ok()?;
    let text: String = full.chars().take(2000).collect();
    Some(
        json!({"attempt":attempt,"kind":kind,"at":at,"truncated":text.len()!=full.len(),"text":text}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_scope_requires_exact_ancestry_without_guessing_missing_room() {
        let main = json!({"participant":"m","room":"r","provider":"codex","state":"joined","participantsOmitted":0,"participants":[
            {"participant":"l","room":"r","parent":"m","role":"worker","joined":true,"provider":"codex"},
            {"participant":"w","room":"r","parent":"l","role":"worker","joined":false,"provider":"claude"}]});
        let m = json!({"attempt_id":"main","provider":"codex","tool":"panel-main","terminal":false,"refs":{"panel_role":"main","panel_room_id":"r","panel_room_participant":"m"}});
        let lead = json!({"attempt_id":"l","provider":"codex","tool":"panel-native-assignment","parent_attempt_id":"main","refs":{"panel_room":"r","native_assignment_schema":"1"}});
        let child = json!({"attempt_id":"worker","provider":"claude","tool":"peer-ask","parent_attempt_id":"l","terminal":true,"refs":{"panel_role":"worker","panel_room_participant":"w"}});
        let ok = |m: Value, l: Value, c: Value| {
            worker_scope(&[m, l, c], &main, "r", "l", "worker").is_some()
        };
        assert!(ok(m.clone(), lead.clone(), child.clone()));
        for (key, value) in [
            ("panel_room_id", json!("other")),
            ("panel_room", json!(null)),
            ("panel_role", json!("researcher")),
            ("panel_room_participant", json!("l")),
        ] {
            let mut wrong = child.clone();
            wrong["refs"][key] = value;
            assert!(!ok(m.clone(), lead.clone(), wrong));
        }
        let mut wrong = child.clone();
        wrong["parent_attempt_id"] = json!("elsewhere");
        assert!(!ok(m.clone(), lead.clone(), wrong));
        let mut wrong = lead.clone();
        wrong["parent_attempt_id"] = json!("other-main");
        assert!(!ok(m.clone(), wrong, child.clone()));
        let mut wrong = m.clone();
        wrong["terminal"] = json!(true);
        assert!(!ok(wrong, lead.clone(), child.clone()));
        assert!(
            worker_scope(&[m, lead, child.clone(), child], &main, "r", "l", "worker").is_none()
        );
    }

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

    #[test]
    fn recorded_result_scans_whole_body_before_cutting_excerpt() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("oms-detail-excerpt-{}-{nonce}", std::process::id()));
        let _cleanup = {
            struct Guard(std::path::PathBuf);
            impl Drop for Guard {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            Guard(root.clone())
        };
        let index = root.join(".oms/artifacts/index.jsonl");
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        let attempt = json!({"attempt_id":"bound","tool":"peer-ask","state":"done"});
        let write_indexed = |body: &str| {
            let raw = format!("## Output\n{body}\n## Exit\n0\n");
            std::fs::write(root.join(".oms/artifacts/answer.md"), raw.as_bytes()).unwrap();
            let row = json!({
                "kind":"ask",
                "attempt_id":"bound",
                "ts":"2026-11-26T00:00:00Z",
                "artifact":".oms/artifacts/answer.md",
                "artifact_sha256":oms_panel_core::digest(raw.as_bytes())
            });
            std::fs::write(&index, format!("{row}\n")).unwrap();
        };

        let clean = format!("## Answer\n{}{}", "c".repeat(2100), "d".repeat(300));
        write_indexed(&clean);
        let ok = recorded_result(&root, &[&attempt]).expect("clean excerpt");
        assert_eq!(ok["truncated"], true);
        assert_eq!(ok["text"].as_str().unwrap().chars().count(), 2000);

        let left = ["gh", "p_LeftHalf012345"].concat();
        let right = "RightHalf67890abcdef01";
        let header = "## Answer\n";
        let straddling = format!(
            "{header}{} {left}{right} {}",
            "a".repeat(2000 - header.chars().count() - 1 - left.chars().count()),
            "z".repeat(300)
        );
        write_indexed(&straddling);
        assert_eq!(recorded_result(&root, &[&attempt]), None);

        let machine = ["/", "ho", "me/fixture_account/work"].concat();
        let machine_straddling = format!(
            "{header}{} {machine} {}",
            "m".repeat(2000 - header.chars().count() - 4),
            "n".repeat(300)
        );
        write_indexed(&machine_straddling);
        assert_eq!(recorded_result(&root, &[&attempt]), None);

        let machine_beyond = format!("{header}{} {machine}", "m".repeat(2200));
        write_indexed(&machine_beyond);
        assert_eq!(recorded_result(&root, &[&attempt]), None);
        assert_eq!(
            result_excerpt(
                json!("native"),
                json!("native-report"),
                Value::Null,
                &machine_beyond,
                results::MAX_DRAFT
            ),
            None
        );
    }
}
