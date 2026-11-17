use num_bigint::BigInt;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_general_category::{GeneralCategory, get_general_category};

const USAGE: &str = "Usage: slurm-lint.sh SCRIPT [--snapshot FILE]\n       slurm-lint.sh --preview SCRIPT [--snapshot FILE]\n       slurm-lint.sh --template\n\nStatic check of a Slurm batch script. Nothing is submitted or executed.\n\nWarnings:\n  time      no --time: the queue's default or maximum decides.\n  log       --output/--error is not job-specific (no %j; an array needs %a\n            beside %A).\n  array     --array has no concurrency limit (%N).\n  requeue   the job's QOS preempts by REQUEUE but the script does not ask for\n            --requeue (or asks for --no-requeue).\n  signal    that QOS requeues but the script has neither --signal nor a trap,\n            or traps in the batch shell while --signal lacks the B: prefix.\n  maxwall   --time is longer than the QOS MaxWall.\n\nThe QOS is the script's --qos, else the snapshot's \"QOS default\". Its\nPreemptMode and MaxWall come from the snapshot's QOS table (default\nlocal/slurm.md, or OH_MY_SETTING_SLURM_REF, or --snapshot). Without a snapshot\nor a known QOS the preemption checks report \"unknown\", never a pass.\n\n--preview prints what the script asks for, from its #SBATCH lines only: GPUs,\nwall time, array size, and the upper-bound GPU-hours (GPUs x wall x array\ntasks, as if every task ran to its limit). It sets those beside the per-job\nand group GPU limits the snapshot records (QOS and association rows, with\nthe snapshot's age). A quantity the script or snapshot does not state is\nshown as unknown, never guessed. Nothing is submitted, and Slurm's own\n--test-only is not used. Exit 0 unless the script or arguments are unusable.\n\n--template prints a preemption-aware starting script (templates/slurm-job.sh).\n\nExit: 0 no warning, 1 at least one warning, 2 usage or unreadable script.\n";

#[derive(Default, Debug)]
struct Parsed {
    opts: BTreeMap<String, String>,
    body: Vec<String>,
}

fn path_from_arg(value: &OsStr, cwd: &Path) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// The router has matched the public verb; every leaf argument form belongs to this command.
pub fn owns(_args: &[OsString]) -> bool {
    true
}

/// Runs static analysis with distinct caller and validated source roots. File content is read-only.
pub fn run_with_source(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_reader(args, cwd, source_root, interrupted, &mut |path| {
        fs::read(path)
    })
}

fn run_with_reader(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut script = OsString::new();
    let env_snapshot = std::env::var_os("OH_MY_SETTING_SLURM_REF").filter(|v| !v.is_empty());
    let (mut snapshot, mut snapshot_label) = env_snapshot.map_or_else(
        || {
            let path = source_root.join("local/slurm.md");
            (path.clone(), path.into_os_string())
        },
        |value| (path_from_arg(&value, cwd), value),
    );
    let mut preview = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.to_str() {
            Some("-h" | "--help") => return (0, USAGE.as_bytes().to_vec(), Vec::new()),
            Some("--preview") => {
                preview = true;
                i += 1;
            }
            Some("--template") => {
                if i + 1 != args.len() || !script.is_empty() {
                    return failure("--template takes no other argument\n".into());
                }
                let path = source_root.join("templates/slurm-job.sh");
                if !path.is_file() {
                    return failure_arg("template missing: ", path.as_os_str());
                }
                return match read(&path) {
                    Ok(bytes) => (0, bytes, Vec::new()),
                    Err(error) => {
                        let mut err = b"cat: ".to_vec();
                        err.extend_from_slice(path.as_os_str().as_encoded_bytes());
                        err.extend_from_slice(
                            format!(": {}\n", error_description(&error)).as_bytes(),
                        );
                        (1, Vec::new(), err)
                    }
                };
            }
            Some("--snapshot") => {
                if i + 1 >= args.len() {
                    return failure("--snapshot requires a file\n".into());
                }
                snapshot = path_from_arg(&args[i + 1], cwd);
                snapshot_label = args[i + 1].clone();
                i += 2;
            }
            _ if arg.as_encoded_bytes().starts_with(b"-") => {
                return failure_arg("unknown argument: ", arg);
            }
            _ => {
                if !script.is_empty() {
                    return failure("one script at a time\n".into());
                }
                script = arg.clone();
                i += 1;
            }
        }
    }
    if script.is_empty() {
        return (2, Vec::new(), USAGE.as_bytes().to_vec());
    }
    let script_path = path_from_arg(&script, cwd);
    if !script_path.is_file() {
        return failure_arg("cannot read script: ", &script);
    }
    if interrupted.load(Ordering::Relaxed) {
        return interrupted_result();
    }
    let bytes = match read(&script_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return failure_arg("cannot read script: ", &script);
        }
        Err(error) => return read_failure("script", &error),
    };
    let parsed = parse_script(&String::from_utf8_lossy(&bytes), interrupted);
    if interrupted.load(Ordering::Relaxed) {
        return interrupted_result();
    }
    // One snapshot observation supplies QOS, association rows and the generation stamp.
    let snap_bytes = read(&snapshot).ok();
    let snap_text = snap_bytes.as_deref().map(String::from_utf8_lossy);
    let snap = Snapshot::parse(snap_text.as_deref());
    if interrupted.load(Ordering::Relaxed) {
        return interrupted_result();
    }
    let result = if preview {
        preview_output(&mut out, &script, &snapshot_label, &parsed, &snap).map(|()| 0)
    } else {
        lint_output(&parsed, &snap).map(|(code, text)| {
            out.extend_from_slice(b"slurm-lint: ");
            out.extend_from_slice(script.as_encoded_bytes());
            out.extend_from_slice(text.as_bytes());
            code
        })
    };
    if interrupted.load(Ordering::Relaxed) {
        return interrupted_result();
    }
    match result {
        Ok(code) => (code, out, Vec::new()),
        Err(error) => (1, out, format!("{}\n", error.0).into_bytes()),
    }
}

