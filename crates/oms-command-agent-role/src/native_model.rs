//! Native CLI request validation shared by role rendering, panel routing and peer
//! argv construction. Token acceptance is not model entitlement or served effort.
//! Reported model support comes from catalog evidence with scope checked by the caller.
use regex::Regex;

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
pub const AGY_FIXED_EFFORT_MODELS: [&str; 2] =
    ["gemini-pro-orchestrator", "gemini-pro-orchestrator-argon"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffortSupport {
    Fixed,
    ProviderDefault,
    ReportedSupported,
    ReportedUnsupported,
    Unknown,
}

pub fn normalized_effort(effort: &str) -> &str {
    if effort == "auto" { "" } else { effort }
}

/// Whether the current native transport accepts an invocation-level model ID.
/// This does not establish model availability or account access.
pub fn supports_exact_model_override(provider: &str) -> bool {
    !matches!(provider, "deepseek" | "vibe" | "openhands")
}

pub fn fixed_effort_model(provider: &str, model: &str) -> Option<&'static str> {
    if provider != "antigravity" {
        return None;
    }
    AGY_FIXED_EFFORT_MODELS
        .into_iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(model))
}

/// Exact AGY model selector from a bare ID or an ID followed by a display label.
pub fn agy_catalog_selector(row: &str) -> Option<&str> {
    let id = if let Some((id, label)) = row.split_once('\t') {
        if label.trim().is_empty() || label.chars().any(char::is_control) {
            return None;
        }
        id
    } else {
        row
    };
    (!id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:".contains(&byte)))
    .then_some(id)
}

/// Validate the existing native transport vocabulary without inventing a model
/// capability list. Unknown model IDs remain usable by explicit native callers;
/// managed role routes separately require a known family or exact adapter ID.
pub fn validate_effort(provider: &str, model: &str, effort: &str) -> Result<(), &'static str> {
    let effort = normalized_effort(effort);
    if !effort.is_empty() && !EFFORTS.contains(&effort) {
        return Err("invalid reasoning effort");
    }
    if effort.is_empty() {
        return Ok(());
    }
    if fixed_effort_model(provider, model).is_some() {
        return Err(
            "Antigravity Orchestrator has fixed effort; omit --reasoning-effort or select an adjustable model",
        );
    }
    if provider == "claude" && effort == "ultra" {
        return Err("Claude does not support the requested effort");
    }
    // The current adjustable Neon/Hydrogen native contract has four levels.
    // An unknown profile is not classified from a provider-wide catalog label.
    if provider == "antigravity"
        && matches!(effort, "max" | "ultra")
        && Regex::new(r"^gemini-\d+(?:\.\d+)?-(?:neon|hydrogen)(?:-(?:low|medium|high|xhigh))?$")
            .expect("static native model pattern")
            .is_match(model)
    {
        return Err("Antigravity Neon/Hydrogen does not support the requested effort");
    }
    Ok(())
}

/// The caller must exclude reports with mismatched or unproved account/endpoint
/// scope or lifetime. An unscoped exact-model report remains a source claim,
/// never entitlement; provider-level effort labels are insufficient.
pub fn effort_support(
    provider: &str,
    model: &str,
    effort: &str,
    reported: Option<&[String]>,
) -> EffortSupport {
    if fixed_effort_model(provider, model).is_some() {
        return EffortSupport::Fixed;
    }
    let effort = normalized_effort(effort);
    if effort.is_empty() {
        return EffortSupport::ProviderDefault;
    }
    match reported.filter(|values| !values.is_empty()) {
        Some(values) if values.iter().any(|value| value == effort) => {
            EffortSupport::ReportedSupported
        }
        Some(_) => EffortSupport::ReportedUnsupported,
        None => EffortSupport::Unknown,
    }
}

pub fn route_family(provider: &str, model: &str) -> Option<(String, Vec<u64>)> {
    if let Some(fixed) = fixed_effort_model(provider, model) {
        return Some((fixed.to_owned(), vec![]));
    }
    let pattern = match provider {
        "codex" => r"^gpt-(\d+(?:\.\d+)?)-(astra|sol|terra|luna)$",
        "claude" => r"^claude-(opus|sonnet|fable|haiku)-(\d+(?:-\d+)*)$",
        "antigravity" => {
            r"^gemini-(\d+(?:\.\d+)?)-([a-z][a-z0-9]*(?:-[a-z0-9]+)*)-(low|medium|high|xhigh)$"
        }
        _ => return None,
    };
    let regex = Regex::new(pattern).ok()?;
    let found = regex.captures(model)?;
    let (family, number) = if provider == "claude" {
        (found[1].to_owned(), &found[2])
    } else if provider == "antigravity" {
        (format!("{}-{}", &found[2], &found[3]), &found[1])
    } else {
        (found[2].to_owned(), &found[1])
    };
    let mut version: Vec<u64> = number
        .split(['.', '-'])
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    while version.len() > 1 && version.last() == Some(&0) {
        version.pop();
    }
    Some((family, version))
}

pub fn valid_role_model(provider: &str, model: &str) -> bool {
    if ["pi", "opencode"].contains(&provider) {
        return model.split_once('/').is_some_and(|(prefix, tail)| {
            !prefix.is_empty()
                && !tail.is_empty()
                && !model.starts_with('-')
                && !model.chars().any(|c| c.is_whitespace() || c.is_control())
        });
    }
    route_family(provider, model)
        .is_some_and(|(_, version)| provider != "codex" || version.first().is_some_and(|v| *v >= 6))
}
