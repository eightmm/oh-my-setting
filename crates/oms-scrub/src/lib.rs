//! Pure byte-oriented scanning for outbound content and retained output.
//! Errors contain no input; callers must withhold content on any error.
mod patterns;
mod unicode15 {
    // Unicode 15.0 digit ranges, corresponding to CPython 3.12 str.isdigit.
    pub const DIGITS: &[(u32, u32)] = &[
        (0x30, 0x39),
        (0xb2, 0xb3),
        (0xb9, 0xb9),
        (0x660, 0x669),
        (0x6f0, 0x6f9),
        (0x7c0, 0x7c9),
        (0x966, 0x96f),
        (0x9e6, 0x9ef),
        (0xa66, 0xa6f),
        (0xae6, 0xaef),
        (0xb66, 0xb6f),
        (0xbe6, 0xbef),
        (0xc66, 0xc6f),
        (0xce6, 0xcef),
        (0xd66, 0xd6f),
        (0xde6, 0xdef),
        (0xe50, 0xe59),
        (0xed0, 0xed9),
        (0xf20, 0xf29),
        (0x1040, 0x1049),
        (0x1090, 0x1099),
        (0x1369, 0x1371),
        (0x17e0, 0x17e9),
        (0x1810, 0x1819),
        (0x1946, 0x194f),
        (0x19d0, 0x19da),
        (0x1a80, 0x1a89),
        (0x1a90, 0x1a99),
        (0x1b50, 0x1b59),
        (0x1bb0, 0x1bb9),
        (0x1c40, 0x1c49),
        (0x1c50, 0x1c59),
        (0x2070, 0x2070),
        (0x2074, 0x2079),
        (0x2080, 0x2089),
        (0x2460, 0x2468),
        (0x2474, 0x247c),
        (0x2488, 0x2490),
        (0x24ea, 0x24ea),
        (0x24f5, 0x24fd),
        (0x24ff, 0x24ff),
        (0x2776, 0x277e),
        (0x2780, 0x2788),
        (0x278a, 0x2792),
        (0xa620, 0xa629),
        (0xa8d0, 0xa8d9),
        (0xa900, 0xa909),
        (0xa9d0, 0xa9d9),
        (0xa9f0, 0xa9f9),
        (0xaa50, 0xaa59),
        (0xabf0, 0xabf9),
        (0xff10, 0xff19),
        (0x104a0, 0x104a9),
        (0x10a40, 0x10a43),
        (0x10d30, 0x10d39),
        (0x10e60, 0x10e68),
        (0x11052, 0x1105a),
        (0x11066, 0x1106f),
        (0x110f0, 0x110f9),
        (0x11136, 0x1113f),
        (0x111d0, 0x111d9),
        (0x112f0, 0x112f9),
        (0x11450, 0x11459),
        (0x114d0, 0x114d9),
        (0x11650, 0x11659),
        (0x116c0, 0x116c9),
        (0x11730, 0x11739),
        (0x118e0, 0x118e9),
        (0x11950, 0x11959),
        (0x11c50, 0x11c59),
        (0x11d50, 0x11d59),
        (0x11da0, 0x11da9),
        (0x11f50, 0x11f59),
        (0x16a60, 0x16a69),
        (0x16ac0, 0x16ac9),
        (0x16b50, 0x16b59),
        (0x1d7ce, 0x1d7ff),
        (0x1e140, 0x1e149),
        (0x1e2f0, 0x1e2f9),
        (0x1e4f0, 0x1e4f9),
        (0x1e950, 0x1e959),
        (0x1f100, 0x1f10a),
        (0x1fbf0, 0x1fbf9),
    ];
}

