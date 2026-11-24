//! Read a page of cards from a board linked to this repository.
use super::super::{digits, plan_issues, spawn, strict, task_issue};
use super::{
    Kind, Number, READ_TIMEOUT, Slug, WEB, display, project_error, projects, stable_id,
    valid_cursor, work_item,
};
use crate::{Context, V, obj};
use std::{collections::BTreeMap, path::Path};

const BOARD_FIELDS: [&str; 4] = ["Status", "Priority", "Area", "Phase"];
const BOARD_QUERY: &str = "query=query($owner:String!,$number:Int!,$after:String){OWNER(login:$owner){login projectV2(number:$number){id number title url closed fields(first:50){pageInfo{hasNextPage} nodes{__typename ... on ProjectV2FieldCommon{id name} ... on ProjectV2SingleSelectField{options{id name}}}} items(first:20,after:$after){pageInfo{hasNextPage endCursor} nodes{type isArchived content{__typename ... on Issue{number title state url repository{nameWithOwner} issueType{name} labels(first:10){pageInfo{hasNextPage} nodes{name}} blockedBy(first:5){pageInfo{hasNextPage} nodes{state}}} ... on PullRequest{number title state url repository{nameWithOwner}}} fieldValues(first:20){pageInfo{hasNextPage} nodes{__typename ... on ProjectV2ItemFieldSingleSelectValue{optionId name field{... on ProjectV2FieldCommon{id}}}}}}}}}}";

/// Explicit board identity: `https://github.com/{orgs|users}/LOGIN/projects/N`.
fn board_target(url: &str) -> Option<(&'static str, String, Number)> {
    let parts: Vec<_> = url.strip_prefix(WEB)?.split('/').collect();
    let [group, login, "projects", number] = parts[..] else {
        return None;
    };
    let group = match group {
        "orgs" => "organization",
        "users" => "user",
        _ => return None,
    };
    (login.len() <= 100 && Slug::parse(&format!("{login}/project")).is_ok()).then_some(())?;
    Some((group, login.to_string(), Number::parse(number).ok()?))
}

fn board_empty(status: &str, reason: Option<&str>) -> V {
    let mut v = obj([
        ("status", status.into()),
        ("board", V::Null),
        ("items", V::Arr(vec![])),
        ("nextCursor", V::Null),
        ("incomplete", true.into()),
    ]);
    if let Some(reason) = reason {
        v.set("reason", reason);
    }
    v
}

/// Selected read-only fields: exact names only; duplicates are ambiguous, never guessed.
type Fields = BTreeMap<&'static str, (String, BTreeMap<String, String>)>;

fn board_fields(connection: &V) -> (V, Fields, bool) {
    let mut fields = Fields::new();
    let mut missing = Vec::new();
    let all = connection.get("pageInfo").get("hasNextPage") == &V::Bool(false);
    for name in BOARD_FIELDS {
        let mut matching = connection
            .get("nodes")
            .array()
            .iter()
            .filter(|v| v.get("name").text() == Some(name));
        if all
            && let Some(field) = matching.next()
            && matching.next().is_none()
            && field.get("__typename").text() == Some("ProjectV2SingleSelectField")
            && let Some(id) = stable_id(field.get("id"))
            && let V::Arr(options) = field.get("options")
        {
            let options_by_id: BTreeMap<_, _> = options
                .iter()
                .take(100)
                .filter_map(|option| {
                    Some((
                        stable_id(option.get("id"))?.to_owned(),
                        display(option.get("name"), 60)?,
                    ))
                })
                .collect();
            if options.len() == options_by_id.len() {
                fields.insert(name, (id.to_owned(), options_by_id));
                continue;
            }
        }
        missing.push(V::from(name));
    }
    let mut report = obj(fields
        .iter()
        .map(|(name, (id, _))| (*name, obj([("id", id.clone().into())]))));
    let partial = !missing.is_empty();
    report.set("missing", V::Arr(missing));
    (report, fields, partial)
}

