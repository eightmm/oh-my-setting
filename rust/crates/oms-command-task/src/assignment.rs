//! Native task-assignment adapters for model-routing, agent-plan, goal-drive,
//! autopilot and plan-from-spec. These functions validate data; they do not
//! select providers, launch workers, or inherit another provider's route.
use crate::{
    Error, Result,
    json::{copy, field, object, string, text},
};
use oms_context::Json;
use regex::Regex;
use std::sync::OnceLock;
use unicode_general_category::{GeneralCategory, get_general_category};

const FIELDS: &[&str] = &[
    "provider",
    "model",
    "fallback_model",
    "reasoning_effort",
    "workload",
];
const ALIASES: &[&str] = &[
    "agy",
    "cursor-agent",
    "grok-build",
    "gemini-cli",
    "qwen-code",
    "opencode2",
    "dsh",
    "deepseek-harness",
    "mistral-vibe",
    "github-copilot",
    "factory-droid",
    "kiro-cli",
    "cn",
];
fn fail(message: impl Into<String>) -> Error {
    Error::new(2, message.into())
}
fn get<'a>(v: &'a Json, key: &str, default: &'a str) -> &'a str {
    text(field(v, key))
        .and_then(|v| std::str::from_utf8(v).ok())
        .unwrap_or(default)
}
/// Return the complete validated assignment, preserving field order and values.
/// An empty object means no override; any nonempty object must name provider.
pub fn validate(value: &Json) -> Result<Json> {
    let Json::Object(items) = value else {
        return Err(fail("assignment must be an object"));
    };
    if items.is_empty() {
        return Ok(object([] as [(&str, Json); 0]));
    }
    if items
        .iter()
        .any(|(k, _)| !FIELDS.iter().any(|name| name.as_bytes() == k))
        || !items.iter().any(|(k, _)| k == b"provider")
    {
        return Err(fail(
            "assignment requires provider and only model/fallback_model/reasoning_effort/workload",
        ));
    }
    for (key, v) in items {
        let valid = text(v)
            .and_then(|v| std::str::from_utf8(v).ok())
            .is_some_and(|s| {
                s.chars().all(|ch| {
                    !matches!(
                        get_general_category(ch),
                        GeneralCategory::Control
                            | GeneralCategory::Format
                            | GeneralCategory::Surrogate
                    )
                })
            });
        if !valid {
            return Err(fail(format!(
                "assignment {} must be text without control characters",
                String::from_utf8_lossy(key)
            )));
        }
    }
    let provider = get(value, "provider", "");
    if provider.is_empty()
        || provider.len() > 64
        || !provider.as_bytes()[0].is_ascii_lowercase() && !provider.as_bytes()[0].is_ascii_digit()
        || !provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        || provider.contains("..")
        || ALIASES.contains(&provider)
    {
        return Err(fail(
            "assignment provider must be a canonical transport identifier",
        ));
    }
    for key in ["model", "fallback_model"] {
        let model = get(value, key, "");
        if model.chars().count() > 160
            || model.starts_with('-')
            || model != model.trim()
            || model == "provider-default"
        {
            return Err(fail(format!("invalid assignment {key}")));
        }
    }
    if !["standard", "routine"].contains(&get(value, "workload", "standard")) {
        return Err(fail("assignment workload must be standard or routine"));
    }
    if !["auto", "low", "medium", "high", "xhigh", "max", "ultra"].contains(&get(
        value,
        "reasoning_effort",
        "auto",
    )) {
        return Err(fail("invalid assignment reasoning_effort"));
    }
    Ok(copy(value))
}
pub fn resolve(task: &Json, defaults: &Json) -> Result<Json> {
    if !matches!(task, Json::Object(_)) {
        return Err(fail("task must be an object"));
    }
    let empty = object([] as [(&str, Json); 0]);
    let assignment = validate(if matches!(field(task, "assignment"), Json::Null) {
        // Explicit null is not the same as an absent assignment.
        if let Json::Object(items) = task
            && items.iter().any(|(k, _)| k == b"assignment")
        {
            return Err(fail("assignment must be an object"));
        }
        &empty
    } else {
        field(task, "assignment")
    })?;
    let source = if matches!(&assignment,Json::Object(v) if !v.is_empty()) {
        &assignment
    } else {
        defaults
    };
    let mut output = vec![
        (b"model".to_vec(), string("")),
        (b"fallback_model".to_vec(), string("")),
        (b"reasoning_effort".to_vec(), string("auto")),
        (b"workload".to_vec(), string("standard")),
    ];
    if let Json::Object(items) = source {
        for (k, v) in items {
            if let Some(i) = output.iter().position(|(key, _)| key == k) {
                output[i].1 = copy(v);
            } else {
                output.push((k.clone(), copy(v)));
            }
        }
    }
    Ok(Json::Object(output))
}
pub fn fields(route: &Json) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (i, key) in FIELDS.iter().enumerate() {
        if i > 0 {
            out.push(0x1f);
        }
        out.extend_from_slice(
            text(field(route, key)).ok_or_else(|| fail("invalid assignment route"))?,
        );
    }
    out.push(b'\n');
    Ok(out)
}
pub fn validate_collaboration(task: &Json, worker: &str) -> Result<()> {
    if !["codex", "claude"].contains(&worker) {
        return Err(fail("auto collaboration worker must be codex or claude"));
    }
    let route = resolve(task, &object([] as [(&str, Json); 0]))?;
    let explicit = matches!(field(task,"assignment"),Json::Object(v) if !v.is_empty());
    if explicit && get(&route, "provider", "") != worker {
        return Err(fail(
            "auto collaboration assignments must use the campaign worker",
        ));
    }
    let provider = get(task, "provider", "");
    if !provider.is_empty() && provider != worker {
        return Err(fail(
            "auto collaboration plan contains another implementation provider",
        ));
    }
    if explicit && worker == "codex" {
        static ROUTE: OnceLock<Regex> = OnceLock::new();
        let re = ROUTE.get_or_init(|| {
            Regex::new(r"\Agpt-0*(?:[6-9]|[1-9]\d+)(?:\.\d+)?-(?:astra|sol|terra|luna)\z").unwrap()
        });
        let fallback = get(&route, "fallback_model", "");
        if !re.is_match(get(&route, "model", "")) || !fallback.is_empty() && !re.is_match(fallback)
        {
            return Err(fail(
                "auto collaboration assignment requires a GPT-6 or newer Astra, Sol, Terra or Luna route",
            ));
        }
    }
    Ok(())
}
pub fn validate_collaboration_plan(plan: &Json, worker: &str) -> Result<()> {
    let Json::Object(tasks) = field(plan, "tasks") else {
        return Err(fail("plan tasks must be an object"));
    };
    for (_, task) in tasks {
        validate_collaboration(task, worker)?;
    }
    Ok(())
}
