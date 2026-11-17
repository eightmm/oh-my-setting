//! Durable observations from GitHub. These records are untrusted evidence;
//! neither their text nor their classification grants execution authority.
use crate::{Context, Error, Result, V, ci, digest, hex, now, obj, require, safe, storage, string};
use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool, time::Duration};

pub const EVENTS: &str = ".oms/feedback/events.jsonl";
const MAX_EVENTS: usize = 20_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_COMMENTS: usize = 50;

fn commit(value: &str) -> bool {
    hex(value, 40) || hex(value, 64)
}
fn timestamp(value: &str) -> bool {
    value.len() == 20
        && value.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            19 => b == b'Z',
            _ => b.is_ascii_digit(),
        })
}
fn bounded(value: &V, field: &str, max: usize) -> String {
    oms_contract_markdown::bounded_line(&string(value, field), max)
}
fn number(value: &V) -> V {
    if value.integer(true) {
        value.clone()
    } else {
        V::Null
    }
}
fn canonical(value: &V) -> Result<Vec<u8>> {
    Ok(oms_command_plan::json::canonical(
        &oms_command_plan::json::parse(&value.json(false)?)?,
    )?)
}
fn identity(row: &V) -> V {
    obj([
        ("repo", row.get("repo").clone()),
        ("pr", row.get("pr").clone()),
        ("head_sha", row.get("head_sha").clone()),
        ("source", row.get("source").clone()),
        ("source_id", row.get("source_id").clone()),
        ("source_revision", row.get("source_revision").clone()),
        ("evidence", row.get("evidence").clone()),
    ])
}
fn finish(mut row: V) -> Result<V> {
    row.set("event_id", digest(&canonical(&identity(&row))?));
    row.set("schema", 1usize);
    row.set("kind", "oms-external-feedback");
    row.set("trust", "untrusted");
    row.set("observation_id", digest(&canonical(&row)?));
    row.set("observed_at", now());
    require(
        row.json(false)?.len() <= 32 * 1024,
        "feedback observation exceeds bound",
    )?;
    Ok(row)
}

