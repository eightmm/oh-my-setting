//! Deterministic failure metadata. Recovery labels never authorize execution.
#![forbid(unsafe_code)]

use oms_json::{PyString, Value};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use unicode_general_category::{GeneralCategory, get_general_category};
mod rules;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Properties {
    pub retryable: bool,
    pub fallback_allowed: bool,
    pub context_escalation_allowed: bool,
    pub recovery: &'static str,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Classification {
    pub code: &'static str,
    pub properties: Properties,
    pub matched: PyString,
    pub exit: Option<i64>,
}
struct Rule {
    code: &'static str,
    pattern: &'static str,
    properties: Properties,
}
const UNKNOWN: Properties = Properties {
    retryable: false,
    fallback_allowed: false,
    context_escalation_allowed: false,
    recovery: "preserve_and_escalate",
};

/// Ordered canonical codes, including the uncatalogued default.
pub fn codes() -> Vec<&'static str> {
    rules::RULES
        .iter()
        .map(|r| r.code)
        .chain(["unknown_failure"])
        .collect()
}
pub fn catalog() -> BTreeMap<&'static str, Properties> {
    rules::RULES
        .iter()
        .map(|r| (r.code, r.properties))
        .collect()
}
impl Properties {
    fn fields(self) -> BTreeMap<PyString, Value> {
        BTreeMap::from([
            ("retryable".into(), Value::Bool(self.retryable)),
            (
                "fallback_allowed".into(),
                Value::Bool(self.fallback_allowed),
            ),
            (
                "context_escalation_allowed".into(),
                Value::Bool(self.context_escalation_allowed),
            ),
            ("recovery".into(), Value::String(self.recovery.into())),
        ])
    }
    pub fn to_json_value(self) -> Value {
        Value::Object(self.fields())
    }
}
impl Classification {
    pub fn to_json_value(&self) -> Value {
        let mut fields = self.properties.fields();
        fields.insert("code".into(), Value::String(self.code.into()));
        fields.insert("matched".into(), Value::String(self.matched.clone()));
        fields.insert(
            "exit".into(),
            self.exit.map_or(Value::Null, |n| {
                Value::integer(&n.to_string()).expect("i64 is valid")
            }),
        );
        Value::Object(fields)
    }
}
fn word(p: u32) -> bool {
    let Some(c) = char::from_u32(p) else {
        return false;
    };
    c == '_'
        || matches!(
            get_general_category(c),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
}
fn space(p: u32) -> bool {
    (0x1c..=0x1f).contains(&p) || char::from_u32(p).is_some_and(char::is_whitespace)
}
fn bounded(points: &[u32]) -> PyString {
    let mut out = Vec::new();
    let mut pending = false;
    for &p in points {
        if space(p) {
            pending = !out.is_empty();
        } else {
            if pending {
                out.push(32);
                pending = false;
            }
            out.push(p);
        }
    }
    if out.len() > 120 {
        out.truncate(119);
        out.push(0x2026);
    }
    PyString::from_codepoints(out).expect("validated input")
}
/// Classify UTF-8 text.
pub fn classify(text: &str, exit: Option<i64>, explicit: &str) -> Classification {
    classify_codepoints(
        &text.chars().map(u32::from).collect::<Vec<_>>(),
        exit,
        explicit,
    )
    .expect("UTF-8 is valid")
}
/// Classify Python codepoints, including surrogates; rejects values above U+10FFFF.
pub fn classify_codepoints(
    text: &[u32],
    exit: Option<i64>,
    explicit: &str,
) -> Result<Classification, oms_json::Error> {
    if text.iter().any(|&p| p > 0x10ffff) {
        return Err(oms_json::Error::InvalidCodepoint);
    }
    let result = |code, properties, matched| Classification {
        code,
        properties,
        matched,
        exit,
    };
    if let Some(r) = rules::RULES.iter().find(|r| r.code == explicit) {
        return Ok(result(r.code, r.properties, "explicit".into()));
    }
    if exit == Some(124) {
        let r = rules::RULES
            .iter()
            .find(|r| r.code == "provider_timeout")
            .expect("timeout rule");
        return Ok(result(r.code, r.properties, "exit-124".into()));
    }
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    let regexes = REGEXES.get_or_init(|| {
        rules::RULES
            .iter()
            .map(|r| Regex::new(r.pattern).expect("canonical pattern"))
            .collect()
    });
    let mut normalized = String::new();
    let mut offsets = Vec::with_capacity(text.len() + 1);
    for &p in text {
        offsets.push(normalized.len());
        let c = match p {
            0x130 | 0x131 => 'i',
            0x17f => 's',
            0x212a => 'k',
            0xd800..=0xdfff => '\u{fffd}',
            _ => char::from_u32(p).expect("validated"),
        };
        normalized.push(c.to_ascii_lowercase());
    }
    offsets.push(normalized.len());
    for (r, re) in rules::RULES.iter().zip(regexes) {
        let mut start = 0;
        while let Some(m) = re.find_at(&normalized, start) {
            let a = offsets
                .binary_search(&m.start())
                .expect("character boundary");
            let b = offsets.binary_search(&m.end()).expect("character boundary");
            let boundary_rule = (r.code == "verifier_failed"
                && &normalized[m.start()..m.end()] == "gate failed")
                || (r.code == "state_conflict" && &normalized[m.start()..m.end()] == "cas");
            if !boundary_rule
                || ((a == 0 || !word(text[a - 1])) && (b == text.len() || !word(text[b])))
            {
                return Ok(result(r.code, r.properties, bounded(&text[a..b])));
            }
            start = offsets[a + 1];
        }
    }
    Ok(result("unknown_failure", UNKNOWN, "".into()))
}