fn interrupted_result() -> (i32, Vec<u8>, Vec<u8>) {
    (130, Vec::new(), b"error: interrupted\n".to_vec())
}

fn failure_arg(prefix: &str, value: &OsStr) -> (i32, Vec<u8>, Vec<u8>) {
    let mut err = format!("error: {prefix}").into_bytes();
    err.extend_from_slice(value.as_encoded_bytes());
    err.push(b'\n');
    (2, Vec::new(), err)
}

fn failure(message: String) -> (i32, Vec<u8>, Vec<u8>) {
    (2, Vec::new(), format!("error: {message}").into_bytes())
}
fn error_description(error: &std::io::Error) -> String {
    let description = error.to_string();
    description
        .split(" (os error")
        .next()
        .unwrap_or(&description)
        .to_owned()
}
fn read_failure(which: &str, error: &std::io::Error) -> (i32, Vec<u8>, Vec<u8>) {
    failure(format!(
        "cannot read {which}: {}\n",
        error_description(error)
    ))
}

fn python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{001c}'..='\u{001f}')
}

fn lines(mut remaining: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        if let Some((end, separator)) = remaining.char_indices().find(|(_, c)| {
            matches!(
                c,
                '\n' | '\r'
                    | '\u{000b}'
                    | '\u{000c}'
                    | '\u{001c}'
                    | '\u{001d}'
                    | '\u{001e}'
                    | '\u{0085}'
                    | '\u{2028}'
                    | '\u{2029}'
            )
        }) {
            let line = &remaining[..end];
            remaining = &remaining[end + separator.len_utf8()..];
            if separator == '\r' && remaining.starts_with('\n') {
                remaining = &remaining[1..];
            }
            Some(line)
        } else {
            let line = remaining;
            remaining = "";
            Some(line)
        }
    })
}
fn parse_script(source: &str, interrupted: &AtomicBool) -> Parsed {
    let mut p = Parsed::default();
    let mut header = true;
    let short = [
        ("t", "time"),
        ("o", "output"),
        ("e", "error"),
        ("a", "array"),
        ("q", "qos"),
        ("N", "nodes"),
        ("n", "ntasks"),
        ("G", "gpus"),
        ("p", "partition"),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    for (n, line) in lines(source).enumerate() {
        if n % 128 == 0 && interrupted.load(Ordering::Relaxed) {
            break;
        }
        let text = line.trim_matches(python_whitespace);
        if header && text.starts_with("#SBATCH") {
            let cleaned = adapt_quoted_escapes(strip_shell_comment(&text[7..]));
            let Some(words) = shlex::split(&cleaned) else {
                continue;
            };
            let mut i = 0;
            while i < words.len() {
                let w = &words[i];
                if let Some(long) = w.strip_prefix("--") {
                    let (name, value, has) = long
                        .split_once('=')
                        .map(|(a, b)| (a, b, true))
                        .unwrap_or((long, "", false));
                    let value = if !has
                        && !matches!(name, "requeue" | "no-requeue")
                        && i + 1 < words.len()
                        && !words[i + 1].starts_with('-')
                    {
                        i += 1;
                        words[i].as_str()
                    } else {
                        value
                    };
                    p.opts.insert(name.into(), value.into());
                } else if w.starts_with('-') && w.len() >= 2 {
                    let tail = &w[1..];
                    let end = tail.chars().next().unwrap().len_utf8();
                    let key = &tail[..end];
                    let name = short.get(key).copied().unwrap_or(key);
                    let mut val = &tail[end..];
                    if val.is_empty() && i + 1 < words.len() && !words[i + 1].starts_with('-') {
                        i += 1;
                        val = &words[i];
                    }
                    p.opts.insert(name.into(), val.into());
                }
                i += 1;
            }
            continue;
        }
        if !text.is_empty() && !text.starts_with('#') {
            header = false;
            p.body.push(text.to_owned());
        }
    }
    p
}
fn rows(text: &str, heading: &str) -> (Vec<BTreeMap<String, String>>, String) {
    let mut found = Vec::new();
    let (mut inside, mut cols, mut stamp) = (false, None::<Vec<String>>, String::new());
    for line in lines(text) {
        if let Some(x) = line.strip_prefix("Generated:") {
            stamp = x.trim_matches(python_whitespace).into();
        }
        if line.starts_with("## ") {
            inside = if heading == "## QOS" {
                line.trim_matches(python_whitespace) == heading
            } else {
                line.starts_with(heading)
            };
            cols = None;
            continue;
        }
        if !inside || line.starts_with("```") || !line.contains('|') {
            continue;
        }
        let cells = line.split('|').map(str::to_owned).collect::<Vec<_>>();
        if let Some(c) = &cols {
            if cells.len() == c.len() {
                let key = cells.first().cloned().unwrap_or_default();
                let mut row: BTreeMap<String, String> = c.iter().cloned().zip(cells).collect();
                row.insert("\0row-key".into(), key);
                found.push(row);
            }
        } else {
            cols = Some(cells);
        }
    }
    (found, stamp)
}
fn strip_shell_comment(input: &str) -> &str {
    let (mut quote, mut escaped) = (None, false);
    for (i, c) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '#' {
            return &input[..i];
        }
    }
    input
}
// Python shlex only unescapes quotes and backslashes within double quotes.
// The declared POSIX lexer also unescapes dollar signs and backticks; protect those two.
fn adapt_quoted_escapes(input: &str) -> String {
    let mut output = String::new();
    let mut quote = None;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        output.push(c);
        if c == '\\' && quote != Some('\'') {
            if let Some(next) = chars.next() {
                if quote == Some('"') && matches!(next, '$' | '`') {
                    output.push('\\');
                }
                output.push(next);
            }
        } else if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        }
    }
    output
}