fn board_item(node: &V, slug: &Slug, chosen: &Fields) -> V {
    let class = |c: &str| obj([("class", c.into())]);
    if node.get("isArchived") != &V::Bool(false) {
        return class(if node.get("isArchived") == &V::Bool(true) {
            "archived"
        } else {
            "unknown"
        });
    }
    let item = node.get("content");
    let kind = match (node.get("type").text(), item.get("__typename").text()) {
        (Some("DRAFT_ISSUE"), _) => return class("draft"),
        (Some("ISSUE"), Some("Issue")) => Kind::Issue,
        (Some("PULL_REQUEST"), Some("PullRequest")) => Kind::Pull,
        _ => return class("unknown"),
    };
    let Some(repository) = item
        .get("repository")
        .get("nameWithOwner")
        .text()
        .and_then(|s| Slug::parse(s).ok())
    else {
        return class("unknown");
    };
    if !repository.matches(slug) {
        return class("foreign");
    }
    let Some(mut row) = work_item(item, slug, kind, false) else {
        return class("unknown");
    };
    let labels = item.get("labels");
    let epic = item
        .get("issueType")
        .get("name")
        .text()
        .is_some_and(|s| s.eq_ignore_ascii_case("epic"))
        || labels.get("nodes").array().iter().any(|l| {
            l.get("name")
                .text()
                .is_some_and(|n| n.eq_ignore_ascii_case("epic"))
        });
    let blocked = item.get("blockedBy");
    let dependencies = if blocked
        .get("nodes")
        .array()
        .iter()
        .any(|n| n.get("state").text() == Some("OPEN"))
    {
        "blocked"
    } else if blocked.get("pageInfo").get("hasNextPage") == &V::Bool(false)
        && let V::Arr(nodes) = blocked.get("nodes")
        && nodes
            .iter()
            .all(|n| n.get("state").text() == Some("CLOSED"))
    {
        "none_observed"
    } else {
        "unknown"
    };
    let classification = match kind {
        Kind::Pull => "pull_request",
        Kind::Issue if row.get("state").text() != Some("OPEN") => "closed",
        Kind::Issue if epic => "epic",
        Kind::Issue
            if labels.get("pageInfo").get("hasNextPage") != &V::Bool(false)
                || !item.has("issueType")
                || !matches!(labels.get("nodes"), V::Arr(_)) =>
        {
            "unknown"
        }
        Kind::Issue if dependencies == "blocked" => "blocked",
        Kind::Issue if dependencies == "unknown" => "unverified",
        Kind::Issue => "candidate",
    };
    row.set("class", classification);
    if matches!(kind, Kind::Issue) {
        row.set("dependencies", dependencies);
    }
    let values = node.get("fieldValues");
    let mut partial = values.get("pageInfo").get("hasNextPage") != &V::Bool(false);
    let mut fields = vec![];
    for (name, (id, options)) in chosen {
        let mut matches = values
            .get("nodes")
            .array()
            .iter()
            .filter(|v| v.get("field").get("id").text() == Some(id));
        let Some(value) = matches.next() else {
            continue;
        };
        if matches.next().is_some()
            || value.get("__typename").text() != Some("ProjectV2ItemFieldSingleSelectValue")
        {
            partial = true;
            continue;
        }
        let Some(option) = stable_id(value.get("optionId")) else {
            partial = true;
            continue;
        };
        let Some(label) = options.get(option) else {
            partial = true;
            continue;
        };
        fields.push((
            *name,
            obj([
                ("fieldId", id.as_str().into()),
                ("optionId", option.into()),
                ("name", label.as_str().into()),
            ]),
        ));
    }
    row.set("fields", obj(fields));
    row.set("incomplete", partial);
    row
}

