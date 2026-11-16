//! Deterministic output digest construction. Capture, storage and redaction are adapter responsibilities.
use regex::Regex;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read};
use std::sync::OnceLock;
use unicode_general_category::{GeneralCategory as Gc, get_general_category};

pub const READ_CAP: usize = 64 * 1024 * 1024;
const LINE_CHARS: usize = 300;
const FAILURE_LINES: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Auto,
    Legacy,
}
#[derive(Debug, Clone)]
pub struct Options {
    pub exit_code: Option<i64>,
    pub duration_s: Option<i64>,
    pub reference: Option<String>,
    pub withhold: Option<String>,
    pub max_bytes: usize,
    pub head: usize,
    pub tail: usize,
    pub strategy: Strategy,
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
            strategy: Strategy::Auto,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Raw {
    pub r#ref: Option<String>,
    pub sha256: String,
    pub bytes: usize,
    pub lines: usize,
    #[serde(skip_serializing)]
    pub source_lines: usize,
    pub partial_last_line: bool,
    pub unread_leading_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_tool: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_arguments: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_unavailable_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_max_bytes: Option<usize>,
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
    pub representation: Representation,
    pub transformations: Transformations,
    pub shown: Shown,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Representation {
    pub algorithm: &'static str,
    pub version: u8,
    pub lossless: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lossless_scope: Option<&'static str>,
    pub source_bytes_lossless: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_cell: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode: Option<&'static str>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Transformations {
    pub normalized_bytes: usize,
    pub clipped_bytes: usize,
    pub clipped_chars: usize,
    pub clipped_lines: usize,
    pub invalid_utf8_bytes: usize,
    pub json_formatting: bool,
}

fn details(raw: &[u8]) -> (String, Transformations) {
    static ANSI_BYTES: OnceLock<regex::bytes::Regex> = OnceLock::new();
    let re = ANSI_BYTES.get_or_init(|| {
        regex::bytes::Regex::new(
            r"(?-u:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_])",
        )
        .unwrap()
    });
    let mut escapes = re.find_iter(raw).peekable();
    let mut offset = 0usize;
    let mut prefix: Vec<(char, usize, bool)> = Vec::with_capacity(LINE_CHARS);
    let (mut count, mut total_bytes, mut final_count, mut normal_bytes) =
        (0usize, 0usize, 0usize, 0usize);
    let mut record = |c: char, len: usize, invalid: bool| {
        let start = offset;
        offset += len;
        while escapes.peek().is_some_and(|m| m.end() <= start) {
            escapes.next();
        }
        if escapes.peek().is_some_and(|m| m.start() <= start)
            || (!invalid && matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'))
        {
            return;
        }
        if prefix.len() < LINE_CHARS {
            prefix.push((c, len, invalid));
        }
        count += 1;
        total_bytes += len;
        if !c.is_whitespace() && !matches!(c, '\u{1c}'..='\u{1f}') {
            final_count = count;
            normal_bytes = total_bytes;
        }
    };
    let mut remaining = raw;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                for c in text.chars() {
                    record(c, c.len_utf8(), false);
                }
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                for c in std::str::from_utf8(&remaining[..good]).unwrap().chars() {
                    record(c, c.len_utf8(), false);
                }
                let bad = e.error_len().unwrap_or(remaining.len() - good);
                for _ in 0..bad {
                    record('�', 1, true);
                }
                remaining = &remaining[good + bad..];
            }
        }
    }
    prefix.truncate(final_count.min(LINE_CHARS));
    let clipped_chars = final_count.saturating_sub(LINE_CHARS);
    let clipped_bytes = normal_bytes - prefix.iter().map(|u| u.1).sum::<usize>();
    let invalid_utf8_bytes = prefix.iter().filter(|u| u.2).count();
    let mut text: String = prefix.iter().map(|u| u.0).collect();
    if clipped_chars > 0 {
        text.push_str(&format!("…[+{clipped_chars} chars]"));
    }
    (
        text,
        Transformations {
            normalized_bytes: raw.len() - normal_bytes,
            clipped_bytes,
            clipped_chars,
            clipped_lines: usize::from(clipped_chars > 0),
            invalid_utf8_bytes,
            json_formatting: false,
        },
    )
}

