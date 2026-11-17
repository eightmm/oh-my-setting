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
    if number != V::Null {
        let listed = api(
            c,
            &format!(
                "repos/{slug}/pulls/{}/comments?sort=updated&direction=desc&per_page=50",
                number.py()
            ),
            wall,
        );
        if let Ok(listed) = listed {
            added += record_comments(c, state, &slug, &number, &observed_head, &listed)?;
        }
    }
    Ok(added)
}

fn fresh_pull(c: &Context<'_>, event: &V) -> Result<V> {
    let slug = string(event, "repo");
    let expected = string(event, "head_sha");
    require(
        event.get("pr").integer(true) && commit(&expected),
        "feedback PR/head identity is unknown",
    )?;
    let actual = crate::report::github_remote(
        &crate::git::Git::new(c.cwd, c.interrupted).text(&["remote", "get-url", "origin"])?,
    )?;
    require(
        slug.eq_ignore_ascii_case(&actual),
        "feedback repository changed",
    )?;
    require(
        crate::git::Git::new(c.cwd, c.interrupted).commit("HEAD")? == expected,
        "feedback local head moved",
    )?;
    let pull = api(
        c,
        &format!("repos/{slug}/pulls/{}", event.get("pr").py()),
        Duration::from_secs(20),
    )?;
    require(
        pull.get("number") == event.get("pr")
            && string(&pull, "state") == "open"
            && string(pull.get("base").get("repo"), "full_name").eq_ignore_ascii_case(&slug)
            && string(pull.get("head"), "sha") == expected,
        "feedback PR is closed, unavailable, mismatched or its head moved",
    )?;
    Ok(pull)
}

fn run_jobs(c: &Context<'_>, slug: &str, run: &V) -> Result<Vec<V>> {
    require(
        run.get("id").integer(true) && run.get("run_attempt").integer(true),
        "CI run attempt identity is unknown",
    )?;
    let jobs = api(
        c,
        &format!(
            "repos/{slug}/actions/runs/{}/attempts/{}/jobs?per_page=100",
            run.get("id").py(),
            run.get("run_attempt").py()
        ),
        Duration::from_secs(20),
    )?;
    require(
        matches!(jobs.get("jobs"), V::Arr(_))
            && jobs.get("jobs").array().len() <= 100
            && jobs.get("total_count").py() == jobs.get("jobs").array().len().to_string(),
        "CI job coverage is incomplete",
    )?;
    let mut names = std::collections::BTreeSet::new();
    for job in jobs.get("jobs").array() {
        require(
            job.get("run_id") == run.get("id")
                // The attempt-specific endpoint binds this response. GitHub's
                // documented job schema does not require run_attempt per job.
                && (!job.has("run_attempt") || job.get("run_attempt") == run.get("run_attempt"))
                && job.get("head_sha") == run.get("head_sha")
                && string(job, "status") == "completed"
                && !string(job, "name").is_empty()
                && names.insert(string(job, "name")),
            "CI jobs are stale or ambiguous",
        )?;
    }
    Ok(jobs.get("jobs").array().to_vec())
}

