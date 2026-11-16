use crate::{GraphResult, Selection};
use serde_json::json;

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    line: usize,
    string: bool,
}

// A flat token stream keeps nesting depth independent of the process stack.
fn tokens(text: &str) -> Vec<Token<'_>> {
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
        } else if matches!(bytes[i], b'"' | b'\'' | b'`') {
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
    let ts = tokens(text);
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
            result.nodes.push(json!({"id":id,"kind":kind,"name":name.text,"path":path,"language":lang,"source_digest":digest,"summary":null,"crux":null,"metadata":{"qualname":name.text,"line":token.line,"end_line":name.line}}));
            result.edges.push(json!({"source":file,"target":id,"relation":"contains","confidence":"EXTRACTED","evidence":{"path":path,"source_digest":digest,"line":token.line}}));
        }
        match selection {
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
