//! Read a retained digest capture through its canonical descriptor-backed reader.
use crate::value::{self, Value};
use crate::{Server, ToolError, ToolResult, delegated};
use oms_output_capture::{CaptureError, RequestError};
use std::os::unix::ffi::OsStrExt;

fn failure(code: &'static str, message: impl Into<String>) -> CaptureError {
    CaptureError {
        code,
        message: message.into(),
    }
}
fn tool_error(error: CaptureError) -> ToolError {
    serde_json::json!({"error":{"code":error.code,"message":error.message}})
        .to_string()
        .into()
}

/// Same bounded environment and shared scanner as the native digest producer.
/// The scanner selects secret environment values; ordinary values are not rules.
pub(super) fn outbound(snapshot: &[u8], args: &Value) -> Result<(), CaptureError> {
    let withheld = || {
        failure(
            "capture_withheld",
            "stored capture or search text failed the outbound content scan; content withheld",
        )
    };
    let mut pairs = Vec::new();
    let mut total = 0usize;
    for (key, value) in std::env::vars_os() {
        total = total
            .saturating_add(key.as_bytes().len())
            .saturating_add(value.as_bytes().len());
        if pairs.len() >= oms_scrub::MAX_ENV_ENTRIES || total > oms_scrub::MAX_ENV_BYTES {
            return Err(withheld());
        }
        pairs.push((key.as_bytes().to_vec(), value.as_bytes().to_vec()));
    }
    let refs: Vec<_> = pairs
        .iter()
        .map(|(key, value)| (key.as_slice(), value.as_slice()))
        .collect();
    let check = |bytes: &[u8]| match oms_scrub::scan(bytes, &refs) {
        Ok(hits) if hits.is_empty() => Ok(()),
        _ => Err(withheld()),
    };
    check(snapshot)?;
    // Search literals can be echoed in continuations and compact batch results.
    // Repository metadata is already authorized and is needed to reuse cursors.
    if let Some(query) = args["query"].as_str() {
        check(query.as_bytes())?;
    }
    if let Some(queries) = args["queries"].as_array() {
        for query in queries {
            if let Some(query) = query.as_str() {
                check(query.as_bytes())?;
            }
        }
    }
    Ok(())
}

impl Server<'_> {
    pub(crate) fn output_read(&self, args: &Value) -> ToolResult {
        let repo = self.repo(args).map_err(|_| {
            tool_error(failure(
                "invalid_repository",
                "repository must be an accessible directory with a UTF-8 path",
            ))
        })?;
        if delegated() && repo != self.caller_cwd {
            return Err(tool_error(failure(
                "capture_repository_denied",
                "a delegated worker can read captures only in its own repository",
            )));
        }
        if repo
            .to_str()
            .is_none_or(|path| path.chars().any(char::is_control))
        {
            return Err(tool_error(failure(
                "invalid_repository",
                "capture repository must be physical UTF-8 without control characters",
            )));
        }
        let root = oms_safe_fs::Root::open(&repo).map_err(|_| {
            tool_error(failure(
                "unsafe_capture",
                "capture repository is unsafe or unreadable",
            ))
        })?;
        // Root::open resolves paths; do not silently authorize a replaced alias.
        if root.physical_root() != repo.as_path() {
            return Err(tool_error(failure(
                "unsafe_capture",
                "capture repository changed before reading",
            )));
        }
        let encoded = value::encode(args, true, false, false, false);
        let arguments = oms_context::parse_experiment_json_with_depth(
            encoded.as_bytes(),
            encoded.len(),
            value::DEPTH,
        )
        .map_err(|_| {
            tool_error(failure(
                "invalid_request",
                "capture arguments could not be decoded",
            ))
        })?;
        let payload = oms_output_capture::read_capture_checked(&root, &arguments, |snapshot| {
            outbound(snapshot, args)
        })
        .map_err(|error| match error {
            RequestError::Capture(error) => tool_error(error),
            RequestError::InvalidRepository(_) => tool_error(failure(
                "invalid_repository",
                "capture repository must have a physical UTF-8 path",
            )),
        })?;
        let response = serde_json::to_string(&payload).expect("capture JSON serializes");
        if response.len() > oms_output_capture::COMPACT_RESPONSE_BYTES {
            return Err(tool_error(failure(
                "response_too_large",
                "capture response exceeds 65536 bytes; lower limit or use compact=true",
            )));
        }
        Ok(response.into())
    }
}
