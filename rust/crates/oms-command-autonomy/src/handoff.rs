//! Mechanical session handoffs over native producer projections. Recorded
//! prose is a claim; it is never fresh verification or resume authority.
use crate::planning::invoke;
use crate::storage::{Context, MAX_FILE};
use crate::{Error, Result, Services, require, value};
use oms_command_plan::storage::Store;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const SECTIONS: &[&str] = &[
    "Goal",
    "State",
    "Decisions",
    "Open findings",
    "Verified / Not verified",
    "Next step",
    "Pointers",
];
fn line(raw: &str, limit: usize) -> String {
    let mut clean = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty() && s != "/") {
        clean = clean.replace(&home.to_string_lossy().into_owned(), "~");
    }
    oms_contract_markdown::bounded_line(&clean, limit)
}
pub fn render(
    repo: &Path,
    state: &Path,
    max_bytes: usize,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<String> {
    require(
        (512..=65536).contains(&max_bytes),
        "handoff byte budget must be 512..65536",
    )?;
    let ctx = Context::new(
        repo.to_path_buf(),
        state.to_path_buf(),
        services,
        interrupted,
    )?;
    let mut data = BTreeMap::<&str, Vec<(String, String)>>::new();
    for name in SECTIONS {
        data.insert(name, vec![]);
    }
    let mut push = |section, text: String, source: &str| {
        data.get_mut(section)
            .unwrap()
            .push((text, source.to_owned()));
    };
    let plan = ctx.plan_value()?;
    push(
        "Goal",
        format!("plan goal: {}", line(value::text(&plan, "goal"), 300)),
        "agent-plan status",
    );
    let packet = ctx
        .store
        .optional(".oms/task/current.md", 65536)?
        .unwrap_or_default();
    let goal = oms_command_task::packet::section(&packet, "Goal");
    push(
        "Goal",
        format!(
            "task goal: {}",
            if goal.is_empty() {
                "not recorded".into()
            } else {
                line(&String::from_utf8_lossy(&goal), 300)
            }
        ),
        "task packet",
    );
    for decision in crate::advisory::handoff_decisions(&packet, 6, 4096) {
        push("Decisions", decision, "task packet Decisions");
    }
    let readers: &[(&str, &str, &[&str])] = &[
        ("State", "agent-task", &["status", "--json"]),
        (
            "Verified / Not verified",
            "runtime",
            &["evidence", "matrix", "--limit", "100"],
        ),
        (
            "Next step",
            "runtime",
            &["next", "--explain", "--limit", "100"],
        ),
        ("Decisions", "journal", &["show", "--decisions", "--json"]),
        (
            "Open findings",
            "peer-review",
            &["findings", "list", "--json"],
        ),
    ];
    for (section, tool, arguments) in readers {
        let mut args = vec!["--repo".into(), ctx.repo.as_os_str().to_owned()];
        args.extend(arguments.iter().map(|s| (*s).into()));
        match invoke(&ctx, tool, args, Duration::from_secs(40)) {
            Ok(out) if out.code == 0 => {
                let text = std::str::from_utf8(&out.stdout)
                    .map_err(|_| Error::new(2, "handoff producer returned non-text projection"))?;
                if let Ok(row) = value::parse(text.as_bytes()) {
                    let raw = value::canonical(&row)?;
                    // Keep complete records only; an oversized projection is
                    // named as a gap, never cut into an apparently valid fact.
                    if raw.len() <= 2048 {
                        push(
                            section,
                            String::from_utf8(raw)
                                .map_err(|_| Error::new(2, "invalid producer JSON"))?,
                            tool,
                        );
                    } else {
                        push(
                            section,
                            "projection exceeds line budget; open the source".into(),
                            tool,
                        );
                    }
                } else {
                    push(
                        section,
                        "producer projection unavailable or malformed".into(),
                        tool,
                    );
                }
            }
            _ => push(
                section,
                "native producer unavailable; not recorded".into(),
                tool,
            ),
        }
    }
    push(
        "Pointers",
        ".oms/task/current.md; .oms/plan/tasks.json; .oms/plan/progress.jsonl".into(),
        "local source records",
    );
    let mut withheld = 0;
    for rows in data.values_mut() {
        for (text, _) in rows {
            if crate::planning::scan(text.as_bytes()).is_err() {
                *text = "withheld: this line matched the sensitive-content scan; open the source"
                    .into();
                withheld += 1;
            }
        }
    }
    let mut omitted = BTreeMap::<&str, usize>::new();
    let compose = |data: &BTreeMap<&str, Vec<(String, String)>>,
                   omitted: &BTreeMap<&str, usize>| {
        let mut out = format!(
            "# Next-session prompt (draft: review and edit before use)\n\nDrafted mechanically at {}; no model was called. Recorded text is a claim, not fresh verification; recheck sources before acting.\n",
            value::now()
        );
        for name in SECTIONS {
            out.push_str(&format!("\n## {name}\n"));
            for (text, source) in &data[name] {
                out.push_str(&format!("- {text} [src: {source}]\n"));
            }
        }
        out.push_str(&format!(
            "\ntruncated: {} line(s) omitted; withheld: {withheld} line(s)\n",
            omitted.values().sum::<usize>()
        ));
        out
    };
    loop {
        let out = compose(&data, &omitted);
        if out.len() <= max_bytes {
            return Ok(out);
        }
        let pool = SECTIONS
            .iter()
            .filter(|name| {
                !["Goal", "Next step", "Pointers"].contains(name) && !data[**name].is_empty()
            })
            .copied()
            .collect::<Vec<_>>();
        let pool = if pool.is_empty() {
            SECTIONS
                .iter()
                .filter(|name| !data[**name].is_empty())
                .copied()
                .collect::<Vec<_>>()
        } else {
            pool
        };
        let Some(victim) = pool.iter().max_by_key(|name| data[**name].len()) else {
            return Err(Error::new(2, "handoff header exceeds byte budget"));
        };
        data.get_mut(*victim).unwrap().pop();
        *omitted.entry(victim).or_default() += 1;
    }
}
/// Append advisory consumption at most once per digest/session. A killed
/// append can leave a partial row; native readers refuse further mutation.
pub fn record_pickup(
    state: &Path,
    name: &str,
    session: &str,
    interrupted: &AtomicBool,
) -> Result<bool> {
    require(
        regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,200}\.md$")
            .unwrap()
            .is_match(name),
        "invalid handoff filename",
    )?;
    crate::checkpoint(interrupted)?;
    let store = Store::open(state)?;
    store.read(format!(".oms/handoffs/{name}"), MAX_FILE)?;
    let session = line(session, 120);
    let session = if session.is_empty() {
        "unknown".into()
    } else {
        session
    };
    let path = ".oms/handoffs/consumed.jsonl";
    let settings = oms_lock::Settings::from_env().map_err(|e| Error::new(2, e.to_string()))?;
    let _lock = oms_lock::Lock::for_file(&state.join(path), settings)
        .and_then(|l| l.acquire(None))
        .map_err(|e| Error::new(2, e.to_string()))?;
    let rows = store.rows(path, true)?;
    for row in rows {
        if row.text("handoff") == name && row.text("session") == session {
            return Ok(false);
        }
    }
    store.append(path, &value::plan(&json!({"schema": 1, "kind": "handoff-consumed", "handoff": name, "session": session, "ts": value::now()}))?)?;
    Ok(true)
}