use aho_corasick::AhoCorasick;
use regex::bytes::{Regex, RegexBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// Limits are checked before scanning, so an error never means clean.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENV_BYTES: usize = 1024 * 1024;
pub const MAX_ENV_ENTRIES: usize = 4096;
pub const MAX_HITS: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InputTooLarge,
    EnvironmentTooLarge,
    TooManyHits,
    PatternFailure,
    MalformedEnvironment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Tier {
    Secret,
    Machine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hit {
    pub line: usize,
    pub tier: Tier,
    pub class: &'static str,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Redacted {
    pub bytes: Vec<u8>,
    pub count: usize,
}

struct Patterns {
    sensitive: Regex,
    machine: Regex,
    reference: Regex,
    toggle: Regex,
    url: regex::Regex,
}
fn patterns() -> Result<&'static Patterns, Error> {
    static PATTERNS: OnceLock<Result<Patterns, Error>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let build = |s: &str, insensitive: bool| RegexBuilder::new(s).unicode(false).case_insensitive(insensitive).build().map_err(|_| Error::PatternFailure);
        let arg = r#"(?:[A-Za-z_][A-Za-z0-9_.]*|"[/.]*"|'[/.]*')"#;
        let call = format!(r"[A-Za-z_][A-Za-z0-9_.]*\([[:space:]]*(?:{arg}(?:[[:space:]]*,[[:space:]]*{arg})*)?[[:space:]]*\)");
        Ok(Patterns {
            sensitive: build(patterns::SENSITIVE, true)?, machine: build(patterns::MACHINE, true)?,
            reference: build(&format!(r"^([+]?[[:space:]]*)[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*({call}(?:\.{call})*)[[:space:]]*$"), false)?,
            toggle: build(concat!(r"^[+]?[[:space:]]*persist-", r"credentials", r": false[[:space:]]*$"), false)?,
            url: regex::Regex::new(r"^[a-z][a-z0-9+.-]*://[^\s\x1c-\x1f/:@]+:([^\s\x1c-\x1f/@]+)@").map_err(|_| Error::PatternFailure)?,
        })
    }).as_ref().map_err(|e| *e)
}

const KEY_BEGIN: &[u8] = concat!("-----", "begin").as_bytes();
const KEY_END: &[u8] = b"-----end";

