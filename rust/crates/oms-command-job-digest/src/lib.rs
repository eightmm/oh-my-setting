#![forbid(unsafe_code)]
use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use regex::Regex;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HELP: &str = r#"Usage: job-digest.sh <logfile>
       job-digest.sh <slurm-job-id> [logfile]
       job-digest.sh --metrics FILE [logfile | slurm-job-id [logfile]]

Emit a compact markdown digest of a training/Slurm run: sacct summary (job id
mode), error-pattern hits, the last Python traceback, and the log tail.

Options:
  --tail N      Lines of raw tail to include. Default: 30.
  --patterns N  Max error-pattern lines to include. Default: 40.
  --wait        Job-id mode only: block until the job leaves the queue
                (squeue), then digest. Poll interval OMS_JOB_DIGEST_POLL=30s.
  --wait-timeout S
                With --wait: stop observing after S seconds, digest what
                exists, mark the job pending and exit 124. The job itself is
                never cancelled or resubmitted. Default: wait without limit.
  --max-bytes N Cap the digest at N bytes (>= 512) of whole lines and say what
                was omitted. Default: no cap.
  --metrics FILE
                Add a metrics section from FILE: JSONL records
                {"step","name","value","ts"} (see LOGGING.md) or DVCLive
                plots/metrics .tsv (a file or a directory of them). Shows the
                last step, latest and best value, recent slope, non-finite
                values and the time since the last record. A read of the file
                only; no process is watched. Can be used without a log.
  -h, --help    Show this help.