fn validate(row: &V) -> Result<()> {
    let required = [
        "schema",
        "kind",
        "trust",
        "repo",
        "pr",
        "branch",
        "head_sha",
        "observed_head",
        "source",
        "source_id",
        "source_revision",
        "classification",
        "evidence",
        "event_id",
        "observation_id",
        "observed_at",
    ];
    let fields = match row {
        V::Obj(fields) => fields,
        _ => return Err(Error::new(2, "invalid feedback journal record")),
    };
    require(
        required.iter().all(|key| row.has(key))
            && fields.len() == required.len() + usize::from(row.has("diagnostic")),
        "invalid feedback journal schema",
    )?;
    let slug = string(row, "repo");
    require(
        crate::report::github_remote(&format!("https://github.com/{slug}"))? == slug
            && row.get("schema") == &1usize.into()
            && string(row, "kind") == "oms-external-feedback"
            && string(row, "trust") == "untrusted"
            && (row.get("pr") == &V::Null || row.get("pr").integer(true))
            && (row.get("source_id") == &V::Null || row.get("source_id").integer(true))
            && ["head_sha", "observed_head"].iter().all(|key| {
                row.get(key)
                    .text()
                    .is_some_and(|s| s.is_empty() || commit(s))
            })
            && row
                .get("branch")
                .text()
                .is_some_and(|s| s.chars().count() <= 300)
            && timestamp(&string(row, "observed_at"))
            && [
                "unknown",
                "pending",
                "success",
                "cancelled",
                "environment_failure",
                "stale_head",
                "review_required",
                "actionable_failure",
                "base_failure",
            ]
            .contains(&string(row, "classification").as_str())
            && matches!(row.get("evidence"), V::Obj(_)),
        "invalid feedback journal record",
    )?;
    let evidence = row.get("evidence");
    let fields = |keys: &[&str]| matches!(evidence, V::Obj(map) if map.len() == keys.len() && keys.iter().all(|key| evidence.has(key)));
    let text = |key, max| {
        evidence
            .get(key)
            .text()
            .is_some_and(|s| s.chars().count() <= max)
    };
    require(
        matches!(evidence.get("withheld"), V::Bool(_)),
        "invalid feedback withholding evidence",
    )?;
    match string(row, "source").as_str() {
        "ci-run" => require(
            fields(&[
                "status",
                "conclusion",
                "workflow",
                "workflow_sha256",
                "branch_sha256",
                "withheld",
            ]) && text("status", 32)
                && text("conclusion", 32)
                && text("workflow", 160)
                && hex(&string(evidence, "workflow_sha256"), 64)
                && hex(&string(evidence, "branch_sha256"), 64)
                && (row.get("source_revision") == &V::Null
                    || row.get("source_revision").integer(true)),
            "invalid feedback run attempt",
        )?,
        "pr-review-comment" => require(
            fields(&[
                "path",
                "line",
                "author",
                "body",
                "content_sha256",
                "withheld",
            ]) && text("path", 300)
                && text("author", 80)
                && text("body", 600)
                && hex(&string(evidence, "content_sha256"), 64)
                && (evidence.get("line") == &V::Null || evidence.get("line").integer(true))
                && row
                    .get("source_revision")
                    .text()
                    .is_some_and(|s| s.is_empty() || timestamp(s)),
            "invalid feedback comment revision",
        )?,
        _ => return Err(Error::new(2, "invalid feedback source")),
    }
    require(
        string(row, "event_id") == digest(&canonical(&identity(row))?),
        "feedback event digest mismatch",
    )?;
    let mut projection = row.clone();
    if let V::Obj(fields) = &mut projection {
        fields.remove(b"observation_id".as_slice());
        fields.remove(b"observed_at".as_slice());
    }
    require(
        string(row, "observation_id") == digest(&canonical(&projection)?),
        "feedback observation digest mismatch",
    )?;
    require(
        row.json(false)?.len() <= 32 * 1024,
        "feedback observation exceeds bound",
    )?;
    safe(&row.json(false)?)
}

/// Workflow run IDs and attempts come from `gh run list`'s databaseId/attempt
/// fields (REST id/run_attempt). A missing identity remains observable but
/// cannot qualify as a repair candidate.
pub(crate) fn ci_event(repo: &str, branch: &str, head: &str, pr: &V, run: &V) -> Result<V> {
    let sha = string(run, "headSha");
    let enum_field = |field| {
        let value = bounded(run, field, 32);
        if value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            value
        } else {
            "unknown".into()
        }
    };
    let status = enum_field("status");
    let conclusion = enum_field("conclusion");
    let workflow = string(run, "workflowName");
    let withheld = safe(workflow.as_bytes()).is_err() || safe(branch.as_bytes()).is_err();
    let id = number(run.get("databaseId"));
    let attempt = number(run.get("attempt"));
    let known = commit(&sha) && id != V::Null && attempt != V::Null && !withheld;
    let class = if !known || !commit(head) {
        "unknown"
    } else if sha != head {
        "stale_head"
    } else if status != "completed" {
        "pending"
    } else {
        match conclusion.as_str() {
            "success" | "neutral" | "skipped" => "success",
            "cancelled" => "cancelled",
            "startup_failure" | "action_required" => "environment_failure",
            // A failed/timed-out job alone does not establish a code regression.
            _ => "unknown",
        }
    };
    let evidence = obj([
        ("status", status.into()),
        ("conclusion", conclusion.into()),
        (
            "workflow",
            if withheld {
                String::new()
            } else {
                bounded(run, "workflowName", 160)
            }
            .into(),
        ),
        ("workflow_sha256", digest(workflow.as_bytes()).into()),
        ("branch_sha256", digest(branch.as_bytes()).into()),
        ("withheld", withheld.into()),
    ]);
    finish(obj([
        ("repo", repo.into()),
        ("pr", number(pr)),
        (
            "branch",
            if withheld {
                String::new()
            } else {
                oms_contract_markdown::bounded_line(branch, 300)
            }
            .into(),
        ),
        (
            "head_sha",
            if commit(&sha) { sha } else { String::new() }.into(),
        ),
        ("observed_head", head.into()),
        ("source", "ci-run".into()),
        ("source_id", id),
        ("source_revision", attempt),
        ("classification", class.into()),
        ("evidence", evidence),
    ]))
}

