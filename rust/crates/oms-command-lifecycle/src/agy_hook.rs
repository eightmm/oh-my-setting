use crate::{
    cli::Options,
    runtime::{self as r, Result},
};
use serde_json::json;
use std::{io::Read, path::Path, sync::atomic::AtomicBool};
pub(crate) fn run(
    cwd: &Path,
    source: &Path,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if !matches!(options.action.as_str(), "preinvocation" | "stop") {
        return r::fail("agy-hook requires preinvocation or stop");
    }
    let empty = || r::json(&json!({}));
    if std::env::var_os("OH_MY_SETTING_AGY_HOOKS").is_some_and(|s| s == "0")
        || std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|s| s == "1")
    {
        return empty();
    }
    let mut raw = Vec::new();
    std::io::stdin()
        .lock()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 1024 * 1024 {
        return empty();
    }
    let Ok(payload) = r::parse(&raw) else {
        return empty();
    };
    let Some(repo) = payload["workspacePaths"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|p| p.as_str())
        .map(Path::new)
        .filter(|p| p.is_absolute() && p.is_dir())
    else {
        return empty();
    };
    let first = options.action == "preinvocation";
    let count = payload[if first {
        "invocationNum"
    } else {
        "executionNum"
    }]
    .as_i64()
    .unwrap_or(0);
    if first && (count > 1 || std::env::var_os("OMS_SKILL_ROUTER_OFF").is_some_and(|s| s == "1")) {
        return empty();
    }
    let session = payload["conversationId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("nosession");
    let adapted = json!({"cwd":repo,"session_id":session,"turn_id":format!("{session}:{count}"),"hook_event_name":if first{"PreInvocation"}else{"Stop"},"model":payload["modelName"].as_str().unwrap_or(""),"prompt":""});
    let input = r::bytes(&adapted)?;
    let mut diagnostics = Vec::new();
    if first {
        let out = services.provider_hook("skill-router", &input, repo, source, true, flag);
        if out.0 != 0 {
            diagnostics.extend_from_slice(b"native Antigravity router unavailable\n");
        }
        let hint = String::from_utf8(out.1).unwrap_or_default();
        let mut result = if out.0 == 0 && !hint.trim().is_empty() {
            r::json(&json!({"injectSteps":[{"ephemeralMessage":hint.trim()}]}))?
        } else {
            empty()?
        };
        result.2 = diagnostics;
        Ok(result)
    } else {
        for name in ["turn-guard", "telemetry-hook"] {
            let out = services.provider_hook(name, &input, repo, source, true, flag);
            if out.0 != 0 {
                diagnostics.extend_from_slice(b"native Antigravity observer unavailable\n");
            }
        }
        let mut result = empty()?;
        result.2 = diagnostics;
        let _ = cwd;
        Ok(result)
    }
}
