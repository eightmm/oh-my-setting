use crate::value::Value;
use crate::{MAX_ANSWER, RUN_ROOT, operation};
use chrono::{DateTime, Utc};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use rustix::process::getuid;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub(crate) fn hash(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data))
}
pub(crate) fn now() -> String {
    iso(SystemTime::now())
}
pub(crate) fn iso(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
fn string_repr(s: &crate::value::Text) -> String {
    use unicode_general_category::{GeneralCategory::*, get_general_category};
    let quote = if s.points().any(|n| n == 39) && !s.points().any(|n| n == 34) {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for n in s.points() {
        let Some(c) = char::from_u32(n) else {
            out.push_str(&format!("\\u{n:04x}"));
            continue;
        };
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c != ' '
                && matches!(
                    get_general_category(c),
                    Control
                        | Format
                        | Surrogate
                        | PrivateUse
                        | Unassigned
                        | LineSeparator
                        | ParagraphSeparator
                        | SpaceSeparator
                ) =>
            {
                out.push_str(&if n < 256 {
                    format!("\\x{n:02x}")
                } else if n < 65536 {
                    format!("\\u{n:04x}")
                } else {
                    format!("\\U{n:08x}")
                });
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
pub(crate) fn repr(value: &Value) -> String {
    enum Frame<'a> {
        Node(&'a Value),
        Key(&'a crate::value::Text),
        Literal(&'static str),
    }
    let mut stack = vec![Frame::Node(value)];
    let mut out = String::new();
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Literal(s) => out.push_str(s),
            Frame::Key(s) => out.push_str(&string_repr(s)),
            Frame::Node(value) => match value {
                Value::Null => out.push_str("None"),
                Value::Bool(v) => out.push_str(if *v { "True" } else { "False" }),
                Value::Number(n) => out.push_str(n.repr()),
                Value::String(s) => out.push_str(&string_repr(s)),
                Value::Array(a) => {
                    out.push('[');
                    stack.push(Frame::Literal("]"));
                    for (i, v) in a.iter().enumerate().rev() {
                        stack.push(Frame::Node(v));
                        if i > 0 {
                            stack.push(Frame::Literal(", "));
                        }
                    }
                }
                Value::Object(o) => {
                    out.push('{');
                    stack.push(Frame::Literal("}"));
                    let fields: Vec<_> = o.iter().collect();
                    for (i, (k, v)) in fields.into_iter().enumerate().rev() {
                        stack.push(Frame::Node(v));
                        stack.push(Frame::Literal(": "));
                        stack.push(Frame::Key(k));
                        if i > 0 {
                            stack.push(Frame::Literal(", "));
                        }
                    }
                }
            },
        }
    }
    out
}
pub(crate) fn display(value: &Value) -> crate::value::Text {
    value
        .as_text()
        .cloned()
        .unwrap_or_else(|| repr(value).into())
}
pub(crate) use oms_command_operations::peer_operations::{
    read_repo_file as read, read_run, started,
};

pub(crate) fn meta(dir: &Path) -> Value {
    read_run(dir, "meta.json", 256 * 1024)
        .ok()
        .and_then(|b| crate::value::from_slice(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn rfc3339_utc(s: &str) -> bool {
    (s.ends_with('Z') || s.ends_with("+00:00"))
        && s.as_bytes().get(10) == Some(&b'T')
        && DateTime::parse_from_rfc3339(s).is_ok_and(|dt| dt.offset().local_minus_utc() == 0)
}
pub(crate) fn receipt(dir: &Path, name: &str) -> Option<Value> {
    read_run(dir, &format!("{name}.json"), 4096)
        .ok()
        .and_then(|b| crate::value::from_slice(&b).ok())
        .filter(|v| {
            v.is_object()
                && v["schema"] == 1
                && v["task_id"].as_str() == dir.file_name().and_then(|s| s.to_str())
                && v["kind"] == format!("mcp-task-{name}")
                && (name != "cancel-request"
                    || v.get("requested_at")
                        .and_then(Value::as_str)
                        .is_some_and(rfc3339_utc))
        })
}
pub(crate) fn valid_receipt(dir: &Path, name: &str) -> bool {
    receipt(dir, name).is_some()
}
pub(crate) fn cancel_request(dir: &Path) -> Option<String> {
    receipt(dir, "cancel-request").and_then(|v| {
        v.get("requested_at")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}
pub(crate) fn sections(data: &[u8]) -> (String, String) {
    let mut offset = 0;
    let (mut start, mut completed) = (None, None);
    let mut exit_seen = false;
    let mut code = String::new();
    for raw in data.split_inclusive(|b| *b == b'\n') {
        let end = raw
            .iter()
            .rposition(|b| !matches!(*b, b'\r' | b'\n'))
            .map_or(0, |i| i + 1);
        let line = &raw[..end];
        if exit_seen
            && code.is_empty()
            && !String::from_utf8_lossy(line)
                .trim_matches(crate::whitespace)
                .is_empty()
        {
            code = String::from_utf8_lossy(line)
                .trim_matches(crate::whitespace)
                .into();
        }
        if line == b"## Output" {
            start = Some(offset + raw.len());
        } else if line == b"## Exit" {
            completed = start.map(|s| (s, offset));
            exit_seen = true;
            code.clear();
        }
        offset += raw.len();
    }
    let answer = completed
        .map(|(a, b)| {
            normalize_lines(&String::from_utf8_lossy(&data[a..b]))
                .trim_matches(crate::whitespace)
                .to_owned()
        })
        .unwrap_or_default();
    (answer, code)
}
pub(crate) fn normalize_lines(s: &str) -> String {
    // str::lines omits Unicode line separators handled by Python splitlines.
    let mut lines = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&s[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, c)| *c == '\n') {
                start = iter.next().unwrap().0 + 1;
            }
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines.join("\n")
}
pub(crate) fn owned_answer(repo: &Path, op: &str, name: &str) -> Result<(String, String), String> {
    if !crate::answer_name(name) {
        return Err("error: invalid answer name".into());
    }
    let bytes = read(repo, &format!("{RUN_ROOT}/{op}/answers/{name}"), MAX_ANSWER)?;
    let (answer, code) = sections(&bytes);
    if answer.len() > MAX_ANSWER {
        return Err("error: normalized answer exceeds 8 MiB read limit".into());
    }
    Ok((answer, code))
}
pub(crate) fn reference(op: &str, name: &str, answer: &str) -> String {
    format!("{name}:{}", hash(format!("{op}\0{name}\0{answer}")))
}
pub(crate) fn bounded(answer: &str, limit: usize) -> String {
    if answer.len() <= limit {
        return answer.into();
    }
    let marker = "\n[TRUNCATED: read full answer]\n";
    let room = limit.saturating_sub(marker.len());
    if room == 0 {
        return marker[..limit.min(marker.len())].into();
    }
    let head = (room * 2).div_ceil(3);
    let tail = room - head;
    format!(
        "{}{marker}{}",
        byte_prefix(answer, head),
        byte_suffix(answer, tail)
    )
}
pub(crate) fn byte_prefix(s: &str, mut n: usize) -> &str {
    n = n.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}
fn byte_suffix(s: &str, n: usize) -> &str {
    let mut start = s.len().saturating_sub(n);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

pub(crate) struct Directory {
    pub fd: OwnedFd,
    pub path: PathBuf,
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn err(e: impl std::fmt::Display) -> String {
    format!("error: operation directory: {e}")
}
impl Directory {
    /// Open relative ancestry without following links. Mutation additionally
    /// requires ownership; existing shared directory modes remain supported.
    pub fn open(repo: &Path, relative: &str, create: bool, write: bool) -> Result<Self, String> {
        let mut fd = fs::open(repo, flags(), Mode::empty()).map_err(err)?;
        let mut path = repo.to_owned();
        for part in relative.split('/') {
            if part.is_empty() || matches!(part, "." | "..") || part.contains('\0') {
                return Err("error: invalid operation directory".into());
            }
            if create {
                match fs::mkdirat(&fd, part, Mode::from_raw_mode(0o700)) {
                    Ok(()) => (),
                    Err(e) if e == rustix::io::Errno::EXIST => (),
                    Err(e) => return Err(err(e)),
                }
            }
            let next = fs::openat(&fd, part, flags(), Mode::empty()).map_err(err)?;
            let st = fs::fstat(&next).map_err(err)?;
            let named = fs::statat(&fd, part, AtFlags::SYMLINK_NOFOLLOW).map_err(err)?;
            if st.st_dev != named.st_dev
                || st.st_ino != named.st_ino
                || FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || (write && (st.st_uid != getuid().as_raw()))
            {
                return Err("error: unsafe operation directory".into());
            }
            path.push(part);
            fd = next;
        }
        Ok(Self { fd, path })
    }
    pub fn run(repo: &Path, op: &str, write: bool) -> Result<Self, String> {
        if !operation(op) {
            return Err("error: operation must be an id returned by oms_peer_start".into());
        }
        Self::open(repo, &format!("{RUN_ROOT}/{op}"), false, write)
    }
    pub fn exclusive(&self, name: &str) -> Result<std::fs::File, String> {
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(err)?;
        Ok(fd.into())
    }
    pub fn atomic(&self, name: &str, data: &[u8]) -> Result<(), String> {
        self.publish(name, data, false)
    }
    pub fn publish_new(&self, name: &str, data: &[u8]) -> Result<(), String> {
        self.publish(name, data, true)
    }
    pub fn record_cancel_request(&self, data: &[u8]) -> Result<(), String> {
        if cancel_request(&self.path).is_some() {
            fs::fsync(&self.fd).map_err(err)?;
            return Ok(());
        }
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let name = "cancel-request.json";
        let temp = format!(
            ".{name}.{}.{}.part",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let result = (|| {
            let mut file = self.exclusive(&temp)?;
            file.write_all(data).map_err(err)?;
            file.sync_all().map_err(err)?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let first = fs::renameat_with(
                &self.fd,
                temp.as_str(),
                &self.fd,
                name,
                fs::RenameFlags::NOREPLACE,
            );
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let first = fs::linkat(&self.fd, temp.as_str(), &self.fd, name, AtFlags::empty());
            match first {
                Ok(()) => {
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    let _ = fs::unlinkat(&self.fd, temp.as_str(), AtFlags::empty());
                    fs::fsync(&self.fd).map_err(err)?;
                }
                Err(e) if e == rustix::io::Errno::EXIST => {
                    if cancel_request(&self.path).is_none() {
                        let _guard = self
                            .lock_named("cancel-request.lock", fs::FlockOperation::LockExclusive)?;
                        if cancel_request(&self.path).is_none() {
                            fs::renameat(&self.fd, temp.as_str(), &self.fd, name).map_err(err)?;
                        } else {
                            let _ = fs::unlinkat(&self.fd, temp.as_str(), AtFlags::empty());
                        }
                        fs::fsync(&self.fd).map_err(err)?;
                    } else {
                        let _ = fs::unlinkat(&self.fd, temp.as_str(), AtFlags::empty());
                        fs::fsync(&self.fd).map_err(err)?;
                    }
                }
                Err(e) => return Err(err(e)),
            }
            Ok(())
        })();
        let _ = fs::unlinkat(&self.fd, temp.as_str(), AtFlags::empty());
        result
    }
    fn publish(&self, name: &str, data: &[u8], create_only: bool) -> Result<(), String> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let temp = format!(
            ".{name}.{}.{}.part",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let result = (|| {
            let mut file = self.exclusive(&temp)?;
            file.write_all(data).map_err(err)?;
            file.sync_all().map_err(err)?;
            if create_only {
                match fs::linkat(&self.fd, temp.as_str(), &self.fd, name, AtFlags::empty()) {
                    Ok(()) => (),
                    Err(e) if e == rustix::io::Errno::EXIST => (),
                    Err(e) => return Err(err(e)),
                }
            } else {
                fs::renameat(&self.fd, temp.as_str(), &self.fd, name).map_err(err)?;
            }
            fs::fsync(&self.fd).map_err(err)?;
            Ok(())
        })();
        let _ = fs::unlinkat(&self.fd, temp.as_str(), AtFlags::empty());
        if create_only && result.is_ok() {
            fs::fsync(&self.fd).map_err(err)?;
        }
        result
    }
    pub fn lock(&self) -> Result<OwnedFd, String> {
        self.lock_named("owner.lock", fs::FlockOperation::NonBlockingLockExclusive)
    }
    fn lock_named(&self, name: &str, mode: fs::FlockOperation) -> Result<OwnedFd, String> {
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(err)?;
        let st = fs::fstat(&fd).map_err(err)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
            || st.st_nlink != 1
            || st.st_uid != getuid().as_raw()
            || st.st_mode & 0o077 != 0
        {
            return Err("error: unsafe operation lock".into());
        }
        loop {
            match fs::flock(&fd, mode) {
                Ok(()) => return Ok(fd),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_sections_and_preview_edges() {
        for (source, answer, exit) in [
            ("## Output\npartial", "", ""),
            ("## Exit\n\n3\n", "", "3"),
            ("## Output\r\nanswer\r\n## Exit\r\n\r\n0\r\n", "answer", "0"),
            (
                "## Output\nquoted\n## Exit\n1\n## Output\nreal\n## Exit\n0",
                "real",
                "0",
            ),
            (
                "## Output\nanswer\n## Exit\nquoted\n## Exit\n0",
                "answer\n## Exit\nquoted",
                "0",
            ),
            (
                "## Output\n a\u{85}b\u{2028}c\u{1c}\n## Exit\n0",
                "a\nb\nc",
                "0",
            ),
        ] {
            assert_eq!(sections(source.as_bytes()), (answer.into(), exit.into()));
        }
        for limit in [0, 1, 4, 28, 29, 30, 100] {
            let answer = "근거🙂".repeat(100);
            let out = bounded(&answer, limit);
            assert!(out.len() <= limit);
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        }
        assert_eq!(bounded("", 0), "");
        assert_eq!(bounded("exact", 5), "exact");
        assert!(!crate::answer_name("a\u{301}.md"));
        assert!(crate::answer_name("근거.md"));
    }

    #[test]
    fn cancel_request_validation_fsync_and_first_wins_replacement() {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::{Arc, Barrier};

        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-storage-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());

        let op = "consult-20261126T000000Z-aabbccdd";
        let dir = Directory::open(&root, &format!("{RUN_ROOT}/{op}"), true, true).unwrap();
        dir.atomic("meta.json", b"{}\n").unwrap();

        for valid in [
            "2020-01-01T00:00:05Z",
            "2020-01-01T00:00:05.123456Z",
            "2020-01-01T00:00:05+00:00",
        ] {
            let body = format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"{valid}\"}}\n"
            );
            std::fs::write(dir.path.join("cancel-request.json"), &body).unwrap();
            assert_eq!(cancel_request(&dir.path).as_deref(), Some(valid));
            assert!(valid_receipt(&dir.path, "cancel-request"));
            dir.record_cancel_request(
                format!(
                    "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"2020-01-01T00:00:59Z\"}}\n"
                )
                .as_bytes(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path.join("cancel-request.json")).unwrap(),
                body
            );
        }

        for invalid in [
            "not json".to_owned(),
            format!("{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\"}}"),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":null}}"
            ),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":123}}"
            ),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"\"}}"
            ),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"not-a-timestamp\"}}"
            ),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"2020-01-01 00:00:05Z\"}}"
            ),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"2020-01-01T09:00:05+09:00\"}}"
            ),
            format!(
                "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"2020-01-01T00:00:05-00:00\"}}"
            ),
            "{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"foreign\",\"requested_at\":\"2020-01-01T00:00:05Z\"}".to_owned(),
        ] {
            std::fs::write(dir.path.join("cancel-request.json"), &invalid).unwrap();
            assert_eq!(cancel_request(&dir.path), None, "{invalid}");
            assert!(!valid_receipt(&dir.path, "cancel-request"), "{invalid}");

            let barrier = Arc::new(Barrier::new(8));
            let mut handles = Vec::new();
            for idx in 0..8 {
                let repo = root.clone();
                let gate = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    let handle = Directory::run(&repo, op, true).unwrap();
                    let row = format!(
                        "{{\"schema\":1,\"kind\":\"mcp-task-cancel-request\",\"task_id\":\"{op}\",\"requested_at\":\"2020-01-01T00:00:0{idx}Z\"}}\n"
                    );
                    gate.wait();
                    handle.record_cancel_request(row.as_bytes()).unwrap();
                    std::fs::read(handle.path.join("cancel-request.json")).unwrap()
                }));
            }
            let observed: Vec<Vec<u8>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winner = &observed[0];
            for seen in &observed[1..] {
                assert_eq!(
                    seen, winner,
                    "concurrent invalid replacement must be first-wins"
                );
            }
            assert_eq!(
                &std::fs::read(dir.path.join("cancel-request.json")).unwrap(),
                winner
            );
            assert!(cancel_request(&dir.path).is_some());
            assert_eq!(
                std::fs::read_dir(&dir.path)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
                    .count(),
                0
            );
        }
    }
}
