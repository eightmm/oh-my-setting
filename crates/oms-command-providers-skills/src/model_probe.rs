//! Shared interpretation of bounded CLI probes. Callers own process execution,
//! binary identity checks, catalog collection, cache locking, and report severity.
//! Help evidence describes a CLI surface, not account entitlement or served effort.

use oms_command_agent_role::native_model::{EFFORTS, agy_catalog_row, fixed_effort_model};
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const SCALE_ORDER: &[&str] = &[
    "none", "off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

pub fn version_args(provider: &str) -> &'static [&'static str] {
    if provider == "grok" {
        &["--no-auto-update", "version"]
    } else {
        &["--version"]
    }
}

pub fn help_args(provider: &str) -> &'static [&'static str] {
    match provider {
        "codex" | "muse" => &["exec", "--help"],
        "kiro" => &["chat", "--help"],
        "goose" | "kilo" => &["run", "--help"],
        "grok" => &["--no-auto-update", "--help"],
        _ => &["--help"],
    }
}

pub struct Evidence {
    pub version_ok: bool,
    pub reachable: bool,
    pub effort_mechanism: &'static str,
    pub effort_values: Vec<String>,
}

impl Evidence {
    pub fn absent() -> Self {
        Self {
            version_ok: false,
            reachable: false,
            effort_mechanism: "absent",
            effort_values: vec![],
        }
    }

    pub fn usable(&self) -> bool {
        self.version_ok && self.reachable
    }

    /// Preserve the existing line-oriented cache keys. Catalog status and binary
    /// identity are supplied by their owners and must not be inferred here.
    pub fn cache_fields(&self, observed_at: u64) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("effort_mechanism".into(), self.effort_mechanism.into()),
            ("effort_values".into(), self.effort_values.join(" ")),
            ("probed_at".into(), observed_at.to_string()),
        ])
    }
}

pub fn inspect(
    provider: &str,
    version_code: i32,
    help_code: i32,
    help_out: &[u8],
    help_err: &[u8],
) -> Evidence {
    let mut combined = Vec::with_capacity(help_out.len() + help_err.len() + 1);
    combined.extend_from_slice(help_out);
    if !help_out.is_empty() && !help_out.ends_with(b"\n") && !help_err.is_empty() {
        combined.push(b'\n');
    }
    combined.extend_from_slice(help_err);
    let text = String::from_utf8_lossy(&combined);
    let control = match provider {
        "claude" | "antigravity" | "grok" | "copilot" | "kiro" => "--effort",
        "pi" | "cline" => "--thinking",
        "droid" | "aider" | "muse" => "--reasoning-effort",
        "kilo" => "--variant",
        _ => "",
    };
    let mechanism = if provider == "codex" {
        "config"
    } else if !control.is_empty() && text.contains(control) {
        "flag"
    } else {
        "none"
    };
    let mut values = if mechanism == "none" {
        vec![]
    } else {
        match provider {
            "codex" | "claude" | "antigravity" | "grok" | "aider" | "muse" | "kiro" => {
                vec!["low", "medium", "high"]
            }
            "cline" => vec!["none", "low", "medium", "high", "xhigh"],
            "pi" | "copilot" | "droid" => vec!["low", "medium", "high", "xhigh", "max"],
            _ => vec![],
        }
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    if mechanism == "flag" {
        static PAREN_PATTERN: OnceLock<Regex> = OnceLock::new();
        static COLON_PATTERN: OnceLock<Regex> = OnceLock::new();
        let paren_pattern = PAREN_PATTERN.get_or_init(|| {
            Regex::new(r"\(([a-z][a-z0-9]*(?:\s*[,|]\s*[a-z][a-z0-9]*)+)\)")
                .expect("static effort pattern")
        });
        let colon_pattern = COLON_PATTERN.get_or_init(|| {
            Regex::new(r"\b(?i:levels?)\s*:\s*([a-z][a-z0-9]*(?:\s*[,|]\s*[a-z][a-z0-9]*)+)")
                .expect("static effort level pattern")
        });
        let lines = text.lines().collect::<Vec<_>>();
        for (i, line) in lines.iter().enumerate() {
            if let Some(pos) = line.find(control) {
                let mut window_lines = vec![*line];
                for next in lines.iter().skip(i + 1).take(2) {
                    if next.trim_start().starts_with('-') {
                        break;
                    }
                    window_lines.push(*next);
                }
                let window = window_lines.join(" ");
                let tail = &window[pos + control.len()..];
                let found = [paren_pattern.captures(tail), colon_pattern.captures(tail)]
                    .into_iter()
                    .flatten()
                    .min_by_key(|capture| capture.get(0).map_or(usize::MAX, |m| m.start()));
                if let Some(found) = found {
                    values = found[1]
                        .split([',', '|'])
                        .map(str::trim)
                        .map(str::to_owned)
                        .collect();
                    break;
                }
            }
        }
    }
    Evidence {
        version_ok: version_code == 0,
        reachable: help_code == 0,
        effort_mechanism: mechanism,
        effort_values: values,
    }
}

pub fn provider_family(provider: &str) -> &'static str {
    match provider {
        "codex" => "openai",
        "claude" => "anthropic",
        "antigravity" | "gemini" => "google",
        "grok" => "xai",
        "qwen" => "alibaba",
        "deepseek" => "deepseek",
        "vibe" => "mistral",
        "muse" => "meta",
        "kimi" => "moonshot",
        _ => "unknown",
    }
}

