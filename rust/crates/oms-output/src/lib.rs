//! Deterministic output digest construction. Capture, storage and redaction are adapter responsibilities.
use regex::Regex;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use unicode_general_category::{GeneralCategory as Gc, get_general_category};

pub const READ_CAP: usize = 64 * 1024 * 1024;
const LINE_CHARS: usize = 300;
const FAILURE_LINES: usize = 40;

#[derive(Debug, Clone)]
pub struct Options {
    pub exit_code: Option<i64>,
    pub duration_s: Option<i64>,
    pub reference: Option<String>,
    pub withhold: Option<String>,
    pub max_bytes: usize,
    pub head: usize,
    pub tail: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            exit_code: None,
            duration_s: None,
            reference: None,
            withhold: None,
            max_bytes: 4000,
            head: 10,
            tail: 30,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Raw {
    pub r#ref: Option<String>,
    pub sha256: String,
    pub bytes: usize,
    pub lines: usize,
    pub partial_last_line: bool,
    pub unread_leading_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct Failures {
    pub matched: usize,
    pub by_recogniser: serde_json::Map<String, serde_json::Value>,
    pub shown: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct Shown {
    pub mode: String,
    pub lines: usize,
    pub omitted_lines: usize,
    pub omitted_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct OutputDigest {
    pub schema: u8,
    pub kind: &'static str,
    pub exit_code: Option<i64>,
    pub duration_s: Option<i64>,
    pub raw: Raw,
    pub parser: Vec<String>,
    pub failures: Failures,
    pub withheld: Option<String>,
    pub shown: Shown,
    pub text: String,
}

fn ansi() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
            .unwrap()
    })
}
fn recognisers() -> &'static Vec<(&'static str, Regex)> {
    static R: OnceLock<Vec<(&str, Regex)>> = OnceLock::new();
    R.get_or_init(|| [
        ("pytest", r"^(?:FAILED|ERROR) \S+(?:::\S+)?(?: - |$)|^=+ .*\b\d+ (?:failed|error)"),
        ("unittest", r"^(?:FAIL|ERROR): \S+ \(|^FAILED \((?:failures|errors)="),
        ("smoke", r"^FAIL: |^check: \S+ FAILED|^not ok \d+"),
        ("traceback", r"^Traceback \(most recent call last\):|^\w+(?:\.\w+)*(?:Error|Exception): "),
        ("compiler", r"^\S+:\d+(?::\d+)?: (?:fatal )?error\b|^error(?:\[\w+\])?: |^make(?:\[\d+\])?: \*\*\* "),
        ("shellcheck", r"\bSC\d{4} \((?:error|warning)\)"),
        ("lint", r"^\S+:\d+:\d+: [EFW]\d+ |^\s*\d+:\d+\s+error\s"),
        ("runtime", r"\b(?:Segmentation fault|Killed|Out of memory|OOM|command not found|No such file or directory|Permission denied|panic:|FATAL)\b"),
        ("generic", r"\b(?:ERROR|Error:|error:|FAILED|FAILURE|Failed:)"),
    ].into_iter().map(|(n,p)| (n, Regex::new(p).unwrap())).collect())
}
// The fixed patterns contain only ASCII literals. Map other characters to
// representatives with Python 3.12's Unicode 15 word, digit and space classes;
// matching uses this projection only, while output retains the original text.
fn recogniser_text(line: &str) -> String {
    line.chars()
        .map(|c| {
            if matches!(c, '\u{1c}'..='\u{1f}' | '\u{85}') {
                return '\t';
            }
            if c.is_ascii() {
                return c;
            }
            match get_general_category(c) {
                Gc::DecimalNumber => '١',
                Gc::UppercaseLetter
                | Gc::LowercaseLetter
                | Gc::TitlecaseLetter
                | Gc::ModifierLetter
                | Gc::OtherLetter
                | Gc::LetterNumber
                | Gc::OtherNumber => 'é',
                Gc::SpaceSeparator | Gc::LineSeparator | Gc::ParagraphSeparator => '\t',
                _ => '☃',
            }
        })
        .collect()
}
/// Strip terminal escapes and controls, trim trailing whitespace and cap at 300 characters.
pub fn clean(line: &str) -> String {
    let stripped = ansi().replace_all(line, "");
    let s: String = stripped
        .chars()
        .filter(|&c| !matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'))
        .collect();
    let s = s.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'));
    let count = s.chars().count();
    if count > LINE_CHARS {
        format!(
            "{}…[+{} chars]",
            s.chars().take(LINE_CHARS).collect::<String>(),
            count - LINE_CHARS
        )
    } else {
        s.to_owned()
    }
}
/// Construct a digest from the complete captured bytes, including the whole-file hash.
/// Options use the CLI's limits; an invalid option returns its diagnostic.
pub fn build(bytes: &[u8], options: &Options) -> Result<OutputDigest, &'static str> {
    if !(256..=1_048_576).contains(&options.max_bytes)
        || options.head > 10000
        || options.tail > 10000
    {
        return Err("output-digest: --max-bytes is 256..1048576 and --head/--tail are 0..10000");
    }
    let mut skipped = 0;
    if bytes.len() > READ_CAP {
        skipped = bytes.len() - READ_CAP;
        skipped += bytes[skipped..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len() - skipped, |i| i + 1);
    }
    let decoded = String::from_utf8_lossy(&bytes[skipped..]);
    let partial = !decoded.is_empty() && !decoded.ends_with('\n');
    let mut lines: Vec<&str> = decoded.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let total = lines.len();
    let mut found = Vec::new();
    let mut by = serde_json::Map::new();
    for (i, line) in lines.iter().enumerate() {
        let stripped = ansi().replace_all(line, "");
        let line = recogniser_text(&stripped);
        for (name, pattern) in recognisers() {
            if pattern.is_match(&line) {
                found.push(i + 1);
                let count = by.entry((*name).to_owned()).or_insert(serde_json::json!(0));
                *count = serde_json::json!(count.as_u64().unwrap() + 1);
                break;
            }
        }
    }
    let parser = if by.is_empty() {
        vec!["none".to_owned()]
    } else {
        {
            let mut names: Vec<_> = by.keys().cloned().collect();
            names.sort();
            names
        }
    };
    let mut doc = OutputDigest {
        schema: 1,
        kind: "oms-output-digest",
        exit_code: options.exit_code,
        duration_s: options.duration_s,
        raw: Raw {
            r#ref: options.reference.clone(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            bytes: bytes.len(),
            lines: total,
            partial_last_line: partial,
            unread_leading_bytes: skipped,
        },
        parser,
        failures: Failures {
            matched: found.len(),
            by_recogniser: by,
            shown: 0,
        },
        withheld: options.withhold.clone(),
        shown: Shown {
            mode: String::new(),
            lines: 0,
            omitted_lines: 0,
            omitted_bytes: 0,
        },
        text: String::new(),
    };
    if options.withhold.as_ref().is_some_and(|s| !s.is_empty()) {
        doc.shown = Shown {
            mode: "withheld".into(),
            lines: 0,
            omitted_lines: total,
            omitted_bytes: bytes.len(),
        };
        return Ok(doc);
    }
    let cleaned: Vec<String> = lines.iter().map(|l| clean(l)).collect();
    let whole: usize = cleaned.iter().map(|l| l.len() + 1).sum();
    if whole <= options.max_bytes && skipped == 0 {
        doc.shown = Shown {
            mode: "whole".into(),
            lines: total,
            omitted_lines: 0,
            omitted_bytes: 0,
        };
        doc.failures.shown = found.len();
        doc.text = cleaned.join("\n");
        return Ok(doc);
    }
    let mut budget = options.max_bytes;
    let mut shown = BTreeMap::new();
    let mut take = |number: usize| {
        if shown.contains_key(&number) {
            return true;
        }
        if number == 0 || number > total {
            return false;
        }
        let line = &cleaned[number - 1];
        let cost = line.len() + number.to_string().len() + 3;
        if cost > budget {
            return false;
        }
        budget -= cost;
        shown.insert(number, line);
        true
    };
    for &number in found.iter().take(FAILURE_LINES) {
        if !take(number) {
            break;
        }
        doc.failures.shown += 1;
        take(number + 1);
    }
    let tail_start = total.saturating_sub(options.tail).saturating_add(1).max(1);
    for number in (tail_start..=total).rev() {
        if !take(number) {
            break;
        }
    }
    for number in 1..=options.head.min(total) {
        if !take(number) {
            break;
        }
    }
    let mut out = Vec::new();
    let mut previous = 0;
    for (&number, line) in &shown {
        if number != previous + 1 {
            out.push(format!("   … {} line(s) omitted …", number - previous - 1));
        }
        out.push(format!("{number}| {line}"));
        previous = number;
    }
    if previous < total {
        out.push(format!("   … {} line(s) omitted …", total - previous));
    }
    let included: usize = shown.keys().map(|&n| lines[n - 1].len() + 1).sum();
    doc.shown = Shown {
        mode: if doc.failures.by_recogniser.is_empty() {
            "raw head+tail"
        } else {
            "failures+tail+head"
        }
        .into(),
        lines: shown.len(),
        omitted_lines: total - shown.len(),
        omitted_bytes: bytes.len().saturating_sub(included),
    };
    doc.text = out.join("\n");
    Ok(doc)
}

