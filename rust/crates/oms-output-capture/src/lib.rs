#![forbid(unsafe_code)]

//! Scoped identifiers for stored output captures.

use sha2::{Digest, Sha256};
use std::path::Path;

/// Mint a reference using an already validated physical UTF-8 repository path.
pub fn capture_reference(repo: &Path, name: &str, sha256: &str) -> Result<String, &'static str> {
    let bytes = name.as_bytes();
    if bytes.len() < 22
        || !bytes[..8].iter().all(u8::is_ascii_digit)
        || bytes[8] != b'T'
        || !bytes[9..15].iter().all(u8::is_ascii_digit)
        || &bytes[15..17] != b"Z-"
        || !bytes[17..bytes.len() - 4].iter().all(u8::is_ascii_digit)
        || !name.ends_with(".log")
    {
        return Err("invalid capture filename");
    }
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("invalid capture SHA-256");
    }
    let repo = repo.to_str().ok_or("repository path is not UTF-8")?;
    if !repo.starts_with('/')
        || repo.as_bytes().contains(&0)
        || (repo != "/"
            && (repo.ends_with('/')
                || repo
                    .split('/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == "." || part == "..")))
    {
        return Err("repository path must be a physical absolute path");
    }
    let mut hash = Sha256::new();
    hash.update(repo.as_bytes());
    hash.update([0]);
    hash.update(name.as_bytes());
    hash.update([0]);
    hash.update(sha256.as_bytes());
    Ok(format!("output:{name}:{sha256}:{:x}", hash.finalize()))
}

/// Describe the reader invocation without opening or authorizing a capture.
pub fn capture_metadata(
    repo: &Path,
    name: &str,
    sha256: &str,
) -> Result<serde_json::Value, &'static str> {
    let reference = capture_reference(repo, name, sha256)?;
    let repo = repo.to_str().ok_or("repository path is not UTF-8")?;
    Ok(
        serde_json::json!({"capture_ref": reference, "capture_sha256": sha256,
        "read_tool": "oms_output_read", "read_arguments": {"repo": repo, "capture_ref": reference}}),
    )
}

use oms_context::Json;
use serde_json::{Map, Value, json};

pub const READ_CAP: usize = 64 * 1024 * 1024;
pub const PAGE_BYTES: usize = 16 * 1024;
pub const COMPACT_RESPONSE_BYTES: usize = 64 * 1024;
pub const BATCH_QUERY_BYTES: usize = 1024;
pub const SEARCH_HITS: usize = 8;
pub const CAPTURE_ROOT: &str = ".oms/artifacts/output-digest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CaptureError {}
fn error(code: &'static str, message: impl Into<String>) -> CaptureError {
    CaptureError {
        code,
        message: message.into(),
    }
}

#[derive(Debug)]
enum Search {
    Page,
    Single(String),
    Batch(Vec<String>),
}
/// Validated arguments bound to a physical repository spelling, without I/O authority.
#[derive(Debug)]
pub struct ValidatedRequest {
    repo: String,
    reference: String,
    name: String,
    sha: String,
    compact: bool,
    offset: usize,
    limit: usize,
    search: Search,
}
impl ValidatedRequest {
    /// Filename to pass to the descriptor-backed store reader.
    pub fn name(&self) -> &str {
        &self.name
    }
}
fn field<'a>(args: &'a Json, key: &str) -> Option<&'a Json> {
    match args {
        Json::Object(v) => v
            .iter()
            .rev()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v),
        _ => None,
    }
}
fn string(v: &Json) -> Option<&str> {
    if let Json::String(b) = v {
        std::str::from_utf8(b).ok()
    } else {
        None
    }
}
fn query(v: &Json) -> Result<String, CaptureError> {
    match string(v) {
        Some(s) if (1..=256).contains(&s.len()) => Ok(s.to_owned()),
        _ => Err(error(
            "invalid_query",
            "each query requires 1-256 UTF-8 bytes",
        )),
    }
}
fn number(
    args: &Json,
    key: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, CaptureError> {
    let n = match field(args, key) {
        None => Some(default),
        Some(Json::Number(n)) if !n.contains(['.', 'e', 'E']) => {
            n.parse::<i128>().ok().and_then(|n| usize::try_from(n).ok())
        }
        _ => None,
    };
    n.filter(|n| (min..=max).contains(n)).ok_or_else(|| {
        error(
            "invalid_range",
            format!("{key} must be an integer from {min} to {max}"),
        )
    })
}
/// Validate arguments in the reference's order before any capture is read.
/// Failures reported by request validation before any store access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// A source-compatible client error with a wire code and message.
    Capture(CaptureError),
    /// The caller did not supply a physical UTF-8 repository spelling.
    InvalidRepository(&'static str),
}

/// Validate reference syntax before the physical repository precondition.
pub fn validate_request(repo: &Path, args: &Json) -> Result<ValidatedRequest, RequestError> {
    let invalid = || {
        RequestError::Capture(error(
            "invalid_reference",
            "capture_ref must be a returned output capture reference",
        ))
    };
    let reference = field(args, "capture_ref")
        .and_then(string)
        .ok_or_else(invalid)?;
    let parts: Vec<_> = reference.split(':').collect();
    if parts.len() != 4
        || parts[0] != "output"
        || parts[3].len() != 64
        || !parts[3]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || capture_reference(Path::new("/"), parts[1], parts[2]).is_err()
    {
        return Err(invalid());
    }
    capture_reference(repo, parts[1], parts[2]).map_err(RequestError::InvalidRepository)?;
    validate_request_inner(repo, args).map_err(RequestError::Capture)
}

/// Validate the filename and caller's bounded stored-byte limit, in source order.
pub fn validate_stored_capture(name: &str, max_bytes: usize) -> Result<(), CaptureError> {
    if capture_reference(Path::new("/"), name, &"0".repeat(64)).is_err() {
        return Err(error("invalid_reference", "invalid capture filename"));
    }
    if !(1..=READ_CAP).contains(&max_bytes) {
        return Err(error(
            "invalid_limit",
            format!("max_bytes must be an integer from 1 to {READ_CAP}"),
        ));
    }
    Ok(())
}

fn validate_request_inner(repo: &Path, args: &Json) -> Result<ValidatedRequest, CaptureError> {
    let invalid = || {
        error(
            "invalid_reference",
            "capture_ref must be a returned output capture reference",
        )
    };
    let reference = field(args, "capture_ref")
        .and_then(string)
        .ok_or_else(invalid)?;
    let parts: Vec<_> = reference.split(':').collect();
    if parts.len() != 4
        || parts[0] != "output"
        || parts[3].len() != 64
        || !parts[3]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid());
    }
    let expected = capture_reference(repo, parts[1], parts[2]).map_err(|_| invalid())?;
    if expected != reference {
        return Err(error(
            "capture_scope_mismatch",
            "capture reference belongs to another repository or was altered",
        ));
    }
    let compact = match field(args, "compact") {
        None => false,
        Some(Json::Bool(b)) => *b,
        _ => return Err(error("invalid_representation", "compact must be a boolean")),
    };
    let offset = number(args, "offset", 0, 0, READ_CAP * 3)?;
    let limit = number(args, "limit", PAGE_BYTES, 4, PAGE_BYTES)?;
    let search = if let Some(requested) = field(args, "queries") {
        let items = if let Json::Array(items) = requested {
            items
        } else {
            return Err(error(
                "invalid_query",
                "queries requires compact=true and 1-8 literals; no query or limit",
            ));
        };
        if !compact
            || field(args, "query").is_some()
            || field(args, "limit").is_some()
            || !(1..=SEARCH_HITS).contains(&items.len())
        {
            return Err(error(
                "invalid_query",
                "queries requires compact=true and 1-8 literals; no query or limit",
            ));
        }
        let queries = items.iter().map(query).collect::<Result<Vec<_>, _>>()?;
        if queries.iter().map(String::len).sum::<usize>() > BATCH_QUERY_BYTES {
            return Err(error(
                "invalid_query",
                "queries exceeds the 1024-byte total UTF-8 limit",
            ));
        }
        Search::Batch(queries)
    } else if let Some(item) = field(args, "query") {
        if field(args, "limit").is_some() {
            return Err(error("invalid_query", "query cannot accompany limit"));
        }
        Search::Single(query(item)?)
    } else {
        Search::Page
    };
    Ok(ValidatedRequest {
        repo: repo.to_str().expect("reference validates UTF-8").into(),
        reference: reference.into(),
        name: parts[1].into(),
        sha: parts[2].into(),
        compact,
        offset,
        limit,
        search,
    })
}
fn base(req: &ValidatedRequest) -> Value {
    json!({"repo":req.repo,"capture_ref":req.reference})
}
fn merge(base: &Value, fields: Value) -> Value {
    let mut result = base.as_object().cloned().unwrap_or_default();
    result.extend(fields.as_object().expect("object").clone());
    Value::Object(result)
}
fn search(
    raw: &str,
    needle: &str,
    offset: usize,
    allowance: usize,
    base: &Value,
    compact: bool,
) -> Value {
    let mut matches = Vec::new();
    let mut end = offset;
    let mut position = raw[offset..].find(needle).map(|i| i + offset);
    while let Some(pos) = position {
        if matches.len() == allowance {
            break;
        }
        let mut start = pos.saturating_sub(128);
        while !raw.is_char_boundary(start) {
            start += 1;
        }
        end = pos + needle.len();
        let mut stop = (end + 128).min(raw.len());
        while !raw.is_char_boundary(stop) {
            stop -= 1;
        }
        let mut hit = json!({"offset":pos,"end":end,"context":&raw[start..stop]});
        if compact {
            hit["read_offset"] = json!(start);
        } else {
            hit["read_arguments"] = merge(base, json!({"offset":start}));
        }
        matches.push(hit);
        position = raw[end..].find(needle).map(|i| i + end);
    }
    let mut result = json!({"matches":matches,"end":end,"has_more":position.is_some()});
    if position.is_some() {
        let next = json!({"offset":end,"query":needle});
        if compact {
            result["next_search"] = next;
        } else {
            result["next_search_arguments"] = merge(base, next);
        }
    }
    result
}
/// Verify and project one complete observed stored-byte snapshot; this grants no read authority.
pub fn project_snapshot(req: &ValidatedRequest, data: &[u8]) -> Result<Value, CaptureError> {
    if data.len() > READ_CAP {
        return Err(error(
            "capture_too_large",
            format!("stored capture exceeds the {READ_CAP}-byte read limit"),
        ));
    }
    if format!("{:x}", Sha256::digest(data)) != req.sha {
        return Err(error(
            "capture_hash_changed",
            "stored capture SHA-256 changed; reference is stale",
        ));
    }
    let raw = String::from_utf8_lossy(data);
    let offset = req.offset;
    if offset > raw.len() || !raw.is_char_boundary(offset) {
        return Err(error(
            "invalid_range",
            "offset must be a UTF-8 boundary within the capture text",
        ));
    }
    let base = base(req);
    let mut obj = Map::new();
    if !req.compact {
        obj.insert("capture_ref".into(), json!(req.reference));
    }
    obj.insert("capture_sha256".into(), json!(req.sha));
    obj.insert("capture_bytes".into(), json!(data.len()));
    obj.insert("content_bytes".into(), json!(raw.len()));
    obj.insert("offset".into(), json!(offset));
    obj.insert("encoding".into(), json!("utf-8 (invalid bytes replaced)"));
    obj.insert(
        "provenance".into(),
        json!("stored redacted capture; not verification evidence"),
    );
    if req.compact {
        obj.insert("representation".into(), json!("compact-v1"));
        obj.insert("read_base".into(), merge(&base, json!({"compact":true})));
    }
    match &req.search {
        Search::Page => {
            let mut end = (offset + req.limit).min(raw.len());
            while !raw.is_char_boundary(end) {
                end -= 1;
            }
            obj.insert("content".into(), json!(&raw[offset..end]));
            obj.insert("end".into(), json!(end));
            obj.insert("has_more".into(), json!(end < raw.len()));
            if end < raw.len() {
                let next = json!({"offset":end,"limit":req.limit});
                obj.insert(
                    if req.compact {
                        "next_read"
                    } else {
                        "next_read_arguments"
                    }
                    .into(),
                    if req.compact {
                        next
                    } else {
                        merge(&base, next)
                    },
                );
            }
        }
        Search::Single(q) => obj.extend(
            search(&raw, q, offset, SEARCH_HITS, &base, req.compact)
                .as_object()
                .unwrap()
                .clone(),
        ),
        Search::Batch(queries) => {
            let mut used = 0;
            let mut searches = Vec::new();
            let mut more = false;
            for (i, q) in queries.iter().enumerate() {
                let result = search(
                    &raw,
                    q,
                    offset,
                    (SEARCH_HITS - used) / (queries.len() - i),
                    &base,
                    true,
                );
                used += result["matches"].as_array().unwrap().len();
                more |= result["has_more"].as_bool().unwrap();
                searches.push(merge(&json!({"query":q}), result));
            }
            obj.insert("searches".into(), json!(searches));
            obj.insert("has_more".into(), json!(more));
        }
    }
    Ok(Value::Object(obj))
}
/// Serialize a complete compact payload or return the adapter's response budget error.
pub fn serialize_compact(payload: &Value) -> Result<String, CaptureError> {
    let text = serde_json::to_string(payload).expect("JSON values serialize");
    if text.len() > COMPACT_RESPONSE_BYTES {
        return Err(error(
            "response_too_large",
            "compact response exceeds 65536 bytes; lower limit or request fewer queries",
        ));
    }
    Ok(text)
}