Leaving the queue is not success: confirm State/ExitCode and required outputs.
The command does not run training; pair it with the run ledger or sbatch.
"#;
fn fail(message: impl std::fmt::Display) -> (i32, Vec<u8>, Vec<u8>) {
    (2, vec![], format!("error: {message}\n").into_bytes())
}
fn fail_path(prefix: &str, value: &OsStr) -> (i32, Vec<u8>, Vec<u8>) {
    let mut err = format!("error: {prefix}").into_bytes();
    err.extend_from_slice(value.as_bytes());
    err.push(b'\n');
    (2, vec![], err)
}
fn path(repo: &Path, arg: &OsStr) -> PathBuf {
    let p = Path::new(arg);
    if p.is_absolute() {
        p.to_owned()
    } else {
        repo.join(p)
    }
}
fn number(s: &OsStr) -> Option<u64> {
    let s = s.to_str()?;
    if s.is_empty() || !s.bytes().all(|x| x.is_ascii_digit()) {
        None
    } else {
        s.parse().ok()
    }
}
fn executable(name: &str, repo: &Path) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| {
        let p = if p.is_absolute() { p } else { repo.join(p) };
        fs::metadata(p.join(name)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
fn query(
    name: &str,
    args: &[&str],
    repo: &Path,
    deadline: Option<Instant>,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], vec![]);
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return (124, vec![], vec![]);
    }
    let Ok((out_read, out_write)) = nix::unistd::pipe() else {
        return (1, vec![], vec![]);
    };
    let (err_read, err_write) = if name == "sacct" {
        let Ok(write) = out_write.try_clone() else {
            return (1, vec![], vec![]);
        };
        (None, write)
    } else {
        let Ok((read, write)) = nix::unistd::pipe() else {
            return (1, vec![], vec![]);
        };
        (Some(read), write)
    };
    let mut child = match Command::new(name)
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::from(out_write))
        .stderr(Stdio::from(err_write))
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (127, vec![], vec![]),
    };
    let mut out = fs::File::from(out_read);
    let accounting = name == "sacct";
    let a = std::thread::spawn(move || {
        let mut v = vec![];
        if accounting {
            let mut reader = BufReader::new(out);
            for _ in 0..20 {
                if reader.read_until(b'\n', &mut v).unwrap_or(0) == 0 {
                    break;
                }
            }
        } else {
            let _ = out.read_to_end(&mut v);
        }
        v
    });
    let b = std::thread::spawn(move || {
        let mut v = vec![];
        if let Some(read) = err_read {
            let _ = fs::File::from(read).read_to_end(&mut v);
        }
        v
    });
    let mut status = None;
    let code = loop {
        if interrupted.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.kill();
            let _ = child.wait();
            break if interrupted.load(Ordering::Relaxed) {
                130
            } else {
                124
            };
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(s)) => {
                    status = Some(s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)))
                }
                Ok(None) => {}
                Err(_) => {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(child.id() as i32),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    break 1;
                }
            }
        }
        if let Some(code) = status
            && a.is_finished()
            && b.is_finished()
        {
            break code;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (
        code,
        a.join().unwrap_or_default(),
        b.join().unwrap_or_default(),
    )
}
fn trimmed(v: &[u8]) -> String {
    String::from_utf8_lossy(v)
        .trim_end_matches('\n')
        .to_string()
}
fn python_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
}
// Python float accepts decimal Unicode digits and underscores between digits.
fn python_float(text: &str) -> Option<f64> {
    const ZEROES: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
        0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x16a60, 0x16ac0,
        0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e950, 0x1fbf0,
    ];
    let normalized: String = python_trim(text)
        .chars()
        .map(|c| {
            ZEROES
                .iter()
                .find_map(|&z| {
                    (z..z + 10)
                        .contains(&(c as u32))
                        .then(|| char::from_u32(c as u32 - z + 48).unwrap())
                })
                .unwrap_or(c)
        })
        .collect();
    let bytes = normalized.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'_'
            && (i == 0
                || i + 1 == bytes.len()
                || !bytes[i - 1].is_ascii_digit()
                || !bytes[i + 1].is_ascii_digit())
        {
            return None;
        }
    }
    let text = normalized.replace('_', "");
    match text.to_ascii_lowercase().as_str() {
        "nan" | "+nan" | "-nan" => Some(f64::NAN),
        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
        x => x.parse().ok(),
    }
}
fn float(raw: &RawValue) -> Option<f64> {
    let s = raw.get();
    if s.starts_with('"') {
        python_float(&serde_json::from_str::<String>(s).ok()?)
    } else {
        python_float(s)
    }
}
fn decimal(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        s.parse().ok()
    }
}
// fromisoformat accepts basic/calendar and ISO-week dates, an arbitrary single
// date/time separator, reduced times, and offsets with fractional seconds.
fn iso_date(text: &str) -> Option<(NaiveDate, usize)> {
    let year = decimal(text.get(..4)?)? as i32;
    if year == 0 {
        return None;
    }
    let extended = text.as_bytes().get(4) == Some(&b'-');
    let start = if extended { 5 } else { 4 };
    if text.as_bytes().get(start) == Some(&b'W') {
        let week = decimal(text.get(start + 1..start + 3)?)?;
        let mut end = start + 3;
        let remaining = text.get(end..)?;
        let digit_separator = remaining.len() >= 3
            && remaining.bytes().all(|b| b.is_ascii_digit())
            && remaining.len() % 2 == 1;
        let day = if extended && text.as_bytes().get(end) == Some(&b'-') {
            end += 2;
            decimal(text.get(end - 1..end)?)?
        } else if !extended
            && !digit_separator
            && text.as_bytes().get(end).is_some_and(u8::is_ascii_digit)
        {
            end += 1;
            decimal(text.get(end - 1..end)?)?
        } else {
            1
        };
        let weekday = match day {
            1 => Weekday::Mon,
            2 => Weekday::Tue,
            3 => Weekday::Wed,
            4 => Weekday::Thu,
            5 => Weekday::Fri,
            6 => Weekday::Sat,
            7 => Weekday::Sun,
            _ => return None,
        };
        Some((NaiveDate::from_isoywd_opt(year, week, weekday)?, end))
    } else {
        let month = decimal(text.get(start..start + 2)?)?;
        let day_start = start + if extended { 3 } else { 2 };
        if extended && text.as_bytes().get(start + 2) != Some(&b'-') {
            return None;
        }
        let day = decimal(text.get(day_start..day_start + 2)?)?;
        Some((NaiveDate::from_ymd_opt(year, month, day)?, day_start + 2))
    }
}
fn iso_clock(text: &str, offset: bool) -> Option<(u32, u32, u32, u32)> {
    let (main, fraction) = text
        .split_once(['.', ','])
        .map_or((text, None), |(a, b)| (a, Some(b)));
    let mut fields = Vec::new();
    if main.contains(':') {
        for part in main.split(':') {
            if part.len() != 2 {
                return None;
            }
            fields.push(decimal(part)?);
        }
    } else {
        if !matches!(main.len(), 2 | 4 | 6) {
            return None;
        }
        for i in (0..main.len()).step_by(2) {
            fields.push(decimal(main.get(i..i + 2)?)?);
        }
    }
    if fields.is_empty() || fields.len() > 3 {
        return None;
    }
    let hour = fields[0];
    let minute = *fields.get(1).unwrap_or(&0);
    let second = *fields.get(2).unwrap_or(&0);
    if hour >= 24
        || (!offset && (minute >= 60 || second >= 60))
        || hour * 3600 + minute * 60 + second >= 86400
    {
        return None;
    }
    let micro = if let Some(f) = fraction {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits = &f[..f.len().min(6)];
        decimal(digits)? * 10u32.pow(6 - digits.len() as u32)
    } else {
        0
    };
    Some((hour, minute, second, micro))
}
fn iso_timestamp(text: &str) -> Option<f64> {
    let (date, end) = iso_date(text)?;
    if !(1..=9999).contains(&date.year()) {
        return None;
    }
    let mut rest = text.get(end..)?;
    if rest.is_empty() {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as f64);
    }
    rest = rest.get(rest.chars().next()?.len_utf8()..)?;
    let offset_start = rest.find(['+', '-']);
    let (clock, offset) = if let Some(i) = offset_start {
        let (clock, zone) = rest.split_at(i);
        let (h, m, s, us) = iso_clock(&zone[1..], true)?;
        let integral = h * 3600 + m * 60 + s;
        let offset = if integral == 0 {
            0i64
        } else {
            integral as i64 * 1_000_000 + us as i64
        };
        (
            clock,
            if zone.starts_with('-') {
                -offset
            } else {
                offset
            },
        )
    } else if let Some(clock) = rest.strip_suffix('Z') {
        (clock, 0i64)
    } else {
        (rest, 0i64)
    };
    let (h, m, s, us) = iso_clock(clock, false)?;
    let dt = date.and_hms_micro_opt(h, m, s, us)?.and_utc();
    let micros = dt.timestamp() * 1_000_000 + dt.timestamp_subsec_micros() as i64 - offset;
    Some(micros.div_euclid(1_000_000) as f64 + micros.rem_euclid(1_000_000) as f64 / 1e6)
}
fn timestamp(raw: &RawValue) -> Option<f64> {
    if let Some(v) = float(raw) {
        return if v.is_finite() {
            Some(if v > 1e11 { v / 1000.0 } else { v })
        } else {
            None
        };
    }
    let s: String = serde_json::from_str(raw.get()).ok()?;
    iso_timestamp(python_trim(&s))
}
fn step_text(step: f64) -> String {
    if step == 0.0 {
        "0".into()
    } else {
        format!("{:.0}", step.trunc())
    }
}
#[derive(Default)]
struct Series {
    count: usize,
    recent: VecDeque<(f64, f64)>,
    min: Option<(f64, f64)>,
    max: Option<(f64, f64)>,
    nonfinite: usize,
    first: f64,
    ts: Option<f64>,
}
impl Series {
    fn add(&mut self, step: f64, value: f64, ts: Option<f64>) {
        self.count += 1;
        if let Some(t) = ts {
            self.ts = Some(self.ts.map_or(t, |x| x.max(t)));
        }
        if value.is_finite() {
            if self.min.is_none_or(|x| value < x.1) {
                self.min = Some((step, value));
            }
            if self.max.is_none_or(|x| value > x.1) {
                self.max = Some((step, value));
            }
        } else {
            if self.nonfinite == 0 {
                self.first = step;
            }
            self.nonfinite += 1;
        }
        if self.recent.len() == 10 {
            self.recent.pop_front();
        }
        self.recent.push_back((step, value));
    }
    fn slope(&self) -> Result<Option<f64>, ()> {
        let p: Vec<_> = self.recent.iter().filter(|x| x.1.is_finite()).collect();
        if p.len() < 3 {
            return Ok(None);
        }
        if p.iter().all(|x| x.0 == p[0].0) {
            return Ok(None);
        }
        let mx = p.iter().map(|x| x.0).sum::<f64>() / p.len() as f64;
        let my = p.iter().map(|x| x.1).sum::<f64>() / p.len() as f64;
        let mut den = 0.0;
        for x in &p {
            let difference = x.0 - mx;
            let square = difference.powi(2);
            if difference.is_finite() && square.is_infinite() {
                return Err(());
            }
            den += square;
        }
        if den == 0.0 {
            Ok(None)
        } else {
            Ok(Some(
                p.iter().map(|x| (x.0 - mx) * (x.1 - my)).sum::<f64>() / den,
            ))
        }
    }
}
fn fmt_g(x: f64, precision: i32, signed: bool) -> String {
    if !x.is_finite() {
        let value = if x.is_nan() {
            "nan"
        } else if x.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        };
        return if signed && (x.is_nan() || x.is_sign_positive()) {
            format!("+{value}")
        } else {
            value.into()
        };
    }
    let rounded = format!("{:.*e}", (precision - 1) as usize, x);
    let exp = rounded
        .split_once('e')
        .and_then(|(_, e)| e.parse::<i32>().ok())
        .unwrap_or(0);
    let sign = if signed && x.is_sign_positive() {
        "+"
    } else {
        ""
    };
    if exp < -4 || exp >= precision {
        let (m, e) = rounded.split_once('e').unwrap();
        let m = m.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().unwrap_or(0);
        format!(
            "{sign}{m}e{}{:02}",
            if e >= 0 { "+" } else { "-" },
            e.unsigned_abs()
        )
    } else {
        let n = (precision - 1 - exp).max(0) as usize;
        let s = format!("{x:.n$}");
        let s = if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.')
        } else {
            &s
        };
        format!("{sign}{s}")
    }
}
fn age(seconds: f64) -> String {
    if seconds < 0.0 {
        "in the future (clock skew?)".into()
    } else if seconds < 90.0 {
        format!("{}s ago", seconds as u64)
    } else if seconds < 5400.0 {
        format!("{}m ago", seconds as u64 / 60)
    } else if seconds < 172800.0 {
        format!("{:.1}h ago", seconds.trunc() / 3600.0)
    } else {
        format!("{:.1}d ago", seconds.trunc() / 86400.0)
    }
}
fn is_tsv(path: &Path) -> bool {
    path.as_os_str().as_bytes().ends_with(b".tsv")
}
fn collect_tsv(p: &Path, files: &mut Vec<PathBuf>, interrupted: &AtomicBool) -> io::Result<()> {
    collect_tsv_with(p, files, interrupted, &mut |p| fs::read_dir(p))
}
fn collect_tsv_with<F>(
    p: &Path,
    files: &mut Vec<PathBuf>,
    interrupted: &AtomicBool,
    read_dir: &mut F,
) -> io::Result<()>
where
    F: FnMut(&Path) -> io::Result<fs::ReadDir>,
{
    check_cancel(interrupted)?;
    let directory = read_dir(p);
    check_cancel(interrupted)?;
    if let Ok(rd) = directory {
        for entry in rd {
            check_cancel(interrupted)?;
            let Ok(entry) = entry else {
                continue;
            };
            let p = entry.path();
            if p.is_dir() {
                if !entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                    collect_tsv_with(&p, files, interrupted, read_dir)?;
                }
            } else if is_tsv(&p) {
                files.push(p);
            }
        }
    }
    Ok(())
}
fn raw_string(s: &str) -> Box<RawValue> {
    RawValue::from_string(serde_json::to_string(s).unwrap()).unwrap()
}
// Protect non-finite leaves with a per-row sentinel that cannot collide with input.
fn python_json_row(line: &str) -> Option<BTreeMap<String, Box<RawValue>>> {
    let mut marker = "__oms_nonfinite".to_owned();
    while line.contains(&marker) {
        marker.push('_');
    }
    let mut adapted = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if quoted {
            adapted.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            adapted.push(c);
        } else if c == 'N' || c == 'I' || c == '-' {
            let remainder: String = chars
                .clone()
                .take(if c == 'N' {
                    2
                } else if c == 'I' {
                    7
                } else {
                    8
                })
                .collect();
            let literal = format!("{c}{remainder}");
            if matches!(literal.as_str(), "NaN" | "Infinity" | "-Infinity") {
                for _ in remainder.chars() {
                    chars.next();
                }
                adapted.push_str(&format!("{{\"{marker}\":\"{literal}\"}}"));
            } else {
                adapted.push(c);
            }
        } else {
            adapted.push(c);
        }
    }
    let validated: Box<RawValue> = serde_json::from_str(&adapted).ok()?;
    let text = validated.get();
    if !text.starts_with('{') {
        return None;
    }
    let mut row = BTreeMap::new();
    let bytes = text.as_bytes();
    let mut i = 1;
    while i < bytes.len() - 1 {
        if bytes[i].is_ascii_whitespace() || bytes[i] == b',' {
            i += 1;
            continue;
        }
        let start = i;
        i = json_string_end(bytes, i)?;
        let name = MetricName::json(
            RawValue::from_string(text[start..i].to_owned())
                .ok()?
                .as_ref(),
        )?;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let start = i;
        let mut depth = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i = json_string_end(bytes, i)?;
                    continue;
                }
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => break,
                b'-' | b'0'..=b'9' => {
                    let begin = i;
                    i += 1;
                    while bytes.get(i).is_some_and(|b| {
                        b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
                    }) {
                        i += 1;
                    }
                    let number = &text[begin..i];
                    if !number.contains(['.', 'e', 'E'])
                        && number.trim_start_matches('-').len() > 4300
                    {
                        return None;
                    }
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        for field in ["step", "name", "value", "ts"] {
            if name == MetricName::from_text(field) {
                row.insert(
                    field.to_owned(),
                    RawValue::from_string(text[start..i].trim().to_owned()).ok()?,
                );
                break;
            }
        }
    }
    for key in ["step", "value", "ts"] {
        if let Some(value) = row.get_mut(key) {
            for token in ["NaN", "Infinity", "-Infinity"] {
                if value.get() == format!("{{\"{marker}\":\"{token}\"}}") {
                    *value = raw_string(token);
                    break;
                }
            }
        }
    }
    Some(row)
}
fn json_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(i + 1),
            b'\\' => i += 1,
            _ => {}
        }
        i += 1;
    }
    None
}
fn integer_float_overflow(raw: &RawValue) -> bool {
    let text = raw.get();
    !text.starts_with('"')
        && !text.contains(['.', 'e', 'E'])
        && float(raw).is_some_and(|v| v.is_infinite())
}
#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct MetricName(Vec<u32>);
impl MetricName {
    fn from_text(s: &str) -> Self {
        Self(s.chars().map(u32::from).collect())
    }
    fn from_os(s: &OsStr) -> Self {
        let mut rest = s.as_bytes();
        let mut points = Vec::new();
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    points.extend(text.chars().map(u32::from));
                    break;
                }
                Err(error) => {
                    let (valid, invalid) = rest.split_at(error.valid_up_to());
                    points.extend(std::str::from_utf8(valid).unwrap().chars().map(u32::from));
                    let length = error.error_len().unwrap_or(invalid.len());
                    points.extend(invalid[..length].iter().map(|&b| 0xdc00 + b as u32));
                    rest = &invalid[length..];
                }
            }
        }
        Self(points)
    }
    fn json(raw: &RawValue) -> Option<Self> {
        let text = raw.get();
        if !text.starts_with('"') {
            return None;
        }
        let mut chars = text[1..text.len() - 1].chars();
        let mut units = Vec::new();
        while let Some(c) = chars.next() {
            if c != '\\' {
                units.push(c as u32);
                continue;
            }
            let c = chars.next()?;
            units.push(match c {
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    u32::from_str_radix(&hex, 16).ok()?
                }
                'n' => 10,
                'r' => 13,
                't' => 9,
                'b' => 8,
                'f' => 12,
                '"' => 34,
                '\\' => 92,
                '/' => 47,
                _ => return None,
            });
        }
        let mut points = Vec::new();
        let mut i = 0;
        while i < units.len() {
            let high = units[i];
            if (0xd800..=0xdbff).contains(&high)
                && units
                    .get(i + 1)
                    .is_some_and(|low| (0xdc00..=0xdfff).contains(low))
            {
                points.push(0x10000 + ((high - 0xd800) << 10) + units[i + 1] - 0xdc00);
                i += 2;
            } else {
                points.push(high);
                i += 1;
            }
        }
        Some(Self(points))
    }
    fn bytes(&self) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        for &point in &self.0 {
            if (0xdc80..=0xdcff).contains(&point) {
                bytes.push((point - 0xdc00) as u8);
            } else {
                let c = char::from_u32(point)?;
                bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        Some(bytes)
    }
    fn lower(&self) -> String {
        self.0
            .iter()
            .map(|&p| char::from_u32(p).unwrap_or('\u{fffd}'))
            .flat_map(char::to_lowercase)
            .collect()
    }
}
// Admit complete output lines, so an oversized line never consumes the cap or
// prevents a later short line from being retained.
struct DigestOutput {
    bytes: Vec<u8>,
    pending: Vec<u8>,
    pending_len: u64,
    cap: Option<usize>,
    omitted: u64,
    omitted_bytes: u64,
}
impl DigestOutput {
    fn new(cap: Option<usize>) -> Self {
        Self {
            bytes: Vec::new(),
            pending: Vec::new(),
            pending_len: 0,
            cap,
            omitted: 0,
            omitted_bytes: 0,
        }
    }
    fn fits(&self, additional: u64) -> bool {
        self.cap.is_none_or(|cap| {
            self.pending_len.saturating_add(additional) <= (cap - 160 - self.bytes.len()) as u64
        })
    }
    fn discard_content(&mut self, count: u64) {
        self.pending_len += count;
        self.pending.clear();
    }
    fn flush_line(&mut self) {
        if self.fits(0) {
            self.bytes.append(&mut self.pending);
        } else {
            self.omitted += 1;
            self.omitted_bytes += self.pending_len;
        }
        self.pending.clear();
        self.pending_len = 0;
    }
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if self.cap.is_none() {
            self.bytes.extend_from_slice(bytes);
            return;
        }
        for piece in bytes.split_inclusive(|&b| b == b'\n') {
            if self.fits(piece.len() as u64) {
                self.pending.extend_from_slice(piece);
            } else {
                self.pending.clear();
            }
            self.pending_len += piece.len() as u64;
            if piece.last() == Some(&b'\n') {
                self.flush_line();
            }
        }
    }
    fn push(&mut self, byte: u8) {
        self.extend_from_slice(&[byte]);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.cap.is_some() && self.pending_len > 0 {
            self.flush_line();
        }
        if self.omitted > 0 {
            self.bytes.extend_from_slice(format!("\n[job-digest: {} line(s), {} bytes omitted by --max-bytes {}; read the log for more]\n", self.omitted, self.omitted_bytes, self.cap.unwrap()).as_bytes());
        }
        self.bytes
    }
}
const TRACEBACK: &[u8] = b"Traceback (most recent call last)";
const ERROR_PATTERN: &str = r"(?-u:Traceback|ERROR|Error:|error:|OOM|[Oo]ut of memory|CUDA|NCCL|NaN|nan loss|Killed|Segmentation fault|exitcode|srun: error)";
// Seekable logs retain ranges; nonseekable logs share selected contents so
// rendering never replays a stream. Oversized capped lines retain only lengths.
#[derive(Clone)]
enum LogContent {
    File,
    Captured(Rc<Vec<u8>>),
    Omitted,
}
#[derive(Clone)]
struct LogRecord {
    offset: u64,
    length: u64,
    newline: bool,
    pattern: bool,
    traceback: bool,
    line: u64,
    content: LogContent,
}
struct LogReader<'a, R> {
    reader: R,
    offset: u64,
    pattern: regex::bytes::Regex,
    capture: bool,
    capture_limit: Option<usize>,
    interrupted: &'a AtomicBool,
}
impl<'a, R: BufRead> LogReader<'a, R> {
    fn new(reader: R, offset: u64, interrupted: &'a AtomicBool) -> Self {
        Self {
            reader,
            offset,
            pattern: regex::bytes::Regex::new(ERROR_PATTERN).unwrap(),
            capture: false,
            capture_limit: None,
            interrupted,
        }
    }
    // Match only the finite literal patterns; the overlap holds every possible
    // cross-buffer match without retaining the current log line.
    fn next(&mut self) -> io::Result<Option<LogRecord>> {
        let mut record = LogRecord {
            offset: self.offset,
            length: 0,
            newline: false,
            pattern: false,
            traceback: false,
            line: 0,
            content: LogContent::File,
        };
        let mut captured = self.capture.then(Vec::new);
        let mut overlap = Vec::with_capacity(TRACEBACK.len() - 1);
        loop {
            if self.interrupted.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let buffer = self.reader.fill_buf()?;
            check_cancel(self.interrupted)?;
            if buffer.is_empty() {
                record.content = if self.capture {
                    captured.map_or(LogContent::Omitted, |bytes| {
                        LogContent::Captured(Rc::new(bytes))
                    })
                } else {
                    LogContent::File
                };
                return Ok((record.length > 0).then_some(record));
            }
            let end = buffer.iter().position(|&b| b == b'\n');
            let content = &buffer[..end.unwrap_or(buffer.len())];
            let mut boundary = overlap;
            boundary.extend_from_slice(&content[..content.len().min(TRACEBACK.len() - 1)]);
            record.pattern |= self.pattern.is_match(content) || self.pattern.is_match(&boundary);
            record.traceback |= content.windows(TRACEBACK.len()).any(|s| s == TRACEBACK)
                || boundary.windows(TRACEBACK.len()).any(|s| s == TRACEBACK);
            if content.len() >= TRACEBACK.len() - 1 {
                overlap = content[content.len() - (TRACEBACK.len() - 1)..].to_vec();
            } else {
                overlap = boundary[boundary.len().saturating_sub(TRACEBACK.len() - 1)..].to_vec();
            }
            if let Some(bytes) = &mut captured {
                if self
                    .capture_limit
                    .is_some_and(|limit| record.length + content.len() as u64 > limit as u64)
                {
                    captured = None;
                } else {
                    bytes.extend_from_slice(content);
                }
            }
            record.length += content.len() as u64;
            let consumed = content.len() + usize::from(end.is_some());
            self.offset += consumed as u64;
            self.reader.consume(consumed);
            if end.is_some() {
                record.newline = true;
                record.content = if self.capture {
                    captured.map_or(LogContent::Omitted, |bytes| {
                        LogContent::Captured(Rc::new(bytes))
                    })
                } else {
                    LogContent::File
                };
                return Ok(Some(record));
            }
        }
    }
}
struct LogSummary {
    newlines: u64,
    matches: u64,
    patterns: VecDeque<LogRecord>,
    traceback: Vec<LogRecord>,
    tail: VecDeque<LogRecord>,
    captured: bool,
}
fn inspect_log(
    target: &Path,
    tail: usize,
    patterns: usize,
    cap: Option<usize>,
    interrupted: &AtomicBool,
) -> io::Result<LogSummary> {
    let mut file = fs::File::open(target)?;
    let captured = file
        .seek(SeekFrom::End(0))
        .and_then(|_| file.seek(SeekFrom::Start(0)))
        .is_err();
    if captured {
        file = fs::File::open(target)?;
    }
    inspect_reader(
        BufReader::new(file),
        tail,
        patterns,
        cap,
        captured,
        interrupted,
    )
}
fn inspect_reader<R: BufRead>(
    input: R,
    tail: usize,
    patterns: usize,
    cap: Option<usize>,
    captured: bool,
    interrupted: &AtomicBool,
) -> io::Result<LogSummary> {
    let mut reader = LogReader::new(input, 0, interrupted);
    reader.capture = captured;
    reader.capture_limit = cap.map(|n| n - 160);
    let mut summary = LogSummary {
        newlines: 0,
        matches: 0,
        patterns: VecDeque::new(),
        traceback: Vec::new(),
        tail: VecDeque::new(),
        captured,
    };
    let mut line = 0;
    while let Some(mut record) = reader.next()? {
        line += 1;
        record.line = line;
        summary.newlines += u64::from(record.newline);
        if record.pattern {
            summary.matches += 1;
            if patterns > 0 {
                if summary.patterns.len() == patterns {
                    summary.patterns.pop_front();
                }
                summary.patterns.push_back(record.clone());
            }
        }
        if record.traceback {
            summary.traceback.clear();
        }
        if (record.traceback || !summary.traceback.is_empty()) && summary.traceback.len() < 40 {
            summary.traceback.push(record.clone());
        }
        if tail > 0 {
            if summary.tail.len() == tail {
                summary.tail.pop_front();
            }
            summary.tail.push_back(record);
        }
    }
    Ok(summary)
}
fn copy_record<R: Read + Seek>(
    file: &mut Option<R>,
    record: &LogRecord,
    newline: bool,
    output: &mut DigestOutput,
    interrupted: &AtomicBool,
) -> io::Result<()> {
    check_cancel(interrupted)?;
    if !output.fits(record.length) {
        output.discard_content(record.length);
    } else {
        match &record.content {
            LogContent::Captured(bytes) => output.extend_from_slice(bytes),
            LogContent::Omitted => output.discard_content(record.length),
            LogContent::File => {
                let file = file.as_mut().expect("seekable log retains a file reader");
                file.seek(SeekFrom::Start(record.offset))?;
                let mut remaining = record.length;
                let mut buffer = [0; 8192];
                while remaining > 0 {
                    check_cancel(interrupted)?;
                    let length = remaining.min(buffer.len() as u64) as usize;
                    let count = file.read(&mut buffer[..length])?;
                    check_cancel(interrupted)?;
                    if count == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "log changed while reading selected lines",
                        ));
                    }
                    output.extend_from_slice(&buffer[..count]);
                    remaining -= count as u64;
                }
            }
        }
    }
    if newline {
        output.push(b'\n');
    }
    Ok(())
}
fn emit_log(
    target: &Path,
    summary: &LogSummary,
    output: &mut DigestOutput,
    interrupted: &AtomicBool,
) -> io::Result<()> {
    check_cancel(interrupted)?;
    let mut source = if summary.captured {
        None
    } else {
        Some(fs::File::open(target)?)
    };
    emit_reader(&mut source, summary, output, interrupted)
}
fn emit_reader<R: Read + Seek>(
    source: &mut Option<R>,
    summary: &LogSummary,
    output: &mut DigestOutput,
    interrupted: &AtomicBool,
) -> io::Result<()> {
    output.extend_from_slice(b"\n## Error patterns\n\n");
    if summary.matches == 0 {
        output.extend_from_slice(b"No error patterns matched.\n");
    } else {
        output.extend_from_slice(b"```\n");
        for record in &summary.patterns {
            output.extend_from_slice(format!("{}:", record.line).as_bytes());
            copy_record(source, record, true, output, interrupted)?;
        }
        output.extend_from_slice(b"```\n");
    }
    output.extend_from_slice(b"\n## Last traceback\n\n");
    if summary.traceback.is_empty() {
        output.extend_from_slice(b"No Python traceback found.\n");
    } else {
        output.extend_from_slice(b"```\n");
        for record in &summary.traceback {
            copy_record(source, record, true, output, interrupted)?;
        }
        output.extend_from_slice(b"```\n");
    }
    output.extend_from_slice(b"\n## Tail\n\n```\n");
    for record in &summary.tail {
        copy_record(source, record, record.newline, output, interrupted)?;
    }
    output.extend_from_slice(b"```\n");
    Ok(())
}
fn check_cancel(interrupted: &AtomicBool) -> io::Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}
// TextIO's universal newlines include bare CR and a CRLF split across reads.
fn metric_record(
    reader: &mut impl BufRead,
    record: &mut Vec<u8>,
    interrupted: &AtomicBool,
) -> io::Result<bool> {
    check_cancel(interrupted)?;
    record.clear();
    loop {
        check_cancel(interrupted)?;
        let buffer = reader.fill_buf()?;
        check_cancel(interrupted)?;
        if buffer.is_empty() {
            return Ok(!record.is_empty());
        }
        let end = buffer.iter().position(|&b| b == b'\r' || b == b'\n');
        let count = end.unwrap_or(buffer.len());
        record.extend_from_slice(&buffer[..count]);
        let terminator = end.map(|i| buffer[i]);
        reader.consume(count + usize::from(end.is_some()));
        if let Some(terminator) = terminator {
            if terminator == b'\r' {
                check_cancel(interrupted)?;
                let next = reader.fill_buf()?.first().copied();
                check_cancel(interrupted)?;
                if next == Some(b'\n') {
                    reader.consume(1);
                }
            }
            return Ok(true);
        }
    }
}
struct MetricsError {
    class: &'static str,
    message: String,
    file: Option<PathBuf>,
}
impl MetricsError {
    fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            file: None,
        }
    }
    fn io(file: &Path, error: io::Error) -> Self {
        let class = match error.kind() {
            io::ErrorKind::Interrupted => "InterruptedError",
            io::ErrorKind::NotFound => "FileNotFoundError",
            io::ErrorKind::PermissionDenied => "PermissionError",
            io::ErrorKind::IsADirectory => "IsADirectoryError",
            _ => "OSError",
        };
        Self {
            class,
            message: error.to_string(),
            file: Some(file.to_owned()),
        }
    }
    fn step(step: f64) -> Self {
        if step.is_nan() {
            Self::new("ValueError", "cannot convert float NaN to integer")
        } else {
            Self::new("OverflowError", "cannot convert float infinity to integer")
        }
    }
    fn encoding(name: &MetricName) -> Self {
        let point = name
            .0
            .iter()
            .find(|&&p| char::from_u32(p).is_none() && !(0xdc80..=0xdcff).contains(&p))
            .unwrap();
        Self::new(
            "UnicodeEncodeError",
            format!("'utf-8' codec cannot encode surrogate \\u{point:04x}: surrogates not allowed"),
        )
    }
    fn emit(&self, stderr: &mut Vec<u8>) {
        stderr.extend_from_slice(
            format!("job-digest: metrics: {}: {}", self.class, self.message).as_bytes(),
        );
        if let Some(file) = &self.file {
            stderr.extend_from_slice(b": ");
            stderr.extend_from_slice(file.as_os_str().as_bytes());
        }
        stderr.push(b'\n');
    }
}
fn log_error(stderr: &mut Vec<u8>, display: &OsStr, error: &io::Error) {
    stderr.extend_from_slice(b"job-digest: cannot read log ");
    stderr.extend_from_slice(display.as_bytes());
    stderr.extend_from_slice(format!(": {error}\n").as_bytes());
}
fn order_metric_files(files: &mut Vec<PathBuf>, interrupted: &AtomicBool) -> io::Result<()> {
    check_cancel(interrupted)?;
    let mut ordered = BTreeMap::new();
    for (index, file) in std::mem::take(files).into_iter().enumerate() {
        check_cancel(interrupted)?;
        ordered.insert((MetricName::from_os(file.as_os_str()), index), file);
    }
    for (_, file) in ordered {
        check_cancel(interrupted)?;
        files.push(file);
    }
    Ok(())
}
fn metrics(
    target: &Path,
    display: &OsStr,
    stderr: &mut Vec<u8>,
    interrupted: &AtomicBool,
) -> Result<Vec<u8>, MetricsError> {
    check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
    let mut files = vec![];
    if target.is_dir() {
        collect_tsv(target, &mut files, interrupted).map_err(|e| MetricsError::io(target, e))?;
        check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
        order_metric_files(&mut files, interrupted).map_err(|e| MetricsError::io(target, e))?;
    } else {
        files.push(target.to_owned());
    }
    let mut series: BTreeMap<MetricName, Series> = BTreeMap::new();
    let mut skipped = 0usize;
    for p in &files {
        check_cancel(interrupted).map_err(|e| MetricsError::io(p, e))?;
        let file = fs::File::open(p).map_err(|e| MetricsError::io(p, e))?;
        let mut reader = BufReader::new(file);
        let mut record = Vec::new();
        if is_tsv(p) {
            metric_record(&mut reader, &mut record, interrupted)
                .map_err(|e| MetricsError::io(p, e))?;
            let text = String::from_utf8_lossy(&record).into_owned();
            let header: Vec<_> = text.split('\t').collect();
            let Some(si) = header.iter().position(|x| *x == "step") else {
                skipped += 1;
                continue;
            };
            let ti = header.iter().position(|x| *x == "timestamp");
            let cols: Vec<_> = header
                .iter()
                .enumerate()
                .filter(|(_, x)| **x != "step" && **x != "timestamp")
                .collect();
            while metric_record(&mut reader, &mut record, interrupted)
                .map_err(|e| MetricsError::io(p, e))?
            {
                let line = String::from_utf8_lossy(&record);
                let cells: Vec<_> = line.split('\t').collect();
                if cells.len() != header.len() {
                    if !python_trim(&line).is_empty() {
                        skipped += 1;
                    }
                    continue;
                }
                let Some(step) = float(&raw_string(cells[si])) else {
                    skipped += 1;
                    continue;
                };
                let ts = ti.and_then(|i| timestamp(&raw_string(cells[i])));
                for (i, name) in &cols {
                    if let Some(value) = float(&raw_string(cells[*i])) {
                        let name = if cols.len() == 1 && (name.is_empty() || **name == "value") {
                            let stem = p.file_name().unwrap_or_default().as_bytes();
                            let stem = if stem[..stem.len() - 4].iter().all(|&b| b == b'.') {
                                stem
                            } else {
                                &stem[..stem.len() - 4]
                            };
                            MetricName::from_os(OsStr::from_bytes(stem))
                        } else {
                            MetricName::from_text(name)
                        };
                        if !step.is_finite() {
                            return Err(MetricsError::step(step));
                        }
                        series.entry(name).or_default().add(step.trunc(), value, ts);
                    } else {
                        skipped += 1;
                    }
                }
            }
        } else {
            while metric_record(&mut reader, &mut record, interrupted)
                .map_err(|e| MetricsError::io(p, e))?
            {
                let text = String::from_utf8_lossy(&record);
                let line = python_trim(&text);
                if line.is_empty() {
                    continue;
                }
                let Some(row) = python_json_row(line) else {
                    skipped += 1;
                    continue;
                };
                if ["step", "value"].iter().any(|field| {
                    row.get(*field)
                        .is_some_and(|raw| integer_float_overflow(raw))
                }) {
                    return Err(MetricsError::new(
                        "OverflowError",
                        "int too large to convert to float",
                    ));
                }
                let name = row
                    .get("name")
                    .and_then(|x| MetricName::json(x))
                    .filter(|x| !x.0.is_empty());
                let value = row.get("value").and_then(|x| float(x));
                let step = row.get("step").and_then(|x| float(x));
                if let (Some(name), Some(value), Some(step)) = (name, value, step) {
                    if !step.is_finite() {
                        return Err(MetricsError::step(step));
                    }
                    if row.get("ts").is_some_and(|raw| integer_float_overflow(raw)) {
                        return Err(MetricsError::new(
                            "OverflowError",
                            "int too large to convert to float",
                        ));
                    }
                    let ts = row.get("ts").and_then(|x| timestamp(x));
                    series.entry(name).or_default().add(step.trunc(), value, ts);
                } else {
                    skipped += 1;
                }
            }
        }
    }
    check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
    let mut out = format!(
        "## Metrics\n\n- source: {} ({} file(s), {} record(s) skipped as malformed)\n",
        display.to_string_lossy(),
        files.len(),
        skipped
    );
    if series.is_empty() {
        out.push_str("\nNo metric records found: not evidence that the run made no progress.\n");
        return Ok(out.into_bytes());
    }
    let mut last = None;
    for item in series.values() {
        check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
        if let Some(t) = item.ts {
            last = Some(last.map_or(t, |value: f64| value.max(t)));
        }
    }
    if let Some(t) = last {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        out.push_str(&format!(
            "- last record: {} (file observation only; not proof the run is alive or dead)\n",
            age(now - t)
        ));
    } else {
        out.push_str("- last record: no timestamp in the file; staleness cannot be read\n");
    }
    out.push_str("\n| metric | records | last step | latest | best (rule) | slope/step (last 10) | non-finite |\n|---|---|---|---|---|---|---|\n");
    let mut out = out.into_bytes();
    for (name, s) in series.iter().take(20) {
        check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
        let low = name.lower();
        let lower = ["loss", "err", "perplex", "ppl", "mae", "mse", "rmse", "nll"]
            .iter()
            .any(|x| low.contains(x));
        let (step, value) = s.recent.back().unwrap();
        let best = if lower { s.min } else { s.max };
        let best = best.map_or("n/a".into(), |(n, v)| {
            format!(
                "{} @{} ({})",
                fmt_g(v, 6, false),
                step_text(n),
                if lower { "min" } else { "max" }
            )
        });
        let Ok(slope) = s.slope() else {
            MetricsError::new(
                "OverflowError",
                "numerical result out of range while calculating slope",
            )
            .emit(stderr);
            out.extend_from_slice(b"Metrics section failed; read the file directly.\n");
            return Ok(out);
        };
        let slope = slope.map_or("n/a".into(), |x| fmt_g(x, 3, true));
        let nonfinite = if s.nonfinite == 0 {
            "0".into()
        } else {
            format!("{}, first @{}", s.nonfinite, step_text(s.first))
        };
        let Some(name) = name.bytes() else {
            MetricsError::encoding(name).emit(stderr);
            out.extend_from_slice(b"Metrics section failed; read the file directly.\n");
            return Ok(out);
        };
        out.extend_from_slice(b"| ");
        out.extend_from_slice(&name);
        out.extend_from_slice(
            format!(
                " | {} | {} | {} | {best} | {slope} | {nonfinite} |\n",
                s.count,
                step_text(*step),
                fmt_g(*value, 6, false)
            )
            .as_bytes(),
        );
    }
    if series.len() > 20 {
        out.extend_from_slice(
            format!("\n{} more metric(s) not shown.\n", series.len() - 20).as_bytes(),
        );
    }
    let mut flagged = Vec::new();
    for (name, item) in &series {
        check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
        if item.nonfinite > 0 {
            flagged.push(name);
        }
    }
    if !flagged.is_empty() {
        let mut line = b"\nNon-finite values recorded in: ".to_vec();
        for (i, name) in flagged.iter().enumerate() {
            check_cancel(interrupted).map_err(|e| MetricsError::io(target, e))?;
            let Some(name) = name.bytes() else {
                MetricsError::encoding(name).emit(stderr);
                out.extend_from_slice(b"Metrics section failed; read the file directly.\n");
                return Ok(out);
            };
            if i > 0 {
                line.extend_from_slice(b", ");
            }
            line.extend_from_slice(&name);
        }
        line.extend_from_slice(b".\n");
        out.extend_from_slice(&line);
    }
    out.extend_from_slice(b"\nBest uses min for names containing loss/err/ppl/mae/mse/nll, otherwise max; check the direction for your metric.\n");
    Ok(out)
}

