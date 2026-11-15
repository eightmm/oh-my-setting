use crate::{
    CONTROL_WORDS, WRAPPERS, case_close_count, case_open_count, command_names, shell_code,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParseResult {
    pub nodes: Vec<Value>,
    pub edges: Vec<Value>,
    pub refs: Vec<Value>,
}

fn re(pattern: &str) -> Arc<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Regex>>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(compiled) = cache.get(pattern) {
        return Arc::clone(compiled);
    }
    let compiled = Arc::new(Regex::new(pattern).expect("static expression"));
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(pattern.to_owned(), Arc::clone(&compiled));
    compiled
}
fn node_id(kind: &str, path: &str, name: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/');
    match kind {
        "env" => format!("env:{name}"),
        "function" => format!("symbol:{normalized}::{name}"),
        _ => format!("{kind}:{normalized}"),
    }
}
fn node(
    kind: &str,
    name: &str,
    path: &str,
    lang: &str,
    digest: &str,
    metadata: Value,
    summary: Value,
) -> Value {
    let id = node_id(kind, path, name);
    let mut v = json!({"id":id,"kind":kind,"name":name,"path":path.replace('\\', "/"),"language":lang,"source_digest":digest,"summary":summary,"crux":null,"metadata":metadata});
    if kind == "env" {
        v.as_object_mut().unwrap().remove("path");
    }
    v
}
fn edge(
    source: &str,
    target: &str,
    relation: &str,
    path: &str,
    digest: &str,
    line: Option<usize>,
) -> Value {
    let mut evidence = json!({"path":path.replace('\\', "/"),"source_digest":digest});
    if let Some(line) = line {
        evidence["line"] = json!(line);
    }
    json!({"source":source,"target":target,"relation":relation,"confidence":"EXTRACTED","evidence":evidence})
}
fn env_name(name: &str) -> bool {
    re(r"^[A-Z][A-Z0-9_]*$").is_match(name)
        && (name.contains('_')
            || ["OMS_", "OH_MY_SETTING_", "CODEX_", "CLAUDE_", "GEMINI_"]
                .iter()
                .any(|p| name.starts_with(p)))
}
fn summary(comments: &[String]) -> Value {
    let text = comments
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return Value::Null;
    }
    let value_pattern = [
        r"(?i)(?:api[_-]?key|access[_-]?token|client[_-]?secret|pass",
        r"word|private[_-]?key)\s*[:",
        concat!(r"=]|-----", r"BEGIN [A-Z ]+PRIVATE KEY-----|\b(?:ghp|github_pat|sk)-[A-Za-z0-9_-]{12,}\b|\bBearer\s+[A-Za-z0-9._~+/-]{12,}"),
        r"={0,2}\b",
    ].concat();
    let absolute = re(&[
        r"\A(?:/(?:[^\s/]+/)+[^\s]+|/ho",
        r"me/[^\s]+|/Us",
        r"ers/[^\s]+|[A-Za-z]:[\\/][^\s]+)",
    ]
    .concat());
    let absolute_match = text.char_indices().any(|(at, ch)| {
        (ch == '/' || ch.is_ascii_alphabetic())
            && text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && !"_.:/-".contains(c))
            && absolute.is_match(&text[at..])
    });
    if re(&value_pattern).is_match(&text) || absolute_match {
        return Value::Null;
    }
    let sentence = re(r"^.*?[.!?](?:\s|$)");
    let chosen = sentence
        .find(&text)
        .map_or(text.as_str(), |m| m.as_str().trim());
    let out = if chosen.chars().count() > 240 {
        format!(
            "{}...",
            chosen.chars().take(237).collect::<String>().trim_end()
        )
    } else {
        chosen.to_owned()
    };
    json!(out)
}
fn add_ref(result: &mut ParseResult, mut v: Value, path: &str, current: &str) {
    result.refs.push(v.clone());
    if !current.is_empty() {
        v["from"] = json!(node_id("function", path, current));
        result.refs.push(v);
    }
}
fn visible(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut quote = '\0';
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if escaped {
            out.push(' ');
            escaped = false;
        } else if c == '\\' && quote != '\'' {
            out.push(' ');
            escaped = true;
        } else if (c == '\'' && quote != '"') || (c == '"' && quote != '\'') {
            quote = if quote == c { '\0' } else { c };
            out.push(' ');
        } else if c == '#'
            && quote == '\0'
            && (i == 0 || (chars[i - 1].is_whitespace() && (i < 2 || chars[i - 2] != '\\')))
        {
            break;
        } else {
            out.push(if quote == '\'' { ' ' } else { c });
        }
    }
    out
}

