//! Bounded Git plumbing with isolated indexes; no ambient Git redirection.
use crate::services::ExecutionRequest;
use crate::{CommandOutput, Error, Result, Services};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub(crate) struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("oms-panel-{}", crate::value::unique()?));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(Error::new)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(crate) fn run<S: Services + ?Sized>(
    service: &S,
    repo: &Path,
    argv: &[&str],
    input: Option<&[u8]>,
    index: Option<&Path>,
    stop: &AtomicBool,
) -> Result<CommandOutput> {
    let mut full = crate::services::args(&[
        "git",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "commit.gpgsign=false",
    ]);
    full.extend(crate::services::args(argv));
    let mut request = ExecutionRequest::new(full, repo);
    request.timeout = Duration::from_secs(60);
    request.output_limit = crate::results::PATCH_MAX;
    request.stdin = input.map(<[u8]>::to_vec);
    request.remove_environment = crate::services::args(&[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ]);
    request.environment = vec![
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
    ];
    if let Some(index) = index {
        request
            .environment
            .push(("GIT_INDEX_FILE".into(), index.into()));
    }
    service.execute(&request, stop)
}
pub(crate) fn bytes<S: Services + ?Sized>(
    service: &S,
    repo: &Path,
    argv: &[&str],
    input: Option<&[u8]>,
    index: Option<&Path>,
    stop: &AtomicBool,
) -> Result<Vec<u8>> {
    let output = run(service, repo, argv, input, index, stop)?;
    output.success()?;
    Ok(output.stdout)
}
pub(crate) fn text<S: Services + ?Sized>(
    service: &S,
    repo: &Path,
    argv: &[&str],
    stop: &AtomicBool,
) -> Result<String> {
    String::from_utf8(bytes(service, repo, argv, None, None, stop)?)
        .map(|v| v.trim().into())
        .map_err(Error::new)
}
pub(crate) fn applies<S: Services + ?Sized>(
    service: &S,
    repo: &Path,
    body: &[u8],
    reverse: bool,
    stop: &AtomicBool,
) -> Result<bool> {
    let scratch = Scratch::new()?;
    let index = scratch.0.join("index");
    bytes(
        service,
        repo,
        &["read-tree", "HEAD"],
        None,
        Some(&index),
        stop,
    )?;
    let mut argv = vec!["apply", "--cached", "--check", "--binary"];
    if reverse {
        argv.push("--reverse");
    }
    argv.push("-");
    let output = run(service, repo, &argv, Some(body), Some(&index), stop)?;
    crate::require(
        output.cleanup_complete && output.output_complete && !output.limited,
        "patch applicability was not observed completely",
    )?;
    crate::require(
        matches!(output.code, 0 | 1),
        "Git could not check patch applicability",
    )?;
    Ok(output.code == 0)
}