struct Snapshot {
    qos: BTreeMap<String, BTreeMap<String, String>>,
    default: String,
    why: String,
    associations: Vec<BTreeMap<String, String>>,
    stamp: String,
}
impl Snapshot {
    fn parse(text: Option<&str>) -> Self {
        let (qos, default, why) = qos(text);
        let (associations, stamp) = rows(text.unwrap_or(""), "## Associations");
        Self {
            qos,
            default,
            why,
            associations,
            stamp,
        }
    }
}

fn qos(text: Option<&str>) -> (BTreeMap<String, BTreeMap<String, String>>, String, String) {
    let Some(text) = text else {
        return (
            BTreeMap::new(),
            String::new(),
            "no cluster snapshot; run: oms snapshot --cluster".into(),
        );
    };
    let mut default = String::new();
    for l in lines(text) {
        if let Some(x) = l.strip_prefix("- QOS default:") {
            let d = x.trim_matches(python_whitespace);
            default = if d.starts_with('(') {
                String::new()
            } else {
                d.into()
            };
        }
    }
    let (rows, _) = rows(text, "## QOS");
    let mut map = BTreeMap::new();
    for r in rows {
        let key = r.get("\0row-key").cloned().unwrap_or_default();
        map.insert(key, r);
    }
    let why = if map.is_empty() {
        "the snapshot has no QOS table"
    } else {
        ""
    };
    (map, default, why.into())
}
fn decimal_digit(c: char) -> Option<u8> {
    const ZEROES: &[u32] = &[
        0x0030, 0x0660, 0x06f0, 0x07c0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0be6, 0x0c66,
        0x0ce6, 0x0d66, 0x0de6, 0x0e50, 0x0ed0, 0x0f20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946,
        0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0,
        0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0,
        0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50,
        0x11da0, 0x11f50, 0x16a60, 0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6,
        0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950, 0x1fbf0,
    ];
    let n = c as u32;
    if get_general_category(c) != GeneralCategory::DecimalNumber {
        return None;
    }
    ZEROES
        .iter()
        .find_map(|z| (n >= *z && n < *z + 10).then(|| (n - *z) as u8))
}
fn big(s: &str) -> Option<BigInt> {
    if s.is_empty() {
        return None;
    }
    let ascii: String = s
        .chars()
        .map(|c| decimal_digit(c).map(|d| char::from(b'0' + d)))
        .collect::<Option<_>>()?;
    ascii.parse().ok()
}
#[derive(Debug)]
struct NumericError(String);

fn integer_digit_limit() -> usize {
    std::env::var("PYTHONINTMAXSTRDIGITS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|limit| *limit == 0 || *limit >= 640)
        .map_or(4300, |limit| limit as usize)
}

fn decimal_literal(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| decimal_digit(c).is_some())
}

// Unicode Numeric_Type=Digit also includes these non-decimal digits, but excludes fractions.
fn python_digit(c: char) -> bool {
    decimal_digit(c).is_some()
        || matches!(c as u32,
        0x00b2..=0x00b3 | 0x00b9 | 0x1369..=0x1371 | 0x19da | 0x2070 | 0x2074..=0x2079
        | 0x2080..=0x2089 | 0x2460..=0x2468 | 0x2474..=0x247c | 0x2488..=0x2490 | 0x24ea
        | 0x24f5..=0x24fd | 0x24ff | 0x2776..=0x277e | 0x2780..=0x2788 | 0x278a..=0x2792
        | 0x10a40..=0x10a43 | 0x10e60..=0x10e68 | 0x11052..=0x1105a | 0x1f100..=0x1f10a)
}

fn python_isdigit(value: &str) -> bool {
    !value.is_empty() && value.chars().all(python_digit)
}

fn python_int(value: &str) -> Result<BigInt, NumericError> {
    if !decimal_literal(value) {
        let repr = format!("'{value}'").chars().take(200).collect::<String>();
        return Err(NumericError(format!(
            "ValueError: invalid literal for int() with base 10: {repr}"
        )));
    }
    let digits = value.chars().count();
    let limit = integer_digit_limit();
    if limit != 0 && digits > limit {
        return Err(NumericError(format!(
            "ValueError: Exceeds the limit ({limit} digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
        )));
    }
    Ok(big(value).unwrap())
}