/// Read one explicitly linked Projects v2 board page. Read-only evidence: neither
/// board state nor item order grants a claim, lease, approval or permission.
pub(super) fn page(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    expected_id: &str,
    cursor: Option<&str>,
) -> crate::Result<V> {
    crate::require(
        stable_id(&V::from(expected_id)).is_some(),
        "invalid project node id",
    )?;
    let Some(slug) = slug else {
        return Ok(board_empty("unsupported", None));
    };
    let prefix = format!(
        "{}.",
        &crate::digest(format!("board:{slug}:{expected_id}").as_bytes())[..16]
    );
    let after = cursor
        .map(|s| {
            s.strip_prefix(&prefix)
                .filter(|s| valid_cursor(s))
                .ok_or_else(|| crate::Error::new(2, "cursor does not match this project"))
        })
        .transpose()?;
    // Recheck the bound repository's links on every page before reading a board.
    let catalog = projects(c, root, slug)?;
    if catalog.get("status").text() != Some("available") {
        return Ok(board_empty("unavailable", catalog.get("reason").text()));
    }
    let mut links = catalog
        .get("items")
        .array()
        .iter()
        .filter(|v| v.get("id").text() == Some(expected_id));
    let Some(canonical) = links.next().and_then(|link| link.get("url").text()) else {
        return Ok(board_empty("unavailable", Some("board_not_in_catalog")));
    };
    let (group, login, number) = board_target(canonical)
        .ok_or_else(|| crate::Error::new(2, "invalid linked board identity"))?;
    if links.next().is_some() {
        return Ok(board_empty("unavailable", Some("identity_mismatch")));
    }
    let query = BOARD_QUERY.replace("OWNER", group);
    let (owner, number_arg) = (format!("owner={login}"), format!("number={number}"));
    let after_arg = after.map(|s| format!("after={s}"));
    let mut args = vec![
        "api",
        "graphql",
        "--hostname",
        "github.com",
        "-f",
        &query,
        "-f",
        &owner,
        "-F",
        &number_arg,
    ];
    if let Some(value) = &after_arg {
        args.extend(["-f", value]);
    }
    let reply = spawn(c, root, &args, READ_TIMEOUT)?;
    if reply.code == 130 {
        return Err(crate::Error::new(130, "interrupted"));
    }
    let v = strict(&reply.stdout).unwrap_or(V::Null);
    let errors = v.get("errors").array();
    let board = v.get("data").get(group).get("projectV2");
    if (reply.code != 0 && !(reply.code == 1 && !errors.is_empty())) || board.get("id") == &V::Null
    {
        return Ok(board_empty(
            "unavailable",
            project_error(reply.code, errors),
        ));
    }
    let node_id = stable_id(board.get("id"));
    let identity = v
        .get("data")
        .get(group)
        .get("login")
        .text()
        .is_some_and(|l| l.eq_ignore_ascii_case(&login))
        && digits(board.get("number")).and_then(|n| Number::parse(n).ok()) == Some(number)
        && board
            .get("url")
            .text()
            .is_some_and(|u| u.eq_ignore_ascii_case(canonical))
        && node_id.is_some()
        && Some(expected_id) == node_id;
    if !identity {
        return Ok(board_empty("unavailable", Some("identity_mismatch")));
    }
    let (fields, chosen, mut incomplete) = board_fields(board.get("fields"));
    incomplete |= !errors.is_empty();
    let (V::Arr(nodes), V::Bool(more), V::Bool(closed)) = (
        board.get("items").get("nodes"),
        board.get("items").get("pageInfo").get("hasNextPage"),
        board.get("closed"),
    ) else {
        return Ok(board_empty("unavailable", Some("partial_response")));
    };
    let planned = c.state(root).ok().filter(|p| p == root).and_then(|state| {
        let mut linked: std::collections::BTreeSet<_> = plan_issues(&state, slug)
            .ok()?
            .into_iter()
            .map(|(_, issue)| issue.number.to_string())
            .collect();
        if let Some(issue) = task_issue(&state, slug).ok()? {
            linked.insert(issue.number.to_string());
        }
        Some(linked)
    });
    let mut seen = std::collections::BTreeSet::new();
    let items: Vec<V> = nodes
        .iter()
        .take(20)
        .map(|n| {
            let mut row = board_item(n, slug, &chosen);
            if row.get("class").text() == Some("candidate")
                && let Some(number) = digits(row.get("number")).map(str::to_owned)
            {
                let class = if !seen.insert(number.clone()) {
                    "duplicate"
                } else if *closed {
                    "closed_board"
                } else if planned
                    .as_ref()
                    .is_some_and(|links| links.contains(&number))
                {
                    "plan_linked"
                } else if planned.is_none()
                    || row.get("incomplete") == &V::Bool(true)
                    || !errors.is_empty()
                {
                    "unverified"
                } else if row
                    .get("fields")
                    .get("Status")
                    .get("name")
                    .text()
                    .is_some_and(|s| s.eq_ignore_ascii_case("In Progress"))
                {
                    "board_in_progress"
                } else {
                    "candidate"
                };
                row.set("class", class);
            }
            row
        })
        .collect();
    incomplete |= planned.is_none()
        || nodes.len() > 20
        || items.iter().any(|i| {
            i.get("incomplete") == &V::Bool(true) || i.get("class").text() == Some("unknown")
        });
    let end = board
        .get("items")
        .get("pageInfo")
        .get("endCursor")
        .text()
        .filter(|s| valid_cursor(s) && Some(*s) != after);
    incomplete |= *more;
    let mut out = obj([
        ("status", "available".into()),
        ("items", V::Arr(items)),
        ("incomplete", incomplete.into()),
    ]);
    out.set(
        "board",
        obj([
            ("id", node_id.unwrap_or_default().into()),
            ("number", V::Num(number.to_string())),
            (
                "title",
                display(board.get("title"), 120).unwrap_or_default().into(),
            ),
            ("url", canonical.into()),
            ("state", if *closed { "CLOSED" } else { "OPEN" }.into()),
            ("owner", login.into()),
            ("fields", fields),
        ]),
    );
    out.set(
        "nextCursor",
        if *more {
            end.map(|s| format!("{prefix}{s}").into())
                .unwrap_or(V::Null)
        } else {
            V::Null
        },
    );
    Ok(out)
}