impl OutputDigest {
    fn by(&self) -> String {
        let mut entries: Vec<_> = self.failures.by_recogniser.iter().collect();
        entries.sort_by_key(|(key, _)| *key);
        entries
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// JSON representation including the CLI's size estimates.
    pub fn json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("digest serialization cannot fail");
        value["size"] = serde_json::json!({"raw_bytes": self.raw.bytes, "digest_text_bytes": self.text.len(), "estimate": {"method": "bytes/4 (a heuristic, not a tokenizer)", "raw": self.raw.bytes / 4, "digest_text": self.text.len() / 4}});
        value
    }
    /// Render the human-readable report, including its terminating newline.
    pub fn render(&self) -> String {
        let raw = &self.raw;
        let shown = &self.shown;
        let fails = &self.failures;
        let mut head = format!(
            "output-digest: exit={}  lines={}  bytes={}",
            self.exit_code
                .map_or_else(|| "unknown".into(), |v| v.to_string()),
            raw.lines,
            raw.bytes
        );
        if let Some(duration) = self.duration_s {
            head.push_str(&format!("  duration={duration}s"));
        }
        let mut out = vec![
            head,
            format!(
                "raw: {}  sha256={}",
                raw.r#ref
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("not kept"),
                raw.sha256
            ),
        ];
        if raw.unread_leading_bytes > 0 {
            out.push(format!(
                "read: the last {} bytes; the first {} bytes were not read",
                raw.bytes - raw.unread_leading_bytes,
                raw.unread_leading_bytes
            ));
        }
        if raw.partial_last_line {
            out.push("note: the capture ends mid-line (no final newline)".into());
        }
        if let Some(reason) = self.withheld.as_deref().filter(|s| !s.is_empty()) {
            out.push(format!("shown: nothing; {reason}"));
            out.push(format!(
                "failures: {} matching line(s){}",
                fails.matched,
                if fails.matched > 0 {
                    format!(" ({})", self.by())
                } else {
                    String::new()
                }
            ));
        } else {
            if shown.mode == "whole" {
                out.push(format!(
                    "shown: all {} line(s); nothing omitted",
                    shown.lines
                ));
            } else {
                out.push(format!("shown: {} of {} line(s) ({}); omitted {} line(s), {} bytes — open the raw file for them", shown.lines, raw.lines, shown.mode, shown.omitted_lines, shown.omitted_bytes));
            }
            if fails.matched > 0 {
                out.push(format!(
                    "failures: {} matching line(s) ({}); {} shown",
                    fails.matched,
                    self.by(),
                    fails.shown
                ));
            } else if self.exit_code.is_some_and(|c| c != 0) {
                out.push("failures: no recogniser matched; the lines below are a raw head and tail, not a diagnosis".into());
            } else {
                out.push("failures: none matched".into());
            }
            if !self.text.is_empty() {
                out.push("---".into());
                out.push(self.text.clone());
                out.push("---".into());
            }
        }
        let size = out.join("\n").len();
        out.push(format!("size: digest {size} bytes of {} raw bytes; a derived view, not the run's record and not verification evidence", raw.bytes));
        format!("{}\n", out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_unicode_recogniser_classes() {
        let cases = [
            ("\u{301}ERROR", "generic"),
            ("\u{200d}ERROR", "generic"),
            ("éERROR", "none"),
            ("²ERROR", "none"),
            ("=== \u{301}١ failed", "pytest"),
            ("not ok ١", "smoke"),
            ("\u{1c}١:٢\u{1d}error\u{1e}", "lint"),
            ("\u{200d}SC١٢٣٤ (warning)", "shellcheck"),
            ("éError: boom", "traceback"),
            ("é:١:٢: error boom", "compiler"),
            ("check: é FAILED", "smoke"),
            ("FAIL: é (boom)", "unittest"),
            ("\u{301}FATAL\u{200d}", "runtime"),
            ("", "none"),
        ];
        for (line, name) in cases {
            let d = build(line.as_bytes(), &Options::default()).unwrap();
            assert_eq!(d.parser, [name], "{line:?}");
        }
    }
    #[test]
    fn whole_and_render() {
        let d = build("hello\n한글 줄\n".as_bytes(), &Options::default()).unwrap();
        assert_eq!(d.text, "hello\n한글 줄");
        assert_eq!(d.raw.lines, 2);
        assert_eq!(d.shown.mode, "whole");
        assert!(d.render().contains("shown: all 2 line(s); nothing omitted"));
        assert_eq!(
            d.raw.sha256,
            format!("{:x}", Sha256::digest("hello\n한글 줄\n".as_bytes()))
        );
        assert_eq!(d.json()["size"]["digest_text_bytes"], d.text.len());
    }
    #[test]
    fn priority_and_boundaries() {
        let input = format!(
            "{}\x1b[31mFAILED tests/x - bad\x1b[0m\nreason\n{}partial",
            "normal\n".repeat(100),
            "after\n".repeat(100)
        );
        let o = Options {
            max_bytes: 256,
            head: 0,
            tail: 3,
            exit_code: Some(3),
            ..Options::default()
        };
        let d = build(input.as_bytes(), &o).unwrap();
        assert!(d.text.contains("101| FAILED tests/x - bad"));
        assert!(d.text.contains("102| reason"));
        assert!(d.text.contains("partial"));
        assert!(d.raw.partial_last_line);
        assert_eq!(d.failures.by_recogniser["pytest"], 1);
        assert!(
            d.render()
                .contains("1 matching line(s) (pytest=1); 1 shown")
        );
        assert!(
            build(
                b"",
                &Options {
                    max_bytes: 255,
                    ..o.clone()
                }
            )
            .is_err()
        );
        assert!(
            build(
                b"",
                &Options {
                    max_bytes: 1_048_577,
                    ..o.clone()
                }
            )
            .is_err()
        );
        assert!(
            build(
                b"",
                &Options {
                    max_bytes: 1_048_576,
                    head: 10000,
                    tail: 10000,
                    ..o.clone()
                }
            )
            .is_ok()
        );
        assert!(build(b"", &Options { head: 10001, ..o }).is_err());
    }
    #[test]
    fn cleaning_withholding_and_invalid_utf8() {
        assert_eq!(
            clean(&format!("\x1b[31m{}\x1b[0m ", "가".repeat(301))),
            format!("{}…[+1 chars]", "가".repeat(300))
        );
        let d = build(
            b"FAIL: broken\n\xff",
            &Options {
                withhold: Some("withheld".into()),
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(d.shown.mode, "withheld");
        assert!(d.text.is_empty());
        assert!(
            d.render()
                .contains("failures: 1 matching line(s) (smoke=1)")
        );
        let d = build(b"\xff", &Options::default()).unwrap();
        assert_eq!(d.text, "�");
    }
    #[test]
    fn read_cap_discards_partial_first_line() {
        let mut bytes = vec![b'x'; READ_CAP + 2];
        bytes[3] = b'\n';
        let d = build(&bytes, &Options::default()).unwrap();
        assert_eq!(d.raw.unread_leading_bytes, 4);
        assert_eq!(d.raw.lines, 1);
    }
}