fn diagnosis(c: &Context<'_>, event: &V, pull: &V) -> Result<(&'static str, V)> {
    let slug = string(event, "repo");
    require(
        event.get("source_id").integer(true) && event.get("source_revision").integer(true),
        "CI run identity is unknown",
    )?;
    let run = api(
        c,
        &format!("repos/{slug}/actions/runs/{}", event.get("source_id").py()),
        Duration::from_secs(20),
    )?;
    require(
        run.get("id") == event.get("source_id")
            && run.get("run_attempt") == event.get("source_revision")
            && run.get("head_sha") == event.get("head_sha")
            && string(run.get("repository"), "full_name").eq_ignore_ascii_case(&slug)
            && string(&run, "head_branch") == string(pull.get("head"), "ref")
            && !string(event, "branch").is_empty()
            && string(&run, "head_branch") == string(event, "branch")
            && run.get("workflow_id").integer(true),
        "CI run changed or its identity is unknown",
    )?;
    let status = string(&run, "status");
    let conclusion = string(&run, "conclusion");
    if status != "completed" {
        return Ok(("pending", V::Arr(vec![])));
    }
    if conclusion != "failure" {
        return Ok((
            match conclusion.as_str() {
                "cancelled" => "cancelled",
                "startup_failure" | "action_required" => "environment_failure",
                "success" | "neutral" | "skipped" => "success",
                _ => "unknown",
            },
            V::Arr(vec![]),
        ));
    }
    let jobs = run_jobs(c, &slug, &run)?;
    let failed: Vec<_> = jobs
        .iter()
        .filter(|j| !["success", "skipped", "neutral"].contains(&string(j, "conclusion").as_str()))
        .collect();
    if failed.is_empty() || failed.iter().any(|j| string(j, "conclusion") != "failure") {
        return Ok(("unknown", V::Arr(vec![])));
    }
    let base_sha = string(pull.get("base"), "sha");
    require(commit(&base_sha), "CI base head is unknown")?;
    let bases = api(
        c,
        &format!(
            "repos/{slug}/actions/workflows/{}/runs?head_sha={base_sha}&status=completed&per_page=2",
            run.get("workflow_id").py()
        ),
        Duration::from_secs(20),
    )?;
    let Some(base) = bases.get("workflow_runs").array().first() else {
        return Ok(("unknown", V::Arr(vec![])));
    };
    require(
        string(base, "head_sha") == base_sha
            && base.get("workflow_id") == run.get("workflow_id")
            && string(base.get("repository"), "full_name").eq_ignore_ascii_case(&slug)
            && string(base, "status") == "completed",
        "CI base run is mismatched",
    )?;
    let base_jobs = run_jobs(c, &slug, base)?;
    let mut rows = vec![];
    let mut regression = true;
    let mut preexisting = true;
    for job in failed {
        let name = string(job, "name");
        let counterpart = base_jobs
            .iter()
            .find(|j| string(j, "name") == name)
            .map(|j| string(j, "conclusion"))
            .unwrap_or_default();
        regression &= counterpart == "success";
        preexisting &= counterpart == "failure";
        let steps = job
            .get("steps")
            .array()
            .iter()
            .filter(|s| string(s, "conclusion") == "failure")
            .take(10)
            .map(|s| bounded(s, "name", 160).into())
            .collect();
        rows.push(obj([
            ("job", bounded(job, "name", 160).into()),
            ("steps", V::Arr(steps)),
        ]));
    }
    rows.sort_by_key(|row| string(row, "job"));
    let rows = V::Arr(rows);
    let encoded = rows.json(false)?;
    if encoded.len() > 12 * 1024 || safe(&encoded).is_err() {
        return Ok(("unknown", V::Arr(vec![])));
    }
    Ok((
        if regression {
            "actionable_failure"
        } else if preexisting {
            "base_failure"
        } else {
            "unknown"
        },
        rows,
    ))
}

