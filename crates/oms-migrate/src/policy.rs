//! Static repository guardrails: crate layering, typed suppressions and
//! workaround markers. Lexical checks skip literal bodies; they are not a parser.

use crate::{Result, git, validate_path};
use oms_toml::{Table, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::{env, fs, io::ErrorKind, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layer {
    Foundation,
    Service,
    Command,
    Ui,
    Entry,
    Tool,
}
use Layer::*;

/// Every workspace crate, by layer; Cargo membership must match this table.
#[rustfmt::skip]
const LAYERS: &[(Layer, &[&str])] = &[
    (Foundation, &[
        "oms-context", "oms-context-report", "oms-context-session", "oms-context-source",
        "oms-contract-markdown", "oms-experiment", "oms-failure", "oms-github", "oms-graph",
        "oms-graph-python", "oms-graph-resolve", "oms-graph-shell", "oms-json", "oms-lock",
        "oms-output", "oms-output-capture", "oms-safe-fs", "oms-scrub", "oms-toml",
        "oms-work-inspect",
    ]),
    (Service, &["oms-bootstrap", "oms-reference", "oms-runtime", "oms-supervisor"]),
    (Command, &[
        "oms-command-agent-role", "oms-command-attempt-bundle", "oms-command-autonomy",
        "oms-command-experiment", "oms-command-failure", "oms-command-graph",
        "oms-command-hooks", "oms-command-input", "oms-command-intake", "oms-command-job-digest",
        "oms-command-journal", "oms-command-landing", "oms-command-lifecycle",
        "oms-command-memory", "oms-command-operations", "oms-command-peer", "oms-command-plan",
        "oms-command-providers-skills", "oms-command-room", "oms-command-runs",
        "oms-command-slurm-lint", "oms-command-slurm-submit", "oms-command-state",
        "oms-command-task", "oms-command-telemetry",
    ]),
    (Ui, &["oms-command-panel", "oms-panel-core"]),
    (Entry, &["oms"]),
    (Tool, &["oms-mcp-server", "oms-migrate"]),
];

/// The layers each layer may depend on (normal and build edges), and the fix.
#[rustfmt::skip]
const ALLOWED: &[(Layer, &[Layer], &str)] = &[
    (Foundation, &[Foundation], "move the shared type into a foundation crate or drop the edge"),
    (Service, &[Foundation, Service, Command], "move the shared code below the ui and entry layers"),
    (Command, &[Foundation, Service, Command], "move the shared code below the ui and entry layers"),
    (Ui, &[Foundation, Service, Command, Ui], "move the shared code from the entry crate into a library"),
    (Tool, &[Foundation, Service, Command, Ui], "move the shared code into a library crate"),
    (Entry, &[Foundation, Service, Command, Ui, Tool], ""),
];

/// Lints an untyped `#[allow]` may name; anything else needs `#[expect(.., reason)]`.
#[rustfmt::skip]
const ALLOW_LINTS: &[&str] = &[
    "clippy::too_many_arguments", "clippy::unnecessary_cast", "clippy::large_enum_variant",
    "clippy::derivable_impls", "dead_code", "unused_mut", "unused_imports", "deprecated",
];
#[rustfmt::skip]
const BLANKET_LINTS: &[&str] = &[
    "warnings", "unused", "nonstandard_style", "future_incompatible", "let_underscore",
    "rust_2018_idioms", "unsafe_code", "clippy::all", "clippy::pedantic", "clippy::nursery",
    "clippy::restriction", "clippy::cargo", "clippy::complexity", "clippy::correctness",
    "clippy::perf", "clippy::style", "clippy::suspicious",
];
/// Non-test files allowed exactly one `MODULE_ALLOW` attribute; an unused entry is stale.
const MODULE_ALLOW_PATHS: &[&str] = &[
    "crates/oms-command-panel/src/agy_child_chat.rs",
    "crates/oms-command-panel/src/agy_child_records.rs",
    "crates/oms-command-panel/src/agy_lead_custody.rs",
];
const MODULE_ALLOW: &str = "#![cfg_attr(not(test),allow(dead_code))]";
const MARKERS: &[&str] = &["FIXME", "XXX", "HACK"];
#[rustfmt::skip]
const PHRASES: &[&str] = &[
    "workaround", "for now", "remove after", "remove once", "remove when", "remove this after",
    "remove this once", "remove this when", "temporary fix", "temporary hack",
    "temporary patch", "temporary shim",
];
const CODE_STARTS: &[&str] = &[
    "let ", "fn ", "use ", "pub ", "return ", "impl ", "struct ", "enum ", "mod ",
];

fn layer(name: &str) -> Option<Layer> {
    let mut rows = LAYERS.iter();
    rows.find(|(_, names)| names.contains(&name))
        .map(|row| row.0)
}

fn layer_name(layer: Layer) -> String {
    format!("{layer:?}").to_ascii_lowercase()
}

/// Run every check; an empty result passes, an error means the checks could not run.
pub fn check(repo: &Path) -> Result<Vec<String>> {
    let mut failures = workspace_check(repo)?;
    failures.extend(source_check(repo)?);
    Ok(failures)
}

fn workspace_check(repo: &Path) -> Result<Vec<String>> {
    let read = |path: &str| {
        let text = fs::read_to_string(repo.join(path))
            .map_err(|error| format!("cannot read {path}: {error}"))?;
        oms_toml::parse_str(&text).map_err(|error| format!("cannot parse {path}: {error}"))
    };
    let root = read("Cargo.toml")?;
    let dirs = value(&root, &["workspace", "members"]).and_then(Value::as_array);
    let mut members = Vec::new();
    for dir in dirs.into_iter().flatten() {
        let dir = dir
            .as_str()
            .ok_or("Cargo.toml: workspace member is not a string")?;
        let manifest = read(&format!("{dir}/Cargo.toml"))?;
        let name = value(&manifest, &["package", "name"])
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{dir}/Cargo.toml: no package name"))?;
        members.push((dir, name.to_string(), manifest));
    }
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    #[rustfmt::skip]
    let output = Command::new(cargo).current_dir(repo).args([
        "tree", "--quiet", "--color", "never", "--locked", "--offline", "--workspace",
        "--all-features", "--target", "all", "--edges", "normal,build", "--depth", "1",
        "--no-dedupe", "--prefix", "depth", "--format", "{p}",
    ]).output().map_err(|error| format!("cannot run cargo tree: {error}"))?;
    if !output.status.success() {
        // Cargo refuses cyclic package graphs while resolving, so this is the cycle signal.
        return Err(format!(
            "cargo tree could not resolve the workspace ({}): {}\nfix the reported Cargo.toml edge (a dependency cycle or a stale Cargo.lock fails resolution); run cargo +1.99.1 tree --workspace --offline --locked to see the error",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let tree = String::from_utf8(output.stdout).map_err(|_| "cargo tree output is not UTF-8")?;
    let names = members
        .iter()
        .map(|member| member.1.as_str())
        .collect::<Vec<_>>();
    let mut failures = dependency_violations(&tree, &names)?;
    let workspace_forbids = forbids_unsafe(&root, &["workspace", "lints"]);
    for (dir, name, manifest) in &members {
        let inherits = workspace_forbids
            && value(manifest, &["lints", "workspace"]).and_then(Value::as_bool) == Some(true);
        let roots = ["lib.rs", "main.rs"]
            .map(|root| fs::read_to_string(repo.join(dir).join("src").join(root)));
        let roots_forbid = roots.iter().any(|root| root.is_ok())
            && roots.iter().all(|root| match root {
                Ok(text) => crate_attributes(text).any(|a| a == "#![forbid(unsafe_code)]"),
                Err(error) => error.kind() == ErrorKind::NotFound,
            });
        if !(inherits || roots_forbid || forbids_unsafe(manifest, &["lints"])) {
            failures.push(format!(
                "{name}: unsafe_code is not forbidden; add [lints.rust] unsafe_code = \"forbid\" to its Cargo.toml"
            ));
        }
    }
    Ok(failures)
}

fn value<'a>(table: &'a Table, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(table.get(*first)?, |value, key| value.get(key))
}

/// `rust.unsafe_code` under `lints` is `"forbid"`, bare or as `{ level = "forbid" }`.
fn forbids_unsafe(manifest: &Table, lints: &[&str]) -> bool {
    let lint = value(manifest, &[lints, &["rust", "unsafe_code"]].concat());
    lint.and_then(|lint| lint.as_str().or_else(|| lint.get("level")?.as_str())) == Some("forbid")
}

/// Check `cargo tree --prefix depth --depth 1` output against the workspace members.
pub fn dependency_violations(tree: &str, members: &[&str]) -> Result<Vec<String>> {
    let mut graph = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut owner = None;
    for row in tree.lines().filter(|row| !row.is_empty()) {
        let (depth, package) = row.split_at_checked(1).ok_or("invalid cargo tree row")?;
        let name = package.split_ascii_whitespace().next();
        match (depth, owner, name) {
            ("0", _, Some(name)) => {
                graph.entry(name).or_default();
                owner = Some(name);
            }
            ("1", Some(owner), Some(name)) => {
                graph.entry(owner).or_default().insert(name);
            }
            _ => return Err(format!("unexpected cargo tree row: {row:?}")),
        }
    }
    let resolved = members
        .iter()
        .filter(|name| graph.contains_key(*name))
        .count();
    if members.is_empty() || resolved < members.len() {
        return Err(format!(
            "dependency graph incomplete: {resolved} of {} workspace members resolved; run cargo +1.99.1 tree --workspace --offline --locked to see the error",
            members.len()
        ));
    }
    let mut failures = Vec::new();
    for (&from, dependencies) in &graph {
        let Some(from_layer) = layer(from) else {
            failures.push(format!(
                "classify {from} in crates/oms-migrate/src/policy.rs LAYERS"
            ));
            continue;
        };
        let (_, allowed, fix) = ALLOWED.iter().find(|row| row.0 == from_layer).unwrap();
        for &to in dependencies.iter().filter(|to| graph.contains_key(*to)) {
            let Some(to_layer) = layer(to) else { continue };
            if !allowed.contains(&to_layer) {
                let mut names = allowed
                    .iter()
                    .map(|layer| layer_name(*layer))
                    .collect::<Vec<_>>();
                let last = names.pop().unwrap_or_default();
                let names = match names.is_empty() {
                    true => last,
                    false => format!("{} and {last}", names.join(", ")),
                };
                let (from_name, to_name) = (layer_name(from_layer), layer_name(to_layer));
                failures.push(format!(
                    "{from} ({from_name}) depends on {to} ({to_name}): {from_name} crates may depend only on {names} crates; {fix}"
                ));
            }
        }
    }
    Ok(failures)
}

/// Check tracked and untracked, non-ignored Rust and owned Bash sources.
pub fn source_check(repo: &Path) -> Result<Vec<String>> {
    let listing = [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ];
    let paths = git(repo, &listing)?;
    let paths = paths.split(|byte| *byte == 0).filter(|raw| !raw.is_empty());
    let paths = paths
        .map(std::str::from_utf8)
        .collect::<std::result::Result<BTreeSet<_>, _>>();
    let paths = paths.map_err(|_| "repository filename is not UTF-8")?;
    let mut failures = Vec::new();
    for stale in MODULE_ALLOW_PATHS
        .iter()
        .filter(|path| !paths.contains(*path))
    {
        failures.push(stale_allow(stale));
    }
    for path in paths {
        validate_path(path)?;
        if language(path).is_none() {
            continue;
        }
        match fs::read_to_string(repo.join(path)) {
            Ok(text) => failures.extend(source_violations(path, &text)),
            // A deletion not yet staged leaves no source to check.
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot read UTF-8 source {path:?}: {error}")),
        }
    }
    Ok(failures)
}

/// `Some(true)` for Rust, `Some(false)` for owned Bash, `None` outside scope or in fixtures.
fn language(path: &str) -> Option<bool> {
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    if dirs.split('/').any(|dir| dir == "fixtures") {
        return None;
    }
    let bash = name.ends_with(".sh") || matches!(path, "scripts/oms" | "verify");
    (name.ends_with(".rs") || bash).then_some(!bash)
}

/// Check one source; fixture, unscanned and extensionless non-shell paths give no findings.
pub fn source_violations(path: &str, text: &str) -> Vec<String> {
    let mut failures = Vec::new();
    let comments = match language(path) {
        None => return failures,
        Some(true) => rust_violations(path, text, &mut failures),
        Some(false)
            if !path.ends_with(".sh") && !text.lines().next().is_some_and(shell_shebang) =>
        {
            return failures;
        }
        Some(false) => bash_violations(path, text, &mut failures),
    };
    for (line, comment) in comments {
        for (offset, row) in comment.lines().enumerate() {
            let words = row.split(|c: char| !c.is_alphanumeric() && c != '_');
            let lower = row.to_lowercase();
            let mut marker = words
                .into_iter()
                .find(|word| MARKERS.contains(word))
                .map(str::to_owned);
            if marker.is_none() {
                let phrase = PHRASES.iter().find(|phrase| lower.contains(*phrase));
                marker = phrase.map(|phrase| format!("{phrase:?}"));
            }
            if let Some(marker) = marker {
                failures.push(format!(
                    "{path}:{}: {marker} comment; fix it now or track it as an issue/plan task and remove the marker",
                    line + offset
                ));
            }
        }
    }
    failures
}

fn shell_shebang(line: &str) -> bool {
    line.starts_with("#!")
        && (line.ends_with("sh") || line.contains("bash") || line.contains("/sh "))
}

fn stale_allow(path: &str) -> String {
    format!(
        "remove {path} from MODULE_ALLOW_PATHS in crates/oms-migrate/src/policy.rs: it has no {MODULE_ALLOW}"
    )
}

/// Crate-root inner attributes, tokens joined without spaces: the leading `#![..]` run
/// before the first item or outer attribute; a nested module's inner attributes stay out.
fn crate_attributes(text: &str) -> impl Iterator<Item = String> {
    let tokens = rust_tokens(text);
    let words = tokens
        .iter()
        .filter(|t| t.2 == Kind::Code)
        .map(|t| t.0)
        .collect::<Vec<_>>();
    let mut start = 0;
    std::iter::from_fn(move || {
        // `!` at the next index also guarantees `start` is in bounds.
        let inner = words.get(start + 1) == Some(&"!");
        let end = inner.then(|| attribute_end(&words, start)).flatten()?;
        let attribute = words[start..=end].concat();
        start = end + 1;
        Some(attribute)
    })
}

fn rust_violations<'a>(
    path: &str,
    text: &'a str,
    failures: &mut Vec<String>,
) -> Vec<(usize, &'a str)> {
    let tokens = rust_tokens(text);
    let test = path.split('/').any(|part| part == "tests") || path.ends_with("/tests.rs");
    let code = tokens
        .iter()
        .filter(|token| token.2 == Kind::Code)
        .collect::<Vec<_>>();
    let words = code.iter().map(|token| token.0).collect::<Vec<_>>();
    let mut module_allow = !MODULE_ALLOW_PATHS.contains(&path);
    for (index, &&(_, line, _)) in code.iter().enumerate() {
        let Some(end) = attribute_end(&words, index) else {
            continue;
        };
        let inner = words[index + 1] == "!";
        let shown = words[index..=end].concat();
        let body = &words[index + 2 + usize::from(inner)..end];
        let mut suppressed = Vec::new();
        for (at, &name) in body.iter().enumerate() {
            if !matches!(name, "allow" | "expect") || body.get(at + 1) != Some(&"(") {
                continue;
            }
            let (reasons, lints): (Vec<_>, Vec<_>) = lint_items(&body[at + 2..])
                .into_iter()
                .partition(|item| item.starts_with("reason="));
            for lint in &lints {
                let blanket = BLANKET_LINTS.contains(&lint.as_str());
                if blanket {
                    failures.push(format!(
                        "{path}:{line}: {shown} silences the {lint} lint group; name the specific lint in #[expect(lint, reason = \"...\")]"
                    ));
                } else if name == "allow" && !ALLOW_LINTS.contains(&lint.as_str()) {
                    failures.push(format!(
                        "{path}:{line}: {shown} allows {lint}; add a reason-bearing #[expect({lint}, reason = \"...\")] or fix the code; extending the allowlist in policy.rs needs review"
                    ));
                }
            }
            if name == "expect" && reasons.iter().all(|reason| blank_lint_reason(reason)) {
                failures.push(format!(
                    "{path}:{line}: {shown} without a reason; add reason = \"...\""
                ));
            }
            suppressed.extend(lints);
        }
        if !inner || suppressed.is_empty() || test {
            continue;
        }
        if !module_allow && shown == MODULE_ALLOW {
            module_allow = true;
        } else {
            let lints = suppressed.join(", ");
            failures.push(format!(
                "{path}:{line}: {shown} suppresses lints for the whole module; move {lints} gating to the item, or add the path to MODULE_ALLOW_PATHS with review"
            ));
        }
    }
    if !module_allow {
        failures.push(stale_allow(path));
    }
    let comments = tokens.iter().filter(|token| token.2 == Kind::Comment);
    let comments = comments.map(|token| (token.1, token.0)).collect::<Vec<_>>();
    for &(line, comment) in &comments {
        let body = comment.strip_prefix("//").unwrap_or_default().trim();
        if CODE_STARTS.iter().any(|start| body.starts_with(start))
            && body.ends_with([';', '{', '}'])
        {
            failures.push(format!(
                "{path}:{line}: commented-out code; delete it (Git keeps the history) or restore it"
            ));
        }
    }
    comments
}

/// Comma-separated items of a parenthesized group, tokens joined without spaces.
fn lint_items(words: &[&str]) -> Vec<String> {
    let (mut items, mut depth) = (vec![String::new()], 1);
    for &word in words {
        match word {
            "(" | "[" => depth += 1,
            ")" | "]" if depth == 1 => break,
            ")" | "]" => depth -= 1,
            "," if depth == 1 => {
                items.push(String::new());
                continue;
            }
            _ => {}
        }
        items.last_mut().unwrap().push_str(word);
    }
    items.retain(|item| !item.is_empty());
    items
}

fn attribute_end(words: &[&str], index: usize) -> Option<usize> {
    if words[index] != "#" {
        return None;
    }
    let open = index + 1 + usize::from(words.get(index + 1) == Some(&"!"));
    if words.get(open) != Some(&"[") {
        return None;
    }
    let mut depth = 0;
    for (offset, &word) in words[open..].iter().enumerate() {
        match word {
            "[" => depth += 1,
            "]" => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Code,
    Comment,
    Doc,
}

/// Rust tokens as (text, line, kind); literals stay whole so their bodies never match.
fn rust_tokens(mut source: &str) -> Vec<(&str, usize, Kind)> {
    let (mut line, mut tokens) = (1, Vec::new());
    while let Some(first) = source.chars().next() {
        let doc = |text: &str| {
            let (third, fourth) = (
                text.get(2..).unwrap_or_default(),
                text.get(3..).unwrap_or_default(),
            );
            let outer =
                |marker| third.starts_with(marker) && !fourth.starts_with(marker) && text != "/**/";
            if outer('/') || outer('*') || third.starts_with('!') {
                Kind::Doc
            } else {
                Kind::Comment
            }
        };
        let skipped = if source.starts_with("//") {
            let end = source.find('\n').unwrap_or(source.len());
            Some((Some(doc(&source[..end])), end))
        } else if source.starts_with("/*") {
            let end = block_comment(source);
            Some((Some(doc(&source[..end])), end))
        } else if first.is_whitespace() {
            Some((None, first.len_utf8()))
        } else if let Some(body) = source.strip_prefix('"') {
            Some((
                Some(Kind::Code),
                1 + quoted(body, '"').unwrap_or(body.len()),
            ))
        } else if let Some(body) = source.strip_prefix('\'') {
            // A lifetime starts with the same glyph but has no closing quote.
            let next = body.chars().next();
            if next == Some('\\') {
                quoted(body, '\'').map(|end| (Some(Kind::Code), 1 + end))
            } else {
                next.map(char::len_utf8)
                    .filter(|&width| body[width..].starts_with('\''))
                    .map(|width| (Some(Kind::Code), width + 2))
            }
        } else {
            raw_literal(source).map(|end| (Some(Kind::Code), end))
        };
        let (kind, end) = skipped.unwrap_or_else(|| {
            let end = if first == '_' || first.is_alphanumeric() {
                source
                    .find(|c: char| c != '_' && !c.is_alphanumeric())
                    .unwrap_or(source.len())
            } else {
                first.len_utf8()
            };
            (Some(Kind::Code), end)
        });
        if let Some(kind) = kind {
            tokens.push((&source[..end], line, kind));
        }
        line += source[..end].bytes().filter(|&byte| byte == b'\n').count();
        source = &source[end..];
    }
    tokens
}

fn block_comment(source: &str) -> usize {
    let (mut depth, mut index) = (0, 0);
    while let Some(rest) = source.get(index..).filter(|rest| !rest.is_empty()) {
        if rest.starts_with("/*") {
            (depth, index) = (depth + 1, index + 2);
        } else if rest.starts_with("*/") {
            (depth, index) = (depth - 1, index + 2);
            if depth == 0 {
                return index;
            }
        } else {
            index += rest.chars().next().unwrap().len_utf8();
        }
    }
    source.len()
}

fn quoted(body: &str, delimiter: char) -> Option<usize> {
    let mut escaped = false;
    for (index, c) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            return Some(index + c.len_utf8());
        }
    }
    None
}

fn raw_literal_parts(source: &str) -> Option<(&str, &str)> {
    let tail = ["r", "br", "cr"]
        .iter()
        .find_map(|prefix| source.strip_prefix(prefix))?;
    let hashes = tail.bytes().take_while(|&b| b == b'#').count();
    Some((tail[hashes..].strip_prefix('"')?, &tail[..hashes]))
}

fn raw_literal(source: &str) -> Option<usize> {
    let (body, suffix) = raw_literal_parts(source)?;
    for (offset, _) in body.match_indices('"') {
        if body[offset + 1..].starts_with(suffix) {
            return Some(source.len() - body.len() + offset + 1 + suffix.len());
        }
    }
    Some(source.len())
}

fn blank_lint_reason(item: &str) -> bool {
    let Some(literal) = item.strip_prefix("reason=") else {
        return false;
    };
    if literal.starts_with('r')
        && let Some((body, suffix)) = raw_literal_parts(literal)
    {
        return body
            .strip_suffix(suffix)
            .and_then(|body| body.strip_suffix('"'))
            .is_some_and(|body| body.trim().is_empty());
    }
    let Some(body) = literal.strip_prefix('"') else {
        return false;
    };
    quoted(body, '"') == Some(body.len()) && cooked_whitespace(&body[..body.len() - 1])
}

/// Classify whitespace escapes only. Rustc still checks the attribute's literal syntax.
fn cooked_whitespace(mut body: &str) -> bool {
    while let Some(c) = body.chars().next() {
        body = &body[c.len_utf8()..];
        if c.is_whitespace() {
            continue;
        }
        if c != '\\' {
            return false;
        }
        let Some(escape) = body.chars().next() else {
            return false;
        };
        body = &body[escape.len_utf8()..];
        match escape {
            'n' | 'r' | 't' | '\n' => {}
            '\r' if body.starts_with('\n') => {}
            'x' => {
                let Some(digits) = body.get(..2) else {
                    return false;
                };
                if !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || !u8::from_str_radix(digits, 16)
                        .is_ok_and(|byte| byte.is_ascii() && char::from(byte).is_whitespace())
                {
                    return false;
                }
                body = &body[2..];
            }
            'u' => {
                let Some((digits, rest)) =
                    body.strip_prefix('{').and_then(|body| body.split_once('}'))
                else {
                    return false;
                };
                let hex = digits.replace('_', "");
                if digits.starts_with('_')
                    || !(1..=6).contains(&hex.len())
                    || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || !u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .is_some_and(char::is_whitespace)
                {
                    return false;
                }
                body = rest;
            }
            _ => return false,
        }
    }
    true
}

/// Full-line `#` comments after the shebang and outside heredoc bodies: `shellcheck`
/// directives must name their codes, and the rest is returned as prose for the marker scan.
fn bash_violations<'a>(
    path: &str,
    text: &'a str,
    failures: &mut Vec<String>,
) -> Vec<(usize, &'a str)> {
    let rows = text.lines().collect::<Vec<_>>();
    let (mut prose, mut skip) = (Vec::new(), 0);
    for (index, row) in rows.iter().enumerate() {
        if index < skip {
            continue;
        }
        let (line, comment) = (index + 1, row.trim_start());
        if !comment.starts_with('#') {
            // Skip bodies only when every terminator is present, in order.
            let mut next = index + 1;
            let closed = heredoc_starts(row).iter().all(|(delimiter, strip)| {
                let end = |row: &&str| {
                    row.trim_start_matches(if *strip { '\t' } else { '\0' }) == delimiter
                };
                let found = rows[next..].iter().position(end);
                found.map(|offset| next += offset + 1).is_some()
            });
            if closed {
                skip = next;
            }
            continue;
        }
        if index == 0 && row.starts_with("#!") {
            continue;
        }
        let directive = comment.trim_start_matches('#').trim_start();
        let directive = directive.strip_prefix("shellcheck");
        let Some(directive) =
            directive.filter(|rest| rest.starts_with(|c: char| c.is_ascii_whitespace()))
        else {
            prose.push((line, comment));
            continue;
        };
        for codes in directive
            .split_whitespace()
            .filter_map(|word| word.strip_prefix("disable="))
        {
            let explicit = codes.split(',').all(|code| {
                code.strip_prefix("SC")
                    .is_some_and(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()))
            });
            if !explicit {
                failures.push(format!(
                    "{path}:{line}: # shellcheck disable={codes}; name the specific SCxxxx codes and the reason"
                ));
            }
        }
    }
    prose
}

/// Unquoted `<<WORD`, `<<-WORD` and quoted-word forms before any comment, as (WORD, strip
/// tabs); `<<<` and `<<` inside `((..))` start no body, and a row ending inside a quote
/// opens nothing.
fn heredoc_starts(row: &str) -> Vec<(String, bool)> {
    let (bytes, mut found, mut quote, mut at) = (row.as_bytes(), Vec::new(), None, 0);
    while at < bytes.len() {
        let byte = bytes[at];
        let word_start = at == 0 || b" \t;&|()".contains(&bytes[at - 1]);
        match (quote, byte) {
            (Some(open), _) if open == byte => quote = None,
            (Some(b'\''), _) => {}
            (_, b'\\') => at += 1,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'#') if word_start => break,
            (None, b'<') if row[at..].starts_with("<<<") => at += 2,
            (None, b'<') if row[at..].starts_with("<<") => {
                let (before, after) = (&row[..at], &row[at + 2..]);
                let arithmetic = before.matches("((").count() > before.matches("))").count();
                let spec = after.strip_prefix('-');
                let word = spec.unwrap_or(after).trim_start_matches([' ', '\t']);
                let end = word.find(|c: char| c.is_whitespace() || ";|&<>()".contains(c));
                let delimiter = word[..end.unwrap_or(word.len())].replace(['\'', '"', '\\'], "");
                if !arithmetic
                    && delimiter.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
                {
                    found.push((delimiter, spec.is_some()));
                }
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
    if quote.is_some() { Vec::new() } else { found }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_forbid_forms_are_recognized() {
        let forbids = |text| crate_attributes(text).any(|a| a == "#![forbid(unsafe_code)]");
        assert!(forbids("//! docs\n#![forbid(unsafe_code)]\nfn main() {}\n"));
        assert!(forbids(
            "/* c */ #![allow(dead_code)]\n#![forbid(unsafe_code)]\n"
        ));
        assert!(!forbids("// #![forbid(unsafe_code)]\n"));
        assert!(!forbids(
            "mod inner {\n    #![forbid(unsafe_code)]\n}\nfn main() {}\n"
        ));
        assert!(!forbids(
            "#[cfg(test)]\nmod t {}\n#![forbid(unsafe_code)]\n"
        ));
        assert!(!forbids(""));
        let manifest = |text| forbids_unsafe(&oms_toml::parse_str(text).unwrap(), &["lints"]);
        assert!(manifest(
            "[package]\nname = \"x\"\n\n[lints.rust]\nunsafe_code = \"forbid\"\n"
        ));
        assert!(!manifest(
            "[lints.rust]\n[dependencies]\nunsafe_code = \"forbid\"\n"
        ));
        assert!(!manifest(
            "[lints.rust]\nunsafe_code = \"warn\" # \"forbid\"\n"
        ));
        assert!(manifest(
            "[lints.rust] # typed\nunsafe_code = \"forbid\" # reason\n"
        ));
    }
}
