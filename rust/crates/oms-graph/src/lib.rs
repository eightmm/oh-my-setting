//! In-memory graph parser selection. Callers own source acquisition and the
//! persistent Python parser capability; this crate never starts a helper.
use oms_graph_python::{Error as PythonError, Parser, compact_summary};
use oms_graph_shell::ParseResult;
use regex::Regex;
use serde_json::json;
use std::sync::OnceLock;

mod polyglot;

pub use oms_graph_shell::ParseResult as GraphResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selection {
    Python,
    Shell,
    Markdown,
    Config,
    Rust,
    JavaScript,
    Go,
    Cpp,
    UnsupportedNotebook,
    Unknown,
}

impl Selection {
    pub fn language_version(self) -> Option<(&'static str, u32)> {
        match self {
            Self::Python => Some(("python", 6)),
            Self::Shell => Some(("shell", 8)),
            Self::Markdown => Some(("markdown", 2)),
            Self::Config => Some(("config", 1)),
            Self::Rust => Some(("rust", 1)),
            Self::JavaScript => Some(("javascript", 1)),
            Self::Go => Some(("go", 1)),
            Self::Cpp => Some(("cpp", 1)),
            Self::UnsupportedNotebook | Self::Unknown => None,
        }
    }
}

pub fn select(path: &str) -> Selection {
    let path = path.to_lowercase();
    for (extensions, selection) in [
        (&[".py"][..], Selection::Python),
        (&[".sh", ".bash"][..], Selection::Shell),
        (&[".md", ".markdown"][..], Selection::Markdown),
        (&[".rs"][..], Selection::Rust),
        (
            &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"][..],
            Selection::JavaScript,
        ),
        (&[".go"][..], Selection::Go),
        (
            &[".c", ".h", ".cc", ".cpp", ".hpp", ".cu", ".cuh"][..],
            Selection::Cpp,
        ),
        (&[".json", ".yaml", ".yml", ".toml"][..], Selection::Config),
        (&[".ipynb"][..], Selection::UnsupportedNotebook),
    ] {
        if extensions.iter().any(|extension| path.ends_with(extension)) {
            return selection;
        }
    }
    Selection::Unknown
}

#[derive(Debug)]
pub enum Error {
    Python(PythonError),
    Shell(String),
    Unsupported(Selection),
}

fn convert(result: oms_graph_python::ParseResult) -> ParseResult {
    let value = serde_json::to_value(result).expect("typed graph data serializes");
    let mut object = value
        .as_object()
        .expect("typed result is an object")
        .clone();
    let mut take = |key| {
        object
            .remove(key)
            .expect("typed result field")
            .as_array()
            .expect("typed result array")
            .clone()
    };
    ParseResult {
        nodes: take("nodes"),
        edges: take("edges"),
        refs: take("refs"),
    }
}

/// Parse UTF-8 data using the caller's single Python capability where needed.
pub fn parse<P: Parser + ?Sized>(
    path: &str,
    text: &str,
    digest: &str,
    python: &mut P,
) -> Result<ParseResult, Error> {
    match select(path) {
        Selection::Python => python
            .parse(path, text, digest)
            .map(convert)
            .map_err(Error::Python),
        Selection::Shell => {
            let mut failure = None;
            let result =
                oms_graph_shell::parse_with_python(path, text, digest, |path, text, digest| {
                    python
                        .parse(path, text, digest)
                        .map(convert)
                        .map_err(|error| {
                            failure = Some(error);
                            "Python parser failed".to_owned()
                        })
                });
            match result {
                Ok(result) => Ok(result),
                Err(error) => Err(failure.map_or(Error::Shell(error), Error::Python)),
            }
        }
        Selection::Markdown => Ok(document(path, text, digest, true)),
        Selection::Config if path == "Cargo.toml" || path.ends_with("/Cargo.toml") => Ok(
            polyglot::cargo(path, text, digest, document(path, text, digest, false)),
        ),
        Selection::Config => Ok(document(path, text, digest, false)),
        selection @ (Selection::Rust | Selection::JavaScript | Selection::Go | Selection::Cpp) => {
            Ok(polyglot::parse(selection, path, text, digest))
        }
        other => Err(Error::Unsupported(other)),
    }
}

fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn lines(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            result.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, next)| *next == '\n') {
                let (j, _) = iter.next().unwrap();
                start = j + 1;
            }
        }
    }
    if start < text.len() {
        result.push(&text[start..]);
    }
    result
}

fn document(path: &str, text: &str, digest: &str, markdown: bool) -> ParseResult {
    let kind = if markdown { "document" } else { "config" };
    let language = if markdown { "markdown" } else { "config" };
    let normalized = path.replace('\\', "/");
    let id = format!("{kind}:{}", normalized.trim_start_matches('/'));
    let mut summary = None;
    let mut refs = Vec::new();
    if markdown {
        let physical = lines(text);
        let mut paragraph = Vec::new();
        for raw in &physical {
            let line = raw.trim_matches(py_space);
            if line.is_empty() {
                if !paragraph.is_empty() {
                    break;
                }
                continue;
            }
            if ["#", "```", "~~~", "---", "<!--"]
                .iter()
                .any(|prefix| line.starts_with(prefix))
            {
                continue;
            }
            paragraph.push(line);
        }
        summary = compact_summary(Some(&paragraph.join(" ")));
        summary = compact_summary(summary.as_deref());
        static PATH: OnceLock<Regex> = OnceLock::new();
        let pattern = PATH.get_or_init(|| {
            Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_][A-Za-z0-9_.-]*)+").unwrap()
        });
        for (line, content) in physical.iter().enumerate() {
            let mut offset = 0;
            while offset < content.len() {
                let tail = &content[offset..];
                let preceding = content[..offset].chars().next_back();
                let allowed = preceding.is_none_or(|c| {
                    !c.is_ascii_alphanumeric() && !matches!(c, '_' | '.' | '/' | '$' | '\\' | '-')
                });
                if allowed && let Some(found) = pattern.find(tail) {
                    refs.push(json!({"from":id,"relation":"references","kind":"path","value":found.as_str().trim_end_matches(['.', ',', ':', ';', '`']),"line":line + 1}));
                    offset += found.end();
                } else {
                    offset += tail.chars().next().unwrap().len_utf8();
                }
            }
        }
    }
    ParseResult {
        nodes: vec![
            json!({"id":id,"kind":kind,"name":path.rsplit('/').next().unwrap_or(path),"path":normalized,"language":language,"source_digest":digest,"summary":summary,"crux":null,"metadata":{}}),
        ],
        edges: vec![],
        refs,
    }
}
