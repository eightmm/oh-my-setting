//! Read-only task routing advice: kind x risk x size -> recommended tier and effort.
use crate::{Error, Output, Result, Services, cli::Options, json, require};
use oms_panel_core::{Value, services::ExecutionRequest};
use regex::Regex;
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const KINDS: &[&str] = &[
    "explore",
    "mechanical-edit",
    "implement",
    "debug",
    "design",
    "review",
    "summarize",
];
const RISKS: &[&str] = &["low", "medium", "high"];
const SIZES: &[&str] = &["small", "medium", "large"];
const EFFORTS: &[&str] = &["low", "medium", "high"];
const TIERS: &[&str] = &["light", "routine", "deep"];
const MAX_PATCH: usize = 4 * 1024 * 1024;
const FRONTIER_PATTERNS: &[&str] = &[
    "plugins/*",
    "hooks/*",
    "install.sh",
    "uninstall.sh",
    "scripts/install*",
    "scripts/uninstall*",
    "scripts/*hook*",
    "scripts/lib/*hook*",
    "scripts/lib/pi-oms-extension.mjs",
    "rust/crates/oms-command-providers-skills/assets/pi-oms-extension.mjs",
    "rust/crates/oms-command-providers-skills/src/harness.rs",
    "scripts/land.sh",
    "scripts/patch-land.sh",
    "scripts/patch-admit.sh",
    "scripts/lib/landing-capture.py",
    "scripts/provider-permissions.sh",
    ".github/workflows/*",
    "rules/*",
    "rust/crates/oms/src/*",
    "rust/crates/oms-command-hooks/*",
    "rust/crates/oms-command-lifecycle/*",
    "rust/crates/oms-command-landing/*",
    "*permission*",
    "*settings*",
    "*policy*",
];
const ON_OPEN_PATTERNS: &[&str] = &[
    ".vscode/tasks.json",
    ".envrc",
    ".husky/*",
    ".githooks/*",
    ".lefthook/*",
    "lefthook.y*ml",
    ".lefthook.y*ml",
    "lefthook-local.y*ml",
    ".lefthook-local.y*ml",
    ".overcommit.yml",
    ".pre-commit-config.yaml",
    ".devcontainer.json",
    ".devcontainer/*",
    ".gitlab-ci.yml",
    ".circleci/*",
    ".buildkite/*",
    ".azure-pipelines/*",
    "azure-pipelines.y*ml",
    "Jenkinsfile",
    ".github/actions/*",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
    ".pnpmfile.cjs",
];

/// Validate and recommend tier/effort from task routing rules without launching a model.
pub fn run(
    execution: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let kind = options.required("recommend")?;
    require(
        KINDS.contains(&kind.as_str()),
        &format!("--recommend must be one of: {}", KINDS.join(", ")),
    )?;
    let has_manual = options.has("risk") || options.has("size");
    let has_diff = options.has("diff");
    require(
        !(has_manual && has_diff) && (has_diff || (options.has("risk") && options.has("size"))),
        "--recommend needs --diff RANGE, or both --risk and --size",
    )?;
    let failed = if options.has("failed") {
        Some(options.integer("failed", 0, 0, usize::MAX)?)
    } else {
        None
    };

    let table = load_table(source)?;
    let problems = lint(&table);
    require(
        problems.is_empty(),
        &format!("task routing rules are invalid: {}", problems.join("; ")),
    )?;

    let measured = if let Some(range) = options.get("diff") {
        Some(inspect_range(execution, range, &table, services, stop)?)
    } else {
        None
    };
    let risk = options
        .get("risk")
        .or_else(|| measured.as_ref().map(|m| m.risk))
        .ok_or_else(|| Error::new("--risk missing"))?;
    let size = options
        .get("size")
        .or_else(|| measured.as_ref().map(|m| m.size))
        .ok_or_else(|| Error::new("--size missing"))?;

    let (rule_no, row) = match_rule(&table, &kind, risk, size)?;
    let tier = row["tier"].as_str().unwrap_or("");
    let effort = row["effort"].as_str().unwrap_or("");
    let reason = row["reason"].as_str().unwrap_or("");
    let is_default = row.get("default") == Some(&Value::Bool(true));

    let mut out = json!({
        "schema": 1,
        "kind": "oms-route-recommendation",
        "task_kind": kind,
        "risk": risk,
        "size": size,
        "rule": rule_no,
        "default": is_default,
        "tier": tier,
        "effort": effort,
        "reason": reason,
        "executes": false,
    });
    if let Some(m) = &measured {
        out["diff"] = json!({
            "range": options.get("diff").unwrap_or(""),
            "lines": m.lines,
            "files": m.files,
        });
    }
    let mut explicit = serde_json::Map::new();
    for (out_key, opt_key) in [
        ("model", "model"),
        ("effort", "reasoning-effort"),
        ("tier", "workload"),
    ] {
        if let Some(val) = options.get(opt_key)
            && !val.is_empty()
        {
            explicit.insert(out_key.into(), Value::String(val.into()));
        }
    }
    if !explicit.is_empty() {
        out["explicit"] = Value::Object(explicit.clone());
        out["note"] = "explicit choices are kept; this row is advice only".into();
    }
    if let Some(failed_count) = failed {
        let base_tier = explicit.get("tier").and_then(Value::as_str).unwrap_or(tier);
        let base_effort = explicit
            .get("effort")
            .and_then(Value::as_str)
            .unwrap_or(effort);
        out["failed"] = json!(failed_count);
        out["escalation"] = match escalate(base_tier, base_effort, failed_count) {
            Some(mut hint) => {
                hint["suggestion"] = Value::Bool(true);
                hint
            }
            None => Value::Null,
        };
    }
    if options.has("json") {
        return Ok((0, json(&out)?, vec![]));
    }
    Ok((
        0,
        render_plain(&out, &explicit, failed).into_bytes(),
        vec![],
    ))
}