fn integer_text(value: &BigInt) -> Result<String, NumericError> {
    let text = value.to_string();
    let digits = text.len() - usize::from(text.starts_with('-'));
    let limit = integer_digit_limit();
    if limit != 0 && digits > limit {
        return Err(NumericError(format!(
            "ValueError: Exceeds the limit ({limit} digits) for integer string conversion; use sys.set_int_max_str_digits() to increase the limit"
        )));
    }
    Ok(text)
}

fn integer_to_float(value: &BigInt) -> Result<f64, NumericError> {
    // This internal conversion has no decimal-format limit, unlike Python's str(int).
    let number = value.to_string().parse::<f64>().unwrap_or(f64::INFINITY);
    if number.is_finite() {
        Ok(number)
    } else {
        Err(NumericError(
            "OverflowError: int too large to convert to float".into(),
        ))
    }
}

fn seconds(value: Option<&str>) -> Result<Option<BigInt>, NumericError> {
    let value = value.unwrap_or("").trim_matches(python_whitespace);
    if value.is_empty() || ["UNLIMITED", "INFINITE", "-1"].contains(&value.to_uppercase().as_str())
    {
        return Ok(None);
    }
    let (days, rest) = value.rsplit_once('-').unwrap_or(("", value));
    let parts = rest.split(':').collect::<Vec<_>>();
    if parts.len() > 3
        || !parts.iter().all(|part| python_isdigit(part))
        || (!days.is_empty() && !python_isdigit(days))
    {
        return Ok(None);
    }
    let numbers = parts
        .iter()
        .map(|part| python_int(part))
        .collect::<Result<Vec<_>, _>>()?;
    let zero = BigInt::from(0);
    let (hours, minutes, secs) = if !days.is_empty() {
        (
            numbers[0].clone(),
            numbers.get(1).unwrap_or(&zero).clone(),
            numbers.get(2).unwrap_or(&zero).clone(),
        )
    } else if numbers.len() == 1 {
        (zero.clone(), numbers[0].clone(), zero.clone())
    } else if numbers.len() == 2 {
        (zero.clone(), numbers[0].clone(), numbers[1].clone())
    } else {
        (numbers[0].clone(), numbers[1].clone(), numbers[2].clone())
    };
    let days = if days.is_empty() {
        zero
    } else {
        python_int(days)?
    };
    Ok(Some(days * 86400 + hours * 3600 + minutes * 60 + secs))
}

fn first_int(value: Option<&str>, upper: bool) -> Result<Option<BigInt>, NumericError> {
    let value = value.unwrap_or("");
    let mut start = None;
    let mut runs = Vec::new();
    for (offset, c) in value.char_indices() {
        if decimal_digit(c).is_some() {
            start.get_or_insert(offset);
        } else if let Some(begin) = start.take() {
            runs.push(&value[begin..offset]);
        }
    }
    if let Some(begin) = start {
        runs.push(&value[begin..]);
    }
    let selected = if upper { runs.last() } else { runs.first() };
    selected.map(|value| python_int(value)).transpose()
}

fn tres(value: Option<&str>) -> Result<Option<BigInt>, NumericError> {
    let (mut bare, mut typed) = (None, BigInt::from(0));
    for part in value.unwrap_or("").split(',') {
        let part = part.trim_matches(python_whitespace);
        if let Some((kind, count)) = part.split_once('=')
            && (kind == "gres/gpu"
                || kind
                    .strip_prefix("gres/gpu:")
                    .is_some_and(|name| !name.is_empty()))
            && decimal_literal(count)
        {
            let count = python_int(count)?;
            if kind == "gres/gpu" {
                bare = Some(count);
            } else {
                typed += count;
            }
        }
    }
    Ok(bare.or(if typed == BigInt::from(0) {
        None
    } else {
        Some(typed)
    }))
}

fn array_size(spec: &str) -> Result<(Option<BigInt>, Option<BigInt>), NumericError> {
    let (body, limit) = spec.split_once('%').unwrap_or((spec, ""));
    let mut total = BigInt::from(0);
    for part in body.split(',') {
        let part = part.trim_matches(python_whitespace);
        let (low, high, step) = if let Some((low, range)) = part.split_once('-') {
            let (high, step) = range.split_once(':').unwrap_or((range, "1"));
            (low, high, step)
        } else {
            (part, part, "1")
        };
        if ![low, high, step].iter().all(|value| decimal_literal(value)) {
            return Ok((None, None));
        }
        let low = python_int(low)?;
        let high = python_int(high)?;
        let step = python_int(step)?;
        if high < low || step < BigInt::from(1) {
            return Ok((None, None));
        }
        total += (high - low) / step + 1;
    }
    let limit = if python_isdigit(limit) {
        Some(python_int(limit)?)
    } else {
        None
    };
    Ok((Some(total), limit))
}

