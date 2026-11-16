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
    let code;
    let complete;
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
        let digest = file_digest(&args.path("file", context.cwd), &options, context)?;
        return render(digest, args.has("json"), 0);
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
            "{}-{}{:020}.log",
            chrono::DateTime::from_timestamp(crate::seconds() as i64, 0)
                .ok_or_else(|| Error::new(125, "invalid clock"))?
                .format("%Y%m%dT%H%M%SZ"),
            std::process::id(),
            CAPTURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
    render(digest, args.has("json"), code)
}
static CAPTURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn render(mut digest: oms_output::OutputDigest, json: bool, code: i32) -> Result<Output> {
    if safety::outbound(digest.semantic_scan_text().as_bytes()).is_err() {
        digest.withhold("outbound content scan".into());
    }
    let output = if json {
        crate::json(&crate::V::parse(digest.json().to_string().as_bytes())?)?
    } else {
        digest.render().into_bytes()
    };
    Ok((code, output, Vec::new()))
}
#[allow(clippy::unnecessary_cast)]
fn file_digest(
    path: &Path,
    options: &oms_output::Options,
    context: &Context<'_>,
) -> Result<oms_output::OutputDigest> {
    use rustix::fs::{self, Mode, OFlags};
    use std::{io::Read, os::unix::fs::MetadataExt};
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let anchor = storage::root(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(2, "file name required"))?;
    let directory = fs::open(
        anchor.physical_root(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Error::new(2, "digest directory unavailable"))?;
    let parent_before =
        fs::fstat(&directory).map_err(|_| Error::new(2, "digest directory unavailable"))?;
    let mut file: std::fs::File = fs::openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Error::new(2, "digest file unavailable"))?
    .into();
    let before = file.metadata()?;
    if !before.is_file() || before.nlink() != 1 {
        return fail("digest input must be a regular non-link file");
    }
    struct Observed<'a, R> {
        reader: R,
        interrupted: &'a std::sync::atomic::AtomicBool,
    }
    impl<R: Read> Read for Observed<'_, R> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupted.load(std::sync::atomic::Ordering::Acquire) {
                return Err(std::io::Error::other("digest interrupted"));
            }
            self.reader.read(bytes)
        }
    }
    let digest = oms_output::build_reader(
        Observed {
            reader: (&mut file).take(before.len().saturating_add(1)),
            interrupted: context.interrupted,
        },
        options,
    )
    .map_err(|_| {
        Error::new(
            if context
                .interrupted
                .load(std::sync::atomic::Ordering::Acquire)
            {
                130
            } else {
                2
            },
            "digest stream failed",
        )
    })?;
    let after = file.metadata()?;
    let named = fs::statat(&directory, name, fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| Error::new(2, "digest source changed"))?;
    let parent_after = std::fs::metadata(anchor.physical_root())?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || named.st_dev as u64 != before.dev()
        || named.st_ino as u64 != before.ino()
        || named.st_nlink != 1
        || parent_after.dev() != parent_before.st_dev as u64
        || parent_after.ino() != parent_before.st_ino as u64
        || parent.canonicalize()? != anchor.physical_root()
        || digest.raw.bytes as u64 != before.len()
    {
        return fail("digest source changed while streaming; output withheld");
    }
    Ok(digest)
}