pub fn clean_catalog_rows(provider: &str, raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    if provider == "antigravity" {
        return raw
            .lines()
            .filter_map(agy_catalog_row)
            .filter(|row| seen.insert(row.selector))
            .map(|row| match row.label {
                Some(label) => format!("{}\t{label}", row.selector),
                None => row.selector.to_owned(),
            })
            .take(500)
            .collect();
    }
    raw.lines()
        .filter_map(|line| {
            let selector = line.split('\t').next().unwrap_or("").trim();
            (!selector.is_empty()
                && selector != "Fetching available models..."
                && selector.len() <= 256
                && !selector.chars().any(char::is_control)
                && seen.insert(selector))
            .then(|| selector.to_owned())
        })
        .take(500)
        .collect()
}

pub fn catalog_selectors(provider: &str, raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    if provider == "antigravity" {
        return raw
            .lines()
            .filter_map(agy_catalog_row)
            .filter(|row| seen.insert(row.selector))
            .map(|row| row.selector.to_owned())
            .collect();
    }
    raw.lines()
        .filter_map(|line| {
            let selector = line.split('\t').next().unwrap_or("").trim();
            (!selector.is_empty()
                && selector != "Fetching available models..."
                && selector.len() <= 256
                && !selector.chars().any(char::is_control)
                && seen.insert(selector))
            .then(|| selector.to_owned())
        })
        .collect()
}