struct RangeMetrics {
    risk: &'static str,
    size: &'static str,
    lines: usize,
    files: usize,
}

fn load_table(source: &Path) -> Result<Value> {
    let path = source.join("config/task-routing-rules.json");
    if path.exists() {
        let store = oms_panel_core::Store::open(source)?;
        let bytes = store.read(
            Path::new("config/task-routing-rules.json"),
            1024 * 1024,
            false,
        )?;
        oms_panel_core::decode(&bytes, 1024 * 1024)
    } else {
        oms_panel_core::decode(
            include_bytes!("../../../../config/task-routing-rules.json"),
            1024 * 1024,
        )
    }
}

fn axis_values<'a>(row: &'a Value, axis: &str) -> Vec<&'a str> {
    match row.get(axis) {
        None | Some(Value::Null) => match axis {
            "kind" => KINDS.to_vec(),
            "risk" => RISKS.to_vec(),
            "size" => SIZES.to_vec(),
            _ => vec![],
        },
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn positive_int(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(n)) => n.as_u64().filter(|&v| v >= 1),
        _ => None,
    }
}

pub fn lint(table: &Value) -> Vec<String> {
    let Some(obj) = table.as_object() else {
        return vec!["table must have exactly schema (1), size_buckets and rules".into()];
    };
    let expected: BTreeSet<&str> = ["schema", "size_buckets", "rules"].into_iter().collect();
    if obj.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected
        || obj.get("schema").and_then(Value::as_u64) != Some(1)
    {
        return vec!["table must have exactly schema (1), size_buckets and rules".into()];
    }
    let mut problems = Vec::new();
    let valid_buckets = obj
        .get("size_buckets")
        .and_then(Value::as_object)
        .filter(|b| b.len() == 2 && b.contains_key("small") && b.contains_key("medium"))
        .and_then(|b| {
            let s = b.get("small")?.as_object()?;
            let m = b.get("medium")?.as_object()?;
            let sl = (
                positive_int(s.get("max_lines")),
                positive_int(s.get("max_files")),
            );
            let ml = (
                positive_int(m.get("max_lines")),
                positive_int(m.get("max_files")),
            );
            if s.len() != 2 || m.len() != 2 {
                return None;
            }
            Some((sl, ml))
        });
    match valid_buckets {
        Some(((Some(sl), Some(sf)), (Some(ml), Some(mf)))) => {
            if sl >= ml || sf >= mf {
                problems.push("size_buckets small limits must be below medium".into());
            }
        }
        _ => problems.push(
            "size_buckets needs small and medium with positive integer max_lines and max_files"
                .into(),
        ),
    }

    let Some(rules) = obj.get("rules").and_then(Value::as_array) else {
        problems.push("rules must be a non-empty list".into());
        return problems;
    };
    if rules.is_empty() {
        problems.push("rules must be a non-empty list".into());
        return problems;
    }

    for (index, row) in rules.iter().enumerate() {
        let label = format!("rule {}", index + 1);
        let Some(map) = row.as_object() else {
            problems.push(format!("{label} is not an object"));
            continue;
        };
        let is_default = map.get("default") == Some(&Value::Bool(true));
        let mut allowed: BTreeSet<&str> = ["tier", "effort", "reason"].into_iter().collect();
        if is_default {
            allowed.insert("default");
        } else {
            allowed.extend(["kind", "risk", "size"]);
        }
        let unknown: Vec<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|k| !allowed.contains(k))
            .collect();
        if !unknown.is_empty()
            || (map.contains_key("default") && map.get("default") != Some(&Value::Bool(true)))
        {
            problems.push(format!("{label} has unknown keys"));
        }
        if !map
            .get("tier")
            .and_then(Value::as_str)
            .is_some_and(|v| TIERS.contains(&v))
        {
            problems.push(format!("{label} tier must be one of {}", TIERS.join("/")));
        }
        if !map
            .get("effort")
            .and_then(Value::as_str)
            .is_some_and(|v| EFFORTS.contains(&v))
        {
            problems.push(format!(
                "{label} effort must be one of {}",
                EFFORTS.join("/")
            ));
        }
        if !map
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty())
        {
            problems.push(format!("{label} needs a reason"));
        }
        if is_default {
            if index + 1 != rules.len() {
                problems.push(format!(
                    "{label}: default row must be last (later rows are unreachable)"
                ));
            }
            continue;
        }
        for (axis, valid) in [("kind", KINDS), ("risk", RISKS), ("size", SIZES)] {
            if let Some(val) = map.get(axis)
                && !val.is_null()
            {
                let Some(items) = val.as_array() else {
                    problems.push(format!("{label} {axis} must be a list"));
                    continue;
                };
                let strs: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                let uniq: BTreeSet<&str> = strs.iter().copied().collect();
                if items.is_empty()
                    || strs.len() != items.len()
                    || uniq.len() != items.len()
                    || strs.iter().any(|s| !valid.contains(s))
                {
                    problems.push(format!(
                        "{label} {axis} must be a list of distinct values from {}",
                        valid.join("/")
                    ));
                }
            }
        }
    }
    if rules.last().and_then(|r| r.get("default")) != Some(&Value::Bool(true)) {
        problems.push("missing default row: the last row must be {\"default\": true, ...}".into());
    }
    if !problems.is_empty() {
        return problems;
    }

    let mut seen = BTreeSet::new();
    for kind in KINDS {
        for risk in RISKS {
            for size in SIZES {
                for (index, row) in rules.iter().enumerate() {
                    if row.get("default") == Some(&Value::Bool(true))
                        || (axis_values(row, "kind").contains(kind)
                            && axis_values(row, "risk").contains(risk)
                            && axis_values(row, "size").contains(size))
                    {
                        seen.insert(index);
                        break;
                    }
                }
            }
        }
    }
    for index in 0..rules.len() {
        if !seen.contains(&index) {
            problems.push(format!(
                "rule {} is unreachable: earlier rows already match everything it matches",
                index + 1
            ));
        }
    }
    problems
}

