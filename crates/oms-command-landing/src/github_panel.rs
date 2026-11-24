//! Live read-only panel detail. A linked checkout owns a PR only when the
//! remote branch and head both occur in this repository's worktree registry.
use super::{Number, Slug, digits, gh, origin};
use crate::{Context, Output, V, check, git::Git, now, obj};
use oms_work_inspect::git::{STATUS_ARGS, object_id, ref_label, snapshot};
use std::collections::BTreeMap;

fn worktrees(raw: &[u8]) -> BTreeMap<&str, &str> {
    let mut result = BTreeMap::new();
    let (mut head, mut branch, mut stale) = (None, None, false);
    for field in raw.split(|b| *b == 0) {
        if field.is_empty() {
            if !stale && let (Some(branch), Some(head)) = (branch, head) {
                result.insert(branch, head);
            }
            (head, branch, stale) = (None, None, false);
        } else if let Some(value) = field.strip_prefix(b"HEAD ") {
            head = std::str::from_utf8(value).ok().filter(|v| object_id(v));
        } else if let Some(value) = field.strip_prefix(b"branch refs/heads/") {
            branch = std::str::from_utf8(value).ok().and_then(ref_label);
        } else if field == b"prunable" || field.starts_with(b"prunable ") {
            stale = true;
        }
    }
    result
}

fn display(value: &V, maximum: usize) -> Option<String> {
    let s = oms_github::freshness::sanitize(value.text()?, maximum);
    (!s.is_empty() && crate::safe(s.as_bytes()).is_ok()).then_some(s)
}

fn checks(rollup: &V, incomplete: &mut bool) -> V {
    let V::Arr(rows) = rollup else {
        *incomplete = true;
        return V::Null;
    };
    // gh bounds the contexts returned in statusCheckRollup.
    *incomplete |= rows.len() >= 100;
    let (mut passed, mut failed, mut pending, mut other) = (0usize, 0usize, 0usize, 0usize);
    let mut named = Vec::new();
    for row in rows {
        let state = match row.get("__typename").text() {
            Some("CheckRun") if row.get("status").text() == Some("COMPLETED") => {
                row.get("conclusion")
            }
            Some("CheckRun") => row.get("status"),
            Some("StatusContext") => row.get("state"),
            _ => &V::Null,
        };
        let (group, count) = match state.text() {
            Some("SUCCESS") => ("passed", &mut passed),
            Some(
                "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
                | "STARTUP_FAILURE" | "STALE",
            ) => ("failed", &mut failed),
            Some("EXPECTED" | "PENDING" | "QUEUED" | "IN_PROGRESS" | "REQUESTED" | "WAITING") => {
                ("pending", &mut pending)
            }
            _ => ("other", &mut other),
        };
        *count += 1;
        let label = if row.get("__typename").text() == Some("StatusContext") {
            row.get("context")
        } else {
            row.get("name")
        };
        if let Some(name) = label.text() {
            named.push((
                match group {
                    "failed" => 0,
                    "pending" => 1,
                    "other" => 2,
                    _ => 3,
                },
                group,
                name,
            ));
        }
    }
    named.sort_by_key(|entry| entry.0);
    let items = named
        .into_iter()
        .take(6)
        .filter_map(|(_, group, name)| {
            display(&name.into(), 96)
                .map(|label| obj([("name", label.into()), ("status", group.into())]))
        })
        .collect();
    obj([
        ("passed", passed.into()),
        ("failed", failed.into()),
        ("pending", pending.into()),
        ("other", other.into()),
        ("items", V::Arr(items)),
    ])
}