fn contains(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}
fn find_ascii_ignore_case(haystack: &[u8], needle_lower: &[u8]) -> Option<usize> {
    haystack.windows(needle_lower.len()).position(|window| {
        window
            .iter()
            .zip(needle_lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}
fn key_block_open_after_line(mut line: &[u8], mut in_block: bool) -> bool {
    loop {
        let needle = if in_block { KEY_END } else { KEY_BEGIN };
        let Some(pos) = find_ascii_ignore_case(line, needle) else {
            return in_block;
        };
        line = &line[pos + needle.len()..];
        in_block = !in_block;
    }
}
/// Returns the 1-based line number of the first key-block END marker that
/// appears before any BEGIN marker (for example in a tail-truncated excerpt).
pub fn first_unpaired_key_end_line(input: &[u8]) -> Option<usize> {
    for (i, line) in input.split(|b| *b == b'\n').enumerate() {
        let end = find_ascii_ignore_case(line, KEY_END);
        let begin = find_ascii_ignore_case(line, KEY_BEGIN);
        match (end, begin) {
            (Some(e), Some(b)) if e < b => return Some(i + 1),
            (Some(_), None) => return Some(i + 1),
            (_, Some(_)) => return None,
            (None, None) => {}
        }
    }
    None
}
fn in_order(mut text: &[u8], parts: &[&[u8]]) -> bool {
    for part in parts {
        let Some(at) = text.windows(part.len()).position(|window| window == *part) else {
            return false;
        };
        text = &text[at + part.len()..];
    }
    true
}
fn class_secret(line: &[u8]) -> &'static str {
    let l = line.to_ascii_lowercase();
    let has = |s: &str| contains(&l, s.as_bytes());
    if has("authorization:") {
        "authorization"
    } else if has("bearer ") {
        "bearer"
    } else if in_order(&l, &[b"://", b":", b"@"]) {
        "url-credentials"
    } else if has(concat!("hooks.sl", "ack.com/services/"))
        || has(concat!("discordapp.com/api/web", "hooks/"))
    {
        "webhook-url"
    } else if in_order(&l, &[b"machine ", b"login ", b"password "]) {
        "machine-login"
    } else if ["token", "secret", "password", "credential"]
        .iter()
        .any(|s| {
            l.windows(s.len())
                .position(|w| w == s.as_bytes())
                .is_some_and(|i| l[i + s.len()..].contains(&b'='))
        })
    {
        "token-assignment"
    } else if [
        concat!("api_", "key="),
        concat!("api-", "key="),
        concat!("privat", "e_key="),
        concat!("privat", "e-key="),
    ]
    .iter()
    .any(|s| has(s))
    {
        "credential-assignment"
    } else if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|s| has(s))
    {
        "github-token"
    } else if has("npm_") {
        "npm-token"
    } else if has("hf_") {
        "hf-token"
    } else if has("glpat-") {
        "gitlab-token"
    } else if has("sk_live_") || has("sk_test_") {
        "stripe-key"
    } else if in_order(&l, &[b"eyj", b".", b"."]) {
        "jwt-like"
    } else if has("akia") {
        "aws-key"
    } else if has("aiza") {
        "google-key"
    } else if contains(&l, KEY_BEGIN) {
        "private-key"
    } else {
        "token-like"
    }
}
fn class_machine(line: &[u8]) -> &'static str {
    let l = line.to_ascii_lowercase();
    for (p, c) in [
        ("/home/", "home-path"),
        ("/users/", "user-path"),
        ("/scratch/", "scratch-path"),
        ("/lustre/", "lustre-path"),
        ("/gpfs/", "gpfs-path"),
        ("/beegfs/", "beegfs-path"),
        (concat!(".s", "sh/"), "ssh-dir"),
        (concat!(".a", "ws/"), "aws-dir"),
    ] {
        if contains(&l, p.as_bytes()) {
            return c;
        }
    }
    for (p, c) in [
        ("cluster", "cluster-field"),
        ("partition", "partition-field"),
        ("nodelist", "nodelist-field"),
    ] {
        if let Some(i) = l.windows(p.len()).position(|w| w == p.as_bytes())
            && l[i + p.len()..].iter().any(|b| *b == b'=' || *b == b':')
        {
            return c;
        }
    }
    "sbatch-partition"
}
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

// Unix Python decodes invalid environment bytes with surrogateescape; encoding
// those surrogates using utf-8/replace produces one question mark per byte.
fn env_text(bytes: &[u8]) -> String {
    let mut remaining = bytes;
    let mut text = String::with_capacity(bytes.len());
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                text.push_str(valid);
                break;
            }
            Err(error) => {
                let at = error.valid_up_to();
                text.push_str(std::str::from_utf8(&remaining[..at]).expect("validated prefix"));
                let bad = error.error_len().unwrap_or(remaining.len() - at);
                for _ in 0..bad {
                    text.push('?');
                }
                remaining = &remaining[at + bad..];
            }
        }
    }
    text
}
fn py_digit(c: char) -> bool {
    let n = c as u32;
    let i = unicode15::DIGITS.partition_point(|(start, _)| *start <= n);
    i > 0 && n <= unicode15::DIGITS[i - 1].1
}
fn usable(v: &str) -> bool {
    let chars: Vec<char> = v.chars().collect();
    if chars.len() < 8
        || chars.iter().all(|c| py_digit(*c))
        || ["true", "false", "yes", "no", "0", "1"].contains(&v.to_lowercase().as_str())
        || matches!(chars[0], '/' | '~')
        || (chars[1] == ':' && matches!(chars[2], '/' | '\\'))
    {
        return false;
    }
    chars.len() >= 16
        || chars.iter().any(|c| {
            let lower: String = c.to_lowercase().collect();
            !("a"..="z").contains(&lower.as_str())
        })
}
fn secret_name(name: &str, value: &str) -> bool {
    if let Some(tail) = name.strip_prefix("GIT_CONFIG_KEY_")
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
        && !value.contains('@')
    {
        return false;
    }
    let n = name.to_uppercase();
    let parts: Vec<&str> = n
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .collect();
    let metadata = [
        "NAME", "METHOD", "TYPE", "KIND", "MODE", "FILE", "PATH", "DIR", "ID", "URL", "VAR", "ENV",
        "FORMAT", "HEADER", "LENGTH",
    ];
    let names = [
        "KEY",
        "APIKEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "CREDENTIAL",
        "CREDENTIALS",
        "AUTHORIZATION",
    ];
    parts.last().is_some_and(|p| !metadata.contains(p)) && parts.iter().any(|p| names.contains(p))
}

