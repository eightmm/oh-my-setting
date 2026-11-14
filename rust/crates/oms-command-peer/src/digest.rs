use crate::{Context, Error, Output, Result, cli::Args, fail, out, process, safety, storage};
use std::{ffi::OsString, path::Path};

pub(crate) fn run(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &["max-bytes", "head", "tail", "file", "exit-code"],
        &["no-store", "json"],
    )?;
    if args.has("help") {
        return Ok(out(include_bytes!("help/output-digest.txt").to_vec()));
    }
    args.exact_operands(0).or_else(|_| {
        if args.positional.is_empty() {
            Ok(())
        } else {
            fail("unexpected argument")
        }
    })?;
    let mut options = oms_output::Options {
        max_bytes: args.limit("max-bytes", 4000, 256, 1_048_576)?,
        head: args.limit("head", 10, 0, usize::MAX)?,
        tail: args.limit("tail", 30, 0, usize::MAX)?,
        ..Default::default()
    };
    let mut code = 0;
    let mut complete = true;
    let repo = storage::repository(context, context.cwd)?;
    let raw = if args.has("file") {
        if !args.command.is_empty() {
            return fail("--file and a command are two modes; give one");
        }
        if args.has("exit-code") {
            options.exit_code = Some(
                args.get("exit-code")
                    .parse()
                    .map_err(|_| Error::new(2, "--exit-code must be an integer"))?,
            );
        }
        options.reference = Some(
            args.path("file", context.cwd)
                .to_string_lossy()
                .into_owned(),
        );
        storage::file(&args.path("file", context.cwd))?
    } else {
        if args.command.is_empty() {
            return fail("give --file PATH or -- COMMAND [ARG...]");
        }
        if args.has("exit-code") {
            return fail("--exit-code goes with --file");
        }
        let result = process::invoke(
            context,
            context.cwd,
            args.command.clone(),
            None,
            31_536_000,
            true,
        )
        .map_err(|e| Error::new(125, e.message))?;
        code = result.code;
        complete = result.complete;
        options.exit_code = Some(i64::from(code));
        options.duration_s = Some(result.elapsed.as_secs() as i64);
        result.stdout
    };
    let mut capture_name = None;
    let bytes = if !args.has("file") && !args.has("no-store") && !safety::child() {
        let redacted =
            safety::redact(&raw).unwrap_or_else(|_| b"[oms-redacted:scan-failed]\n".to_vec());
        let name = format!(
            "{}-{}.log",
            chrono::DateTime::from_timestamp(crate::seconds() as i64, 0)
                .ok_or_else(|| Error::new(125, "invalid clock"))?
                .format("%Y%m%dT%H%M%SZ"),
            std::process::id()
        );
        let path = Path::new(".oms/artifacts/output-digest").join(&name);
        // A process can make two captures in one second. Never overwrite one.
        storage::write(&repo, &path, redacted.clone(), false, context.interrupted)
            .map_err(|e| Error::new(125, e.message))?;
        options.reference = Some(path.to_string_lossy().into_owned());
        capture_name = Some(name);
        redacted
    } else {
        raw
    };
    let mut digest = oms_output::build(&bytes, &options)
        .map_err(|e| Error::new(if args.has("file") { 2 } else { 125 }, e))?;
    if !complete {
        digest.withhold("capture incomplete: process supervision or output limit".into());
    }
    if safety::outbound(digest.semantic_scan_text().as_bytes()).is_err() {
        digest.withhold("outbound content scan".into());
    }
    if digest.withheld.is_none()
        && let Some(name) = capture_name
    {
        digest
            .retained_capture(&storage::root(&repo)?, &name)
            .map_err(|e| Error::new(125, e))?;
    }
    let output = if args.has("json") {
        crate::json(&crate::V::parse(digest.json().to_string().as_bytes())?)?
    } else {
        digest.render().into_bytes()
    };
    Ok((code, output, Vec::new()))
}
