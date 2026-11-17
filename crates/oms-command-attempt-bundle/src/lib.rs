#![forbid(unsafe_code)]
mod bundle;
mod diagnostic;
mod projection;
mod render;
mod source;
mod value;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const USAGE: &str = "usage: agent-events.sh show [-h] --attempt ATTEMPT [--json] [--bundle]\n                            [--transcript PATH]\n";
#[derive(Default)]
struct Args {
    owned: bool,
    repo: Option<PathBuf>,
    attempt: Option<OsString>,
    transcript: Option<PathBuf>,
    json: bool,
    help: bool,
    error: Option<String>,
}
enum Optional {
    Value,
    Known(&'static [u8], Option<OsString>),
    Unknown,
}
fn optional(raw: &OsStr, show: bool) -> Optional {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::sync::OnceLock;
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"-") || bytes.len() == 1 {
        return Optional::Value;
    }
    let candidates: &[&[u8]] = if show {
        &[
            b"--bundle",
            b"--json",
            b"--attempt",
            b"--transcript",
            b"--help",
            b"-h",
        ]
    } else {
        &[b"--repo", b"--help", b"-h"]
    };
    let split = bytes.iter().position(|b| *b == b'=');
    let name = split.map_or(bytes, |n| &bytes[..n]);
    let inline = split.map(|n| OsString::from_vec(bytes[n + 1..].to_vec()));
    if let Some(known) = candidates.iter().find(|v| **v == name) {
        return Optional::Known(known, inline);
    }
    if name.starts_with(b"--") && name.len() > 2 {
        let matches: Vec<_> = candidates.iter().filter(|v| v.starts_with(name)).collect();
        if matches.len() == 1 {
            return Optional::Known(matches[0], inline);
        }
    }
    if bytes.starts_with(b"-h") {
        return Optional::Known(b"-h", Some(OsString::from_vec(bytes[2..].to_vec())));
    }
    static NEGATIVE: OnceLock<regex::Regex> = OnceLock::new();
    let negative = NEGATIVE.get_or_init(|| {
        regex::Regex::new(r"^-\d+$|^-\d*\.\d+$").expect("constant argparse number pattern")
    });
    if raw
        .to_str()
        .is_some_and(|s| negative.is_match(s.strip_suffix('\n').unwrap_or(s)))
        || bytes.contains(&b' ')
    {
        return Optional::Value;
    }
    Optional::Unknown
}
impl Args {
    fn fail(&mut self, message: String) {
        if !self.help && self.error.is_none() {
            self.error = Some(message);
        }
    }
}
fn parse(args: &[OsString]) -> Args {
    let mut out = Args::default();
    let mut i = 0;
    let mut show = false;
    let mut ended = false;
    let mut unexpected = vec![];
    while i < args.len() {
        let raw = args[i].as_os_str();
        if ended {
            unexpected.push(raw.to_string_lossy().into_owned());
            i += 1;
            continue;
        }
        if raw == OsStr::new("--") {
            ended = true;
            i += 1;
            continue;
        }
        if !show && raw == OsStr::new("show") {
            show = true;
            i += 1;
            continue;
        }
        match optional(raw, show) {
            Optional::Known(option, inline) => {
                if option == b"--bundle" {
                    out.owned = true;
                }
                if matches!(option, b"--repo" | b"--attempt" | b"--transcript") {
                    // argparse never consumes an optional token as a separate argument value.
                    let value = inline.or_else(|| {
                        args.get(i + 1)
                            .filter(|v| {
                                v.as_os_str() != OsStr::new("--")
                                    && matches!(optional(v, show), Optional::Value)
                            })
                            .map(|v| {
                                i += 1;
                                v.clone()
                            })
                    });
                    if let Some(value) = value {
                        match option {
                            b"--repo" => out.repo = Some(value.into()),
                            b"--attempt" => out.attempt = Some(value),
                            _ => out.transcript = Some(value.into()),
                        }
                    } else {
                        out.fail(format!(
                            "argument {}: expected one argument",
                            String::from_utf8_lossy(option)
                        ));
                    }
                } else if let Some(value) = inline {
                    let label = if matches!(option, b"--help" | b"-h") {
                        "-h/--help"
                    } else {
                        std::str::from_utf8(option).expect("constant option")
                    };
                    out.fail(format!(
                        "argument {label}: ignored explicit argument {}",
                        value::V::from(value.to_string_lossy().as_ref()).repr()
                    ));
                } else if matches!(option, b"--help" | b"-h") {
                    if !show {
                        return Args::default();
                    }
                    if out.error.is_none() {
                        out.help = true;
                    }
                } else if option == b"--json" {
                    out.json = true;
                }
            }
            Optional::Value if !show => return out,
            _ => unexpected.push(raw.to_string_lossy().into_owned()),
        }
        i += 1;
    }
    if !out.help && out.error.is_none() {
        if show && out.attempt.is_none() {
            out.fail("the following arguments are required: --attempt".into());
        } else if !unexpected.is_empty() {
            out.fail(format!("unrecognized arguments: {}", unexpected.join(" ")));
        }
    }
    out
}
/// Classify full command-local argv without acquiring data or running the command.
pub fn owns(args: &[OsString]) -> bool {
    parse(args).owned
}
fn execute(args: Args, cwd: &Path, interrupted: &AtomicBool) -> Result<Vec<u8>, String> {
    if std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(OsStr::new("1")) {
        return Err("a harness child may only use read-only runtime actions".into());
    }
    if interrupted.load(Ordering::Relaxed) {
        return Err("acquisition cancelled".into());
    }
    let attempt = args
        .attempt
        .ok_or("the following arguments are required: --attempt")?;
    let root = source::root(args.repo.as_deref().unwrap_or(cwd), cwd)?;
    let rows = source::rows(&root, Path::new(".oms/lifecycle/events.jsonl"), true)?;
    let attempts = projection::project_values(&rows)?;
    let id = attempt
        .to_str()
        .filter(|s| projection::id(s))
        .ok_or("attempt must match [A-Za-z0-9][A-Za-z0-9._:-]{0,159}")?;
    let attempt =
        projection::select(&attempts, id).ok_or_else(|| format!("unknown attempt: {id}"))?;
    let mut bundle = bundle::build(
        &root,
        attempt,
        &rows,
        args.transcript.as_deref(),
        cwd,
        interrupted,
    )
    .map_err(|e| {
        format!(
            "post-mortem bundle failed: {}",
            e.chars().take(200).collect::<String>()
        )
    })?;
    use std::os::unix::ffi::OsStrExt;
    let env: Vec<_> = std::env::vars_os()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect();
    let refs: Vec<_> = env
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    bundle::scanned(&mut bundle, &refs).map_err(|e| format!("post-mortem bundle failed: {e}"))?;
    if interrupted.load(Ordering::Relaxed) {
        return Err("acquisition cancelled".into());
    }
    let output = if args.json {
        bundle.json(true)?
    } else {
        render::text(&bundle)?
    };
    bundle::checked_output(output, &refs).map_err(|e| format!("post-mortem bundle failed: {e}"))
}
/// Run the bundle slice once. Status 75 with empty streams delegates genuinely unowned actions.
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let parsed = parse(args);
    if !parsed.owned {
        return (75, vec![], vec![]);
    }
    if parsed.help {
        return (0, include_bytes!("help.txt").to_vec(), vec![]);
    }
    if let Some(error) = &parsed.error {
        return (
            2,
            vec![],
            format!("{USAGE}agent-events.sh show: error: {error}\n").into_bytes(),
        );
    }
    match execute(parsed, default_repo, interrupted) {
        Ok(out) => (0, out, vec![]),
        Err(error) => (2, vec![], format!("error: {error}\n").into_bytes()),
    }
}