/// Parse shell data. A completed Python stdin heredoc requires an adapter.
pub fn parse(path: &str, text: &str, digest: &str) -> Result<ParseResult, String> {
    parse_with_python(path, text, digest, |_, _, _| {
        Err("Python fragment adapter required".to_owned())
    })
}

/// Parse shell data with an adapter for Python stdin heredocs. The callback receives
/// the original path, normalized fragment text, and digest, and returns Python's
/// complete `{nodes, edges, refs}` result using fragment-relative one-based lines.
/// Callback errors abort parsing. The shell projection adjusts lines to file offsets.
pub fn parse_with_python<F>(
    path: &str,
    text: &str,
    digest: &str,
    mut python: F,
) -> Result<ParseResult, String>
where
    F: FnMut(&str, &str, &str) -> Result<ParseResult, String>,
{
    let mut result = ParseResult::default();
    let file_id = node_id("file", path, "");
    if path.starts_with("scripts/lib/") {
        result.nodes.push(node(
            "module",
            path.rsplit('/').next().unwrap_or(path),
            path,
            "shell",
            digest,
            json!({}),
            Value::Null,
        ));
        result.edges.push(edge(
            &file_id,
            &node_id("module", path, ""),
            "contains",
            path,
            digest,
            None,
        ));
    }
    let function_re =
        re(r"^\s*(?:function\s+([A-Za-z_][A-Za-z0-9_]*)|([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\))");
    let expansion = re(r"\$(?:\{([A-Za-z_][A-Za-z0-9_]*)|([A-Za-z_][A-Za-z0-9_]*))");
    let here_re = re(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)"#);
    let python_re =
        re(r"^\s*(?:[A-Za-z_][A-Za-z0-9_]*=[^\s]+\s+)*(?:python|python3(?:\.\d+)?)\s+(?:-\s|<<)");
    let invocation = re(
        r"(\$\{?[A-Za-z_][A-Za-z0-9_]*\}?/)?((?:[A-Za-z0-9_][A-Za-z0-9_.-]*/)+[A-Za-z0-9_][A-Za-z0-9_.-]*\.(?:sh|bash|py))\b",
    );
    let include = re(r"^\s*(?:source|\.)\s+(.+?)\s*$");
    let clean = re(r#"^(?:\$\{?(?:ROOT|SCRIPTS_DIR)\}?/?|\$\(dirname\s+['"]?\$0['"]?\)/?)"#);
    let opener = re(r#"^\s*case\s+("(?:\\.|[^"])*"|'[^']*'|[^\s]+)\s+in\s*(?:#.*)?$"#);
    let arm = re(r"^\s*([A-Za-z0-9_./*?@:+-]+(?:\|[A-Za-z0-9_./*?@:+-]+)*)\)(.*)$");
    let terminators = re(r";;&|;&|;;");
    let term_end = re(r"(?:;;&|;&|;;)\s*$");
    let close_line = re(r"^}\s*(?:#.*)?$");
    let multiline_re = re(r"\{\s*(?:#.*)?$");
    let plain_esac = re(r"^\s*esac\s*(?:#.*)?$");
    let code_words = re(r"\b(?:case|esac)\b");
    let mut heredoc = String::new();
    let mut heredoc_python = false;
    let mut strip_tabs = false;
    let mut body = Vec::new();
    let mut heredoc_line = 0;
    let mut depth = 0usize;
    let mut patterns = false;
    let mut arm_head = false;
    let mut current = String::new();
    let mut functions: HashMap<String, usize> = HashMap::new();
    let mut comments = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let number = index + 1;
        let raw = raw.trim_end_matches('\r');
        if !heredoc.is_empty() {
            let line = if strip_tabs {
                raw.trim_start_matches('\t')
            } else {
                raw
            };
            if line.trim() == heredoc {
                heredoc.clear();
                if heredoc_python {
                    let parsed = python(path, &body.join("\n"), digest)?;
                    result
                        .nodes
                        .extend(parsed.nodes.into_iter().filter(|n| n["kind"] == "env"));
                    for mut e in parsed
                        .edges
                        .into_iter()
                        .filter(|e| e["relation"] == "reads_env")
                    {
                        let n = e["evidence"]["line"]
                            .as_u64()
                            .filter(|n| *n > 0)
                            .and_then(|n| n.checked_add(heredoc_line as u64))
                            .ok_or("invalid Python edge line")?;
                        e["source"] = json!(if current.is_empty() {
                            file_id.clone()
                        } else {
                            node_id("function", path, &current)
                        });
                        e["evidence"]["line"] = json!(n);
                        result.edges.push(e);
                    }
                    for mut r in parsed
                        .refs
                        .into_iter()
                        .filter(|r| r["relation"] == "imports" && r["from"] == file_id)
                    {
                        let n = r["line"]
                            .as_u64()
                            .filter(|n| *n > 0)
                            .and_then(|n| n.checked_add(heredoc_line as u64))
                            .ok_or("invalid Python ref line")?;
                        r["line"] = json!(n);
                        r["heredoc_python"] = json!(true);
                        add_ref(&mut result, r, path, &current);
                    }
                }
                heredoc_python = false;
                strip_tabs = false;
                body.clear();
            } else if heredoc_python {
                body.push(if raw.chars().count() > 4096 {
                    String::new()
                } else {
                    line.to_owned()
                });
            }
            continue;
        }
        if raw.chars().count() > 4096 {
            comments.clear();
            continue;
        }
        let stripped = raw.trim();
        if stripped.starts_with('#') {
            if !stripped.starts_with("#!") {
                comments.push(stripped.trim_start_matches('#').trim().to_owned());
            }
            continue;
        }
        if stripped.is_empty() {
            comments.clear();
            continue;
        }
        for cap in expansion.captures_iter(&visible(raw)) {
            let name = cap.get(1).or_else(|| cap.get(2)).unwrap().as_str();
            if env_name(name) {
                result.nodes.push(node(
                    "env",
                    name,
                    path,
                    "env",
                    digest,
                    json!({}),
                    Value::Null,
                ));
                let inline = function_re
                    .captures(raw)
                    .and_then(|c| c.get(1).or_else(|| c.get(2)))
                    .map_or("", |m| m.as_str());
                let owner = if current.is_empty() { inline } else { &current };
                let source = if owner.is_empty() {
                    file_id.clone()
                } else {
                    node_id("function", path, owner)
                };
                result.edges.push(edge(
                    &source,
                    &node_id("env", path, name),
                    "reads_env",
                    path,
                    digest,
                    Some(number),
                ));
            }
        }
        if let Some(cap) = here_re.captures(raw) {
            heredoc = cap[1].to_owned();
            heredoc_python = python_re.is_match(raw);
            strip_tabs = raw.contains("<<-");
            body.clear();
            heredoc_line = number;
        }
        if !current.is_empty() && close_line.is_match(raw) {
            if let Some(&i) = functions.get(&current) {
                result.nodes[i]["metadata"]["end_line"] = json!(number);
            }
            current.clear();
            comments.clear();
            continue;
        }
        if let Some(cap) = function_re.captures(raw) {
            let name = cap.get(1).or_else(|| cap.get(2)).unwrap().as_str();
            let metadata = json!({"signature":format!("{name}()"),"end_line":number,"qualname":name,"line":number});
            result.nodes.push(node(
                "function",
                name,
                path,
                "shell",
                digest,
                metadata,
                summary(&comments),
            ));
            functions.insert(name.to_owned(), result.nodes.len() - 1);
            result.edges.push(edge(
                &file_id,
                &node_id("function", path, name),
                "contains",
                path,
                digest,
                Some(number),
            ));
            current = if raw == raw.trim_start() && multiline_re.is_match(raw) {
                name.to_owned()
            } else {
                String::new()
            };
            comments.clear();
            continue;
        }
        comments.clear();
        let (code, quoted_control) = shell_code(raw);
        let clear = opener
            .captures(raw)
            .is_some_and(|c| !["$(", "`", "\\"].iter().any(|v| c[1].contains(v)));
        let mut opened = false;
        let mut scan = raw;
        if depth == 0 && clear && !quoted_control && !raw.trim_end().ends_with('\\') {
            depth = 1;
            patterns = true;
            arm_head = true;
            opened = true;
        }
        if depth == 1 && patterns && !opened {
            let terms = terminators.find_iter(&code).count();
            if quoted_control || raw.trim_end().ends_with('\\') || code_words.is_match(&code) {
                patterns = false;
            } else if arm_head {
                if let Some(c) = arm.captures(raw) {
                    scan = c.get(2).unwrap().as_str();
                    arm_head = false;
                } else if !plain_esac.is_match(raw) {
                    patterns = false;
                }
            }
            if terms > 0 {
                if term_end.is_match(&code) && terms == 1 && !quoted_control {
                    if patterns {
                        arm_head = true;
                    }
                } else {
                    patterns = false;
                }
            }
        } else if depth > 0 && (quoted_control || raw.trim_end().ends_with('\\')) {
            patterns = false;
        }
        if let Some(c) = include.captures(raw) {
            let arg = c[1].trim();
            let quoted = arg
                .chars()
                .next()
                .filter(|c| *c == '\'' || *c == '"')
                .and_then(|quote| {
                    arg[1..]
                        .char_indices()
                        .filter(|(i, c)| {
                            *c == quote
                                && (arg[1 + i + 1..].is_empty()
                                    || arg[1 + i + 1..]
                                        .chars()
                                        .next()
                                        .is_some_and(char::is_whitespace))
                        })
                        .map(|(i, _)| &arg[1..1 + i])
                        .next_back()
                });
            let target = quoted
                .unwrap_or_else(|| arg.split_whitespace().next().unwrap_or(""))
                .trim_matches(['\'', '"']);
            let variable = target.contains('$');
            let cleaned = clean
                .replace(target, "")
                .replace("scripts/lib/../", "scripts/");
            let cleaned = cleaned.trim_start_matches(['.', '/']);
            if !cleaned.is_empty() {
                add_ref(
                    &mut result,
                    json!({"from":file_id,"relation":"imports","kind":"path","value":cleaned,"line":number,"variable":variable,"shell_include":true}),
                    path,
                    &current,
                );
            }
            continue;
        }
        for c in invocation.captures_iter(scan) {
            let value = c[2].trim_start_matches(['.', '/']);
            if value.contains('/') && value != path {
                add_ref(
                    &mut result,
                    json!({"from":file_id,"relation":"calls","kind":"path","value":value,"line":number,"variable":c.get(1).is_some()}),
                    path,
                    &current,
                );
            }
        }
        for command in command_names(scan) {
            if !CONTROL_WORDS.contains(&command.as_str()) && !WRAPPERS.contains(&command.as_str()) {
                add_ref(
                    &mut result,
                    json!({"from":file_id,"relation":"calls","kind":"name","value":command,"line":number,"shell_bare":true}),
                    path,
                    &current,
                );
            }
        }
        if !opened {
            depth = depth
                .saturating_add(case_open_count(&code))
                .saturating_sub(case_close_count(&code));
        }
        if depth == 0 {
            patterns = false;
            arm_head = false;
        }
    }
    Ok(result)
}
