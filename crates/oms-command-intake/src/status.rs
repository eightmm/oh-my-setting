//! Read-only `oms intake status`: adapter environment readiness and per-route Room health.
//!
//! Environment values are never printed, only whether each is set, missing or invalid. A route's
//! last delivery is the newest intake question the route's participant appended to its Room
//! recipient; it names the Room target, so routes sharing one target report the same message,
//! and refused, duplicate or unrouted sessions leave none.

use crate::routes::Route;
use crate::serve::validate_route_room;
use crate::source::{EnvVarSpec, compiled_sources};
use oms_command_room::{Room, Services as RoomServices, Session as RoomSession};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::atomic::AtomicBool;

fn env_state(spec: &EnvVarSpec) -> &'static str {
    match std::env::var_os(spec.name) {
        None => "missing",
        // Endpoint overrides treat an empty value as unset (`EndpointUrl::from_env`).
        Some(raw) if raw.is_empty() && !spec.sensitive => "missing",
        Some(raw) => match raw.into_string() {
            Ok(text)
                if !text.is_empty()
                    && !(spec.sensitive
                        && text.chars().any(|c| c.is_whitespace() || c.is_control())) =>
            {
                "set"
            }
            _ => "invalid",
        },
    }
}

fn env_report(specs: &[EnvVarSpec]) -> Value {
    let env: Vec<_> = specs
        .iter()
        .map(|s| json!({ "name": s.name, "required": s.required, "state": env_state(s) }))
        .collect();
    let ready = specs
        .iter()
        .zip(&env)
        .all(|(s, e)| e["state"] == "set" || (!s.required && e["state"] == "missing"));
    json!({ "ready": ready, "env": env })
}

fn last_delivery(
    route: &Route,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> Result<Value, String> {
    validate_route_room(route, room_services, interrupted)?;
    let room = Room::open(
        &route.repo,
        RoomSession::from_env(),
        room_services,
        interrupted,
        false,
        false,
    )
    .map_err(|e| format!("room unavailable: {}", e.message))?;
    let messages = room
        .messages(&route.room)
        .map_err(|e| format!("room '{}' unavailable: {}", route.room, e.message))?;
    let prefixes = [
        format!("{}-created-", route.source),
        format!("{}-prompted-", route.source),
    ];
    Ok(messages
        .iter()
        .rev()
        .find(|m| {
            m.text("sender") == route.participant
                && m.text("recipient") == route.to
                && m.text("message_kind") == "question"
                && prefixes
                    .iter()
                    .any(|p| m.text("id").starts_with(p.as_str()))
        })
        .map_or(
            Value::Null,
            |m| json!({ "id": m.text("id"), "ts": m.text("ts") }),
        ))
}

fn route_report(route: &Route, room_services: &dyn RoomServices, stop: &AtomicBool) -> Value {
    let (last, error) = match last_delivery(route, room_services, stop) {
        Ok(last) => (last, Value::Null),
        Err(msg) => (Value::Null, msg.into()),
    };
    json!({
        "source": route.source,
        "team": route.team,
        "repo": route.repo.to_string_lossy(),
        "room": route.room,
        "participant": route.participant,
        "to": route.to,
        "ok": error.is_null(),
        "error": error,
        "last_delivery": last,
    })
}

pub fn status_report(
    routes: &[Route],
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> Value {
    let sources: Vec<_> = compiled_sources()
        .iter()
        .map(|s| {
            json!({
                "source": s.name(),
                "serve": env_report(s.serve_env()),
                "reply": env_report(s.reply_env()),
            })
        })
        .collect();
    let routes: Vec<_> = routes
        .iter()
        .map(|r| route_report(r, room_services, interrupted))
        .collect();
    json!({ "sources": sources, "routes": routes })
}

pub fn render_status(report: &Value, json_mode: bool) -> Vec<u8> {
    if json_mode {
        let mut out = serde_json::to_vec(report).unwrap_or_default();
        out.push(b'\n');
        return out;
    }
    let mut out = String::new();
    let side = |r: &Value| {
        let env = r["env"].as_array().map_or_else(String::new, |env| {
            env.iter()
                .map(|e| {
                    let optional = if e["required"] == true {
                        ""
                    } else {
                        ", optional"
                    };
                    // Not NAME=state: the outbound scrubber reads that as a credential assignment.
                    format!(
                        "{} ({}{optional})",
                        e["name"].as_str().unwrap_or(""),
                        e["state"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        });
        let ready = if r["ready"] == true {
            "ready"
        } else {
            "not ready"
        };
        format!("{ready} ({env})")
    };
    for s in report["sources"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "source {}:", s["source"].as_str().unwrap_or(""));
        let _ = writeln!(out, "  serve env: {}", side(&s["serve"]));
        let _ = writeln!(out, "  reply env: {}", side(&s["reply"]));
    }
    let routes = report["routes"].as_array().map_or(&[][..], Vec::as_slice);
    if routes.is_empty() {
        out.push_str("no intake routes\n");
    }
    for r in routes {
        let text = |k: &str| r[k].as_str().unwrap_or("").to_owned();
        let state = match (&r["error"], &r["last_delivery"]) {
            (Value::String(e), _) => format!("error: {e}"),
            (_, Value::Null) => "ok, no delivery yet".to_owned(),
            (_, last) => format!(
                "ok, last delivery {} at {}",
                last["id"].as_str().unwrap_or(""),
                last["ts"].as_str().unwrap_or("")
            ),
        };
        let _ = writeln!(
            out,
            "route {} {} -> {} room={}: {state}",
            text("source"),
            text("team"),
            text("repo"),
            text("room")
        );
    }
    out.into_bytes()
}
