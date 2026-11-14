use crate::{
    cli::Options,
    sources,
    store::{self, Store},
    workflows::preview,
    *,
};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
fn normalize(s: &str) -> String {
    let re = regex::Regex::new(r"^\s*(?:#{1,6}\s+|>+\s*|[-*+]\s+|\d+[.)]\s+)").unwrap();
    fold(&flat(
        &s.lines()
            .map(|l| re.replace(l, "").replace(['`', '*'], ""))
            .collect::<Vec<_>>()
            .join(" "),
    ))
}
fn shingles(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .windows(5)
        .map(|w| w.join(" "))
        .collect()
}
fn read(path: &Path, limit: usize) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    let f = std::fs::File::open(path).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    f.take(limit as u64).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
fn instructions(repo: &Path) -> Vec<(PathBuf, String)> {
    let home = store::home();
    let codex = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let mut wanted: Vec<_> = [
        "AGENTS.md",
        "AGENTS.override.md",
        "CLAUDE.md",
        ".claude/CLAUDE.md",
        "CLAUDE.local.md",
        "GEMINI.md",
    ]
    .iter()
    .map(|p| repo.join(p))
    .collect();
    wanted.extend([
        home.join(".claude/CLAUDE.md"),
        codex.join("AGENTS.md"),
        home.join(".gemini/AGENTS.md"),
        home.join(".gemini/GEMINI.md"),
    ]);
    let re = regex::Regex::new(r"@([A-Za-z0-9._~/-][A-Za-z0-9._/~\-]*)").unwrap();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in wanted {
        let Some(body) = read(&path, 1048576) else {
            continue;
        };
        let Ok(real) = std::fs::canonicalize(&path) else {
            continue;
        };
        if !seen.insert(real) {
            continue;
        }
        out.push((path.clone(), body.clone()));
        for cap in re.captures_iter(&body) {
            let index = cap.get(0).unwrap().start();
            if index > 0
                && body[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '@')
            {
                continue;
            }
            let target = &cap[1];
            let target = target
                .strip_prefix("~/")
                .map(|p| home.join(p))
                .unwrap_or_else(|| path.parent().unwrap().join(target));
            let Ok(target) = std::fs::canonicalize(target) else {
                continue;
            };
            if !target.starts_with(repo) || seen.contains(&target) {
                continue;
            }
            if let Some(text) = read(&target, 1048576) {
                seen.insert(target.clone());
                out.push((target, text));
            }
        }
    }
    out
}
fn cap(name: &str, default: u64) -> Result<u64> {
    let raw = std::env::var(name).unwrap_or_default();
    if raw.is_empty() {
        return Ok(default);
    }
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return err(format!(
            "{name} must be a whole number (0 turns that check off), not {raw:?}"
        ));
    }
    raw.parse().map_err(|_| {
        Error(
            2,
            format!("{name} must be a whole number (0 turns that check off), not {raw:?}"),
        )
    })
}
pub(crate) fn run(s: &Store, o: &Options) -> Result<Output> {
    let days = o.number("--cold-days", 30)?;
    let limit = o.number("--limit", 20)?;
    let (notes, pins) = s.logs()?;
    let status = records::states(&notes, &pins);
    let notes: Vec<_> = notes
        .iter()
        .filter(|e| e.kind != "retirement" && !status.contains_key(&e.event_id))
        .collect();
    let pins: Vec<_> = pins
        .iter()
        .filter(|e| !status.contains_key(&e.event_id))
        .collect();
    let (receipts_state, receipts) = if !s.database.exists() {
        ("absent", Default::default())
    } else {
        match db::connect(&s.database, true).and_then(|db| db::receipts(&db)) {
            Ok(r) => ("present", r),
            Err(_) => ("unreadable", Default::default()),
        }
    };
    let clock = timestamp(&now()).unwrap_or(0);
    let mut cold = Vec::new();
    if receipts_state == "present" {
        for e in &notes {
            let Some(born) = timestamp(&e.occurred_at) else {
                continue;
            };
            if ((clock - born) as i128) < (days as i128) * 86400 {
                continue;
            }
            let stats = receipts.get(&e.event_id);
            let hits = stats.map(|(_, n)| *n).unwrap_or(0);
            let last = stats.map(|(s, _)| s.as_str());
            let reason = if hits == 0 {
                "never"
            } else if last
                .and_then(timestamp)
                .is_some_and(|ts| (clock - ts) as i128 >= days as i128 * 86400)
            {
                "long-ago"
            } else {
                continue;
            };
            cold.push(json!({"id":e.event_id,"source":e.source,"kind":e.kind,"reason":reason,"age_days":(clock-born)/86400,"hits":hits,"last_recalled":if hits>0{last}else{None},"preview":preview(&e.body,80)}));
        }
    }
    cold.sort_by(|a, b| {
        b["age_days"]
            .as_i64()
            .cmp(&a["age_days"].as_i64())
            .then(text(a, "id").cmp(&text(b, "id")))
    });
    let files = instructions(&s.repo);
    let prepared: Vec<_> = files
        .iter()
        .map(|(p, t)| {
            let t = normalize(t);
            (sources::display(p, &s.repo), shingles(&t), t)
        })
        .collect();
    let mut dup = Vec::new();
    for e in notes.iter().chain(&pins) {
        let flat = normalize(&e.body);
        if flat.chars().count() < 20 {
            continue;
        }
        let mine = if flat.split_whitespace().count() >= 8 {
            shingles(&flat)
        } else {
            HashSet::new()
        };
        let mut best: Option<(&str, &str, f64)> = None;
        for (name, theirs, body) in &prepared {
            if body.contains(&flat) {
                best = Some(("exact", name, 1.));
                break;
            }
            if !mine.is_empty() {
                let ratio = mine.intersection(theirs).count() as f64 / mine.len() as f64;
                if ratio >= 0.9 && best.is_none_or(|(_, _, r)| ratio > r) {
                    best = Some(("near", name, (ratio * 100.).round() / 100.));
                }
            }
        }
        if let Some((kind, name, ratio)) = best {
            dup.push(json!({"id":e.event_id,"source":e.source,"kind":e.kind,"match":kind,"file":name,"overlap":ratio,"bytes":e.body.len(),"preview":preview(&e.body,80)}));
        }
    }
    dup.sort_by(|a, b| {
        (a["match"] != "exact")
            .cmp(&(b["match"] != "exact"))
            .then(b["bytes"].as_u64().cmp(&a["bytes"].as_u64()))
            .then(text(a, "id").cmp(&text(b, "id")))
    });
    let lines_cap = cap("OMS_MEMORY_INDEX_CAP_LINES", 200)?;
    let bytes_cap = cap("OMS_MEMORY_INDEX_CAP_BYTES", 25000)?;
    let index = sources::claude_index(&s.repo);
    let mut caps = Vec::new();
    if let Ok(m) = std::fs::metadata(&index) {
        if m.is_file() {
            let text = read(&index, 4 * 1024 * 1024).unwrap_or_default();
            let rows: Vec<_> = text.split_inclusive('\n').collect();
            let (mut used, mut loaded) = (0u64, 0u64);
            for row in &rows {
                let width = row.len() as u64;
                if (lines_cap > 0 && loaded >= lines_cap)
                    || (bytes_cap > 0 && used + width > bytes_cap)
                {
                    break;
                }
                loaded += 1;
                used += width;
            }
            let lines = rows.len() as u64;
            let over =
                (lines_cap > 0 && lines > lines_cap) || (bytes_cap > 0 && m.len() > bytes_cap);
            let near = (lines_cap > 0 && lines as f64 >= lines_cap as f64 * 0.9)
                || (bytes_cap > 0 && m.len() as f64 >= bytes_cap as f64 * 0.9);
            caps.push(json!({"provider":"claude","role":"auto-memory index","path":sources::display(&index,&s.repo),"lines":lines,"bytes":m.len(),"cap_lines":if lines_cap>0{Some(lines_cap)}else{None},"cap_bytes":if bytes_cap>0{Some(bytes_cap)}else{None},"status":if over{"over"}else if near{"near"}else{"ok"},"lines_not_loaded":if over{lines-loaded}else{0},"cap_source":"https://code.claude.com/docs/en/memory","cap_overridden":(["OMS_MEMORY_INDEX_CAP_LINES","OMS_MEMORY_INDEX_CAP_BYTES"].iter().any(|k|std::env::var_os(k).is_some_and(|v|!v.is_empty())))}));
        }
    }
    let cold_count = cold.len();
    let dup_count = dup.len();
    cold.truncate(limit);
    dup.truncate(limit);
    let report = json!({"schema":1,"kind":"oms-memory-hygiene","repo":os::diagnostic(s.repo.file_name().unwrap_or_default()),"read_only":true,"cold_days":days,"active":{"notes":notes.len(),"pins":pins.len()},"cold":{"recall_receipts":receipts_state,"count":cold_count,"items":cold,"truncated":cold_count>limit},"duplicated":{"instruction_files":prepared.iter().map(|(p,_,_)|p).collect::<Vec<_>>(),"count":dup_count,"items":dup,"truncated":dup_count>limit},"index_caps":caps,"not_checked":["codex memories and antigravity rules: no documented load cap was confirmed, so none is assumed"]});
    if o.has("--json") {
        let mut paths = vec![(
            "/repo".into(),
            s.repo.file_name().unwrap_or_default().to_owned(),
        )];
        for (i, (path, _)) in files.iter().enumerate() {
            paths.push((
                format!("/duplicated/instruction_files/{i}"),
                sources::display_os(path, &s.repo),
            ));
        }
        for (i, row) in report["duplicated"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            if let Some((path, _)) = files
                .iter()
                .find(|(path, _)| sources::display(path, &s.repo) == text(row, "file"))
            {
                paths.push((
                    format!("/duplicated/items/{i}/file"),
                    sources::display_os(path, &s.repo),
                ));
            }
        }
        if !caps.is_empty() {
            paths.push((
                "/index_caps/0/path".into(),
                sources::display_os(&index, &s.repo),
            ));
        }
        return Ok((0, os::json_line(&report, &paths)?, Vec::new()));
    }
    let mut heading = b"memory hygiene for ".to_vec();
    heading.extend(s.repo.file_name().unwrap_or_default().as_encoded_bytes());
    heading.extend(b": read-only; no note is changed (retire or supersede one yourself)\n");
    let mut out = format!("active: {} note(s), {} pin(s)\n", notes.len(), pins.len());
    if receipts_state != "present" {
        out.push_str(&format!("cold notes: not assessed; recall receipts are {} (they appear after `oms agent-memory recall`)\n",if receipts_state=="absent"{"absent: no index yet"}else{"unreadable"}));
    } else {
        out.push_str(&format!("cold notes: {cold_count} never recalled or not recalled for {days}+ days (receipts count hits since this index was last rebuilt)\n"));
        for r in report["cold"]["items"].as_array().unwrap() {
            out.push_str(&format!(
                "  {}  {}d old, {} hit(s), {}  {}\n",
                text(r, "id"),
                r["age_days"],
                r["hits"],
                if r["reason"] == "never" {
                    "never recalled".into()
                } else {
                    format!("last recalled {}", text(r, "last_recalled"))
                },
                text(r, "preview")
            ));
        }
        if cold_count > limit {
            out.push_str("  ... more; raise --limit\n");
        }
    }
    let names: Vec<_> = prepared.iter().map(|(p, _, _)| p.as_str()).collect();
    out.push_str(&format!(
        "duplicated in always-loaded instructions: {dup_count} ({})\n",
        if names.is_empty() {
            "no instruction file found".into()
        } else {
            names.join(", ")
        }
    ));
    for r in report["duplicated"]["items"].as_array().unwrap() {
        out.push_str(&format!(
            "  {}  {} in {}  {}\n",
            text(r, "id"),
            text(r, "match"),
            text(r, "file"),
            text(r, "preview")
        ));
    }
    if dup_count > limit {
        out.push_str("  ... more; raise --limit\n");
    }
    if caps.is_empty() {
        out.push_str(
            "provider index caps: no Claude auto-memory index found for this repository\n",
        );
    }
    for row in caps {
        let mut cap = Vec::new();
        if lines_cap > 0 {
            cap.push(format!("{lines_cap} lines"));
        }
        if bytes_cap > 0 {
            cap.push(format!("{bytes_cap} bytes"));
        }
        out.push_str(&format!("provider index caps: claude auto-memory index ({}): {} lines, {} bytes; load cap {} -> {}{}\n  cap source: {}{}\n",text(&row,"path"),row["lines"],row["bytes"],if cap.is_empty(){"no cap set".into()}else{cap.join(" / ")},text(&row,"status"),if row["status"]=="over"{format!("; about {} line(s) at the end are not loaded at session start",row["lines_not_loaded"])}else{String::new()},text(&row,"cap_source"),if row["cap_overridden"]==true{" (overridden by environment)"}else{""}));
    }
    out.push_str("not checked: codex memories and antigravity rules: no documented load cap was confirmed, so none is assumed\n");
    heading.extend(out.as_bytes());
    Ok((0, heading, Vec::new()))
}
