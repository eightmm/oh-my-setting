//! Internal adapters consumed by runtime envelope/context, goal-drive and peers.
//! These are projections of canonical records, never mutation or acceptance
//! authority. Callers supply bytes from their fresh safe read.
use crate::{
    Error, Result, hash,
    json::{copy, field, object, string, text},
};
use oms_context::Json;
use oms_contract_markdown as md;
use std::{
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::Path,
};

pub fn task_packet(
    repo: &Path,
    path: &Path,
    raw: Option<&[u8]>,
    status: Option<&Json>,
) -> Result<Json> {
    let empty_scope = || {
        object([
            ("allowed", Json::Array(Vec::new())),
            ("forbidden", Json::Array(Vec::new())),
        ])
    };
    let Some(raw) = raw else {
        return Ok(object([
            ("present", Json::Bool(false)),
            ("source", Json::Null),
            ("criteria", Json::Array(Vec::new())),
            ("scope", empty_scope()),
        ]));
    };
    let status = status.ok_or_else(|| {
        Error::new(
            1,
            "canonical task status projection is unavailable or invalid",
        )
    })?;
    oms_work_inspect::task_status_snapshot(&crate::json::encode(status))
        .map_err(|e| Error::new(1, e.to_string()))?;
    let decoded = String::from_utf8_lossy(raw);
    let (body, metadata) = md::sections(&decoded);
    let goal = md::first_nonempty(
        &md::section_text(&body, &["goal", "objective", "목표"]),
        1000,
    );
    let constraints = md::section_text(&body, &["constraint", "제약"]);
    let criteria = md::section_text(
        &body,
        &["done criteria", "acceptance", "completion", "완료", "성공"],
    );
    let verify = md::section_text_exact(&body, &["verify", "검증 명령"]);
    let loop_text = md::section_text_exact(&body, &["loop state", "budget", "예산"]);
    let mut constraint_lines = md::bullet_items(&constraints, false);
    if constraint_lines.is_empty() {
        constraint_lines = constraints.lines().map(str::to_owned).collect();
    }
    let scope = md::parse_scope(constraint_lines.iter().map(String::as_str));
    let re = regex::Regex::new(r"^\s*[-*]?\s*([A-Za-z][A-Za-z0-9_.-]*):\s*(.+?)\s*$").unwrap();
    let mut budget = Vec::new();
    for line in loop_text.lines() {
        if let Some(c) = re.captures(line) {
            let v = &c[2];
            let value = if let Some(normalized) = crate::observe::decimal(v) {
                Json::Number(normalized)
            } else {
                string(md::bounded_line(v, 160))
            };
            if let Some(i) = budget
                .iter()
                .position(|(k, _): &(Vec<u8>, Json)| k == c[1].as_bytes())
            {
                budget[i].1 = value;
            } else {
                budget.push((c[1].as_bytes().to_vec(), value));
            }
        }
    }
    let verification = text(field(status, "verification"))
        .filter(|v| [b"fresh".as_slice(), b"stale", b"none"].contains(v))
        .unwrap_or(b"unknown");
    let relative = path.strip_prefix(repo).ok();
    let source = relative.map_or(Json::Null, |rel| {
        use std::os::unix::ffi::OsStrExt;
        object([
            ("path", string(rel.as_os_str().as_bytes())),
            ("bytes", Json::Number(raw.len().to_string())),
            ("sha256", string(hash(raw))),
        ])
    });
    let md_meta = object(metadata.iter().map(|(k, v)| (k.as_str(), string(v))));
    let state = text(field(status, "status")).unwrap_or_else(|| {
        metadata
            .get("status")
            .map(String::as_bytes)
            .unwrap_or(b"active")
    });
    let task_id = text(field(status, "task_id")).unwrap_or_else(|| {
        metadata
            .get("task_id")
            .map(String::as_bytes)
            .unwrap_or_default()
    });
    fn array(items: Vec<String>) -> Json {
        Json::Array(items.into_iter().map(string).collect())
    }
    Ok(object([
        ("present", Json::Bool(true)),
        ("source", source),
        ("metadata", md_meta),
        (
            "task_id",
            string(md::bounded_line(&String::from_utf8_lossy(task_id), 160)),
        ),
        (
            "status",
            string(md::bounded_line(&String::from_utf8_lossy(state), 40).to_lowercase()),
        ),
        ("goal", string(goal)),
        (
            "constraints",
            array(
                constraint_lines
                    .iter()
                    .map(|v| md::normalized_line(v))
                    .filter(|v| !v.is_empty())
                    .collect(),
            ),
        ),
        ("criteria", array(md::bullet_items(&criteria, true))),
        ("verify_present", Json::Bool(!verify.trim().is_empty())),
        (
            "verify_digest",
            string(if verify.trim().is_empty() {
                String::new()
            } else {
                hash(verify.trim().as_bytes())
            }),
        ),
        ("verification", string(verification)),
        (
            "state",
            string(md::first_nonempty(
                &md::section_text_exact(&body, &["current state", "현재 상태"]),
                1000,
            )),
        ),
        (
            "next",
            string(md::first_nonempty(
                &md::section_text_exact(&body, &["next step", "다음 단계"]),
                1000,
            )),
        ),
        (
            "scope",
            object([
                ("allowed", array(scope.allowed)),
                ("forbidden", array(scope.forbidden)),
            ]),
        ),
        ("budget", Json::Object(budget)),
        ("stale", copy(field(status, "stale"))),
    ]))
}

