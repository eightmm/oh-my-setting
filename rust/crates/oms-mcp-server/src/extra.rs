//! Memory recall, code slicing, failure diagnosis and state compaction tools.
//!
//! Every command-backed tool issues exactly one fixed `oms` argv through the
//! executor; request text is a single argv element, never shell-interpolated.
//! `oms_symbol_slice` never spawns anything.
use crate::value::Value;
use crate::{OUTPUT_LIMIT, Server, ToolResult, integer, prefix_chars, pretty, text};
use oms_safe_fs::{Policy, Root};
use regex::Regex;
use std::sync::OnceLock;

/// Largest slice returned, in lines.
pub(crate) const SLICE_LINES: usize = 250;
const SLICE_FILE_BYTES: usize = 2 * 1024 * 1024;
const SLICE_LINE_CHARS: usize = 200;
const SIGNATURE_LINES: usize = 40;

fn argument(args: &Value, name: &str, limit: usize) -> Result<String, String> {
    let value = text(args, name, limit)?.trim_matches(crate::whitespace);
    if value.starts_with('-') {
        return Err(format!("error: {name} cannot begin with '-'"));
    }
    Ok(value.to_owned())
}
fn boolean(args: &Value, name: &str) -> Result<bool, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| format!("error: {name} must be a boolean")),
    }
}
fn optional_integer(args: &Value, name: &str, low: i64, high: i64) -> Result<Option<i64>, String> {
    if args.get(name).is_none_or(Value::is_null) {
        return Ok(None);
    }
    integer(args, name, low, low, high).map(Some)
}

/// Fail-closed substitute for the outbound scrubber: `oms-scrub` is not a
/// dependency of this crate. Whole-file scan, so a key block that begins above
/// the requested range is still withheld.
fn secret_shaped(file: &str) -> Option<usize> {
    static FILE: OnceLock<Regex> = OnceLock::new();
    let re = FILE.get_or_init(|| {
        Regex::new(concat!(
            r"-----BEG",
            r"IN [A-Z ]*PRIV",
            r"ATE KEY-----",
            r"|\bAKIA[0-9A-Z]{16}\b",
            r"|\bgh[pousr]_[A-Za-z0-9]{30,}",
            r"|\bgithub_pat_[A-Za-z0-9_]{30,}",
            r"|\bsk-[A-Za-z0-9_-]{20,}",
            r"|\bxox[abprs]-[A-Za-z0-9-]{10,}",
            r"|\bAIza[0-9A-Za-z_-]{35}",
            r"|(?i:bearer)\s+[A-Za-z0-9._~+/-]{24,}"
        ))
        .expect("fixed pattern")
    });
    file.lines().position(|l| re.is_match(l)).map(|n| n + 1)
}
fn assignment_shaped(line: &str) -> bool {
    static ASSIGN: OnceLock<Regex> = OnceLock::new();
    ASSIGN
        .get_or_init(|| {
            Regex::new(
                r#"(?i)(api[_-]?key|secret|passwd|password|token|credential|private[_-]?key)[A-Za-z0-9_]*["']?\s*[:=]\s*["'][^"'\s]{12,}["']"#,
            )
            .expect("fixed pattern")
        })
        .is_match(line)
}
fn private_path(rel: &str) -> bool {
    const SUFFIX: [&str; 8] = [
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        ".jks",
        ".keystore",
        ".kdbx",
        ".gpg",
    ];
    const PART: [&str; 7] = [
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "credentials",
        "secrets",
        ".netrc",
    ];
    rel.split('/').any(|c| {
        let c = c.to_ascii_lowercase();
        c.starts_with(".env")
            || matches!(c.as_str(), ".git" | ".oms" | ".ssh" | ".aws" | ".gnupg")
            || SUFFIX.iter().any(|s| c.ends_with(s))
            || PART.iter().any(|p| c.contains(p))
    })
}
fn relative(path: &str) -> Result<&str, String> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..");
    if bad {
        return Err(
            "error: path must be a repository-relative path without '.', '..' or empty parts"
                .into(),
        );
    }
    Ok(path)
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}
fn comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with('#') || t.starts_with('*') || t.starts_with("/*")
}
fn definition(symbol: &str) -> Regex {
    let s = regex::escape(symbol);
    Regex::new(&format!(
        r"(\b(fn|struct|enum|trait|union|mod|type|const|static|def|class|function|interface|func|macro_rules!|let|var|namespace|object)\s+{s}\b|^\s*impl\b[^{{;]*\b{s}\b|^\s*(function\s+)?{s}\s*\(\)\s*\{{?|\b{s}\s*[:=]\s*(async\s+)?(function\b|\([^)]*\)\s*=>))"
    ))
    .expect("escaped symbol")
}
/// Zero-based end line (inclusive) of the block starting at `start`.
fn block_end(lines: &[&str], start: usize) -> usize {
    let signature_end = (start + SIGNATURE_LINES).min(lines.len());
    let mut depth = 0i32;
    let mut opened = false;
    for (i, line) in lines.iter().enumerate().skip(start) {
        // Only the signature search is bounded; an opened block runs to its close.
        if !opened && i >= signature_end {
            break;
        }
        let mut quote = None;
        let mut escaped = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if let Some(q) = quote {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '"' => quote = Some(c),
                '\'' => {
                    // Only a char literal ('x' or '\x') quotes; lifetimes do not.
                    let mut look = chars.clone();
                    let width = match look.next() {
                        Some('\\') => look.next().and(Some(2)),
                        Some(_) => Some(1),
                        None => None,
                    };
                    if let Some(width) = width
                        && look.next() == Some('\'')
                    {
                        for _ in 0..=width {
                            chars.next();
                        }
                    }
                }
                '/' if chars.peek() == Some(&'/') => break,
                '#' if !opened && i == start => break,
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' if opened => {
                    depth -= 1;
                    if depth <= 0 {
                        return i;
                    }
                }
                ';' if !opened => return i,
                _ => {}
            }
        }
        if !opened && line.trim_end().ends_with(':') && i == start {
            break;
        }
    }
    if opened {
        // Never closed: the rest of the file belongs to the block.
        return lines.len() - 1;
    }
    // Indentation block (def/class): body is every deeper or blank line.
    let base = indent(lines[start]);
    let mut end = start;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) <= base {
            break;
        }
        end = i;
    }
    end
}

