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
    start: usize,
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

fn raw_string(text: &str) -> Option<(usize, usize, usize)> {
    let bytes = text.as_bytes();
    let prefix = if bytes.starts_with(b"br") || bytes.starts_with(b"cr") {
        2
    } else if bytes.starts_with(b"r") {
        1
    } else {
        return None;
    };
    let hashes = bytes[prefix..].iter().take_while(|&&c| c == b'#').count();
    let quote = prefix + hashes;
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let start = quote + 1;
    let mut cursor = start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hashes)
                .is_some_and(|s| s.iter().all(|&c| c == b'#'))
        {
            return Some((start, cursor, cursor + 1 + hashes));
        }
        cursor += 1;
    }
    Some((start, bytes.len(), bytes.len()))
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
        } else if rust && let Some((start, end, consumed)) = raw_string(&text[i..]) {
            out.push(Token {
                text: &text[i + start..i + end],
                line,
                string: true,
                start: i,
            });
            line += text[i..i + consumed]
                .bytes()
                .filter(|&b| b == b'\n')
                .count();
            i += consumed;
        } else if matches!(bytes[i], b'"' | b'`')
            || (bytes[i] == b'\'' && (!rust || rust_char_literal(&text[i..])))
        {
            let quote = bytes[i];
            let token_start = i;
            let start_line = line;
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    line += usize::from(bytes[i + 1] == b'\n');
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
                    start: token_start,
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
                start,
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
                start,
            });
        }
    }
    out
}

fn normalized_prefix(text: &str, max_chars: usize) -> (String, bool) {
    let mut value = String::new();
    let mut count = 0;
    for word in text.split_whitespace() {
        for c in (!value.is_empty())
            .then_some(' ')
            .into_iter()
            .chain(word.chars())
        {
            if count == max_chars {
                value.push('…');
                return (value, true);
            }
            value.push(c);
            count += 1;
        }
    }
    (value, false)
}

// Bound the lexical evidence for an inferred direct forwarding call.
const FORWARD_HEADER_TOKENS: usize = 256;
const FORWARD_BODY_TOKENS: usize = 64;

struct RootFunction {
    id: String,
    keyword: usize,
    open: usize,
}

fn code<'a>(ts: &[Token<'a>], index: usize) -> &'a str {
    ts.get(index).filter(|t| !t.string).map_or("", |t| t.text)
}

