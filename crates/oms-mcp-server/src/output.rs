//! Read a retained digest capture through its canonical descriptor-backed reader.
use crate::value::{self, Value};
use crate::{Server, ToolError, ToolResult, delegated};
use oms_output_capture::{CaptureError, RequestError};

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
    let check = |bytes: &[u8]| oms_scrub::check_outbound(bytes).map_err(|_| withheld());
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

#[cfg(test)]
mod tests {
    use oms_scrub::{OUTBOUND_PAGE_BYTES, PageError, PageSelector, outbound_page_with_env};

    #[test]
    fn shared_page_helper_withholds_all_pages_when_fixture_straddles_boundary() {
        let left = ["gh", "p_LeftHalf012345"].concat();
        let right = "RightHalf67890abcdef01";
        let body = format!(
            "{} {left}{right} {}",
            "a".repeat(OUTBOUND_PAGE_BYTES - 1 - left.len()),
            "z".repeat(256)
        );
        for selector in [
            PageSelector::Offset(0),
            PageSelector::Offset(OUTBOUND_PAGE_BYTES),
            PageSelector::Number(1),
            PageSelector::Number(2),
        ] {
            assert_eq!(
                outbound_page_with_env(&body, 64 * 1024, selector, &[]),
                Err(PageError::OutboundWithheld)
            );
        }
        let env_key = [b"FIXTURE_API_".as_slice(), b"KEY"].concat();
        let env_val = b"alpha1234beta5678";
        let env_body = format!(
            "{}{}{}",
            "a".repeat(OUTBOUND_PAGE_BYTES - 5),
            std::str::from_utf8(env_val).unwrap(),
            "z".repeat(256)
        );
        for selector in [
            PageSelector::Offset(0),
            PageSelector::Offset(OUTBOUND_PAGE_BYTES),
            PageSelector::Number(1),
            PageSelector::Number(2),
        ] {
            assert_eq!(
                outbound_page_with_env(
                    &env_body,
                    64 * 1024,
                    selector,
                    &[(env_key.as_slice(), env_val.as_slice())]
                ),
                Err(PageError::OutboundWithheld)
            );
        }
    }
}
