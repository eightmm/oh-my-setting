//! Strictly read-only, bounded project graph projection for the OMS side panel.
//!
//! Reuses existing `oms-command-graph` index, visual, query and neighbor owners
//! when a valid non-writing index is present. When no usable index exists, falls
//! back to bounded `git ls-files` tracked directory aggregation emitting only structural
//! containment edges (`relation = "contains"`, `confidence = "STRUCTURAL"`).
//! Never builds or writes index files, never reads file contents in fallback,
//! and never escapes the selected repository.

use super::{build, index::Graph};
use crate::{CommandResult, Error, Result, cli::Args, util::*};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const SCHEMA: u32 = 1;
pub const KIND: &str = "oms-panel-project-graph";
pub const MAX_LIMIT: i64 = 60;
pub const MAX_DEPTH: i64 = 3;
pub const MAX_EDGES: usize = 180;
pub const MAX_SELECTION_NEIGHBORS: usize = 32;
pub const MAX_SCAN_PATHS: usize = 20_000;
const MAX_TREE_ENTRIES: usize = 30_000;

const MAX_GIT_STDOUT_BYTES: usize = 4 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 52_000;

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".oms",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "target",
    "__pycache__",
];

const PRIVATE_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg"];

const PRIVATE_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".kdbx",
    ".gpg",
];

const PRIVATE_PARTS: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "credentials",
    "secrets",
    ".netrc",
];