fn scalar_cell(value: &oms_json::OrderedValue) -> Option<oms_json::Value> {
    match value {
        oms_json::OrderedValue::Null => Some(oms_json::Value::Null),
        oms_json::OrderedValue::Bool(v) => Some(oms_json::Value::Bool(*v)),
        oms_json::OrderedValue::Integer(n) => Some(oms_json::Value::Integer(n.clone())),
        oms_json::OrderedValue::Float(f) => Some(oms_json::Value::Float(*f)),
        oms_json::OrderedValue::String(s) => Some(oms_json::Value::String(s.clone())),
        oms_json::OrderedValue::Array(_) | oms_json::OrderedValue::Object(_) => None,
    }
}
fn all_floats_exact(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    i += 1;
                }
                let tok = &text[start..i];
                if tok.contains(['.', 'e', 'E']) {
                    let Ok(parsed) = tok.parse::<f64>() else {
                        return false;
                    };
                    if !parsed.is_finite() || decimal(tok) != decimal(&python_float(parsed)) {
                        return false;
                    }
                }
            }
            _ => i += 1,
        }
    }
    true
}
fn python_float(value: f64) -> String {
    String::from_utf8(
        oms_json::canonical_json(&oms_json::Value::Float(value)).expect("validated finite float"),
    )
    .expect("numeric JSON is ASCII")
}
// Numeric tokens preserve exact decimal identity under IEEE-754 float parsing.
fn decimal(numeric_lexeme: &str) -> Option<(bool, String, i64)> {
    let (negative, value) = numeric_lexeme
        .strip_prefix('-')
        .map_or((false, numeric_lexeme), |s| (true, s));
    let (mantissa, exponent) = value.split_once(['e', 'E']).unwrap_or((value, "0"));
    let mut exponent: i64 = exponent.parse().ok()?;
    let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    exponent = exponent.checked_sub(frac.len().try_into().ok()?)?;
    let mut digits = format!("{whole}{frac}");
    let trim = digits.trim_start_matches('0');
    if trim.is_empty() {
        return Some((negative, "0".into(), 0));
    }
    digits = trim.to_owned();
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    Some((negative, digits, exponent))
}
fn json_table(text: &str, budget: usize) -> Option<(String, usize)> {
    let limit = budget / 2;
    let mut options = oms_json::ParseOptions::strict();
    options.max_depth = 4;
    options.max_bytes = text.len().max(1);
    let parsed = oms_json::parse_ordered_with_options(text.as_bytes(), &options).ok()?;
    let oms_json::OrderedValue::Array(ref rows) = parsed else {
        return None;
    };
    if rows.len() < 2 || rows.len() >= limit {
        return None;
    }
    let row_count = rows.len();
    let mut keys = Vec::<&oms_json::PyString>::new();
    let mut seen = std::collections::HashSet::new();
    let mut objects = Vec::with_capacity(row_count);
    for row in rows {
        let items = row.as_object()?;
        for (key, value) in items.iter() {
            if matches!(
                value,
                oms_json::OrderedValue::Array(_) | oms_json::OrderedValue::Object(_)
            ) {
                return None;
            }
            if seen.insert(key) {
                keys.push(key);
                if keys.len().checked_mul(row_count)?.checked_mul(2)? > budget {
                    return None;
                }
            }
        }
        objects.push(items);
    }
    if keys.is_empty() || !all_floats_exact(text) {
        return None;
    }
    let cells = objects
        .into_iter()
        .map(|row| {
            oms_json::Value::Array(
                keys.iter()
                    .map(|key| {
                        row.get_key(key)
                            .and_then(scalar_cell)
                            .unwrap_or_else(|| oms_json::Value::Array(Vec::new()))
                    })
                    .collect(),
            )
        })
        .collect();
    let mut payload = std::collections::BTreeMap::new();
    payload.insert(
        oms_json::PyString::from("keys"),
        oms_json::Value::Array(
            keys.into_iter()
                .cloned()
                .map(oms_json::Value::String)
                .collect(),
        ),
    );
    payload.insert(
        oms_json::PyString::from("rows"),
        oms_json::Value::Array(cells),
    );
    let encoded =
        String::from_utf8(oms_json::canonical_json(&oms_json::Value::Object(payload)).ok()?)
            .ok()?;
    Some((encoded, row_count))
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
    build_tail(
        &bytes[skipped..],
        bytes.len(),
        format!("{:x}", Sha256::digest(bytes)),
        bytes.iter().filter(|&&b| b == b'\n').count()
            + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n")),
        skipped,
        options,
    )
}

