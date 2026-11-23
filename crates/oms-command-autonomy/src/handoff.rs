//! Mechanical session handoffs over native producer projections. Recorded
//! prose is a claim; it is never fresh verification or resume authority.
use crate::planning::invoke;
use crate::storage::{Context, MAX_FILE};
use crate::{Error, Result, Services, require, value};
use oms_command_plan::storage::Store;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const SECTIONS: &[&str] = &[
    "Goal",
    "State",
    "Decisions",
    "Open findings",
    "Verified / Not verified",
    "Next step",
    "Pointers",
];
const MAX_PROJECTION_BYTES: usize = 65536;
const WITHHELD: &str = "withheld: this source matched the sensitive-content scan; open the source";

#[derive(Default)]
struct Section {
    rows: Vec<Row>,
    producer_omitted: u64,
    renderer_omitted: u64,
    shortened: bool,
    withheld: bool,
    unavailable: bool,
    has_records: bool,
}

struct Row {
    text: String,
    source: String,
    record: bool,
}

impl Section {
    fn push(&mut self, text: String, source: &str) {
        self.rows.push(Row {
            text,
            source: source.to_owned(),
            record: true,
        });
    }

    fn note(&mut self, text: String, source: &str) {
        self.rows.push(Row {
            text,
            source: source.to_owned(),
            record: false,
        });
    }

    fn flags(&self) -> String {
        let mut flags = String::new();
        for (flag, present) in [
            ('p', self.producer_omitted > 0),
            ('r', self.renderer_omitted > 0 || self.shortened),
            ('w', self.withheld),
            ('u', self.unavailable),
        ] {
            if present {
                flags.push(flag);
            }
        }
        if flags.is_empty() {
            flags.push(if self.has_records { '-' } else { 'e' });
        }
        flags
    }
}

fn projection_records<'a>(projection: &'a Value, key: Option<&str>) -> Option<(&'a [Value], u64)> {
    let Some(key) = key else {
        let rows = projection
            .as_array()
            .map_or_else(|| std::slice::from_ref(projection), Vec::as_slice);
        return Some((rows, 0));
    };
    let collection = projection.get(key)?;
    let (rows, omitted) = if let Some(rows) = collection.as_array() {
        (rows, &projection["omitted"])
    } else {
        (collection["items"].as_array()?, &collection["omitted"])
    };
    Some((rows, omitted.as_u64().unwrap_or(0)))
}

fn blocked_task(record: &Value) -> bool {
    !matches!(record["state"].as_str(), Some("done" | "cancelled"))
        && record["reasons"].as_array().is_some_and(|reasons| {
            reasons.iter().any(|reason| {
                matches!(
                    reason["code"].as_str(),
                    Some("plan-contract-blocked" | "dependencies-unfinished")
                )
            })
        })
}