fn request_gpus(p: &Parsed) -> Result<(Option<BigInt>, String), NumericError> {
    let nodes = first_int(p.opts.get("nodes").map(String::as_str), true)?
        .filter(|n| *n > BigInt::from(0))
        .unwrap_or_else(|| BigInt::from(1));
    let tasks = if let Some(value) = p.opts.get("ntasks") {
        first_int(Some(value), false)?
    } else if let Some(value) = p.opts.get("ntasks-per-node") {
        first_int(Some(value), false)?.map(|n| n * &nodes)
    } else {
        Some(nodes.clone())
    };
    if let Some(value) = p.opts.get("gpus") {
        return Ok((first_int(Some(value), false)?, format!("--gpus={value}")));
    }
    if let Some(value) = p.opts.get("gpus-per-node") {
        let count = first_int(Some(value), false)?.map(|n| n * &nodes);
        return Ok((
            count,
            format!(
                "--gpus-per-node={value} x {} node(s)",
                integer_text(&nodes)?
            ),
        ));
    }
    if let Some(value) = p.opts.get("gpus-per-task") {
        let count = first_int(Some(value), false)?;
        if let (Some(count), Some(tasks)) = (count, tasks) {
            let label = format!(
                "--gpus-per-task={value} x {} task(s)",
                integer_text(&tasks)?
            );
            return Ok((Some(count * tasks), label));
        }
        return Ok((
            None,
            format!("--gpus-per-task={value}, task count not stated"),
        ));
    }
    if let Some(value) = p.opts.get("gres") {
        let mut per_node = BigInt::from(0);
        let mut cleaned = String::new();
        let mut rest = value.as_str();
        while let Some(start) = rest.find('(') {
            let Some(end) = rest[start..].find(')') else {
                break;
            };
            cleaned.push_str(&rest[..start]);
            rest = &rest[start + end + 1..];
        }
        cleaned.push_str(rest);
        for item in cleaned.split(',') {
            let parts = item.split(':').collect::<Vec<_>>();
            if parts[0] == "gpu" {
                let last = parts.last().unwrap();
                per_node += if parts.len() > 1 && python_isdigit(last) {
                    python_int(last)?
                } else {
                    BigInt::from(1)
                };
            }
        }
        return Ok((
            Some(per_node * &nodes),
            format!("--gres={value} x {} node(s)", integer_text(&nodes)?),
        ));
    }
    Ok((
        Some(BigInt::from(0)),
        "no GPU option in the #SBATCH lines".into(),
    ))
}

