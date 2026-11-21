use super::{StoreError, paths};
use crate::{
    metrics,
    value::{V, display, obj, ratio, rounded},
};
use oms_safe_fs::Root;
use std::cmp::Ordering as Cmp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};

const LEDGER: &str = ".oms/hooks/events.jsonl";
const DIRECTORY: &str = ".oms/artifacts/output-digest";
const BASIS: &str = "reported values from retained rows, never approval or verification evidence; a category that is absent or `not-recorded` means the data was not recorded, not that nothing was wasted";
const WINDOW: &str = "categories 1-2 use the selected index rows; 3-5 read the hook ledger and capture directory as they are, ignoring --since/--until";
fn label(r: &V, key: &str, fallback: &str) -> String {
    r[key]
        .text()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .into()
}

fn sum<'a>(values: impl Iterator<Item = &'a V>) -> V {
    let mut n = obj!("n":0);
    for v in values {
        n.add("n", v);
    }
    n.remove("n")
}
fn share(a: &V, b: &V) -> Result<V, String> {
    if !b.truth() {
        Ok(V::Null)
    } else {
        Ok(rounded(ratio(a, b)?, 4).into())
    }
}
fn absent(unit: &str, reason: &str) -> V {
    obj!("status":"not-recorded","unit":unit,"reason":reason)
}
fn source(row: &V, pos: usize) -> String {
    label(row, "event_id", &format!("index-row-{pos}"))
}
fn descending(a: &V, b: &V, key: &str) -> Cmp {
    let a = a[key].int().unwrap_or("0");
    let b = b[key].int().unwrap_or("0");
    b.len().cmp(&a.len()).then_with(|| b.cmp(a))
}
fn failed(row: &V) -> bool {
    row["exit"].int().is_some_and(|s| s != "0")
        || (![Some("none"), Some("dry-run")].contains(&row["context_verification"].text())
            && row["verify_exit"].int().is_some_and(|s| s != "0"))
}
struct Delegate<'a> {
    row: &'a V,
    position: usize,
    measured: metrics::Metrics,
}
pub(crate) fn report(
    root: &Root,
    selected: &[&V],
    rows: &[V],
    admitted: &BTreeSet<Vec<u8>>,
    top: usize,
    interrupted: &AtomicBool,
) -> Result<V, String> {
    let positions: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(n, r)| (std::ptr::from_ref(r), n + 1))
        .collect();
    let mut delegates = Vec::new();
    for row in selected
        .iter()
        .filter(|r| r["kind"].text() == Some("delegate"))
    {
        if interrupted.load(Ordering::Acquire) {
            return Err("interrupted".into());
        }
        delegates.push(Delegate {
            row,
            position: positions[&std::ptr::from_ref(*row)],
            measured: metrics::metrics(root.physical_root(), row)?,
        });
    }
    let ledger = root.read(LEDGER, 256 * 1024 * 1024, oms_safe_fs::Policy::source());
    let hook_rows: Option<Vec<V>> = ledger.as_ref().ok().map(|bytes| {
        let decoded = String::from_utf8_lossy(bytes)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        decoded
            .split('\n')
            .filter_map(|line| crate::value::parse(line.as_bytes(), false).ok())
            .filter(|r| matches!(r, V::Object(_)))
            .collect()
    });
    let switches = switches(hook_rows.as_deref(), top)?;
    let injections = match &ledger {
        Ok(bytes) if bytes.len() > 64 * 1024 * 1024 => absent(
            "bytes",
            "ledger unreadable: hook event ledger is larger than 67108864 bytes",
        ),
        Ok(bytes) => injections(bytes, top)?,
        Err(_) => absent("bytes", "no hook event ledger"),
    };
    Ok(
        obj!("schema":1,"top":top,"basis":BASIS,"windowing":WINDOW,"categories":obj!(
        "unadmitted_delegate_tokens":unadmitted(&delegates,admitted,top)?,
        "repeated_failed_delegations":repeated(&delegates,admitted,top)?,
        "model_switches":switches,"hook_injected_bytes":injections,"stored_output_bytes":outputs(root,top)?)),
    )
}
fn is_admitted(row: &V, admitted: &BTreeSet<Vec<u8>>) -> bool {
    row["patch_sha256"]
        .bytes()
        .is_some_and(|d| admitted.contains(d))
}
fn unadmitted(
    delegates: &[Delegate<'_>],
    admitted: &BTreeSet<Vec<u8>>,
    top: usize,
) -> Result<V, String> {
    let total = sum(delegates.iter().filter_map(|d| d.measured.tokens.as_ref()));
    let mut items = Vec::new();
    for d in delegates {
        let Some(usage) = &d.measured.tokens else {
            continue;
        };
        if is_admitted(d.row, admitted) {
            continue;
        }
        let model = if d.measured.ambiguous {
            "ambiguous".into()
        } else {
            d.measured
                .model
                .clone()
                .unwrap_or_else(|| label(d.row, "selected_model", "unrecorded").into())
        };
        items.push(obj!("tokens":usage.clone(),"role":label(d.row,"role","unknown"),"provider":label(d.row,"provider","unrecorded"),"model":model,"source":source(d.row,d.position),"artifact":d.row["artifact"].text().filter(|s|!s.is_empty()).map(str::to_string)));
    }
    if !total.truth() {
        return Ok(absent("tokens", "no delegate row reports tokens"));
    }
    let reported = sum(items.iter().map(|i| &i["tokens"]));
    let n = items.len();
    items.sort_by(|a, b| {
        descending(a, b, "tokens").then_with(|| a["source"].bytes().cmp(&b["source"].bytes()))
    });
    items.truncate(top);
    // Distinct admitted patches among delegate rows that report tokens: the
    // denominator must come from the same rows as `of_total`.
    let mut patches = BTreeSet::new();
    for d in delegates {
        if d.measured.tokens.is_some()
            && is_admitted(d.row, admitted)
            && let Some(digest) = d.row["patch_sha256"].bytes()
        {
            patches.insert(digest);
        }
    }
    let per_patch = if patches.is_empty() {
        None
    } else {
        Some(rounded(ratio(&total, &V::from(patches.len()))?, 1))
    };
    Ok(
        obj!("status":"recorded","unit":"tokens","tokens":reported.clone(),"of_total":total.clone(),"share":share(&reported,&total)?,"rows":n,"admitted_patches":patches.len(),"tokens_per_admitted_patch":per_patch,"items":items,"note":"delegate rows whose patch no later patch-admit/patch-land accepted; includes delegations still awaiting admission"),
    )
}
fn repeated(
    delegates: &[Delegate<'_>],
    admitted: &BTreeSet<Vec<u8>>,
    top: usize,
) -> Result<V, String> {
    let total = sum(delegates.iter().filter_map(|d| d.measured.tokens.as_ref()));
    let mut groups = BTreeMap::<(String, String), Vec<&Delegate<'_>>>::new();
    let mut order = Vec::new();
    for d in delegates {
        for (field, kind) in [("prompt_sha256", "brief"), ("task_id", "task")] {
            if let Some(key) = d.row[field].text().filter(|s| !s.is_empty()) {
                let group_key = (kind.to_string(), key.to_string());
                if !groups.contains_key(&group_key) {
                    order.push(group_key.clone());
                }
                groups.entry(group_key).or_default().push(d);
            }
        }
    }
    if groups.is_empty() {
        return Ok(absent(
            "tokens",
            "no delegate row carries a brief digest or task id",
        ));
    }
    let mut items = Vec::new();
    let mut counted = BTreeMap::new();
    for (kind, key) in order {
        let members = groups.remove(&(kind.clone(), key.clone())).unwrap();
        if members.len() < 2
            || !members
                .iter()
                .all(|d| failed(d.row) && !is_admitted(d.row, admitted))
        {
            continue;
        }
        for d in &members {
            counted.insert(
                d.position,
                d.measured.tokens.clone().unwrap_or_else(|| 0.into()),
            );
        }
        let usage = sum(members.iter().filter_map(|d| d.measured.tokens.as_ref()));
        let providers: BTreeSet<_> = members
            .iter()
            .map(|d| label(d.row, "provider", "unrecorded"))
            .collect();
        let sources: Vec<V> = members
            .iter()
            .take(5)
            .map(|d| source(d.row, d.position).into())
            .collect();
        items.push(obj!("by":kind,"key":key.chars().take(16).collect::<String>(),"attempts":members.len(),"tokens":usage,"token_reports":members.iter().filter(|d|d.measured.tokens.is_some()).count(),"providers":providers.into_iter().map(V::from).collect::<Vec<_>>(),"sources":sources));
    }
    items.sort_by(|a, b| {
        descending(a, b, "tokens")
            .then_with(|| descending(a, b, "attempts"))
            .then_with(|| a["by"].bytes().cmp(&b["by"].bytes()))
            .then_with(|| a["key"].bytes().cmp(&b["key"].bytes()))
    });
    let n = items.len();
    items.truncate(top);
    let reported = sum(counted.values());
    Ok(
        obj!("status":"recorded","unit":"tokens","tokens":reported.clone(),"of_total":total.clone(),"share":share(&reported,&total)?,"groups":n,"items":items,"note":"every attempt in the group failed (non-zero exit or verification, patch not admitted); a brief and a task group can list the same rows, the total counts each row once"),
    )
}
fn switches(rows: Option<&[V]>, top: usize) -> Result<V, String> {
    let Some(rows) = rows else {
        return Ok(absent("events", "no hook event ledger"));
    };
    let mut last = BTreeMap::new();
    let mut pairs = 0;
    let mut items = Vec::new();
    for r in rows {
        let (Some(session), Some(model)) = (
            r["session"].text().filter(|s| !s.is_empty()),
            r["model"].text().filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        if r["action"].text() != Some("telemetry")
            || r["agent_id_hash"].truth()
            || r["subagent_type"].truth()
        {
            continue;
        }
        let Some(before) = last.insert(session, model) else {
            continue;
        };
        pairs += 1;
        if before != model {
            items.push(obj!("session":session,"from":before,"to":model,"ts":r["ts"].text().filter(|s|!s.is_empty()),"cache_creation_tokens":r["cache_creation_tokens"].int().map(|_|r["cache_creation_tokens"].clone()),"source":format!("{LEDGER} session={session} ts={}",r["ts"].text().filter(|s|!s.is_empty()).unwrap_or("undated"))));
        }
    }
    if pairs == 0 {
        return Ok(absent(
            "events",
            "no session has two hook telemetry rows that record a model",
        ));
    }
    let n = items.len();
    items.sort_by(|a, b| {
        descending(a, b, "cache_creation_tokens")
            .then_with(|| {
                a["ts"]
                    .bytes()
                    .unwrap_or(b"")
                    .cmp(b["ts"].bytes().unwrap_or(b""))
            })
            .then_with(|| a["session"].bytes().cmp(&b["session"].bytes()))
    });
    items.truncate(top);
    Ok(
        obj!("status":"recorded","unit":"events","switches":n,"of_transitions":pairs,"share":share(&V::from(n),&V::from(pairs))?,"items":items,"note":"share is of consecutive same-session model-recording rows, not tokens; cache_creation_tokens is shown only where the row reported it; effort changes are not recorded"),
    )
}
fn safe_label(v: &V, limit: usize) -> Option<&str> {
    v.text().filter(|s| {
        !s.is_empty()
            && s.chars().count() <= limit
            && s.chars()
                .all(|c| c.is_alphanumeric() || "_.:+() /-".contains(c))
    })
}
fn injections(raw: &[u8], top: usize) -> Result<V, String> {
    let mut sessions = BTreeMap::<String, V>::new();
    let mut total = obj!("n":0);
    let mut n = 0;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line.len() > 16 * 1024 * 1024 {
            continue;
        }
        let Ok(r) = crate::value::parse(line, false) else {
            continue;
        };
        if r["action"].text() != Some("injection")
            || safe_label(&r["status"], 40).is_none()
            || r["bytes"].int().is_none()
        {
            continue;
        }
        let size = r["bytes"]
            .int()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0 && *n <= 1 << 40);
        let Some(size) = size else {
            continue;
        };
        n += 1;
        total.add("n", &r["bytes"]);
        let mut session = safe_label(&r["session"], 64)
            .unwrap_or("unknown")
            .to_string();
        if !sessions.contains_key(&session) && sessions.len() >= 4096 {
            session = "other".into();
        }
        let entry=sessions.entry(session.clone()).or_insert_with(||obj!("session":session.clone(),"bytes":0,"calls":0,"largest":0,"source":format!("{LEDGER} session={session}")));
        entry.add("bytes", &r["bytes"]);
        entry.inc("calls");
        if entry["largest"].number().unwrap() < size as f64 {
            entry["largest"] = r["bytes"].clone();
        }
    }
    if n == 0 {
        return Ok(absent("bytes", "the ledger holds no injection rows"));
    }
    let n = sessions.len();
    let mut items: Vec<_> = sessions.into_values().collect();
    items.sort_by(|a, b| {
        descending(a, b, "bytes").then_with(|| a["session"].bytes().cmp(&b["session"].bytes()))
    });
    items.truncate(top);
    let listed = sum(items.iter().map(|i| &i["bytes"]));
    let total = total.remove("n");
    Ok(
        obj!("status":"recorded","unit":"bytes","bytes":listed.clone(),"of_total":total.clone(),"share":share(&listed,&total)?,"sessions":n,"items":items,"note":"the top sessions' share of every byte OMS hooks recorded injecting; utf-8 bytes, not tokens; hooks outside the ledger are not counted"),
    )
}
fn outputs(root: &Root, top: usize) -> Result<V, String> {
    let dir = match paths::Directory::open(root, &root.physical_root().join(DIRECTORY), false) {
        Ok(d) => d,
        Err(_) => return Ok(absent("bytes", "no output-digest capture directory")),
    };
    let mut items = Vec::new();
    for name in dir.names(65536).map_err(|e| e.to_string())? {
        let Ok(info) = dir.stat(&name) else {
            continue;
        };
        if rustix::fs::FileType::from_raw_mode(info.st_mode) != rustix::fs::FileType::RegularFile
            || info.st_size < 0
        {
            continue;
        }
        items.push(obj!("bytes":V::Int(info.st_size.to_string()),"source":format!("{DIRECTORY}/{}",name.to_string_lossy())));
    }
    dir.verify().map_err(|e: StoreError| e.to_string())?;
    if items.is_empty() {
        return Ok(absent("bytes", "the capture directory is empty"));
    }
    let total = sum(items.iter().map(|i| &i["bytes"]));
    let n = items.len();
    items.sort_by(|a, b| {
        descending(a, b, "bytes").then_with(|| a["source"].bytes().cmp(&b["source"].bytes()))
    });
    items.truncate(top);
    let listed = sum(items.iter().map(|i| &i["bytes"]));
    Ok(
        obj!("status":"recorded","unit":"bytes","bytes":listed.clone(),"of_total":total.clone(),"share":share(&listed,&total)?,"files":n,"items":items,"note":"largest raw captures kept on disk; bytes, not tokens, and the newest 50 are all it keeps"),
    )
}
pub(crate) fn emit(r: &V) -> String {
    let mut out = format!(
        "token waste view (top {} per category)\n{}\n{}\n",
        display(&r["top"]),
        display(&r["basis"]),
        display(&r["windowing"])
    );
    for (number, key, label) in [
        (
            1,
            "unadmitted_delegate_tokens",
            "unadmitted delegate tokens",
        ),
        (
            2,
            "repeated_failed_delegations",
            "repeated failed delegations",
        ),
        (3, "model_switches", "model switches within a session"),
        (4, "hook_injected_bytes", "hook-injected bytes by session"),
        (5, "stored_output_bytes", "largest stored outputs"),
    ] {
        let b = &r["categories"][key];
        if b["status"].text() != Some("recorded") {
            writeln!(
                out,
                "{number} {label}: not recorded ({})",
                display(&b["reason"])
            )
            .unwrap();
            continue;
        }
        let get = |k| display(&b[k]);
        let detail = match number {
            1 => format!(
                "{} of {} reported delegate tokens over {} row(s)",
                get("tokens"),
                get("of_total"),
                get("rows")
            ),
            2 => format!(
                "{} of {} reported delegate tokens in {} group(s)",
                get("tokens"),
                get("of_total"),
                get("groups")
            ),
            3 => format!(
                "{} of {} transitions",
                get("switches"),
                get("of_transitions")
            ),
            4 => format!(
                "{} of {} bytes in the top session(s), {} session(s) in all",
                get("bytes"),
                get("of_total"),
                get("sessions")
            ),
            _ => format!(
                "{} of {} bytes in the largest file(s), {} file(s) in all",
                get("bytes"),
                get("of_total"),
                get("files")
            ),
        };
        let percent = b["share"]
            .number()
            .map(|v| format!("{:.1}%", v * 100.0))
            .unwrap_or_else(|| "-".into());
        writeln!(
            out,
            "{number} {label}: {detail} -- share {percent}; {}",
            get("note")
        )
        .unwrap();
        if number == 1 {
            let per_patch = b["tokens_per_admitted_patch"]
                .number()
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "-".into());
            writeln!(
                out,
                "  admitted patches: {}; delegate tokens per admitted patch: {per_patch}",
                get("admitted_patches")
            )
            .unwrap();
        }
        for i in b["items"].array() {
            let get = |k| display(&i[k]);
            let line = match number {
                1 => format!(
                    "{} tokens  role={} provider={} model={}  source={}{}",
                    get("tokens"),
                    get("role"),
                    get("provider"),
                    get("model"),
                    get("source"),
                    i["artifact"]
                        .text()
                        .map(|s| format!(" artifact={s}"))
                        .unwrap_or_default()
                ),
                2 => format!(
                    "{} tokens  {} attempts  {}={}  providers={}  sources={}",
                    get("tokens"),
                    get("attempts"),
                    get("by"),
                    get("key"),
                    i["providers"]
                        .array()
                        .iter()
                        .map(display)
                        .collect::<Vec<_>>()
                        .join(","),
                    i["sources"]
                        .array()
                        .iter()
                        .map(display)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                3 => format!(
                    "{} -> {}  cache-creation={}  source={}",
                    get("from"),
                    get("to"),
                    if i["cache_creation_tokens"] == V::Null {
                        "-".into()
                    } else {
                        get("cache_creation_tokens")
                    },
                    get("source")
                ),
                4 => format!(
                    "{} bytes  {} call(s)  largest={}  source={}",
                    get("bytes"),
                    get("calls"),
                    get("largest"),
                    get("source")
                ),
                _ => format!("{} bytes  source={}", get("bytes"), get("source")),
            };
            writeln!(out, "  {line}").unwrap();
        }
    }
    out
}
