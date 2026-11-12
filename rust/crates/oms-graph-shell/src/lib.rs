#![forbid(unsafe_code)]

use regex::Regex;
use std::sync::LazyLock;

static CONTROL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:case|esac)\b|;;&|;&|;;").unwrap());
static SUBSTITUTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\(\s*(?:[A-Za-z_][A-Za-z0-9_]*=[^\s()]+\s+)*(?:!\s*)?([A-Za-z_][A-Za-z0-9_]*)")
        .unwrap()
});
static SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&&|\|\||[;|]").unwrap());
static ASSIGNMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").unwrap());
static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_]*\z").unwrap());

const CONTROL_WORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "select", "function", "source", "return", "local", "export", "readonly", "declare", "typeset",
    "in",
];
const WRAPPERS: &[&str] = &["command", "builtin", "env", "time", "nohup", "sudo"];

/// Return likely command names in the same order as the shell graph parser.
pub fn command_names(raw: &str) -> Vec<String> {
    let mut names: Vec<String> = SUBSTITUTION
        .captures_iter(raw)
        .map(|capture| capture[1].to_owned())
        .collect();
    for segment in SEGMENT.split(raw) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let mut pos = 0;
        while pos < words.len()
            && (CONTROL_WORDS.contains(&words[pos]) || ["!", "{", "("].contains(&words[pos]))
        {
            pos += 1;
        }
        while pos < words.len() && ASSIGNMENT.is_match(words[pos]) {
            pos += 1;
        }
        if pos < words.len() && WRAPPERS.contains(&words[pos]) {
            pos += 1;
            while pos < words.len()
                && (words[pos].starts_with('-') || ASSIGNMENT.is_match(words[pos]))
            {
                pos += 1;
            }
        }
        if pos < words.len() && NAME.is_match(words[pos]) {
            names.push(words[pos].to_owned());
        }
    }
    names
}

/// Remove quoted and commented text for the parser's case-scope analysis.
/// The boolean identifies shell control syntax found inside quotes.
pub fn shell_code(raw: &str) -> (String, bool) {
    let mut code = String::new();
    let mut quoted = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut previous = None;
    for ch in raw.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                quoted.push(' ');
                escaped = false;
            } else if ch == '\\' && delimiter != '\'' {
                quoted.push(' ');
                escaped = true;
            } else if ch == delimiter {
                quote = None;
                code.push(' ');
            } else {
                quoted.push(ch);
            }
        } else if escaped {
            code.push(' ');
            escaped = false;
        } else if ['\'', '"', '`'].contains(&ch) {
            quote = Some(ch);
            code.push(' ');
        } else if ch == '\\' {
            code.push(' ');
            escaped = true;
        } else if ch == '#' && previous.is_none_or(char::is_whitespace) {
            break;
        } else {
            code.push(ch);
        }
        previous = Some(ch);
    }
    (code, CONTROL.is_match(&quoted))
}

static CASE_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bcase\b").unwrap());
static IN_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bin\b").unwrap());
static POSITION_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:then|do|else)\s*$").unwrap());
static CASE_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[;(){}])\s*esac\b").unwrap());

/// Count case openers at recognized command positions in unquoted code.
pub fn case_open_count(code: &str) -> usize {
    CASE_WORD
        .find_iter(code)
        .filter(|found| {
            let prefix = code[..found.start()].trim_end();
            (prefix.is_empty()
                || prefix.ends_with([';', '&', '|', '(', ')', '{'])
                || POSITION_WORD.is_match(prefix))
                && IN_WORD.is_match(&code[found.end()..])
        })
        .count()
}

/// Count case closers at recognized command positions in unquoted code.
pub fn case_close_count(code: &str) -> usize {
    CASE_CLOSE.find_iter(code).count()
}

mod parser;
pub use parser::{ParseResult, parse, parse_with_python};