impl Server<'_> {
    pub(crate) fn memory_recall(&self, tool: &Value, args: &Value) -> ToolResult {
        let repo = self.repo(args)?;
        let query = argument(args, "query", 2048)?;
        if query.is_empty() {
            return Err("error: oms_memory_recall requires a query".into());
        }
        let limit = integer(args, "limit", 10, 1, 50)?;
        let argv = vec![
            "agent-memory".into(),
            "recall".into(),
            "--json".into(),
            "--limit".into(),
            limit.to_string(),
            query,
        ];
        self.run_argv(tool, &argv, &repo)
    }
    pub(crate) fn state_compact(&self, tool: &Value, args: &Value) -> ToolResult {
        let repo = self.repo(args)?;
        let apply = boolean(args, "apply")?;
        let mut argv = vec![
            "gc".into(),
            if apply { "--apply" } else { "--dry-run" }.to_owned(),
        ];
        if let Some(days) = optional_integer(args, "keep_recent", 1, 3650)? {
            argv.extend(["--days".into(), days.to_string()]);
        }
        self.run_argv(tool, &argv, &repo)
    }
    pub(crate) fn failure_diagnose(&self, args: &Value) -> ToolResult {
        let repo = self.repo(args)?;
        let message = text(args, "text", 8192)?.trim_matches(crate::whitespace);
        let cmd = argument(args, "cmd", 4096)?;
        let exit = optional_integer(args, "exit", -255, 255)?;
        let (mode, argv): (&str, Vec<String>) = if !message.is_empty() {
            let mut argv = vec!["runtime".into(), "failure".into(), "classify".into()];
            if let Some(code) = exit {
                argv.push(format!("--exit={code}"));
            }
            argv.extend(["--".into(), message.to_owned()]);
            ("classify", argv)
        } else if !cmd.is_empty() {
            (
                "check",
                vec!["fail-ledger".into(), "check".into(), "--cmd".into(), cmd],
            )
        } else {
            (
                "list",
                ["fail-ledger", "list", "--json", "--limit", "10"]
                    .map(String::from)
                    .to_vec(),
            )
        };
        let proc = self.command(&argv, &repo, std::time::Duration::from_secs(30))?;
        // `check` exits 3 for an unchanged retry of a recorded failure.
        let answered = proc.code == 0 || (mode == "check" && proc.code == 3);
        let body = if proc.stdout.trim().is_empty() {
            proc.stderr.as_str()
        } else {
            proc.stdout.as_str()
        }
        .trim_matches(crate::whitespace);
        if !answered {
            let body = if body.is_empty() {
                format!("exit {}", proc.code)
            } else {
                prefix_chars(body, OUTPUT_LIMIT)
            };
            return Err(body.into());
        }
        let output = crate::value::from_str(body).unwrap_or_else(|_| json!(body));
        let result = pretty(&json!({"mode":mode,"exit":proc.code,"output":output}));
        if result.len() > OUTPUT_LIMIT {
            return Err(
                format!("error: diagnosis exceeds {OUTPUT_LIMIT} bytes; narrow the input").into(),
            );
        }
        Ok(result)
    }
    pub(crate) fn symbol_slice(&self, args: &Value) -> ToolResult {
        let repo = self.repo(args)?;
        let path = text(args, "path", 1024)?;
        let path = relative(path)?;
        let symbol = text(args, "symbol", 200)?;
        let has_range = args.get("start_line").is_some_and(|v| !v.is_null())
            || args.get("end_line").is_some_and(|v| !v.is_null());
        if !symbol.is_empty() && has_range {
            return Err("error: symbol excludes start_line and end_line".into());
        }
        if !symbol.is_empty()
            && !symbol
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
        {
            return Err("error: symbol must be an identifier".into());
        }
        let start = optional_integer(args, "start_line", 1, 100_000_000)?;
        let end = optional_integer(args, "end_line", 1, 100_000_000)?;
        if private_path(path) {
            return Err("error: path names private or secret-bearing state".into());
        }
        let root = Root::open(&repo).map_err(|_| "error: repository is unreadable")?;
        let bytes = root
            .read(
                path,
                SLICE_FILE_BYTES,
                Policy {
                    single_link: true,
                    owner: None,
                    max_components: Some(64),
                },
            )
            .map_err(|e| format!("error: cannot read {path}: {e:?}"))?;
        if bytes.contains(&0) {
            return Err("error: binary file refused".into());
        }
        let file = String::from_utf8_lossy(&bytes);
        if let Some(line) = secret_shaped(&file) {
            return Err(
                format!("error: secret-shaped content near line {line}; file withheld").into(),
            );
        }
        let lines: Vec<&str> = file.lines().collect();
        let total = lines.len();
        if total == 0 {
            return Err(format!("error: {path} is empty").into());
        }
        let (first, last) = if symbol.is_empty() {
            let first = start.map_or(1, |n| n as usize);
            let last = end.map_or(first + SLICE_LINES - 1, |n| n as usize);
            if first > total {
                return Err(
                    format!("error: start_line {first} is beyond the {total}-line file").into(),
                );
            }
            if last < first {
                return Err("error: end_line precedes start_line".into());
            }
            (
                first - 1,
                (last.min(first + SLICE_LINES - 1))
                    .min(total)
                    .saturating_sub(1),
            )
        } else {
            let re = definition(symbol);
            let Some(at) = lines.iter().position(|l| !comment(l) && re.is_match(l)) else {
                return Err(format!("error: no definition of {symbol} in {path}").into());
            };
            (at, block_end(&lines, at))
        };
        let shown_end = last.min(first + SLICE_LINES - 1);
        let truncated = shown_end < last;
        let slice = &lines[first..=shown_end.min(total - 1)];
        if slice.iter().any(|l| assignment_shaped(l)) {
            return Err("error: secret-shaped assignment in range; slice withheld".into());
        }
        let numbered: Vec<Value> = slice
            .iter()
            .enumerate()
            .map(|(i, l)| {
                json!(format!(
                    "{:>6}: {}",
                    first + i + 1,
                    prefix_chars(l, SLICE_LINE_CHARS)
                ))
            })
            .collect();
        Ok(pretty(&json!({
            "path": path,
            "symbol": if symbol.is_empty() { Value::Null } else { json!(symbol) },
            "start_line": first + 1,
            "end_line": shown_end + 1,
            "total_lines": total,
            "truncated": truncated,
            "lines": Value::Array(numbered)
        })))
    }
}
