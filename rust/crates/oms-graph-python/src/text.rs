//! Summary privacy filter: a port of `compact_summary` and `sensitive_text`
//! (`scripts/lib/oms_graph/project/model.py`, `oms_runtime/common.py`).
//!
//! The Python patterns use backtracking regexes with lookarounds and Unicode
//! case-insensitive matching; they are written out by hand here so the crate
//! needs no regex engine. Case folding is the one Python `re.IGNORECASE`
//! applies to ASCII pattern letters: ASCII case plus U+0130/U+0131 (i),
//! U+017F (s) and U+212A (k).
//!
//! Text is processed as code point *units* (`u32`), not `char`s, so a lone
//! surrogate of a Python `str` keeps its own value: it is neither space, word
//! nor pattern character, exactly as in Python, and cannot collide with any
//! real character. Summaries show a surrogate as `?`.

use unicode_general_category::get_general_category;

type Unit = u32;

fn units_of(text: &str) -> Vec<Unit> {
    text.chars().map(Unit::from).collect()
}

fn scalar(unit: Unit) -> Option<char> {
    char::from_u32(unit)
}

/// Python `str.isspace` (and `\s` for `str` patterns): Unicode White_Space
/// plus the information separators U+001C..=U+001F.
fn is_space(unit: Unit) -> bool {
    scalar(unit).is_some_and(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Python `\w` for `str` patterns: `str.isalnum()` or `_`, that is letters and
/// numbers but not the combining marks Rust counts as alphabetic.
fn is_word(unit: Unit) -> bool {
    use unicode_general_category::GeneralCategory as G;
    let Some(c) = scalar(unit) else { return false };
    c == '_'
        || matches!(
            get_general_category(c),
            G::UppercaseLetter
                | G::LowercaseLetter
                | G::TitlecaseLetter
                | G::ModifierLetter
                | G::OtherLetter
                | G::DecimalNumber
                | G::LetterNumber
                | G::OtherNumber
        )
}

fn fold(unit: Unit) -> Unit {
    match scalar(unit) {
        Some('\u{130}' | '\u{131}') => Unit::from('i'),
        Some('\u{17f}') => Unit::from('s'),
        Some('\u{212a}') => Unit::from('k'),
        Some(c) => Unit::from(c.to_ascii_lowercase()),
        None => unit,
    }
}

fn is(unit: Unit, expected: char) -> bool {
    unit == Unit::from(expected)
}

/// `" ".join(value.split())`, the first sentence, bounded to 240 code
/// points; `None` when empty or when it carries a credential or an absolute
/// path.
///
/// `inspect.cleandoc` is not ported: it only strips and re-indents whitespace,
/// which the word-splitting below discards.
pub fn compact_summary(value: Option<&str>) -> Option<String> {
    compact_units(&units_of(value?), 240)
}

/// [`compact_summary`] of text that may hold lone surrogates (`units`). The
/// graph does not scrub category C characters in general; a surrogate cannot
/// be stored in a Rust string, so it is shown as `?` and every other character,
/// U+FFFD included, is kept.
pub(crate) fn compact_units(units: &[Unit], limit: usize) -> Option<String> {
    let mut text: Vec<Unit> = Vec::new();
    for word in units
        .split(|&unit| is_space(unit))
        .filter(|w| !w.is_empty())
    {
        if !text.is_empty() {
            text.push(Unit::from(' '));
        }
        text.extend_from_slice(word);
    }
    if text.is_empty() || sensitive_units(&text) {
        return None;
    }
    let end = (0..text.len())
        .find(|&i| {
            matches!(scalar(text[i]), Some('.' | '!' | '?'))
                && text.get(i + 1).is_none_or(|&next| is_space(next))
        })
        .map_or(text.len(), |i| i + 1);
    let mut sentence = &text[..end];
    let mut ellipsis = false;
    if sentence.len() > limit {
        sentence = &sentence[..limit.saturating_sub(3)];
        while let Some((&last, rest)) = sentence.split_last() {
            if !is_space(last) {
                break;
            }
            sentence = rest;
        }
        ellipsis = true;
    }
    let mut out: String = sentence
        .iter()
        .map(|&unit| scalar(unit).unwrap_or('?'))
        .collect();
    if ellipsis {
        out.push_str("...");
    }
    Some(out)
}

pub fn sensitive_text(text: &str) -> bool {
    sensitive_units(&units_of(text))
}

fn sensitive_units(units: &[Unit]) -> bool {
    secret_value(units) || absolute_path(units)
}

fn starts_with(folded: &[Unit], at: usize, literal: &str) -> Option<usize> {
    let mut index = at;
    for expected in literal.chars() {
        if folded.get(index) != Some(&Unit::from(expected)) {
            return None;
        }
        index += 1;
    }
    Some(index)
}

fn boundary(units: &[Unit], at: usize) -> bool {
    let before = at.checked_sub(1).and_then(|i| units.get(i)).copied();
    let after = units.get(at).copied();
    before.is_some_and(is_word) != after.is_some_and(is_word)
}

fn ascii(folded: Unit) -> Option<char> {
    scalar(folded).filter(char::is_ascii)
}

fn token_char(folded: Unit) -> bool {
    ascii(folded).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn bearer_char(folded: Unit) -> bool {
    ascii(folded).is_some_and(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '-')
    })
}

fn secret_value(units: &[Unit]) -> bool {
    let folded: Vec<Unit> = units.iter().map(|&u| fold(u)).collect();
    (0..units.len()).any(|at| {
        credential_assignment(&folded, at)
            || private_key_header(&folded, at)
            || prefixed_token(units, &folded, at)
            || bearer_token(units, &folded, at)
    })
}

/// `(api[_-]?key|access[_-]?token|client[_-]?secret|password|private[_-]?key)\s*[:=]`
fn credential_assignment(folded: &[Unit], at: usize) -> bool {
    let mut ends = Vec::new();
    if let Some(end) = starts_with(folded, at, "password") {
        ends.push(end);
    }
    for (head, tail) in [
        ("api", "key"),
        ("access", "token"),
        ("client", "secret"),
        ("private", "key"),
    ] {
        let Some(after_head) = starts_with(folded, at, head) else {
            continue;
        };
        ends.extend(starts_with(folded, after_head, tail));
        if folded
            .get(after_head)
            .is_some_and(|&u| is(u, '_') || is(u, '-'))
        {
            ends.extend(starts_with(folded, after_head + 1, tail));
        }
    }
    ends.into_iter().any(|end| {
        let mut index = end;
        while folded.get(index).is_some_and(|&u| is_space(u)) {
            index += 1;
        }
        folded.get(index).is_some_and(|&u| is(u, ':') || is(u, '='))
    })
}

/// The PEM private-key header: a dashed BEGIN marker, labels, then the key marker.
fn private_key_header(folded: &[Unit], at: usize) -> bool {
    let Some(start) = starts_with(folded, at, concat!("---", "--be", "gin ")) else {
        return false;
    };
    let run = folded[start..]
        .iter()
        .take_while(|&&u| ascii(u).is_some_and(|c| c.is_ascii_lowercase() || c == ' '))
        .count();
    (1..=run)
        .any(|len| starts_with(folded, start + len, concat!("private ", "key", "-----")).is_some())
}

/// `\b(?:ghp|github_pat|sk)-[A-Za-z0-9_-]{12,}\b`
fn prefixed_token(units: &[Unit], folded: &[Unit], at: usize) -> bool {
    if !boundary(units, at) {
        return false;
    }
    ["ghp-", "github_pat-", "sk-"].iter().any(|prefix| {
        let Some(start) = starts_with(folded, at, prefix) else {
            return false;
        };
        let run = folded[start..]
            .iter()
            .take_while(|&&u| token_char(u))
            .count();
        (12..=run).any(|len| boundary(units, start + len))
    })
}

/// `\bBearer\s+[A-Za-z0-9._~+/-]{12,}={0,2}\b`
fn bearer_token(units: &[Unit], folded: &[Unit], at: usize) -> bool {
    if !boundary(units, at) {
        return false;
    }
    let Some(mut start) = starts_with(folded, at, "bearer") else {
        return false;
    };
    let spaces = units[start..].iter().take_while(|&&u| is_space(u)).count();
    if spaces == 0 {
        return false;
    }
    start += spaces;
    let run = folded[start..]
        .iter()
        .take_while(|&&u| bearer_char(u))
        .count();
    (12..=run).any(|len| {
        let end = start + len;
        // Characters inside the run are never `=`, so padding applies only
        // when the whole run is taken.
        let padding = if len == run {
            units[end..]
                .iter()
                .take(2)
                .take_while(|&&u| is(u, '='))
                .count()
        } else {
            0
        };
        (0..=padding).any(|pad| boundary(units, end + pad))
    })
}

/// `(?<![A-Za-z0-9_.:/-])(?:/(?!/)(?:[^\s/]+/)+[^\s]+|/Users/[^\s]+|/home/[^\s]+|[A-Za-z]:[\\/][^\s]+)`
///
/// The `/Users/` and `/home/` branches match only where the first branch
/// already does, so they need no separate test.
fn absolute_path(units: &[Unit]) -> bool {
    (0..units.len()).any(|at| {
        let after_word = at.checked_sub(1).map(|i| units[i]).is_some_and(|u| {
            ascii(u).is_some_and(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-')
            })
        });
        if after_word {
            return false;
        }
        let first = units[at];
        if is(first, '/') {
            let rest = &units[at + 1..];
            let len = rest.iter().take_while(|&&u| !is_space(u)).count();
            let word = &rest[..len];
            return word.first().is_some_and(|&u| !is(u, '/'))
                && word
                    .iter()
                    .position(|&u| is(u, '/'))
                    .is_some_and(|slash| slash + 1 < word.len());
        }
        ascii(first).is_some_and(|c| c.is_ascii_alphabetic())
            && units.get(at + 1).is_some_and(|&u| is(u, ':'))
            && units
                .get(at + 2)
                .is_some_and(|&u| is(u, '\\') || is(u, '/'))
            && units.get(at + 3).is_some_and(|&u| !is_space(u))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sentence_is_bounded() {
        assert_eq!(
            compact_summary(Some("  A  b.\n c")).as_deref(),
            Some("A b.")
        );
        assert_eq!(compact_summary(Some("v1.2 ok")).as_deref(), Some("v1.2 ok"));
        assert_eq!(compact_summary(Some("")), None);
        assert_eq!(compact_summary(None), None);
        let long = "x".repeat(300);
        let bounded = compact_summary(Some(&long)).unwrap();
        assert_eq!(bounded.chars().count(), 240);
        assert!(bounded.ends_with("..."));
        assert_eq!(
            compact_units(&units_of("abcdef"), 6).as_deref(),
            Some("abcdef")
        );
        assert_eq!(
            compact_units(&units_of("abcdefg"), 6).as_deref(),
            Some("abc...")
        );
    }

    #[test]
    fn credential_shapes_are_withheld() {
        let key = ["pass", "word"].concat();
        assert!(sensitive_text(&format!("the {key} : x")));
        assert!(sensitive_text(&format!("the {} =x", key.to_uppercase())));
        assert!(sensitive_text(&["API", "-", "KEY", "="].concat()));
        assert!(!sensitive_text(&["api", " keys are listed"].concat()));
        assert!(sensitive_text(
            &["\u{212a}ey is api_", "\u{212a}", "ey:"].concat()
        ));
        let run = "a".repeat(12);
        assert!(sensitive_text(&format!("x sk-{run} y")));
        assert!(!sensitive_text(&format!("xsk-{run}")));
        assert!(!sensitive_text(&format!("sk-{}", "a".repeat(11))));
        assert!(sensitive_text(&format!("Bearer {run}==")));
        assert!(!sensitive_text(&format!("Bearer{run}")));
        let dashes = "-".repeat(5);
        let header = |label: &str| format!("{dashes}{} {label}PRIVATE KEY{dashes}", "BEGIN");
        assert!(sensitive_text(&header("RSA ")));
        assert!(!sensitive_text(&header("")));
    }

    #[test]
    fn absolute_paths_are_withheld() {
        assert!(sensitive_text("see /usr/bin/env now"));
        assert!(sensitive_text("C:\\Windows\\x"));
        assert!(sensitive_text("d:/x"));
        assert!(!sensitive_text("https://example.com/a/b"));
        assert!(!sensitive_text("a/b/c and /usr/ and //x/y"));
        assert!(!sensitive_text("/usr"));
        assert!(sensitive_text("(/a/b)"));
    }

    #[test]
    fn a_surrogate_unit_is_neutral_and_shown_as_a_question_mark() {
        let sentence = |units: &[Unit]| compact_units(units, 240);
        let units = [0xD800, u32::from(' '), u32::from('x'), u32::from('.')];
        assert_eq!(sentence(&units).as_deref(), Some("? x."));
        // It never ends a sentence, and U+FFFD stays itself.
        let mixed = [0xDFFF, 0xFFFD, u32::from(' '), u32::from('y')];
        assert_eq!(sentence(&mixed).as_deref(), Some("?\u{fffd} y"));
        let key: Vec<Unit> = units_of(&["pass", "word"].concat())
            .into_iter()
            .chain([0xD800, u32::from(':')])
            .collect();
        assert_eq!(sentence(&key).as_deref(), Some("password?:"));
    }
}