/// Detail required from a future descriptor-backed reader to retain legacy errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotFailure {
    Missing,
    Unsafe,
    Unreadable,
    InitialOversize { max_bytes: usize },
    ChangedBeforeRead,
    ChangedDuringRead,
    PathChanged,
    RootChanged,
    UnsafeOpen,
}
impl From<SnapshotFailure> for CaptureError {
    fn from(failure: SnapshotFailure) -> Self {
        match failure {
            SnapshotFailure::Missing => error(
                "capture_missing",
                "stored capture is missing or evicted; no command was rerun",
            ),
            SnapshotFailure::Unsafe => error(
                "unsafe_capture",
                "capture store contains an unsafe file or directory",
            ),
            SnapshotFailure::Unreadable => error(
                "capture_unreadable",
                "capture store is unsafe or unreadable",
            ),
            SnapshotFailure::InitialOversize { max_bytes } => error(
                "capture_too_large",
                format!("stored capture exceeds the {max_bytes}-byte read limit"),
            ),
            SnapshotFailure::ChangedBeforeRead => {
                error("capture_changed", "capture changed before reading")
            }
            SnapshotFailure::ChangedDuringRead => {
                error("capture_changed", "capture changed while reading")
            }
            SnapshotFailure::RootChanged => {
                error("unsafe_capture", "repository changed while reading")
            }
            SnapshotFailure::UnsafeOpen => {
                error("unsafe_capture", "capture store is unsafe or unreadable")
            }
            SnapshotFailure::PathChanged => {
                error("unsafe_capture", "capture path changed while reading")
            }
        }
    }
}

