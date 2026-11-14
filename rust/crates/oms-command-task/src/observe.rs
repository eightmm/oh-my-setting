//! Native verify_observe data logic. Version probes are injected read-only
//! observations, not implicit execution of tools mentioned in command output.
use crate::{Result, hash, packet, services::VerifyObservation};
use oms_safe_fs::{Policy, Root};
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::LazyLock,
};
use unicode_general_category::{GeneralCategory, get_general_category};

const LOCK_NAMES: &[&str] = &[
    "requirements.txt",
    "constraints.txt",
    "uv.lock",
    "poetry.lock",
    "Pipfile.lock",
    "pdm.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "Cargo.lock",
    "go.mod",
    "go.sum",
    "Gemfile.lock",
    "composer.lock",
    "conda-lock.yml",
];

/// Convert Python decimal digits without narrowing arbitrarily large counts.
pub fn decimal(raw: &str) -> Option<String> {
    let mut out = String::new();
    for ch in raw.chars() {
        if get_general_category(ch) != GeneralCategory::DecimalNumber {
            return None;
        }
        let mut start = ch as u32;
        while start > 0
            && char::from_u32(start - 1)
                .is_some_and(|ch| get_general_category(ch) == GeneralCategory::DecimalNumber)
        {
            start -= 1;
        }
        out.push(char::from(b'0' + ((ch as u32 - start) % 10) as u8));
    }
    if out.is_empty() {
        None
    } else {
        Some(out.trim_start_matches('0').to_owned())
            .map(|v| if v.is_empty() { "0".into() } else { v })
    }
}
fn add(a: &str, b: &str) -> String {
    let mut a = a.bytes().rev();
    let mut b = b.bytes().rev();
    let mut carry = 0;
    let mut out = Vec::new();
    loop {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() && carry == 0 {
            break;
        }
        let n = x.map_or(0, |v| v - b'0') + y.map_or(0, |v| v - b'0') + carry;
        out.push(b'0' + n % 10);
        carry = n / 10;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}
fn subtract(a: &str, b: &str) -> String {
    let mut b = b.bytes().rev();
    let mut borrow = 0i16;
    let mut out = Vec::new();
    for x in a.bytes().rev() {
        let n = i16::from(x - b'0') - i16::from(b.next().map_or(0, |v| v - b'0')) - borrow;
        borrow = i16::from(n < 0);
        out.push(b'0' + n.rem_euclid(10) as u8);
    }
    while out.len() > 1 && out.last() == Some(&b'0') {
        out.pop();
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}
fn counts(text: &str) -> BTreeMap<String, String> {
    static COUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+) ([A-Za-z]+)").unwrap());
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for c in COUNT.captures_iter(text) {
        if let Some(n) = decimal(&c[1]) {
            let v = found
                .entry(c[2].to_lowercase())
                .or_insert_with(|| "0".into());
            *v = add(v, &n);
        }
    }
    found
}
fn sum(counts: &BTreeMap<String, String>, keys: &[&str]) -> String {
    keys.iter().fold("0".into(), |a, k| {
        add(&a, counts.get(*k).map(String::as_str).unwrap_or("0"))
    })
}
fn bump(ran: &mut BTreeMap<String, String>, name: &str, amount: &str) {
    let v = ran.entry(name.into()).or_insert_with(|| "0".into());
    *v = add(v, amount);
}

/// Recognized runner counts, expressed as unbounded decimal strings. Unknown
/// output makes no claim. Ignored/skipped tests do not count as tests executed.
pub fn scan_text(raw: &[u8]) -> BTreeMap<String, String> {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
            .unwrap()
    });
    static PYTEST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^=*\s*((?:\d+ [A-Za-z]+(?:, )?)+)\s+in [\d.]+s\b").unwrap());
    static PYZERO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^=*\s*no tests ran\b").unwrap());
    static UNIT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^Ran (\d+) tests? in [\d.]+s").unwrap());
    static SKIP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^OK \(.*\bskipped=(\d+)").unwrap());
    static GOZERO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:\?\s+\S+\s+\[no test files\]|ok\s+\S+\s.*\[no tests to run\])").unwrap()
    });
    static GO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^ok\s+\S+\s+(?:\(cached\)|[\d.]+s)(?:\s+\(cached\))?(?:\s+coverage:.*)?$")
            .unwrap()
    });
    static CARGO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;").unwrap()
    });
    static JEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*Tests:\s+(.+)$").unwrap());
    static VITEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*Tests\s+(.+)$").unwrap());
    static SMOKE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-z][a-z0-9-]*: ok \((?:shard \d+/\d+, )?(\d+) tests?\b").unwrap()
    });
    static SMOKESKIP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-z][a-z0-9-]*: skipped \(shard \d+/\d+, no selected tests\)").unwrap()
    });
    static VIZERO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^no tests\b").unwrap());
    static JZERO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^No tests found, exiting with code 0\b").unwrap());
    static VZERO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^No test files found, exiting with code 0\b").unwrap());
    let mut ran = BTreeMap::new();
    let mut last_unittest = None;
    for raw in String::from_utf8_lossy(raw).split('\n') {
        let line = ANSI.replace_all(raw, "");
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if PYZERO.is_match(line) {
            bump(&mut ran, "pytest", "0");
            continue;
        }
        if let Some(c) = PYTEST.captures(line) {
            bump(
                &mut ran,
                "pytest",
                &sum(
                    &counts(&c[1]),
                    &["passed", "failed", "xfailed", "xpassed", "error", "errors"],
                ),
            );
            continue;
        }
        if let Some(c) = UNIT.captures(line)
            && let Some(n) = decimal(&c[1])
        {
            bump(&mut ran, "unittest", &n);
            last_unittest = Some(n);
            continue;
        }
        if let Some(c) = SKIP.captures(line)
            && let Some(last) = last_unittest.take()
            && let Some(n) = decimal(&c[1])
        {
            let n = if packet::decimal_cmp(n.as_bytes(), last.as_bytes()).is_gt() {
                last
            } else {
                n
            };
            if let Some(total) = ran.get_mut("unittest") {
                *total = subtract(total, &n);
            }
            continue;
        }
        if GOZERO.is_match(line) {
            bump(&mut ran, "go", "0");
            continue;
        }
        if GO.is_match(line) {
            bump(&mut ran, "go", "1");
            continue;
        }
        if let Some(c) = CARGO.captures(line)
            && let (Some(a), Some(b)) = (decimal(&c[1]), decimal(&c[2]))
        {
            bump(&mut ran, "cargo", &add(&a, &b));
            continue;
        }
        if let Some(c) = JEST.captures(line) {
            let found = counts(&c[1]);
            if found.contains_key("total") {
                bump(&mut ran, "jest", &sum(&found, &["passed", "failed"]));
            }
            continue;
        }
        if let Some(c) = VITEST.captures(line)
            && !c[1].starts_with(':')
        {
            if VIZERO.is_match(&c[1]) {
                bump(&mut ran, "vitest", "0");
            } else {
                let found = counts(&c[1]);
                if !found.is_empty() {
                    bump(&mut ran, "vitest", &sum(&found, &["passed", "failed"]));
                }
            }
            continue;
        }
        if JZERO.is_match(line) {
            bump(&mut ran, "jest", "0");
            continue;
        }
        if VZERO.is_match(line) {
            bump(&mut ran, "vitest", "0");
            continue;
        }
        if let Some(c) = SMOKE.captures(line)
            && let Some(n) = decimal(&c[1])
        {
            bump(&mut ran, "oms-smoke", &n);
            continue;
        }
        if SMOKESKIP.is_match(line) {
            bump(&mut ran, "oms-smoke", "0");
        }
    }
    ran
}
pub fn scan(raw: &[u8]) -> oms_context::Json {
    let counts = scan_text(raw);
    crate::json::object([
        (
            "runners",
            oms_context::Json::Array(counts.keys().map(crate::json::string).collect()),
        ),
        (
            "zero",
            oms_context::Json::Array(
                counts
                    .iter()
                    .filter(|(_, v)| *v == "0")
                    .map(|(name, _)| crate::json::string(name))
                    .collect(),
            ),
        ),
    ])
}
/// Version extraction follows the first stdout+stderr line; a caller must
/// supply actual bounded `--version` output, not a claimed version string.
pub fn version(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+(?:\.\d+)+").unwrap());
    let combined = [stdout, stderr].concat();
    let decoded = String::from_utf8_lossy(&combined);
    VERSION
        .find(decoded.trim().split('\n').next().unwrap_or_default())
        .map(|v| v.as_str().to_owned())
}
pub struct Fingerprint {
    pub os: String,
    pub tools: Vec<(String, String)>,
    pub locks: Vec<String>,
    pub lock_sha256: String,
}
/// The callback owns native version probes (five-second deadline, stdin closed,
/// cwd `/`, bounded stdout/stderr); None means unavailable, never a guessed
/// version. All file hashing and environment-note assembly is native here.
pub fn fingerprint(
    repo: &Path,
    runners: &[String],
    mut probe: impl FnMut(&str) -> Option<String>,
) -> Result<Fingerprint> {
    let mut wanted = vec!["python3", "bash", "git"];
    for runner in runners {
        for tool in match runner.as_str() {
            "pytest" => &["pytest"][..],
            "go" => &["go"][..],
            "cargo" => &["cargo", "rustc"][..],
            "jest" | "vitest" => &["node"][..],
            _ => &[][..],
        } {
            if !wanted.contains(tool) {
                wanted.push(tool);
            }
        }
    }
    let tools = wanted
        .into_iter()
        .filter_map(|name| probe(name).map(|v| (name.into(), v)))
        .collect();
    let uname = rustix::system::uname();
    let os = format!(
        "{}/{}",
        uname.sysname().to_string_lossy(),
        uname.machine().to_string_lossy()
    );
    let (locks, lock_sha256) = lock_digest(repo)?;
    Ok(Fingerprint {
        os,
        tools,
        locks,
        lock_sha256,
    })
}
pub fn lock_digest(repo: &Path) -> Result<(Vec<String>, String)> {
    static REQUIREMENTS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\Arequirements[A-Za-z0-9._-]*\.txt\z").unwrap());
    static TEXT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9._-]+\.txt\z").unwrap());
    let mut candidates: BTreeSet<String> = LOCK_NAMES.iter().map(|s| (*s).into()).collect();
    for folder in ["", "requirements"] {
        if let Ok(entries) = std::fs::read_dir(repo.join(folder)) {
            for item in entries.flatten() {
                if let Some(name) = item.file_name().to_str()
                    && (REQUIREMENTS.is_match(name) || !folder.is_empty() && TEXT.is_match(name))
                {
                    candidates.insert(if folder.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{folder}/{name}")
                    });
                }
            }
        }
    }
    let Ok(root) = Root::open(repo) else {
        return Ok((Vec::new(), String::new()));
    };
    let mut names = Vec::new();
    for name in candidates {
        if let Ok(info) = std::fs::symlink_metadata(repo.join(&name))
            && info.is_file()
            && info.len() <= 8 * 1024 * 1024
        {
            names.push(name);
        }
    }
    names.truncate(64);
    let mut kept = Vec::new();
    let mut outer = Vec::new();
    for name in names {
        if let Ok(bytes) = root.read_path(Path::new(&name), 8 * 1024 * 1024, Policy::source()) {
            outer.extend_from_slice(format!("{name}\0{}\n", hash(&bytes)).as_bytes());
            kept.push(name);
        }
    }
    let digest = if kept.is_empty() {
        String::new()
    } else {
        hash(&outer)
    };
    Ok((kept, digest))
}
pub fn note_lines(output: &[u8], fingerprint: &Fingerprint) -> VerifyObservation {
    let found = scan_text(output);
    let zero_runners: Vec<_> = found
        .iter()
        .filter(|(_, v)| *v == "0")
        .map(|(k, _)| k.clone())
        .collect();
    let mut parts: Vec<_> = fingerprint
        .tools
        .iter()
        .map(|(name, v)| format!("{name}={v}"))
        .collect();
    parts.push(format!("os={}", fingerprint.os));
    if !fingerprint.locks.is_empty() {
        parts.push(format!("locks={}", fingerprint.locks.len()));
        parts.push(format!(
            "lock_sha256={}",
            fingerprint.lock_sha256.chars().take(12).collect::<String>()
        ));
    }
    let mut lines = format!("env: {}", parts.join(" ")).into_bytes();
    if !zero_runners.is_empty() {
        lines.extend_from_slice(
            format!("\ntests: {} reported no tests run", zero_runners.join(", ")).as_bytes(),
        );
    }
    VerifyObservation {
        lines,
        zero_runners,
    }
}
/// Extract an actual verify run's output from existing review/admission reports.
pub fn verify_output(text: &str) -> Option<&str> {
    if text.starts_with("# gate verify") {
        let start = text.find("\n## Output\n")? + "\n## Output\n".len();
        let body = &text[start..];
        let end = [body.find("\n\n## Exit\n"), body.find("\n\n## Timeout\n")]
            .into_iter()
            .flatten()
            .min();
        return Some(end.map_or(body, |end| &body[..end]));
    }
    let marker = "\n## Verify output (tail)\n";
    let start = text.find(marker)? + marker.len();
    let body = &text[start..];
    let from = body.find("```").map_or(2, |i| i + 3);
    let end = body
        .get(from..)
        .and_then(|s| s.find("\n```\n"))
        .map(|i| i + from);
    Some(end.map_or(body, |i| &body[..i]))
}
