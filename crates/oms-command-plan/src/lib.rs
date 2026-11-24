#![forbid(unsafe_code)]
//! Schema-3 plan authority and the `agent-plan` CLI entrypoints (`owns`, `run*`).
//! Process routing and provider integration belong to the caller.
//!
//! `Plan::execute` and its narrow bound-delegate adapter share the command and
//! owner-lock path; receipts are checked again on
//! reads. No worker exit, room membership, or caller-supplied success flag grants
//! completion. See README.md for producer interfaces and publication recovery.

pub mod acceptance;
mod approval;
mod batch;
mod cli;
pub mod completion;
mod contracts;
pub mod decision;
pub mod discussion;
mod engine;
pub mod floor;
pub mod json;
pub mod panel_composition;
mod product;
mod proposal;
mod provenance;
mod query;
pub mod repository;
pub mod request;
pub mod retirement;
pub mod services;
pub mod storage;
pub mod team;
pub mod team_request;

pub use cli::{owns, run, run_with_io, run_with_services};
pub use engine::{Context, DelegateBinding, Plan, ReviewFence, Snapshot};
pub use product::{completed_patch_landing, patch_landing_receipt_digest};
pub use request::{Action, Request};
pub use services::{Services, UnavailableServices};
pub const USAGE: &str = include_str!("usage.txt");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn error(message: impl ToString) -> Error {
    Error {
        code: 2,
        message: message.to_string(),
    }
}
pub(crate) fn drift(message: impl Into<String>) -> Error {
    Error {
        code: 3,
        message: message.into(),
    }
}
pub(crate) fn require(condition: bool, message: impl ToString) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(error(message))
    }
}

#[derive(Debug, Default)]
pub struct Response {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl Response {
    pub fn text(text: impl AsRef<str>) -> Self {
        Self {
            stdout: format!("{}\n", text.as_ref()).into_bytes(),
            ..Self::default()
        }
    }
    pub fn json(value: &json::Value) -> Result<Self> {
        let mut raw = json::pretty(value)?;
        raw.push(b'\n');
        Ok(Self {
            stdout: raw,
            ..Self::default()
        })
    }
    pub(crate) fn json_line(value: &json::Value, ascii: bool, compact: bool) -> Result<Self> {
        let mut raw = if compact {
            if ascii {
                json::ascii_canonical(value)?
            } else {
                json::canonical(value)?
            }
        } else {
            json::default_bytes(value, ascii)?
        };
        raw.push(b'\n');
        Ok(Self {
            stdout: raw,
            ..Self::default()
        })
    }
    pub fn failure(e: Error) -> Self {
        Self {
            code: e.code,
            stderr: format!("error: {}\n", e.message).into_bytes(),
            ..Self::default()
        }
    }
}
