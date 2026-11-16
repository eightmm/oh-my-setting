use crate::{GraphResult, Selection};
use regex::Regex;
use serde_json::json;

fn resolve_dir(base: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/') || relative.contains('\\') {
        return None;
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

pub(crate) fn cargo(path: &str, text: &str, _digest: &str, mut result: GraphResult) -> GraphResult {
    let base = path.rsplit_once('/').map_or("", |(p, _)| p);
    let id = format!("file:{path}");
    let name = Regex::new(r#"^name\s*=\s*["']([^"']+)["']"#).unwrap();
    let dep = Regex::new(r#"\bpath\s*=\s*["']([^"']+)["']"#).unwrap();
    let member = Regex::new(r#"["']([^"']+)["']"#).unwrap();
    let mut section = "";
    let mut members = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && !members {
            section = line;
            continue;
        }
        if section == "[package]"
            && let Some(c) = name.captures(line)
        {
            result.nodes[0]["metadata"]["package"] = json!(&c[1]);
        }
        let mut paths = Vec::new();
        if section.contains("dependencies") {
            paths.extend(dep.captures_iter(line).map(|c| c[1].to_owned()));
        }
        if section == "[workspace]" && (members || line.starts_with("members")) {
            let value = if members {
                line
            } else {
                line.split_once('=').map_or("", |(_, v)| v)
            };
            paths.extend(member.captures_iter(value).map(|c| c[1].to_owned()));
            members = !value.contains(']');
        }
        for relative in paths {
            if let Some(dir) = resolve_dir(base, &relative) {
                for suffix in ["Cargo.toml", "src/lib.rs"] {
                    let value = if dir.is_empty() {
                        suffix.to_owned()
                    } else {
                        format!("{dir}/{suffix}")
                    };
                    result.refs.push(json!({"from":id,"relation":"imports","kind":"path","value":value,"line":index+1}));
                }
            }
        }
    }
    result
}

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    line: usize,
    string: bool,
}

fn rust_char_literal(text: &str) -> bool {
    let mut chars = text[1..].chars();
    match chars.next() {
        Some('\\') => match chars.next() {
            Some('x') => {
                if !chars.next().is_some_and(|c| c.is_ascii_hexdigit())
                    || !chars.next().is_some_and(|c| c.is_ascii_hexdigit())
                {
                    return false;
                }
            }
            Some('u') => {
                if chars.next() != Some('{') {
                    return false;
                }
                let mut count = 0;
                loop {
                    match chars.next() {
                        Some('}') if count > 0 => break,
                        Some(c) if c.is_ascii_hexdigit() || c == '_' => count += 1,
                        _ => return false,
                    }
                }
            }
            Some(c) if c != '\n' && c != '\r' => {}
            _ => return false,
        },
        Some(c) if c != '\n' && c != '\r' && c != '\'' => {}
        _ => return false,
    }
    chars.next() == Some('\'')
}

// A flat token stream keeps nesting depth independent of the process stack.
fn tokens(text: &str, rust: bool) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let (mut i, mut line) = (0, 1);
    let mut out = Vec::new();
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            line += usize::from(bytes[i] == b'\n');
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    line += usize::from(bytes[i] == b'\n');
                    i += 1;
                }
            }
        } else if matches!(bytes[i], b'"' | b'`')
            || (bytes[i] == b'\'' && (!rust || rust_char_literal(&text[i..])))
        {
            let quote = bytes[i];
            let start_line = line;
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    line += usize::from(bytes[i] == b'\n');
                    i += 1;
                }
            }
            if let Some(value) = text.get(start..i) {
                out.push(Token {
                    text: value,
                    line: start_line,
                    string: true,
                });
            }
            i += usize::from(i < bytes.len());
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(Token {
                text: &text[start..i],
                line,
                string: false,
            });
        } else {
            let start = i;
            i += 1;
            while i < bytes.len() && !text.is_char_boundary(i) {
                i += 1;
            }
            out.push(Token {
                text: &text[start..i],
                line,
                string: false,
            });
        }
    }
    out
}