fn match_rule<'a>(
    table: &'a Value,
    kind: &str,
    risk: &str,
    size: &str,
) -> Result<(usize, &'a Value)> {
    let rules = table["rules"]
        .as_array()
        .ok_or_else(|| Error::new("no rules"))?;
    for (index, row) in rules.iter().enumerate() {
        if row.get("default") == Some(&Value::Bool(true))
            || (axis_values(row, "kind").contains(&kind)
                && axis_values(row, "risk").contains(&risk)
                && axis_values(row, "size").contains(&size))
        {
            return Ok((index + 1, row));
        }
    }
    Err(Error::new("no default row"))
}

fn escalate(tier: &str, effort: &str, failed: usize) -> Option<Value> {
    if failed < 2 {
        return None;
    }
    if let Some(pos) = EFFORTS.iter().position(|e| *e == effort)
        && pos + 1 < EFFORTS.len()
    {
        return Some(json!({"tier": tier, "effort": EFFORTS[pos + 1], "step": "effort"}));
    }
    if let Some(pos) = TIERS.iter().position(|t| *t == tier)
        && pos + 1 < TIERS.len()
    {
        return Some(json!({"tier": TIERS[pos + 1], "effort": effort, "step": "tier"}));
    }
    Some(json!({"tier": tier, "effort": effort, "step": "ceiling"}))
}

fn size_bucket(table: &Value, lines: usize, files: usize) -> &'static str {
    for name in ["small", "medium"] {
        let bucket = &table["size_buckets"][name];
        let max_lines = bucket["max_lines"].as_u64().unwrap_or(0) as usize;
        let max_files = bucket["max_files"].as_u64().unwrap_or(0) as usize;
        if lines <= max_lines && files <= max_files {
            return name;
        }
    }
    "large"
}