/// Errors from streaming input or invalid digest options.
#[derive(Debug)]
pub enum StreamError {
    Io(io::Error),
    InvalidOptions(&'static str),
}
impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::InvalidOptions(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for StreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::InvalidOptions(_) => None,
        }
    }
}
/// Hash one observed stream while retaining at most READ_CAP source bytes.
/// This does not assert filesystem snapshot consistency or perform secret scanning.
pub fn build_reader(mut reader: impl Read, options: &Options) -> Result<OutputDigest, StreamError> {
    if !(256..=1_048_576).contains(&options.max_bytes)
        || options.head > 10000
        || options.tail > 10000
    {
        return Err(StreamError::InvalidOptions(
            "output-digest: --max-bytes is 256..1048576 and --head/--tail are 0..10000",
        ));
    }
    let mut tail: VecDeque<u8> = VecDeque::new();
    let mut size = 0usize;
    let mut newlines = 0usize;
    let mut last = None;
    let mut hash = Sha256::new();
    let mut block = [0u8; 65536];
    loop {
        let count = match reader.read(&mut block) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(StreamError::Io(e)),
        };
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count)
            .ok_or_else(|| StreamError::Io(io::Error::other("capture size overflow")))?;
        newlines += block[..count].iter().filter(|&&b| b == b'\n').count();
        last = Some(block[count - 1]);
        hash.update(&block[..count]);
        let over = tail.len().saturating_add(count).saturating_sub(READ_CAP);
        if over > 0 {
            tail.drain(..over);
        }
        tail.extend(&block[..count]);
    }
    let mut tail: Vec<u8> = tail.into();
    let mut skipped = size - tail.len();
    if skipped > 0 {
        let drop = tail
            .iter()
            .position(|&b| b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        skipped += drop;
        tail.drain(..drop);
    }
    build_tail(
        &tail,
        size,
        format!("{:x}", hash.finalize()),
        newlines + usize::from(last.is_some_and(|b| b != b'\n')),
        skipped,
        options,
    )
    .map_err(StreamError::InvalidOptions)
}

