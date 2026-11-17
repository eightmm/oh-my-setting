use crate::{
    cli::{Options, parent_only},
    store::{self, Store},
    workflows::*,
    *,
};
use serde_json::json;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn auto(e: &records::Entry) -> bool {
    ["task_close", "distill", "imported"].contains(&e.kind.as_str())
        || (e.agent == "journal" && e.body.trim_start().starts_with("journal-distill:"))
}
fn origin(e: &records::Entry) -> &str {
    match e.kind.as_str() {
        "task_close" => "task-close",
        "imported" => "imported",
        _ => "distill",
    }
}
fn decided(s: &Store) -> Result<HashSet<String>> {
    Ok(s.rows("reviews.jsonl")?
        .iter()
        .filter(|r| ["accepted", "rejected"].contains(&text(r, "decision").as_str()))
        .map(|r| text(r, "id"))
        .collect())
}
pub(crate) fn context_filter(s: &Store) -> Result<impl Fn(&records::Entry) -> bool> {
    let mut accepted = HashSet::new();
    for row in s.rows("reviews.jsonl")? {
        match text(&row, "decision").as_str() {
            "accepted" => {
                accepted.insert(text(&row, "id"));
            }
            "rejected" => {
                accepted.remove(&text(&row, "id"));
            }
            _ => {}
        }
    }
    Ok(move |e: &records::Entry| {
        let correction = e.field("accepted_correction");
        if !correction.is_empty() {
            // The correction note can survive a crash before its decision receipt.
            accepted.contains(correction)
        } else {
            // Explicitly authored and legacy manual notes need no review migration.
            !auto(e) || accepted.contains(&e.event_id)
        }
    })
}
fn pending(s: &Store) -> Result<Vec<Value>> {
    let decisions = decided(s)?;
    let mut items: Vec<_> = s.active()?.iter().filter(|e| auto(e) && !decisions.contains(&e.event_id)).map(|e| json!({"id":e.event_id,"origin":origin(e),"recorded":e.occurred_at,"agent":e.agent,"text":preview(&e.body,160)})).collect();
    let mut seen = HashSet::new();
    for row in s.rows("pending.jsonl")? {
        let id = text(&row, "id");
        if decisions.contains(&id) || !row["text"].is_string() || !seen.insert(id.clone()) {
            continue;
        }
        items.push(json!({"id":id,"origin":"correction","recorded":text(&row,"ts"),"agent":"","text":preview(&text(&row,"text"),160),"_full":row["text"]}));
    }
    Ok(items)
}
pub(crate) fn review(s: &Store, o: &Options, author: &str, flag: &AtomicBool) -> Result<Output> {
    if ["--from", "--select"]
        .iter()
        .any(|k| !o.value(k).is_empty())
        || ["--apply", "--dry-run", "--all"].iter().any(|k| o.has(k))
    {
        return err("--from/--apply/--dry-run/--select/--all apply only to import");
    }
    let actions: Vec<_> = ["--accept", "--retire", "--reject"]
        .into_iter()
        .filter(|k| !o.value(k).is_empty())
        .collect();
    if actions.len() > 1 {
        return err("review takes one of --accept, --retire or --reject");
    }
    let mut items = pending(s)?;
    if !o.kinds.is_empty() {
        if o.kinds
            .iter()
            .any(|k| !["task-close", "distill", "imported", "correction"].contains(&k.as_str()))
        {
            return err("--kind takes task-close, distill, imported or correction");
        }
        items.retain(|r| o.kinds.contains(&text(r, "origin")));
    }
    let reason = o.value("--reason");
    if actions.is_empty() {
        if !reason.is_empty() {
            return err("--reason applies only with --retire or --reject");
        }
        if o.has("--json") {
            for r in &mut items {
                r.as_object_mut().unwrap().remove("_full");
            }
            return Ok((
                0,
                json_line(&json!({"schema":1,"kind":"oms-memory-review","pending":items}))?,
                Vec::new(),
            ));
        }
        if items.is_empty() {
            return Ok(output("memory review: nothing waiting"));
        }
        let mut out = format!(
            "memory review: {} waiting (unreviewed; listed, not verified)\n",
            items.len()
        );
        for r in items {
            out.push_str(&format!(
                "{:<10} {}  {}\n    {}\n",
                text(&r, "origin"),
                text(&r, "recorded"),
                text(&r, "id"),
                text(&r, "text")
            ));
        }
        out.push_str("accept: oms agent-memory review --accept ID    turn down: --retire ID --reason TEXT (a note) / --reject ID (a correction)\n");
        return Ok((0, out.into_bytes(), Vec::new()));
    }
    parent_only("decide a memory review")?;
    let action = actions[0];
    let id = o.value(action);
    if !review_id(&id) {
        return err("invalid memory ID");
    }
    // Serialize eligibility, the optional note append, and its decision receipt.
    let receipt_path = s.shared.parent().unwrap().join("reviews.jsonl");
    let _decision = store::lock(&receipt_path, flag)?;
    let item = pending(s)?.into_iter().find(|i| i["id"] == id).ok_or_else(|| Error(2,format!("{id} is not waiting for review (unknown, already decided, superseded or retired)")))?;
    let correction = item["origin"] == "correction";
    if action == "--retire" && correction {
        return err(format!(
            "{id} is a captured correction, not a note; use --reject"
        ));
    }
    if action == "--reject" && !correction {
        return err(format!(
            "{id} is a recorded note; turn it down with --retire ID --reason TEXT"
        ));
    }
    if action == "--retire" && reason.is_empty() {
        return err("--retire requires --reason");
    }
    if action == "--accept" && !reason.is_empty() {
        return err("--reason applies only with --retire or --reject");
    }
    if action == "--retire" {
        s.append(
            author,
            &format!("{reason}\n"),
            "retirement",
            None,
            "",
            Some((&id, "retires")),
            flag,
        )?;
        return Ok(os::line(&[
            b"memory: retire recorded in ",
            s.shared.as_os_str().as_encoded_bytes(),
        ]));
    }
    let mut receipt = json!({"schema":1,"ts":now(),"id":id,"agent":author,"origin":item["origin"],"decision":if action=="--accept"{"accepted"}else{"rejected"}});
    if action == "--accept" && correction {
        // A retry after a crash before the receipt must not copy a lesson twice.
        if !s
            .active()?
            .iter()
            .any(|e| e.field("accepted_correction") == id)
        {
            s.append_extra(
                author,
                &format!("{}\n", text(&item, "_full")),
                "lesson",
                &[("accepted_correction", &id)],
                flag,
            )
            .map_err(|_| {
                Error(
                    3,
                    "the memory writer refused the correction; nothing recorded".into(),
                )
            })?;
        }
    }
    if action == "--reject" && !reason.is_empty() {
        if !store::clean(&reason, true) {
            return Err(Error(
                3,
                "--reason contains sensitive-looking content; nothing recorded".into(),
            ));
        }
        receipt["reason"] = clip(&flat(&reason), 200).into();
    }
    let bytes = json_line(&receipt)?;
    store::mutate(&receipt_path, flag, |old| {
        let mut out = old.to_vec();
        out.extend(&bytes);
        Ok(out)
    })?;
    if let Err(Error(_, message)) = s.compact(flag) {
        s.warning(&format!("warning: {message}"));
    }
    Ok(output(if action == "--accept" {
        format!(
            "memory: accepted {id}{}",
            if correction {
                " and recorded it as a note"
            } else {
                ""
            }
        )
    } else {
        format!("memory: rejected {id}; it will not be offered again")
    }))
}
fn label_os(s: &Store, path: &Path) -> std::ffi::OsString {
    path.strip_prefix(&s.repo)
        .unwrap_or_else(|_| Path::new(path.file_name().unwrap_or_default()))
        .as_os_str()
        .to_owned()
}
fn label(s: &Store, path: &Path) -> String {
    os::diagnostic(&label_os(s, path))
}
fn source_files(s: &Store, raw_os: &std::ffi::OsStr, cwd: &Path) -> Result<(Vec<PathBuf>, usize)> {
    let raw = os::diagnostic(raw_os);
    if ["claude", "codex"].contains(&raw.as_str()) {
        return err(format!(
            "import --from {raw} is not supported: where a provider keeps its own memory is not something OMS verifies; pass the file or directory itself with --from PATH"
        ));
    }
    let base = if raw == "serena" {
        s.repo.join(".serena/memories")
    } else {
        let bytes = raw_os.as_encoded_bytes();
        let raw = bytes
            .strip_prefix(b"~/")
            .map(|p| store::home().join(os::from_bytes(p.to_vec())))
            .unwrap_or_else(|| PathBuf::from(raw_os));
        store::absolute(cwd, &raw)
    };
    let base = std::fs::canonicalize(&base).map_err(|_| {
        Error(
            2,
            if raw == "serena" {
                format!(
                    "no .serena/memories directory in {}",
                    os::diagnostic(s.repo.as_os_str())
                )
            } else {
                format!("--from {raw}: no such file or directory")
            },
        )
    })?;
    if base.starts_with(s.shared.parent().unwrap()) {
        return err("--from points at OMS memory itself; there is nothing to import");
    }
    if base.is_file() {
        return Ok((vec![base], 0));
    }
    fn walk(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        let mut entries = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        let mut dirs = Vec::new();
        for e in entries {
            let t = e.file_type()?;
            let name = e.file_name();
            let name = name.as_encoded_bytes();
            if t.is_dir() && !name.starts_with(b".") {
                dirs.push(e.path());
            } else if t.is_file()
                && [".md", ".markdown", ".txt"]
                    .iter()
                    .any(|ext| name.to_ascii_lowercase().ends_with(ext.as_bytes()))
            {
                files.push(e.path());
            }
        }
        for dir in dirs {
            walk(&dir, files)?;
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(&base, &mut files)?;
    let extra = files.len().saturating_sub(50);
    files.truncate(50);
    Ok((files, extra))
}
fn candidates(path: &Path) -> Option<Vec<(usize, String)>> {
    if std::fs::metadata(path).ok()?.len() > 262144 {
        return None;
    }
    let raw = store::read(path).ok()?;
    let raw = std::str::from_utf8(&raw).ok()?;
    let bullet = regex::Regex::new(r"^(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?").unwrap();
    let mut out = Vec::new();
    let (mut fenced, mut front) = (false, false);
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if i == 0 && line == "---" {
            front = true;
            continue;
        }
        if front {
            front = line != "---";
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced
            || line.is_empty()
            || ["#", "|", "<!--", ">", "---"]
                .iter()
                .any(|p| line.starts_with(p))
        {
            continue;
        }
        let text = bullet.replace(line, "").trim().to_owned();
        if (12..=400).contains(&text.chars().count()) {
            out.push((i + 1, text));
        }
    }
    Some(out)
}
pub(crate) fn import(
    s: &Store,
    o: &Options,
    author: &str,
    cwd: &Path,
    flag: &AtomicBool,
) -> Result<Output> {
    let source_path = o.path("--from").unwrap_or_default();
    let source = os::diagnostic(source_path.as_os_str());
    if source.is_empty() {
        return err("import requires --from serena|PATH");
    }
    if o.has("--apply") && o.has("--dry-run") {
        return err("--apply and --dry-run are opposites");
    }
    if ["--accept", "--retire", "--reject", "--reason"]
        .iter()
        .any(|k| !o.value(k).is_empty())
        || !o.kinds.is_empty()
    {
        return err("--accept/--retire/--reject/--reason/--kind apply only to review");
    }
    if o.has("--apply") {
        parent_only("import into parent-owned memory")?;
    }
    let (files, extra) = source_files(s, source_path.as_os_str(), cwd)?;
    let notes = s.active()?;
    let existing: HashSet<_> = notes
        .iter()
        .flat_map(|e| {
            std::iter::once(normalized(&e.body)).chain(
                e.body
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(normalized),
            )
        })
        .collect();
    let (mut rows, mut unread, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
    let mut row_paths = Vec::new();
    let mut unread_paths = Vec::new();
    for path in &files {
        check(flag)?;
        let Some(lines) = candidates(path) else {
            unread.push(label(s, path));
            unread_paths.push(label_os(s, path));
            continue;
        };
        for (line, body) in lines {
            let digest = short_hash(&body);
            if !seen.insert(digest.clone()) {
                continue;
            }
            let status = if existing.contains(&normalized(&body)) {
                "duplicate"
            } else {
                "new"
            };
            row_paths.push(label_os(s, path));
            rows.push(json!({"n":0,"path":label(s,path),"line":line,"sha256":digest,"text":body,"status":status}));
        }
    }
    let omitted = rows.len().saturating_sub(200);
    rows.truncate(200);
    row_paths.truncate(200);
    for (i, row) in rows.iter_mut().enumerate() {
        row["n"] = (i + 1).into();
        if !store::clean(&text(row, "text"), true) {
            row["status"] = "withheld".into();
            row["text"] = "".into();
        }
    }
    let mut chosen = Vec::new();
    let select = o.value("--select");
    if o.has("--apply") {
        if o.has("--all") == !select.is_empty() {
            return err("import --apply needs exactly one of --select N[,N...] or --all");
        }
        if o.has("--all") {
            chosen = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r["status"] == "new")
                .map(|(i, _)| i)
                .collect();
        } else {
            let mut seen = BTreeSet::new();
            for part in select.split(',') {
                let n = part.trim().parse::<usize>().unwrap_or(0);
                if n == 0 || n > rows.len() {
                    return err(format!(
                        "--select takes candidate numbers between 1 and {}",
                        rows.len()
                    ));
                }
                if !seen.insert(n) {
                    continue;
                }
                if rows[n - 1]["status"] != "new" {
                    return err(format!(
                        "candidate {n} is {} and cannot be imported",
                        text(&rows[n - 1], "status")
                    ));
                }
                chosen.push(n - 1);
            }
        }
    } else if !select.is_empty() || o.has("--all") {
        return err("--select/--all apply only with --apply");
    }
    let (mut imported, mut refused) = (0, 0);
    for i in chosen {
        let r = &rows[i];
        let mut body = format!("{}\nimported from: ", text(r, "text")).into_bytes();
        body.extend(row_paths[i].as_encoded_bytes());
        body.extend(format!("#L{} sha256:{}\n", r["line"], text(r, "sha256")).as_bytes());
        let ok = s
            .append_bytes(author, &body, "imported", None, "", None, flag)
            .is_ok();
        rows[i]["status"] = if ok { "imported" } else { "refused" }.into();
        if ok {
            imported += 1;
        } else {
            refused += 1;
        }
    }
    let mut counts = HashMap::<String, usize>::new();
    for r in &rows {
        *counts.entry(text(r, "status")).or_default() += 1;
    }
    if o.has("--json") {
        let mut paths: Vec<_> = row_paths
            .iter()
            .enumerate()
            .map(|(i, p)| (format!("/candidates/{i}/path"), p.clone()))
            .collect();
        paths.extend(
            unread_paths
                .iter()
                .enumerate()
                .map(|(i, p)| (format!("/unread/{i}"), p.clone())),
        );
        return Ok((
            if refused > 0 { 1 } else { 0 },
            os::json_line(
                &json!({"schema":1,"kind":"oms-memory-import","applied":o.has("--apply"),"files":files.len(),"unread":unread,"files_omitted":extra,"candidates_omitted":omitted,"counts":counts,"candidates":rows}),
                &paths,
            )?,
            Vec::new(),
        ));
    }
    let mut out = b"memory import from ".to_vec();
    out.extend(source_path.as_os_str().as_encoded_bytes());
    out.extend(
        format!(
            ": {} candidate(s) in {} file(s){}\n",
            rows.len(),
            files.len(),
            if o.has("--apply") {
                ""
            } else {
                "; dry run, nothing written"
            }
        )
        .as_bytes(),
    );
    for (r, path) in rows.iter().zip(&row_paths) {
        out.extend(format!("{:>3}  {:<9} ", r["n"], text(r, "status")).as_bytes());
        out.extend(path.as_encoded_bytes());
        out.extend(format!(":{}  sha256:{}\n", r["line"], text(r, "sha256")).as_bytes());
        let body = text(r, "text");
        if !body.is_empty() {
            out.extend(format!("       {}\n", preview(&body, 160)).as_bytes());
        }
    }
    for path in unread_paths {
        out.extend(b"unread: ");
        out.extend(path.as_encoded_bytes());
        out.extend(b" (not UTF-8, or larger than 262144 bytes)\n");
    }
    let mut suffix = String::new();
    if extra > 0 {
        suffix.push_str(&format!(
            "incomplete: {extra} more file(s) not read (limit 50)\n"
        ));
    }
    if omitted > 0 {
        suffix.push_str(&format!(
            "incomplete: {omitted} more candidate(s) not listed (limit 200)\n"
        ));
    }
    if o.has("--apply") {
        suffix.push_str(&format!("memory: imported {imported} note(s) as kind=imported{}; they wait in `oms agent-memory review`\n",if refused>0{format!(", {refused} refused by the memory writer")}else{String::new()}));
    } else if counts.contains_key("new") {
        suffix.push_str(&format!(
            "next: oms agent-memory import --from {source} --apply --select N[,N...]   (or --all)\n"
        ));
    }
    suffix.push_str("note: the source files were only read; an imported line is someone's note, not a verified fact\n");
    Ok((
        if refused > 0 { 1 } else { 0 },
        {
            out.extend(suffix.as_bytes());
            out
        },
        Vec::new(),
    ))
}
pub(crate) fn capture(s: &Store, stdin: Option<&[u8]>, flag: &AtomicBool) -> Result<Output> {
    let silent = || Ok((0, Vec::new(), Vec::new()));
    if std::env::var("OMS_LESSON_CAPTURE").as_deref() != Ok("1")
        || crate::cli::child()
        || !s.repo.join(".oms").is_dir()
    {
        return silent();
    }
    let mut raw = Vec::new();
    if let Some(bytes) = stdin {
        if bytes.len() > 65536 {
            return silent();
        }
        raw.extend_from_slice(bytes);
    } else if std::io::stdin().take(65537).read_to_end(&mut raw).is_err() || raw.len() > 65536 {
        return silent();
    }
    let Some(payload) = parse_json(&raw) else {
        return silent();
    };
    let Some(prompt) = payload["prompt"].as_str() else {
        return silent();
    };
    if prompt.chars().count() > 4000 || prompt.trim_start().starts_with(['<', '[', '{']) {
        return silent();
    }
    let re=regex::Regex::new(r"(?i)^(?:no[,.! ]|nope\b|actually[, ]|don'?t\b|do not\b|never\b|always\b|stop\b|instead[, ]|please (?:don'?t|never|always)\b|use\b.{1,80}\binstead\b|not\b.{1,60},\s*(?:use|but)\b|아니[,. ]|아니야|아니요|그게 아니|말고\b|.{1,60}말고 |.{1,60}대신 |.{0,80}(?:하지 ?마|쓰지 ?마|쓰지 말|하지 말)|항상 |절대 )").unwrap();
    let sentences = regex::Regex::new(r"[.!?。]\s+|\n+").unwrap();
    let mut parts = Vec::new();
    let mut start = 0;
    for found in sentences.find_iter(prompt.trim()) {
        let punct = found
            .as_str()
            .chars()
            .next()
            .filter(|c| ".!?。".contains(*c))
            .map(char::len_utf8)
            .unwrap_or(0);
        parts.push(&prompt.trim()[start..found.start() + punct]);
        start = found.end();
    }
    parts.push(&prompt.trim()[start..]);
    let sentence = parts
        .into_iter()
        .map(flat)
        .find(|s| (12..=240).contains(&s.chars().count()) && re.is_match(s))
        .unwrap_or_default();
    if sentence.is_empty() || !store::clean(&sentence, true) {
        return silent();
    }
    let id = format!("pend-{}", short_hash(&sentence));
    let path = s.shared.parent().unwrap().join("pending.jsonl");
    let result = (|| -> Result<()> {
        let _g = store::lock(&path, flag)?;
        let rows = s.rows("pending.jsonl")?;
        let decisions = decided(s)?;
        let ids: HashSet<_> = rows.iter().map(|r| text(r, "id")).collect();
        if ids.contains(&id) || decisions.contains(&id) || ids.difference(&decisions).count() >= 200
        {
            return Ok(());
        }
        s.ignore(flag)?;
        let data = json_line(&json!({"schema":1,"ts":now(),"id":id,"text":sentence}))?;
        store::mutate(&path, flag, |old| {
            let mut out = old.to_vec();
            out.extend(data);
            Ok(out)
        })?;
        Ok(())
    })();
    let _ = result;
    silent()
}