fn assess(c: &Context<'_>, event: &V) -> Result<V> {
    require(
        event.get("evidence").get("withheld") == &false.into(),
        "feedback evidence is withheld or unknown",
    )?;
    let pull = fresh_pull(c, event)?;
    let source = string(event, "source");
    let (class, diagnostic) = if source == "pr-review-comment" {
        require(
            event.get("source_id").integer(true),
            "feedback comment identity is unknown",
        )?;
        let slug = string(event, "repo");
        let item = api(
            c,
            &format!(
                "repos/{slug}/pulls/comments/{}",
                event.get("source_id").py()
            ),
            Duration::from_secs(20),
        )?;
        require(
            string(&item, "pull_request_url").eq_ignore_ascii_case(&format!(
                "https://api.github.com/repos/{slug}/pulls/{}",
                event.get("pr").py()
            )),
            "feedback comment belongs to another PR",
        )?;
        let fresh = comment_event(&slug, event.get("pr"), &string(event, "head_sha"), &item)?;
        require(
            fresh.get("event_id") == event.get("event_id"),
            "feedback comment was edited or moved",
        )?;
        (
            string(&fresh, "classification"),
            fresh.get("evidence").clone(),
        )
    } else if source == "ci-run" {
        let (class, diagnostic) = diagnosis(c, event, &pull)?;
        (class.into(), diagnostic)
    } else {
        return Err(Error::new(2, "unsupported feedback source"));
    };
    let failure = obj([
        ("repo", string(event, "repo").to_ascii_lowercase().into()),
        ("pr", event.get("pr").clone()),
        ("source", source.into()),
        ("diagnostic", diagnostic.clone()),
    ]);
    Ok(obj([
        ("schema", 1usize.into()),
        ("kind", "oms-feedback-repair-evidence".into()),
        ("trust", "untrusted".into()),
        ("event_id", event.get("event_id").clone()),
        ("repo", event.get("repo").clone()),
        ("pr", event.get("pr").clone()),
        ("head_sha", event.get("head_sha").clone()),
        ("classification", class.into()),
        ("failure_fingerprint", digest(&canonical(&failure)?).into()),
        ("diagnostic", diagnostic),
        ("event", event.clone()),
    ]))
}

/// Native host adapter for an explicitly selected event. The caller owns task
/// scope and repair authority; this function grants neither. Fresh nonactionable
/// diagnoses are retained as observations even when candidate selection refuses.
pub fn repair_candidate(
    repo: &Path,
    state: &Path,
    event_id: &str,
    head: &str,
    interrupted: &AtomicBool,
) -> Result<V> {
    require(
        hex(event_id, 64) && commit(head),
        "invalid feedback selection",
    )?;
    let event = observations(state)?
        .into_iter()
        .find(|row| string(row, "event_id") == event_id)
        .ok_or_else(|| Error::new(2, "reviewed feedback observation is unavailable"))?;
    require(
        string(&event, "head_sha") == head,
        "reviewed feedback head moved",
    )?;
    let c = Context {
        cwd: repo,
        source: repo,
        interrupted,
        services: &crate::Unavailable,
    };
    let evidence = assess(&c, &event)?;
    let mut observation = event;
    if let V::Obj(fields) = &mut observation {
        for field in ["event_id", "observation_id", "observed_at"] {
            fields.remove(field.as_bytes());
        }
    }
    observation.set("classification", evidence.get("classification").clone());
    observation.set("diagnostic", evidence.get("diagnostic").clone());
    append(state, &[finish(observation)?], interrupted)?;
    require(
        ["actionable_failure", "review_required"]
            .contains(&string(&evidence, "classification").as_str()),
        format!(
            "feedback is not repairable: {}",
            string(&evidence, "classification")
        ),
    )?;
    Ok(evidence)
}

pub fn revalidate_repair(repo: &Path, evidence: &V, interrupted: &AtomicBool) -> Result<()> {
    require(
        string(evidence, "kind") == "oms-feedback-repair-evidence"
            && string(evidence, "trust") == "untrusted",
        "invalid feedback repair evidence",
    )?;
    let c = Context {
        cwd: repo,
        source: repo,
        interrupted,
        services: &crate::Unavailable,
    };
    let fresh = assess(&c, evidence.get("event"))?;
    require(
        [
            "event_id",
            "repo",
            "pr",
            "head_sha",
            "classification",
            "failure_fingerprint",
        ]
        .iter()
        .all(|key| fresh.get(key) == evidence.get(key))
            && ["actionable_failure", "review_required"]
                .contains(&string(&fresh, "classification").as_str()),
        "feedback changed before dispatch; no repair sent",
    )
}
