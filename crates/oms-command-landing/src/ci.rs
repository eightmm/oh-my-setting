//! CI observations. An unreadable lookup is unknown, and an old green run or
//! maintenance observation never certifies an unpushed or different commit.
use crate::{
    Context, Error, Output, Result, V,
    cli::Options,
    git::{Git, process},
    now, obj, require, safe, storage, string,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
    time::Duration,
};
pub(crate) fn gh(c: &Context<'_>, args: &[&str], timeout: Duration) -> Result<V> {
    let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| OsString::from("gh"));
    let out = process(
        c.cwd,
        std::iter::once(bin)
            .chain(args.iter().map(OsString::from))
            .collect(),
        None,
        timeout,
        c.interrupted,
        &[("GH_HOST".into(), "github.com".into())],
    )?;
    if out.code == 130 {
        return Err(Error::new(130, "CI lookup interrupted"));
    }
    require(
        out.code == 0,
        "gh not authenticated, unreachable or unreadable",
    )?;
    V::parse(&out.stdout).map_err(Error::from)
}
pub(crate) fn api(c: &Context<'_>, endpoint: &str) -> Result<V> {
    gh(
        c,
        &[
            "api",
            "--method",
            "GET",
            "-H",
            "Accept: application/vnd.github+json",
            endpoint,
        ],
        Duration::from_secs(30),
    )
}

