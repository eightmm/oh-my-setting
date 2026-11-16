//! Owned native processes and the durable agent-supervisor command adapter.
#![forbid(unsafe_code)]
pub mod capture;
mod cli;
mod command;
mod lifecycle;
pub mod process;
pub mod sandbox;
mod storage;
pub use command::{CommandHost, run, run_streaming};
pub type Result<T> = std::result::Result<T, String>;