pub(crate) fn comment_event(repo: &str, pr: &V, head: &str, item: &V) -> Result<V> {
    let sha = string(item, "commit_id");
    let body = bounded(item, "body", 600);
    let path = bounded(item, "path", 300);
    let author = bounded(item.get("user"), "login", 80);
    let withheld = safe(body.as_bytes()).is_err()
        || safe(path.as_bytes()).is_err()
        || safe(author.as_bytes()).is_err();
    let id = number(item.get("id"));
    let updated = bounded(item, "updated_at", 40);
    let updated = if timestamp(&updated) {
        updated
    } else {
        String::new()
    };
    // Hash the original body, not its displayed prefix: an edit beyond the
    // bounded excerpt must still produce a distinct observation.
    let content_sha256 = digest(string(item, "body").as_bytes());
    let known = id != V::Null && !updated.is_empty() && commit(&sha) && commit(head);
    let class = if !known || withheld {
        "unknown"
    } else if sha != head {
        "stale_head"
    } else {
        "review_required"
    };
    finish(obj([
        ("repo", repo.into()),
        ("pr", number(pr)),
        ("branch", "".into()),
        (
            "head_sha",
            if commit(&sha) { sha } else { String::new() }.into(),
        ),
        ("observed_head", head.into()),
        ("source", "pr-review-comment".into()),
        ("source_id", id),
        ("source_revision", updated.into()),
        ("classification", class.into()),
        (
            "evidence",
            obj([
                ("path", if withheld { String::new() } else { path }.into()),
                ("line", number(item.get("line"))),
                (
                    "author",
                    if withheld { String::new() } else { author }.into(),
                ),
                ("body", if withheld { String::new() } else { body }.into()),
                ("content_sha256", content_sha256.into()),
                ("withheld", withheld.into()),
            ]),
        ),
    ]))
}

pub fn observations(state: &Path) -> Result<Vec<V>> {
    let bytes = storage::read(state, Path::new(EVENTS), true)?;
    require(bytes.len() <= MAX_BYTES, "feedback journal exceeds bound")?;
    let rows = storage::parse_rows(&bytes)?;
    require(
        rows.len() <= MAX_EVENTS,
        "feedback journal exceeds event bound",
    )?;
    let mut latest = BTreeMap::new();
    for (index, row) in rows.into_iter().enumerate() {
        validate(&row)?;
        latest.insert(string(&row, "event_id"), (index, row));
    }
    let mut latest = latest.into_values().collect::<Vec<_>>();
    latest.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
    Ok(latest.into_iter().map(|(_, row)| row).collect())
}

fn append(state: &Path, rows: &[V], interrupted: &AtomicBool) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    storage::ensure_ignore(state, interrupted)?;
    let path = Path::new(EVENTS);
    let _guard = storage::lock(&state.join(path), interrupted)?;
    let raw = storage::read(state, path, true)?;
    require(raw.len() <= MAX_BYTES, "feedback journal exceeds bound")?;
    let previous = storage::parse_rows(&raw)?;
    for row in previous.iter().chain(rows) {
        validate(row)?;
    }
    let mut next = raw.clone();
    let mut identities = previous
        .iter()
        .map(|r| string(r, "observation_id"))
        .collect::<std::collections::BTreeSet<_>>();
    let mut added = 0;
    for row in rows {
        if identities.insert(string(row, "observation_id")) {
            next.extend(row.json(false)?);
            next.push(b'\n');
            added += 1;
        }
    }
    require(
        next.len() <= MAX_BYTES && previous.len() + added <= MAX_EVENTS,
        "feedback journal is full; retain evidence before starting a new journal",
    )?;
    if added > 0 {
        storage::replace(
            state,
            path,
            if state.join(path).try_exists()? {
                Some(&raw)
            } else {
                None
            },
            &next,
        )?;
    }
    Ok(added)
}