/// A bounded read of one exact current run attempt, retained in the existing
/// CI ledger. This never dispatches a workflow and never certifies another
/// workflow, commit, repository, or a later rerun of the same run id.
pub fn observe_attempt(
    repo: &Path,
    state: &Path,
    identity: &V,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<V> {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let remaining = || {
        deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| Error::new(124, "CI observation deadline exceeded"))
    };
    let fields = [
        "repository",
        "head_sha",
        "workflow_id",
        "run_id",
        "run_attempt",
    ];
    require(
        matches!(identity, V::Obj(keys) if keys.len() == fields.len()
            && fields.iter().all(|name| identity.has(name))),
        "malformed CI wait identity",
    )?;
    let repository = string(identity, "repository");
    let (owner, name) = repository
        .split_once('/')
        .ok_or_else(|| Error::new(2, "invalid CI repository"))?;
    require(
        crate::report::github_slug(owner, name)? == repository,
        "invalid CI repository",
    )?;
    let head = string(identity, "head_sha");
    require(
        [40, 64].contains(&head.len())
            && head
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && ["workflow_id", "run_id", "run_attempt"]
                .iter()
                .all(|key| identity.get(key).integer(true)),
        "invalid CI head or run attempt",
    )?;
    let git_text = |args: &[&str]| -> Result<String> {
        let out = process(
            repo,
            [
                "git",
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ]
            .into_iter()
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect(),
            None,
            remaining()?.min(Duration::from_secs(2)),
            interrupted,
            &[],
        )?;
        if out.code == 130 {
            return Err(Error::new(130, "CI lookup interrupted"));
        }
        require(out.code == 0, "CI repository identity unavailable")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let current = || -> Result<()> {
        require(
            crate::report::github_remote(&git_text(&["remote", "get-url", "origin"])?)?
                .eq_ignore_ascii_case(&repository)
                && git_text(&["rev-parse", "--verify", "HEAD"])? == head,
            "CI wait repository or HEAD changed",
        )
    };
    current()?;
    let context = Context {
        cwd: repo,
        source: repo,
        interrupted,
        services: &crate::Unavailable,
    };
    let get = |endpoint: &str| {
        gh(
            &context,
            &[
                "api",
                "--method",
                "GET",
                "-H",
                "Accept: application/vnd.github+json",
                endpoint,
            ],
            remaining()?.min(Duration::from_secs(8)),
        )
    };
    let endpoint = format!(
        "repos/{repository}/actions/runs/{}",
        identity.get("run_id").py()
    );
    let run = get(&endpoint)?;
    let validate = |row: &V| -> Result<()> {
        require(
            row.get("id") == identity.get("run_id")
                && row.get("workflow_id") == identity.get("workflow_id")
                && row.get("run_attempt") == identity.get("run_attempt")
                && row.get("head_sha") == identity.get("head_sha")
                && string(row.get("repository"), "full_name").eq_ignore_ascii_case(&repository),
            "CI observation belongs to another repository, head, workflow or run attempt",
        )
    };
    validate(&run)?;
    let jobs = get(&format!(
        "{endpoint}/attempts/{}/jobs?per_page=100",
        identity.get("run_attempt").py()
    ))?;
    require(
        matches!(jobs.get("jobs"), V::Arr(_))
            && jobs.get("jobs").array().len() <= 100
            && jobs.get("total_count").py() == jobs.get("jobs").array().len().to_string()
            && jobs.get("jobs").array().iter().all(|job| {
                job.get("run_id") == identity.get("run_id")
                    && job.get("run_attempt") == identity.get("run_attempt")
                    && job.get("head_sha") == identity.get("head_sha")
                    && string(job, "run_url")
                        .eq_ignore_ascii_case(&format!("https://api.github.com/{endpoint}"))
            }),
        "incomplete or mixed CI job-attempt observation",
    )?;
    let after = get(&endpoint)?;
    validate(&after)?;
    require(
        ["status", "conclusion", "updated_at"]
            .iter()
            .all(|key| run.get(key) == after.get(key)),
        "CI run changed during observation; reconcile again",
    )?;
    require(
        string(&run, "status") != "completed"
            || jobs
                .get("jobs")
                .array()
                .iter()
                .all(|job| string(job, "status") == "completed"),
        "completed CI run has incomplete job observations",
    )?;
    let mut job_ids = BTreeSet::new();
    let mut selected_jobs = Vec::new();
    for job in jobs.get("jobs").array() {
        require(
            job.get("id").integer(true) && job_ids.insert(job.get("id").py()),
            "invalid or duplicate CI job identity",
        )?;
        selected_jobs.push(obj([
            "id",
            "run_id",
            "run_attempt",
            "head_sha",
            "name",
            "status",
            "conclusion",
            "started_at",
            "completed_at",
        ]
        .into_iter()
        .map(|key| (key, job.get(key).clone()))));
    }
    selected_jobs.sort_by_key(|job| job.get("id").py());
    let mut row = obj([
        ("schema", 1usize.into()),
        ("kind", "exact-run-attempt".into()),
        ("identity", identity.clone()),
        ("status", run.get("status").clone()),
        ("conclusion", run.get("conclusion").clone()),
        ("jobs", V::Arr(selected_jobs)),
    ]);
    // The result fingerprint excludes lookup time, so duplicate observations
    // retain one identity even when the process restarts or polls the next day.
    use sha2::{Digest, Sha256};
    row.set(
        "observation_sha256",
        format!("{:x}", Sha256::digest(row.json(false)?)),
    );
    row.set("updated_at", run.get("updated_at").clone());
    row.set("ts", now());
    let ledger = state.join(".oms/ci.jsonl");
    crate::check(interrupted)?;
    // A read-only observer does not wait behind a long-lived writer. The
    // caller can observe again; no stale success is returned on contention.
    let mut settings =
        oms_lock::Settings::from_env().map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    settings.timeout = remaining()?;
    let _guard = oms_lock::Lock::for_file(&ledger, settings)
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))?
        .try_acquire()
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    current()?;
    let raw = storage::root(state)?
        .read_no_follow(
            Path::new(".oms/ci.jsonl"),
            oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: 8 * 1024 * 1024,
            },
        )
        .map_err(|e| Error::new(2, format!("CI ledger unreadable: {e:?}")))?;
    let prior = storage::parse_rows(&raw)?;
    if !prior
        .iter()
        .any(|old| old.get("observation_sha256") == row.get("observation_sha256"))
    {
        let mut next = raw.clone();
        next.extend(row.json(false)?);
        next.push(b'\n');
        require(
            next.len() <= 8 * 1024 * 1024,
            "CI ledger exceeds observation bound",
        )?;
        storage::replace(
            state,
            Path::new(".oms/ci.jsonl"),
            if ledger.exists() { Some(&raw) } else { None },
            &next,
        )?;
    }
    Ok(row)
}
pub fn query_repo(destination: &str) -> Result<String> {
    require(
        !destination.trim().is_empty() && !destination.chars().any(char::is_control),
        "invalid repository destination",
    )?;
    if !destination.contains(':') && !destination.contains('@') {
        return Ok(destination
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .into());
    }
    if let Some(path) = destination.strip_prefix("file:///") {
        return Ok(format!(
            "/{}",
            path.trim_end_matches('/').trim_end_matches(".git")
        ));
    }
    let (scheme, rest) = destination
        .split_once("://")
        .unwrap_or(("ssh", destination));
    let (authority, path) = if rest.contains('/') {
        if destination.contains("://") {
            rest.split_once('/')
                .ok_or_else(|| Error::new(2, "invalid repository destination"))?
        } else {
            rest.rsplit('@')
                .next()
                .unwrap_or(rest)
                .split_once(':')
                .ok_or_else(|| Error::new(2, "invalid repository destination"))?
        }
    } else {
        return Err(Error::new(2, "invalid repository destination"));
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(h, p)| (h, Some(p)));
    let host = host.to_ascii_lowercase();
    require(
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b)),
        "invalid repository destination host",
    )?;
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .map_err(|_| Error::new(2, "invalid repository destination port"))
        })
        .transpose()?;
    require(port != Some(0), "invalid repository destination port")?;
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    let parts: Vec<_> = path.split('/').collect();
    require(parts.len() == 2, "invalid repository destination path")?;
    let slug = crate::report::github_slug(parts[0], parts[1])?;
    let host = if ["github.com", "ssh.github.com", "www.github.com"].contains(&host.as_str()) {
        "github.com".into()
    } else {
        host
    };
    let port = if scheme == "ssh"
        || scheme == "https" && port == Some(443)
        || scheme == "http" && port == Some(80)
        || host == "github.com"
    {
        None
    } else {
        port
    };
    Ok(format!(
        "{host}{}/{slug}",
        port.map(|p| format!(":{p}")).unwrap_or_default()
    ))
}
fn public_destination(destination: &str) -> bool {
    [
        "https://github.com/eightmm/oh-my-setting",
        "https://github.com/eightmm/oh-my-setting.git",
        "ssh://git@github.com/eightmm/oh-my-setting",
        "ssh://git@github.com/eightmm/oh-my-setting.git",
        "git@github.com:eightmm/oh-my-setting",
        "git@github.com:eightmm/oh-my-setting.git",
        "eightmm/oh-my-setting",
        "github.com/eightmm/oh-my-setting",
    ]
    .contains(&destination)
}
fn component(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn public_json(c: &Context<'_>, endpoint: &str) -> Result<V> {
    let url = format!("https://api.github.com/repos/eightmm/oh-my-setting/{endpoint}");
    let result = process(
        c.cwd,
        vec![
            "curl".into(),
            "--disable".into(),
            "--fail".into(),
            "--silent".into(),
            "--show-error".into(),
            "--max-time".into(),
            "30".into(),
            "--proto".into(),
            "=https".into(),
            "-H".into(),
            "Accept: application/vnd.github+json".into(),
            "-H".into(),
            "User-Agent: oms-native-ci".into(),
            url.into(),
        ],
        None,
        Duration::from_secs(35),
        c.interrupted,
        &[],
    )?;
    require(
        result.code == 0 && result.stdout.len() <= 1024 * 1024,
        "public CI API unavailable or exceeds bound",
    )?;
    V::parse(&result.stdout).map_err(Error::from)
}
pub(crate) fn run_details(c: &Context<'_>, destination: &str, id: &V, sha: &str) -> Result<V> {
    let destination_arg = query_repo(destination)?;
    match gh(
        c,
        &[
            "run",
            "view",
            &id.py(),
            "--repo",
            &destination_arg,
            "--json",
            "headSha,status,conclusion,jobs",
        ],
        Duration::from_secs(30),
    ) {
        Ok(row) => {
            conclusion(&row, sha)?;
            Ok(row)
        }
        Err(e) if !public_destination(destination) => Err(e),
        Err(_) => {
            require(id.integer(true), "invalid CI run id")?;
            let run = public_json(c, &format!("actions/runs/{}", id.py()))?;
            require(
                run.get("id") == id
                    && run.get("head_sha").text() == Some(sha)
                    && run.get("repository").get("full_name").text()
                        == Some("eightmm/oh-my-setting"),
                "mismatched public CI run",
            )?;
            let attempt = run.get("run_attempt");
            require(attempt.integer(true), "invalid public CI run attempt")?;
            let jobs = public_json(
                c,
                &format!(
                    "actions/runs/{}/attempts/{}/jobs?per_page=100",
                    id.py(),
                    attempt.py()
                ),
            )?;
            require(
                matches!(jobs.get("jobs"), V::Arr(_))
                    && jobs.get("total_count").py() == jobs.get("jobs").array().len().to_string(),
                "incomplete public CI jobs",
            )?;
            require(
                jobs.get("jobs").array().iter().all(|j| {
                    j.get("run_id") == id
                        && j.get("head_sha").text() == Some(sha)
                        && j.get("run_attempt") == attempt
                }),
                "mismatched public CI jobs",
            )?;
            let row = obj([
                ("headSha", sha.into()),
                ("status", run.get("status").clone()),
                ("conclusion", run.get("conclusion").clone()),
                ("jobs", jobs.get("jobs").clone()),
            ]);
            conclusion(&row, sha)?;
            Ok(row)
        }
    }
}
pub(crate) enum ReconciledCi {
    Pending,
    Unsuccessful,
    Full(V),
}

/// Verify one exact PR observation. The check rollup only locates the gate job;
/// its publisher, workflow, event, run attempt, full job set and PR association
/// come from the authenticated repository-scoped Actions API.
pub(crate) fn reconciled_pr(
    c: &Context<'_>,
    repo: &str,
    number: &str,
    sha: &str,
    branch: &str,
    target: &str,
    rollup: &V,
) -> Result<ReconciledCi> {
    let prefix = format!("repos/{repo}");
    let runs = api(
        c,
        &format!(
            "{prefix}/actions/workflows/test.yml/runs?head_sha={sha}&branch={}&event=pull_request&per_page=1",
            component(branch)
        ),
    )?;
    let [run] = runs.get("workflow_runs").array() else {
        return Err(Error::new(2, "latest PR workflow run unavailable"));
    };
    let id = run.get("id");
    let attempt = run.get("run_attempt");
    require(
        id.integer(true)
            && attempt.integer(true)
            && string(run, "path") == ".github/workflows/test.yml"
            && string(run, "event") == "pull_request"
            && string(run, "head_sha") == sha
            && string(run, "head_branch") == branch
            && string(run.get("repository"), "full_name") == repo
            && string(run.get("head_repository"), "full_name") == repo,
        "workflow run identity mismatch",
    )?;
    let V::Arr(linked) = run.get("pull_requests") else {
        return Err(Error::new(2, "missing PR association"));
    };
    if linked.is_empty() {
        // A check rollup can be shared by two PRs using the same head.
        // GitHub may omit association after merge. Require precisely one PR ever
        // opened from this head branch in the repository in that case.
        let owner = repo
            .split_once('/')
            .ok_or_else(|| Error::new(2, "invalid repository"))?
            .0;
        let pulls = api(
            c,
            &format!(
                "{prefix}/pulls?state=all&head={owner}%3A{}&per_page=2",
                component(branch)
            ),
        )?;
        let [pr] = pulls.array() else {
            return Err(Error::new(2, "ambiguous PR association"));
        };
        require(
            pr.get("number").py() == number
                && string(pr.get("head"), "sha") == sha
                && string(pr.get("head"), "ref") == branch
                && string(pr.get("base"), "ref") == target
                && string(pr.get("head").get("repo"), "full_name") == repo
                && string(pr.get("base").get("repo"), "full_name") == repo,
            "unbound PR branch",
        )?;
    } else {
        let [pr] = linked.as_slice() else {
            return Err(Error::new(2, "ambiguous workflow association"));
        };
        require(
            pr.get("number").py() == number
                && string(pr.get("head"), "sha") == sha
                && string(pr.get("head"), "ref") == branch
                && string(pr.get("base"), "ref") == target
                && run.get("repository").get("id").integer(true)
                && pr.get("head").get("repo").get("id") == run.get("repository").get("id")
                && pr.get("base").get("repo").get("id") == run.get("repository").get("id"),
            "workflow belongs to another pull request",
        )?;
    }
    let mut detail = obj([
        ("headSha", sha.into()),
        ("status", run.get("status").clone()),
        ("conclusion", run.get("conclusion").clone()),
    ]);
    match conclusion(&detail, sha)?.as_str() {
        "success" => {}
        "pending" => return Ok(ReconciledCi::Pending),
        _ => return Ok(ReconciledCi::Unsuccessful),
    }
    let response = api(
        c,
        &format!(
            "{prefix}/actions/runs/{}/attempts/{}/jobs?per_page=100",
            id.py(),
            attempt.py()
        ),
    )?;
    let V::Arr(jobs) = response.get("jobs") else {
        return Err(Error::new(2, "CI jobs unavailable"));
    };
    require(
        !jobs.is_empty()
            && jobs.len() <= 100
            && response.get("total_count") == &V::from(jobs.len())
            && jobs.iter().all(|job| {
                job.get("run_id") == id
                    && job.get("run_attempt") == attempt
                    && string(job, "head_sha") == sha
                    && string(job, "status") == "completed"
            }),
        "unbound or incomplete jobs",
    )?;
    let mut gates = jobs.iter().filter(|job| string(job, "name") == "gate");
    let gate = gates
        .next()
        .ok_or_else(|| Error::new(2, "stable gate missing"))?;
    let url = format!(
        "https://github.com/{repo}/actions/runs/{}/job/{}",
        id.py(),
        gate.get("id").py()
    );
    require(
        gates.next().is_none()
            && gate.get("id").integer(true)
            && string(gate, "conclusion") == "success"
            && string(gate, "html_url") == url
            && rollup.array().len() <= 100
            && rollup.array().iter().any(|check| {
                string(check, "__typename") == "CheckRun"
                    && string(check, "conclusion") == "SUCCESS"
                    && string(check, "name") == "gate"
                    && string(check, "detailsUrl") == url
            }),
        "PR gate job mismatch",
    )?;
    detail.set("jobs", response.get("jobs").clone());
    require(
        full_scope(&detail, sha)? == "full",
        "full CI proof unavailable",
    )?;
    Ok(ReconciledCi::Full(obj([
        ("run_id", id.clone()),
        ("attempt", attempt.clone()),
        ("scope", "full".into()),
    ])))
}
fn red(v: &str) -> bool {
    [
        "failure",
        "timed_out",
        "startup_failure",
        "cancelled",
        "action_required",
    ]
    .contains(&v)
}
pub fn conclusion(run: &V, sha: &str) -> Result<String> {
    require(
        run.get("headSha").text() == Some(sha),
        "CI run describes another SHA",
    )?;
    let status = string(run, "status");
    require(
        [
            "completed",
            "queued",
            "in_progress",
            "pending",
            "waiting",
            "requested",
        ]
        .contains(&status.as_str()),
        "invalid CI run status",
    )?;
    if status == "completed" {
        let value = string(run, "conclusion");
        require(
            !value.is_empty() && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "invalid CI conclusion",
        )?;
        Ok(value)
    } else {
        Ok("pending".into())
    }
}
pub(crate) fn latest(
    c: &Context<'_>,
    destination: &str,
    sha: &str,
    branch: &str,
    event: &str,
) -> Result<Option<V>> {
    let destination_arg = query_repo(destination)?;
    let queried = gh(
        c,
        &[
            "run",
            "list",
            "--repo",
            &destination_arg,
            "--commit",
            sha,
            "--event",
            event,
            "--limit",
            "1",
            "--json",
            "databaseId,status,conclusion,headSha",
            "--branch",
            branch,
            "--workflow",
            "test.yml",
        ],
        Duration::from_secs(30),
    );
    let rows = match queried {
        Ok(rows) => rows,
        Err(e) if !public_destination(destination) => return Err(e),
        Err(_) => {
            let payload = public_json(
                c,
                &format!(
                    "actions/workflows/test.yml/runs?head_sha={}&branch={}&event={}&per_page=1",
                    component(sha),
                    component(branch),
                    component(event)
                ),
            )?;
            require(
                matches!(payload.get("workflow_runs"), V::Arr(_)),
                "invalid public CI run list",
            )?;
            let mut rows = vec![];
            for row in payload.get("workflow_runs").array() {
                require(
                    row.get("head_sha").text() == Some(sha)
                        && row.get("head_branch").text() == Some(branch)
                        && row.get("event").text() == Some(event)
                        && row.get("repository").get("full_name").text()
                            == Some("eightmm/oh-my-setting")
                        && row.get("path").text() == Some(".github/workflows/test.yml"),
                    "mismatched public CI evidence",
                )?;
                rows.push(obj([
                    ("databaseId", row.get("id").clone()),
                    ("headSha", row.get("head_sha").clone()),
                    ("status", row.get("status").clone()),
                    ("conclusion", row.get("conclusion").clone()),
                ]));
            }
            V::Arr(rows)
        }
    };
    require(
        matches!(rows, V::Arr(_)) && rows.array().len() <= 1,
        "invalid CI run list",
    )?;
    if let Some(run) = rows.array().first() {
        require(run.get("databaseId").integer(true), "invalid CI run id")?;
        conclusion(run, sha)?;
        Ok(Some(run.clone()))
    } else {
        Ok(None)
    }
}
pub fn full_scope(run: &V, sha: &str) -> Result<&'static str> {
    require(
        conclusion(run, sha)? == "success",
        "CI scope requires successful exact-SHA evidence",
    )?;
    let jobs = run.get("jobs");
    require(matches!(jobs, V::Arr(_)), "CI jobs unavailable")?;
    let mut names = BTreeMap::new();
    for job in jobs.array() {
        let name = string(job, "name");
        require(
            !name.is_empty() && names.insert(name, string(job, "conclusion")).is_none(),
            "unnamed or duplicate CI job",
        )?;
    }
    // Accept either the legacy sharded matrix or the streamlined pure-Rust workflow.
    let mut sharded_required: Vec<String> = ["lint", "gate", "native (ubuntu-latest, copy)"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for lane in 1..=4 {
        for family in ["focused", "scripts-smoke", "operator-tools"] {
            sharded_required.push(format!("{family} ({lane}/4)"));
        }
    }
    let streamlined_required: Vec<String> =
        ["lint", "focused", "gate", "native (ubuntu-latest, copy)"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let matched_required = if sharded_required
        .iter()
        .all(|n| names.get(n).is_some_and(|c| c == "success"))
    {
        Some(&sharded_required)
    } else if streamlined_required
        .iter()
        .all(|n| names.get(n).is_some_and(|c| c == "success"))
    {
        Some(&streamlined_required)
    } else {
        None
    };
    if let Some(required) = matched_required {
        let optional = regex::Regex::new(r"^(?:python311|native \(windows-latest, auto\)|operator-tools \(affected\)|affected checks|rust \(ubuntu-24\.04\))$").unwrap();
        require(
            names.iter().all(|(name, value)| {
                required.contains(name)
                    || value == "success"
                    || value == "skipped" && optional.is_match(name)
            }),
            "unexpected CI job results make scope ambiguous",
        )?;
        return Ok("full");
    }
    require(
        names.get("lint").is_some_and(|s| s == "skipped")
            && names.get("affected_plan").is_some_and(|s| s == "success")
            && names.get("gate").is_some_and(|s| s == "success"),
        "CI selection is incomplete or failed",
    )?;
    Ok("affected")
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    let known = [
        "status",
        "record",
        "tick",
        "history",
        "flaky",
        "protection",
        "triage",
        "suggest",
        "feedback",
        "attention",
    ];
    let mut words = o.words.clone();
    let action = if words.first().is_some_and(|s| known.contains(&s.as_str())) {
        words.remove(0)
    } else if o.has("record") {
        String::from("record")
    } else {
        String::from("status")
    };
    require(words.len() <= 1, "unexpected CI argument")?;
    require(
        !o.has("sha") || action == "triage",
        "--sha is only read by triage",
    )?;
    require(
        (["suggest", "feedback", "attention"].contains(&action.as_str()) || !o.has("json"))
            && (action == "suggest" || !o.has("refresh")),
        "--json is only read by suggest/feedback/attention; --refresh is only read by suggest",
    )?;
    let limit = o
        .value("limit", "20")
        .parse::<usize>()
        .map_err(|_| Error::new(2, "--limit must be a positive integer"))?;
    require(limit > 0, "--limit must be a positive integer")?;
    if o.has("sha") {
        require(
            (7..=40).contains(&o.get("sha").len())
                && o.get("sha").bytes().all(|b| b.is_ascii_hexdigit()),
            "--sha must be 7 to 40 hexadecimal characters",
        )?;
    }
    let repo = Git::root(c.cwd, c.interrupted)?;
    let state = c.state(&repo)?;
    let git = Git::new(&repo, c.interrupted);
    if action == "attention" {
        require(limit <= 100, "attention --limit must be between 1 and 100")?;
        return attention(
            c,
            &git,
            words.first().map(String::as_str),
            limit,
            o.has("json"),
        );
    }
    let branch = words.first().cloned().unwrap_or_else(|| {
        git.text(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|_| "main".into())
    });
    if action == "record" {
        storage::ensure_ignore(&state, c.interrupted)?;
    }
    if action == "suggest" {
        return crate::suggestions::execute(c, o, &state, &branch);
    }
    if action == "feedback" {
        let rows = crate::feedback::observations(&state)?
            .into_iter()
            .take(limit)
            .collect::<Vec<_>>();
        return if o.has("json") {
            Output::json(&V::Arr(rows))
        } else {
            Ok(Output::text(
                rows.iter()
                    .map(|row| {
                        format!(
                            "feedback:{} {} {} #{} {}\n",
                            string(row, "event_id"),
                            string(row, "classification"),
                            string(row, "repo"),
                            row.get("pr").py(),
                            string(row, "head_sha")
                        )
                    })
                    .collect::<String>(),
            ))
        };
    }
    if action == "flaky" {
        let rows = storage::rows(&state, Path::new(".oms/ci.jsonl"))?;
        let mut bad = BTreeMap::new();
        let mut out = String::new();
        for row in rows.iter().filter(|r| string(r, "branch") == branch) {
            let sha = string(row, "sha");
            if red(&string(row, "conclusion")) {
                bad.insert(sha, row);
            } else if string(row, "conclusion") == "success"
                && let Some(before) = bad.remove(&sha)
            {
                out += &format!(
                    "{} red {} -> green {}; jobs: {}\n",
                    &sha[..sha.len().min(12)],
                    string(before, "ts"),
                    string(row, "ts"),
                    before
                        .get("failed_jobs")
                        .array()
                        .iter()
                        .map(V::py)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        if out.is_empty() {
            out = "ci-status flaky: no recorded red-to-green commits\n".into();
        }
        return Ok(Output::text(out));
    }
    if action == "protection" {
        return protection(c, &git, words.first().map(String::as_str));
    }
    if action == "history" {
        let rows = gh(
            c,
            &[
                "run",
                "list",
                "--branch",
                &branch,
                "--status",
                "completed",
                "--limit",
                &limit.to_string(),
                "--json",
                "databaseId,headSha,conclusion,createdAt,updatedAt",
            ],
            Duration::from_secs(30),
        )?;
        require(matches!(rows, V::Arr(_)), "unreadable run list")?;
        let mut out = String::new();
        for row in rows.array() {
            out += &format!(
                "{}  {}\n",
                string(row, "headSha").chars().take(12).collect::<String>(),
                string(row, "conclusion")
            );
        }
        return Ok(Output::text(out));
    }
    if action == "triage" {
        return triage(c, &git, &branch, o.get("sha"));
    }
    let head = git.commit(&branch).unwrap_or_default();
    let ledger = std::env::var_os("OMS_CI_LEDGER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.join(".oms/ci.jsonl"));
    let mut tick_lock = None;
    if action == "tick" {
        if !state.join(".oms").is_dir() {
            return Ok(Output::default());
        }
        let marker = state.join(".oms/hooks/ci-tick");
        let interval = std::env::var("OMS_CI_TICK_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        let settings =
            oms_lock::Settings::from_env().map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        let lock = oms_lock::Lock::for_file(&marker, settings)
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        let Ok(guard) = lock.try_acquire() else {
            return Ok(Output::default());
        };
        tick_lock = Some(guard);
        let old = storage::read_external(&marker, true)?;
        if String::from_utf8_lossy(&old)
            .trim()
            .parse::<u64>()
            .is_ok_and(|t| crate::timestamp().saturating_sub(t) < interval)
        {
            return Ok(Output::default());
        }
        let mut ancestor = marker.parent().unwrap();
        while !ancestor.exists() {
            ancestor = ancestor.parent().unwrap();
        }
        storage::replace(
            &std::fs::canonicalize(ancestor)?,
            marker.strip_prefix(ancestor).unwrap(),
            if marker.exists() { Some(&old) } else { None },
            crate::timestamp().to_string().as_bytes(),
        )?;
    }
    let query = gh(
        c,
        &[
            "run",
            "list",
            "--branch",
            &branch,
            "--limit",
            "10",
            "--json",
            "status,conclusion,workflowName,headSha,url,attempt,databaseId",
        ],
        if action == "tick" {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(30)
        },
    );
    let rows = match query {
        Ok(rows) => rows,
        Err(_) if action == "tick" => return Ok(Output::default()),
        Err(e) => return Err(e),
    };
    require(
        matches!(rows, V::Arr(_)),
        "gh returned an unreadable run list (lookup failed)",
    )?;
    let ahead = if git
        .text(&["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .as_deref()
        == Some(&branch)
    {
        git.text(&["rev-list", "--count", "@{upstream}..HEAD"])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
    } else {
        None
    };
    let matched = rows.array().iter().find(|r| string(r, "headSha") == head);
    let latest = rows.array().first();
    let current = if ahead.is_some_and(|n| n > 0) {
        None
    } else {
        matched.or(if ahead.is_none() { latest } else { None })
    };
    let chosen = current.or(latest);
    let mut out = if let Some(n) = ahead.filter(|n| *n > 0) {
        format!(
            "ci-status [{branch}]: unpushed ({n} commit{} ahead of the upstream — push to get CI)\n",
            if n == 1 { "" } else { "s" }
        )
    } else if let Some(run) = current {
        format!(
            "ci-status [{branch}]: {} {}  {}\n",
            string(run, "status"),
            run.get("conclusion")
                .text()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| run.get("status").text().unwrap_or("?")),
            string(run, "url")
        )
    } else if latest.is_some() {
        format!(
            "ci-status [{branch}]: no run yet for HEAD {} (pending or not started)\n",
            &head[..head.len().min(12)]
        )
    } else {
        format!("ci-status: no runs for {branch}\n")
    };
    if current.is_none()
        && let Some(row) = latest
    {
        out += &format!(
            "  history: {} {} on {}  {}\n",
            string(row, "status"),
            string(row, "conclusion"),
            string(row, "headSha").chars().take(12).collect::<String>(),
            string(row, "url")
        );
    }
    if ["record", "tick"].contains(&action.as_str())
        && let Some(run) = chosen
    {
        let concl = run
            .get("conclusion")
            .text()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| run.get("status").text().unwrap_or("unknown"));
        let mut row = obj([
            ("schema", 1usize.into()),
            ("ts", now().into()),
            ("branch", branch.as_str().into()),
            ("sha", run.get("headSha").clone()),
            ("status", run.get("status").clone()),
            ("conclusion", concl.into()),
            ("url", run.get("url").clone()),
            ("run_id", run.get("databaseId").clone()),
        ]);
        if run.has("attempt") {
            row.set("attempt", run.get("attempt").clone());
        }
        if run.get("workflowName").truth() {
            row.set("workflow", run.get("workflowName").clone());
        }
        let _guard = storage::lock(&ledger, c.interrupted)?;
        let raw = storage::read_external(&ledger, true)?;
        let prior = storage::parse_rows(&raw)?;
        if !prior.iter().any(|r| {
            ["branch", "sha", "status", "conclusion", "run_id", "attempt"]
                .iter()
                .all(|key| r.get(key) == row.get(key))
        }) {
            let mut next = raw.clone();
            next.extend(row.json(false)?);
            next.push(b'\n');
            let mut ancestor = ledger.parent().unwrap();
            while !ancestor.exists() {
                ancestor = ancestor.parent().unwrap();
            }
            storage::replace(
                &std::fs::canonicalize(ancestor)?,
                ledger.strip_prefix(ancestor).unwrap(),
                if ledger.exists() { Some(&raw) } else { None },
                &next,
            )?;
        }
    }
    let mut stderr = vec![];
    if ["record", "tick"].contains(&action.as_str()) {
        (_, stderr) = crate::feedback::optional_result(crate::feedback::poll(
            c,
            &state,
            &git,
            &branch,
            &head,
            &rows,
            if action == "tick" {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(20)
            },
        ))?;
    }
    drop(tick_lock);
    Ok(Output {
        code: i32::from(current.is_some_and(|r| red(&string(r, "conclusion")))),
        stdout: if action == "tick" {
            vec![]
        } else {
            out.into_bytes()
        },
        stderr,
    })
}
/// Maintenance observations never certify the selected checkout or write a CI receipt.
fn attention(
    c: &Context<'_>,
    git: &Git<'_>,
    selected_branch: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<Output> {
    let slug = crate::report::github_remote(&git.text(&["remote", "get-url", "origin"])?)?;
    let branch = match selected_branch {
        Some(branch) => branch.to_owned(),
        None => string(&api(c, &format!("repos/{slug}"))?, "default_branch"),
    };
    require(
        !branch.is_empty() && branch.len() <= 255 && !branch.chars().any(char::is_control),
        "CI attention branch unavailable or invalid",
    )?;
    let mut observations = Vec::new();
    for event in ["schedule", "workflow_dispatch"] {
        let endpoint = format!(
            "repos/{slug}/actions/runs?event={event}&branch={}&per_page={limit}",
            component(&branch)
        );
        match api(c, &endpoint)
            .and_then(|response| attention_rows(&response, &slug, &branch, event, limit))
        {
            Ok(rows) => observations.extend(rows),
            Err(error) if error.code == 130 => return Err(error),
            Err(_) => observations.push(obj([
                ("event", event.into()),
                ("state", "unknown".into()),
                ("reason", "run lookup unavailable or invalid".into()),
            ])),
        }
    }
    let code = if observations
        .iter()
        .any(|r| ["unknown", "pending"].contains(&string(r, "state").as_str()))
    {
        2
    } else if observations.iter().any(|r| string(r, "state") == "failed") {
        1
    } else {
        0
    };
    let report = obj([
        ("schema", 1usize.into()),
        ("kind", "oms-ci-attention".into()),
        ("repo", slug.into()),
        ("branch", branch.clone().into()),
        ("observed_at", now().into()),
        ("observations", V::Arr(observations)),
    ]);
    let mut output = if json {
        Output::json(&report)?
    } else {
        Output::text(
            report
                .get("observations")
                .array()
                .iter()
                .map(|row| {
                    let workflow = row.get("workflow_id");
                    format!(
                        "ci-status attention [{branch}]: {}{} {} {}{}{}{}\n",
                        string(row, "event"),
                        if workflow.integer(true) {
                            format!(" workflow {}", workflow.py())
                        } else {
                            String::new()
                        },
                        string(row, "state"),
                        if row.has("reason") {
                            string(row, "reason")
                        } else {
                            string(row, "conclusion")
                        },
                        if row.has("url") {
                            format!("  {}", string(row, "url"))
                        } else {
                            String::new()
                        },
                        if row.has("updated_at") {
                            format!(" updated {}", string(row, "updated_at"))
                        } else {
                            String::new()
                        },
                        if string(row, "coverage") == "bounded" {
                            " (bounded window)"
                        } else {
                            ""
                        },
                    )
                })
                .collect::<String>(),
        )
    };
    output.code = code;
    Ok(output)
}
fn attention_rows(
    response: &V,
    slug: &str,
    branch: &str,
    event: &str,
    limit: usize,
) -> Result<Vec<V>> {
    let rows = response.get("workflow_runs");
    require(matches!(rows, V::Arr(_)), "unreadable maintenance run list")?;
    let total = response.get("total_count");
    require(total.integer(false), "unreadable maintenance run count")?;
    let total = total
        .py()
        .parse::<u64>()
        .map_err(|_| Error::new(2, "invalid run count"))?;
    require(
        rows.array().len() <= limit && total >= rows.array().len() as u64,
        "invalid maintenance run window",
    )?;
    let mut latest: BTreeMap<u64, ((u64, u64), &V)> = BTreeMap::new();
    for row in rows.array() {
        let number = |key: &str| -> Result<u64> {
            require(
                row.get(key).integer(true),
                "invalid maintenance run identity",
            )?;
            row.get(key)
                .py()
                .parse()
                .map_err(|_| Error::new(2, "invalid maintenance run identity"))
        };
        let id = number("id")?;
        let workflow = number("workflow_id")?;
        let order = (number("run_number")?, number("run_attempt")?);
        let sha = string(row, "head_sha");
        let updated = string(row, "updated_at");
        require(
            string(row, "event") == event
                && string(row, "head_branch") == branch
                && sha.len() == 40
                && sha.bytes().all(|b| b.is_ascii_hexdigit())
                && updated.len() <= 64
                && chrono::DateTime::parse_from_rfc3339(&updated).is_ok()
                && string(row, "html_url")
                    .strip_prefix("https://github.com/")
                    .and_then(|path| path.strip_suffix(&format!("/actions/runs/{id}")))
                    .is_some_and(|repository| repository.eq_ignore_ascii_case(slug)),
            "mismatched maintenance run",
        )?;
        match latest.get(&workflow) {
            Some((old, _)) if *old >= order => {}
            _ => {
                latest.insert(workflow, (order, row));
            }
        }
    }
    let mut observations = Vec::new();
    for (_, (_, row)) in latest {
        let status = string(row, "status");
        let conclusion = string(row, "conclusion");
        let state = match status.as_str() {
            "completed" if red(&conclusion) => "failed",
            "completed" if conclusion == "success" => "clear",
            "queued" | "in_progress" | "waiting" | "requested" | "pending" => "pending",
            _ => "unknown",
        };
        observations.push(obj([
            ("event", event.into()),
            ("state", state.into()),
            ("workflow_id", row.get("workflow_id").clone()),
            ("run_id", row.get("id").clone()),
            ("attempt", row.get("run_attempt").clone()),
            ("sha", row.get("head_sha").clone()),
            ("updated_at", row.get("updated_at").clone()),
            (
                "coverage",
                if total > rows.array().len() as u64 {
                    "bounded"
                } else {
                    "complete"
                }
                .into(),
            ),
            (
                "conclusion",
                if state == "unknown" {
                    "unrecognized outcome".into()
                } else if state == "pending" {
                    status.into()
                } else {
                    conclusion.into()
                },
            ),
            ("url", row.get("html_url").clone()),
        ]));
    }
    if rows.array().is_empty() {
        observations.push(obj([
            ("event", event.into()),
            ("state", "unknown".into()),
            ("reason", "no observed runs".into()),
        ]));
    }
    Ok(observations)
}
fn protection(c: &Context<'_>, git: &Git<'_>, branch: Option<&str>) -> Result<Output> {
    let slug = crate::report::github_remote(&git.text(&["remote", "get-url", "origin"])?)?;
    let branch = match branch {
        Some(b) => b.to_owned(),
        None => string(&api(c, &format!("repos/{slug}"))?, "default_branch"),
    };
    require(!branch.is_empty(), "default branch unavailable")?;
    let encoded = component(&branch);
    let result = api(
        c,
        &format!("repos/{slug}/branches/{encoded}/protection/required_status_checks"),
    );
    let mut required = BTreeSet::new();
    let mut unknown = false;
    match result {
        Ok(row) => {
            for context in row.get("contexts").array() {
                if let Some(s) = context.text() {
                    required.insert(s.to_owned());
                }
            }
            for check in row.get("checks").array() {
                required.insert(string(check, "context"));
            }
        }
        Err(_) => unknown = true,
    };
    match api(c, &format!("repos/{slug}/rules/branches/{encoded}")) {
        Ok(rows) => {
            require(matches!(rows, V::Arr(_)), "unreadable branch rules")?;
            for rule in rows.array() {
                if string(rule, "type") == "required_status_checks" {
                    for check in rule.get("parameters").get("required_status_checks").array() {
                        required.insert(string(check, "context"));
                    }
                }
            }
        }
        Err(_) => unknown = true,
    };
    let proven = required.contains("gate");
    let code = if proven {
        0
    } else if unknown {
        2
    } else {
        1
    };
    Ok(Output {
        code,
        stdout: format!(
            "ci-status protection [{branch}]: {}\n",
            if proven {
                "gate required"
            } else if unknown {
                "unknown"
            } else {
                "gate not required"
            }
        )
        .into_bytes(),
        stderr: vec![],
    })
}
fn triage(c: &Context<'_>, git: &Git<'_>, branch: &str, sha: &str) -> Result<Output> {
    let head = if sha.is_empty() {
        git.commit(branch)?
    } else {
        git.commit(sha)?
    };
    let runs = gh(
        c,
        &[
            "run",
            "list",
            "--branch",
            branch,
            "--limit",
            "20",
            "--json",
            "databaseId,status,conclusion,workflowName,headSha,url",
        ],
        Duration::from_secs(20),
    )?;
    let Some(run) = runs.array().iter().find(|r| string(r, "headSha") == head) else {
        return Err(Error::new(2, "no run for requested commit"));
    };
    if string(run, "status") != "completed" {
        return Err(Error::new(2, "CI run is pending"));
    }
    if !red(&string(run, "conclusion")) {
        return Ok(Output::text("ci-status triage: nothing red\n"));
    }
    let jobs = gh(
        c,
        &["run", "view", &run.get("databaseId").py(), "--json", "jobs"],
        Duration::from_secs(20),
    )?;
    let mut out = format!(
        "ci-status triage [{branch}]: {} on {}\n",
        string(run, "conclusion"),
        &head[..12]
    );
    if let Ok(comparison) = comparison(git.repo, branch, &head, c.interrupted) {
        out += &format!(
            "  base comparison: {} ({})\n",
            string(&comparison, "class"),
            string(&comparison, "note")
        );
        for job in comparison.get("jobs").array() {
            out += &format!(
                "  job: {}; base: {}\n",
                string(job, "job"),
                string(job, "base")
            );
        }
    }
    for job in jobs
        .get("jobs")
        .array()
        .iter()
        .filter(|j| red(&string(j, "conclusion")))
    {
        out += &format!(
            "  failed job: {}\n",
            oms_contract_markdown::bounded_line(&string(job, "name"), 160)
        );
        for step in job
            .get("steps")
            .array()
            .iter()
            .filter(|s| red(&string(s, "conclusion")))
        {
            out += &format!(
                "    {}\n",
                oms_contract_markdown::bounded_line(&string(step, "name"), 160)
            );
        }
    }
    let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| "gh".into());
    let log = process(
        c.cwd,
        vec![
            bin,
            "run".into(),
            "view".into(),
            run.get("databaseId").py().into(),
            "--log-failed".into(),
        ],
        None,
        Duration::from_secs(60),
        c.interrupted,
        &[],
    )?;
    if log.code == 0 && safe(&log.stdout).is_ok() {
        out += &String::from_utf8_lossy(&log.stdout)
            .lines()
            .rev()
            .take(80)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        out.push('\n');
    } else {
        out += "[failed log unavailable or withheld]\n";
    }
    Ok(Output {
        code: 1,
        stdout: out.into_bytes(),
        stderr: vec![],
    })
}

/// Read-only comparison used by explicit suggestion refresh and triage. A
/// missing counterpart is unknown, never evidence of a preexisting failure.
pub fn comparison(
    repo: &Path,
    branch: &str,
    sha: &str,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<V> {
    let c = Context {
        cwd: repo,
        source: repo,
        interrupted,
        services: &crate::Unavailable,
    };
    let fields = "databaseId,status,conclusion,workflowName,headSha,url";
    let runs = gh(
        &c,
        &[
            "run", "list", "--branch", branch, "--limit", "20", "--json", fields,
        ],
        Duration::from_secs(20),
    )?;
    let run = runs
        .array()
        .iter()
        .find(|r| {
            string(r, "headSha") == sha
                && string(r, "status") == "completed"
                && red(&string(r, "conclusion"))
        })
        .ok_or_else(|| Error::new(2, "no completed red run for comparison"))?;
    let jobs = gh(
        &c,
        &["run", "view", &run.get("databaseId").py(), "--json", "jobs"],
        Duration::from_secs(20),
    )?;
    let failed: Vec<_> = jobs
        .get("jobs")
        .array()
        .iter()
        .filter(|j| red(&string(j, "conclusion")))
        .collect();
    let info = gh(
        &c,
        &["repo", "view", "--json", "defaultBranchRef"],
        Duration::from_secs(20),
    )?;
    let default = string(info.get("defaultBranchRef"), "name");
    require(!default.is_empty(), "base branch unknown")?;
    let base_runs = gh(
        &c,
        &[
            "run", "list", "--branch", &default, "--limit", "20", "--json", fields,
        ],
        Duration::from_secs(20),
    )?;
    let base = base_runs
        .array()
        .iter()
        .find(|r| {
            string(r, "status") == "completed"
                && string(r, "headSha") != sha
                && string(r, "workflowName") == string(run, "workflowName")
        })
        .ok_or_else(|| Error::new(2, "comparable base run unavailable"))?;
    let base_jobs = gh(
        &c,
        &[
            "run",
            "view",
            &base.get("databaseId").py(),
            "--json",
            "jobs",
        ],
        Duration::from_secs(20),
    )?;
    let mut rows = vec![];
    let (mut same, mut regression) = (0usize, false);
    for job in &failed {
        let name = string(job, "name");
        let status = base_jobs
            .get("jobs")
            .array()
            .iter()
            .find(|j| string(j, "name") == name)
            .map(|j| string(j, "conclusion"))
            .unwrap_or_else(|| "unknown".into());
        same += usize::from(red(&status));
        regression |= status == "success";
        rows.push(obj([
            (
                "job",
                oms_contract_markdown::bounded_line(&name, 160).into(),
            ),
            ("base", status.into()),
        ]));
    }
    let class = if regression {
        "regression"
    } else if !failed.is_empty() && same == failed.len() {
        "preexisting"
    } else {
        "unknown"
    };
    Ok(obj([
        ("class", class.into()),
        (
            "note",
            format!(
                "{} {}",
                oms_contract_markdown::bounded_line(&default, 40),
                string(base, "headSha").chars().take(12).collect::<String>()
            )
            .into(),
        ),
        ("jobs", V::Arr(rows)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn run(jobs: &[(&str, &str)]) -> V {
        let jobs: Vec<String> = jobs
            .iter()
            .map(|(n, c)| format!("{{\"name\":\"{n}\",\"conclusion\":\"{c}\"}}"))
            .collect();
        V::parse(
            format!(
                "{{\"headSha\":\"{SHA}\",\"status\":\"completed\",\"conclusion\":\"success\",\"jobs\":[{}]}}",
                jobs.join(",")
            )
            .as_bytes(),
        )
        .unwrap()
    }

    fn pure_rust() -> Vec<(String, &'static str)> {
        let mut jobs: Vec<(String, &str)> = ["lint", "native (ubuntu-latest, copy)", "gate"]
            .iter()
            .map(|n| (n.to_string(), "success"))
            .collect();
        for lane in 1..=4 {
            for family in ["focused", "scripts-smoke", "operator-tools"] {
                jobs.push((format!("{family} ({lane}/4)"), "success"));
            }
        }
        jobs
    }

    fn scope(jobs: &[(String, &str)]) -> Result<&'static str> {
        let borrowed: Vec<(&str, &str)> = jobs.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        full_scope(&run(&borrowed), SHA)
    }

    #[test]
    fn full_scope_accepts_pure_rust_runs_without_python() {
        let jobs = pure_rust();
        assert_eq!(scope(&jobs).unwrap(), "full");
        for extra in [
            ("affected_plan", "success"),
            ("python311", "success"),
            ("python311", "skipped"),
        ] {
            let mut with = jobs.clone();
            with.push((extra.0.into(), extra.1));
            assert_eq!(scope(&with).unwrap(), "full", "{extra:?}");
        }
        for bad in [
            ("affected_plan", "failure"),
            ("affected_plan", "skipped"),
            ("python311", "failure"),
            ("extra", "skipped"),
        ] {
            let mut with = jobs.clone();
            with.push((bad.0.into(), bad.1));
            assert!(scope(&with).is_err(), "{bad:?}");
        }
        let mut missing = jobs.clone();
        missing.retain(|(n, _)| n != "operator-tools (4/4)");
        assert!(scope(&missing).is_err());
        let mut duplicate = jobs.clone();
        duplicate.push(("gate".into(), "success"));
        assert!(scope(&duplicate).is_err());
        assert!(scope(&[]).is_err());
        let affected = [
            ("lint".to_string(), "skipped"),
            ("affected_plan".into(), "success"),
            ("gate".into(), "success"),
        ];
        assert_eq!(scope(&affected).unwrap(), "affected");
        assert!(scope(&affected[..1]).is_err());

        let streamlined = [
            ("affected_plan".to_string(), "success"),
            ("lint".into(), "success"),
            ("rust (ubuntu-24.04)".into(), "success"),
            ("focused".into(), "success"),
            ("native (ubuntu-latest, copy)".into(), "success"),
            ("affected checks".into(), "skipped"),
            ("gate".into(), "success"),
        ];
        assert_eq!(scope(&streamlined).unwrap(), "full");
        for stale in ["native (macos-latest, auto)", "rust (macos-latest)"] {
            let mut with = streamlined.to_vec();
            with.push((stale.into(), "skipped"));
            assert!(scope(&with).is_err(), "{stale}");
        }
    }
}