pub(crate) fn validate_args(args: &Args) -> Result<()> {
    let limit = args.int("limit");
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(Error::new(format!(
            "project panel --limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    let depth = args.int("depth");
    if !(0..=MAX_DEPTH).contains(&depth) {
        return Err(Error::new(format!(
            "project panel --depth must be between 0 and {MAX_DEPTH}"
        )));
    }
    let node = args.text("node");
    let query = args.text("query");
    if !node.is_empty() && !query.is_empty() {
        return Err(Error::new(
            "project panel cannot combine --node with --query",
        ));
    }
    if !node.is_empty() && !valid_node_id(node) {
        return Err(Error::new("project panel --node has an invalid node id"));
    }
    if !query.is_empty() {
        let trimmed = query.trim();
        if trimmed.is_empty()
            || trimmed.len() > 256
            || query.chars().any(char::is_control)
            || query.contains('\0')
        {
            return Err(Error::new(
                "project panel --query must be 1-256 non-control characters",
            ));
        }
    }
    Ok(())
}

pub(crate) fn valid_node_id(value: &str) -> bool {
    if value == "dir:." {
        return true;
    }
    let Some((kind, suffix)) = value.split_once(':') else {
        return false;
    };
    let path = if kind == "symbol" {
        let Some((path, name)) = suffix.split_once("::") else {
            return false;
        };
        if name.is_empty() || name.contains('/') {
            return false;
        }
        path
    } else if matches!(kind, "dir" | "file" | "test") {
        suffix
    } else {
        return false;
    };
    value.len()<=512 && !value.contains('\\') && !value.chars().any(|c|c.is_control() || matches!(c,'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{feff}'))
        && path.split('/').all(|c| !matches!(c,""|"."|".."))
}

fn sanitize(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (count, ch) in text.chars().enumerate() {
        if count >= max_chars {
            break;
        }
        let unsafe_char = ch.is_control()
            || matches!(
                ch,
                '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
            );
        out.push(if unsafe_char { '?' } else { ch });
    }
    out
}

fn is_excluded_path(path: &str) -> bool {
    if !safe_path(path) || path.contains('\\') || path.chars().any(char::is_control) {
        return true;
    }
    path.split('/').any(|part| {
        if EXCLUDED_DIRS.contains(&part) {
            return true;
        }
        let lower = part.to_ascii_lowercase();
        lower.starts_with(".env")
            || PRIVATE_DIRS.contains(&lower.as_str())
            || PRIVATE_SUFFIXES.iter().any(|s| lower.ends_with(s))
            || PRIVATE_PARTS.iter().any(|p| lower.contains(p))
    })
}

fn language_for_path(path: &str) -> &'static str {
    if let Some((lang, _)) = build::language(path) {
        return lang;
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("md" | "mdx") => "markdown",
        Some("json" | "jsonl") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css" | "scss") => "css",
        Some("sql") => "sql",
        Some("txt") => "text",
        _ => "",
    }
}

fn repo_basename(repo: &Path) -> String {
    let name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository");
    sanitize(name, 120)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckoutKind {
    Git,
    OmsOnly,
}

fn verify_checkout(selected: &Path) -> Result<(PathBuf, CheckoutKind)> {
    let canonical = std::fs::canonicalize(selected)
        .map_err(|_| Error::new("invalid project repository: directory does not exist"))?;
    if !canonical.is_dir() {
        return Err(Error::new(
            "invalid project repository: path is not a directory",
        ));
    }
    let git_meta = canonical.join(".git").symlink_metadata();
    let has_git = match git_meta {
        Ok(m) if !m.file_type().is_symlink() && (m.is_dir() || m.is_file()) => true,
        Ok(_) => {
            return Err(Error::new(
                "invalid project repository: .git must not be a symbolic link",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(Error::new("invalid project repository: .git is unreadable"));
        }
    };
    let oms_meta = canonical.join(".oms").symlink_metadata();
    let has_oms = match oms_meta {
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => true,
        Ok(_) => {
            return Err(Error::new(
                "invalid project repository: .oms must be a real directory",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(Error::new("invalid project repository: .oms is unreadable"));
        }
    };
    if has_git {
        Ok((canonical, CheckoutKind::Git))
    } else if has_oms {
        Ok((canonical, CheckoutKind::OmsOnly))
    } else {
        Err(Error::new(
            "invalid project repository: expected a .git or .oms checkout root",
        ))
    }
}

pub(crate) fn dispatch(args: &Args, selected: &Path, cancel: &AtomicBool) -> Result<CommandResult> {
    validate_args(args)?;
    let (repo, checkout_kind) = verify_checkout(selected)?;
    let limit = args.int("limit").clamp(1, MAX_LIMIT);
    let depth = args.int("depth").clamp(0, MAX_DEPTH);
    let include_tests = args.flag("include_tests");
    let node = args.text("node");
    let query = args.text("query").trim();
    let mut payload = build_projection(
        &repo,
        checkout_kind,
        node,
        query,
        limit,
        depth,
        include_tests,
        cancel,
    )?;
    enforce_byte_budget(&mut payload, args.flag("pretty"))?;
    let out = if args.flag("json") {
        bytes(&payload, args.flag("pretty"))?
    } else {
        render_text(&payload).into_bytes()
    };
    Ok((0, out, Vec::new()))
}

fn render_text(payload: &Value) -> String {
    let mut out = format!(
        "panel graph: status={} mode={} source={} nodes={}/{} edges={}/{}\n",
        s(payload, "status"),
        s(&payload["provenance"], "mode"),
        s(&payload["provenance"], "source"),
        n(&payload["counts"], "shown_nodes"),
        n(&payload["counts"], "total_nodes"),
        n(&payload["counts"], "shown_edges"),
        n(&payload["counts"], "total_edges"),
    );
    for node in array(payload, "nodes") {
        out.push_str(&format!(
            "{}  {}  {}\n",
            s(node, "id"),
            s(node, "kind"),
            s(node, "path")
        ));
    }
    out
}

enum IndexAttempt {
    Ready {
        graph: Box<Graph>,
        revision: String,
        freshness: &'static str,
        indexed_at: Option<String>,
        coverage: Value,
    },
    Empty {
        indexed_at: Option<String>,
        coverage: Value,
    },
    Unindexed,
    Invalid(&'static str),
}

fn try_read_indexed(repo: &Path, tracked: &[String], cancel: &AtomicBool) -> Result<IndexAttempt> {
    let state = repo.join(".oms/project-graph");
    if no_symlinks(&state).is_err() {
        return Ok(IndexAttempt::Invalid(
            "project graph state traverses a symbolic link",
        ));
    }
    let graph_path = state.join("graph.json");
    let meta = match std::fs::symlink_metadata(&graph_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(IndexAttempt::Unindexed),
        Err(_) => {
            return Ok(IndexAttempt::Invalid(
                "project graph index metadata is unreadable",
            ));
        }
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Ok(IndexAttempt::Invalid(
            "project graph index is not a regular file",
        ));
    }
    if meta.len() > MAX_INDEX_BYTES as u64 {
        return Ok(IndexAttempt::Invalid(
            "project graph index exceeds the read-only panel size budget",
        ));
    }
    let mut raw_graph = match read_json(&graph_path, MAX_INDEX_BYTES) {
        Ok(Some(v)) if v.is_object() => v,
        _ => {
            return Ok(IndexAttempt::Invalid(
                "project graph index is unreadable or malformed",
            ));
        }
    };
    let revision = s(&raw_graph, "revision").to_owned();
    if raw_graph["schema"] != build::SCHEMA
        || revision.len() != 64
        || !revision.bytes().all(|b| b.is_ascii_hexdigit())
        || !raw_graph["nodes"].is_array()
        || !raw_graph["edges"].is_array()
    {
        return Ok(IndexAttempt::Invalid(
            "project graph index schema is incompatible",
        ));
    }
    if array(&raw_graph, "nodes").len() > 30_000 || array(&raw_graph, "edges").len() > 75_000 {
        return Ok(IndexAttempt::Invalid(
            "Cached graph exceeds the panel read budget.",
        ));
    }
    // General-purpose indexes may include untracked work. Rebuild the panel's
    // in-memory adjacency exclusively from identities in this checkout's Git index.
    let rows = raw_graph["nodes"].as_array_mut().unwrap();
    rows.retain(|row| {
        let path = s(row, "path");
        let id = s(row, "id");
        let Some((kind, suffix)) = id.split_once(':') else {
            return false;
        };
        matches!(kind, "file" | "test" | "symbol")
            && (suffix == path
                || (kind == "symbol" && suffix.split_once("::").is_some_and(|(p, _)| p == path)))
            && valid_node_id(id)
            && tracked
                .binary_search_by(|key| key.as_str().cmp(path))
                .is_ok()
    });
    let ids: BTreeSet<_> = rows.iter().map(|row| s(row, "id").to_owned()).collect();
    raw_graph["edges"]
        .as_array_mut()
        .unwrap()
        .retain_mut(|row| {
            let valid = ids.contains(s(row, "source"))
                && ids.contains(s(row, "target"))
                && matches!(
                    s(row, "relation"),
                    "calls" | "contains" | "imports" | "references" | "tests"
                )
                && matches!(s(row, "confidence"), "EXTRACTED" | "INFERRED" | "AMBIGUOUS");
            if let Some(object) = row.as_object_mut() {
                object.retain(|field, _| {
                    matches!(
                        field.as_str(),
                        "source" | "target" | "relation" | "confidence"
                    )
                });
            }
            valid
        });
    check(cancel)?;
    let manifest = read_json(&state.join("manifest.json"), 4 * 1024 * 1024)
        .ok()
        .flatten()
        .filter(Value::is_object);
    let (freshness, indexed_at, coverage) = match manifest.as_ref() {
        Some(m) => {
            let indexed_at = m["generated_at"]
                .as_str()
                .filter(|v| {
                    v.len() <= 32
                        && v.bytes()
                            .all(|b| b.is_ascii_digit() || b"-:+.TZ".contains(&b))
                })
                .map(str::to_owned);
            let coverage = if m["coverage"].is_object() {
                json!({"parsed":n(&m["coverage"],"parsed"),"unparsed":n(&m["coverage"],"unparsed")})
            } else {
                json!({})
            };
            let matches_current = m["schema"] == build::SCHEMA
                && s(m, "revision") == revision
                && m["parser_version"] == build::VERSION;
            let freshness = if matches_current { "cached" } else { "stale" };
            (freshness, indexed_at, coverage)
        }
        None => ("unverified", None, json!({})),
    };
    if array(&raw_graph, "nodes").is_empty() {
        return Ok(IndexAttempt::Empty {
            indexed_at,
            coverage,
        });
    }
    match Graph::new(raw_graph) {
        Ok(graph) => Ok(IndexAttempt::Ready {
            graph: Box::new(graph),
            revision,
            freshness,
            indexed_at,
            coverage,
        }),
        Err(_) => Ok(IndexAttempt::Invalid(
            "project graph index failed validation",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_projection(
    repo: &Path,
    checkout_kind: CheckoutKind,
    node: &str,
    query: &str,
    limit: i64,
    depth: i64,
    include_tests: bool,
    cancel: &AtomicBool,
) -> Result<Value> {
    check(cancel)?;
    let scan = scan_repo_paths(repo, checkout_kind, cancel)?;
    let index = match &scan {
        ScanOutcome::Paths {
            paths,
            scan_truncated: false,
            ..
        } => try_read_indexed(repo, paths, cancel)?,
        ScanOutcome::InvalidRepo(msg) => return Err(Error::new(*msg)),
        _ => IndexAttempt::Unindexed,
    };
    let (index_state, index_note, index_coverage, indexed_at) = match index {
        IndexAttempt::Ready {
            graph,
            revision,
            freshness,
            indexed_at,
            coverage,
        } => {
            if node.starts_with("dir:") {
                let mut tree = build_fallback_projection(
                    repo,
                    scan,
                    "present",
                    "This directory contains Git-tracked paths. Indexed file relationships are available on selection.",
                    &coverage,
                    indexed_at,
                    node,
                    query,
                    limit,
                    depth,
                    include_tests,
                    cancel,
                )?;
                tree["provenance"]["indexFreshness"] = json!(freshness);
                return Ok(tree);
            }
            return Ok(build_indexed_projection(
                repo,
                &graph,
                &revision,
                freshness,
                indexed_at,
                &coverage,
                node,
                query,
                limit,
                depth,
                include_tests,
            ));
        }
        IndexAttempt::Empty {
            indexed_at,
            coverage,
            ..
        } => (
            "empty",
            "Existing project graph index has 0 parsed nodes; showing bounded structural containment without writing or rebuilding the index.",
            coverage,
            indexed_at,
        ),
        IndexAttempt::Unindexed => (
            "unindexed",
            "No project graph index found; showing bounded structural containment from repository paths without building an index or synthesizing dependency edges.",
            json!({}),
            None,
        ),
        IndexAttempt::Invalid(reason) => ("invalid", reason, json!({}), None),
    };
    build_fallback_projection(
        repo,
        scan,
        index_state,
        index_note,
        &index_coverage,
        indexed_at,
        node,
        query,
        limit,
        depth,
        include_tests,
        cancel,
    )
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "EXTRACTED" => 0,
        "INFERRED" => 1,
        "AMBIGUOUS" => 2,
        "STRUCTURAL" => 3,
        _ => 4,
    }
}

fn dir_breadcrumbs(repo_name: &str, path: &str, include_leaf_dir: bool) -> Vec<Value> {
    let mut crumbs = vec![json!({
        "id": "dir:.",
        "label": repo_name,
        "kind": "directory",
        "path": "",
    })];
    if path.is_empty() {
        return crumbs;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let end = if include_leaf_dir {
        parts.len()
    } else {
        parts.len().saturating_sub(1)
    };
    let mut acc = String::new();
    for part in &parts[..end] {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        crumbs.push(json!({
            "id": format!("dir:{acc}"),
            "label": sanitize(part, 120),
            "kind": "directory",
            "path": sanitize(&acc, 260),
        }));
    }
    crumbs
}

fn count_edge_confidences(edges: &[Value]) -> Value {
    let mut counts = BTreeMap::<String, usize>::new();
    for edge in edges {
        let conf = s(edge, "confidence");
        if !conf.is_empty() {
            *counts.entry(conf.to_owned()).or_default() += 1;
        }
    }
    json!(counts)
}

fn indexed_row(row: &Value) -> Value {
    let id = s(row, "id");
    let path = s(row, "path");
    let (_, suffix) = id.split_once(':').unwrap_or(("", id));
    let label = if let Some((_, symbol)) = suffix.split_once("::") {
        symbol
    } else {
        path.rsplit('/').next().unwrap_or(path)
    };
    let kind = match s(row, "kind") {
        known @ ("file" | "test" | "module" | "function" | "class" | "method" | "symbol"
        | "type" | "struct" | "enum" | "trait" | "interface" | "constant" | "variable") => known,
        _ => id.split_once(':').map_or("symbol", |(kind, _)| kind),
    };
    json!({"id":id,"path":path,"kind":kind,"language":language_for_path(path),"label":sanitize(label,120)})
}

// A matching manifest version alone cannot detect an unindexed working-tree edit.
// Only the selected, tracked symbol may contribute declaration text.
fn selected_definition(
    repo: &Path,
    graph: &Graph,
    symbol: &Value,
    freshness: &str,
) -> Option<Value> {
    let path = s(symbol, "path");
    let metadata = &symbol["metadata"];
    if freshness != "cached"
        || !s(symbol, "id").starts_with("symbol:")
        || is_excluded_path(path)
        || metadata["span_complete"] == false
        || metadata["signature_truncated"] == true
        || metadata["scope_truncated"] == true
    {
        return None;
    }
    let signature = metadata["signature"].as_str()?;
    let line = metadata["line"].as_u64()?;
    let end_line = metadata["end_line"].as_u64()?;
    if line == 0
        || end_line < line
        || signature.is_empty()
        || signature.len() > 320
        || signature.trim() != signature
        || signature
            .chars()
            .any(|c| !(c.is_alphanumeric() || " _:&<>,()[]?+-*!.='".contains(c)))
        || signature.contains('\'')
            && (s(symbol, "language") != "rust"
                || signature
                    .split('\'')
                    .skip(1)
                    .any(|tail| !tail.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')))
        || scrub(signature) != signature
    {
        return None;
    }
    let file = graph
        .nodes
        .get(&format!("file:{path}"))
        .or_else(|| graph.nodes.get(&format!("test:{path}")))?;
    let digest = s(symbol, "source_digest");
    if digest.len() != 64 || s(file, "source_digest") != digest {
        return None;
    }
    let data = oms_safe_fs::Root::open(repo)
        .ok()?
        .read(
            path,
            2 * 1024 * 1024,
            oms_safe_fs::Policy {
                single_link: true,
                ..oms_safe_fs::Policy::source()
            },
        )
        .ok()?;
    if sha(&data) != digest {
        return None;
    }
    let text = std::str::from_utf8(&data).ok()?;
    if end_line > text.lines().count() as u64 {
        return None;
    }
    // Do not emit synthesized signatures, body excerpts or a signature stored
    // at an unrelated location. Match only this bounded declaration prefix.
    let header = text
        .lines()
        .skip((line - 1) as usize)
        .take(((end_line - line + 1).min(12)) as usize)
        .flat_map(str::split_whitespace)
        .flat_map(|word| word.chars().chain(std::iter::once(' ')))
        .take(640)
        .collect::<String>();
    let name = s(symbol, "name");
    if name.is_empty() {
        return None;
    }
    let language = s(symbol, "language");
    let header = if language == "shell" {
        header.strip_prefix("function ").unwrap_or(&header)
    } else if language == "rust" {
        ["pub(crate) ", "pub(super) ", "pub(self) "]
            .iter()
            .find_map(|prefix| header.strip_prefix(prefix))
            .unwrap_or(&header)
    } else {
        &header
    };
    let tail = header.strip_prefix(signature)?.trim_start();
    if !tail.starts_with(['{', ':', ';']) {
        return None;
    }
    let valid_name = |declaration: &str| {
        declaration.strip_prefix(name).is_some_and(|rest| {
            rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric() && c != '_')
        })
    };
    let valid_decl = match language {
        "shell" => signature == format!("{name}()"),
        "rust" | "python" => {
            let mut words = signature.split_whitespace().skip_while(|word| {
                matches!(
                    *word,
                    "pub" | "async" | "unsafe" | "const" | "default" | "extern"
                )
            });
            matches!(
                (language, words.next()),
                ("rust", Some("fn" | "struct" | "enum" | "trait" | "mod"))
                    | ("python", Some("def" | "class"))
            ) && words.next().is_some_and(valid_name)
        }
        _ => false,
    };
    if !valid_decl {
        return None;
    }
    Some(json!({"id":symbol["id"],"line":line,"endLine":end_line,"signature":signature}))
}

#[allow(clippy::too_many_arguments)]
fn build_indexed_projection(
    repo: &Path,
    graph: &Graph,
    revision: &str,
    freshness: &'static str,
    indexed_at: Option<String>,
    coverage: &Value,
    node: &str,
    query: &str,
    limit: i64,
    depth: i64,
    include_tests: bool,
) -> Value {
    let repo_name = repo_basename(repo);
    let include_tests = include_tests || (!node.is_empty() && graph.is_test(node));
    let mut selection = Value::Null;
    let mut focus = Vec::new();
    let mut miss = None;
    if !node.is_empty() {
        if let Some(record) = graph.nodes.get(node) {
            focus.push(node.to_string());
            let path = s(record, "path");
            let mut crumbs = dir_breadcrumbs(&repo_name, path, false);
            if node.starts_with("symbol:") {
                for prefix in ["file", "test"] {
                    if let Some(file) = graph.nodes.get(&format!("{prefix}:{path}")) {
                        crumbs.push(indexed_row(file));
                        break;
                    }
                }
            }
            let selected = indexed_row(record);
            crumbs.push(selected.clone());
            let mut rows = graph.neighbors(node, "", "both").unwrap_or_default();
            rows.retain(|edge| include_tests || !graph.is_test(s(edge, "id")));
            rows.sort_by_key(|r| {
                (
                    confidence_rank(s(r, "confidence")),
                    s(r, "relation").to_owned(),
                    s(r, "id").to_owned(),
                )
            });
            let neighbors: Vec<_> = rows
                .iter()
                .take(MAX_SELECTION_NEIGHBORS)
                .filter_map(|r| {
                    let mut entry = indexed_row(graph.nodes.get(s(r, "id"))?);
                    for key in ["direction", "relation", "confidence"] {
                        entry[key] = r[key].clone();
                    }
                    Some(entry)
                })
                .collect();
            selection = json!({"status":"found","node":selected,"breadcrumbs":crumbs,"neighbors":neighbors,"omittedNeighbors":rows.len().saturating_sub(neighbors.len())});
            if node.starts_with("symbol:")
                && let Some(detail) = selected_definition(repo, graph, record, freshness)
            {
                selection["definition"] = detail;
            }
        } else {
            miss = Some("not_found");
            selection = json!({"status":"not_found","node":null,"neighbors":[],"breadcrumbs":[]});
        }
    } else if !query.is_empty() {
        focus.extend(
            graph
                .find(query, "", limit, include_tests)
                .iter()
                .map(|r| s(r, "id").to_owned()),
        );
        if focus.is_empty() {
            miss = Some("empty");
        }
    }
    let visual = if miss.is_none() {
        graph.visual(limit, depth, include_tests, coverage, &focus, &[], &[], &[])
    } else {
        Value::Null
    };
    let nodes: Vec<_> = array(&visual, "nodes")
        .iter()
        .take(limit as usize)
        .map(|row| {
            let mut out = indexed_row(row);
            for key in ["distance", "degree", "focus", "show_label"] {
                out[key] = row[key].clone();
            }
            out
        })
        .collect();
    let edges: Vec<_> = array(&visual, "edges").iter().take(MAX_EDGES).collect();
    let total = visual["counts"]["total_nodes"]
        .as_u64()
        .unwrap_or(nodes.len() as u64);
    let edge_count = visual["counts"]["total_edges"]
        .as_u64()
        .unwrap_or(edges.len() as u64);
    let truncated = total > nodes.len() as u64 || edge_count > edges.len() as u64;
    let status = miss.unwrap_or(if nodes.is_empty() {
        "empty"
    } else if freshness == "stale" {
        "partial"
    } else {
        "ready"
    });
    let mut counts = if visual["counts"].is_object() {
        visual["counts"].clone()
    } else {
        json!({})
    };
    counts["shown_nodes"] = json!(nodes.len());
    counts["shown_edges"] = json!(edges.len());
    counts["confidence"] = count_edge_confidences(array(&visual, "edges"));
    json!({
        "schema":SCHEMA,"kind":KIND,"status":status,"completeness":if truncated||freshness=="stale" {"partial"} else {"complete"},
        "provenance":{"source":"indexed-graph","mode":"indexed","indexState":"present","indexFreshness":freshness,"relationshipBasis":"verified-and-inferred",
            "verifiedRelationships":edges.iter().any(|e| e["confidence"]=="EXTRACTED"),"revision":revision,"indexedAt":indexed_at,"readOnly":true,"wroteIndex":false},
        "scope":{"repository":repo_name,"focusNode":(!node.is_empty()).then_some(node),"query":(!query.is_empty()).then_some(query),"includeTests":include_tests,"depth":depth,"limit":limit},
        "limits":{"maxNodes":limit,"maxEdges":MAX_EDGES,"maxBytes":MAX_RESPONSE_BYTES,"maxDepth":MAX_DEPTH,"truncated":truncated,"truncatedReason":truncated.then_some("view_limit")},
        "counts":counts,"coverage":coverage,"nodes":nodes,"edges":edges,"selection":selection,
    })
}

struct BoundedOutput {
    code: i32,
    stdout: Vec<u8>,
    capped_bytes: bool,
    timed_out: bool,
}

fn run_git_bounded(
    repo: &Path,
    args: &[&str],
    max_stdout: usize,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<BoundedOutput> {
    use rustix::{
        fs::{OFlags, fcntl_getfl, fcntl_setfl},
        process::{Pid, Signal, kill_process_group},
    };
    check(cancel)?;
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ])
    .args(args)
    .current_dir(repo)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .process_group(0);
    use std::os::unix::ffi::OsStrExt;
    for (key, _) in std::env::vars_os() {
        if key.as_bytes().to_ascii_uppercase().starts_with(b"GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(parent) = repo.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let mut child = cmd.spawn()?;
    let pid =
        Pid::from_raw(child.id() as i32).ok_or_else(|| Error::new("invalid git child process"))?;
    let mut out = child.stdout.take().unwrap();
    let flags = fcntl_getfl(&out)
        .map_err(|e| Error::new(e.to_string()))
        .and_then(|f| {
            fcntl_setfl(&out, f | OFlags::NONBLOCK).map_err(|e| Error::new(e.to_string()))
        });
    if let Err(err) = flags {
        let _ = kill_process_group(pid, Signal::KILL);
        let _ = child.wait();
        return Err(err);
    }
    let mut stdout = Vec::new();
    let mut capped_bytes = false;
    let mut timed_out = false;
    let mut eof = false;
    let start = Instant::now();
    let mut buf = [0u8; 8192];
    let code = loop {
        if check(cancel).is_err() {
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = child.wait();
            return Err(Error {
                code: 130,
                message: "interrupted".into(),
            });
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = child.wait();
            break -1;
        }
        if !eof {
            for _ in 0..16 {
                match out.read(&mut buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => {
                        let remaining = max_stdout.saturating_sub(stdout.len());
                        if n > remaining {
                            stdout.extend_from_slice(&buf[..remaining]);
                            capped_bytes = true;
                            break;
                        }
                        stdout.extend_from_slice(&buf[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        eof = true;
                        break;
                    }
                }
            }
        }
        if capped_bytes {
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = child.wait();
            break 0;
        }
        if let Some(status) = child.try_wait()?
            && eof
        {
            break status.code().unwrap_or(1);
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    Ok(BoundedOutput {
        code,
        stdout,
        capped_bytes,
        timed_out,
    })
}

enum ScanOutcome {
    Paths {
        source: &'static str,
        revision: String,
        paths: Vec<String>,
        scanned_paths: usize,
        excluded_paths: usize,
        scan_truncated: bool,
    },
    InvalidRepo(&'static str),
    Unavailable(&'static str),
}

fn scan_repo_paths(
    repo: &Path,
    checkout_kind: CheckoutKind,
    cancel: &AtomicBool,
) -> Result<ScanOutcome> {
    if checkout_kind == CheckoutKind::Git {
        let top = match run_git_bounded(
            repo,
            &["rev-parse", "--show-toplevel"],
            4096,
            GIT_TIMEOUT,
            cancel,
        ) {
            Ok(out) => out,
            Err(e) if e.code == 130 => return Err(e),
            Err(_) => {
                return Ok(ScanOutcome::Unavailable(
                    "git executable is unavailable to inspect repository paths",
                ));
            }
        };
        if top.timed_out {
            return Ok(ScanOutcome::Unavailable(
                "git repository check timed out before path scan completed",
            ));
        }
        if top.code != 0 {
            return Ok(ScanOutcome::InvalidRepo(
                "invalid project repository: .git does not resolve to this checkout",
            ));
        }
        let top_str = String::from_utf8_lossy(&top.stdout).trim().to_owned();
        if std::fs::canonicalize(&top_str).ok().as_deref() != Some(repo) {
            return Ok(ScanOutcome::InvalidRepo(
                "invalid project repository: git root does not match selected checkout",
            ));
        }
        let revision = run_git_bounded(
            repo,
            &["rev-parse", "--short=12", "HEAD"],
            128,
            GIT_TIMEOUT,
            cancel,
        )
        .ok()
        .filter(|o| o.code == 0 && !o.timed_out)
        .map(|o| sanitize(String::from_utf8_lossy(&o.stdout).trim(), 32))
        .unwrap_or_default();

        let listed = match run_git_bounded(
            repo,
            &["ls-files", "--cached", "-z"],
            MAX_GIT_STDOUT_BYTES,
            GIT_TIMEOUT,
            cancel,
        ) {
            Ok(out) => out,
            Err(e) if e.code == 130 => return Err(e),
            Err(_) => {
                return Ok(ScanOutcome::Unavailable(
                    "git ls-files could not be executed for this repository",
                ));
            }
        };
        if listed.timed_out {
            return Ok(ScanOutcome::Unavailable(
                "git ls-files timed out within the panel read budget",
            ));
        }
        if listed.code != 0 && !listed.capped_bytes {
            return Ok(ScanOutcome::Unavailable(
                "git ls-files could not enumerate repository paths",
            ));
        }
        let slice = if listed.capped_bytes {
            match listed.stdout.iter().rposition(|b| *b == 0) {
                Some(idx) => &listed.stdout[..idx],
                None => &[],
            }
        } else {
            &listed.stdout[..]
        };
        let mut scan_truncated = listed.capped_bytes;
        let mut scanned_paths = 0usize;
        let mut excluded_paths = 0usize;
        let mut seen = BTreeSet::new();
        for raw in slice.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            if scanned_paths >= MAX_SCAN_PATHS {
                scan_truncated = true;
                break;
            }
            scanned_paths += 1;
            let Ok(text) = std::str::from_utf8(raw) else {
                excluded_paths += 1;
                continue;
            };
            if is_excluded_path(text) || text.len() > 470 || no_symlinks(&repo.join(text)).is_err()
            {
                excluded_paths += 1;
                continue;
            }
            seen.insert(text.to_owned());
        }
        return Ok(ScanOutcome::Paths {
            source: "git-ls-files",
            revision,
            paths: seen.into_iter().collect(),
            scanned_paths,
            excluded_paths,
            scan_truncated,
        });
    }

    Ok(ScanOutcome::Unavailable(
        "No Git-tracked inventory is available for this project.",
    ))
}

#[derive(Default)]
struct TreeItem {
    children: Vec<String>,
    parent: Option<String>,
    files: usize,
}

fn directory_id(path: &str) -> String {
    format!("dir:{}", if path.is_empty() { "." } else { path })
}

fn item_row(tree: &BTreeMap<String, TreeItem>, id: &str, repo_name: &str) -> Value {
    let item = &tree[id];
    let (prefix, path) = id.split_once(':').unwrap();
    let directory = prefix == "dir";
    json!({
        "id":id,
        "label":if id=="dir:." {repo_name} else {path.rsplit('/').next().unwrap_or(path)},
        "kind":if directory {"directory"} else {prefix},
        "path":if id=="dir:." {""} else {path},
        "language":if directory {""} else {language_for_path(path)},
        "file_count":item.files
    })
}

#[allow(clippy::too_many_arguments)]
fn build_fallback_projection(
    repo: &Path,
    scan: ScanOutcome,
    index_state: &'static str,
    index_note: &'static str,
    index_coverage: &Value,
    indexed_at: Option<String>,
    node: &str,
    query: &str,
    limit: i64,
    depth: i64,
    include_tests: bool,
    cancel: &AtomicBool,
) -> Result<Value> {
    let (source, revision, all_paths, scanned, excluded, mut clipped, note) = match scan {
        ScanOutcome::Paths {
            source,
            revision,
            paths,
            scanned_paths,
            excluded_paths,
            scan_truncated,
        } => (
            source,
            revision,
            paths,
            Some(scanned_paths),
            excluded_paths,
            scan_truncated,
            index_note,
        ),
        ScanOutcome::Unavailable(reason) => {
            ("none", String::new(), Vec::new(), None, 0, false, reason)
        }
        ScanOutcome::InvalidRepo(msg) => return Err(Error::new(msg)),
    };
    let mut tree = BTreeMap::<String, TreeItem>::new();
    let mut hidden_tests = 0;
    let include_tests = include_tests || node.starts_with("test:");
    for path in &all_paths {
        check(cancel)?;
        let test = build::test_path(path);
        if test && !include_tests {
            hidden_tests += 1;
            continue;
        }
        // A single deep path can introduce many intermediate directories.
        if tree.len() + path.bytes().filter(|b| *b == b'/').count() + 2 > MAX_TREE_ENTRIES {
            clipped = true;
            break;
        }
        let mut id = format!("{}:{path}", if test { "test" } else { "file" });
        let mut rest = path.as_str();
        while id != "dir:." {
            let parent_path = rest.rsplit_once('/').map_or("", |(p, _)| p);
            let parent = directory_id(parent_path);
            let item = tree.entry(id.clone()).or_default();
            item.files += 1;
            if item.parent.is_none() {
                item.parent = Some(parent.clone());
                tree.entry(parent.clone()).or_default().children.push(id);
            }
            id = parent;
            rest = parent_path;
        }
        tree.entry(id).or_default().files += 1;
    }
    let ranking: BTreeMap<_, _> = tree
        .iter()
        .map(|(id, item)| {
            (
                id.clone(),
                (!id.starts_with("dir:"), std::cmp::Reverse(item.files)),
            )
        })
        .collect();
    for item in tree.values_mut() {
        item.children
            .sort_by(|a, b| ranking[a].cmp(&ranking[b]).then_with(|| a.cmp(b)));
    }
    let repo_name = repo_basename(repo);
    let mut selection = Value::Null;
    if !node.is_empty() {
        selection = if let Some(item) = tree.get(node) {
            let (_, path) = node.split_once(':').unwrap();
            let mut breadcrumbs = dir_breadcrumbs(
                &repo_name,
                if node == "dir:." { "" } else { path },
                node.starts_with("dir:"),
            );
            if !node.starts_with("dir:") {
                breadcrumbs.push(item_row(&tree, node, &repo_name));
            }
            let mut neighbors = Vec::new();
            for (direction, id) in item
                .parent
                .iter()
                .map(|p| ("in", p))
                .chain(item.children.iter().map(|p| ("out", p)))
                .take(MAX_SELECTION_NEIGHBORS)
            {
                let mut row = item_row(&tree, id, &repo_name);
                row["direction"] = json!(direction);
                row["relation"] = json!("contains");
                row["confidence"] = json!("STRUCTURAL");
                neighbors.push(row);
            }
            json!({"status":"found","node":item_row(&tree,node,&repo_name),"breadcrumbs":breadcrumbs,"neighbors":neighbors,
                "omittedNeighbors":(item.children.len()+usize::from(item.parent.is_some())).saturating_sub(neighbors.len())})
        } else {
            json!({"status":if clipped {"unknown"} else {"not_found"},"node":null,"breadcrumbs":[],"neighbors":[]})
        };
    }
    let mut queue = VecDeque::new();
    if !query.is_empty() {
        let query = query.to_lowercase();
        queue.extend(
            tree.keys()
                .filter(|id| id.to_lowercase().contains(&query))
                .take(limit as usize)
                .map(|id| (id.clone(), 0)),
        );
    } else if let Some((id, _)) = tree.get_key_value(if node.is_empty() { "dir:." } else { node }) {
        queue.push_back((id.clone(), 0));
    }
    let mut order = Vec::new();
    let mut visible = BTreeSet::new();
    while let Some((id, distance)) = queue.pop_front() {
        if order.len() == limit as usize {
            break;
        }
        if !visible.insert(id.clone()) {
            continue;
        }
        let item = &tree[&id];
        if distance < depth && query.is_empty() {
            let remaining = (limit as usize).saturating_sub(order.len());
            queue.extend(
                item.children
                    .iter()
                    .take(remaining)
                    .map(|c| (c.clone(), distance + 1)),
            );
        }
        order.push((id, distance));
    }
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for (id, distance) in order {
        let item = &tree[&id];
        let omitted = item
            .children
            .iter()
            .filter(|key| !visible.contains(*key))
            .count();
        let mut row = item_row(&tree, &id, &repo_name);
        row["focus"] = json!(id == node);
        row["distance"] = json!(distance);
        row["omitted_children"] = json!(omitted);
        row["expandable"] = json!(omitted > 0);
        nodes.push(row);
        if let Some(parent) = &item.parent
            && visible.contains(parent)
        {
            edges.push(json!({"source":parent,"target":id,"relation":"contains","confidence":"STRUCTURAL"}));
        }
    }
    let missed = !node.is_empty() && !tree.contains_key(node);
    let truncated = clipped || (!nodes.is_empty() && nodes.len() < tree.len());
    let incomplete = clipped || index_state == "invalid";
    let status = if scanned.is_none() {
        "unknown"
    } else if clipped && (missed || nodes.is_empty()) {
        "partial"
    } else if missed {
        "not_found"
    } else if nodes.is_empty() {
        "empty"
    } else if incomplete {
        "partial"
    } else {
        "ready"
    };
    Ok(json!({
        "schema":SCHEMA,"kind":KIND,"status":status,
        "completeness":if scanned.is_none() {"unknown"} else if truncated || incomplete {"partial"} else {"complete"},
        "scope":{"repository":repo_name,"focusNode":(!node.is_empty()).then_some(node),"query":(!query.is_empty()).then_some(query),"includeTests":include_tests,"limit":limit,"depth":depth},
        "provenance":{"source":source,"mode":if scanned.is_none() {"unavailable"} else {"fallback"},"indexState":index_state,"indexFreshness":"absent",
            "relationshipBasis":"structural-containment","verifiedRelationships":false,"revision":revision,"indexedAt":indexed_at,"readOnly":true,"wroteIndex":false,"note":note},
        "limits":{"maxNodes":limit,"maxEdges":MAX_EDGES,"maxBytes":MAX_RESPONSE_BYTES,"maxDepth":MAX_DEPTH,"maxScanPaths":MAX_SCAN_PATHS,
            "scannedPaths":scanned,"excludedPaths":excluded,"truncated":truncated,"truncatedReason":if clipped {Some("scan_limit")} else if truncated {Some("view_limit")} else {None}},
        "counts":{"shown_nodes":nodes.len(),"total_nodes":tree.len(),"shown_edges":edges.len(),"total_edges":tree.len().saturating_sub(1),
            "hidden_test_nodes":hidden_tests,"confidence":count_edge_confidences(&edges)},
        "coverage":index_coverage,"nodes":nodes,"edges":edges,"selection":selection,
    }))
}

fn enforce_byte_budget(payload: &mut Value, pretty: bool) -> Result<()> {
    let mut size = bytes(payload, pretty)?.len();
    if size <= MAX_RESPONSE_BYTES {
        return Ok(());
    }
    let mark_budget_trim = |payload: &mut Value| {
        payload["limits"]["truncated"] = json!(true);
        payload["limits"]["truncatedReason"] = json!("byte_budget");
        if payload["completeness"] == "complete" {
            payload["completeness"] = json!("partial");
        }
        if payload["status"] == "ready" {
            payload["status"] = json!("partial");
        }
    };
    mark_budget_trim(payload);

    // 1. Trim edges first.
    while size > MAX_RESPONSE_BYTES {
        let len = payload["edges"].as_array().map_or(0, Vec::len);
        if len == 0 {
            break;
        }
        let next = len / 2;
        payload["edges"].as_array_mut().unwrap().truncate(next);
        sync_counts(payload);
        size = bytes(payload, pretty)?.len();
    }

    // 2. Trim selection neighbors if still oversize.
    while size > MAX_RESPONSE_BYTES {
        let Some(neighbors) = payload["selection"]["neighbors"].as_array_mut() else {
            break;
        };
        if neighbors.is_empty() {
            break;
        }
        let old_len = neighbors.len();
        let next = old_len / 2;
        neighbors.truncate(next);
        let omitted = n(&payload["selection"], "omittedNeighbors") as usize + (old_len - next);
        payload["selection"]["omittedNeighbors"] = json!(omitted);
        size = bytes(payload, pretty)?.len();
    }

    // 3. Trim non-focus nodes from the back if still oversize.
    while size > MAX_RESPONSE_BYTES {
        let len = payload["nodes"].as_array().map_or(0, Vec::len);
        if len <= 1 {
            break;
        }
        let drop_idx = payload["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .rposition(|row| row["focus"] != true)
            .unwrap_or(len - 1);
        payload["nodes"].as_array_mut().unwrap().remove(drop_idx);
        let kept: BTreeSet<String> = array(payload, "nodes")
            .iter()
            .map(|r| s(r, "id").to_owned())
            .collect();
        payload["edges"]
            .as_array_mut()
            .unwrap()
            .retain(|e| kept.contains(s(e, "source")) && kept.contains(s(e, "target")));
        sync_counts(payload);
        size = bytes(payload, pretty)?.len();
    }
    if size > MAX_RESPONSE_BYTES {
        return Err(Error::new(
            "Graph selection exceeds the panel response budget",
        ));
    }
    Ok(())
}

fn sync_counts(payload: &mut Value) {
    let shown_nodes = array(payload, "nodes").len();
    let edges = array(payload, "edges").to_vec();
    let shown_edges = edges.len();
    payload["counts"]["shown_nodes"] = json!(shown_nodes);
    payload["counts"]["shown_edges"] = json!(shown_edges);
    payload["counts"]["confidence"] = count_edge_confidences(&edges);
}
