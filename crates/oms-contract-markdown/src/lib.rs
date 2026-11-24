//! Pure OMS Markdown helpers. Scope paths are descriptive, never authorization.
//! Markdown inputs are UTF-8 Rust strings; Python's arbitrary objects and surrogate
//! strings require explicit conversion or rejection. The [`scope`] module also
//! accepts raw OS bytes and Python codepoints for changed-path classification.
//! Limits are nonnegative `usize`; callers migrating negative Python limits must
//! handle them before calling (Python returns an ellipsis for any negative limit).
pub mod scope;
mod unicode15;

use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use unicode_general_category::{GeneralCategory, get_general_category};

pub type Sections = Vec<(String, Vec<String>)>;
#[derive(Debug, PartialEq, Eq)]
pub struct Scope {
    pub allowed: Vec<String>,
    pub forbidden: Vec<String>,
}

fn ws(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
fn trim(s: &str) -> &str {
    s.trim_matches(ws)
}
fn collapse(s: &str) -> String {
    s.split(ws)
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
fn member(c: char, ranges: &[(u32, u32)]) -> bool {
    let n = c as u32;
    ranges
        .binary_search_by(|&(a, b)| {
            if n < a {
                std::cmp::Ordering::Greater
            } else if n > b {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}
fn lower(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut output = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == 'Σ' {
            let before = chars[..i]
                .iter()
                .rev()
                .copied()
                .find(|&x| !member(x, unicode15::CASEIGNORABLE));
            let after = chars[i + 1..]
                .iter()
                .copied()
                .find(|&x| !member(x, unicode15::CASEIGNORABLE));
            if before.is_some_and(|x| member(x, unicode15::CASED))
                && !after.is_some_and(|x| member(x, unicode15::CASED))
            {
                output.push('ς');
            } else {
                output.push('σ');
            }
        } else if get_general_category(c) == GeneralCategory::Unassigned {
            output.push(c);
        } else {
            output.extend(c.to_lowercase());
        }
    }
    output
}
fn lines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, n)| *n == '\n') {
                let (j, _) = iter.next().unwrap();
                start = j + 1;
            }
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}
const W: &str = r"[\s\x{1c}-\x{1f}]";
fn pattern(s: &str) -> Regex {
    let decimal = unicode15::DECIMAL
        .iter()
        .map(|(a, b)| format!(r"\x{{{a:x}}}-\x{{{b:x}}}"))
        .collect::<String>();
    Regex::new(&s.replace("@", W).replace(r"\d", &format!("[{decimal}]"))).expect("static regex")
}
macro_rules! cached {
    ($name:ident, $value:expr) => {{
        static $name: LazyLock<Regex> = LazyLock::new(|| pattern($value));
        &*$name
    }};
}

pub fn normalized_line(text: &str) -> String {
    let p = cached!(NORMALIZED, r"\[[ xX]\]@*");
    collapse(&p.replace_all(trim(text), ""))
        .trim_matches([' ', '-', '\t'])
        .to_owned()
}
pub fn bounded_line(value: &str, limit: usize) -> String {
    let text = collapse(value);
    if text.chars().count() <= limit {
        text
    } else {
        format!(
            "{}…",
            text.chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    }
}
pub fn sections(text: &str) -> (Sections, BTreeMap<String, String>) {
    let mut result: Sections = vec![("__preamble__".into(), vec![])];
    let mut metadata = BTreeMap::new();
    let mut current = 0;
    let mut front = false;
    let meta = cached!(META, r"^([A-Za-z][A-Za-z0-9_.-]{0,63}):@*(.*)$");
    let pre = cached!(PRE, r"^@*-@*([A-Za-z][A-Za-z0-9_.-]{0,63}):@*(.*)$");
    let heading = cached!(HEADING, r"^#{1,6}@+(.+?)@*$");
    for (i, line) in lines(text).into_iter().enumerate() {
        if i == 0 && trim(line) == "---" {
            front = true;
            continue;
        }
        if front {
            if trim(line) == "---" {
                front = false;
                continue;
            }
            if let Some(c) = meta.captures(line) {
                metadata.insert(c[1].to_ascii_lowercase(), trim(&c[2]).to_owned());
            }
            continue;
        }
        if let Some(c) = heading.captures(line) {
            let name = trim(&c[1]);
            current = result
                .iter()
                .position(|(h, _)| h == name)
                .unwrap_or_else(|| {
                    result.push((name.to_owned(), vec![]));
                    result.len() - 1
                });
        } else {
            result[current].1.push(line.to_owned());
        }
    }
    for line in result[0].1.iter().take(40) {
        if let Some(c) = pre.captures(line) {
            metadata
                .entry(c[1].to_ascii_lowercase())
                .or_insert_with(|| trim(&c[2]).to_owned());
        }
    }
    (result, metadata)
}
pub fn section_text(mapping: &Sections, names: &[&str]) -> String {
    let wanted: Vec<_> = names.iter().map(|s| lower(s)).collect();
    let body = mapping
        .iter()
        .filter(|(h, _)| wanted.iter().any(|n| lower(h).contains(n)))
        .flat_map(|(_, l)| l.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    trim(&body).to_owned()
}
pub fn task_goal(body: &Sections) -> String {
    first_nonempty(&section_text(body, &["goal", "objective", "목표"]), 1000)
}

/// Project objective from the declared goal section, with the legacy preamble fallback.
pub fn project_goal(body: &Sections) -> String {
    let mut goal = first_nonempty(
        &section_text(body, &["goal", "objective", "purpose", "목표", "목적"]),
        1000,
    );
    if goal.is_empty() {
        goal = first_nonempty(
            &body
                .iter()
                .find(|(k, _)| k == "__preamble__")
                .map(|(_, v)| v.join("\n"))
                .unwrap_or_default(),
            1000,
        )
    }
    goal
}

pub fn section_text_exact(mapping: &Sections, names: &[&str]) -> String {
    let wanted: Vec<_> = names.iter().map(|s| collapse(&lower(trim(s)))).collect();
    let body = mapping
        .iter()
        .filter(|(h, _)| wanted.contains(&collapse(&lower(trim(h)))))
        .flat_map(|(_, l)| l.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    trim(&body).to_owned()
}
pub fn bullet_items(text: &str, preserve_text: bool) -> Vec<String> {
    let list = if preserve_text {
        cached!(LIST_PRESERVE, r"^@*(?:[-*+]@+|\d+[.)]@+)(.*)$")
    } else {
        cached!(
            LIST_NORMAL,
            r"^@*(?:[-*+]@+|\d+[.)]@+)(?:\[[ xX]\]@*)?(.*)$"
        )
    };
    let mut result = Vec::new();
    let mut pending = String::new();
    fn flush(out: &mut Vec<String>, pending: &mut String, preserve: bool) {
        if !pending.is_empty() {
            let v = if preserve {
                pending.clone()
            } else {
                normalized_line(pending)
            };
            if !v.is_empty() {
                out.push(v);
            }
            pending.clear();
        }
    }
    for line in lines(text) {
        if let Some(c) = list.captures(line) {
            flush(&mut result, &mut pending, preserve_text);
            pending = trim(&c[1]).to_owned();
        } else if !pending.is_empty() && (line.starts_with("  ") || line.starts_with('\t')) {
            pending.push(' ');
            pending.push_str(trim(line));
        } else {
            flush(&mut result, &mut pending, preserve_text);
        }
    }
    flush(&mut result, &mut pending, preserve_text);
    result
}
pub fn first_nonempty(text: &str, limit: usize) -> String {
    for line in lines(text) {
        let value = normalized_line(line);
        if !value.is_empty() {
            return bounded_line(&value, limit);
        }
    }
    String::new()
}
fn marker() -> &'static Regex {
    cached!(
        MARKER,
        r"(?i)\[(?:[iıİ]d|cr[iıİ]ter[iıİ]on):@*([A-Za-zİı0-9._:-]{1,80})\]"
    )
}
pub fn stable_criterion_id(source: &str, text: &str) -> String {
    if let Some(c) = marker().captures(text) {
        return c[1].to_owned();
    }
    let normalized = collapse(&lower(trim(text)));
    let digest = Sha256::digest(format!("{source}\0{normalized}").as_bytes());
    format!(
        "criterion-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}
pub fn strip_criterion_marker(text: &str) -> String {
    let p = cached!(
        STRIP,
        r"(?i)@*\[(?:[iıİ]d|cr[iıİ]ter[iıİ]on):@*[A-Za-zİı0-9._:-]{1,80}\]@*"
    );
    trim(&p.replace_all(text, " ")).to_owned()
}
pub fn parse_path_list(value: &str) -> Vec<String> {
    parse_paths(value.split([',', '\n']))
}
pub fn parse_paths<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut result = BTreeSet::new();
    for item in items {
        let mut cleaned = trim(item).trim_matches(['`', '"', '\'']).replace('\\', "/");
        while cleaned.starts_with("./") {
            cleaned = cleaned[2..].to_owned();
        }
        let b = cleaned.as_bytes();
        if cleaned.is_empty()
            || cleaned.starts_with('/')
            || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/')
            || cleaned.split('/').any(|c| c == "..")
        {
            continue;
        }
        result.insert(cleaned);
    }
    result.into_iter().collect()
}
pub fn parse_scope<'a>(lines: impl IntoIterator<Item = &'a str>) -> Scope {
    let allow = cached!(
        ALLOW,
        r"(?i)^(?:allowed(?:_paths)?|[iıİ]nclude|scope\.allow)@*:@*(.+)$"
    );
    let deny = cached!(
        DENY,
        r"(?i)^(?:forb[iıİ]dden(?:_paths)?|exclude|scope\.deny)@*:@*(.+)$"
    );
    let mut a = BTreeSet::new();
    let mut d = BTreeSet::new();
    for raw in lines {
        let line = normalized_line(raw);
        if let Some(c) = allow.captures(&line) {
            a.extend(parse_path_list(&c[1]));
        } else if let Some(c) = deny.captures(&line) {
            d.extend(parse_path_list(&c[1]));
        }
    }
    Scope {
        allowed: a.into_iter().collect(),
        forbidden: d.into_iter().collect(),
    }
}