fn line(raw: &str, limit: usize) -> (String, bool) {
    let mut clean = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty() && s != "/") {
        clean = clean.replace(&home.to_string_lossy().into_owned(), "~");
    }
    let clean = oms_contract_markdown::bounded_line(&clean, usize::MAX);
    let shortened = clean.chars().count() > limit;
    (
        oms_contract_markdown::bounded_line(&clean, limit),
        shortened,
    )
}
pub fn render(
    repo: &Path,
    state: &Path,
    max_bytes: usize,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<String> {
    render_checked(repo, state, max_bytes, services, interrupted, &mut |_| {
        Ok(())
    })
}

/// The consumer may withhold a complete source before projection or budget
/// trimming. Independent eligible sources still contribute to the draft.
pub fn render_checked(
    repo: &Path,
    state: &Path,
    max_bytes: usize,
    services: &dyn Services,
    interrupted: &AtomicBool,
    check_source: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<String> {
    require(
        (512..=65536).contains(&max_bytes),
        "handoff byte budget must be 512..65536",
    )?;
    crate::checkpoint(interrupted)?;
    let deadline = Instant::now() + Duration::from_secs(40);
    let ctx = Context::new(
        repo.to_path_buf(),
        state.to_path_buf(),
        services,
        interrupted,
    )?;
    let mut data = BTreeMap::<&str, Section>::new();
    for name in SECTIONS {
        data.insert(name, Section::default());
    }
    let request = oms_command_plan::Request::new(oms_command_plan::Action::Show);
    let plan = oms_command_plan::Context::from_selected_state_before(
        &request,
        repo,
        repo,
        state,
        interrupted,
        deadline,
    )
    .and_then(|context| oms_command_plan::Plan::open(context, services, interrupted)?.load());
    crate::checkpoint(interrupted)?;
    let mut withheld = 0;
    match plan {
        Ok(snapshot)
            if snapshot
                .raw
                .as_deref()
                .is_none_or(|raw| check_source(raw).is_ok()) =>
        {
            let raw = snapshot.plan.text("goal");
            let (goal, shortened) = line(raw, 300);
            let section = data.get_mut("Goal").unwrap();
            section.has_records |= !goal.is_empty();
            section.shortened |= shortened;
            if goal.is_empty() {
                section.note("plan goal: not recorded".into(), "agent-plan status");
            } else {
                section.push(format!("plan goal: {goal}"), "agent-plan status");
            }
        }
        Ok(_) => {
            withheld += 1;
            let section = data.get_mut("Goal").unwrap();
            section.withheld = true;
            section.note(WITHHELD.into(), "agent-plan status");
        }
        Err(_) => {
            let section = data.get_mut("Goal").unwrap();
            section.unavailable = true;
            section.note("plan source unavailable".into(), "agent-plan status");
        }
    }
    let packet = ctx
        .store
        .optional(".oms/task/current.md", MAX_PROJECTION_BYTES);
    crate::checkpoint(interrupted)?;
    match packet {
        Ok(packet) => {
            let packet = packet.unwrap_or_default();
            if check_source(&packet).is_ok() {
                let goal = oms_command_task::packet::section(&packet, "Goal");
                let section = data.get_mut("Goal").unwrap();
                let (goal, shortened) = line(&String::from_utf8_lossy(&goal), 300);
                section.has_records |= !goal.is_empty();
                section.shortened |= shortened;
                if goal.is_empty() {
                    section.note("task goal: not recorded".into(), "task packet");
                } else {
                    section.push(format!("task goal: {goal}"), "task packet");
                }
                let (decisions, omitted) = crate::advisory::handoff_decisions(&packet, 6, 4096);
                let section = data.get_mut("Decisions").unwrap();
                section.has_records |= !decisions.is_empty();
                section.producer_omitted = omitted as u64;
                for decision in decisions {
                    section.push(decision, "task packet Decisions");
                }
            } else {
                withheld += 1;
                for name in ["Goal", "Decisions"] {
                    let section = data.get_mut(name).unwrap();
                    section.withheld = true;
                    section.note(WITHHELD.into(), "task packet");
                }
            }
        }
        Err(_) => {
            for name in ["Goal", "Decisions"] {
                let section = data.get_mut(name).unwrap();
                section.unavailable = true;
                section.note("task source unavailable".into(), "task packet");
            }
        }
    }
    let readers = [
        ("State", "agent-task", &["status", "--json"][..], None),
        (
            "Verified / Not verified",
            "runtime",
            &["evidence", "matrix", "--limit", "100"][..],
            Some("criteria"),
        ),
        (
            "Next step",
            "runtime",
            &["next", "--explain", "--limit", "100"][..],
            Some("actions"),
        ),
        (
            "Decisions",
            "journal",
            &["show", "--decisions", "--json"][..],
            Some("decisions"),
        ),
        (
            "Open findings",
            "peer-review",
            &["findings", "list", "--status", "open", "--json"][..],
            None,
        ),
    ];
    for (name, tool, arguments, collection) in readers {
        crate::checkpoint(interrupted)?;
        let section = data.get_mut(name).unwrap();
        let source = format!("{tool} {}", arguments.join(" "));
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            section.unavailable = true;
            section.note(
                "native producer unavailable: shared read deadline exhausted".into(),
                &source,
            );
            continue;
        }
        let mut args = arguments.iter().map(|s| (*s).into()).collect::<Vec<_>>();
        args.extend(["--repo".into(), ctx.repo.as_os_str().to_owned()]);
        let output = invoke(&ctx, tool, args, remaining);
        crate::checkpoint(interrupted)?;
        if Instant::now() >= deadline {
            section.unavailable = true;
            section.note(
                "native producer unavailable: shared read deadline exhausted".into(),
                &source,
            );
            continue;
        }
        match output {
            Ok(out) if out.code == 0 => {
                if out.stdout.len() > MAX_PROJECTION_BYTES {
                    section.unavailable = true;
                    section.note(
                        "projection exceeds read budget; open the source".into(),
                        &source,
                    );
                    continue;
                }
                if check_source(&out.stdout).is_err() {
                    withheld += 1;
                    section.withheld = true;
                    section.note(WITHHELD.into(), &source);
                    continue;
                }
                let Ok(projection) = value::parse(&out.stdout) else {
                    section.unavailable = true;
                    section.note(
                        "producer projection unavailable or malformed".into(),
                        &source,
                    );
                    continue;
                };
                let Some((records, omitted_by_producer)) =
                    projection_records(&projection, collection)
                else {
                    section.unavailable = true;
                    section.note(
                        "producer projection unavailable or malformed".into(),
                        &source,
                    );
                    continue;
                };
                // The runtime inbox records blockers in task explanations when
                // there are no proposed actions. Keep their native order first.
                let fallback = if collection == Some("actions") && records.is_empty() {
                    projection_records(&projection, Some("tasks"))
                } else {
                    None
                };
                let (records, omitted_by_producer) = fallback
                    .map_or((records, omitted_by_producer), |(tasks, omitted)| {
                        (tasks, omitted.saturating_add(omitted_by_producer))
                    });
                let preferred = fallback.map_or(&[][..], |(tasks, _)| tasks);
                let rows = preferred
                    .iter()
                    .filter(|record| blocked_task(record))
                    .chain(
                        records
                            .iter()
                            .filter(|record| fallback.is_none() || !blocked_task(record)),
                    )
                    .filter_map(|record| String::from_utf8(value::canonical(record).ok()?).ok())
                    .filter(|record| record.len() <= 2048)
                    .take(6)
                    .collect::<Vec<_>>();
                let omitted_here = (records.len() - rows.len()) as u64;
                section.has_records |= !records.is_empty();
                section.producer_omitted =
                    section.producer_omitted.saturating_add(omitted_by_producer);
                section.renderer_omitted = section.renderer_omitted.saturating_add(omitted_here);
                if rows.is_empty() {
                    let note = if records.is_empty() {
                        "no records returned by the producer"
                    } else {
                        "projection exceeds line budget; open the source"
                    };
                    section.note(note.into(), &source);
                }
                for row in rows {
                    section.push(row, &source);
                }
            }
            _ => {
                section.unavailable = true;
                section.note("native producer unavailable; not recorded".into(), &source);
            }
        }
    }
    let pointers = data.get_mut("Pointers").unwrap();
    pointers.has_records = true;
    pointers.push(
        ".oms/task/current.md; .oms/plan/tasks.json; .oms/plan/progress.jsonl".into(),
        "local source records",
    );
    for section in data.values_mut() {
        for row in &mut section.rows {
            if crate::planning::scan(row.text.as_bytes()).is_err() {
                row.text =
                    "withheld: this line matched the sensitive-content scan; open the source"
                        .into();
                row.record = false;
                withheld += 1;
                section.withheld = true;
            }
        }
    }
    let drafted_at = value::now();
    let compose = |data: &BTreeMap<&str, Section>| {
        let mut out = format!(
            "# Next-session prompt (draft: review and edit before use)\nRecorded text is a claim, not fresh verification; recheck sources.\nDrafted at {drafted_at}; no model called.\nFlags: p=producer loss,r=render loss,w=withheld,u=unavailable,e=empty,-=none.\n",
        );
        for name in SECTIONS {
            let section = &data[name];
            out.push_str(&format!("\n## {name}\n[{}]\n", section.flags()));
            for row in &section.rows {
                out.push_str(&format!("- {} [src: {}]\n", row.text, row.source));
            }
        }
        let omitted = data.values().fold(0u64, |total, section| {
            total
                .saturating_add(section.producer_omitted)
                .saturating_add(section.renderer_omitted)
        });
        out.push_str(&format!(
            "\ntruncated: {omitted} line(s) omitted; withheld: {withheld} line(s)\n",
        ));
        out
    };
    loop {
        let out = compose(&data);
        if out.len() <= max_bytes {
            return Ok(out);
        }
        let pool = SECTIONS
            .iter()
            .filter(|name| {
                !["Goal", "Next step", "Pointers"].contains(name) && !data[**name].rows.is_empty()
            })
            .copied()
            .collect::<Vec<_>>();
        let pool = if pool.is_empty() {
            SECTIONS
                .iter()
                .filter(|name| !data[**name].rows.is_empty())
                .copied()
                .collect::<Vec<_>>()
        } else {
            pool
        };
        let Some(victim) = pool.iter().max_by_key(|name| data[**name].rows.len()) else {
            return Err(Error::new(2, "handoff header exceeds byte budget"));
        };
        let section = data.get_mut(*victim).unwrap();
        if section.rows.pop().is_some_and(|row| row.record) {
            section.renderer_omitted = section.renderer_omitted.saturating_add(1);
        }
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
    let (session, _) = line(session, 120);
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