fn pulls(
    rows: &[V],
    slug: &Slug,
    checkout: &V,
    linked: &BTreeMap<&str, &str>,
    incomplete: &mut bool,
) -> Vec<V> {
    let mut matches = Vec::new();
    for row in rows {
        if matches!(row.get("isCrossRepository"), V::Bool(true)) {
            continue;
        }
        let Some(branch) = row.get("headRefName").text().and_then(ref_label) else {
            *incomplete = true;
            continue;
        };
        let current = checkout.get("branch").text() == Some(branch);
        if !current && !linked.contains_key(branch) {
            continue;
        }
        let (Some(number), Some(head), Some(base), V::Bool(draft), V::Bool(false), Some("OPEN")) = (
            digits(row.get("number")).and_then(|n| Number::parse(n).ok()),
            row.get("headRefOid").text().filter(|s| object_id(s)),
            row.get("baseRefName").text().and_then(ref_label),
            row.get("isDraft"),
            row.get("isCrossRepository"),
            row.get("state").text(),
        ) else {
            *incomplete = true;
            continue;
        };
        let same = if current {
            checkout.get("head").text() == Some(head)
        } else {
            linked.get(branch).copied() == Some(head)
        };
        if !current && !same {
            continue;
        }
        matches.push(obj([
            ("number", V::Num(number.to_string())),
            (
                "title",
                display(row.get("title"), 160)
                    .map(V::from)
                    .unwrap_or(V::Null),
            ),
            (
                "url",
                format!("https://github.com/{slug}/pull/{number}").into(),
            ),
            ("head", head.into()),
            ("headBranch", branch.into()),
            ("baseBranch", base.into()),
            ("draft", (*draft).into()),
            ("state", "OPEN".into()),
            (
                "scope",
                if current {
                    "checkout"
                } else {
                    "linkedWorktree"
                }
                .into(),
            ),
            ("matchesLocalHead", same.into()),
            ("checks", checks(row.get("statusCheckRollup"), incomplete)),
        ]));
    }
    matches.sort_by_key(|v| v.get("scope").text() != Some("checkout"));
    *incomplete |= matches.len() > 8;
    matches.truncate(8);
    matches
}

pub(super) fn status(c: &Context<'_>, o: &crate::cli::Options) -> crate::Result<Output> {
    let requested = o.path("repo", c.cwd);
    let path = std::fs::canonicalize(requested.as_deref().unwrap_or(c.cwd))?;
    crate::require(path.is_dir(), "project directory unavailable")?;
    let root = Git::root(&path, c.interrupted)
        .ok()
        .filter(|root| requested.is_none() || *root == path);
    let git = Git::new(root.as_deref().unwrap_or(&path), c.interrupted);
    let status = root
        .as_ref()
        .and_then(|_| git.run(STATUS_ARGS).ok())
        .unwrap_or_default();
    let checkout = V::parse(snapshot(&status).to_string().as_bytes())?;
    let slug = root.as_ref().and_then(|_| origin(&git).ok());
    let mut incomplete = false;
    let mut list = vec![];
    let status = if let Some(ref slug) = slug {
        let registry = git.run(&["worktree", "list", "--porcelain", "-z"]);
        incomplete = registry.is_err() || !matches!(checkout.get("available"), V::Bool(true));
        let registry = registry.unwrap_or_default();
        let repo = format!("github.com/{slug}");
        match gh(
            c,
            git.repo,
            &[
                "pr",
                "list",
                "--repo",
                &repo,
                "--state",
                "open",
                "--limit",
                "51",
                "--json",
                "number,title,state,isDraft,headRefName,headRefOid,isCrossRepository,baseRefName,statusCheckRollup",
            ],
        ) {
            Ok(V::Arr(rows)) => {
                incomplete |= rows.len() >= 51;
                list = pulls(
                    &rows,
                    slug,
                    &checkout,
                    &worktrees(&registry),
                    &mut incomplete,
                );
                "available"
            }
            Err(e) if e.code == 130 => return Err(e),
            _ => "unavailable",
        }
    } else {
        "unsupported"
    };
    check(c.interrupted)?;
    Output::json(&obj([
        ("git", checkout),
        (
            "github",
            obj([
                ("status", status.into()),
                (
                    "repository",
                    slug.map(|s| s.to_string().into()).unwrap_or(V::Null),
                ),
                ("observedAt", now().into()),
                ("pullRequests", V::Arr(list)),
                ("incomplete", incomplete.into()),
            ]),
        ),
    ]))
}
