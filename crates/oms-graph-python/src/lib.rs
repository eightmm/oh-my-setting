//! Native Python source graph parser: the Rust counterpart of
//! `scripts/lib/oms_graph/project/parsers/python.py` (parser version 6).
//!
//! # Boundary
//!
//! Python source is untrusted input to a recursive parser and AST. The pinned
//! Ruff parser has no depth limit of its own and its recovery paths recurse,
//! so a hostile file can overflow the stack and abort the process. This crate
//! therefore exposes no in-process parse function: parsing happens only in a
//! private helper executable (`oms-graph-python-helper`, a libexec artifact,
//! not a command) that a [`Session`] starts from an explicit packaged path and
//! reuses for many requests. A helper crash, hang or protocol violation is a
//! typed [`Error`], never an abort of the caller and never an empty graph.
//!
//! Consumers: the native graph builder and the shell `parse_with_python`
//! callback hold a [`Session`] (or an injected [`Parser`]) and call
//! [`Session::parse`]; packaging installs the helper and passes its absolute
//! path. This crate does not locate the helper.
//!
//! Containment is process isolation plus what the OS enforces. On Linux the
//! helper runs with an address-space limit and no core dumps. macOS accepts the
//! same calls but does not enforce the address-space limit, so there memory
//! containment is not guaranteed (stack overflow and crashes are still
//! contained). No guarantee is claimed beyond that.
//!
//! # Private helper contract (packaging and consumers)
//!
//! - Artifact: `oms-graph-python-helper`, built by Cargo from this package.
//!   Install it only under a private libexec directory owned by the install,
//!   never on PATH, and remove it on uninstall with the rest of that directory.
//!   Its only argument is the internal `--memory-mib N`, set by [`Session`]; it
//!   has no CLI, reads no environment and files, spawns nothing, and writes only
//!   protocol frames to stdout. Do not run it by hand.
//! - Not a product: `oms-graph-python-fixture` (a test stand-in that misbehaves
//!   on purpose, used by `tests/graph_contract.rs`) is built with the package
//!   but must be excluded from install, packaging and PATH.
//! - Start: the consumer passes the helper's absolute path to
//!   [`Session::start`]. A relative or missing path is refused; there is no
//!   PATH search, shell, or environment-selected executable.
//! - Ownership: a session owns exactly one child and its two I/O threads. It
//!   serializes its calls (`&mut self`); a consumer that wants parallel parsing
//!   holds one session per worker. The session never restarts or retries: after
//!   any infrastructure error it is poisoned and the consumer starts a new one.
//! - Limits: [`Options::max_bytes`] (default 2 MiB, never above
//!   [`MAX_SOURCE_BYTES`]), [`MAX_RESPONSE_BYTES`] for the serialized graph, and
//!   [`Options::timeout`] (default 60 s) for one request, after which the child
//!   is killed and reaped. [`CancelFlag`] ends a request early the same way.
//! - Protocol: version 2 frames (see `protocol.rs`); a version mismatch is a
//!   [`Error::Protocol`], so helper and library must be built together.
//!
//! # Output
//!
//! [`parse`](Parser::parse) projects one file into the nodes, edges and
//! unresolved refs the Python parser emits, in the same order. Invalid Python
//! source yields only the module node and its `contains` edge, as the legacy
//! parser does.
//!
//! Symbol metadata additionally exposes declared class fields and annotated
//! callable contracts. These preserve source text and provenance, never
//! runtime type/shape validation. Class kinds are direct marker spellings;
//! imports, aliases and inherited fields are not resolved.
//!
//! Grammar: Ruff parses with target version Python 3.12 (PEP 701 f-strings,
//! PEP 695 generics), the baseline whose output is matched exactly. Newer
//! syntax Ruff understands (3.13 type-parameter defaults, 3.14 t-strings,
//! `lazy import`) is accepted as an intentional extension where the legacy
//! parser on 3.12 would have returned the invalid-source result.
//!
//! Deliberate differences from the raw legacy output: a docstring's escaped
//! lone surrogate (`"\ud800"`), which no Rust string can hold, appears in a
//! summary as `?`. The graph does not scrub category C characters in general,
//! so an actual U+FFFD and every other character stay as written. A leading BOM
//! is invalid source, as for the legacy parser's `str` input, and a decimal
//! integer literal over 4300 digits is invalid source, as in CPython 3.12.
//! Expression depth is not bounded by flag heuristics; it is the helper's
//! crash containment that covers it.