fn lint_output(p: &Parsed, snap: &Snapshot) -> Result<(i32, String), NumericError> {
    let mut w = Vec::<(&str, String)>::new();
    let mut notes = Vec::new();
    if !p.opts.contains_key("time") {
        w.push((
            "time",
            "no --time: the queue's default or maximum decides how long this runs".into(),
        ));
    }
    let array = p.opts.get("array");
    for key in ["output", "error"] {
        if let Some(target) = p.opts.get(key).filter(|x| !x.is_empty()) {
            let hasj = has_token(target, "jJ");
            let hasa = has_token(target, "A");
            if !hasj && !hasa {
                w.push(("log",format!("--{key} {target} is not job-specific: add %j so runs do not overwrite each other")));
            } else if array.is_some() && hasa && !has_token(target, "ajJ") {
                w.push((
                    "log",
                    format!("--{key} {target} is shared by every array task: add %a"),
                ));
            }
        }
    }
    if let Some(a) = array
        && !a.contains('%')
    {
        w.push((
            "array",
            format!("--array {a} has no concurrency limit: add %N (for example {a}%8)"),
        ));
    }
    let (map, default, why) = (&snap.qos, &snap.default, &snap.why);
    let q = p
        .opts
        .get("qos")
        .filter(|x| !x.is_empty())
        .unwrap_or(default)
        .split(',')
        .next()
        .unwrap_or("")
        .trim_matches(python_whitespace);
    let row = map.get(q);
    if !why.is_empty() {
        notes.push(format!("preemption: unknown ({why})"));
    } else if q.is_empty() {
        notes.push(
            "preemption: unknown (the script names no --qos and the snapshot has no QOS default)"
                .into(),
        );
    } else if let Some(r) = row {
        let mode = r
            .get("PreemptMode")
            .map(|x| x.to_uppercase())
            .unwrap_or_default();
        if !r.contains_key("PreemptMode") {
            notes.push(
                "preemption: unknown (the snapshot's QOS table has no PreemptMode column)".into(),
            );
        } else if mode.contains("REQUEUE") {
            let trapped = p.body.iter().any(|x| {
                x.strip_prefix("trap")
                    .is_some_and(|tail| tail.chars().next().is_none_or(|c| !python_word(c)))
            });
            let signal = p.opts.get("signal");
            if p.opts.contains_key("no-requeue") {
                w.push(("requeue",format!("QOS {q} preempts by REQUEUE but the script says --no-requeue: a preempted job ends instead of returning to the queue")));
            } else if !p.opts.contains_key("requeue") {
                w.push(("requeue",format!("QOS {q} preempts by REQUEUE but the script does not ask for --requeue: whether it returns to the queue then depends on the cluster's JobRequeue, which the snapshot does not record")));
            }
            if signal.is_none() && !trapped {
                w.push(("signal",format!("QOS {q} requeues, and the script has neither --signal nor a trap: the run gets no warning to write a checkpoint")));
            } else if let Some(sig) = signal.filter(|_| trapped) {
                if !sig.to_uppercase().starts_with("B:") {
                    w.push(("signal",format!("--signal={sig} reaches the job steps only; the trap in this script needs the B: prefix (--signal=B:{sig}) to run")));
                }
            } else if signal.is_none() {
                notes.push("signal: the script traps but sets no --signal, so only the scheduler's final TERM arrives".into());
            }
        } else {
            notes.push(format!(
                "preemption: QOS {q} PreemptMode is {}; no requeue check applies",
                if mode.is_empty() { "not set" } else { &mode }
            ));
        }
        let cap = seconds(r.get("MaxWall").map(String::as_str))?;
        let asked = seconds(p.opts.get("time").map(String::as_str))?;
        let unlimited = p.opts.get("time").is_some_and(|x| {
            ["0", "UNLIMITED", "INFINITE", "-1"]
                .contains(&x.trim_matches(python_whitespace).to_uppercase().as_str())
        });
        if cap.as_ref().is_some_and(|c| *c != BigInt::from(0))
            && (asked.as_ref().is_some_and(|a| a > cap.as_ref().unwrap()) || unlimited)
        {
            w.push((
                "maxwall",
                format!(
                    "--time {} is longer than QOS {q} MaxWall {}",
                    p.opts["time"],
                    r.get("MaxWall").map(String::as_str).unwrap_or("")
                ),
            ));
        }
    } else {
        notes.push(format!(
            "preemption: unknown (QOS {q} is not in the snapshot)"
        ));
    }
    let code = i32::from(!w.is_empty());
    let mut out = format!(
        ": {} warning(s); static check, nothing was submitted or run\n",
        w.len()
    );
    for (id, msg) in w {
        out.push_str(&format!("warn: {id}: {msg}\n"));
    }
    for n in notes {
        out.push_str(&format!("note: {n}\n"));
    }
    Ok((code, out))
}
fn python_word(c: char) -> bool {
    c == '_'
        || matches!(
            get_general_category(c),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
}

fn has_token(s: &str, letters: &str) -> bool {
    let chars = s.chars().collect::<Vec<_>>();
    chars.iter().enumerate().any(|(i, c)| {
        *c == '%' && {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            j < chars.len() && letters.contains(chars[j])
        }
    })
}

struct PreviewRequest {
    gpus: Option<BigInt>,
    how: String,
    wall: Option<BigInt>,
    tasks: Option<BigInt>,
    concurrent: Option<BigInt>,
    array_note: String,
}
impl PreviewRequest {
    fn parse(p: &Parsed) -> Result<Self, NumericError> {
        let (gpus, how) = request_gpus(p)?;
        let wall = seconds(p.opts.get("time").map(String::as_str))?;
        let (tasks, concurrent, array_note) = if let Some(array) = p.opts.get("array") {
            let (tasks, concurrent) = array_size(array)?;
            let note = if tasks.is_some() {
                format!("--array={array}")
            } else {
                format!("--array={array} not understood")
            };
            (tasks, concurrent, note)
        } else {
            (Some(BigInt::from(1)), None, "no --array: one job".into())
        };
        Ok(Self {
            gpus,
            how,
            wall,
            tasks,
            concurrent,
            array_note,
        })
    }
}

fn preview_output(
    out: &mut Vec<u8>,
    script: &OsStr,
    snapshot: &OsStr,
    p: &Parsed,
    snap: &Snapshot,
) -> Result<(), NumericError> {
    let request = PreviewRequest::parse(p)?;
    out.extend_from_slice(b"slurm-preview: ");
    out.extend_from_slice(script.as_encoded_bytes());
    out.extend_from_slice(
        b" (read from #SBATCH lines only; nothing was submitted, Slurm --test-only was not used)\n",
    );
    let absent = snap.qos.is_empty() && snap.associations.is_empty() && snap.stamp.is_empty();
    out.extend_from_slice(if absent {
        b"snapshot: none readable at "
    } else {
        b"snapshot: "
    });
    out.extend_from_slice(snapshot.as_encoded_bytes());
    if absent {
        out.extend_from_slice(b"; run: oms snapshot --cluster\n");
    } else {
        let detail = if snap.stamp.is_empty() {
            "no Generated line".into()
        } else {
            age(&snap.stamp)
        };
        out.extend_from_slice(format!(" ({detail})\n").as_bytes());
    }
    let mut text = String::new();
    let result = preview_text(&mut text, p, snap, &request);
    out.extend_from_slice(text.as_bytes());
    result
}
fn preview_text(
    o: &mut String,
    p: &Parsed,
    snap: &Snapshot,
    request: &PreviewRequest,
) -> Result<(), NumericError> {
    let PreviewRequest {
        gpus,
        how,
        wall,
        tasks,
        concurrent,
        array_note,
    } = request;
    let (qrows, default, assoc) = (&snap.qos, &snap.default, &snap.associations);
    o.push_str("\nrequest\n");
    o.push_str(&format!(
        "  GPUs per array task          {}  ({how})\n",
        gpus.as_ref()
            .map(integer_text)
            .transpose()?
            .unwrap_or_else(|| "unknown".into())
    ));
    let shown_wall = if let Some(wall) = wall {
        format!(
            "{} ({:.2} h)",
            p.opts["time"],
            integer_to_float(wall)? / 3600.0
        )
    } else {
        "unknown (no --time, or not understood)".into()
    };
    o.push_str(&format!("  wall time                    {shown_wall}\n"));
    o.push_str(&format!(
        "  array tasks                  {}  ({array_note})\n",
        tasks
            .as_ref()
            .map(integer_text)
            .transpose()?
            .unwrap_or_else(|| "unknown".into())
    ));
    let known = gpus.is_some() && wall.is_some() && tasks.is_some();
    let gpuhrs = if known {
        let product = gpus.as_ref().unwrap() * wall.as_ref().unwrap();
        let x = integer_to_float(&product)? / 3600.0 * integer_to_float(tasks.as_ref().unwrap())?;
        format!("{x:.1}  (GPUs x wall x tasks, as if every task ran to its limit)")
    } else {
        "unknown".into()
    };
    o.push_str(&format!("  GPU-hours, upper bound       {gpuhrs}\n"));
    let effective_concurrency = concurrent.as_ref().filter(|c| **c != BigInt::from(0));
    let peak = if let (Some(g), Some(t)) = (gpus.as_ref(), tasks.as_ref()) {
        let count =
            effective_concurrency.map_or(t.clone(), |c| if c < t { c.clone() } else { t.clone() });
        Some(g * count)
    } else {
        None
    };
    o.push_str(&format!(
        "  GPUs held at once, peak      {}{}\n",
        peak.as_ref()
            .map(integer_text)
            .transpose()?
            .unwrap_or_else(|| "unknown".into()),
        if effective_concurrency.is_none() && tasks.as_ref().is_some_and(|x| *x >= BigInt::from(2))
        {
            "  (no %N limit: every task may run together)"
        } else {
            ""
        }
    ));
    o.push_str("\nlimits recorded in the snapshot (a limit not listed here is not recorded, which is not the same as no limit)\n");
    let q = p
        .opts
        .get("qos")
        .filter(|x| !x.is_empty())
        .unwrap_or(default)
        .split(',')
        .next()
        .unwrap_or("")
        .trim_matches(python_whitespace);
    let qrow = if q.is_empty() { None } else { qrows.get(q) };
    let mut checks: Vec<(String, String, Option<BigInt>)> = Vec::new();
    if let Some(r) = qrow {
        for (c, k) in [
            ("MaxTRESPJ", "per job"),
            ("MaxTRESPU", "at once"),
            ("GrpTRES", "at once"),
        ] {
            checks.push((
                format!("QOS {q} {c}"),
                k.into(),
                tres(r.get(c).map(String::as_str))?,
            ));
        }
    } else {
        o.push_str(&format!(
            "  QOS: {}\n",
            if q.is_empty() {
                "none named and no snapshot default".into()
            } else {
                format!("{q} is not in the snapshot")
            }
        ));
    }
    let part = p.opts.get("partition").filter(|value| !value.is_empty());
    let mine = assoc
        .iter()
        .filter(|r| part.is_none_or(|x| r.get("Partition").is_some_and(|v| v.is_empty() || v == x)))
        .collect::<Vec<_>>();
    for (c, k) in [
        ("MaxTRESPJ", "per job"),
        ("MaxTRES", "per job"),
        ("GrpTRES", "at once"),
    ] {
        let vals = mine
            .iter()
            .map(|r| tres(r.get(c).map(String::as_str)))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if let Some(v) = vals.into_iter().min() {
            checks.push((
                format!("association {c} ({} row(s), lowest)", mine.len()),
                k.into(),
                Some(v),
            ));
        }
    }
    for (label, kind, limit) in checks {
        let asked = if kind == "per job" {
            gpus.as_ref()
        } else {
            peak.as_ref()
        };
        if let Some(l) = limit {
            let verdict = if let Some(asked) = asked {
                if asked <= &l {
                    "within".into()
                } else {
                    format!("OVER by {}", integer_text(&(asked - &l))?)
                }
            } else {
                "request unknown".into()
            };
            let limit = integer_text(&l)?;
            let shown_asked = asked
                .map(integer_text)
                .transpose()?
                .unwrap_or_else(|| "?".into());
            o.push_str(&format!(
                "  {label:<44} gres/gpu={limit:<4} {kind} asks {shown_asked:<4} {verdict}\n"
            ));
        }
    }
    let cap = if let Some(row) = qrow {
        seconds(row.get("MaxWall").map(String::as_str))?
    } else {
        None
    };
    if let (Some(c), Some(w)) = (cap, wall) {
        let verdict = if w <= &c { "within" } else { "OVER" };
        o.push_str(&format!(
            "  {:<44} {:<9} wall asks {}  {verdict}\n",
            format!("QOS {q} MaxWall"),
            qrow.unwrap()["MaxWall"],
            p.opts.get("time").unwrap_or(&String::new())
        ));
    }
    o.push_str("  GPU-hour budgets (GrpTRESMins) are not in the snapshot, so the GPU-hours above have no limit to compare with.\n");
    Ok(())
}
fn age(stamp: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Ok(t) = time_parse(stamp) else {
        return "generation time unreadable".into();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let delta = (now - t).max(0);
    let days = delta / 86400;
    let mut x = format!("{stamp}, {days}d {}h old", delta % 86400 / 3600);
    if days >= 7 {
        x.push_str(" - older than 7 days; refresh with: oms snapshot --cluster");
    }
    x
}
fn time_parse(s: &str) -> Result<i64, ()> {
    let s = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix('z'))
        .ok_or(())?;
    let (date, clock) = s.split_once(['T', 't']).ok_or(())?;
    let date = date.split('-').collect::<Vec<_>>();
    let clock = clock.split(':').collect::<Vec<_>>();
    if date.len() != 3 || clock.len() != 3 || date[0].chars().count() != 4 {
        return Err(());
    }
    let y = big(date[0])
        .and_then(|n| n.to_string().parse::<i64>().ok())
        .ok_or(())?;
    let month = date[1].as_bytes();
    if !matches!(
        month,
        [b'1'..=b'9'] | [b'0', b'1'..=b'9'] | [b'1', b'0'..=b'2']
    ) {
        return Err(());
    }
    let m = date[1].parse::<i64>().map_err(|_| ())?;
    let day = date[2];
    // strptime's day grammar permits Unicode only after an ASCII 1 or 2.
    let valid_day = match day.chars().collect::<Vec<_>>().as_slice() {
        ['1'..='9'] | [' ' | '0', '1'..='9'] | ['3', '0'..='1'] => true,
        ['1' | '2', units] => decimal_digit(*units).is_some(),
        _ => false,
    };
    if !valid_day {
        return Err(());
    }
    let d = big(day.trim_start_matches(' '))
        .and_then(|n| n.to_string().parse::<i64>().ok())
        .ok_or(())?;
    let h = timestamp_number(clock[0], '1', Some(('2', '3')))?;
    let mi = timestamp_number(clock[1], '5', None)?;
    let se = timestamp_number(clock[2], '5', Some(('6', '1')))?;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if y < 1
        || !(1..=12).contains(&m)
        || d < 1
        || d > month_days[(m - 1) as usize]
        || h > 23
        || mi > 59
        || se > 61
    {
        return Err(());
    }
    let yy = y - (m <= 2) as i64;
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Ok((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + se)
}

// Shapes mirror strptime's decimal fields: unpadded digits are accepted, signs are not.
fn timestamp_number(
    value: &str,
    regular_tens: char,
    special: Option<(char, char)>,
) -> Result<i64, ()> {
    let digits = value.chars().collect::<Vec<_>>();
    let valid = match digits.as_slice() {
        [digit] => decimal_digit(*digit).is_some(),
        [tens, units] => {
            (('0'..=regular_tens).contains(tens) && decimal_digit(*units).is_some())
                || special.is_some_and(|(t, maximum)| *tens == t && ('0'..=maximum).contains(units))
        }
        _ => false,
    };
    if !valid {
        return Err(());
    }
    big(value)
        .and_then(|n| n.to_string().parse().ok())
        .ok_or(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn one_read_per_input_and_permission_errors_use_the_requested_path() {
        let root = std::env::temp_dir().join(format!("oms-slurm-read-once-{}", std::process::id()));
        fs::create_dir_all(root.join("templates")).unwrap();
        let _cleanup = Cleanup(root.clone());
        fs::write(root.join("job.sh"), b"#SBATCH --time=60 --gpus=2\n").unwrap();
        fs::write(root.join("snapshot.md"), b"Generated: 9999-1-1T0:0:0Z\n- QOS default: high\n## QOS\nName|MaxTRESPJ\nhigh|gres/gpu=2\n## Associations (current user)\nPartition|GrpTRES\ngpu|gres/gpu=1\n").unwrap();
        fs::write(root.join("templates/slurm-job.sh"), b"fixture\n").unwrap();
        let mut reads = BTreeMap::new();
        let result = run_with_reader(
            &[
                "--preview".into(),
                "job.sh".into(),
                "--snapshot".into(),
                "snapshot.md".into(),
            ],
            &root,
            &root,
            &AtomicBool::new(false),
            &mut |path| {
                *reads.entry(path.to_path_buf()).or_insert(0) += 1;
                fs::read(path)
            },
        );
        assert_eq!(result.0, 0);
        assert_eq!(
            reads,
            BTreeMap::from([(root.join("job.sh"), 1), (root.join("snapshot.md"), 1)])
        );
        let output = String::from_utf8(result.1).unwrap();
        assert!(output.contains("9999-1-1T0:0:0Z, 0d 0h old"));
        assert!(output.contains("QOS high MaxTRESPJ"));
        assert!(output.contains("association GrpTRES (1 row(s), lowest)"));
        assert!(output.contains("OVER by 1"));

        let denied = run_with_reader(
            &["job.sh".into()],
            &root,
            &root,
            &AtomicBool::new(false),
            &mut |_| Err(std::io::ErrorKind::PermissionDenied.into()),
        );
        assert_eq!(
            denied,
            (
                2,
                Vec::new(),
                b"error: cannot read script: job.sh\n".to_vec()
            )
        );
        let template = run_with_reader(
            &["--template".into()],
            &root,
            &root,
            &AtomicBool::new(false),
            &mut |_| Err(std::io::ErrorKind::PermissionDenied.into()),
        );
        let mut expected = b"cat: ".to_vec();
        expected.extend_from_slice(
            root.join("templates/slurm-job.sh")
                .as_os_str()
                .as_encoded_bytes(),
        );
        expected.extend_from_slice(b": permission denied\n");
        assert_eq!(template, (1, Vec::new(), expected));
    }
}