fn build_tail(
    bytes: &[u8],
    source_size: usize,
    source_hash: String,
    source_lines: usize,
    skipped: usize,
    options: &Options,
) -> Result<OutputDigest, &'static str> {
    let partial = !bytes.is_empty() && !bytes.ends_with(b"\n");
    let total = bytes.iter().filter(|&&b| b == b'\n').count() + usize::from(partial);
    let mut found = Vec::new();
    let mut matched = 0usize;
    let mut by = serde_json::Map::new();
    let mut samples: BTreeMap<usize, (&[u8], bool)> = BTreeMap::new();
    let mut tail_samples = VecDeque::new();
    let mut keep_next = false;
    for (i, line) in bytes.split(|&b| b == b'\n').take(total).enumerate() {
        let number = i + 1;
        let decoded = String::from_utf8_lossy(line);
        let stripped = ansi().replace_all(&decoded, "");
        let projected = recogniser_text(&stripped);
        let mut failure = false;
        for (name, pattern) in recognisers() {
            if pattern.is_match(&projected) {
                failure = true;
                matched += 1;
                let count = by.entry((*name).to_owned()).or_insert(serde_json::json!(0));
                *count = serde_json::json!(count.as_u64().unwrap() + 1);
                break;
            }
        }
        let first_failure = failure && found.len() < FAILURE_LINES;
        if first_failure {
            found.push(number);
        }
        if number <= options.head
            || keep_next
            || first_failure
            || (bytes.len() <= options.max_bytes && skipped == 0)
        {
            samples.insert(number, (line, failure));
        }
        keep_next = first_failure;
        if options.tail > 0 {
            tail_samples.push_back((number, line, failure));
            if tail_samples.len() > options.tail {
                tail_samples.pop_front();
            }
        }
    }
    for (number, line, failure) in tail_samples {
        samples.insert(number, (line, failure));
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
            sha256: source_hash,
            bytes: source_size,
            lines: total,
            source_lines,
            partial_last_line: partial,
            unread_leading_bytes: skipped,
            capture_ref: None,
            capture_sha256: None,
            read_tool: None,
            read_arguments: None,
            read_unavailable_reason: None,
            read_max_bytes: None,
        },
        parser,
        failures: Failures {
            matched,
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
        representation: Representation {
            algorithm: "none",
            version: 1,
            lossless: false,
            source_bytes_lossless: false,
            lossless_scope: None,
            runs: None,
            rows: None,
            missing_cell: None,
            decode: None,
        },
        transformations: Transformations::default(),
    };
    if options.withhold.as_ref().is_some_and(|s| !s.is_empty()) {
        doc.shown = Shown {
            mode: "withheld".into(),
            lines: 0,
            omitted_lines: total,
            omitted_bytes: source_size,
        };
        return Ok(doc);
    }
    let whole = if bytes.len() <= options.max_bytes && skipped == 0 {
        Some(
            bytes
                .split(|&b| b == b'\n')
                .take(total)
                .map(|line| details(line).0)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        None
    };
    if !whole
        .as_ref()
        .is_some_and(|text| text.len() <= options.max_bytes)
        && options.strategy == Strategy::Auto
        && skipped == 0
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        if let Some((encoded, row_count)) = json_table(text, options.max_bytes)
            && encoded.len() <= options.max_bytes
            && encoded.len() + 64 <= bytes.len()
            && encoded.len() * 5 <= bytes.len() * 4
        {
            doc.representation = Representation {
                algorithm: "json-table",
                version: 1,
                lossless: true,
                source_bytes_lossless: false,
                lossless_scope: Some("JSON values and scalar types"),
                runs: None,
                rows: Some(row_count),
                missing_cell: Some(serde_json::json!([])),
                decode: Some(
                    "For each row zip keys with cells; omit cells equal to []; all other cells are JSON scalars.",
                ),
            };
            doc.transformations.json_formatting = true;
            doc.shown = Shown {
                mode: "json-table".into(),
                lines: total,
                omitted_lines: 0,
                omitted_bytes: 0,
            };
            doc.failures.shown = matched;
            doc.text = encoded;
            return Ok(doc);
        }
        let mut runs: Vec<(usize, &str)> = Vec::new();
        let mut valid = total > 0;
        for raw in bytes.split(|&b| b == b'\n').take(total) {
            let line = std::str::from_utf8(raw).expect("validated source");
            let prefix = [
                "TRACE", "DEBUG", "INFO", "WARN", "WARNING", "ERROR", "FATAL",
            ]
            .iter()
            .any(|p| {
                line.strip_prefix(p).is_some_and(|rest| {
                    rest.starts_with(':')
                        || rest
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
                })
            });
            let stripped = ansi().replace_all(line, "");
            let projected = recogniser_text(&stripped);
            if !prefix
                && !recognisers()
                    .iter()
                    .any(|(_, pattern)| pattern.is_match(&projected))
            {
                valid = false;
                break;
            }
            if let Some(last) = runs.last_mut().filter(|last| last.1 == line) {
                last.0 += 1;
            } else {
                runs.push((1, line));
            }
            if runs.len() > options.max_bytes {
                valid = false;
                break;
            }
        }
        if valid && runs.iter().any(|(n, _)| *n >= 3) {
            let value = serde_json::json!({"runs": runs, "final_newline": text.ends_with('\n')});
            let encoded = serde_json::to_string(&value).expect("serializable runs");
            if encoded.len() <= options.max_bytes
                && encoded.len() + 64 <= bytes.len()
                && encoded.len() * 5 <= bytes.len() * 4
            {
                doc.representation = Representation {
                    algorithm: "repeated-lines",
                    version: 1,
                    lossless: true,
                    source_bytes_lossless: true,
                    lossless_scope: Some("source bytes"),
                    runs: Some(runs.len()),
                    rows: None,
                    missing_cell: None,
                    decode: Some(
                        "Repeat each runs [count,line] in order, join with LF; append LF only if final_newline is true.",
                    ),
                };
                doc.shown = Shown {
                    mode: "repeated-lines".into(),
                    lines: total,
                    omitted_lines: 0,
                    omitted_bytes: 0,
                };
                doc.failures.shown = matched;
                doc.text = encoded;
                return Ok(doc);
            }
        }
    }
    let mut shown: BTreeMap<usize, String> = BTreeMap::new();
    let whole_mode = whole
        .as_ref()
        .is_some_and(|text| text.len() <= options.max_bytes);
    if whole_mode {
        for (i, line) in bytes.split(|&b| b == b'\n').take(total).enumerate() {
            shown.insert(i + 1, details(line).0);
        }
    } else {
        fn gap(first: usize, last: usize) -> usize {
            if first > last {
                0
            } else {
                format!("   … {} line(s) omitted …\n", last - first + 1).len()
            }
        }
        let mut used = gap(1, total);
        let mut take = |number: usize| {
            if shown.contains_key(&number) {
                return true;
            }
            if number == 0 || number > total {
                return false;
            }
            let line = details(samples[&number].0).0;
            let before = shown.range(..number).next_back().map_or(0, |(&n, _)| n);
            let after = shown.range(number..).next().map_or(total + 1, |(&n, _)| n);
            let removed = gap(before + 1, after - 1);
            let added = format!("{number}| {line}\n").len()
                + gap(before + 1, number - 1)
                + gap(number + 1, after - 1);
            let candidate = used - removed + added;
            if candidate.saturating_sub(1) > options.max_bytes {
                return false;
            }
            used = candidate;
            shown.insert(number, line);
            true
        };
        for &number in found.iter().take(FAILURE_LINES) {
            if !take(number) {
                break;
            }
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
    }
    let mut out = Vec::new();
    let mut previous = 0;
    let mut retained = 0;
    for (&number, line) in &shown {
        if !whole_mode && number != previous + 1 {
            out.push(format!("   … {} line(s) omitted …", number - previous - 1));
        }
        out.push(if whole_mode {
            line.clone()
        } else {
            format!("{number}| {line}")
        });
        let raw_line = samples[&number].0;
        let (_, d) = details(raw_line);
        doc.transformations.normalized_bytes += d.normalized_bytes;
        doc.transformations.clipped_bytes += d.clipped_bytes;
        doc.transformations.clipped_chars += d.clipped_chars;
        doc.transformations.clipped_lines += d.clipped_lines;
        doc.transformations.invalid_utf8_bytes += d.invalid_utf8_bytes;
        retained += raw_line.len() + usize::from(number < total || !partial);
        previous = number;
    }
    if !whole_mode && previous < total {
        out.push(format!("   … {} line(s) omitted …", total - previous));
    }
    let removed = doc.transformations.normalized_bytes
        + doc.transformations.clipped_bytes
        + doc.transformations.invalid_utf8_bytes;
    let omitted = source_size - retained + removed;
    doc.shown = Shown {
        mode: if whole_mode {
            if removed == 0 {
                "whole"
            } else {
                "all-lines-transformed"
            }
        } else if doc.failures.by_recogniser.is_empty() {
            "raw head+tail"
        } else {
            "failures+tail+head"
        }
        .into(),
        lines: shown.len(),
        omitted_lines: total - shown.len(),
        omitted_bytes: omitted,
    };
    doc.representation.lossless = omitted == 0;
    doc.representation.source_bytes_lossless = omitted == 0;
    doc.failures.shown = shown.keys().filter(|n| samples[n].1).count();
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
    /// Unescaped content for an external scanner; callers must scan before delivery.
    pub fn semantic_scan_text(&self) -> String {
        let mut text = self.render();
        if self.representation.algorithm == "repeated-lines"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&self.text)
            && let Some(runs) = value["runs"].as_array()
        {
            for run in runs {
                if let Some(line) = run[1].as_str() {
                    text.push('\n');
                    text.push_str(line);
                }
            }
        }
        if self.representation.algorithm == "json-table"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&self.text)
            && let (Some(keys), Some(rows)) = (value["keys"].as_array(), value["rows"].as_array())
        {
            for row in rows {
                if let Some(cells) = row.as_array() {
                    for (key, cell) in keys.iter().zip(cells) {
                        if cell.as_array().is_some_and(|a| a.is_empty()) {
                            continue;
                        }
                        if let Some(key) = key.as_str() {
                            text.push('\n');
                            text.push_str(key);
                            text.push('=');
                            if let Some(value) = cell.as_str() {
                                text.push_str(value);
                            } else {
                                text.push_str(&cell.to_string());
                            }
                        }
                    }
                }
            }
        }
        text
    }
    /// Withhold a constructed view after an adapter scan.
    pub fn withhold(&mut self, reason: String) {
        if reason.is_empty() {
            return;
        }
        self.raw.capture_ref = None;
        self.raw.capture_sha256 = None;
        self.raw.read_tool = None;
        self.raw.read_arguments = None;
        self.raw.read_unavailable_reason = None;
        self.raw.read_max_bytes = None;
        self.withheld = Some(reason);
        self.shown = Shown {
            mode: "withheld".into(),
            lines: 0,
            omitted_lines: self.raw.lines,
            omitted_bytes: self.raw.bytes,
        };
        self.failures.shown = 0;
        self.text.clear();
        self.representation = Representation {
            algorithm: "none",
            version: 1,
            lossless: false,
            source_bytes_lossless: false,
            lossless_scope: None,
            runs: None,
            rows: None,
            missing_cell: None,
            decode: None,
        };
        self.transformations = Transformations::default();
    }
    /// Attach a scoped reference after the adapter has retained and scanned these bytes.
    /// The root supplies its captured physical spelling; the reference binds bytes,
    /// repository and filename, and grants no filesystem or retrieval authority.
    pub fn retained_capture(
        &mut self,
        root: &oms_safe_fs::Root,
        name: &str,
    ) -> Result<(), &'static str> {
        static NAME: OnceLock<Regex> = OnceLock::new();
        let valid_name =
            NAME.get_or_init(|| Regex::new(r"\A[0-9]{8}T[0-9]{6}Z-[0-9]+\.log\z").unwrap());
        let repo = root
            .physical_root()
            .to_str()
            .ok_or("invalid capture repository")?;
        if self.withheld.as_ref().is_some_and(|s| !s.is_empty())
            || self.raw.bytes > READ_CAP
            || repo.chars().any(char::is_control)
            || !valid_name.is_match(name)
            || self.raw.sha256.len() != 64
            || !self
                .raw
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid retained capture descriptor");
        }
        let scope = format!(
            "{:x}",
            Sha256::digest(format!("{repo}\0{name}\0{}", self.raw.sha256).as_bytes())
        );
        let capture_ref = format!("output:{name}:{}:{scope}", self.raw.sha256);
        self.raw.read_arguments =
            Some(serde_json::json!({"repo": repo, "capture_ref": capture_ref}));
        self.raw.capture_ref = Some(capture_ref);
        self.raw.capture_sha256 = Some(self.raw.sha256.clone());
        self.raw.read_tool = Some("oms_output_read");
        self.raw.read_unavailable_reason = None;
        self.raw.read_max_bytes = None;
        Ok(())
    }
    /// Mark a retained capture as too large for the retrieval adapter.
    pub fn retrieval_unavailable(&mut self, max_bytes: usize) -> Result<(), &'static str> {
        if self.withheld.as_ref().is_some_and(|s| !s.is_empty())
            || !(1..=READ_CAP).contains(&max_bytes)
            || self.raw.bytes <= max_bytes
        {
            return Err("invalid retrieval limit");
        }
        self.raw.capture_ref = None;
        self.raw.capture_sha256 = None;
        self.raw.read_tool = None;
        self.raw.read_arguments = None;
        self.raw.read_unavailable_reason = Some("capture_too_large");
        self.raw.read_max_bytes = Some(max_bytes);
        Ok(())
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
        if let (Some(tool), Some(args)) = (raw.read_tool, &raw.read_arguments) {
            out.push(format!(
                "read: {tool} {}",
                serde_json::to_string(args).expect("serializable arguments")
            ));
        } else if let (Some(reason), Some(limit)) =
            (raw.read_unavailable_reason, raw.read_max_bytes)
        {
            out.push(format!("read: unavailable ({reason}); MCP capture limit={limit} bytes; stored file and SHA are retained"));
        }
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
            if self.representation.algorithm != "none" {
                out.push(format!(
                    "shown: all {} source line(s) as {} v{}; lossless for {}; no data omitted",
                    shown.lines,
                    self.representation.algorithm,
                    self.representation.version,
                    self.representation.lossless_scope.unwrap_or("unknown")
                ));
                if let Some(decode) = self.representation.decode {
                    out.push(format!("decode: {decode}"));
                }
                if !self.representation.source_bytes_lossless {
                    out.push("note: JSON formatting is changed; the raw file retains the source spelling and whitespace".into());
                }
            } else if shown.mode == "whole" {
                out.push(format!(
                    "shown: all {} line(s); nothing omitted",
                    shown.lines
                ));
            } else {
                out.push(format!("shown: {} of {} line(s) ({}); omitted {} line(s), {} bytes — open the raw file for them", shown.lines, raw.lines, shown.mode, shown.omitted_lines, shown.omitted_bytes));
            }
            let t = &self.transformations;
            if t.normalized_bytes + t.clipped_bytes + t.invalid_utf8_bytes > 0 {
                out.push(format!("transformed: normalized {} source bytes; clipped {} bytes ({} chars across {} lines); replaced {} invalid UTF-8 bytes", t.normalized_bytes, t.clipped_bytes, t.clipped_chars, t.clipped_lines, t.invalid_utf8_bytes));
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
    fn stream_reports_io_errors_and_matches_bytes() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("broken stream"))
            }
        }
        assert!(matches!(
            build_reader(Broken, &Options::default()),
            Err(StreamError::Io(_))
        ));
        struct Partial(bool);
        impl std::io::Read for Partial {
            fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    target[0] = b'x';
                    Ok(1)
                } else {
                    Err(std::io::Error::other("broken after data"))
                }
            }
        }
        assert!(matches!(
            build_reader(Partial(false), &Options::default()),
            Err(StreamError::Io(_))
        ));
        let invalid = Options {
            max_bytes: 0,
            ..Options::default()
        };
        assert!(matches!(
            build_reader(b"".as_slice(), &invalid),
            Err(StreamError::InvalidOptions(_))
        ));

        for data in [b"".as_slice(), b"ok\nFAIL: bad\n", b"partial"] {
            let a = build(data, &Options::default()).unwrap();
            let b = build_reader(data, &Options::default()).unwrap();
            assert_eq!(a.json(), b.json());
        }
    }
    #[test]
    fn stream_cap_keeps_one_observed_hash_and_tail() {
        let prefix = std::io::repeat(b'x').take((READ_CAP + 7) as u64);
        let suffix = b"\nFAIL: end\n";
        let d = build_reader(prefix.chain(suffix.as_slice()), &Options::default()).unwrap();
        let mut hash = Sha256::new();
        let block = [b'x'; 65536];
        for _ in 0..READ_CAP / block.len() {
            hash.update(block);
        }
        hash.update(b"xxxxxxx\nFAIL: end\n");
        assert_eq!(d.raw.sha256, format!("{:x}", hash.finalize()));
        assert_eq!(d.raw.bytes, READ_CAP + 7 + suffix.len());
        assert_eq!(d.raw.unread_leading_bytes, READ_CAP + 8);
        assert_eq!(d.raw.lines, 1);
        assert_eq!(d.raw.source_lines, 2);
        assert!(d.json()["raw"].get("source_lines").is_none());
        assert!(d.text.contains("FAIL: end"));
        assert!(d.shown.omitted_bytes >= d.raw.unread_leading_bytes);
    }
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
    fn repeated_representation_and_withhold() {
        let expanded = format!("INFO {}\n", "x".repeat(296)).repeat(13);
        let expanded_doc = build(expanded.as_bytes(), &Options::default()).unwrap();
        assert_eq!(expanded_doc.representation.algorithm, "repeated-lines");
        let many = "FAIL: x\n".repeat(100_000);
        let many_doc = build_reader(many.as_bytes(), &Options::default()).unwrap();
        assert_eq!(many_doc.failures.matched, 100_000);
        assert_eq!(many_doc.representation.algorithm, "repeated-lines");
        let log = "INFO repeated work\n".repeat(100);
        let mut d = build(
            log.as_bytes(),
            &Options {
                max_bytes: 256,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(d.representation.algorithm, "repeated-lines");
        assert!(d.representation.source_bytes_lossless);
        assert!(d.semantic_scan_text().contains("INFO repeated work"));
        let v: serde_json::Value = serde_json::from_str(&d.text).unwrap();
        assert_eq!(v["runs"][0][0], 100);
        d.withhold("scan failed".into());
        assert!(d.text.is_empty());
        assert_eq!(d.shown.omitted_bytes, log.len());
        let legacy = build(
            log.as_bytes(),
            &Options {
                max_bytes: 256,
                strategy: Strategy::Legacy,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(legacy.representation.algorithm, "none");
    }
    #[test]
    fn json_table_retrieval_and_declines() {
        let row = r#"{"measurement":1,"name":"ordinary value"}"#;
        let source = format!("[{}]", (0..100).map(|_| row).collect::<Vec<_>>().join(","));
        let mut doc = build(
            source.as_bytes(),
            &Options {
                max_bytes: 3000,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(doc.representation.algorithm, "json-table");
        assert!(doc.transformations.json_formatting);
        assert!(doc.semantic_scan_text().contains("name=ordinary value"));
        assert!(doc.render().contains("JSON formatting is changed"));
        let capture_dir = std::env::temp_dir().join(format!(
            "oms-output-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&capture_dir).unwrap();
        let capture_root = oms_safe_fs::Root::open(&capture_dir).unwrap();
        let name = "20261112T000000Z-1.log";
        assert!(doc.retained_capture(&capture_root, "capture").is_err());
        assert!(
            doc.retained_capture(&capture_root, "20261112T000000Z-1.log\ninjected")
                .is_err()
        );
        doc.retained_capture(&capture_root, name).unwrap();
        let repo = capture_root.physical_root().to_str().unwrap();
        let scope = format!(
            "{:x}",
            Sha256::digest(format!("{repo}\0{name}\0{}", doc.raw.sha256).as_bytes())
        );
        assert_eq!(
            doc.raw.capture_ref.as_deref(),
            Some(format!("output:{name}:{}:{scope}", doc.raw.sha256).as_str())
        );
        let mut too_large = doc.clone();
        too_large.raw.bytes = READ_CAP + 1;
        assert!(too_large.retained_capture(&capture_root, name).is_err());
        assert!(doc.retrieval_unavailable(READ_CAP + 1).is_err());
        assert!(doc.render().contains("read: oms_output_read"));
        assert!(doc.retrieval_unavailable(0).is_err());
        assert!(doc.retrieval_unavailable(source.len()).is_err());
        doc.retrieval_unavailable(1).unwrap();
        assert!(doc.render().contains("capture_too_large"));
        doc.retained_capture(&capture_root, name).unwrap();
        doc.withhold(String::new());
        assert!(doc.raw.capture_ref.is_some());
        doc.withhold("blocked".into());
        assert!(doc.raw.capture_ref.is_none());
        drop(capture_root);
        std::fs::remove_dir(&capture_dir).unwrap();
        let number_source = format!(
            "[{}]",
            (0..100)
                .map(|_| r#"{"measurement":1e3,"label":"long measurement"}"#)
                .collect::<Vec<_>>()
                .join(",")
        );
        let numeric = build(number_source.as_bytes(), &Options::default()).unwrap();
        assert_eq!(numeric.representation.algorithm, "json-table");
        assert!(numeric.text.contains("1000.0"));
        let zero_source = format!(
            "[{}]",
            (0..100)
                .map(|_| r#"{"measurement":-0,"label":"long measurement"}"#)
                .collect::<Vec<_>>()
                .join(",")
        );
        let zero = build(zero_source.as_bytes(), &Options::default()).unwrap();
        assert_eq!(zero.representation.algorithm, "json-table");
        assert!(!zero.text.contains("-0"));
        let control = format!("[{}]", vec![r#"{"a":1}"#; 100].join(","));
        assert_eq!(json_table(&control, 1500).unwrap().1, 100);
        let control_doc = build(
            control.as_bytes(),
            &Options {
                max_bytes: 256,
                ..Options::default()
            },
        )
        .unwrap();
        assert_ne!(control_doc.representation.algorithm, "json-table");
        for (number, accepted) in [
            ("1000000000000000.2", true),
            ("1000000000000000.3", false),
            ("-1000000000000000.2", true),
            ("-1000000000000000.3", false),
        ] {
            if accepted {
                assert_eq!(python_float(number.parse().unwrap()), number);
            }
            let row = format!(r#"{{"v":{number},"label":"long measurement row"}}"#);
            let source = format!("[{}]", vec![row; 100].join(","));
            let d = build(
                source.as_bytes(),
                &Options {
                    max_bytes: 4700,
                    ..Options::default()
                },
            )
            .unwrap();
            assert_eq!(
                d.representation.algorithm == "json-table",
                accepted,
                "{number}"
            );
            if accepted {
                assert!(d.text.contains(number));
            }
        }
        for digits in [4300, 4301] {
            let row = format!(
                "{}{{\"v\":{},\"label\":\"long measurement row\"}}",
                " ".repeat(1500),
                "1".repeat(digits)
            );
            let source = format!("[\n{}\n]", vec![row; 10].join(",\n"));
            let d = build(
                source.as_bytes(),
                &Options {
                    max_bytes: 55_000,
                    ..Options::default()
                },
            )
            .unwrap();
            assert_eq!(
                d.representation.algorithm == "json-table",
                digits == 4300,
                "{digits}"
            );
        }
        for bad in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1e-400}"#,
            r#"{"a":[1]}"#,
            r#"{"a":{"$serde_json::private::RawValue":"\"x\""}}"#,
            r#"{"a":{"$serde_json::private::RawValue":"true"}}"#,
            r#"{"a":{"$serde_json::private::RawValue":"null"}}"#,
            r#"{"a":{"$serde_json::private::Number":"-0"}}"#,
            r#"{"a":{"$s\u0065rd\u0065_json::privat\u0065::Numb\u0065r":"1\u0065-400"}}"#,
            r#"{"a":NaN}"#,
            r#"{"a":"\ud800"}"#,
        ] {
            let source = format!("[{}]", (0..100).map(|_| bad).collect::<Vec<_>>().join(","));
            assert!(json_table(&source, 1500).is_none(), "{bad}");
            let doc = build(
                source.as_bytes(),
                &Options {
                    max_bytes: 1500,
                    ..Options::default()
                },
            )
            .unwrap();
            assert_ne!(doc.representation.algorithm, "json-table", "{bad}");
        }
    }
    #[test]
    fn loss_and_selection_regressions() {
        let d = build(&vec![b'x'; 5000], &Options::default()).unwrap();
        assert_ne!(d.shown.mode, "whole");
        assert_eq!(d.shown.omitted_bytes, 4700);
        let d = build(b"\x1b[31mvalue\x1b[0m  \n", &Options::default()).unwrap();
        assert_eq!(d.shown.mode, "all-lines-transformed");
        assert_eq!(d.shown.omitted_bytes, 11);
        let d = build(b"a\xff", &Options::default()).unwrap();
        assert_eq!(d.shown.omitted_bytes, 1);
        assert_eq!(d.transformations.invalid_utf8_bytes, 1);

        let o = Options {
            max_bytes: 256,
            head: 0,
            tail: 0,
            ..Options::default()
        };
        let d = build(&vec![b'z'; 256], &o).unwrap();
        assert_eq!(d.shown.lines, 1);
        let log = (0..100)
            .map(|i| if i % 5 == 0 { "FAIL: a\n" } else { "ok\n" })
            .collect::<String>();
        let d = build(log.as_bytes(), &o).unwrap();
        assert!(d.text.len() <= 256);
        let log = format!(
            "FAIL: {}\n{}FAIL: short\n",
            "x".repeat(300),
            "ok\n".repeat(100)
        );
        let d = build(log.as_bytes(), &Options { tail: 1, ..o }).unwrap();
        assert!(d.text.contains("FAIL: short"));
        assert_eq!(d.failures.shown, 1);
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
        for (raw, text, invalid, omitted) in [
            (b"a\xc3\x00\xa9\n".as_slice(), "a��", 2, 3),
            (b"a\xc3\r\xa9\n".as_slice(), "a��", 2, 3),
            (b"a\xe2\x1b[31m\x82\xac\n".as_slice(), "a���", 3, 8),
            (b"a\x1b]bad\xff\x07z\n".as_slice(), "az", 0, 7),
        ] {
            let d = build(raw, &Options::default()).unwrap();
            assert_eq!(d.text, text, "{raw:?}");
            assert_eq!(d.transformations.invalid_utf8_bytes, invalid, "{raw:?}");
            assert_eq!(d.shown.omitted_bytes, omitted, "{raw:?}");
        }
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
