//! Live read-only panel detail. A linked checkout owns a PR only when the
//! remote branch and head both occur in this repository's worktree registry.
#[path = "github_board.rs"]
mod board;

use super::{Kind, Number, Slug, WEB, digits, gh_with_timeout, names, origin};
use crate::{Context, Output, V, check, git::Git, now, obj};
use oms_work_inspect::git::{STATUS_ARGS, object_id, ref_label, snapshot};
use std::{collections::BTreeMap, path::Path, time::Duration};

// Three independent reads must finish within the desktop tool's 30-second deadline.
const READ_TIMEOUT: Duration = Duration::from_secs(7);
const PROJECT_QUERY: &str = "query=query($owner:String!,$name:String!){repository(owner:$owner,name:$name){nameWithOwner projectsV2(first:6){pageInfo{hasNextPage} nodes{id number title url closed owner{__typename ... on Organization{login} ... on User{login}} items(first:8){pageInfo{hasNextPage} nodes{type content{__typename ... on Issue{number title state url repository{nameWithOwner}} ... on PullRequest{number title state url repository{nameWithOwner}}}}}}}}}";

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
    let raw = value.text()?;
    crate::safe(raw.as_bytes()).ok()?;
    let s = oms_github::freshness::sanitize(raw, maximum);
    (!s.is_empty() && (s == raw || crate::safe(s.as_bytes()).is_ok())).then_some(s)
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
    bound_rows(&mut matches, 18_000, incomplete);
    matches
}

// The MCP returns all four collections together. Count encoded bytes so long
// Unicode titles and check names cannot make one source hide the other three.
fn bound_rows(rows: &mut Vec<V>, budget: usize, incomplete: &mut bool) {
    let mut remaining = budget;
    if let Some(cut) = rows.iter().position(|row| {
        let bytes = row.json(false).map_or(usize::MAX, |v| v.len() + 1);
        if bytes > remaining {
            return true;
        }
        remaining -= bytes;
        false
    }) {
        rows.truncate(cut);
        *incomplete = true;
    }
}

fn source(status: &str, mut items: Vec<V>, mut incomplete: bool) -> V {
    bound_rows(&mut items, 10_000, &mut incomplete);
    obj([
        ("status", status.into()),
        ("items", V::Arr(items)),
        ("incomplete", incomplete.into()),
    ])
}

fn work_item(row: &V, slug: &Slug, kind: Kind, open_only: bool) -> Option<V> {
    let number = Number::parse(digits(row.get("number"))?).ok()?;
    let state = row.get("state").text()?;
    if !(state == "OPEN"
        || (!open_only && (state == "CLOSED" || (matches!(kind, Kind::Pull) && state == "MERGED"))))
        || !names(row.get("url").text(), WEB, slug, kind.html(), number)
    {
        return None;
    }
    Some(obj([
        ("number", V::Num(number.to_string())),
        ("title", display(row.get("title"), 120)?.into()),
        ("kind", kind.name().into()),
        (
            "url",
            format!("{WEB}{slug}/{}/{number}", kind.html()).into(),
        ),
        ("state", state.into()),
    ]))
}

fn open_work(rows: &[V], slug: &Slug, kind: Kind) -> V {
    let mut incomplete = rows.len() > 12;
    let mut items = Vec::new();
    for row in rows.iter().take(12) {
        if let Some(item) = work_item(row, slug, kind, true) {
            items.push(item);
        } else {
            incomplete = true;
        }
    }
    source("available", items, incomplete)
}