fn rust_inert_attribute(ts: &[Token<'_>], open: usize) -> bool {
    matches!(
        code(ts, open + 1),
        "allow" | "warn" | "deny" | "forbid" | "expect" | "doc" | "inline" | "must_use" | "cold"
    ) && matches!(code(ts, open + 2), "(" | "=" | "]")
}

// Only visibility, qualifiers and attributes that cannot remove or rewrite an
// item may precede an endpoint. Everything else, including `cfg`, is opaque.
fn rust_unconditional_item(ts: &[Token<'_>], pairs: &[Option<usize>], keyword: usize) -> bool {
    let mut start = keyword;
    while start > 0 {
        if ts[start - 1].string && start >= 2 && code(ts, start - 2) == "extern" {
            start -= 2;
            continue;
        }
        match code(ts, start - 1) {
            "pub" | "async" | "unsafe" | "const" | "extern" => start -= 1,
            ")" => match pairs[start - 1] {
                Some(open) if open > 0 && code(ts, open - 1) == "pub" => start = open - 1,
                _ => return false,
            },
            _ => break,
        }
    }
    while start > 0 && code(ts, start - 1) == "]" {
        let Some(open) = pairs[start - 1] else {
            return false;
        };
        // The file scan already checked all root inner attributes.
        if open >= 2 && code(ts, open - 1) == "!" && code(ts, open - 2) == "#" {
            return true;
        }
        if open == 0 || code(ts, open - 1) != "#" || !rust_inert_attribute(ts, open) {
            return false;
        }
        start = open - 1;
    }
    start == 0 || matches!(code(ts, start - 1), ";" | "}")
}

fn rust_simple_argument(token: &Token<'_>) -> bool {
    let keyword = matches!(
        token.text,
        "_" | "as"
            | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "do"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "fn"
            | "for"
            | "gen"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "static"
            | "struct"
            | "trait"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "yield"
    );
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    token.string || (!keyword && token.text.bytes().all(word))
}

// Accept exactly `[return] callee(arguments) [;]`, where each argument is one
// name or literal, optionally borrowed. Returns the callee token's index.
fn rust_forwarded_callee(ts: &[Token<'_>], open: usize, close: usize) -> Option<usize> {
    if close - open > FORWARD_BODY_TOKENS {
        return None;
    }
    let mut first = open + 1;
    let mut end = close;
    if code(ts, first) == "return" {
        first += 1;
    }
    if code(ts, end - 1) == ";" {
        end -= 1;
    }
    if end < first + 3 || ts[first].string || code(ts, first + 1) != "(" || code(ts, end - 1) != ")"
    {
        return None;
    }
    let mut i = first + 2;
    while i < end - 1 {
        if code(ts, i) == "&" {
            i += 1;
            if code(ts, i) == "mut" {
                i += 1;
            }
        }
        if i >= end - 1 || !rust_simple_argument(&ts[i]) {
            return None;
        }
        i += 1;
        if i < end - 1 {
            if code(ts, i) != "," {
                return None;
            }
            i += 1;
        }
    }
    Some(first)
}

// Count competing function spellings even in nested or opaque syntax. At the
// root, known value declarations and explicit import leaves can also bind a
// name. Imports do not establish which namespace an external item belongs to.
fn rust_forwarding_calls(
    path: &str,
    digest: &str,
    ts: &[Token<'_>],
    pairs: &[Option<usize>],
    at_root: &[bool],
    constructors: &[&str],
    functions: &[RootFunction],
) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;
    let mut declared = BTreeMap::new();
    for name in constructors {
        *declared.entry(*name).or_insert(0usize) += 1;
    }
    let mut in_use = false;
    for (i, &root) in at_root.iter().enumerate() {
        let word = code(ts, i);
        if root
            && word == "#"
            && code(ts, i + 1) == "!"
            && code(ts, i + 2) == "["
            && !rust_inert_attribute(ts, i + 2)
        {
            return Vec::new();
        }
        if root && word == "use" && code(ts, i + 1) != "<" && (i == 0 || code(ts, i - 1) != "#") {
            in_use = true;
        }
        if in_use {
            if !word.is_ascii() {
                // Rust normalizes identifiers to NFC. This lexer does not.
                return Vec::new();
            }
            if matches!(code(ts, i + 1), "," | "}" | ";")
                && word
                    .as_bytes()
                    .first()
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                && !matches!(word, "self" | "_")
            {
                *declared.entry(word).or_insert(0usize) += 1;
            }
            if root && word == ";" {
                in_use = false;
            }
            continue;
        }
        if word == "fn" || (root && matches!(word, "const" | "static" | "struct")) {
            let mut name = i + 1 + usize::from(word == "static" && code(ts, i + 1) == "mut");
            if code(ts, name) == "r" && code(ts, name + 1) == "#" {
                name += 2;
            }
            let spelling = code(ts, name);
            // A non-ASCII identifier may be one or multiple tokens here; do not
            // compare its bytes to a root value name as if they were normalized.
            if !spelling.is_ascii()
                || ts.get(name + 1).is_some_and(|next| {
                    !next.string
                        && !next.text.is_ascii()
                        && next.start == ts[name].start + spelling.len()
                })
            {
                return Vec::new();
            }
            // Only tuple and unit structs contribute a value constructor.
            if word != "struct" {
                *declared.entry(spelling).or_insert(0usize) += 1;
            }
        }
    }
    let functions: Vec<_> = functions
        .iter()
        .filter(|f| {
            declared.get(code(ts, f.keyword + 1)) == Some(&1)
                && rust_unconditional_item(ts, pairs, f.keyword)
        })
        .collect();
    let callees: BTreeMap<&str, &str> = functions
        .iter()
        .filter(|f| matches!(code(ts, f.keyword + 2), "(" | "<"))
        .map(|f| (code(ts, f.keyword + 1), f.id.as_str()))
        .collect();
    let mut edges = Vec::new();
    for caller in &functions {
        let (keyword, open) = (caller.keyword, caller.open);
        if open - keyword > FORWARD_HEADER_TOKENS || code(ts, keyword + 2) != "(" {
            continue;
        }
        let (Some(parameters), Some(close)) = (pairs[keyword + 2], pairs[open]) else {
            continue;
        };
        // A brace between the parameters and the body means the body boundary
        // is not positively known.
        if (parameters + 1..open).any(|i| code(ts, i) == "{") {
            continue;
        }
        let Some(call) = rust_forwarded_callee(ts, open, close) else {
            continue;
        };
        let name = ts[call].text;
        let Some(target) = callees.get(name) else {
            continue;
        };
        // Any header mention may bind the name (and a macro may bind it
        // invisibly). This also excludes self-calls.
        if (keyword..open).any(|i| {
            let token = code(ts, i);
            !token.is_ascii() || [name, "!"].contains(&token)
        }) {
            continue;
        }
        edges.push(json!({"source":caller.id,"target":target,"relation":"calls","confidence":"INFERRED","evidence":{"path":path,"source_digest":digest,"line":ts[call].line}}));
    }
    edges
}

fn rust_declarations(path: &str, text: &str, digest: &str, ts: &[Token<'_>]) -> GraphResult {
    use std::collections::BTreeMap;
    let file = format!("file:{path}");
    let mut result = GraphResult {
        nodes: vec![],
        edges: vec![],
        refs: vec![],
    };
    let mut pairs = vec![None; ts.len()];
    let mut delimiters = Vec::new();
    let mut at_root = vec![false; ts.len()];
    let mut balanced = true;
    for (i, token) in ts.iter().enumerate().filter(|(_, t)| !t.string) {
        at_root[i] = delimiters.is_empty();
        match token.text {
            "(" | "[" | "{" => delimiters.push(i),
            ")" | "]" | "}" => {
                if let Some(&start) = delimiters.last()
                    && matches!(
                        (ts[start].text, token.text),
                        ("(", ")") | ("[", "]") | ("{", "}")
                    )
                {
                    delimiters.pop();
                    pairs[start] = Some(i);
                    pairs[i] = Some(start);
                } else {
                    balanced = false;
                }
            }
            _ => {}
        }
    }
    balanced &= delimiters.is_empty();
    let mut root_functions = Vec::new();
    let mut constructors = Vec::new();
    // Retain the stack for correct exits, but cap rendered ancestry and labels so
    // deeply nested named scopes cannot produce quadratic-size graph output.
    let mut scopes: Vec<(usize, usize, String)> = vec![];
    let mut opening = BTreeMap::new();
    let mut occurrences = BTreeMap::new();
    let mut skip_header = 0;
    for (i, token) in ts.iter().enumerate() {
        while scopes.last().is_some_and(|(end, _, _)| *end < i) {
            scopes.pop();
        }
        if let Some((offset, scope)) = opening.remove(&i) {
            scopes.push((pairs[i].unwrap_or(ts.len()), offset, scope));
        }
        if token.string || i < skip_header {
            continue;
        }
        let kind = match token.text {
            "fn" => "function",
            "struct" | "enum" | "trait" => "class",
            "mod" | "impl" => "symbol",
            _ => continue,
        };
        let Some(next) = ts.get(i + 1) else {
            continue;
        };
        let raw_name = next.text == "r" && ts.get(i + 2).is_some_and(|t| t.text == "#");
        let name_index = i + if raw_name { 3 } else { 1 };
        let Some(name_token) = ts.get(name_index) else {
            continue;
        };
        if token.text != "impl"
            && (name_token.string
                || !name_token
                    .text
                    .as_bytes()
                    .first()
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_'))
        {
            continue;
        }
        let mut end_header = i + 1;
        let mut angles = 0usize;
        while end_header < ts.len() {
            let current = &ts[end_header];
            if !current.string {
                match current.text {
                    "(" | "[" => {
                        if let Some(end) = pairs[end_header] {
                            end_header = end + 1;
                            continue;
                        }
                    }
                    "<" => angles += 1,
                    ">" => angles = angles.saturating_sub(1),
                    "{" if angles > 0 => {
                        if let Some(end) = pairs[end_header] {
                            end_header = end + 1;
                            continue;
                        }
                    }
                    "{" | ";" if angles == 0 => break,
                    "}" => break,
                    _ => {}
                }
            }
            end_header += 1;
        }
        let boundary = ts.get(end_header);
        let body = boundary
            .filter(|t| !t.string && t.text == "{")
            .map(|_| end_header);
        if at_root[i] && token.text == "struct" && body.is_none() {
            constructors.push(name_token.text);
        }
        let (last, complete) = if let Some(open) = body {
            (
                pairs[open].unwrap_or_else(|| ts.len().saturating_sub(1)),
                pairs[open].is_some(),
            )
        } else {
            (
                end_header.min(ts.len().saturating_sub(1)),
                boundary.is_some_and(|t| t.text == ";"),
            )
        };
        let mut start = i;
        while start > 0
            && matches!(
                ts[start - 1].text,
                "pub" | "async" | "unsafe" | "const" | "default" | "extern"
            )
        {
            start -= 1;
        }
        let header_end = boundary.map_or(text.len(), |t| t.start);
        let (signature, signature_truncated) =
            normalized_prefix(&text[ts[start].start..header_end], 2048);
        let (mut name, name_truncated) = if token.text == "impl" {
            normalized_prefix(&text[token.start..header_end], 256)
        } else {
            normalized_prefix(name_token.text, 256)
        };
        if raw_name {
            name = format!("r#{name}");
        }
        if name_truncated {
            name.push_str(&format!("@{}", token.start));
        }
        let skipped = scopes.len().saturating_sub(16);
        let mut parent = scopes[skipped..]
            .iter()
            .map(|(_, _, name)| name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        if skipped > 0 {
            parent = format!("scope@{}::{parent}", scopes[skipped - 1].1);
        }
        let occurrence = occurrences
            .entry((parent.clone(), token.text.to_owned(), name.clone()))
            .or_insert(0usize);
        *occurrence += 1;
        let segment = if *occurrence == 1 {
            name.clone()
        } else {
            format!("{name}#{}", *occurrence)
        };
        let qualname = if parent.is_empty() {
            segment.clone()
        } else {
            format!("{parent}::{segment}")
        };
        let id = format!("symbol:{}::{kind}:{qualname}", path.trim_start_matches('/'));
        let mut metadata = json!({"qualname":qualname,"line":token.line,"end_line":ts[last].line,"signature":signature,"span_complete":complete,"syntax_only":true});
        if signature_truncated {
            metadata["signature_truncated"] = json!(true);
        }
        if skipped > 0 || name_truncated {
            metadata["scope_truncated"] = json!(true);
        }
        if token.text == "fn" {
            let prefix = &ts[..i];
            let attr_start = prefix
                .iter()
                .rposition(|t| !t.string && matches!(t.text, ";" | "{" | "}"))
                .map_or(0, |n| n + 1);
            let attr: Vec<_> = prefix[attr_start..].iter().map(|t| t.text).collect();
            if attr.windows(4).any(|w| w == ["#", "[", "test", "]"])
                || attr
                    .windows(7)
                    .any(|w| w == ["#", "[", "tokio", ":", ":", "test", "]"])
            {
                metadata["test"] = json!(true);
            }
        }
        result.nodes.push(json!({"id":id,"kind":kind,"name":name,"path":path,"language":"rust","source_digest":digest,"summary":null,"crux":null,"metadata":metadata}));
        result.edges.push(json!({"source":file,"target":id,"relation":"contains","confidence":"EXTRACTED","evidence":{"path":path,"source_digest":digest,"line":token.line}}));
        if let Some(open) = body {
            opening.insert(open, (token.start, segment));
            if token.text == "fn" && at_root[i] && complete {
                root_functions.push(RootFunction {
                    id,
                    keyword: i,
                    open,
                });
            }
        }
        skip_header = end_header;
    }
    // A mismatched delimiter anywhere makes every scope claim unreliable.
    if balanced {
        let calls = rust_forwarding_calls(
            path,
            digest,
            ts,
            &pairs,
            &at_root,
            &constructors,
            &root_functions,
        );
        result.edges.extend(calls);
    }
    result
}

pub(crate) fn parse(selection: Selection, path: &str, text: &str, digest: &str) -> GraphResult {
    let lang = selection.language_version().unwrap().0;
    let ts = tokens(text, selection == Selection::Rust);
    let mut result = if selection == Selection::Rust {
        rust_declarations(path, text, digest, &ts)
    } else {
        GraphResult {
            nodes: vec![],
            edges: vec![],
            refs: vec![],
        }
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
            Selection::Rust => None,
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
            let metadata = json!({"qualname":name.text,"line":token.line,"end_line":name.line});
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
