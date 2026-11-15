use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, safety, storage, text,
};
use std::os::unix::fs::MetadataExt;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MAX_LOG: usize = 16 * 1024 * 1024;
fn path(ident: &str) -> Result<PathBuf> {
    if !storage::identifier(ident) {
        return fail("invalid thread id");
    }
    Ok(Path::new(".oms/threads").join(format!("{ident}.jsonl")))
}
fn active_task(repo: &Path) -> String {
    storage::read(repo, Path::new(".oms/task/current.md"), true)
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .take_while(|l| !l.starts_with("## "))
                .find_map(|l| l.strip_prefix("- task_id:").map(|s| s.trim().to_owned()))
                .unwrap_or_default()
        })
        .unwrap_or_default()
}
pub(crate) fn current(repo: &Path) -> Option<String> {
    let bytes = storage::read(repo, Path::new(".oms/threads/CURRENT"), true).ok()?;
    let line = String::from_utf8_lossy(&bytes);
    let parts: Vec<_> = line.lines().next()?.split_whitespace().collect();
    if parts.len() < 2
        || !storage::identifier(parts[0])
        || parts.len() > 2 && parts[2] != active_task(repo)
    {
        return None;
    }
    let minted = parts[1].parse::<u64>().ok()?;
    let ttl = std::env::var("OMS_THREAD_CURRENT_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(86400);
    if crate::seconds().saturating_sub(minted) > ttl {
        return None;
    }
    storage::read(repo, &path(parts[0]).ok()?, false).ok()?;
    Some(parts[0].into())
}
fn set_current(repo: &Path, ident: &str, context: &Context<'_>) -> Result<()> {
    storage::write(
        repo,
        Path::new(".oms/threads/CURRENT"),
        format!("{ident} {} {}\n", crate::seconds(), active_task(repo)).into_bytes(),
        true,
        context.interrupted,
    )
}
fn load(repo: &Path, ident: &str) -> Result<Vec<V>> {
    let bytes = storage::read(repo, &path(ident)?, false)?;
    if bytes.len() > MAX_LOG {
        return fail("thread exceeds storage budget");
    }
    let rows = storage::rows(&bytes)?;
    if rows.iter().any(|row| text(row, "thread") != ident) {
        return fail("thread id does not match its log");
    }
    Ok(rows)
}
fn room_log(ident: &str, rows: &[V]) -> bool {
    regex::Regex::new(r"(?i)\Aroom-[0-9a-f]{12}\z")
        .unwrap()
        .is_match(ident)
        || rows
            .iter()
            .any(|r| text(r.get("room_event"), "kind") == "created")
}
pub(crate) fn create(repo: &Path, ident: &str, topic: &str, context: &Context<'_>) -> Result<()> {
    safety::outbound(topic.as_bytes())?;
    storage::log(repo, &path(ident)?, context.interrupted, |old, _| {
        let rows = storage::rows(old)?;
        if safety::child() && room_log(ident, &rows) {
            return fail("a worker cannot create a room");
        }
        if !old.is_empty() {
            return Ok((Vec::new(), ()));
        }
        let mut bytes = Vec::new();
        if !topic.is_empty() {
            let row = obj([
                ("schema", 1usize.into()),
                ("thread", ident.into()),
                ("seq", 1usize.into()),
                ("ts", now().into()),
                ("role", "topic".into()),
                ("agent", agent().into()),
                ("text", topic.into()),
            ]);
            bytes = row.json(false)?;
            bytes.push(b'\n');
        }
        Ok((bytes, ()))
    })?;
    set_current(repo, ident, context)
}
fn agent() -> String {
    std::env::var("OMS_AGENT").unwrap_or_else(|_| "agent".into())
}
pub(crate) fn append(
    repo: &Path,
    ident: &str,
    role: &str,
    body: &str,
    fields: &[(&str, V)],
    context: &Context<'_>,
) -> Result<()> {
    if ![
        "question", "answer", "note", "decision", "summary", "topic", "closed",
    ]
    .contains(&role)
    {
        return fail("unknown thread role");
    }
    safety::outbound(body.as_bytes())?;
    storage::read(repo, &path(ident)?, false)?;
    storage::log(repo, &path(ident)?, context.interrupted, |old, meta| {
        let rows = storage::rows(old)?;
        if rows.iter().any(|r| text(r, "thread") != ident) {
            return fail("thread id does not match its log");
        }
        if rows.iter().any(|r| text(r, "role") == "closed") {
            return fail("thread is closed");
        }
        if safety::child() && room_log(ident, &rows) {
            return fail(
                "a worker can only message or acknowledge as its admitted room participant",
            );
        }
        if let Some(last) = rows.last()
            && text(last.get("room_event"), "kind") == "successor"
        {
            return fail(format!(
                "room continues as {}",
                text(last.get("room_event"), "target")
            ));
        }
        if let Some((_, after)) = fields.iter().find(|(k, _)| *k == "after") {
            offset(old, ident, meta, after.text().unwrap_or(""))?;
            if rows
                .iter()
                .any(|row| fields.iter().all(|(key, value)| row.get(key) == value))
            {
                return Ok((Vec::new(), ()));
            }
        }
        let body = turn_body(body);
        let seq = rows
            .iter()
            .filter_map(|r| crate::number(r.get("seq")))
            .fold(0_f64, f64::max) as u64
            + 1;
        let mut row = obj([
            ("schema", 1usize.into()),
            ("thread", ident.into()),
            ("seq", V::Num(seq.to_string())),
            ("ts", now().into()),
            ("role", role.into()),
            ("agent", agent().into()),
            ("text", body.into()),
        ]);
        for (key, value) in fields {
            row.set(key, value.clone());
        }
        let mut bytes = row.json(false)?;
        bytes.push(b'\n');
        if bytes.len() > 65536 || old.len() + bytes.len() > MAX_LOG {
            return fail("thread storage budget exceeded");
        }
        Ok((bytes, ()))
    })
}
pub(crate) fn context(
    repo: &Path,
    ident: &str,
    budget: usize,
    limit: usize,
    notes: bool,
    require_open: bool,
) -> Result<String> {
    let rows = load(repo, ident)?;
    if require_open && rows.iter().any(|r| text(r, "role") == "closed") {
        return fail("thread is closed");
    }
    let rows: Vec<_> = rows
        .iter()
        .filter(|r| {
            text(r, "role") != "closed"
                && text(r, "receipt") != "ack"
                && (!notes
                    || ["note", "decision"].contains(&text(r, "role"))
                        && text(r, "provider").is_empty())
        })
        .collect();
    if rows.is_empty() || limit == 0 {
        return Ok(String::new());
    }
    let live = rows.iter().any(|r| r.get("live").truth());
    let mut kept = Vec::new();
    let mut used = 0;
    let header = |n: usize| {
        format!(
            "## Conversation so far (thread {ident})\n{}\n",
            if n > 0 {
                format!("[{n} earlier turn(s) omitted]\n")
            } else {
                String::new()
            }
        )
    };
    let available = budget.saturating_sub(header(rows.len()).len());
    for row in rows.iter().rev().take(limit) {
        let who = if text(row, "provider").is_empty() {
            text(row, "agent")
        } else {
            text(row, "provider")
        };
        let quality = text(row, "quality");
        let quality = if quality.is_empty() || quality == "ok" {
            String::new()
        } else {
            format!(" [{quality} answer]")
        };
        let prefix = format!(
            "### {} ({who}, {}){quality}\n",
            text(row, "role"),
            text(row, "ts")
        );
        let body = if text(row, "role") == "answer" || live {
            safety::untrusted(who, "peer answer", text(row, "text").as_bytes())?
        } else {
            text(row, "text").into()
        };
        let mut block = format!("{prefix}{body}\n\n");
        if used + block.len() > available {
            if !kept.is_empty() {
                break;
            }
            let marker = "\n[newest turn truncated to the thread context budget]\n\n";
            if available < prefix.len() + marker.len() {
                break;
            }
            block = format!(
                "{prefix}{}{marker}",
                safety::truncate(&body, available - prefix.len() - marker.len())
            );
        }
        used += block.len();
        kept.push(block);
    }
    let mut output = header(rows.len() - kept.len());
    for block in kept.iter().rev() {
        output.push_str(block);
    }
    if output.len() > budget {
        return fail("thread context budget is too small for its omission notice");
    }
    Ok(output)
}
fn anchor(bytes: &[u8], offset: usize) -> String {
    storage::hash(
        &[
            &bytes[..offset.min(256)],
            &bytes[offset.saturating_sub(256)..offset],
        ]
        .concat(),
    )
}
fn cursor(bytes: &[u8], ident: &str, meta: &std::fs::Metadata, at: usize) -> Result<String> {
    let value = V::Arr(vec![
        1usize.into(),
        ident.into(),
        V::Num(meta.dev().to_string()),
        V::Num(meta.ino().to_string()),
        V::Num(at.to_string()),
        anchor(bytes, at).into(),
    ]);
    Ok(encode(&value.json(false)?))
}
fn offset(bytes: &[u8], ident: &str, meta: &std::fs::Metadata, after: &str) -> Result<usize> {
    if after.is_empty() {
        return Ok(0);
    }
    let value = V::parse(&decode(after)?)?;
    let fields = value.array();
    if fields.len() != 6 {
        return fail("invalid thread cursor");
    }
    let at = match &fields[4] {
        V::Num(n) => n.parse::<usize>().ok(),
        _ => None,
    }
    .ok_or_else(|| Error::new(2, "invalid thread cursor offset"))?;
    if fields[0] != 1usize.into()
        || fields[1].text() != Some(ident)
        || fields[2] != V::Num(meta.dev().to_string())
        || fields[3] != V::Num(meta.ino().to_string())
        || at > bytes.len()
        || at > 0 && bytes[at - 1] != b'\n'
        || fields[5].text() != Some(&anchor(bytes, at))
    {
        return fail("stale thread cursor; inspect the thread before restarting delivery");
    }
    Ok(at)
}
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for b in bytes {
        acc = (acc << 8) | u32::from(*b);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(ALPHABET[((acc >> bits) & 63) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (6 - bits)) & 63) as usize] as char);
    }
    out
}
pub(crate) fn decode(raw: &str) -> Result<Vec<u8>> {
    if raw.is_empty() || raw.len() > 1024 {
        return fail("invalid cursor");
    }
    let mut bytes = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for b in raw.bytes() {
        let n = ALPHABET
            .iter()
            .position(|c| *c == b)
            .ok_or_else(|| Error::new(2, "invalid cursor encoding"))?;
        acc = (acc << 6) | n as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
        }
    }
    if raw.len() % 4 == 1 || bits > 0 && acc & ((1 << bits) - 1) != 0 {
        return fail("invalid cursor padding");
    }
    Ok(bytes)
}
fn snapshot(repo: &Path, ident: &str) -> Result<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let label = path(ident)?;
    storage::read(repo, &label, false)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(repo.join(&label))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > MAX_LOG as u64 {
        return fail("unsafe or oversized thread");
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_LOG as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LOG {
        return fail("thread exceeds storage budget");
    }
    Ok((bytes, meta))
}
pub(crate) fn run(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "repo",
            "id",
            "topic",
            "role",
            "text",
            "text-file",
            "provider",
            "model",
            "artifact",
            "quality",
            "summary",
            "max-bytes",
            "turns",
            "after",
            "wait",
            "consumer",
            "room-event",
            "stale-ttl",
        ],
        &[
            "json",
            "all",
            "stale",
            "apply",
            "live",
            "notes-only",
            "require-open",
        ],
    )?;
    if args.has("help") {
        return Ok(out(include_bytes!("help/thread.txt").to_vec()));
    }
    args.exact_operands(1)?;
    if args.has("room-event") && args.action() != "append" {
        return fail("--room-event applies to append");
    }
    if args.has("stale") && !matches!(args.action(), "list" | "close") {
        return fail("--stale applies to list or close");
    }
    if args.has("stale-ttl") && !matches!(args.action(), "list" | "stats" | "close") {
        return fail("--stale-ttl applies to list, stats or close");
    }
    if args.has("stale-ttl") && args.action() == "close" && !args.has("stale") {
        return fail("--stale-ttl on close requires --stale");
    }
    if args.has("apply") && !(args.action() == "close" && args.has("stale")) {
        return fail("--apply requires close --stale");
    }
    if args.action() == "close"
        && args.has("stale")
        && ["id", "all", "json"].iter().any(|key| args.has(key))
    {
        return fail("close --stale cannot combine with --id, --all or --json");
    }
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let ident = if args.has("id") {
        Some(args.get("id").to_owned())
    } else {
        current(&repo)
    };
    match args.action() {
        "current" => {
            return Ok(ident.map_or((1, Vec::new(), Vec::new()), |s| {
                out(format!("{s}\n").into_bytes())
            }));
        }
        "new" => {
            let ident = ident
                .filter(|_| args.has("id"))
                .unwrap_or(crate::id("th-")?);
            create(
                &repo,
                &ident,
                args.value(
                    "topic",
                    if args.has("live") {
                        "Live collaboration"
                    } else {
                        ""
                    },
                ),
                context,
            )?;
            if args.has("live") {
                append(
                    &repo,
                    &ident,
                    "note",
                    "Live collaboration",
                    &[("live", true.into())],
                    context,
                )?;
            }
            return Ok(out(format!("{ident}\n").into_bytes()));
        }
        "list" | "stats" => return listing(&repo, &args),
        "close" if args.has("stale") => return close_stale(&repo, &args, context),
        _ => {}
    }
    let ident =
        ident.ok_or_else(|| Error::new(2, "no current thread; create one with thread new"))?;
    match args.action() {
        "context" => Ok(out(self::context(
            &repo,
            &ident,
            args.limit("max-bytes", 6000, 0, 65536)?,
            args.limit("turns", 12, 0, 200)?,
            args.has("notes-only"),
            args.has("require-open"),
        )?
        .into_bytes())),
        "show" => {
            let rows = load(&repo, &ident)?;
            Ok(out(if args.has("json") {
                json(&V::Arr(rows))?
            } else {
                rows.iter()
                    .map(|r| {
                        format!(
                            "### {} ({}, {})\n{}\n\n",
                            text(r, "role"),
                            text(r, "agent"),
                            text(r, "ts"),
                            text(r, "text")
                        )
                    })
                    .collect::<String>()
                    .into_bytes()
            }))
        }
        "append" => {
            let body = if args.has("text-file") {
                String::from_utf8_lossy(&storage::file(&args.path("text-file", context.cwd))?)
                    .into_owned()
            } else {
                args.required("text")?.into()
            };
            if args.has("room-event") {
                let event =
                    oms_command_room::value::parse(args.get("room-event").as_bytes(), 65536)
                        .map_err(|e| Error::new(2, e.to_string()))?;
                let room = oms_command_room::Room::open(
                    &repo,
                    oms_command_room::Session::from_env(),
                    &oms_command_room::LocalServices,
                    context.interrupted,
                    false,
                    false,
                )
                .map_err(|e| Error::new(2, e.to_string()))?;
                room.append(&ident, &event, &body)
                    .map_err(|e| Error::new(2, e.to_string()))?;
                return Ok(out(Vec::new()));
            }
            let mut fields = Vec::new();
            for key in ["provider", "model", "artifact", "quality"] {
                if args.has(key) {
                    fields.push((key, args.get(key).into()));
                }
            }
            if args.has("quality")
                && !["ok", "thin", "empty", "blocked", "truncated"].contains(&args.get("quality"))
            {
                return fail("invalid answer quality");
            }
            append(
                &repo,
                &ident,
                args.required("role")?,
                &body,
                &fields,
                context,
            )?;
            Ok((
                0,
                Vec::new(),
                format!("thread: appended {} to {ident}\n", args.get("role")).into_bytes(),
            ))
        }
        "close" => {
            append(&repo, &ident, "closed", args.get("summary"), &[], context)?;
            Ok(out(format!("thread: closed {ident}\n").into_bytes()))
        }
        "updates" => {
            let budget = args.limit("max-bytes", 6000, 1, 65536)?;
            let limit = args.limit("turns", 12, 1, 200)?;
            let wait = args.limit("wait", 0, 0, 30)?;
            let deadline = Instant::now() + Duration::from_secs(wait as u64);
            let mut after = args.get("after").to_owned();
            loop {
                crate::check(context.interrupted)?;
                let (bytes, meta) = snapshot(&repo, &ident)?;
                let mut at = offset(&bytes, &ident, &meta, &after)?;
                let mut selected = Vec::new();
                let mut used = 0;
                for line in bytes[at..].split_inclusive(|b| *b == b'\n').take(limit) {
                    if !line.ends_with(b"\n") {
                        break;
                    }
                    if line.len() > 65536 {
                        return fail("thread row exceeds 65536 bytes");
                    }
                    if used + line.len() > budget {
                        if selected.is_empty() {
                            return fail(
                                "next complete turn exceeds byte budget; increase --max-bytes",
                            );
                        }
                        break;
                    }
                    let row = V::parse(line)?;
                    if text(&row, "thread") != ident {
                        return fail("invalid thread row");
                    }
                    selected.push(row);
                    at += line.len();
                    used += line.len();
                }
                let next = cursor(&bytes, &ident, &meta, at)?;
                if !selected.is_empty() || Instant::now() >= deadline {
                    return Ok(out(json(&obj([
                        ("schema", 1usize.into()),
                        ("thread", ident.clone().into()),
                        ("turns", V::Arr(selected)),
                        ("cursor", next.into()),
                        ("has_more", (at < bytes.len()).into()),
                    ]))?));
                }
                after = next;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        "ack" => {
            let consumer = args.required("consumer")?;
            if !storage::identifier(consumer) {
                return fail("invalid consumer");
            }
            let after = args.required("after")?;
            append(
                &repo,
                &ident,
                "note",
                "",
                &[
                    ("receipt", "ack".into()),
                    ("consumer", consumer.into()),
                    ("after", after.into()),
                ],
                context,
            )?;
            Ok(out(Vec::new()))
        }
        _ => fail("unknown thread subcommand"),
    }
}

fn stale_ttl(args: &Args) -> Result<i64> {
    if args.has("stale-ttl") {
        return args
            .get("stale-ttl")
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| Error::new(2, "--stale-ttl must be a nonnegative integer"));
    }
    let raw = std::env::var("OMS_THREAD_STALE_TTL").unwrap_or_else(|_| "259200".into());
    raw.parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| Error::new(2, "OMS_THREAD_STALE_TTL must be a nonnegative integer"))
}
fn last_epoch(rows: &[V]) -> Option<i64> {
    rows.iter().rev().find_map(|row| {
        chrono::NaiveDateTime::parse_from_str(text(row, "ts"), "%Y-%m-%dT%H:%M:%SZ")
            .ok()
            .map(|stamp| stamp.and_utc().timestamp())
    })
}
fn closed(rows: &[V]) -> bool {
    rows.iter().any(|row| {
        text(row, "role") == "closed"
            || text(row.get("room_event"), "kind") == "successor"
                && storage::identifier(text(row.get("room_event"), "target"))
    })
}
fn classification(ident: &str, rows: &[V], current: &str, now: i64, ttl: i64) -> &'static str {
    if closed(rows) {
        "closed"
    } else if ident == current {
        "current"
    } else {
        match last_epoch(rows) {
            None => "unknown",
            Some(stamp) if now.saturating_sub(stamp) > ttl => "stale",
            _ => "fresh",
        }
    }
}
fn human(age: Option<i64>) -> String {
    match age {
        None => "?".into(),
        Some(n) if n >= 86400 => format!("{}d", n / 86400),
        Some(n) if n >= 3600 => format!("{}h", n / 3600),
        Some(n) => format!("{}m", n.div_euclid(60)),
    }
}
fn views(repo: &Path, args: &Args) -> Result<(Vec<V>, V, i64)> {
    let ttl = stale_ttl(args)?;
    let current = current(repo).unwrap_or_default();
    let now = crate::seconds() as i64;
    let keys = [
        "threads",
        "open",
        "closed",
        "stale_open",
        "undatable_open",
        "second_question",
        "multi_provider",
        "answers_rated",
        "answers_ok",
    ];
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    let mut entries = Vec::new();
    for name in storage::names(repo, Path::new(".oms/threads"), ".jsonl")? {
        let ident = name.trim_end_matches(".jsonl");
        let rows = load(repo, ident)?;
        let state = classification(ident, &rows, &current, now, ttl);
        if state == "closed" && !args.has("all") && args.action() != "stats" {
            continue;
        }
        let mut providers = Vec::new();
        let mut questions = 0;
        for row in &rows {
            let provider = text(row, "provider");
            if !provider.is_empty() && !providers.contains(&provider) {
                providers.push(provider);
            }
            if text(row, "role") == "question" {
                questions += 1;
            }
            if text(row, "role") == "answer" && row.get("quality").truth() {
                *counts.entry("answers_rated").or_default() += 1;
                if text(row, "quality") == "ok" {
                    *counts.entry("answers_ok").or_default() += 1;
                }
            }
        }
        for key in [
            Some("threads"),
            Some(if state == "closed" { "closed" } else { "open" }),
            (state == "stale").then_some("stale_open"),
            (state == "unknown").then_some("undatable_open"),
            (questions >= 2).then_some("second_question"),
            (providers.len() >= 2).then_some("multi_provider"),
        ]
        .into_iter()
        .flatten()
        {
            *counts.entry(key).or_default() += 1;
        }
        let topic = rows
            .iter()
            .find(|r| text(r, "role") == "topic")
            .or_else(|| rows.iter().find(|r| text(r, "role") == "question"))
            .map(|r| {
                text(r, "text")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect::<String>();
        let age = last_epoch(&rows).map(|stamp| now.saturating_sub(stamp));
        let mut entry = obj([
            ("id", ident.into()),
            ("turns", rows.len().into()),
            ("closed", (state == "closed").into()),
            ("current", (ident == current).into()),
            ("topic", topic.into()),
            (
                "providers",
                V::Arr(providers.into_iter().map(V::from).collect()),
            ),
            (
                "last_ts",
                rows.last().map_or(V::Null, |r| r.get("ts").clone()),
            ),
            ("staleness", state.into()),
            (
                "age_seconds",
                age.map_or(V::Null, |n| V::Num(n.to_string())),
            ),
        ]);
        if state == "stale" {
            entry.set(
                "close_command",
                format!("oms thread close --id {ident} --summary \"stale\""),
            );
        }
        if !args.has("stale") || state == "stale" {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        text(b, "last_ts")
            .cmp(text(a, "last_ts"))
            .then_with(|| text(a, "id").cmp(text(b, "id")))
    });
    let stats = V::Obj(
        keys.into_iter()
            .map(|key| {
                (
                    key.as_bytes().to_vec(),
                    counts.get(key).copied().unwrap_or(0).into(),
                )
            })
            .collect(),
    );
    Ok((entries, stats, ttl))
}
fn listing(repo: &Path, args: &Args) -> Result<Output> {
    let (entries, stats, ttl) = views(repo, args)?;
    if args.action() == "stats" {
        if args.has("json") {
            return Ok(out(json(&obj([
                ("schema", 1usize.into()),
                ("stale_ttl_seconds", V::Num(ttl.to_string())),
                ("stats", stats),
            ]))?));
        }
        let count = |key| crate::number(stats.get(key)).unwrap_or(0.0) as usize;
        return Ok(out(format!("# thread stats\n  threads: {} (open {}, closed {})\n  stale open: {} (idle over {}; oms thread list --stale)\n  undatable open: {}\n  with a follow-up question: {}\n  with multiple providers: {}\n  rated answers: {} (quality=ok {})\n",
            count("threads"), count("open"), count("closed"), count("stale_open"), human(Some(ttl)),
            count("undatable_open"), count("second_question"), count("multi_provider"),
            count("answers_rated"), count("answers_ok")).into_bytes()));
    }
    if args.has("json") {
        return Ok(out(json(&obj([
            ("schema", 1usize.into()),
            ("threads", V::Arr(entries)),
        ]))?));
    }
    let mut output = String::new();
    if entries.is_empty() {
        output.push_str(&if args.has("stale") {
            format!(
                "no stale open threads (none idle longer than {})\n",
                human(Some(ttl))
            )
        } else {
            "no threads\n".into()
        });
    } else if args.has("stale") {
        output.push_str(&format!(
            "{} stale open thread(s), idle over {}. Nothing is closed automatically:\n",
            entries.len(),
            human(Some(ttl))
        ));
    }
    for entry in entries {
        if args.has("stale") {
            output.push_str(&stale_line(&entry));
            output.push_str(&format!("  close: {}\n", text(&entry, "close_command")));
            continue;
        }
        let providers = entry
            .get("providers")
            .array()
            .iter()
            .filter_map(V::text)
            .collect::<Vec<_>>()
            .join(",");
        output.push_str(&format!(
            "{:<28} {} turns={:<3} {}{}{}\n",
            text(&entry, "id"),
            if entry.get("closed").truth() {
                "closed"
            } else {
                "open  "
            },
            crate::number(entry.get("turns")).unwrap_or(0.0) as usize,
            if providers.is_empty() {
                "-"
            } else {
                &providers
            },
            if entry.get("current").truth() {
                "  *current"
            } else {
                ""
            },
            if text(&entry, "topic").is_empty() {
                String::new()
            } else {
                format!("  {}", text(&entry, "topic"))
            }
        ));
        if args.has("stale") {
            output.push_str(&format!("  close: {}\n", text(&entry, "close_command")));
        }
    }
    Ok(out(output.into_bytes()))
}
fn close_stale(repo: &Path, args: &Args, context: &Context<'_>) -> Result<Output> {
    safety::parent()?;
    let (entries, _, ttl) = views(repo, args)?;
    if !args.has("apply") {
        let mut output = format!(
            "Dry run: {} stale open thread(s); re-run with --apply to close eligible non-room threads.\n",
            entries.len()
        );
        for row in entries {
            output.push_str(&stale_line(&row));
        }
        return Ok(out(output.into_bytes()));
    }
    let summary = args.value("summary", "stale");
    safety::outbound(summary.as_bytes())?;
    let mut output = String::new();
    for entry in entries {
        let ident = text(&entry, "id");
        if ident.starts_with("room-") {
            continue;
        }
        let did_close = storage::log(repo, &path(ident)?, context.interrupted, |old, _| {
            let rows = storage::rows(old)?;
            let now = crate::seconds() as i64;
            if rows.iter().any(|r| r.has("room_event"))
                || classification(ident, &rows, &current(repo).unwrap_or_default(), now, ttl)
                    != "stale"
                || rows.iter().any(|r| {
                    last_epoch(std::slice::from_ref(r))
                        .is_some_and(|stamp| now.saturating_sub(stamp) <= ttl)
                })
            {
                return Ok((Vec::new(), false));
            }
            if rows.iter().any(|r| text(r, "thread") != ident) {
                return fail("thread id does not match its log");
            }
            let seq = rows
                .iter()
                .filter_map(|r| crate::number(r.get("seq")))
                .fold(0.0, f64::max) as u64
                + 1;
            let row = obj([
                ("schema", 1usize.into()),
                ("ts", crate::now().into()),
                ("thread", ident.into()),
                ("seq", V::Num(seq.to_string())),
                ("role", "closed".into()),
                ("agent", agent().into()),
                ("text", turn_body(summary).into()),
            ]);
            let mut bytes = row.json(false)?;
            bytes.push(b'\n');
            if bytes.len() > 65536 || old.len() + bytes.len() > MAX_LOG {
                return fail("thread storage budget exceeded");
            }
            Ok((bytes, true))
        })?;
        if did_close {
            output.push_str(&format!("thread: closed {ident}\n"));
        }
    }
    Ok((0, Vec::new(), output.into_bytes()))
}

fn turn_body(body: &str) -> String {
    let maximum = std::env::var("OMS_THREAD_TURN_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4000)
        .clamp(1, 32768);
    let trimmed = body.trim_end_matches(['\r', '\n']);
    if trimmed.len() > maximum {
        let mut start = trimmed.len() - maximum;
        while !trimmed.is_char_boundary(start) {
            start += 1;
        }
        format!(
            "[earlier output truncated: see artifact]\n{}",
            &trimmed[start..]
        )
    } else {
        trimmed.into()
    }
}
fn stale_line(row: &V) -> String {
    let providers = row
        .get("providers")
        .array()
        .iter()
        .filter_map(V::text)
        .collect::<Vec<_>>()
        .join(",");
    let age = if let V::Num(value) = row.get("age_seconds") {
        value.parse::<i64>().ok()
    } else {
        None
    };
    format!(
        "{:<28} idle={:<5} turns={:<3} {}{}\n",
        text(row, "id"),
        human(age),
        row.get("turns").py(),
        if providers.is_empty() {
            "-"
        } else {
            &providers
        },
        if text(row, "topic").is_empty() {
            String::new()
        } else {
            format!("  {}", text(row, "topic"))
        }
    )
}
