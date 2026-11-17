//! Repository scope contracts and bounded streams, ported from `scripts/lib/path_scope.py`.
//!
//! Declarations must pass [`normalize`] before storage. Pattern validation alone
//! deliberately does not normalize or reject controls, matching the reference.
//! Wildcards span directory separators and newlines; literals include descendants.
//! Glob matching uses a bounded-state automaton, with the reference's Python
//! regex-output budget charged even though no regex is constructed.
//!
//! [`Classifier::classify_changed_path`] accepts original OS bytes and decodes
//! UTF-8 with Python's surrogateescape semantics. It never normalizes the path or
//! substitutes replacement characters. [`read_rules`] and
//! [`Classifier::classify_stream`] supply bounded adapters for the pending
//! `change-guard` cutover. File opening, regular-file checks and flushing belong
//! to consumers; these helpers never open files or mutate repository state.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, BufRead, Read, Write};
use unicode_general_category::{GeneralCategory, get_general_category};

pub const MAX_SCOPE_ITEM_BYTES: usize = 4096;
pub const MAX_BRACKET_RANGE: usize = 4096;
pub const MAX_BRACKET_EXPANSION: usize = 4096;
pub const MAX_BRACKET_ITEMS: usize = 4096;
pub const MAX_PATTERN_EXPANSION: usize = 4096;
pub const MAX_PATTERN_REGEX_OUTPUT: usize = 16384;
pub const MAX_RULE_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_RULES: usize = 8192;
pub const MAX_CLASSIFY_PATHS: usize = 100000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeError(String);

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScopeError {}

/// Streaming may fail after emitting earlier rows. I/O failures retain their
/// original kind and source so consumers can handle a broken pipe separately.
#[derive(Debug)]
pub enum ScopeStreamError {
    Scope(ScopeError),
    Io(io::Error),
}

