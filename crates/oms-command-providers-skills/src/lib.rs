#![forbid(unsafe_code)]

//! Provider, project, and skill command ownership for the native dispatcher.

mod broker;
mod common;
mod connect;
mod gateway;
mod harness;
pub mod https;
mod lifecycle;
mod messages;
pub mod model_probe;
mod models;
mod permissions;
mod project;
mod reports;
mod skills;
mod utilities;

use common::{Context, Output, Result};
pub use https::HttpsTransport;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Statically validates one `custom-skills/oms-*` bundle under a verified source
/// root. Reuses the project skill budgets, scrubber and reference checks without
/// executing host commands or writing state. The caller owns manifest selection.
pub fn validate_bundled_skill(source: &Path, skill: &str) -> std::result::Result<(), String> {
    skills::validate_bundled(source, skill).map_err(|error| error.message)
}

/// Classification is side-effect free, including malformed command arguments.
pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    matches!(
        tool,
        "provider-permissions"
            | "connect-services"
            | "models"
            | "model-doctor"
            | "harness"
            | "skill-doctor"
            | "skill-forge"
            | "apply-project-template"
            | "remove-project-template"
            | "project-doctor"
            | "project-private"
            | "detect-project-style"
            | "init"
            | "generate-slurm-reference"
            | "agent-ml-context"
            | "import-agent-result"
    )
}

/// The caller supplies a verified installation source root independently of the
/// selected repository. No command changes process-global cwd or environment.
pub fn run_with_source(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let context = Context {
        cwd,
        source,
        interrupted,
    };
    let result = context
        .check()
        .and_then(|()| dispatch(tool, args, &context));
    match result {
        Ok(out) => (out.code, out.out, out.err),
        Err(error) => (
            error.code,
            vec![],
            format!("error: {}\n", error.message).into_bytes(),
        ),
    }
}
fn dispatch(tool: &str, args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    if owns(tool, args)
        && args
            .iter()
            .take_while(|a| *a != "--")
            .any(|a| a == "--help" || a == "-h")
    {
        return Ok(Output::text(usage(tool)));
    }
    match tool {
        "provider-permissions" => permissions::run(args, ctx),
        "connect-services" => connect::run(args, ctx),
        "models" => models::run(args, false, ctx),
        "model-doctor" => models::run(args, true, ctx),
        "harness" => harness::run(args, ctx),
        "skill-doctor" => skills::doctor(args, ctx),
        "skill-forge" => skills::run(args, ctx),
        "apply-project-template" => project::apply(args, ctx),
        "remove-project-template" => project::remove(args, ctx),
        "project-doctor" => project::doctor(args, ctx),
        "project-private" => project::privacy(args, ctx),
        "detect-project-style" => project::detect(args, ctx),
        "init" => project::init(args, ctx),
        "generate-slurm-reference" => utilities::slurm(args, ctx),
        "agent-ml-context" => utilities::ml(args, ctx),
        "import-agent-result" => utilities::import(args, ctx),
        _ => Err(common::error("unsupported providers-skills command")),
    }
}
fn usage(tool: &str) -> String {
    let arguments = match tool {
        "provider-permissions" => {
            "[--check|--print|--apply|--remove] [--profile consult|delegate] [--settings PATH] [--worktree-parent PATH] [--allow-command C] [--allow-toolchains]"
        }
        "connect-services" => {
            "[--auto|--required] [--data-source-id ID] [--no-github] [--no-notion] | --account PROVIDER:ID [--home PATH] [--first|--last] [--repair|--remove|--verify]"
        }
        "models" => "[--json] [--refresh] [--providers default|auto|all|CSV]",
        "model-doctor" => {
            "[--json] [--live-models] [--require-all] [--strict-diversity] [--providers default|auto|all|CSV]"
        }
        "harness" => {
            "codex|pi|claude --model ID [--effort LEVEL] [--context-window N] [--check|--dry-run] [-- NATIVE_ARGS]"
        }
        "skill-forge" => {
            "[--repo PATH] add|admit|validate|link|list|show|remove|status|contracts|preview|import|update|rollback|eval|outdated [ARGS]\nNew project skills require the oms- prefix; legacy names remain readable."
        }
        "apply-project-template" => {
            "[auto|general|ml|slurm] [project_dir] [--full-docs] [--no-private] [--python-ci] [--python-layout] [--package NAME] [files...]"
        }
        "remove-project-template" => "[all|general|ml|slurm] [project_dir] [files...]",
        "project-doctor" => {
            "[--strict] [--contract|--budget|--prompts|--team] [--json] [project_dir]"
        }
        "project-private" => {
            "[status|apply|remove] [--repo PATH] [--path REL] [--check] [--untrack] [--dry-run] [--json]\nEntries live in .git/info/exclude; .gitignore is unchanged."
        }
        "detect-project-style" => "[project_dir]",
        "init" => "[--repo PATH] [--no-private]",
        "generate-slurm-reference" => "[--check|--dry-run]",
        "agent-ml-context" => "[--repo PATH] [--ledger PATH] [--max-bytes N] [--force]",
        "import-agent-result" => {
            "--kind KIND --provider NAME --file PATH [--prompt-file PATH] [--repo PATH] [--artifact-dir PATH] [--exit N]"
        }
        _ => "",
    };
    format!("Usage: oms {tool} {arguments}\n")
}