fn project(row: &V, slug: &Slug) -> Option<V> {
    let number = Number::parse(digits(row.get("number"))?).ok()?;
    let owner = row.get("owner");
    let login = owner.get("login").text()?;
    if login.len() > 100 || Slug::parse(&format!("{login}/project")).is_err() {
        return None;
    }
    let group = match owner.get("__typename").text()? {
        "Organization" => "orgs",
        "User" => "users",
        _ => return None,
    };
    let url = format!("{WEB}{group}/{login}/projects/{number}");
    if !row.get("url").text()?.eq_ignore_ascii_case(&url) {
        return None;
    }
    let V::Bool(closed) = row.get("closed") else {
        return None;
    };
    let connection = row.get("items");
    let mut partial = connection.get("pageInfo").get("hasNextPage") != &V::Bool(false);
    let mut items = Vec::new();
    if let V::Arr(nodes) = connection.get("nodes") {
        partial |= nodes.len() > 8;
        for node in nodes.iter().take(8) {
            let item = node.get("content");
            let kind = match (node.get("type").text(), item.get("__typename").text()) {
                (Some("ISSUE"), Some("Issue")) => Kind::Issue,
                (Some("PULL_REQUEST"), Some("PullRequest")) => Kind::Pull,
                (Some("DRAFT_ISSUE"), _) => continue,
                _ => {
                    partial = true;
                    continue;
                }
            };
            let Some(repository) = item
                .get("repository")
                .get("nameWithOwner")
                .text()
                .and_then(|s| Slug::parse(s).ok())
            else {
                partial = true;
                continue;
            };
            if !repository.matches(slug) {
                continue;
            }
            if let Some(item) = work_item(item, slug, kind, false) {
                items.push(item);
            } else {
                partial = true;
            }
        }
    } else {
        partial = true;
    }
    Some(obj([
        (
            "id",
            stable_id(row.get("id")).map(V::from).unwrap_or(V::Null),
        ),
        ("number", V::Num(number.to_string())),
        ("title", display(row.get("title"), 120)?.into()),
        ("url", url.into()),
        ("state", if *closed { "CLOSED" } else { "OPEN" }.into()),
        ("items", V::Arr(items)),
        ("incomplete", partial.into()),
    ]))
}

fn project_error(code: i32, errors: &[V]) -> Option<&'static str> {
    if code == 124 {
        return Some("timeout");
    }
    if errors
        .iter()
        .any(|e| e.get("type").text() == Some("RATE_LIMITED"))
    {
        return Some("rate_limited");
    }
    if errors
        .iter()
        .any(|e| e.get("type").text() == Some("INSUFFICIENT_SCOPES"))
    {
        return Some("insufficient_scope");
    }
    if errors
        .iter()
        .any(|e| e.get("type").text() == Some("FORBIDDEN"))
    {
        return Some("forbidden");
    }
    if errors
        .iter()
        .any(|e| e.get("type").text() == Some("NOT_FOUND"))
    {
        return Some("not_found");
    }
    None
}

fn projects(c: &Context<'_>, root: &Path, slug: &Slug) -> crate::Result<V> {
    let query = [
        "api",
        "graphql",
        "--hostname",
        "github.com",
        "-f",
        PROJECT_QUERY,
        "-f",
        &format!("owner={}", slug.owner),
        "-f",
        &format!("name={}", slug.repo),
    ];
    // gh can return a nonzero code alongside partially readable GraphQL data.
    let reply = super::spawn(c, root, &query, READ_TIMEOUT)?;
    if reply.code == 130 {
        return Err(crate::Error::new(130, "interrupted"));
    }
    let v = super::strict(&reply.stdout).unwrap_or(V::Null);
    let errors = v.get("errors").array();
    let has_errors = !errors.is_empty();
    let unavailable = || {
        let mut out = source("unavailable", vec![], true);
        if let Some(reason) = project_error(reply.code, errors) {
            out.set("reason", reason);
        }
        out
    };
    if reply.code != 0 && !(reply.code == 1 && has_errors) {
        return Ok(unavailable());
    }
    let repo = v.get("data").get("repository");
    if !repo
        .get("nameWithOwner")
        .text()
        .and_then(|s| Slug::parse(s).ok())
        .is_some_and(|s| s.matches(slug))
    {
        return Ok(unavailable());
    }
    let connection = repo.get("projectsV2");
    let V::Arr(nodes) = connection.get("nodes") else {
        return Ok(unavailable());
    };
    let mut incomplete = has_errors
        || connection.get("pageInfo").get("hasNextPage") != &V::Bool(false)
        || nodes.len() > 6;
    let mut boards = Vec::new();
    for node in nodes.iter().take(6) {
        let Some(board) = project(node, slug) else {
            incomplete = true;
            continue;
        };
        incomplete |= board.get("incomplete") == &V::Bool(true);
        boards.push(board);
    }
    Ok(source("available", boards, incomplete))
}