/// This handler owns every command-local argument sequence.
pub fn owns(_args: &[OsString]) -> bool {
    true
}

/// Run a job digest without changing the process working directory.
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut tail = 30usize;
    let mut patterns = 40usize;
    let mut wait = false;
    let mut timeout = None;
    let mut cap = None;
    let mut metric: Option<OsString> = None;
    let mut positional = vec![];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        match a.as_ref() {
            "-h" | "--help" => return (0, HELP.as_bytes().to_vec(), vec![]),
            "--wait" => wait = true,
            "--tail" | "--patterns" | "--wait-timeout" | "--max-bytes" | "--metrics" => {
                let option = a.to_string();
                i += 1;
                if i >= args.len() {
                    return fail(match option.as_str() {
                        "--tail" => "--tail requires count",
                        "--patterns" => "--patterns requires count",
                        "--wait-timeout" => "--wait-timeout requires seconds",
                        "--max-bytes" => "--max-bytes requires count",
                        _ => "--metrics requires a file or directory",
                    });
                }
                if option == "--metrics" {
                    metric = Some(args[i].clone());
                } else {
                    let Some(n) = number(&args[i]) else {
                        return fail(format!("{option} requires a positive integer"));
                    };
                    match option.as_str() {
                        "--tail" => tail = n as usize,
                        "--patterns" => patterns = n as usize,
                        "--wait-timeout" => {
                            if n == 0 {
                                return fail("--wait-timeout requires a positive integer");
                            }
                            timeout = Some(n);
                        }
                        _ => {
                            if n < 512 {
                                return fail("--max-bytes must be at least 512");
                            }
                            cap = Some(n as usize);
                        }
                    }
                }
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }
    if positional.is_empty() && metric.as_ref().is_none_or(|x| x.is_empty()) {
        return (2, vec![], HELP.as_bytes().to_vec());
    }
    if positional.len() > 2 {
        return fail("too many arguments");
    }
    if let Some(m) = &metric
        && !m.is_empty()
        && !path(default_repo, m).exists()
    {
        return fail_path("metrics file not found: ", m);
    }
    let job_re = Regex::new(r"^[0-9]+(_[0-9]+)?$").unwrap();
    let mut job = None;
    let mut log = None;
    if let Some(first) = positional.first() {
        if path(default_repo, first).is_file() {
            if positional.len() > 1 {
                return fail("too many arguments");
            }
            log = Some(first.clone());
        } else if first.to_str().is_some_and(|s| job_re.is_match(s)) {
            job = Some(first.to_string_lossy().to_string());
            if let Some(second) = positional.get(1) {
                if !second.is_empty() && !path(default_repo, second).is_file() {
                    return fail_path("log file not found: ", second);
                }
                if !second.is_empty() {
                    log = Some(second.clone());
                }
            }
        } else {
            return fail_path("not a log file or job id: ", first);
        }
    }
    if timeout.is_some() && !wait {
        return fail("--wait-timeout requires --wait");
    }
    if wait && job.is_none() {
        return fail("--wait requires a slurm job id");
    }
    if wait && !executable("squeue", default_repo) {
        return fail("--wait needs squeue (Slurm)");
    }
    let mut stderr = String::new();
    let mut pending = false;
    let started = Instant::now();
    let deadline = if wait {
        timeout.and_then(|n| started.checked_add(Duration::from_secs(n)))
    } else {
        None
    };
    if wait {
        let id = job.as_ref().unwrap();
        let poll = std::env::var_os("OMS_JOB_DIGEST_POLL")
            .and_then(|x| number(&x))
            .unwrap_or(30)
            .max(1);
        stderr.push_str(&format!(
            "job-digest: waiting for job {id} to leave the queue (poll {poll}s{})\n",
            timeout.map_or(String::new(), |n| format!(", budget {n}s"))
        ));
        let mut failures = 0;
        let mut last = String::new();
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                pending = true;
                break;
            }
            let (rc, out, err) = query(
                "squeue",
                &["-h", "-j", id],
                default_repo,
                deadline,
                interrupted,
            );
            if interrupted.load(Ordering::Relaxed) {
                return (130, vec![], stderr.into_bytes());
            }
            let err = trimmed(&err);
            let e = err.to_ascii_lowercase();
            if (rc == 0 && trimmed(&out).is_empty())
                || (rc != 0 && (e.contains("invalid job id") || e.contains("unknown job")))
            {
                break;
            }
            if rc != 0 {
                failures += 1;
                let first = err
                    .lines()
                    .next()
                    .unwrap_or("")
                    .bytes()
                    .filter(|b| *b == 9 || (32..=126).contains(b))
                    .take(200)
                    .map(char::from)
                    .collect::<String>();
                let failure = format!("{rc}: {first}");
                if failure != last {
                    stderr.push_str(&format!("job-digest: squeue query failed transiently (rc={failure}); retrying every {poll}s\n"));
                    last = failure;
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                pending = true;
                break;
            }
            let duration = deadline.map_or(Duration::from_secs(poll), |d| {
                d.saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(poll))
            });
            let until = Instant::now() + duration;
            if std::env::var("OMS_POLL_VERBOSE").is_ok_and(|v| v == "1") {
                let remaining = deadline.map_or(String::new(), |d| {
                    format!(
                        " remaining={}s",
                        d.saturating_duration_since(Instant::now())
                            .as_secs_f64()
                            .ceil() as u64
                    )
                });
                stderr.push_str(&format!(
                    "oh-my-setting poll: job-digest elapsed={}s next={}s{}\n",
                    started.elapsed().as_secs(),
                    duration.as_secs_f64().ceil() as u64,
                    remaining
                ));
            }
            while Instant::now() < until && !interrupted.load(Ordering::Relaxed) {
                std::thread::sleep(
                    until
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                );
            }
            if interrupted.load(Ordering::Relaxed) {
                return (130, vec![], stderr.into_bytes());
            }
        }
        if failures > 1 {
            stderr.push_str(&format!(
                "job-digest: squeue query failed {failures} times while waiting\n"
            ));
        }
        if pending {
            stderr.push_str(&format!("job-digest: wait budget {}s spent; observation incomplete, job {id} untouched (exit 124; queue state may be unknown)\n",timeout.unwrap()));
        } else {
            stderr.push_str(&format!(
                "job-digest: job {id} no longer queued; digesting\n"
            ));
        }
    }
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], stderr.into_bytes());
    }
    let mut stderr = stderr.into_bytes();
    let mut output = format!(
        "# Job digest\n\n- generated: {}\n",
        DateTime::<Utc>::from(SystemTime::now()).format("%Y-%m-%dT%H:%M:%SZ")
    );
    if let Some(id) = &job {
        output.push_str(&format!("- slurm job: {id}\n"));
    }
    if pending {
        output.push_str(&format!("- wait: pending after {}s; observation incomplete, not proof of queue state or completion\n",timeout.unwrap()));
    }
    let log_marker = "\u{0}oms-log-path\u{0}";
    let mut log_summary = None;
    let mut log_failed = false;
    if let Some(display) = &log {
        match inspect_log(
            &path(default_repo, display),
            tail,
            patterns,
            cap,
            interrupted,
        ) {
            Ok(summary) => {
                output.push_str(&format!(
                    "- log: {log_marker} ({} lines)\n",
                    summary.newlines
                ));
                log_summary = Some(summary);
            }
            Err(error) => {
                log_error(&mut stderr, display, &error);
                output.push_str(&format!("- log: {log_marker} ( lines)\n"));
                log_failed = true;
            }
        }
    }
    let (rc, _, _) = query(
        "git",
        &["rev-parse", "--git-dir"],
        default_repo,
        None,
        interrupted,
    );
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], stderr);
    }
    if rc == 0 {
        let (_, rev, _) = query(
            "git",
            &["rev-parse", "--short", "HEAD"],
            default_repo,
            None,
            interrupted,
        );
        if interrupted.load(Ordering::Relaxed) {
            return (130, vec![], stderr);
        }
        let (_, status, _) = query(
            "git",
            &["status", "--porcelain", "--untracked-files=no"],
            default_repo,
            None,
            interrupted,
        );
        if interrupted.load(Ordering::Relaxed) {
            return (130, vec![], stderr);
        }
        let rev = trimmed(&rev);
        output.push_str(&format!(
            "- git: {}, {} dirty files\n",
            if rev.is_empty() { "no commit" } else { &rev },
            status.iter().filter(|&&x| x == b'\n').count()
        ));
    }
    let mut accounting_bytes = None;
    let accounting_marker = "\0oms-accounting-output\0";
    if let Some(id) = &job {
        output.push_str("\n## Slurm accounting\n\n");
        if executable("sacct", default_repo) {
            let (rc, out, err) = if deadline.is_some_and(|d| Instant::now() >= d) {
                (
                    124,
                    b"Accounting skipped: observation budget exhausted.".to_vec(),
                    vec![],
                )
            } else {
                query(
                    "sacct",
                    &[
                        "-j",
                        id,
                        "--format=JobID,JobName%20,State,ExitCode,Elapsed,MaxRSS,ReqMem,AllocTRES%40",
                    ],
                    default_repo,
                    deadline,
                    interrupted,
                )
            };
            if interrupted.load(Ordering::Relaxed) {
                return (130, vec![], stderr);
            }
            let mut combined = out;
            combined.extend(err);
            while combined.last() == Some(&b'\n') {
                combined.pop();
            }
            let lines: Vec<_> = combined.split(|&b| b == b'\n').take(20).collect();
            accounting_bytes = Some(lines.join(&b'\n'));
            output.push_str(&format!("```\n{accounting_marker}\n```\n"));
            if rc != 0 || lines.len() <= 2 {
                output.push_str(
                    "Accounting unavailable or not recorded yet: outcome unknown, not verified.\n",
                );
            }
        } else {
            output.push_str("sacct not available on this machine.\n");
        }
    }
    let mut output = output.into_bytes();
    if let Some(accounting) = accounting_bytes {
        let marker = accounting_marker.as_bytes();
        if let Some(pos) = output.windows(marker.len()).position(|x| x == marker) {
            output.splice(pos..pos + marker.len(), accounting);
        }
    }
    if let Some(log) = &log {
        let marker = log_marker.as_bytes();
        if let Some(pos) = output.windows(marker.len()).position(|x| x == marker) {
            output.splice(pos..pos + marker.len(), log.as_bytes().iter().copied());
        }
    }
    let mut digest = DigestOutput::new(cap);
    digest.extend_from_slice(&output);
    let mut output = digest;
    if log_failed {
        if interrupted.load(Ordering::Relaxed) {
            return (130, vec![], stderr);
        }
        output.extend_from_slice(b"\n## Error patterns\n\nNo error patterns matched.\n\n## Last traceback\n\nNo Python traceback found.\n\n## Tail\n\n```\n");
        return (1, output.finish(), stderr);
    }
    if let (Some(display), Some(summary)) = (&log, &log_summary)
        && let Err(error) = emit_log(
            &path(default_repo, display),
            summary,
            &mut output,
            interrupted,
        )
    {
        if interrupted.load(Ordering::Relaxed) {
            return (130, vec![], stderr);
        }
        log_error(&mut stderr, display, &error);
        return (1, output.finish(), stderr);
    }
    if let Some(m) = metric.filter(|x| !x.is_empty()) {
        output.push(b'\n');
        let marker = OsStr::new("\0oms-metric-path\0");
        match metrics(&path(default_repo, &m), marker, &mut stderr, interrupted) {
            Ok(s) => {
                let mut bytes = s;
                let key = marker.as_bytes();
                if let Some(pos) = bytes.windows(key.len()).position(|x| x == key) {
                    bytes.splice(pos..pos + key.len(), m.as_bytes().iter().copied());
                }
                output.extend_from_slice(&bytes);
            }
            Err(error) => {
                if error.class == "InterruptedError" || interrupted.load(Ordering::Relaxed) {
                    return (130, vec![], stderr);
                }
                error.emit(&mut stderr);
                output.extend_from_slice(b"Metrics section failed; read the file directly.\n")
            }
        }
    }
    if interrupted.load(Ordering::Relaxed) {
        return (130, vec![], stderr);
    }
    (if pending { 124 } else { 0 }, output.finish(), stderr)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;
    use std::rc::Rc;

    struct Controlled<'a> {
        input: Cursor<Vec<u8>>,
        interrupted: &'a AtomicBool,
        cutoff: usize,
        read: Rc<Cell<usize>>,
    }
    impl Read for Controlled<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.input.read(buffer)?;
            self.read.set(self.read.get() + count);
            if self.read.get() >= self.cutoff {
                self.interrupted.store(true, Ordering::Relaxed);
            }
            Ok(count)
        }
    }
    fn reader<'a>(
        bytes: &[u8],
        flag: &'a AtomicBool,
        cutoff: usize,
    ) -> (BufReader<Controlled<'a>>, Rc<Cell<usize>>) {
        let read = Rc::new(Cell::new(0));
        (
            BufReader::with_capacity(
                8,
                Controlled {
                    input: Cursor::new(bytes.to_vec()),
                    interrupted: flag,
                    cutoff,
                    read: read.clone(),
                },
            ),
            read,
        )
    }
    struct Counted {
        input: Cursor<Rc<[u8]>>,
        bytes: Rc<Cell<u64>>,
        eof: Rc<Cell<usize>>,
    }
    impl Read for Counted {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.input.read(buffer)?;
            self.bytes.set(self.bytes.get() + count as u64);
            if count == 0 {
                self.eof.set(self.eof.get() + 1);
            }
            Ok(count)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.input.seek(position)
        }
    }
    #[test]
    fn selected_log_ranges_do_not_rescan_the_input() {
        let line = format!("ERROR {}\n", "x".repeat(4089));
        let data: Rc<[u8]> = line.repeat(2048).into_bytes().into();
        for (captured, cap) in [(false, None), (true, None), (true, Some(512))] {
            let bytes = Rc::new(Cell::new(0));
            let eof = Rc::new(Cell::new(0));
            let source = || Counted {
                input: Cursor::new(data.clone()),
                bytes: bytes.clone(),
                eof: eof.clone(),
            };
            let flag = AtomicBool::new(false);
            let summary =
                inspect_reader(BufReader::new(source()), 2, 3, cap, captured, &flag).unwrap();
            assert_eq!(summary.patterns.len(), 3);
            assert_eq!(summary.tail.len(), 2);
            let mut file = (!captured).then(source);
            let mut output = DigestOutput::new(cap);
            emit_reader(&mut file, &summary, &mut output, &flag).unwrap();
            let expected = data.len() as u64 + if captured { 0 } else { 5 * 4095 };
            eprintln!(
                "compiled reader: captured={captured}, {} bytes, {} EOF observation(s)",
                bytes.get(),
                eof.get()
            );
            assert_eq!(bytes.get(), expected);
            assert_eq!(eof.get(), 1);
            let output = output.finish();
            assert!(!output.is_empty());
            if cap.is_some() {
                assert!(output.len() <= 512);
            }
        }
    }
    #[test]
    fn metrics_stop_at_controlled_chunk_and_record_boundaries() {
        let flag = AtomicBool::new(false);
        let (mut input, count) = reader(&[b'x'; 128], &flag, 8);
        let error = metric_record(&mut input, &mut Vec::new(), &flag).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(count.get(), 8);
        let flag = AtomicBool::new(false);
        let (mut input, count) = reader(b"one\ntwo\n", &flag, usize::MAX);
        let mut record = Vec::new();
        assert!(metric_record(&mut input, &mut record, &flag).unwrap());
        let before = count.get();
        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            metric_record(&mut input, &mut record, &flag)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(count.get(), before);
        let flag = AtomicBool::new(false);
        let (mut input, count) = reader(b"1234567\r\n", &flag, 9);
        assert_eq!(
            metric_record(&mut input, &mut Vec::new(), &flag)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(count.get(), 9);
    }
    #[test]
    fn log_stops_when_a_controlled_read_sets_cancellation() {
        let flag = AtomicBool::new(false);
        let (input, count) = reader(b"ERROR x\nnext\n", &flag, 8);
        let result = LogReader::new(input, 0, &flag).next();
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::Interrupted));
        assert_eq!(count.get(), 8);
    }
    #[test]
    fn directory_observation_stops_before_consuming_entries() {
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = Remove(
            std::env::temp_dir().join(format!("oms-digest-cancel-dir-{}", std::process::id())),
        );
        fs::create_dir(&root.0).unwrap();
        fs::create_dir(root.0.join("nested")).unwrap();
        fs::write(root.0.join("metric.tsv"), "step\tvalue\n1\t2\n").unwrap();
        let flag = AtomicBool::new(false);
        let mut files = Vec::new();
        let mut observed = 0;
        let result = collect_tsv_with(&root.0, &mut files, &flag, &mut |p| {
            observed += 1;
            let directory = fs::read_dir(p)?;
            flag.store(true, Ordering::Relaxed);
            Ok(directory)
        });
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::Interrupted));
        assert_eq!(observed, 1);
        assert!(files.is_empty());
        flag.store(false, Ordering::Relaxed);
        let ignored = collect_tsv_with(&root.0, &mut files, &flag, &mut |_| {
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert!(ignored.is_ok());
        let stopped = collect_tsv_with(&root.0, &mut files, &flag, &mut |_| {
            flag.store(true, Ordering::Relaxed);
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert!(matches!(stopped, Err(error) if error.kind() == io::ErrorKind::Interrupted));
        let mut paths = ["b.tsv", "a.tsv", "b.tsv"].map(PathBuf::from).to_vec();
        assert_eq!(
            order_metric_files(&mut paths, &flag).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        flag.store(false, Ordering::Relaxed);
        let mut paths = ["b.tsv", "a.tsv", "b.tsv"].map(PathBuf::from).to_vec();
        order_metric_files(&mut paths, &flag).unwrap();
        assert_eq!(paths, ["a.tsv", "b.tsv", "b.tsv"].map(PathBuf::from));
        flag.store(true, Ordering::Relaxed);
        let result = metrics(&root.0, OsStr::new("metrics"), &mut Vec::new(), &flag);
        assert!(matches!(result, Err(error) if error.class == "InterruptedError"));
    }
}