/// Safe bounded progress tail with the same 8 MiB cutoff and first-partial-line
/// discard as plan_retro.read_progress. Malformed/non-object rows are ignored.
pub fn progress_tail(path: &Path) -> Result<Vec<Json>> {
    use rustix::fs::{self, AtFlags, Mode, OFlags};
    use std::path::Component;
    const CAP: u64 = 8 * 1024 * 1024;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir =
        fs::open("/", flags, Mode::empty()).map_err(|e| Error::io("open progress root", e))?;
    for part in path
        .parent()
        .ok_or_else(|| Error::new(1, "progress path has no parent"))?
        .components()
    {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                dir = match fs::openat(&dir, name, flags, Mode::empty()) {
                    Ok(fd) => fd,
                    Err(rustix::io::Errno::NOENT) => return Ok(Vec::new()),
                    Err(e) => return Err(Error::io("open progress ancestry", e)),
                };
            }
            _ => return Err(Error::new(1, "unsafe progress path")),
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(1, "progress path has no leaf"))?;
    let fd = match fs::openat(
        &dir,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(Vec::new()),
        Err(e) => return Err(Error::io("open progress record", e)),
    };
    let before = fs::fstat(&fd).map_err(|e| Error::io("inspect progress record", e))?;
    if before.st_nlink != 1
        || before.st_uid != rustix::process::getuid().as_raw()
        || fs::FileType::from_raw_mode(before.st_mode) != fs::FileType::RegularFile
    {
        return Err(Error::new(1, "unsafe progress record"));
    }
    let mut file = File::from(fd);
    let size = u64::try_from(before.st_size).map_err(|_| Error::new(1, "invalid progress size"))?;
    if size > CAP {
        file.seek(SeekFrom::Start(size - CAP))
            .map_err(|e| Error::io("seek progress tail", e))?;
    }
    let mut reader = BufReader::new(file);
    let mut row = Vec::new();
    if size > CAP {
        reader
            .read_until(b'\n', &mut row)
            .map_err(|e| Error::io("skip partial progress row", e))?;
    }
    let mut rows = Vec::new();
    let mut consumed = 0;
    loop {
        row.clear();
        let read = reader
            .read_until(b'\n', &mut row)
            .map_err(|e| Error::io("read progress tail", e))?;
        if read == 0 {
            break;
        }
        consumed += read;
        if consumed > CAP as usize {
            return Err(Error::new(1, "progress changed beyond read bound"));
        }
        if let Ok(value) = oms_context::parse_experiment_json(&row, CAP as usize)
            && matches!(value, Json::Object(_))
        {
            rows.push(value);
        }
    }
    let after = fs::fstat(reader.get_ref()).map_err(|e| Error::io("inspect progress record", e))?;
    let named = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| Error::io("inspect progress name", e))?;
    let shape = |s: &rustix::fs::Stat| {
        (
            s.st_dev,
            s.st_ino,
            s.st_nlink,
            s.st_uid,
            s.st_mode,
            s.st_size,
            s.st_mtime,
            s.st_mtime_nsec,
            s.st_ctime,
            s.st_ctime_nsec,
        )
    };
    if shape(&before) != shape(&after) || shape(&after) != shape(&named) {
        return Err(Error::new(1, "progress changed during read"));
    }
    Ok(rows)
}