fn stable_id(value: &V) -> Option<&str> {
    value.text().filter(|s| {
        s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            && s.len() <= 100
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn valid_cursor(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 480
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b))
}

fn inventory(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    collection: &str,
    cursor: Option<&str>,
) -> crate::Result<V> {
    let kind = match collection {
        "issues" => Kind::Issue,
        "pullRequests" => Kind::Pull,
        _ => return Err(crate::Error::new(2, "invalid GitHub collection")),
    };
    let empty = |status: &str| {
        obj([
            ("kind", collection.into()),
            ("status", status.into()),
            ("items", V::Arr(vec![])),
            ("nextCursor", V::Null),
            ("incomplete", true.into()),
        ])
    };
    let Some(slug) = slug else {
        return Ok(empty("unsupported"));
    };
    let key =
        crate::digest(format!("{}:{collection}", slug.to_string().to_ascii_lowercase()).as_bytes());
    let prefix = format!("{}.", &key[..16]);
    let after = cursor
        .map(|s| {
            s.strip_prefix(&prefix)
                .filter(|s| valid_cursor(s))
                .ok_or_else(|| crate::Error::new(2, "cursor does not match this GitHub collection"))
        })
        .transpose()?;
    // COLLECTION is one of the two fixed fields above. All remote/user values are GraphQL variables.
    let query = "query=query($owner:String!,$name:String!,$after:String){repository(owner:$owner,name:$name){nameWithOwner work:COLLECTION(first:12,states:[OPEN],after:$after,orderBy:{field:CREATED_AT,direction:ASC}){pageInfo{hasNextPage endCursor} nodes{number title state url}}}}".replace("COLLECTION",collection);
    let owner = format!("owner={}", slug.owner);
    let name = format!("name={}", slug.repo);
    let mut args = vec![
        "api",
        "graphql",
        "--hostname",
        "github.com",
        "-f",
        &query,
        "-f",
        &owner,
        "-f",
        &name,
    ];
    let after_arg = after.map(|s| format!("after={s}"));
    if let Some(value) = &after_arg {
        args.extend(["-f", value]);
    }
    let data = match gh_with_timeout(c, root, &args, READ_TIMEOUT) {
        Ok(v) => v,
        Err(e) if e.code == 130 => return Err(e),
        Err(_) => return Ok(empty("unavailable")),
    };
    let repo = data.get("data").get("repository");
    if !repo
        .get("nameWithOwner")
        .text()
        .and_then(|s| Slug::parse(s).ok())
        .is_some_and(|s| s.matches(slug))
    {
        return Ok(empty("unavailable"));
    }
    let work = repo.get("work");
    let (V::Arr(rows), V::Bool(more)) =
        (work.get("nodes"), work.get("pageInfo").get("hasNextPage"))
    else {
        return Ok(empty("unavailable"));
    };
    let mut page = open_work(rows, slug, kind);
    page.set("kind", collection);
    let end = work
        .get("pageInfo")
        .get("endCursor")
        .text()
        .filter(|s| valid_cursor(s) && Some(*s) != after);
    if *more {
        page.set("incomplete", true);
    }
    page.set(
        "nextCursor",
        if *more {
            end.map(|s| format!("{prefix}{s}").into())
                .unwrap_or(V::Null)
        } else {
            V::Null
        },
    );
    Ok(page)
}

fn lookup(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    kind: Kind,
    number: Number,
) -> crate::Result<V> {
    let response = |status: &str, item: V| obj([("status", status.into()), ("item", item)]);
    let Some(slug) = slug else {
        return Ok(response("unsupported", V::Null));
    };
    let cmd = if matches!(kind, Kind::Issue) {
        "issue"
    } else {
        "pr"
    };
    let read = gh_with_timeout(
        c,
        root,
        &[
            cmd,
            "view",
            &number.to_string(),
            "--repo",
            &format!("github.com/{slug}"),
            "--json",
            "number,title,url,state",
        ],
        READ_TIMEOUT,
    );
    let item = match read {
        Ok(v) if digits(v.get("number")).and_then(|n| Number::parse(n).ok()) == Some(number) => {
            work_item(&v, slug, kind, false)
        }
        Err(e) if e.code == 130 => return Err(e),
        _ => None,
    };
    Ok(match item {
        Some(item) if item.get("state").text() == Some("OPEN") => response("available", item),
        Some(_) => response("closed", V::Null),
        None => response("unavailable", V::Null),
    })
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
    let slug = root
        .as_ref()
        .and_then(|_| origin(&git).ok())
        .filter(|s| s.owner.len() <= 100 && s.repo.len() <= 100);
    let look_up = o.has("issue") || o.has("pull");
    if o.has("drift") {
        crate::require(o.has("project-id"), "--drift requires --project-id NODE_ID")?;
        crate::require(!o.has("cursor"), "--drift cannot combine with --cursor")?;
        crate::require(
            !look_up && !o.has("inventory"),
            "project cannot combine with issue, pull or inventory",
        )?;
        let observed_at = now();
        let report = board::drift(
            c,
            git.repo,
            slug.as_ref(),
            o.get("project-id"),
            &observed_at,
        )?;
        return if o.has("json") {
            Output::json(&obj([
                ("git", checkout),
                (
                    "github",
                    obj([
                        (
                            "repository",
                            slug.map(|s| s.to_string().into()).unwrap_or(V::Null),
                        ),
                        ("observedAt", observed_at.into()),
                        ("drift", report),
                    ]),
                ),
            ]))
        } else {
            Ok(Output::text(board::render_drift(
                &report,
                slug.as_ref(),
                o.get("project-id"),
            )))
        };
    }
    if look_up || o.has("inventory") || o.has("cursor") || o.has("project-id") {
        let (key, detail) = if o.has("project-id") {
            crate::require(
                !look_up && !o.has("inventory"),
                "project cannot combine with issue, pull or inventory",
            )?;
            (
                "board",
                board::page(
                    c,
                    git.repo,
                    slug.as_ref(),
                    o.get("project-id"),
                    o.has("cursor").then(|| o.get("cursor")),
                )?,
            )
        } else if look_up {
            crate::require(
                !o.has("inventory")
                    && !o.has("cursor")
                    && !o.has("project-id")
                    && o.all("issue").len() + o.all("pull").len() == 1,
                "select one issue or pull number",
            )?;
            let kind = if o.has("issue") {
                Kind::Issue
            } else {
                Kind::Pull
            };
            let number = Number::parse(o.get(kind.name()))
                .map_err(|_| crate::Error::new(2, "invalid issue or pull number"))?;
            ("lookup", lookup(c, git.repo, slug.as_ref(), kind, number)?)
        } else {
            (
                "inventory",
                inventory(
                    c,
                    git.repo,
                    slug.as_ref(),
                    o.get("inventory"),
                    o.has("cursor").then(|| o.get("cursor")),
                )?,
            )
        };
        return Output::json(&obj([
            ("git", checkout),
            (
                "github",
                obj([
                    (
                        "repository",
                        slug.map(|s| s.to_string().into()).unwrap_or(V::Null),
                    ),
                    ("observedAt", now().into()),
                    (key, detail),
                ]),
            ),
        ]));
    }
    let mut incomplete = false;
    let mut list = vec![];
    let missing = if slug.is_some() {
        "unavailable"
    } else {
        "unsupported"
    };
    let mut work =
        obj(["issues", "pullRequests", "projects"].map(|key| (key, source(missing, vec![], true))));
    let status = if let Some(ref slug) = slug {
        let registry = git.run(&["worktree", "list", "--porcelain", "-z"]);
        incomplete = registry.is_err() || !matches!(checkout.get("available"), V::Bool(true));
        let registry = registry.unwrap_or_default();
        let repo = format!("github.com/{slug}");
        match gh_with_timeout(
            c,
            git.repo,
            &[
                "issue",
                "list",
                "--repo",
                &repo,
                "--state",
                "open",
                "--limit",
                "13",
                "--json",
                "number,title,url,state",
            ],
            READ_TIMEOUT,
        ) {
            Ok(V::Arr(rows)) => work.set("issues", open_work(&rows, slug, Kind::Issue)),
            Err(e) if e.code == 130 => return Err(e),
            _ => {}
        }
        match projects(c, git.repo, slug) {
            Ok(data) => work.set("projects", data),
            Err(e) if e.code == 130 => return Err(e),
            _ => {}
        }
        match gh_with_timeout(
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
                "number,title,url,state,isDraft,headRefName,headRefOid,isCrossRepository,baseRefName,statusCheckRollup",
            ],
            READ_TIMEOUT,
        ) {
            Ok(V::Arr(rows)) => {
                work.set("pullRequests", open_work(&rows, slug, Kind::Pull));
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
                ("work", work),
                ("pullRequests", V::Arr(list)),
                ("incomplete", incomplete.into()),
            ]),
        ),
    ]))
}
