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

fn split_agy_effort_suffix(model: &str) -> Option<(&str, &'static str)> {
    let (base, suffix) = model.rsplit_once('-')?;
    if base.is_empty() {
        return None;
    }
    let effort = EFFORTS
        .iter()
        .copied()
        .find(|candidate| *candidate == suffix)?;
    Some((base, effort))
}

/// Whether `model` already ends in `-<effort>` for a known effort token.
pub fn is_agy_suffixed_selector(model: &str) -> bool {
    split_agy_effort_suffix(model).is_some()
}

/// Strip a known `-<effort>` suffix from an Antigravity selector, returning
/// `model` unchanged when no known effort suffix is present.
pub fn agy_base_model(model: &str) -> &str {
    split_agy_effort_suffix(model).map_or(model, |(base, _)| base)
}

/// Derive the exact Antigravity CLI selector `<base>-<effort>` from a base
/// model and effort. Fixed-effort models, already-suffixed selectors,
/// `provider-default`, and empty/auto/unknown efforts are returned unchanged.
pub fn agy_selector(model: &str, effort: &str) -> String {
    let effort = normalized_effort(effort);
    if model.is_empty()
        || model == "provider-default"
        || effort.is_empty()
        || !EFFORTS.contains(&effort)
        || fixed_effort_model("antigravity", model).is_some()
        || is_agy_suffixed_selector(model)
    {
        return model.to_owned();
    }
    format!("{model}-{effort}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgyCatalogRow<'a> {
    pub selector: &'a str,
    pub base_model: &'a str,
    pub effort: Option<&'static str>,
    pub label: Option<&'a str>,
}

fn label_agrees_with_effort(label: &str, effort: &str) -> bool {
    label
        .trim_end()
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once('('))
        .is_some_and(|(_, tag)| tag.trim().eq_ignore_ascii_case(effort))
}

/// Parse one AGY catalog row into its selector, base model, optional effort
/// (only when the selector ends in `-<effort>` for a known effort and the
/// trailing parenthesised label agrees), and optional display label.
pub fn agy_catalog_row(row: &str) -> Option<AgyCatalogRow<'_>> {
    let (selector, label) = if let Some((id, raw_label)) = row.split_once('\t') {
        let trimmed = raw_label.trim();
        if trimmed.is_empty() || raw_label.chars().any(char::is_control) {
            return None;
        }
        (id, Some(trimmed))
    } else {
        (row, None)
    };
    if selector.is_empty()
        || selector.len() > 256
        || !selector
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:".contains(&byte))
    {
        return None;
    }
    let (base_model, effort) = split_agy_effort_suffix(selector)
        .and_then(|(prefix, effort)| {
            label
                .filter(|text| label_agrees_with_effort(text, effort))
                .map(|_| (prefix, Some(effort)))
        })
        .unwrap_or((selector, None));
    Some(AgyCatalogRow {
        selector,
        base_model,
        effort,
        label,
    })
}

/// Exact AGY model selector from a bare ID or an ID followed by a display label.
pub fn agy_catalog_selector(row: &str) -> Option<&str> {
    agy_catalog_row(row).map(|parsed| parsed.selector)
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
        && Regex::new(
            r"^gemini-\d+(?:\.\d+)?-(?:neon|hydrogen)(?:-(?:low|medium|high|xhigh|max|ultra))?$",
        )
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
    if provider == "claude" && matches!(model, "opus" | "sonnet" | "fable" | "haiku") {
        return Some((model.to_owned(), vec![u64::MAX]));
    }
    let pattern = match provider {
        "codex" => r"^gpt-(\d+(?:\.\d+)?)-(astra|sol|terra|luna)$",
        "claude" => r"^claude-(opus|sonnet|fable|haiku)-(\d+(?:-\d+)*)$",
        "antigravity" => r"^gemini-(\d+(?:\.\d+)?)-([a-z][a-z0-9]*(?:-[a-z0-9]+)*)$",
        _ => return None,
    };
    let regex = Regex::new(pattern).ok()?;
    let found = regex.captures(model)?;
    let (family, number) = if provider == "claude" {
        (found[1].to_owned(), &found[2])
    } else if provider == "antigravity" {
        if EFFORTS.contains(&&found[2]) {
            return None;
        }
        (found[2].to_owned(), &found[1])
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