mod extract;
#[doc(hidden)]
pub mod helper;
mod literal;
mod model;
mod protocol;
mod scan;
mod session;
mod text;

use std::fmt;

pub use model::{
    CallableContract, DataContract, DeclaredField, DeclaredParameter, Edge, Evidence, Metadata,
    Node, ParseResult, Reference,
};
pub use protocol::{MAX_RESPONSE_BYTES, MAX_SOURCE_BYTES};
pub use session::{CancelFlag, Options, Parser, Session};
pub use text::{compact_summary, sensitive_text};

/// Parser version recorded by the graph builder; bump with any output change.
pub const VERSION: u32 = 8;
pub const LANGUAGE: &str = "python";
pub const EXTENSIONS: &[&str] = &[".py"];
/// The graph builder's default source cap.
pub const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Why no graph was produced. None of these says the source is invalid Python.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    SourceTooLarge {
        bytes: usize,
        max_bytes: usize,
    },
    ResponseTooLarge {
        max_bytes: usize,
    },
    FieldTooLong {
        field: &'static str,
        max: usize,
    },
    /// The helper could not be started.
    HelperUnavailable(String),
    /// The helper exited or was killed by a signal.
    HelperExited(String),
    Timeout {
        millis: u64,
    },
    Cancelled,
    /// Framing, version, identity or result validation failed.
    Protocol(String),
    /// An earlier failure ended this session; start a new one.
    Poisoned,
}

impl Error {
    /// Stable machine-readable code for adapters.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SourceTooLarge { .. } => "source_too_large",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::FieldTooLong { .. } => "field_too_long",
            Self::HelperUnavailable(_) => "helper_unavailable",
            Self::HelperExited(_) => "helper_exited",
            Self::Timeout { .. } => "timeout",
            Self::Cancelled => "cancelled",
            Self::Protocol(_) => "protocol",
            Self::Poisoned => "poisoned",
        }
    }

    /// Whether the session that returned this error can still be used.
    pub fn session_survives(&self) -> bool {
        matches!(
            self,
            Self::SourceTooLarge { .. } | Self::ResponseTooLarge { .. } | Self::FieldTooLong { .. }
        )
    }

    /// `{"error": {"code", "message"}}`, the shell adapter's failure object.
    pub fn to_json(&self) -> String {
        serde_json::json!({"error": {"code": self.code(), "message": self.to_string()}}).to_string()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceTooLarge { bytes, max_bytes } => {
                write!(
                    f,
                    "source is {bytes} bytes, over the {max_bytes} byte limit"
                )
            }
            Self::ResponseTooLarge { max_bytes } => {
                write!(f, "graph is over the {max_bytes} byte response limit")
            }
            Self::FieldTooLong { field, max } => write!(f, "{field} is over {max} bytes"),
            Self::HelperUnavailable(reason) => write!(f, "parser helper unavailable: {reason}"),
            Self::HelperExited(reason) => write!(f, "parser helper failed: {reason}"),
            Self::Timeout { millis } => write!(f, "parser helper timed out after {millis} ms"),
            Self::Cancelled => write!(f, "parse cancelled"),
            Self::Protocol(reason) => write!(f, "parser helper protocol error: {reason}"),
            Self::Poisoned => write!(f, "parser session ended after an earlier failure"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_json_names_code_and_message_without_content() {
        let error = Error::HelperExited("helper killed by signal 6".to_owned());
        let value: serde_json::Value = serde_json::from_str(&error.to_json()).unwrap();
        assert_eq!(value["error"]["code"], "helper_exited");
        assert_eq!(value.as_object().unwrap().len(), 1);
        assert!(!error.session_survives());
        assert!(
            Error::SourceTooLarge {
                bytes: 2,
                max_bytes: 1
            }
            .session_survives()
        );
    }
}
