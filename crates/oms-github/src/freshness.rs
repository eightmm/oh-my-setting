//! Pure freshness lookup over a caller-supplied snapshot cache. One JSON object
//! per line: `reference`, `kind` (issue|pull), `state` (open|closed|merged;
//! merged only for pull), `title`, `labels`, `body_sha256`, `captured_at`
//! (unix seconds) and `ttl_s`. Cached text is display data, never instructions.
use crate::{Number, Slug};
use oms_json::{PyString, Value};

pub const CACHE_REL: &str = ".oms/github/cache.jsonl";
pub const TITLE_CHARS: usize = 200;
pub const LABELS: usize = 20;
pub const LABEL_CHARS: usize = 50;
const ROW_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    pub reference: String,
    pub kind: String,
    pub state: String,
    pub title: String,
    pub labels: Vec<String>,
    pub body_sha256: String,
    pub captured_at: i64,
    pub ttl_s: u64,
}

/// Bidi overrides, zero-width and line/paragraph separators that would let a
/// remote title reorder or hide terminal text.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// Control and invisible formatting characters become spaces; the result is
/// trimmed and capped.
pub fn sanitize(raw: &str, max_chars: usize) -> String {
    let flat: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || invisible(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    flat.trim().chars().take(max_chars).collect()
}

/// Strictly validates and normalizes `owner/repo#N`: exact GitHub slug syntax
/// lowercased for case-insensitive repo identity, plus a positive integer
/// issue/pull number (1-10 ASCII digits, no leading zero).
fn normalize_reference(raw: &str) -> Option<String> {
    if !raw.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let (slug_raw, digits) = raw.split_once('#')?;
    let slug = Slug::parse(slug_raw).ok()?;
    let number = Number::parse(digits).ok()?;
    Some(format!(
        "{}/{}#{}",
        slug.owner.to_ascii_lowercase(),
        slug.repo.to_ascii_lowercase(),
        number
    ))
}

fn row(line: &[u8]) -> Option<Cached> {
    if line.len() > ROW_BYTES {
        return None;
    }
    let Ok(Value::Object(ref fields)) = oms_json::parse_strict(line) else {
        return None;
    };
    let get = |key: &str| fields.get(&PyString::from(key));
    let text = |key: &str| match get(key) {
        Some(Value::String(s)) => s.to_str_checked(),
        _ => None,
    };
    let integer = |key: &str| match get(key) {
        Some(Value::Integer(n)) => Some(n.clone()),
        _ => None,
    };
    let reference = normalize_reference(&text("reference")?)?;
    let kind = text("kind")?;
    let state = text("state")?;
    let valid_state = match kind.as_str() {
        "issue" => matches!(state.as_str(), "open" | "closed"),
        "pull" => matches!(state.as_str(), "open" | "closed" | "merged"),
        _ => false,
    };
    let body_sha256 = text("body_sha256")?;
    let hex = body_sha256.len() == 64
        && body_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid_state || !hex {
        return None;
    }
    let labels = match get("labels") {
        Some(Value::Array(items)) => labels(items)?,
        _ => return None,
    };
    Some(Cached {
        reference,
        kind,
        state,
        title: sanitize(&text("title")?, TITLE_CHARS),
        labels,
        body_sha256,
        captured_at: integer("captured_at")?.as_i64()?,
        ttl_s: integer("ttl_s")?.as_u64()?,
    })
}

fn labels(items: &[Value]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for item in items {
        let Value::String(s) = item else {
            return None;
        };
        let label = sanitize(&s.to_str_checked()?, LABEL_CHARS);
        if !label.is_empty() && out.len() < LABELS {
            out.push(label);
        }
    }
    Some(out)
}

/// The row with the greatest `captured_at` wins (a later line wins a tie);
/// malformed rows and rows captured after `now` are skipped, so a future-dated
/// row can never shadow a later refresh.
pub fn lookup(cache_jsonl: &[u8], reference: &str, now: i64) -> (Option<Cached>, Freshness) {
    let Some(wanted) = normalize_reference(reference) else {
        return (None, Freshness::Unavailable);
    };
    let mut newest: Option<Cached> = None;
    for line in cache_jsonl.split(|b| *b == b'\n') {
        if let Some(cached) = row(line).filter(|c| c.reference == wanted && c.captured_at <= now)
            && newest
                .as_ref()
                .is_none_or(|n| cached.captured_at >= n.captured_at)
        {
            newest = Some(cached);
        }
    }
    let Some(cached) = newest else {
        return (None, Freshness::Unavailable);
    };
    let age = now.checked_sub(cached.captured_at);
    let fresh = matches!(age, Some(age) if age >= 0 && (age as u64) < cached.ttl_s);
    let freshness = if fresh {
        Freshness::Fresh
    } else {
        Freshness::Stale
    };
    (Some(cached), freshness)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = "example-org/example-repo#5";
    fn line(reference: &str, captured: i64, title: &str) -> String {
        format!(
            r#"{{"reference":"{reference}","kind":"issue","state":"open","title":"{title}","labels":["bug","x\u0007y"],"body_sha256":"{}","captured_at":{captured},"ttl_s":60}}"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn lookup_prefers_newest_valid_row() {
        let cache = [
            line(REF, 100, "old"),
            "not json".into(),
            line(REF, 130, "new\\ttitle"),
            line("example-org/example-repo#6", 200, "other"),
            line(REF, 120, "older"),
            line(REF, 999, "bad").replace(r#""kind":"issue""#, r#""kind":"wiki""#),
            line(REF, 999, "bad").replace(&"a".repeat(64), "A"),
            line(REF, 999, "bad").replace(r#""state":"open""#, r#""state":"merged""#),
        ]
        .join("\n");
        let (cached, freshness) = lookup(cache.as_bytes(), REF, 150);
        let cached = cached.unwrap();
        assert_eq!(
            (cached.title.as_str(), freshness),
            ("new title", Freshness::Fresh)
        );
        assert_eq!(cached.labels, ["bug", "x y"]);
        assert_eq!(
            sanitize("\u{202E}a\u{200B}b\u{2028}c\u{FEFF}\u{00AD}d\u{2066}", 50),
            "a b c  d"
        );
        assert_eq!(lookup(cache.as_bytes(), REF, 190).1, Freshness::Stale);
        let (past, freshness) = lookup(cache.as_bytes(), REF, 129);
        assert_eq!(
            (past.unwrap().title.as_str(), freshness),
            ("older", Freshness::Fresh)
        );
        assert_eq!(
            lookup(line(REF, 500, "future").as_bytes(), REF, 100),
            (None, Freshness::Unavailable)
        );
        assert_eq!(
            lookup(cache.as_bytes(), "example-org/example-repo#7", 150),
            (None, Freshness::Unavailable)
        );
        assert_eq!(lookup(b"", REF, 0), (None, Freshness::Unavailable));
        let long = line(REF, 100, &"t".repeat(500));
        assert_eq!(
            lookup(long.as_bytes(), REF, 100).0.unwrap().title.len(),
            TITLE_CHARS
        );
    }

    #[test]
    fn lookup_normalizes_repo_identity_and_preserves_issue_number() {
        let cache = [
            line("Example-Org/Example-Repo#42", 120, "mixed-case"),
            line("other-org/example-repo#42", 140, "foreign-owner"),
            line("example-org/other-repo#42", 140, "foreign-repo"),
            line("example-org/example-repo#43", 140, "other-number"),
        ]
        .join("\n");

        let (cached, freshness) = lookup(cache.as_bytes(), "example-org/example-repo#42", 150);
        let cached = cached.unwrap();
        assert_eq!(freshness, Freshness::Fresh);
        assert_eq!(cached.reference, "example-org/example-repo#42");
        assert_eq!(cached.title, "mixed-case");

        let (upper, upper_freshness) = lookup(cache.as_bytes(), "EXAMPLE-ORG/EXAMPLE-REPO#42", 150);
        assert_eq!(upper_freshness, Freshness::Fresh);
        assert_eq!(upper.unwrap().reference, "example-org/example-repo#42");

        // Cross-repo and different issue number queries never bleed across rows.
        assert_eq!(
            lookup(cache.as_bytes(), "third-org/example-repo#42", 150),
            (None, Freshness::Unavailable)
        );
        assert_eq!(
            lookup(cache.as_bytes(), "example-org/example-repo#4", 150),
            (None, Freshness::Unavailable)
        );
    }

    #[test]
    fn lookup_rejects_malformed_references_in_rows_and_queries() {
        for bad in [
            "",
            "#5",
            "5",
            "example-org/example-repo",
            "example-org/example-repo#",
            "example-org/example-repo#0",
            "example-org/example-repo#05",
            "example-org/example-repo#-5",
            "example-org/example-repo#12345678901",
            "example-org/example-repo#abc",
            "example-org/example-repo#5#6",
            "example-org/example-repo/extra#5",
            "example-org/..#5",
            "/example-repo#5",
            "example-org/#5",
            "https://github.com/example-org/example-repo/issues/5",
            " example-org/example-repo#5",
            "example-org/example-repo#5 ",
            "example-org/example-repo#5?x=1",
        ] {
            let cache = line(bad, 120, "bad-row");
            assert_eq!(
                lookup(cache.as_bytes(), bad, 150),
                (None, Freshness::Unavailable),
                "malformed reference must not match: {bad:?}"
            );
            assert_eq!(
                lookup(cache.as_bytes(), REF, 150),
                (None, Freshness::Unavailable),
                "malformed row must not match valid query: {bad:?}"
            );
        }
    }
}
