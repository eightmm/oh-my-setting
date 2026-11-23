//! Shared interpretation of bounded CLI probes. Callers own process execution,
//! binary identity checks, catalog collection, cache locking, and report severity.
//! Help evidence describes a CLI surface, not account entitlement or served effort.

use regex::Regex;
use std::{collections::BTreeMap, sync::OnceLock};

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

pub fn inspect(provider: &str, version_code: i32, help_code: i32, help: &[u8]) -> Evidence {
    let text = String::from_utf8_lossy(help);
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
        static PATTERN: OnceLock<Regex> = OnceLock::new();
        let pattern = PATTERN.get_or_init(|| {
            Regex::new(r"\(([a-z][a-z0-9]*(?:\s*[,|]\s*[a-z][a-z0-9]*)+)\)")
                .expect("static effort pattern")
        });
        let lines = text.lines().collect::<Vec<_>>();
        for (i, line) in lines.iter().enumerate() {
            if line.contains(control) {
                let window = lines[i..lines.len().min(i + 3)].join(" ");
                if let Some(found) = pattern.captures(&window) {
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

pub fn cache_text(fields: &BTreeMap<String, String>) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}