struct Found {
    hits: Vec<Hit>,
    redact: BTreeMap<usize, &'static str>,
    key_blocks: BTreeSet<usize>,
}
// Bound work from adversarial overlapping multiline matches.
const MAX_MULTILINE_WORK: usize = 1_000_000;
const LINE_BLOCK: usize = 256;
fn line_at(input: &[u8], blocks: &[usize], position: usize) -> usize {
    let block = position / LINE_BLOCK;
    blocks[block]
        + input[block * LINE_BLOCK..position]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
        + 1
}
fn find(input: &[u8], env: &[(&[u8], &[u8])], machine: bool) -> Result<Found, Error> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(Error::InputTooLarge);
    }
    if env.len() > MAX_ENV_ENTRIES
        || env
            .iter()
            .try_fold(0usize, |n, (k, v)| {
                n.checked_add(k.len())?.checked_add(v.len())
            })
            .is_none_or(|n| n > MAX_ENV_BYTES)
    {
        return Err(Error::EnvironmentTooLarge);
    }
    if env.iter().any(|(name, value)| {
        name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0)
    }) {
        return Err(Error::MalformedEnvironment);
    }
    let p = patterns()?;
    let mut hits = Vec::new();
    let mut redact = BTreeMap::new();
    let mut key_blocks = BTreeSet::new();
    let mut classified = 0usize;
    for (i, line) in input.split(|b| *b == b'\n').enumerate() {
        let filtered = if p.toggle.is_match(line) {
            &[][..]
        } else if let Some(c) = p.reference.captures(line) {
            c.get(2).map_or(line, |m| m.as_bytes())
        } else {
            line
        };
        if p.sensitive.is_match(filtered) {
            let class = class_secret(filtered);
            if find_ascii_ignore_case(filtered, KEY_BEGIN).is_some() {
                key_blocks.insert(i + 1);
            }
            hits.push(Hit {
                line: i + 1,
                tier: Tier::Secret,
                class,
            });
            redact.insert(
                i + 1,
                if classified < 100 {
                    class
                } else {
                    "token-like"
                },
            );
            classified += 1;
        }
        if machine && p.machine.is_match(filtered) {
            hits.push(Hit {
                line: i + 1,
                tier: Tier::Machine,
                class: class_machine(filtered),
            });
        }
        if hits.len() > MAX_HITS {
            return Err(Error::TooManyHits);
        }
    }
    let mut single = BTreeSet::new();
    let mut multi = BTreeSet::new();
    for (name, value) in env {
        let name = env_text(name);
        let value = env_text(value);
        let v = value.trim_matches(py_space);
        let mut add = |needle: &str| {
            if needle.contains('\n') {
                multi.insert(needle.as_bytes().to_vec());
            } else {
                single.insert(needle.as_bytes().to_vec());
            }
        };
        if usable(v) && secret_name(&name, v) {
            add(v);
        }
        if let Some(c) = p.url.captures(v) {
            let candidate = c.get(1).expect("capture").as_str();
            if candidate.chars().count() >= 8
                && !["true", "false", "yes", "no", "0", "1"]
                    .contains(&candidate.to_lowercase().as_str())
            {
                add(candidate);
            }
        }
    }
    let mut env_lines = BTreeSet::new();
    if !single.is_empty() {
        let ac = AhoCorasick::new(single).map_err(|_| Error::PatternFailure)?;
        for (i, line) in input.split(|b| *b == b'\n').enumerate() {
            if ac.is_match(line) {
                env_lines.insert(i + 1);
                redact.entry(i + 1).or_insert("env-secret");
            }
            if hits.len() + env_lines.len() > MAX_HITS {
                return Err(Error::TooManyHits);
            }
        }
    }
    if !multi.is_empty() {
        let ac = AhoCorasick::new(multi).map_err(|_| Error::PatternFailure)?;
        let mut blocks = vec![0usize];
        for block in input.chunks(LINE_BLOCK) {
            blocks.push(
                blocks.last().copied().unwrap_or(0) + block.iter().filter(|b| **b == b'\n').count(),
            );
        }
        let mut expansion_work = 0usize;
        for m in ac.find_overlapping_iter(input) {
            let start = line_at(input, &blocks, m.start());
            let end = line_at(input, &blocks, m.end() - 1);
            env_lines.insert(start);
            if hits.len() + env_lines.len() > MAX_HITS || end - start >= MAX_HITS {
                return Err(Error::TooManyHits);
            }
            expansion_work = expansion_work.saturating_add(end - start + 1);
            if expansion_work > MAX_MULTILINE_WORK {
                return Err(Error::TooManyHits);
            }
            for line in start..=end {
                redact.entry(line).or_insert("env-secret");
                if redact.len() > MAX_HITS {
                    return Err(Error::TooManyHits);
                }
            }
        }
    }
    hits.extend(env_lines.into_iter().map(|line| Hit {
        line,
        tier: Tier::Secret,
        class: "env-secret",
    }));
    hits.sort_by_key(|h| (h.tier, h.line));
    Ok(Found {
        hits,
        redact,
        key_blocks,
    })
}
/// Returns metadata only. An empty vector means clean; an error requires withholding.
pub fn scan(input: &[u8], env: &[(&[u8], &[u8])]) -> Result<Vec<Hit>, Error> {
    Ok(find(input, env, true)?.hits)
}
/// Scans the secret tier for output-digest's displayed lines. Errors require withholding.
/// Multiline environment matches have a separate one-million covered-line
/// work limit; exhausting it returns `TooManyHits`.
pub fn scan_secret(input: &[u8], env: &[(&[u8], &[u8])]) -> Result<Vec<Hit>, Error> {
    Ok(find(input, env, false)?.hits)
}
/// Replaces secret-bearing lines for output-digest storage, without charging machine hits.
pub fn redact(input: &[u8], env: &[(&[u8], &[u8])]) -> Result<Redacted, Error> {
    let found = find(input, env, false)?;
    if found.redact.is_empty() {
        return Ok(Redacted {
            bytes: input.to_vec(),
            count: 0,
        });
    }
    let mut bytes = Vec::with_capacity(input.len());
    let mut count = 0;
    let mut skip = false;
    for (i, part) in input.split_inclusive(|b| *b == b'\n').enumerate() {
        if skip {
            skip = key_block_open_after_line(part, true);
            continue;
        }
        if let Some(class) = found.redact.get(&(i + 1)) {
            bytes.extend_from_slice(format!("[oms-redacted:{class}]\n").as_bytes());
            count += 1;
            if found.key_blocks.contains(&(i + 1)) {
                skip = key_block_open_after_line(part, false);
            }
        } else {
            bytes.extend_from_slice(part);
            if !part.ends_with(b"\n") {
                bytes.push(b'\n');
            }
        }
    }
    Ok(Redacted { bytes, count })
}