impl fmt::Display for ScopeStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scope(err) => err.fmt(f),
            Self::Io(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for ScopeStreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Scope(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<ScopeError> for ScopeStreamError {
    fn from(err: ScopeError) -> Self {
        Self::Scope(err)
    }
}

impl From<io::Error> for ScopeStreamError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn error(message: impl Into<String>) -> ScopeError {
    ScopeError(message.into())
}

fn validate_size(points: impl IntoIterator<Item = u32>, label: &str) -> Result<(), ScopeError> {
    let mut size = 0;
    for point in points {
        // Python counts lone surrogates as three bytes before rejecting controls.
        size += match point {
            0..=0x7f => 1,
            0x80..=0x7ff => 2,
            0x800..=0xffff => 3,
            _ => 4,
        };
        if size > MAX_SCOPE_ITEM_BYTES {
            return Err(error(format!(
                "{label} exceeds the {MAX_SCOPE_ITEM_BYTES}-byte UTF-8 limit"
            )));
        }
    }
    Ok(())
}

pub fn normalize(value: &str) -> Result<String, ScopeError> {
    normalize_with_label(value, "scope path")
}

pub fn normalize_with_label(value: &str, label: &str) -> Result<String, ScopeError> {
    validate_size(value.chars().map(u32::from), label)?;
    if value.chars().any(|c| {
        matches!(
            get_general_category(c),
            GeneralCategory::Control | GeneralCategory::Format
        )
    }) {
        return Err(error(format!(
            "{label} contains a control or format character"
        )));
    }
    normalize_validated(value, label)
}

/// Python codepoints, including lone surrogates. Values above U+10FFFF are
/// rejected explicitly; they cannot occur in a Python string.
pub fn normalize_codepoints(value: &[u32], label: &str) -> Result<String, ScopeError> {
    validate_size(value.iter().copied(), label)?;
    if value.iter().any(|&p| {
        (0xd800..=0xdfff).contains(&p)
            || char::from_u32(p).is_some_and(|c| {
                matches!(
                    get_general_category(c),
                    GeneralCategory::Control | GeneralCategory::Format
                )
            })
    }) {
        return Err(error(format!(
            "{label} contains a control or format character"
        )));
    }
    let text: Option<String> = value.iter().copied().map(char::from_u32).collect();
    normalize_validated(
        &text.ok_or_else(|| error("scope input contains an invalid Unicode codepoint"))?,
        label,
    )
}

fn normalize_validated(value: &str, label: &str) -> Result<String, ScopeError> {
    let replaced = super::trim(value).replace('\\', "/");
    let mut value = replaced.as_str();
    while let Some(tail) = value.strip_prefix("./") {
        value = tail;
    }
    value = value.trim_end_matches('/');
    if value.is_empty() {
        value = ".";
    }
    let bytes = value.as_bytes();
    if value.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || (value != "." && value.split('/').any(|part| matches!(part, "" | "." | "..")))
    {
        return Err(error(format!(
            "{label} must be a normalized repo-relative path"
        )));
    }
    Ok(value.to_owned())
}

pub fn has_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

#[derive(Debug, Clone)]
enum Atom {
    Literal(u32),
    Any,
    Star,
    Class { members: Vec<u32>, negated: bool },
}

#[derive(Debug, Clone)]
enum BracketAtom {
    Char(u32),
    Set(Vec<u32>),
}

fn posix_class(name: &str) -> Result<Vec<u32>, ScopeError> {
    if !matches!(
        name,
        "alnum"
            | "alpha"
            | "ascii"
            | "blank"
            | "cntrl"
            | "digit"
            | "graph"
            | "lower"
            | "print"
            | "punct"
            | "space"
            | "upper"
            | "word"
            | "xdigit"
    ) {
        return Err(error("scope pattern has an unsupported POSIX class"));
    }
    Ok((0u8..=127)
        .filter(|&c| match name {
            "alnum" => c.is_ascii_alphanumeric(),
            "alpha" => c.is_ascii_alphabetic(),
            "ascii" => true,
            "blank" => matches!(c, b' ' | b'\t'),
            "cntrl" => c.is_ascii_control(),
            "digit" => c.is_ascii_digit(),
            "graph" => c.is_ascii_graphic(),
            "lower" => c.is_ascii_lowercase(),
            "print" => (b' '..=b'~').contains(&c),
            "punct" => c.is_ascii_punctuation(),
            "space" => matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
            "upper" => c.is_ascii_uppercase(),
            "word" => c.is_ascii_alphanumeric() || c == b'_',
            "xdigit" => c.is_ascii_hexdigit(),
            _ => unreachable!(),
        })
        .map(u32::from)
        .collect())
}

fn append_bracket(atoms: &mut Vec<BracketAtom>, atom: BracketAtom) -> Result<(), ScopeError> {
    if atoms.len() >= MAX_BRACKET_ITEMS {
        return Err(error(
            "scope pattern bracket expression has too many tokens",
        ));
    }
    atoms.push(atom);
    Ok(())
}

fn charge_expansion(local: &mut usize, total: &mut usize, amount: usize) -> Result<(), ScopeError> {
    *local += amount;
    if *local > MAX_BRACKET_EXPANSION {
        return Err(error("scope pattern bracket expansion is too large"));
    }
    if *total + amount > MAX_PATTERN_EXPANSION {
        return Err(error("scope pattern total bracket expansion is too large"));
    }
    *total += amount;
    Ok(())
}

fn bracket_members(atoms: &[BracketAtom], total: &mut usize) -> Result<Vec<u32>, ScopeError> {
    let mut members = BTreeSet::new();
    let mut local = 0;
    let mut i = 0;
    while i < atoms.len() {
        if let [
            BracketAtom::Char(first),
            BracketAtom::Char(45),
            BracketAtom::Char(last),
            ..,
        ] = &atoms[i..]
        {
            if first > last {
                return Err(error("scope pattern has a descending bracket range"));
            }
            let span = (last - first + 1) as usize;
            if span > MAX_BRACKET_RANGE {
                return Err(error("scope pattern bracket range is too large"));
            }
            charge_expansion(&mut local, total, span)?;
            members.extend(*first..=*last);
            i += 3;
        } else {
            match &atoms[i] {
                BracketAtom::Char(point) => {
                    charge_expansion(&mut local, total, 1)?;
                    members.insert(*point);
                }
                BracketAtom::Set(set) => {
                    charge_expansion(&mut local, total, set.len())?;
                    members.extend(set);
                }
            }
            i += 1;
        }
        if members.len() > MAX_BRACKET_RANGE {
            return Err(error("scope pattern bracket set is too large"));
        }
    }
    Ok(members.into_iter().collect())
}

fn class_output_size(members: &[u32], negated: bool) -> usize {
    if members.is_empty() {
        return if negated { 6 } else { 4 };
    }
    2 + usize::from(negated)
        + members
            .iter()
            .map(|&p| match p {
                0..=31 | 127 => 4,
                92 | 94 | 45 | 93 | 91 => 2,
                _ => 1,
            })
            .sum::<usize>()
}

fn bracket(
    pattern: &[char],
    start: usize,
    total: &mut usize,
) -> Result<Option<(Atom, usize, usize)>, ScopeError> {
    let mut i = start + 1;
    let negated = pattern.get(i).is_some_and(|c| matches!(c, '!' | '^'));
    i += usize::from(negated);
    let mut atoms = Vec::new();
    if pattern.get(i) == Some(&']') {
        append_bracket(&mut atoms, BracketAtom::Char(u32::from(']')))?;
        i += 1;
    }
    while i < pattern.len() {
        if pattern[i] == ']' {
            let members = bracket_members(&atoms, total)?;
            let size = class_output_size(&members, negated);
            return Ok(Some((Atom::Class { members, negated }, i + 1, size)));
        }
        if pattern[i..].starts_with(&['[', ':'])
            && let Some(end) = pattern[i + 2..].windows(2).position(|p| p == [':', ']'])
        {
            let end = i + 2 + end;
            let name: String = pattern[i + 2..end].iter().collect();
            append_bracket(&mut atoms, BracketAtom::Set(posix_class(&name)?))?;
            i = end + 2;
            continue;
        }
        if pattern[i..].starts_with(&['[', '.']) || pattern[i..].starts_with(&['[', '=']) {
            let marker = pattern[i + 1];
            if pattern[i + 2..].windows(2).any(|p| p == [marker, ']']) {
                return Err(error(
                    "scope pattern has an unsupported collating expression",
                ));
            }
        }
        append_bracket(&mut atoms, BracketAtom::Char(u32::from(pattern[i])))?;
        i += 1;
    }
    Ok(None)
}

fn charge_output(size: &mut usize, amount: usize) -> Result<(), ScopeError> {
    *size += amount;
    if *size > MAX_PATTERN_REGEX_OUTPUT {
        return Err(error("scope pattern regex output is too large"));
    }
    Ok(())
}

/// A validated pattern. Construction preserves validation order and limits;
/// matching takes O(pattern length × path codepoints) time and O(pattern length)
/// auxiliary memory, with no backtracking, global cache, or filesystem access.
#[derive(Debug, Clone)]
pub struct Pattern {
    source: String,
    atoms: Option<Vec<Atom>>,
}

pub fn validate_pattern(pattern: &str) -> Result<Pattern, ScopeError> {
    validate_size(pattern.chars().map(u32::from), "scope pattern")?;
    let mut result = Pattern {
        source: pattern.to_owned(),
        atoms: None,
    };
    if !has_glob(pattern) {
        return Ok(result);
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut atoms = Vec::new();
    let mut output = 2; // Python's \A anchor, measured in codepoints, not UTF-8.
    let mut total = 0;
    let mut i = 0;
    while i < chars.len() {
        let (atom, next, size) = match chars[i] {
            '*' => (Atom::Star, i + 1, 7),
            '?' => (Atom::Any, i + 1, 6),
            '[' => bracket(&chars, i, &mut total)?.unwrap_or((Atom::Literal(91), i + 1, 2)),
            c => {
                let escaped = "()[]{}?*+-|^$\\.&~# \t\n\r\u{b}\u{c}".contains(c);
                (Atom::Literal(u32::from(c)), i + 1, 1 + usize::from(escaped))
            }
        };
        charge_output(&mut output, size)?;
        atoms.push(atom);
        i = next;
    }
    charge_output(&mut output, 2)?; // Python's \Z anchor.
    result.atoms = Some(atoms);
    Ok(result)
}

pub fn validate_patterns(patterns: &[&str]) -> Result<Vec<Pattern>, ScopeError> {
    patterns.iter().map(|p| validate_pattern(p)).collect()
}

impl Pattern {
    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.is_match_points(path.chars().map(u32::from))
    }

    fn is_match_points(&self, points: impl IntoIterator<Item = u32>) -> bool {
        let Some(atoms) = &self.atoms else {
            if self.source == "." {
                return true;
            }
            let mut points = points.into_iter();
            for c in self.source.chars() {
                if points.next() != Some(u32::from(c)) {
                    return false;
                }
            }
            return matches!(points.next(), None | Some(47));
        };
        let mut state = vec![false; atoms.len() + 1];
        let mut next = state.clone();
        state[0] = true;
        for (i, atom) in atoms.iter().enumerate() {
            state[i + 1] = matches!(atom, Atom::Star) && state[i];
        }
        for point in points {
            next[0] = false;
            for (i, atom) in atoms.iter().enumerate() {
                next[i + 1] = match atom {
                    Atom::Star => next[i] || state[i + 1],
                    Atom::Any => state[i],
                    Atom::Literal(c) => state[i] && *c == point,
                    Atom::Class { members, negated } => {
                        state[i] && (members.binary_search(&point).is_ok() != *negated)
                    }
                };
            }
            std::mem::swap(&mut state, &mut next);
        }
        state[atoms.len()]
    }
}

pub fn matches(path: &str, pattern: &str) -> Result<bool, ScopeError> {
    // The reference only bounds literal patterns when explicitly validating them.
    if !has_glob(pattern) {
        return Ok(literal_match(path, pattern));
    }
    Ok(validate_pattern(pattern)?.is_match(path))
}

fn literal_match(path: &str, pattern: &str) -> bool {
    pattern == "."
        || path == pattern
        || path
            .strip_prefix(pattern)
            .is_some_and(|p| p.starts_with('/'))
}

pub fn matches_any(path: &str, patterns: &[&str]) -> Result<bool, ScopeError> {
    Ok(validate_patterns(patterns)?
        .iter()
        .any(|p| p.is_match(path)))
}

pub fn within_envelope(candidate: &str, envelope: &[&str]) -> Result<bool, ScopeError> {
    validate_pattern(candidate)?;
    let envelope = validate_patterns(envelope)?;
    Ok(envelope.iter().any(|p| {
        if has_glob(candidate) && has_glob(p.as_str()) {
            candidate == p.as_str()
        } else {
            p.is_match(candidate)
        }
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Forbidden,
    Outside,
    Allowed,
}

impl Classification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Outside => "outside",
            Self::Allowed => "allowed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Classifier {
    allowed: Vec<Pattern>,
    forbidden: Vec<Pattern>,
}

impl Classifier {
    /// Validates all allowed patterns before all forbidden patterns. Declarations
    /// are not normalized here; use [`validate_rules`] for stored rule inputs.
    pub fn new(allowed: &[&str], forbidden: &[&str]) -> Result<Self, ScopeError> {
        let allowed = validate_patterns(allowed)?;
        let forbidden = validate_patterns(forbidden)?;
        Ok(Self { allowed, forbidden })
    }

    pub fn classify(&self, path: &str) -> Classification {
        self.decide(|| path.chars().map(u32::from))
    }

    fn decide<I: Iterator<Item = u32>>(&self, points: impl Fn() -> I) -> Classification {
        if self.forbidden.iter().any(|p| p.is_match_points(points())) {
            Classification::Forbidden
        } else if !self.allowed.is_empty()
            && !self.allowed.iter().any(|p| p.is_match_points(points()))
        {
            Classification::Outside
        } else {
            Classification::Allowed
        }
    }

    /// Matches Python codepoints without path normalization or control rejection.
    /// Lone surrogates remain distinguishable from U+FFFD. The changed-path size
    /// limit counts them as three bytes, just as the reference does.
    pub fn classify_codepoints(&self, path: &[u32]) -> Result<Classification, ScopeError> {
        validate_size(path.iter().copied(), "changed path")?;
        if path.iter().any(|&p| p > 0x10ffff) {
            return Err(error("scope input contains an invalid Unicode codepoint"));
        }
        Ok(self.decide(|| path.iter().copied()))
    }

    /// One raw input line: removes at most one LF and then one CR, skips an
    /// empty result, and otherwise preserves every original byte. This function
    /// does not implement a stream-entry limit; [`Self::classify_changed_paths`]
    /// enforces that limit over a supplied batch; [`Self::classify_stream`]
    /// enforces it while writing rows. Use [`changed_line`] to borrow
    /// the exact cleaned path bytes when echoing a classified path.
    pub fn classify_changed_path(&self, raw: &[u8]) -> Result<Option<Classification>, ScopeError> {
        let path = changed_line(raw);
        if path.is_empty() {
            return Ok(None);
        }
        validate_size(surrogateescape(path), "changed path")?;
        Ok(Some(self.decide(|| surrogateescape(path))))
    }

    /// Pure batch classification. On error there is no partial result or output;
    /// returned path slices retain the original spelling after line cleanup.
    pub fn classify_changed_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Vec<(Classification, &'a [u8])>, ScopeError> {
        let mut result = Vec::new();
        for raw in paths {
            let path = changed_line(raw);
            if path.is_empty() {
                continue;
            }
            if result.len() >= MAX_CLASSIFY_PATHS {
                return Err(error("scope path stream has too many entries"));
            }
            validate_size(surrogateescape(path), "changed path")?;
            let class = self.decide(|| surrogateescape(path));
            result.push((class, path));
        }
        Ok(result)
    }

    /// Emits `classification\tpath\n` for each nonempty LF-delimited input row,
    /// preserving original bytes after [`changed_line`] cleanup. Unlike batch
    /// classification, earlier rows remain on a later validation or I/O error.
    /// Memory is bounded by one path and one output row, even without input LF.
    /// The count limit precedes the path-size limit. Surrogateescaped bytes are
    /// charged as three UTF-8 bytes, exactly as in single-path classification.
    ///
    /// Returns the emitted row count. The caller owns flushing, including on
    /// error, and serialization of shared output: a failed or interrupted write
    /// may leave a partial final row. This method does not retry whole rows.
    pub fn classify_stream(
        &self,
        mut input: impl BufRead,
        mut output: impl Write,
    ) -> Result<usize, ScopeStreamError> {
        let mut line = Vec::new();
        let mut row = Vec::new();
        let mut count = 0;
        loop {
            line.clear();
            // Two suffix bytes may be removed; the next byte proves oversize.
            if input
                .by_ref()
                .take((MAX_SCOPE_ITEM_BYTES + 3) as u64)
                .read_until(b'\n', &mut line)?
                == 0
            {
                return Ok(count);
            }
            let path = changed_line(&line);
            if path.is_empty() {
                continue;
            }
            if count >= MAX_CLASSIFY_PATHS {
                return Err(error("scope path stream has too many entries").into());
            }
            let class = self.classify_changed_path(&line)?.expect("nonempty path");
            row.clear();
            row.extend_from_slice(class.as_str().as_bytes());
            row.push(b'\t');
            row.extend_from_slice(path);
            row.push(b'\n');
            output.write_all(&row)?;
            count += 1;
        }
    }
}

/// Removes at most one LF and then one CR, returning borrowed original bytes.
/// Performs no normalization, decoding, or validation. An empty result means
/// skip at the stream adapter. Adapters must reuse this helper to echo the exact
/// cleaned changed path instead of duplicating line cleanup.
pub fn changed_line(raw: &[u8]) -> &[u8] {
    let line = raw.strip_suffix(b"\n").unwrap_or(raw);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn surrogateescape(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes.utf8_chunks().flat_map(|chunk| {
        chunk
            .valid()
            .chars()
            .map(u32::from)
            .chain(chunk.invalid().iter().map(|&b| 0xdc00 + u32::from(b)))
    })
}

pub fn classify(
    path: &str,
    allowed: &[&str],
    forbidden: &[&str],
) -> Result<Classification, ScopeError> {
    Ok(Classifier::new(allowed, forbidden)?.classify(path))
}

#[derive(Debug, Clone, Copy)]
pub enum RuleKind {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy)]
pub struct Rule<'a> {
    pub kind: RuleKind,
    pub path: &'a str,
}

/// Supplied, already parsed rule rows, in source order. Count and normalization
/// errors precede grammar validation; allowed grammar precedes forbidden grammar.
/// File type, encoding, byte budget, and row syntax remain the reader's contract.
pub fn validate_rules(rules: &[Rule<'_>]) -> Result<Classifier, ScopeError> {
    classifier_from_rules(rules.iter().copied().map(Ok))
}

fn classifier_from_rules<'a>(
    rules: impl IntoIterator<Item = Result<Rule<'a>, ScopeError>>,
) -> Result<Classifier, ScopeError> {
    let mut allowed = Vec::new();
    let mut forbidden = Vec::new();
    for (i, rule) in rules.into_iter().enumerate() {
        let rule = rule?;
        if i >= MAX_RULES {
            return Err(error("scope rule file has too many rules"));
        }
        let path = normalize_with_label(rule.path, "scope rule")?;
        match rule.kind {
            RuleKind::Allow => allowed.push(path),
            RuleKind::Deny => forbidden.push(path),
        }
    }
    Classifier::new(
        &allowed.iter().map(String::as_str).collect::<Vec<_>>(),
        &forbidden.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

/// Reads at most [`MAX_RULE_FILE_BYTES`] plus one byte before parsing stored
/// `allow`/`deny` TSV rows. NUL and UTF-8 checks precede row validation, unknown
/// row kinds are ignored, and all declarations validate even for an empty path
/// stream. Normalization errors precede grammar errors, with allowed grammar
/// validated first. The bounded payload is discarded after compilation.
///
/// The caller must open and check the regular file and its metadata size before
/// calling, matching the reference's distinct pre-read error. This reader also
/// enforces the byte budget in case the file grows after that check.
pub fn read_rules(input: impl Read) -> Result<Classifier, ScopeError> {
    let mut payload = Vec::new();
    input
        .take((MAX_RULE_FILE_BYTES + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(|_| error("cannot read scope rule file"))?;
    if payload.len() > MAX_RULE_FILE_BYTES || payload.contains(&0) {
        return Err(error("scope rule file is invalid or too large"));
    }
    let text = std::str::from_utf8(&payload).map_err(|_| error("scope rule file must be UTF-8"))?;
    classifier_from_rules(text.split('\n').filter_map(|raw| {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let mut fields = line.split('\t');
        let kind = match fields.next() {
            Some("allow") => RuleKind::Allow,
            Some("deny") => RuleKind::Deny,
            _ => return None,
        };
        Some(match fields.next() {
            Some(path) if !path.is_empty() && fields.next().is_none() => Ok(Rule { kind, path }),
            _ => Err(error("scope rule row is malformed")),
        })
    }))
}

pub fn normalize_list(text: &str) -> Result<Vec<String>, ScopeError> {
    let values: Result<BTreeSet<_>, _> = text
        .split(|c| c == ',' || super::ws(c))
        .filter(|s| !super::trim(s).is_empty())
        .map(normalize)
        .collect();
    let values: Vec<_> = values?.into_iter().collect();
    if values.is_empty() {
        return Err(error("scope list is empty"));
    }
    validate_patterns(&values.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(values)
}

/// Exit statuses from `normalize-scope-default`, retained without CLI I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DefaultScopeError {
    Invalid = 2,
    Empty = 3,
    Unbounded = 4,
    NotPlain = 5,
}

impl DefaultScopeError {
    pub fn exit_code(self) -> u8 {
        self as u8
    }
}

pub fn normalize_scope_default(raw: &str) -> Result<Vec<String>, DefaultScopeError> {
    if super::trim(raw).is_empty() {
        return Err(DefaultScopeError::Empty);
    }
    let items: Vec<_> = raw.split(',').map(super::trim).collect();
    if items.contains(&"") {
        return Err(DefaultScopeError::Invalid);
    }
    let items: Vec<_> = items
        .into_iter()
        .map(|s| {
            if s.len() >= 2 && s.starts_with('`') && s.ends_with('`') {
                &s[1..s.len() - 1]
            } else {
                s
            }
        })
        .collect();
    if items.contains(&".") {
        return Err(DefaultScopeError::Unbounded);
    }
    if items.iter().any(|item| {
        item.is_empty()
            || item.chars().any(|c| {
                super::ws(c)
                    || !("._/-:".contains(c)
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
                        ))
            })
    }) {
        return Err(DefaultScopeError::NotPlain);
    }
    let normalized = normalize_list(&items.join(",")).map_err(|_| DefaultScopeError::Invalid)?;
    if normalized.iter().any(|p| p == ".") {
        return Err(DefaultScopeError::Unbounded);
    }
    Ok(normalized)
}

fn static_prefix(pattern: &str) -> &str {
    let cut = pattern.find(['*', '?', '[']).unwrap_or(pattern.len());
    let head = &pattern[..cut];
    head.rfind('/').map_or("", |i| &head[..i])
}

fn prefix_overlap(one: &str, two: &str) -> bool {
    one.is_empty() || two.is_empty() || literal_match(one, two) || literal_match(two, one)
}

fn patterns_overlap(one: &Pattern, two: &Pattern) -> bool {
    match (has_glob(one.as_str()), has_glob(two.as_str())) {
        (false, false) => prefix_overlap(one.as_str(), two.as_str()),
        (true, true) => prefix_overlap(static_prefix(one.as_str()), static_prefix(two.as_str())),
        (one_glob, _) => {
            let (literal, glob) = if one_glob { (two, one) } else { (one, two) };
            glob.is_match(literal.as_str())
                || prefix_overlap(literal.as_str(), static_prefix(glob.as_str()))
        }
    }
}

/// Conservative MAY-overlap, never pattern-language intersection. `None`, an
/// empty scope, or any invalid declaration conflicts with everything. Unlike
/// Python's scheduler prefix shortcut, malformed glob/glob pairs are validated
/// before concluding disjointness. Callers must represent non-string or otherwise
/// unknown source values as `None`, rather than silently dropping them.
pub fn scopes_overlap(left: Option<&[&str]>, right: Option<&[&str]>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return true;
    };
    if left.is_empty() || right.is_empty() {
        return true;
    }
    let normalize_patterns = |items: &[&str]| -> Result<Vec<Pattern>, ScopeError> {
        items
            .iter()
            .map(|p| validate_pattern(&normalize(p)?))
            .collect()
    };
    let (Ok(left), Ok(right)) = (normalize_patterns(left), normalize_patterns(right)) else {
        return true;
    };
    left.iter()
        .any(|one| right.iter().any(|two| patterns_overlap(one, two)))
}