pub fn routable_models(models: &[String], provider: &str) -> Vec<String> {
    static CODEX_ROLE: OnceLock<Regex> = OnceLock::new();
    let role = CODEX_ROLE.get_or_init(|| {
        Regex::new(r"^gpt-([0-9]+)(?:\.([0-9]+))?-(astra|sol|terra|luna)$")
            .expect("static Codex family pattern")
    });
    let tiered = provider == "codex" && models.iter().any(|m| role.is_match(m));
    fn generation(model: &str) -> Option<(u64, u64)> {
        let mut first = None;
        let mut second = 0;
        for segment in model
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
        {
            if !segment.bytes().all(|b| b.is_ascii_digit()) {
                if first.is_some() {
                    break;
                }
                continue;
            }
            let value = segment.parse::<u64>().ok()?;
            if first.is_none() {
                first = Some(value);
            } else {
                second = value;
                break;
            }
        }
        first.map(|a| (a, second))
    }
    fn inferred(model: &str) -> &'static str {
        let model = model.to_ascii_lowercase();
        if model.contains("claude")
            || ["opus", "sonnet", "haiku", "fable"]
                .iter()
                .any(|s| model.starts_with(s))
        {
            "anthropic"
        } else if model.contains("gemini") {
            "google"
        } else if model.contains("gpt-")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
        {
            "openai"
        } else if model.contains("grok") {
            "xai"
        } else if model.contains("qwen") {
            "alibaba"
        } else if model.contains("deepseek") {
            "deepseek"
        } else if model.contains("mistral") {
            "mistral"
        } else if model.contains("llama") {
            "meta"
        } else if model.contains("kimi") {
            "moonshot"
        } else {
            "unknown"
        }
    }
    let mut candidates = Vec::new();
    let mut newest = BTreeMap::new();
    let mut major = 0;
    for model in models {
        let model_family = if tiered {
            let Some(capture) = role.captures(model) else {
                continue;
            };
            format!("codex-{}", &capture[3])
        } else {
            let mut detected = inferred(model);
            let primary = provider_family(provider);
            if detected == "unknown" {
                detected = primary;
            }
            if primary != "unknown" && detected != primary {
                continue;
            }
            detected.into()
        };
        let version = generation(model);
        if let Some(version) = version {
            if tiered {
                major = major.max(version.0);
            }
            newest
                .entry(model_family.clone())
                .and_modify(|v: &mut (u64, u64)| *v = (*v).max(version))
                .or_insert(version);
        }
        candidates.push((model, model_family, version));
    }
    candidates
        .into_iter()
        .filter(|(model, family, version)| {
            fixed_effort_model(provider, model).is_some()
                || (newest
                    .get(family)
                    .is_none_or(|latest| version.as_ref() == Some(latest))
                    && (!tiered || version.is_some_and(|v| v.0 == major)))
        })
        .map(|(m, _, _)| m.clone())
        .collect()
}

pub fn parse_effort_scales(raw: &str) -> BTreeMap<String, Vec<String>> {
    let mut scales = BTreeMap::new();
    for line in raw.lines() {
        if let Some((name, scale)) = line.split_once('\t') {
            let values = scale
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if !name.is_empty() && !values.is_empty() {
                scales.insert(name.to_owned(), values);
            }
        }
    }
    scales
}

pub fn union_effort_scales(scales: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut extras = Vec::new();
    for values in scales.values() {
        for value in values {
            if seen.insert(value.as_str()) && !SCALE_ORDER.contains(&value.as_str()) {
                extras.push(value.clone());
            }
        }
    }
    let mut ordered = SCALE_ORDER
        .iter()
        .filter(|effort| seen.contains(**effort))
        .map(|effort| (*effort).to_owned())
        .collect::<Vec<_>>();
    ordered.extend(extras);
    ordered
}

/// Base model -> effort -> exact catalog selector.
pub type AgySelectors = BTreeMap<String, BTreeMap<String, String>>;

pub fn agy_effort_scales_and_selectors(raw: &str) -> (BTreeMap<String, Vec<String>>, AgySelectors) {
    let mut seen_selectors = BTreeSet::new();
    let mut selectors = BTreeMap::<String, BTreeMap<String, String>>::new();
    for row in raw.lines().filter_map(agy_catalog_row) {
        if !seen_selectors.insert(row.selector) {
            continue;
        }
        if let Some(effort) = row.effort {
            selectors
                .entry(row.base_model.to_owned())
                .or_default()
                .entry(effort.to_owned())
                .or_insert_with(|| row.selector.to_owned());
        }
    }
    let scales = selectors
        .iter()
        .map(|(base, map)| {
            let ordered = EFFORTS
                .iter()
                .filter(|effort| map.contains_key(**effort))
                .map(|effort| (*effort).to_owned())
                .collect();
            (base.clone(), ordered)
        })
        .collect();
    (scales, selectors)
}

pub fn cache_text(fields: &BTreeMap<String, String>) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}