/// Read one bounded capture through the repository descriptor without following links.
pub fn read_stored_capture(
    root: &oms_safe_fs::Root,
    name: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, CaptureError> {
    use oms_safe_fs::CaptureReadError as E;
    validate_stored_capture(name, max_bytes)?;
    root.read_capture(name, max_bytes)
        .map_err(|failure| match failure {
            E::InvalidPath => error("invalid_reference", "invalid capture filename"),
            E::InvalidLimit => error(
                "invalid_limit",
                format!("max_bytes must be an integer from 1 to {READ_CAP}"),
            ),
            E::Missing => SnapshotFailure::Missing.into(),
            E::Unreadable => SnapshotFailure::Unreadable.into(),
            E::Unsafe => SnapshotFailure::Unsafe.into(),
            E::InitialOversize => SnapshotFailure::InitialOversize { max_bytes }.into(),
            E::ChangedBeforeRead => SnapshotFailure::ChangedBeforeRead.into(),
            E::FileChanged => SnapshotFailure::ChangedDuringRead.into(),
            E::PathChanged => SnapshotFailure::PathChanged.into(),
            E::RootChanged => SnapshotFailure::RootChanged.into(),
            E::UnsafeOpen => SnapshotFailure::UnsafeOpen.into(),
        })
}

/// Validate the request, read one verified snapshot, then project it without rerunning a command.
pub fn read_capture(root: &oms_safe_fs::Root, arguments: &Json) -> Result<Value, RequestError> {
    let request = validate_request(root.physical_root(), arguments)?;
    let data =
        read_stored_capture(root, request.name(), READ_CAP).map_err(RequestError::Capture)?;
    project_snapshot(&request, &data).map_err(RequestError::Capture)
}
