use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static MANDATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^## Mandate[\s\x1c-\x1f]*\n").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^## ").unwrap());
static PARAGRAPH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n[\s\x1c-\x1f]*\n").unwrap());
static STRATEGY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z0-9-]+-STRATEGY$").unwrap());

fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
fn trim(s: &str) -> &str {
    s.trim_matches(whitespace)
}
fn description(text: &str, name: &str) -> String {
    let block = if let Some(m) = MANDATE.find(text) {
        let tail = &text[m.end()..];
        trim(&tail[..HEADING.find(tail).map_or(tail.len(), |m| m.start())])
    } else {
        PARAGRAPH
            .split(text)
            .map(trim)
            .find(|p| !p.is_empty() && !p.starts_with('#') && !STRATEGY.is_match(p))
            .unwrap_or("")
    };
    let first = PARAGRAPH.split(block).next().unwrap_or("");
    let parts: Vec<_> = first
        .split([
            '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
            '\u{2029}',
        ])
        .map(trim)
        .collect();
    let line = parts.join(" ");
    let line = trim(line.trim_start_matches(['-', ' ']));
    if line.is_empty() {
        format!("OMS {name} role")
    } else {
        line.to_string()
    }
}
fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("string JSON")
}

pub(crate) fn render(
    provider: &str,
    role: &str,
    text: &str,
    bundled: bool,
    policy: &Value,
) -> Result<String, String> {
    // Source membership is decisive; a same-name project/global override never writes.
    let writable = bundled
        && matches!(
            role,
            "worker"
                | "builder"
                | "refactorer"
                | "documenter"
                | "integrator"
                | "performance-engineer"
                | "implementation-worker"
        );
    let name = format!("oms-{role}");
    let desc = description(text, role);
    let body = text.trim_end_matches('\n').to_string() + "\n";
    // Rendering does not use dispatch aliases or catalog upgrades: the canonical
    // renderer takes the first matching provider in the role's configured tier.
    let route = policy["roles"]
        .get(role)
        .and_then(|row| row["tier"].as_str())
        .and_then(|tier| policy["tiers"][tier].as_array())
        .and_then(|routes| routes.iter().find(|route| route["provider"] == provider));
    if provider == "claude" {
        let mut lines = vec![
            "---".into(),
            format!("name: {name}"),
            format!("description: {}", quoted(&desc)),
            format!(
                "tools: {}",
                if writable {
                    "Read, Grep, Glob, Edit, Write, Bash"
                } else {
                    "Read, Grep, Glob"
                }
            ),
        ];
        if let Some(route) = route {
            lines.push(format!("model: {}", route["model"].as_str().unwrap()));
            let effort =
                crate::native_model::normalized_effort(route["effort"].as_str().unwrap_or(""));
            if !effort.is_empty() {
                lines.push(format!("effort: {effort}"));
            }
        }
        lines.extend(["---".into(), String::new(), body]);
        return Ok(lines.join("\n"));
    }
    if provider == "antigravity" {
        let mut lines = vec![
            "---".into(),
            format!("name: {name}"),
            format!("description: {}", quoted(&desc)),
            "mainAgent: false".into(),
            "subagent: true".into(),
            "model: inherit".into(),
            // YAML 1.1 parsers treat an unquoted off as a boolean.
            format!(
                "commandExecutionPolicy: {}",
                quoted(if writable { "sandbox" } else { "off" })
            ),
            "tools:".into(),
            "  - view_file".into(),
            "  - grep_search".into(),
        ];
        if writable {
            lines.extend([
                "  - replace_file_content".into(),
                "  - write_to_file".into(),
                "  - run_command".into(),
            ]);
        } else {
            lines.push("inheritMcp: false".into());
        }
        lines.extend(["---".into(), String::new(), body]);
        return Ok(lines.join("\n"));
    }
    if body
        .chars()
        .any(|c| matches!(c, '\0'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'))
    {
        return Err("role text holds a control character".into());
    }
    let mut lines = vec![
        format!("name = {}", quoted(&name)),
        format!("description = {}", quoted(&desc)),
    ];
    if let Some(route) = route {
        lines.push(format!(
            "model = {}",
            quoted(route["model"].as_str().unwrap())
        ));
        let effort = crate::native_model::normalized_effort(route["effort"].as_str().unwrap_or(""));
        if !effort.is_empty() {
            lines.push(format!("model_reasoning_effort = {}", quoted(effort)));
        }
    }
    lines.push(format!(
        "sandbox_mode = {}",
        quoted(if writable {
            "workspace-write"
        } else {
            "read-only"
        })
    ));
    let escaped = body.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"");
    lines.push(format!(
        "developer_instructions = \"\"\"\n{}\n\"\"\"",
        escaped.trim_end_matches('\n')
    ));
    Ok(lines.join("\n") + "\n")
}