fn numstat(
    repo: &Path,
    range: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<(Vec<String>, usize)> {
    let argv = ["git", "diff", "--numstat", "--no-renames", range, "--"]
        .map(OsString::from)
        .to_vec();
    let stat = services.execute(&ExecutionRequest::new(argv, repo), stop)?;
    require(stat.code == 0, &format!("git diff failed for {range}"))?;
    let mut paths = Vec::new();
    let mut lines = 0usize;
    for row in String::from_utf8_lossy(&stat.stdout).lines() {
        let parts: Vec<&str> = row.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        lines += parts[0].parse::<usize>().unwrap_or(0) + parts[1].parse::<usize>().unwrap_or(0);
        paths.push(parts[parts.len() - 1].to_owned());
    }
    Ok((paths, lines))
}
fn inspect_range(
    repo: &Path,
    range: &str,
    table: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<RangeMetrics> {
    require(
        !range.is_empty() && !range.starts_with('-'),
        "invalid diff range",
    )?;
    let diff_argv = vec![
        OsString::from("git"),
        OsString::from("diff"),
        OsString::from("--no-renames"),
        OsString::from(range),
        OsString::from("--"),
    ];
    let diff = services.execute(&ExecutionRequest::new(diff_argv, repo), stop)?;
    require(diff.code == 0, &format!("git diff failed for {range}"))?;
    let patch_slice = &diff.stdout[..diff.stdout.len().min(MAX_PATCH)];

    let (paths, lines) = match oms_panel_core::files::patch_changes(&diff.stdout) {
        Ok(changes) => (
            changes.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
            changes.iter().map(|c| c.added + c.removed).sum::<usize>(),
        ),
        // Oversized, non-UTF-8 or unparsable patches keep the numstat view.
        Err(_) => numstat(repo, range, services, stop)?,
    };

    let risk = if paths.is_empty() {
        "low"
    } else if !security_audit_paths(&paths, patch_slice).is_empty() {
        "high"
    } else if is_routine_doc_workload(&paths) {
        "low"
    } else {
        "medium"
    };
    let size = size_bucket(table, lines, paths.len());
    Ok(RangeMetrics {
        risk,
        size,
        lines,
        files: paths.len(),
    })
}

fn is_safe_patch_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

fn is_documentation(path: &str) -> bool {
    path.ends_with(".md") || path.ends_with(".rst") || path.starts_with("docs/")
}

fn is_routine_doc_workload(paths: &[String]) -> bool {
    if paths.is_empty() || paths.iter().any(|p| !is_safe_patch_path(p)) {
        return false;
    }
    if paths
        .iter()
        .any(|p| FRONTIER_PATTERNS.iter().any(|pat| glob_match(pat, p)))
    {
        return false;
    }
    paths.iter().all(|p| {
        let base = p.rsplit('/').next().unwrap_or(p);
        let doc = matches!(p.as_str(), "README.md" | "CHANGELOG.md")
            || (p.starts_with("docs/") && (p.ends_with(".md") || p.ends_with(".rst")));
        doc && !matches!(base, "AGENTS.md" | "CLAUDE.md")
    })
}

fn security_audit_paths(changed_files: &[String], patch_bytes: &[u8]) -> Vec<String> {
    let mut selected = BTreeSet::new();
    let mut touched: BTreeSet<String> = changed_files.iter().cloned().collect();
    if let Ok(extra) = oms_panel_core::files::patch_paths(patch_bytes) {
        touched.extend(extra);
    }

    let deletion = Regex::new(
        r"\bos\.(?:remove|unlink)\s*\(|\bshutil\.rmtree\s*\(|\.unlink\s*\(|\brm\s+-[^\s]*[rf][^\s]*(?:\s|$)|\bfind\b[^\n]*\s-delete\b",
    )
    .expect("valid regex");
    let lifecycle = Regex::new(
        r#""(?:preinstall|install|postinstall|prepare|prepack|postpack|prepublish|prepublishOnly|simple-git-hooks|husky)"\s*:"#,
    )
    .expect("valid regex");
    let hunk_re =
        Regex::new(r"^@@ -[0-9]+(?:,([0-9]+))? \+[0-9]+(?:,([0-9]+))? @@").expect("valid regex");

    let mut current: Option<String> = None;
    let mut deletion_file = false;
    let mut hunk_remaining = 0usize;
    for raw_line in String::from_utf8_lossy(patch_bytes).split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.starts_with("diff --git ") {
            current = None;
            deletion_file = false;
            hunk_remaining = 0;
        } else if hunk_remaining == 0 && line.starts_with("deleted file mode ") {
            deletion_file = true;
        } else if hunk_remaining == 0
            && let Some(rest) = line.strip_prefix("--- ")
        {
            current = patch_name(rest.split('\t').next().unwrap_or(""));
            if deletion_file
                && let Some(p) = &current
                && !is_documentation(p)
            {
                selected.insert(p.clone());
            }
        } else if hunk_remaining == 0
            && let Some(rest) = line.strip_prefix("+++ ")
        {
            if let Some(p) = patch_name(rest.split('\t').next().unwrap_or("")) {
                current = Some(p);
            }
        } else if let Some(caps) = hunk_re.captures(line) {
            let old_n: usize = caps
                .get(1)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let new_n: usize = caps
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            hunk_remaining = old_n + new_n;
        } else if hunk_remaining > 0 {
            if let Some(added) = line.strip_prefix('+')
                && let Some(p) = &current
            {
                if !is_documentation(p) && deletion.is_match(added) {
                    selected.insert(p.clone());
                }
                if p.rsplit('/').next() == Some("package.json") && lifecycle.is_match(added) {
                    selected.insert(p.clone());
                }
            }
            let consume = if line.starts_with(' ') {
                2
            } else if line.starts_with(['+', '-']) {
                1
            } else {
                0
            };
            hunk_remaining = hunk_remaining.saturating_sub(consume);
        }
    }

    for path in &touched {
        let matches_base = FRONTIER_PATTERNS
            .iter()
            .chain(["*secret*", "*scanner*", "*credential*"].iter())
            .chain(ON_OPEN_PATTERNS.iter())
            .any(|pat| glob_match(pat, path));
        let matches_nested = ON_OPEN_PATTERNS.iter().any(|pat| {
            let prefixed = format!("*/{pat}");
            glob_match(&prefixed, path)
        });
        if matches_base || matches_nested {
            selected.insert(path.clone());
        }
    }
    selected.into_iter().collect()
}

fn patch_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed == "/dev/null" {
        return None;
    }
    trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut match_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            match_ti = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            match_ti += 1;
            ti = match_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn render_plain(
    out: &Value,
    explicit: &serde_json::Map<String, Value>,
    failed: Option<usize>,
) -> String {
    let str_field = |key: &str| out[key].as_str().unwrap_or("");
    let mut lines = Vec::new();
    let suffix = if out["default"] == true {
        ", default"
    } else {
        ""
    };
    lines.push(format!(
        "recommend {} (risk {}, size {}): tier {}, effort {}  [rule {}{suffix}]",
        str_field("task_kind"),
        str_field("risk"),
        str_field("size"),
        str_field("tier"),
        str_field("effort"),
        out["rule"]
    ));
    lines.push(format!("reason: {}", str_field("reason")));
    if !explicit.is_empty() {
        let joined: Vec<String> = explicit
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
            .collect();
        lines.push(format!(
            "explicit {} is kept; this is advice only",
            joined.join(", ")
        ));
    }
    if let Some(failed_count) = failed {
        if out["escalation"].is_null() {
            lines.push(format!(
                "escalation: {failed_count} failure(s); the hint starts at 2"
            ));
        } else if out["escalation"]["step"] == "ceiling" {
            lines.push(format!(
                "escalation (suggestion): {failed_count} failures at deep/high; no higher tier is advised (frontier is reserved for security audits): narrow the task or ask the owner"
            ));
        } else {
            let note = if out["escalation"]["step"] == "effort" {
                ""
            } else {
                " (effort is already at max, so the tier goes up)"
            };
            lines.push(format!(
                "escalation (suggestion): {failed_count} failures; try {} effort at tier {}{note}",
                out["escalation"]["effort"].as_str().unwrap_or(""),
                out["escalation"]["tier"].as_str().unwrap_or("")
            ));
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod native_path_tests {
    #[test]
    fn native_frontier_paths_preserve_legacy_and_ordinary_paths() {
        for path in [
            "rust/crates/oms/src/main.rs",
            "rust/crates/oms-command-hooks/src/lib.rs",
            "rust/crates/oms-command-lifecycle/src/update.rs",
            "rust/crates/oms-command-landing/src/lib.rs",
            "rust/crates/oms-command-providers-skills/assets/pi-oms-extension.mjs",
            "rust/crates/oms-command-providers-skills/src/harness.rs",
            "scripts/land.sh",
            "scripts/install.sh",
        ] {
            assert_eq!(super::security_audit_paths(&[path.into()], b""), [path]);
        }
        for path in [
            "",
            "rust/crates/oms-panel-core/src/activity.rs",
            "README.md",
        ] {
            assert!(super::security_audit_paths(&[path.into()], b"").is_empty());
        }
    }
}