pub(crate) fn parse(selection: Selection, path: &str, text: &str, digest: &str) -> GraphResult {
    let lang = selection.language_version().unwrap().0;
    let ts = tokens(text, selection == Selection::Rust);
    let mut result = GraphResult {
        nodes: vec![],
        edges: vec![],
        refs: vec![],
    };
    let file = format!("file:{path}");
    let add_ref = |result: &mut GraphResult, value: &str, line: usize| {
        result.refs.push(
            json!({"from":file,"relation":"imports","kind":"path","value":value,"line":line}),
        );
    };
    for (i, token) in ts.iter().enumerate() {
        if token.string {
            continue;
        }
        let next = |offset: usize| ts.get(i + offset);
        let symbol = match selection {
            Selection::Rust => match token.text {
                "fn" => Some("function"),
                "struct" | "enum" | "trait" => Some("class"),
                "mod" | "impl" => Some("symbol"),
                _ => None,
            },
            Selection::JavaScript => match token.text {
                "function" => Some("function"),
                "class" | "interface" => Some("class"),
                "type" => Some("symbol"),
                _ => None,
            },
            Selection::Go => match token.text {
                "func" => Some("function"),
                "type" => Some("class"),
                _ => None,
            },
            Selection::Cpp => match token.text {
                "namespace" => Some("symbol"),
                "struct" | "class" => Some("class"),
                _ => None,
            },
            _ => None,
        };
        if let (Some(kind), Some(name)) = (symbol, next(1))
            && !name.string
            && name
                .text
                .as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        {
            let id = format!("symbol:{}::{}", path.trim_start_matches('/'), name.text);
            let mut metadata = json!({"qualname":name.text,"line":token.line,"end_line":name.line});
            if selection == Selection::Rust && kind == "function" {
                let prefix = &ts[..i];
                let start = prefix
                    .iter()
                    .rposition(|t| matches!(t.text, ";" | "{" | "}"))
                    .map_or(0, |n| n + 1);
                let attr: Vec<&str> = prefix[start..].iter().map(|t| t.text).collect();
                if attr.windows(4).any(|w| w == ["#", "[", "test", "]"])
                    || attr
                        .windows(7)
                        .any(|w| w == ["#", "[", "tokio", ":", ":", "test", "]"])
                {
                    metadata["test"] = json!(true);
                }
            }
            result.nodes.push(json!({"id":id,"kind":kind,"name":name.text,"path":path,"language":lang,"source_digest":digest,"summary":null,"crux":null,"metadata":metadata}));
            result.edges.push(json!({"source":file,"target":id,"relation":"contains","confidence":"EXTRACTED","evidence":{"path":path,"source_digest":digest,"line":token.line}}));
        }
        match selection {
            Selection::Rust if token.text == "mod" && next(2).is_some_and(|t| t.text == ";") => {
                if let Some(name) = next(1) {
                    let base = path.rsplit_once('/').map_or("", |(p, _)| p);
                    for suffix in [format!("{}.rs", name.text), format!("{}/mod.rs", name.text)] {
                        if let Some(value) = resolve_dir(base, &suffix) {
                            add_ref(&mut result, &value, token.line);
                        }
                    }
                }
            }
            Selection::Rust
                if token.text == "use"
                    && next(1).is_some_and(|t| t.text == "crate")
                    && next(2).is_some_and(|t| t.text == ":")
                    && next(3).is_some_and(|t| t.text == ":") =>
            {
                if let Some(name) = next(4) {
                    let base = path
                        .split("/src/")
                        .next()
                        .filter(|_| path.contains("/src/"))
                        .map_or("src".to_owned(), |p| format!("{p}/src"));
                    for suffix in [format!("{}.rs", name.text), format!("{}/mod.rs", name.text)] {
                        if let Some(value) = resolve_dir(&base, &suffix) {
                            add_ref(&mut result, &value, token.line);
                        }
                    }
                }
            }
            Selection::JavaScript if matches!(token.text, "import" | "export" | "require") => {
                for t in ts.iter().skip(i + 1) {
                    if t.text == ";" || t.line > token.line + 20 {
                        break;
                    }
                    if t.string {
                        add_ref(&mut result, t.text, token.line);
                        break;
                    }
                }
            }
            Selection::Go if token.text == "import" => {
                let mut j = i + 1;
                let group = ts.get(j).is_some_and(|t| t.text == "(");
                if group {
                    j += 1;
                }
                while let Some(t) = ts.get(j) {
                    if t.text == ")" || (!group && t.line > token.line) {
                        break;
                    }
                    if t.string {
                        add_ref(&mut result, t.text, t.line);
                        if !group {
                            break;
                        }
                    }
                    j += 1;
                }
            }
            Selection::Cpp if token.text == "include" && i > 0 && ts[i - 1].text == "#" => {
                if let Some(t) = next(1).filter(|t| t.string) {
                    add_ref(&mut result, t.text, token.line);
                }
            }
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_lifetimes_do_not_hide_tests() {
        let result = parse(
            Selection::Rust,
            "src/lib.rs",
            "fn helper(x: &'static str) {}\n#[test] fn after_lifetime() {}",
            "digest",
        );
        let node = result
            .nodes
            .iter()
            .find(|n| n["name"] == "after_lifetime")
            .unwrap();
        assert_eq!(node["kind"], "function");
        assert_eq!(node["metadata"]["test"], true);
    }
}
