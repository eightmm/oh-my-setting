//! Interpreter-free, read-only frontdoor preparation for a trusted native OMS entry.
//!
//! A caller must authenticate this code before it starts. A checksum checked by
//! the program it purports to authenticate cannot establish that trust.
#![forbid(unsafe_code)]
#![cfg(unix)]

mod filesystem;
mod git;
mod inventory;
mod payload;
mod producer;
mod route;
mod source;

pub use payload::{CheckedExec, SourceFile, SourceInventory, ValidatedPayload};
pub use producer::{ProducerOperation, ProducerReceipt, ProducerRequest, ProducerUnavailable};
pub use route::{
    CommandAdapter, CommandOutput, Frontdoor, LegacyCommand, Navigation, Prepared,
    PrivateInvocation, PythonRequirement, Route, Terminal,
};
pub use source::{Discovery, DiscoveryKind, ReceiptLocation, discover, receipt_location};

use std::fmt;

#[derive(Debug)]
pub struct Error {
    pub status: i32,
    pub message: String,
    output: Vec<u8>,
}
impl Error {
    pub(crate) fn integrity(message: impl Into<String>) -> Self {
        Self::bytes(2, message.into().into_bytes())
    }
    pub(crate) fn discovery(message: impl Into<String>) -> Self {
        Self::bytes(1, message.into().into_bytes())
    }
    pub(crate) fn bytes(status: i32, output: Vec<u8>) -> Self {
        Self {
            status,
            message: String::from_utf8_lossy(&output).into_owned(),
            output,
        }
    }
    /// Exact diagnostic bytes, without a trailing LF. Use these for stderr;
    /// Display is a lossy human-readable projection for non-UTF8 paths.
    pub fn diagnostic(&self) -> &[u8] {
        &self.output
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

pub(crate) fn member<'a>(
    value: &'a oms_context::Json,
    name: &[u8],
) -> Option<&'a oms_context::Json> {
    let oms_context::Json::Object(entries) = value else {
        return None;
    };
    entries
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}
pub(crate) fn hex(value: &[u8], size: usize) -> bool {
    value.len() == size
        && value
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
