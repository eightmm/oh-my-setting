//! Effective contract projection. Persisted envelopes are never authority inputs.
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, error, require};
use oms_contract_markdown as md;
use std::collections::{BTreeMap, BTreeSet};
fn scope(s: md::Scope) -> Value {
    object([
        ("allowed", list(s.allowed.iter().map(String::as_str))),
        ("forbidden", list(s.forbidden.iter().map(String::as_str))),
    ])
}
fn absent() -> Value {
    object([
        ("present", b(false)),
        ("source", Value::Null),
        ("criteria", a([])),
        ("scope", object([("allowed", a([])), ("forbidden", a([]))])),
    ])
}
fn metadata(m: BTreeMap<String, String>) -> Value {
    Value::Object(
        m.into_iter()
            .map(|(k, v)| (k.into_bytes(), string(v)))
            .collect(),
    )
}
fn statement(s: &str) -> String {
    let s = s.trim();
    let s = if s.starts_with("[ ]") || s.starts_with("[x]") || s.starts_with("[X]") {
        &s[3..]
    } else {
        s
    };
    md::strip_criterion_marker(&md::bounded_line(s, usize::MAX))
}
pub fn project(rt: &Runtime<'_>) -> Result<Value> {
    match std::fs::symlink_metadata(rt.repo.join("PROJECT.md")) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => return Ok(absent()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(absent()),
        Err(e) => return Err(error(e)),
        _ => {}
    }
    let Some(text) = rt.text("PROJECT.md")? else {
        return Ok(absent());
    };
    let (body, meta) = md::sections(&text);
    let mut goal = md::first_nonempty(
        &md::section_text(&body, &["goal", "objective", "purpose", "목표", "목적"]),
        1000,
    );
    if goal.is_empty() {
        goal = md::first_nonempty(
            &body
                .iter()
                .find(|(k, _)| k == "__preamble__")
                .map(|(_, v)| v.join("\n"))
                .unwrap_or_default(),
            1000,
        )
    }
    let ct = md::section_text(&body, &["constraint", "scope", "제약", "범위"]);
    let mut constraints = md::bullet_items(&ct, false);
    if constraints.is_empty() {
        constraints = ct.lines().map(str::to_owned).collect()
    }
    let state = meta
        .get("status")
        .cloned()
        .unwrap_or_else(|| md::first_nonempty(&md::section_text(&body, &["status", "상태"]), 80));
    Ok(object([
        ("present", b(true)),
        ("source", rt.descriptor("PROJECT.md")?),
        ("metadata", metadata(meta)),
        ("status", string(state)),
        ("goal", string(goal)),
        (
            "criteria",
            list(
                md::bullet_items(
                    &md::section_text(
                        &body,
                        &[
                            "acceptance",
                            "done",
                            "success",
                            "completion",
                            "완료",
                            "성공",
                            "수용",
                        ],
                    ),
                    true,
                )
                .iter()
                .map(String::as_str),
            ),
        ),
        (
            "constraints",
            list(
                constraints
                    .iter()
                    .map(|s| md::normalized_line(s))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str),
            ),
        ),
        (
            "scope",
            scope(md::parse_scope(constraints.iter().map(String::as_str))),
        ),
    ]))
}
pub fn task(rt: &Runtime<'_>) -> Result<Value> {
    let path = ".oms/task/current.md";
    let Some(raw) = rt.bytes(path, crate::storage::MAX_STATE)? else {
        return Ok(absent());
    };
    let text = std::str::from_utf8(&raw).map_err(error)?;
    let (body, meta) = md::sections(text);
    let status = from_native(
        &oms_work_inspect::task_status_snapshot(&rt.services.task_status(
            &rt.repo,
            &rt.state_repo,
            &raw,
        )?)
        .map_err(error)?,
    );
    let ct = md::section_text(&body, &["constraint", "제약"]);
    let mut constraints = md::bullet_items(&ct, false);
    if constraints.is_empty() {
        constraints = ct.lines().map(str::to_owned).collect()
    }
    let verify = md::section_text_exact(&body, &["verify", "검증 명령"]);
    let mut budget = empty();
    let re =
        regex::Regex::new(r"^\s*[-*]?\s*([A-Za-z][A-Za-z0-9_.-]*):\s*(.+?)\s*$").map_err(error)?;
    for line in md::section_text_exact(&body, &["loop state", "budget", "예산"]).lines() {
        if let Some(c) = re.captures(line) {
            let v = &c[2];
            budget.set(
                &c[1],
                if v.bytes().all(|b| b.is_ascii_digit()) {
                    Value::Number(v.into())
                } else {
                    string(bounded(v, 160))
                },
            )?;
        }
    }
    Ok(object([
        ("present", b(true)),
        ("source", rt.descriptor(path)?),
        ("metadata", metadata(meta.clone())),
        (
            "task_id",
            string(bounded(
                if status.opt("task_id").is_some() {
                    status.text("task_id")
                } else {
                    meta.get("task_id").map(String::as_str).unwrap_or("")
                },
                160,
            )),
        ),
        (
            "status",
            string(bounded(status.text("status"), 40).to_lowercase()),
        ),
        (
            "goal",
            string(md::first_nonempty(
                &md::section_text(&body, &["goal", "objective", "목표"]),
                1000,
            )),
        ),
        (
            "criteria",
            list(
                md::bullet_items(
                    &md::section_text(
                        &body,
                        &["done criteria", "acceptance", "completion", "완료", "성공"],
                    ),
                    true,
                )
                .iter()
                .map(String::as_str),
            ),
        ),
        ("verify_present", b(!verify.trim().is_empty())),
        (
            "verify_digest",
            string(if verify.trim().is_empty() {
                String::new()
            } else {
                sha(verify.trim())
            }),
        ),
        (
            "verification",
            string(
                if matches!(status.text("verification"), "fresh" | "stale" | "none") {
                    status.text("verification")
                } else {
                    "unknown"
                },
            ),
        ),
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
            scope(md::parse_scope(constraints.iter().map(String::as_str))),
        ),
        ("budget", budget),
        ("stale", b(status.get("stale").truth())),
    ]))
}
fn paths(v: &Value) -> Vec<String> {
    if let Ok(a) = v.strings() {
        md::parse_paths(a)
    } else {
        md::parse_path_list(text(v))
    }
}
pub fn plan(rt: &Runtime<'_>) -> Result<Value> {
    let path = ".oms/plan/tasks.json";
    let Some(raw) = rt.bytes(path, crate::storage::MAX_STATE)? else {
        let retirement = rt
            .rows(".oms/plan/retirements.jsonl")?
            .into_iter()
            .filter(|r| r.get("schema").is_int(1) && r.text("kind") == "plan-retirement")
            .last();
        let mut out = absent();
        out.set("tasks", a([]))?;
        for k in [
            "task_count",
            "verified_count",
            "cancelled_count",
            "closed_count",
            "unfinished_count",
        ] {
            out.set(k, number(0))?
        }
        for k in ["nonempty", "all_done", "all_closed", "has_unfinished"] {
            out.set(k, b(false))?
        }
        out.set("counts", empty())?;
        out.set(
            "latest_retirement",
            retirement
                .as_ref()
                .map(|r| {
                    pick(
                        r,
                        &[
                            "ts",
                            "retirement_id",
                            "plan_sha256",
                            "plan_id",
                            "disposition",
                            "reason",
                            "head",
                            "git_ref",
                            "archive",
                            "acceptance_verified",
                            "task_landings_claimed",
                        ],
                    )
                })
                .unwrap_or_else(empty),
        )?;
        if retirement.is_some() {
            out.set("source", rt.descriptor(".oms/plan/retirements.jsonl")?)?
        }
        return Ok(out);
    };
    let raw = parse(&raw)?;
    raw.object()?;
    let source_tasks = match raw.get("tasks") {
        Value::Object(v) => v
            .iter()
            .filter_map(|(id, v)| {
                let mut v = v.clone();
                if v.object().is_err() {
                    return None;
                }
                if v.opt("id").is_none() {
                    v.set("id", Value::String(id.clone())).ok()?
                }
                Some(v)
            })
            .collect(),
        Value::Array(v) => v.iter().filter(|v| v.object().is_ok()).cloned().collect(),
        _ => vec![],
    };
    let mut tasks = Vec::new();
    let mut counts = BTreeMap::new();
    let mut allowed = BTreeSet::new();
    let mut forbidden = BTreeSet::new();
    for row in source_tasks {
        let ap = paths(row.opt("allowed").unwrap_or(row.get("allowed_paths")));
        let fp = paths(row.opt("forbidden").unwrap_or(row.get("forbidden_paths")));
        allowed.extend(ap.clone());
        forbidden.extend(fp.clone());
        let title = row.opt("title").unwrap_or(row.get("goal"));
        let title = text(title);
        let stmt = statement(title);
        let stmt = if stmt.is_empty() {
            format!("Plan task {} is admitted.", bounded(row.text("id"), 160))
        } else {
            stmt
        };
        let state = bounded(
            if row.opt("state").is_some() {
                row.text("state")
            } else {
                "ready"
            },
            40,
        )
        .to_lowercase();
        *counts.entry(state.clone()).or_insert(0usize) += 1;
        let verify = row.text("verify");
        tasks.push(object([
            ("id", string(bounded(row.text("id"), 160))),
            ("title", string(bounded(title, 300))),
            (
                "criterion_digest",
                string(sha(format!("plan-task\0{stmt}"))),
            ),
            ("state", string(state)),
            (
                "depends",
                a(rows(row.opt("depends").unwrap_or(row.get("dependencies")))
                    .iter()
                    .map(|v| string(bounded(text(v), 160)))),
            ),
            (
                "scope",
                object([
                    ("allowed", list(ap.iter().map(String::as_str))),
                    ("forbidden", list(fp.iter().map(String::as_str))),
                ]),
            ),
            ("verify_present", b(!verify.is_empty())),
            (
                "verify_digest",
                string(if verify.is_empty() {
                    String::new()
                } else {
                    sha(verify)
                }),
            ),
            ("provider", string(bounded(row.text("provider"), 80))),
            ("lease_id_present", b(row.get("lease_id").truth())),
            ("artifact_present", b(row.get("artifact").truth())),
            ("patch_present", b(row.get("patch").truth())),
        ]));
    }
    let ids = tasks.iter().map(|r| r.text("id")).collect::<Vec<_>>();
    let raw_bytes = rt.bytes(path, crate::storage::MAX_STATE)?.unwrap();
    let st = from_native(
        &oms_work_inspect::plan_status_snapshot(
            &rt.services
                .plan_status(&rt.repo, &rt.state_repo, &raw_bytes)?,
            &ids,
        )
        .map_err(error)?,
    );
    let accept = raw.opt("accept").unwrap_or(raw.get("acceptance"));
    let accept = text(accept);
    let mut out = pick(
        &st,
        &[
            "task_count",
            "nonempty",
            "all_done",
            "all_closed",
            "has_unfinished",
            "verified_count",
            "cancelled_count",
            "closed_count",
            "unfinished_count",
            "actionable",
            "contract",
        ],
    );
    for (k, v) in [
        ("present", b(true)),
        ("source", rt.descriptor(path)?),
        (
            "plan_id",
            string(
                if regex::Regex::new(r"^plan_[0-9a-f]{32}$")
                    .unwrap()
                    .is_match(raw.text("plan_id"))
                {
                    raw.text("plan_id")
                } else {
                    ""
                },
            ),
        ),
        ("goal", string(bounded(raw.text("goal"), 1000))),
        ("acceptance_present", b(!accept.is_empty())),
        (
            "acceptance_digest",
            string(if accept.is_empty() {
                String::new()
            } else {
                sha(accept)
            }),
        ),
        ("tasks", a(tasks)),
        (
            "counts",
            Value::Object(
                counts
                    .into_iter()
                    .map(|(k, n)| (k.into_bytes(), number(n)))
                    .collect(),
            ),
        ),
        (
            "scope",
            object([
                ("allowed", list(allowed.iter().map(String::as_str))),
                ("forbidden", list(forbidden.iter().map(String::as_str))),
            ]),
        ),
    ] {
        out.set(k, v)?
    }
    Ok(out)
}
pub fn criteria(project: &Value, task: &Value, plan: &Value) -> Result<Value> {
    let mut out = Vec::new();
    let mut seen = BTreeMap::<String, String>::new();
    fn claim(seen: &mut BTreeMap<String, String>, id: &str, stmt: &str) -> Result<bool> {
        if let Some(old) = seen.get(id) {
            require(
                old == stmt,
                format!(
                    "criterion id {id} names two different criteria; give each acceptance criterion its own [id:...]"
                ),
            )?;
            Ok(false)
        } else {
            seen.insert(id.into(), stmt.into());
            Ok(true)
        }
    }
    for (source, layer) in [("project", project), ("task", task)] {
        for raw in rows(layer.get("criteria")) {
            let raw = text(raw);
            let stmt = statement(raw);
            if stmt.is_empty() {
                continue;
            }
            let id = md::stable_criterion_id(source, &md::normalized_line(raw));
            if !claim(&mut seen, &id, &stmt.to_lowercase())? {
                continue;
            }
            let mut item = object([
                ("id", string(id)),
                ("text", string(bounded(&stmt, 500))),
                ("criterion_digest", string(sha(format!("{source}\0{stmt}")))),
                ("source", string(source)),
                ("weight", number(2)),
            ]);
            if source == "task" {
                item.set("active_task_id", string(bounded(task.text("task_id"), 160)))?
            }
            out.push(item)
        }
    }
    if plan.get("acceptance_present").truth() {
        let digest = plan.text("acceptance_digest");
        out.push(object([
            (
                "id",
                string(format!(
                    "criterion-plan-acceptance-{}",
                    &digest[..digest.len().min(10)]
                )),
            ),
            (
                "text",
                string("The plan-level acceptance command passes on the final tree."),
            ),
            ("criterion_digest", string(sha(format!("plan\0{digest}")))),
            ("source", string("plan")),
            ("weight", number(3)),
            ("command_digest", string(digest)),
        ]));
    }
    let re = regex::Regex::new(r"(?i)\[(?:id|criterion):\s*([A-Za-z0-9._:-]{1,80})\]").unwrap();
    for row in rows(plan.get("tasks")) {
        let task_id = row.text("id");
        if task_id.is_empty() {
            continue;
        }
        let title = row.text("title");
        let id = re
            .captures(title)
            .map(|c| c[1].to_owned())
            .unwrap_or_else(|| format!("plan-task-{task_id}"));
        if !claim(&mut seen, &id, &task_id.to_lowercase())? {
            continue;
        }
        let stmt = md::strip_criterion_marker(title);
        out.push(object([
            ("id", string(id)),
            (
                "text",
                string(bounded(
                    &if stmt.is_empty() {
                        format!("Plan task {task_id} is admitted.")
                    } else {
                        stmt
                    },
                    500,
                )),
            ),
            ("criterion_digest", row.get("criterion_digest").clone()),
            ("source", string("plan-task")),
            ("weight", number(1)),
            ("plan_task_id", string(task_id)),
            ("plan_id", string(plan.text("plan_id"))),
        ]));
    }
    Ok(a(out))
}
pub fn failures(rt: &Runtime<'_>) -> Result<Value> {
    let mut canonical = Vec::new();
    if let Some(raw) = rt.bytes(".oms/failures.jsonl", crate::storage::MAX_STREAM)? {
        let payload = parse(
            &rt.services
                .unresolved_failures(&rt.repo, &rt.state_repo, &raw)?,
        )?;
        require(
            payload.get("schema").is_int(1),
            "canonical failure ledger projection is unavailable or invalid",
        )?;
        for row in payload.get("failures").array()? {
            require(
                matches!(
                    row.text("attention"),
                    "none" | "retiring" | "stale" | "actionable"
                ) && matches!(row.get("actionable"), Value::Bool(_))
                    && matches!(row.get("retiring"), Value::Bool(_)),
                "canonical failure ledger projection contains invalid attention state",
            )?;
            if row.text("attention") == "retiring" || row.get("actionable") == &b(false) {
                continue;
            }
            let summary = bounded(
                if !row.text("summary").is_empty() {
                    row.text("summary")
                } else {
                    row.text("cmd")
                },
                300,
            );
            canonical.push(failure_item(
                row,
                &summary,
                ".oms/failures.jsonl",
                "fingerprint",
                "",
            )?);
        }
    }
    let mut paths =
        std::collections::BTreeSet::from([std::path::PathBuf::from(".oms/fail-ledger.jsonl")]);
    let mut pending = vec![
        std::path::PathBuf::from(".oms/failures"),
        std::path::PathBuf::from(".oms/failure"),
    ];
    while let Some(directory) = pending.pop() {
        let actual = rt.state_repo.join(&directory);
        let info = match std::fs::symlink_metadata(&actual) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(error(e)),
        };
        if info.file_type().is_symlink() || !info.is_dir() {
            continue;
        }
        for name in rt
            .store
            .names(&directory, crate::storage::MAX_ROWS, false)?
        {
            let path = directory.join(name);
            let info = std::fs::symlink_metadata(rt.state_repo.join(&path)).map_err(error)?;
            if info.file_type().is_symlink() {
                continue;
            }
            if info.is_dir() {
                pending.push(path)
            } else if info.is_file() && path.extension().is_some_and(|v| v == "jsonl") {
                paths.insert(path);
            }
        }
    }
    if rt
        .bytes(".oms/failures.jsonl", crate::storage::MAX_STREAM)?
        .is_some()
    {
        paths.insert(std::path::PathBuf::from(".oms/failures.jsonl"));
    }
    let mut result = Vec::new();
    for path in paths {
        if path == std::path::Path::new(".oms/failures.jsonl") {
            result.extend(std::mem::take(&mut canonical));
            continue;
        }
        let path = path
            .to_str()
            .ok_or_else(|| error("failure source path is not UTF-8"))?;
        for row in rt.rows(path)? {
            if matches!(
                text(row.opt("status").unwrap_or(row.get("state")))
                    .to_lowercase()
                    .as_str(),
                "resolved" | "closed" | "superseded" | "done"
            ) {
                continue;
            }
            let summary = bounded(
                text(
                    row.opt("summary")
                        .or(row.opt("reason"))
                        .unwrap_or(row.get("message")),
                ),
                300,
            );
            result.push(failure_item(
                &row,
                &summary,
                path,
                if row.opt("id").is_some() {
                    "id"
                } else {
                    "event_id"
                },
                text(row.opt("reason_code").unwrap_or(row.get("code"))),
            )?);
        }
    }
    if result.len() > 100 {
        result.drain(..result.len() - 100);
    }
    Ok(a(result))
}
fn failure_item(row: &Value, summary: &str, path: &str, id: &str, code: &str) -> Result<Value> {
    Ok(object([
        ("id", string(bounded(row.text(id), 160))),
        ("kind", string(bounded(row.text("kind"), 80))),
        ("summary", string(summary)),
        (
            "classification",
            from_encoded(
                &oms_failure::classify(
                    summary,
                    row.get("exit").int().ok().and_then(|n| n.try_into().ok()),
                    code,
                )
                .to_json_value(),
            )?,
        ),
        ("source", string(path)),
    ]))
}
pub fn base(rt: &Runtime<'_>) -> Result<Value> {
    let p = project(rt)?;
    let t = task(rt)?;
    let plan = plan(rt)?;
    let failures = failures(rt)?;
    let mut sources = vec![];
    let mut layers = vec![];
    let mut forbidden = BTreeSet::new();
    let mut allowed = a([]);
    let mut allowed_source = "unbounded";
    for (name, layer) in [("project", &p), ("task", &t), ("plan", &plan)] {
        if layer.get("source").truth() {
            sources.push(layer.get("source").clone())
        }
        let s = layer.get("scope");
        let ap = s.get("allowed").clone();
        if ap.truth() {
            allowed = ap.clone();
            allowed_source = name
        }
        for v in rows(s.get("forbidden")) {
            forbidden.insert(text(v).to_owned());
        }
        layers.push(object([
            ("source", string(name)),
            ("allowed", ap),
            ("forbidden", s.get("forbidden").clone()),
        ]));
    }
    let mut objective = object([("text", string("")), ("source", string("none"))]);
    for (name, layer) in [("task", &t), ("plan", &plan), ("project", &p)] {
        if !layer.text("goal").is_empty() {
            objective = object([
                ("text", string(bounded(layer.text("goal"), 1000))),
                ("source", string(name)),
            ]);
            break;
        }
    }
    let criteria = criteria(&p, &t, &plan)?;
    let mut warnings = vec![];
    if !criteria.truth() {
        warnings.push(
            "No explicit acceptance criteria were found; completion coverage cannot be proven."
                .into(),
        )
    }
    if plan.get("present").truth() && !plan.get("acceptance_present").truth() {
        warnings.push("The active plan has no plan-level acceptance command.".into())
    }
    if plan.get("present").truth() && !plan.get("nonempty").truth() {
        warnings.push(
            "The active plan has no tasks; completion is gated until plan work is defined.".into(),
        )
    }
    if failures.truth() {
        warnings.push(format!(
            "{} unresolved failure record(s) require attention.",
            rows(&failures).len()
        ))
    }
    let state_digest = hash(&object([
        ("head", string(&rt.repository.head)),
        ("sources", a(sources.clone())),
        ("failures", failures.clone()),
    ]))?;
    Ok(object([
        ("schema", number(2)),
        ("generated_at", string(&rt.now)),
        (
            "repo",
            object([
                ("head", optional(&rt.repository.head)),
                ("branch", optional(&rt.repository.branch)),
            ]),
        ),
        ("state_digest", string(state_digest)),
        ("sources", a(sources)),
        ("objective", objective),
        (
            "scope",
            object([
                ("allowed", allowed),
                ("allowed_source", string(allowed_source)),
                ("forbidden", list(forbidden.iter().map(String::as_str))),
                ("layers", a(layers)),
                ("authoritative", b(false)),
            ]),
        ),
        ("criteria", criteria),
        ("budget", t.opt("budget").cloned().unwrap_or_else(empty)),
        ("task", without(&t, &["criteria", "source", "constraints"])),
        ("plan", without(&plan, &["source"])),
        (
            "executor",
            object([
                ("present", b(false)),
                ("active", b(false)),
                ("retired", b(true)),
                ("frozen", b(false)),
                ("inspected", b(false)),
            ]),
        ),
        (
            "authority",
            object([
                ("repo_write", string("external_parent_decision")),
                ("remote_create", string("external_parent_decision")),
                ("authority_transferable_by_capsule", b(false)),
            ]),
        ),
        (
            "continuity",
            object([("latest_import", crate::capsule::latest_import(rt)?)]),
        ),
        ("failures", failures),
        ("warnings", list(warnings.iter().map(String::as_str))),
    ]))
}
pub fn finalize(mut base: Value, evidence: Value) -> Result<Value> {
    let raw = to_native(&base);
    let ev = to_native(&evidence);
    let completion = oms_work_inspect::envelope_completion(&raw, &ev).map_err(error)?;
    base.set("criteria", evidence.get("criteria").clone())?;
    base.set("evidence", without(&evidence, &["criteria"]))?;
    let mut task = base.get("task").clone();
    if let Some(state) = completion.task_completion {
        task.set("completion", string(state))?
    };
    base.set("task", task)?;
    base.set("next_actions", from_native(&completion.next_actions))?;
    let mut effective = base.get("evidence").clone();
    effective.set("complete", b(completion.evidence_complete))?;
    if let Some(blocker) = completion.completion_blocker {
        effective.set("completion_blocker", string(blocker))?
    }
    base.set("evidence", effective)?;
    let digest = hash(&object([
        ("contract", base.get("state_digest").clone()),
        ("evidence", base.get("evidence").clone()),
    ]))?;
    base.set("state_digest", string(&digest))?;
    let mut latest = base.get("continuity").get("latest_import").clone();
    if latest.get("present").truth() {
        let m = if latest.text("source_state_digest").is_empty() {
            Value::Null
        } else {
            b(latest.text("source_state_digest") == digest)
        };
        latest.set("state_digest_matches", m.clone())?;
        let state = if latest.text("status") == "invalid" {
            "invalid"
        } else if latest.get("head_matches") == &b(false) {
            "head-diverged"
        } else if m == b(false) {
            "state-diverged"
        } else if m == b(true) && latest.get("head_matches") == &b(true) {
            "current"
        } else {
            "unknown"
        };
        latest.set("status", string(state))?;
        base.set("continuity", object([("latest_import", latest)]))?;
    }
    Ok(base)
}
pub fn envelope(rt: &Runtime<'_>) -> Result<Value> {
    let base = base(rt)?;
    let coverage = crate::evidence::coverage(rt, &base)?;
    finalize(base, coverage)
}