pub(crate) fn record_comments(
    c: &Context<'_>,
    state: &Path,
    repo: &str,
    pr: &V,
    head: &str,
    listed: &V,
) -> Result<usize> {
    require(matches!(listed, V::Arr(_)), "review comments unavailable")?;
    let rows = listed
        .array()
        .iter()
        .take(MAX_COMMENTS)
        .map(|item| comment_event(repo, pr, head, item))
        .collect::<Result<Vec<_>>>()?;
    append(state, &rows, c.interrupted)
}

/// Optional observation failures must not replace an ordinary CI/comment view.
/// Raw storage and subprocess errors can contain private paths or remote text.
pub(crate) fn optional_result(result: Result<usize>) -> Result<(V, Vec<u8>)> {
    match result {
        Ok(added) => Ok((
            obj([("status", "recorded".into()), ("added", added.into())]),
            vec![],
        )),
        Err(error) if error.code == 130 => Err(error),
        Err(error) => Ok((
            obj([
                ("status", "unavailable".into()),
                ("error_code", i64::from(error.code).into()),
            ]),
            format!(
                "warning: feedback observation unavailable (code {}); durable feedback may be incomplete or stale\n",
                error.code
            )
            .into_bytes(),
        )),
    }
}

pub(crate) fn api(c: &Context<'_>, endpoint: &str, wall: Duration) -> Result<V> {
    let value = ci::gh(
        c,
        &[
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            endpoint,
        ],
        wall,
    )?;
    require(
        value.json(false)?.len() <= 1024 * 1024,
        "feedback API response exceeds bound",
    )?;
    Ok(value)
}

/// Invoked only from the existing bounded record/tick producer. Ambiguous or
/// unreadable PR association never invents a number or widens the destination.
pub(crate) fn poll(
    c: &Context<'_>,
    state: &Path,
    git: &crate::git::Git<'_>,
    branch: &str,
    head: &str,
    runs: &V,
    wall: Duration,
) -> Result<usize> {
    let Ok(remote) = git.text(&["remote", "get-url", "origin"]) else {
        return Ok(0);
    };
    let Ok(slug) = crate::report::github_remote(&remote) else {
        return Ok(0);
    };
    let pulls = ci::gh(
        c,
        &[
            "pr",
            "list",
            "--repo",
            &slug,
            "--head",
            branch,
            "--state",
            "open",
            "--limit",
            "2",
            "--json",
            "number,headRefOid",
        ],
        wall,
    );
    let pull = pulls
        .as_ref()
        .ok()
        .and_then(|p| (matches!(p, V::Arr(_)) && p.array().len() == 1).then(|| &p.array()[0]));
    let number = pull.map(|p| number(p.get("number"))).unwrap_or(V::Null);
    let observed_head = pull
        .map(|p| string(p, "headRefOid"))
        .filter(|s| commit(s))
        .unwrap_or_else(|| head.to_owned());
    let rows = runs
        .array()
        .iter()
        .take(10)
        .map(|run| ci_event(&slug, branch, &observed_head, &number, run))
        .collect::<Result<Vec<_>>>()?;
    let mut added = append(state, &rows, c.interrupted)?;
    // Preserve available CI evidence even when PR association cannot be read,
    // but report the incomplete poll instead of claiming a successful refresh.
    require(matches!(pulls?, V::Arr(_)), "review PR lookup unavailable")?;
    if number != V::Null {
        let listed = api(
            c,
            &format!(
                "repos/{slug}/pulls/{}/comments?sort=updated&direction=desc&per_page=50",
                number.py()
            ),
            wall,
        )?;
        added += record_comments(c, state, &slug, &number, &observed_head, &listed)?;
    }
    Ok(added)
}
