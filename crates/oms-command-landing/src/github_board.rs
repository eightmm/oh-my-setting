//! Read a page of cards, whole-board ranked candidates, or a drift report from a board linked to this repository.
use super::super::{IssueRef, digits, plan_issue_evidence, spawn, strict, task_issue_evidence};
use super::{
    Kind, Number, READ_TIMEOUT, Slug, WEB, bound_rows, display, project_error, projects, stable_id,
    valid_cursor, work_item,
};
use crate::{Context, V, obj};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
};

const BOARD_FIELDS: [&str; 4] = ["Status", "Priority", "Area", "Phase"];
const BOARD_QUERY: &str = "query=query($owner:String!,$number:Int!,$after:String){OWNER(login:$owner){login projectV2(number:$number){id number title url closed fields(first:50){pageInfo{hasNextPage} nodes{__typename ... on ProjectV2FieldCommon{id name} ... on ProjectV2SingleSelectField{options{id name}}}} items(first:20,after:$after){pageInfo{hasNextPage endCursor} nodes{type isArchived content{__typename ... on Issue{number title state url repository{nameWithOwner} issueType{name} labels(first:10){pageInfo{hasNextPage} nodes{name}} blockedBy(first:5){pageInfo{hasNextPage} nodes{state}}} ... on PullRequest{number title state url repository{nameWithOwner}}} fieldValues(first:20){pageInfo{hasNextPage} nodes{__typename ... on ProjectV2ItemFieldSingleSelectValue{optionId name field{... on ProjectV2FieldCommon{id}}}}}}}}}}";
const OPEN_WORK_QUERY: &str = "query=query($owner:String!,$name:String!,$after:String){repository(owner:$owner,name:$name){nameWithOwner work:COLLECTION(first:100,states:[OPEN],after:$after,orderBy:{field:CREATED_AT,direction:DESC}){pageInfo{hasNextPage endCursor} nodes{number title state url}}}}";
const DONE_STATUS_NAMES: [&str; 3] = ["Done", "Completed", "Closed"];
const MAX_BOARD_PAGES: usize = 25;
const MAX_WORK_PAGES: usize = 10;
const STATUS_DRIFT_LIMIT: usize = 50;
const MISSING_LIMIT: usize = 20;
const CANDIDATE_LIMIT: usize = 25;
const CANDIDATES_WALK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
const CANDIDATES_BYTES: usize = 10_000;
const RECOMMENDATION_LIMIT: usize = 100;
const RECOMMENDATIONS_BYTES: usize = 8_000;

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
type FieldOrder = BTreeMap<&'static str, BTreeMap<String, usize>>;
type OrderedFields = BTreeMap<&'static str, (String, Vec<(String, String)>)>;

fn single_select_field(connection: &V, name: &str) -> Option<(String, Vec<(String, String)>)> {
    if connection.get("pageInfo").get("hasNextPage") != &V::Bool(false) {
        return None;
    }
    let mut matching = connection
        .get("nodes")
        .array()
        .iter()
        .filter(|v| v.get("name").text() == Some(name));
    let field = matching.next()?;
    if matching.next().is_some()
        || field.get("__typename").text() != Some("ProjectV2SingleSelectField")
    {
        return None;
    }
    let id = stable_id(field.get("id"))?.to_owned();
    let V::Arr(options) = field.get("options") else {
        return None;
    };
    let mut ordered = Vec::new();
    let mut seen = BTreeSet::new();
    for option in options.iter().take(100) {
        let opt_id = stable_id(option.get("id"))?.to_owned();
        let label = display(option.get("name"), 60)?;
        if !seen.insert(opt_id.clone()) {
            return None;
        }
        ordered.push((opt_id, label));
    }
    (options.len() == ordered.len()).then_some((id, ordered))
}

fn board_fields_with_order(connection: &V) -> (V, Fields, FieldOrder, OrderedFields, bool) {
    let mut fields = Fields::new();
    let mut ranks = FieldOrder::new();
    let mut ordered_fields = OrderedFields::new();
    let mut missing = Vec::new();
    for name in BOARD_FIELDS {
        if let Some((id, ordered)) = single_select_field(connection, name) {
            let order_map = ordered
                .iter()
                .enumerate()
                .map(|(idx, (opt_id, _))| (opt_id.clone(), idx))
                .collect();
            let by_id = ordered.iter().cloned().collect();
            ranks.insert(name, order_map);
            ordered_fields.insert(name, (id.clone(), ordered));
            fields.insert(name, (id, by_id));
        } else {
            missing.push(V::from(name));
        }
    }
    let mut report = obj(fields
        .iter()
        .map(|(name, (id, _))| (*name, obj([("id", id.clone().into())]))));
    let partial = !missing.is_empty();
    report.set("missing", V::Arr(missing));
    (report, fields, ranks, ordered_fields, partial)
}

fn board_fields(connection: &V) -> (V, Fields, bool) {
    let (report, fields, _, _, partial) = board_fields_with_order(connection);
    (report, fields, partial)
}

/// The browse projection may shorten display labels. A decision uses exact
/// option names and rejects identities that the schema does not distinguish.
fn recommendation_schema(connection: &V, fields: &Fields) -> bool {
    fields.len() == BOARD_FIELDS.len()
        && fields.values().all(|(id, options)| {
            let mut matching = connection
                .get("nodes")
                .array()
                .iter()
                .filter(|field| field.get("id").text() == Some(id));
            let Some(field) = matching.next() else {
                return false;
            };
            matching.next().is_none()
                && options.values().collect::<BTreeSet<_>>().len() == options.len()
                && field.get("options").array().iter().all(|option| {
                    option
                        .get("id")
                        .text()
                        .and_then(|id| options.get(id))
                        .map(String::as_str)
                        == option.get("name").text()
                })
        })
}

fn recommendation(row: &V, node: &V, incomplete: Option<&str>, duplicate: bool) -> V {
    let class = row.get("class").text().unwrap_or("unknown");
    let fields = row.get("fields");
    let values = node.get("fieldValues").get("nodes").array();
    let issue = node.get("content");
    let (state, reason) = if duplicate {
        ("excluded", "duplicate")
    } else if !matches!(class, "candidate" | "unknown" | "unverified") {
        ("excluded", class)
    } else if let Some(reason) = incomplete {
        ("unverified", reason)
    } else if row.get("dependencies").text() == Some("unknown") {
        ("unverified", "dependencies_unknown")
    } else if class != "candidate"
        || row.get("incomplete") != &V::Bool(false)
        || !(issue.get("issueType") == &V::Null
            || issue
                .get("issueType")
                .get("name")
                .text()
                .is_some_and(|s| !s.trim().is_empty()))
        || issue
            .get("labels")
            .get("nodes")
            .array()
            .iter()
            .any(|n| n.get("name").text().is_none_or(|s| s.trim().is_empty()))
    {
        ("unverified", "item_evidence_incomplete")
    } else if !BOARD_FIELDS.iter().all(|name| {
        let selection = fields.get(name);
        selection.get("optionId").text().is_some()
            && values.iter().any(|v| {
                v.get("field").get("id") == selection.get("fieldId")
                    && v.get("optionId") == selection.get("optionId")
                    && v.get("name") == selection.get("name")
            })
    }) {
        ("unverified", "field_selection_unverified")
    } else if fields.get("Status").get("name").text() != Some("Ready") {
        ("excluded", "status_not_ready")
    } else if !matches!(
        fields.get("Priority").get("name").text(),
        Some("P0" | "P1" | "P2")
    ) {
        ("unverified", "priority_unrecognized")
    } else {
        ("recommended", "ready_with_priority")
    };
    obj([("state", state.into()), ("reason", reason.into())])
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

struct ResolvedBoard {
    group: &'static str,
    login: String,
    number: Number,
    canonical: String,
}

enum BoardLookup {
    Ready(ResolvedBoard),
    Unavailable {
        status: &'static str,
        reason: Option<String>,
    },
}

fn resolve_board(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    expected_id: &str,
) -> crate::Result<BoardLookup> {
    let Some(slug) = slug else {
        return Ok(BoardLookup::Unavailable {
            status: "unsupported",
            reason: None,
        });
    };
    let catalog = projects(c, root, slug)?;
    if catalog.get("status").text() != Some("available") {
        return Ok(BoardLookup::Unavailable {
            status: "unavailable",
            reason: catalog.get("reason").text().map(str::to_owned),
        });
    }
    let mut links = catalog
        .get("items")
        .array()
        .iter()
        .filter(|v| v.get("id").text() == Some(expected_id));
    let Some(canonical) = links.next().and_then(|link| link.get("url").text()) else {
        return Ok(BoardLookup::Unavailable {
            status: "unavailable",
            reason: Some("board_not_in_catalog".into()),
        });
    };
    let (group, login, number) = board_target(canonical)
        .ok_or_else(|| crate::Error::new(2, "invalid linked board identity"))?;
    if links.next().is_some() {
        return Ok(BoardLookup::Unavailable {
            status: "unavailable",
            reason: Some("identity_mismatch".into()),
        });
    }
    Ok(BoardLookup::Ready(ResolvedBoard {
        group,
        login,
        number,
        canonical: canonical.to_owned(),
    }))
}

// A present malformed GraphQL errors member cannot establish a complete response.
fn response_has_errors(reply: &V) -> bool {
    reply.has("errors") && !matches!(reply.get("errors"), V::Arr(errors) if errors.is_empty())
}

enum BoardPageRaw {
    Ok {
        board: V,
        has_errors: bool,
        error_reason: Option<&'static str>,
    },
    Failed(Option<&'static str>),
}

fn fetch_board_raw(
    c: &Context<'_>,
    root: &Path,
    target: &ResolvedBoard,
    expected_id: &str,
    after: Option<&str>,
    classify: fn(i32, &[V]) -> Option<&'static str>,
) -> crate::Result<BoardPageRaw> {
    let query = BOARD_QUERY.replace("OWNER", target.group);
    let (owner, number_arg) = (
        format!("owner={}", target.login),
        format!("number={}", target.number),
    );
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
    let board = v.get("data").get(target.group).get("projectV2");
    let err_reason = classify(reply.code, errors);
    if (reply.code != 0 && !(reply.code == 1 && !errors.is_empty())) || board.get("id") == &V::Null
    {
        return Ok(BoardPageRaw::Failed(err_reason));
    }
    let node_id = stable_id(board.get("id"));
    let identity = v
        .get("data")
        .get(target.group)
        .get("login")
        .text()
        .is_some_and(|l| l.eq_ignore_ascii_case(&target.login))
        && digits(board.get("number")).and_then(|n| Number::parse(n).ok()) == Some(target.number)
        && board
            .get("url")
            .text()
            .is_some_and(|u| u.eq_ignore_ascii_case(&target.canonical))
        && node_id.is_some()
        && Some(expected_id) == node_id;
    if !identity {
        return Ok(BoardPageRaw::Failed(Some("identity_mismatch")));
    }
    Ok(BoardPageRaw::Ok {
        board: board.clone(),
        has_errors: response_has_errors(&v),
        error_reason: err_reason,
    })
}

fn planned_issues(c: &Context<'_>, root: &Path, slug: &Slug) -> Option<(BTreeSet<String>, bool)> {
    c.state(root).ok().filter(|p| p == root).and_then(|state| {
        let (links, complete) = plan_issue_evidence(&state, slug).ok()?;
        let mut linked: BTreeSet<_> = links
            .into_iter()
            .map(|(_, issue)| issue.number.to_string())
            .collect();
        let (packet, packet_complete) = task_issue_evidence(&state, slug).ok()?;
        if let Some(issue) = packet {
            linked.insert(issue.number.to_string());
        }
        Some((linked, complete && packet_complete))
    })
}

fn card_evidence_gap(
    partial_page: bool,
    planned: &Option<(BTreeSet<String>, bool)>,
    schema_ok: bool,
) -> Option<&'static str> {
    if partial_page {
        Some("partial_response")
    } else if !planned.as_ref().is_some_and(|(_, complete)| *complete) {
        Some("plan_unverified")
    } else if !schema_ok {
        Some("field_schema_unverified")
    } else {
        None
    }
}

fn refine_candidate_class(
    row: &mut V,
    duplicate: bool,
    closed: bool,
    planned: &Option<(BTreeSet<String>, bool)>,
    number: &str,
    page_unverified: bool,
) {
    let class = if duplicate {
        "duplicate"
    } else if closed {
        "closed_board"
    } else if planned
        .as_ref()
        .is_some_and(|(links, _)| links.contains(number))
    {
        "plan_linked"
    } else if planned.is_none() || row.get("incomplete") == &V::Bool(true) || page_unverified {
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

fn verified_option_rank(row: &V, node: &V, field_name: &'static str, ranks: &FieldOrder) -> usize {
    let selection = row.get("fields").get(field_name);
    let Some(option_id) = selection.get("optionId").text() else {
        return usize::MAX;
    };
    let verified = node
        .get("fieldValues")
        .get("nodes")
        .array()
        .iter()
        .any(|v| {
            v.get("field").get("id") == selection.get("fieldId")
                && v.get("optionId") == selection.get("optionId")
                && v.get("name") == selection.get("name")
        });
    if !verified {
        return usize::MAX;
    }
    ranks
        .get(field_name)
        .and_then(|m| m.get(option_id).copied())
        .unwrap_or(usize::MAX)
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
    let target = match resolve_board(c, root, Some(slug), expected_id)? {
        BoardLookup::Ready(target) => target,
        BoardLookup::Unavailable { status, reason } => {
            return Ok(board_empty(status, reason.as_deref()));
        }
    };
    let (board, has_errors) =
        match fetch_board_raw(c, root, &target, expected_id, after, project_error)? {
            BoardPageRaw::Ok {
                board, has_errors, ..
            } => (board, has_errors),
            BoardPageRaw::Failed(reason) => return Ok(board_empty("unavailable", reason)),
        };
    let (fields, chosen, mut incomplete) = board_fields(board.get("fields"));
    incomplete |= has_errors;
    let (V::Arr(nodes), V::Bool(more), V::Bool(closed)) = (
        board.get("items").get("nodes"),
        board.get("items").get("pageInfo").get("hasNextPage"),
        board.get("closed"),
    ) else {
        return Ok(board_empty("unavailable", Some("partial_response")));
    };
    let planned = planned_issues(c, root, slug);
    let evidence_gap = card_evidence_gap(
        has_errors || nodes.len() > 20,
        &planned,
        recommendation_schema(board.get("fields"), &chosen),
    );
    // Keep both copies out of recommendations, even when an archived or closed
    // copy was omitted from browse candidates. This comparison is page-local;
    // `candidates(...)` performs the cross-page canonical Issue deduplication.
    let mut issue_numbers = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for node in nodes.iter().take(20) {
        let issue = node.get("content");
        if issue.get("__typename").text() == Some("Issue")
            && issue
                .get("repository")
                .get("nameWithOwner")
                .text()
                .and_then(|s| Slug::parse(s).ok())
                .is_some_and(|r| r.matches(slug))
            && let Some(number) = digits(issue.get("number"))
            && !issue_numbers.insert(number)
        {
            duplicates.insert(number);
        }
    }
    let mut seen = BTreeSet::new();
    let items: Vec<V> = nodes
        .iter()
        .take(20)
        .map(|n| {
            let mut row = board_item(n, slug, &chosen);
            if row.get("class").text() == Some("candidate")
                && let Some(number) = digits(row.get("number")).map(str::to_owned)
            {
                let seen_dup = !seen.insert(number.clone());
                refine_candidate_class(&mut row, seen_dup, *closed, &planned, &number, has_errors);
            }
            let duplicate = row.get("kind").text() == Some("issue")
                && digits(row.get("number")).is_some_and(|n| duplicates.contains(n));
            let advice = recommendation(&row, n, evidence_gap, duplicate);
            row.set("recommendation", advice);
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
            ("id", expected_id.into()),
            ("number", V::Num(target.number.to_string())),
            (
                "title",
                display(board.get("title"), 120).unwrap_or_default().into(),
            ),
            ("url", target.canonical.as_str().into()),
            ("state", if *closed { "CLOSED" } else { "OPEN" }.into()),
            ("owner", target.login.as_str().into()),
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

fn drift_error(code: i32, errors: &[V]) -> Option<&'static str> {
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
    project_error(code, errors)
}

fn reason_rank(reason: &str) -> u8 {
    match reason {
        "insufficient_scope" => 0,
        "forbidden" => 1,
        "rate_limited" => 2,
        "timeout" => 3,
        "not_found" => 4,
        "board_not_in_catalog" => 5,
        "identity_mismatch" => 6,
        "page_limit" => 7,
        "status_field_unavailable" => 8,
        "partial_response" => 9,
        "plan_unverified" => 10,
        "field_schema_unverified" => 11,
        "result_limit" => 12,
        "unavailable" => 13,
        "unsupported" => 14,
        _ => 15,
    }
}

fn record_reason(slot: &mut Option<&'static str>, candidate: &'static str) {
    if slot.is_none_or(|current| reason_rank(candidate) < reason_rank(current)) {
        *slot = Some(candidate);
    }
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        "insufficient_scope" => "permission missing (insufficient_scope)",
        "forbidden" => "permission missing (forbidden)",
        "rate_limited" => "rate limit (rate_limited)",
        "timeout" => "timeout",
        "not_found" => "board or repository not found (not_found)",
        "board_not_in_catalog" => "board not linked to this repository (board_not_in_catalog)",
        "identity_mismatch" => "board identity mismatch (identity_mismatch)",
        "page_limit" => "pagination page limit reached (page_limit)",
        "status_field_unavailable" => {
            "Status field missing, duplicated, or truncated (status_field_unavailable)"
        }
        "partial_response" => "incomplete or truncated response (partial_response)",
        "unsupported" => "origin is not a github.com repository (unsupported)",
        "unavailable" => "GitHub read failed or unavailable (unavailable)",
        _ => "incomplete read",
    }
}

struct WorkWalk {
    items: Vec<(Number, V)>,
    complete: bool,
    pages: usize,
    reason: Option<&'static str>,
}

impl WorkWalk {
    fn skipped(reason: &'static str) -> Self {
        Self {
            items: Vec::new(),
            complete: false,
            pages: 0,
            reason: Some(reason),
        }
    }
}

fn walk_open_work(
    c: &Context<'_>,
    root: &Path,
    slug: &Slug,
    collection: &'static str,
    kind: Kind,
) -> crate::Result<WorkWalk> {
    let query = OPEN_WORK_QUERY.replace("COLLECTION", collection);
    let owner = format!("owner={}", slug.owner);
    let name = format!("name={}", slug.repo);
    let mut after: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    let mut seen_numbers = BTreeSet::new();
    let mut items = Vec::new();
    let mut pages = 0usize;
    let mut reason: Option<&'static str> = None;
    let mut complete = false;

    while pages < MAX_WORK_PAGES {
        pages += 1;
        let after_arg = after.as_deref().map(|s| format!("after={s}"));
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
        if let Some(value) = &after_arg {
            args.extend(["-f", value]);
        }
        let reply = spawn(c, root, &args, READ_TIMEOUT)?;
        if reply.code == 130 {
            return Err(crate::Error::new(130, "interrupted"));
        }
        let v = strict(&reply.stdout).unwrap_or(V::Null);
        let errors = v.get("errors").array();
        let err_reason = drift_error(reply.code, errors);
        if reply.code != 0 && !(reply.code == 1 && !errors.is_empty()) {
            record_reason(&mut reason, err_reason.unwrap_or("unavailable"));
            break;
        }
        let repo = v.get("data").get("repository");
        if !repo
            .get("nameWithOwner")
            .text()
            .and_then(|s| Slug::parse(s).ok())
            .is_some_and(|s| s.matches(slug))
        {
            record_reason(&mut reason, err_reason.unwrap_or("unavailable"));
            break;
        }
        let work = repo.get("work");
        let (V::Arr(rows), V::Bool(more)) =
            (work.get("nodes"), work.get("pageInfo").get("hasNextPage"))
        else {
            record_reason(&mut reason, err_reason.unwrap_or("partial_response"));
            break;
        };
        if response_has_errors(&v) {
            record_reason(&mut reason, err_reason.unwrap_or("partial_response"));
        }
        if rows.len() > 100 {
            record_reason(&mut reason, "partial_response");
        }
        for row in rows.iter().take(100) {
            if let Some(item) = work_item(row, slug, kind, true)
                && let Some(number) = digits(item.get("number")).and_then(|n| Number::parse(n).ok())
            {
                if seen_numbers.insert(number) {
                    items.push((number, item));
                }
            } else {
                record_reason(&mut reason, "partial_response");
            }
        }
        if !*more {
            complete = reason.is_none();
            break;
        }
        let next = work
            .get("pageInfo")
            .get("endCursor")
            .text()
            .filter(|s| valid_cursor(s));
        let Some(cursor) = next else {
            record_reason(&mut reason, "partial_response");
            break;
        };
        if !seen_cursors.insert(cursor.to_owned()) {
            record_reason(&mut reason, "partial_response");
            break;
        }
        if pages >= MAX_WORK_PAGES {
            record_reason(&mut reason, "page_limit");
            break;
        }
        after = Some(cursor.to_owned());
    }

    Ok(WorkWalk {
        items,
        complete,
        pages,
        reason,
    })
}

fn source_completeness(complete: bool, pages: usize, items_read: usize, reason: Option<&str>) -> V {
    obj([
        ("complete", complete.into()),
        ("pages", pages.into()),
        ("itemsRead", items_read.into()),
        ("reason", reason.map(V::from).unwrap_or(V::Null)),
    ])
}

fn missing_bucket(
    total: usize,
    issues_total: usize,
    pulls_total: usize,
    items: Vec<V>,
    omitted: usize,
    unverified: Option<bool>,
) -> V {
    let mut bucket = obj([
        ("total", total.into()),
        ("issuesTotal", issues_total.into()),
        ("pullRequestsTotal", pulls_total.into()),
        ("items", V::Arr(items)),
        ("omitted", omitted.into()),
        ("limit", MISSING_LIMIT.into()),
    ]);
    if let Some(flag) = unverified {
        bucket.set("unverified", flag);
    }
    bucket
}

fn drift_empty(status: &str, reason: Option<&str>, observed_at: &str) -> V {
    let resolved_reason = reason.unwrap_or(status);
    obj([
        ("status", status.into()),
        ("observedAt", observed_at.into()),
        ("board", V::Null),
        ("incomplete", true.into()),
        ("partial", true.into()),
        ("reason", resolved_reason.into()),
        (
            "statusField",
            obj([
                ("available", false.into()),
                ("options", V::Arr(vec![])),
                ("doneMatchRule", "exact_configured_names".into()),
                (
                    "doneConfiguredNames",
                    V::Arr(DONE_STATUS_NAMES.iter().map(|&s| s.into()).collect()),
                ),
                ("doneMatchedOptions", V::Arr(vec![])),
                ("hasDoneOption", false.into()),
            ]),
        ),
        (
            "statusDrift",
            obj([
                ("total", 0usize.into()),
                ("closedNotDone", 0usize.into()),
                ("mergedNotDone", 0usize.into()),
                ("openInDone", 0usize.into()),
                ("unverifiedStatus", 0usize.into()),
                ("items", V::Arr(vec![])),
                ("omitted", 0usize.into()),
                ("limit", STATUS_DRIFT_LIMIT.into()),
            ]),
        ),
        (
            "missingFromBoard",
            missing_bucket(0, 0, 0, vec![], 0, Some(true)),
        ),
        (
            "unverifiedMissing",
            missing_bucket(0, 0, 0, vec![], 0, None),
        ),
        (
            "excluded",
            obj([
                ("foreignRepository", 0usize.into()),
                ("archived", 0usize.into()),
                ("draft", 0usize.into()),
                ("unknown", 0usize.into()),
                ("total", 0usize.into()),
            ]),
        ),
        (
            "completeness",
            obj([
                ("complete", false.into()),
                ("partial", true.into()),
                ("reason", resolved_reason.into()),
                ("reasons", V::Arr(vec![resolved_reason.into()])),
                (
                    "board",
                    source_completeness(false, 0, 0, Some(resolved_reason)),
                ),
                ("openIssues", source_completeness(false, 0, 0, None)),
                ("openPullRequests", source_completeness(false, 0, 0, None)),
            ]),
        ),
    ])
}

/// Extract an item's verified Status option name.
/// Returns `None` if Status cannot be verified (truncated without match, duplicate,
/// wrong type, or unrecognized optionId); `Some(None)` if verified unset;
/// `Some(Some(label))` if verified set to an exact board option.
fn verified_status<'a>(
    node: &V,
    status_field_id: &str,
    options_by_id: &'a BTreeMap<String, String>,
) -> Option<Option<&'a str>> {
    let values = node.get("fieldValues");
    let truncated = values.get("pageInfo").get("hasNextPage") != &V::Bool(false);
    let mut matches = values
        .get("nodes")
        .array()
        .iter()
        .filter(|v| v.get("field").get("id").text() == Some(status_field_id));
    let Some(value) = matches.next() else {
        return (!truncated).then_some(None);
    };
    if matches.next().is_some()
        || value.get("__typename").text() != Some("ProjectV2ItemFieldSingleSelectValue")
    {
        return None;
    }
    let option_id = stable_id(value.get("optionId"))?;
    let label = options_by_id.get(option_id)?;
    Some(Some(label.as_str()))
}

struct BoardWalkPage {
    board: V,
    nodes: Vec<V>,
    has_errors: bool,
    truncated_nodes: bool,
}

struct BoardWalk {
    closed: bool,
    board_meta: V,
    first_board: V,
    pages_data: Vec<BoardWalkPage>,
    pages: usize,
    items_read: usize,
    exhausted: bool,
    reason: Option<&'static str>,
}

enum BoardWalkOutcome {
    Ready(Box<BoardWalk>),
    Unavailable {
        status: &'static str,
        reason: Option<String>,
    },
}

fn walk_board_pages(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    expected_id: &str,
    classify: fn(i32, &[V]) -> Option<&'static str>,
    deadline: Option<std::time::Instant>,
) -> crate::Result<BoardWalkOutcome> {
    crate::require(
        stable_id(&V::from(expected_id)).is_some(),
        "invalid project node id",
    )?;
    let target = match resolve_board(c, root, slug, expected_id)? {
        BoardLookup::Ready(target) => target,
        BoardLookup::Unavailable { status, reason } => {
            return Ok(BoardWalkOutcome::Unavailable { status, reason });
        }
    };

    let mut after: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    let mut pages = 0usize;
    let mut items_read = 0usize;
    let mut exhausted = false;
    let mut reason: Option<&'static str> = None;
    let mut closed_state = false;
    let mut board_meta: Option<V> = None;
    let mut first_board: Option<V> = None;
    let mut pages_data = Vec::new();

    while pages < MAX_BOARD_PAGES {
        pages += 1;
        let (board, has_errors, err_reason) =
            match fetch_board_raw(c, root, &target, expected_id, after.as_deref(), classify)? {
                BoardPageRaw::Ok {
                    board,
                    has_errors,
                    error_reason,
                } => (board, has_errors, error_reason),
                BoardPageRaw::Failed(err) => {
                    if pages == 1 {
                        return Ok(BoardWalkOutcome::Unavailable {
                            status: "unavailable",
                            reason: err.map(str::to_owned),
                        });
                    }
                    record_reason(&mut reason, err.unwrap_or("unavailable"));
                    break;
                }
            };
        if has_errors {
            record_reason(&mut reason, err_reason.unwrap_or("partial_response"));
        }
        if pages == 1 {
            let V::Bool(closed) = board.get("closed") else {
                return Ok(BoardWalkOutcome::Unavailable {
                    status: "unavailable",
                    reason: Some("partial_response".into()),
                });
            };
            closed_state = *closed;
            board_meta = Some(obj([
                ("id", expected_id.into()),
                ("number", V::Num(target.number.to_string())),
                (
                    "title",
                    display(board.get("title"), 120).unwrap_or_default().into(),
                ),
                ("url", target.canonical.as_str().into()),
                ("state", if *closed { "CLOSED" } else { "OPEN" }.into()),
                ("owner", target.login.as_str().into()),
            ]));
            first_board = Some(board.clone());
        }
        let (V::Arr(nodes), V::Bool(more)) = (
            board.get("items").get("nodes"),
            board.get("items").get("pageInfo").get("hasNextPage"),
        ) else {
            if pages == 1 {
                return Ok(BoardWalkOutcome::Unavailable {
                    status: "unavailable",
                    reason: Some("partial_response".into()),
                });
            }
            record_reason(&mut reason, "partial_response");
            break;
        };
        let truncated_nodes = nodes.len() > 20;
        if truncated_nodes {
            record_reason(&mut reason, "partial_response");
        }
        let page_nodes: Vec<V> = nodes.iter().take(20).cloned().collect();
        items_read += page_nodes.len();
        let more = *more;
        let next = board
            .get("items")
            .get("pageInfo")
            .get("endCursor")
            .text()
            .filter(|s| valid_cursor(s))
            .map(str::to_owned);
        pages_data.push(BoardWalkPage {
            board,
            nodes: page_nodes,
            has_errors,
            truncated_nodes,
        });
        if !more {
            exhausted = true;
            break;
        }
        let Some(cursor) = next else {
            record_reason(&mut reason, "partial_response");
            break;
        };
        if !seen_cursors.insert(cursor.clone()) {
            record_reason(&mut reason, "partial_response");
            break;
        };
        if pages >= MAX_BOARD_PAGES {
            record_reason(&mut reason, "page_limit");
            break;
        }
        // Stop before a read that could outlast the caller's deadline and report it as incomplete.
        if deadline.is_some_and(|d| std::time::Instant::now() + READ_TIMEOUT > d) {
            record_reason(&mut reason, "timeout");
            break;
        }
        after = Some(cursor);
    }

    Ok(BoardWalkOutcome::Ready(Box::new(BoardWalk {
        closed: closed_state,
        board_meta: board_meta.unwrap_or(V::Null),
        first_board: first_board.unwrap_or(V::Null),
        pages_data,
        pages,
        items_read,
        exhausted,
        reason,
    })))
}

fn candidates_empty(status: &str, reason: Option<&str>, observed_at: &str) -> V {
    obj([
        ("status", status.into()),
        ("source", "explicit_project".into()),
        ("observed_at", observed_at.into()),
        ("board", V::Null),
        ("items", V::Arr(vec![])),
        ("total", 0usize.into()),
        ("omitted", 0usize.into()),
        ("limit", CANDIDATE_LIMIT.into()),
        ("recommendations", V::Arr(vec![])),
        ("recommendationsTotal", 0usize.into()),
        ("recommendationsOmitted", 0usize.into()),
        ("pages", 0usize.into()),
        ("itemsRead", 0usize.into()),
        ("incomplete", true.into()),
        ("reason", reason.map(V::from).unwrap_or(V::Null)),
    ])
}

/// Whole-board deterministic candidate ranking for an explicitly linked Projects v2 board.
/// Walks all pages up to `MAX_BOARD_PAGES`, deduplicating by canonical `IssueRef` across pages
/// and ranking recommended candidates by `(Priority option order, Phase option order, IssueRef)`.
pub(super) fn candidates(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    expected_id: &str,
    observed_at: &str,
) -> crate::Result<V> {
    // The MCP panel kills its child after 30 seconds; leave room for process start and encoding.
    let deadline = std::time::Instant::now() + CANDIDATES_WALK_BUDGET;
    let walk = match walk_board_pages(c, root, slug, expected_id, project_error, Some(deadline))? {
        BoardWalkOutcome::Ready(walk) => *walk,
        BoardWalkOutcome::Unavailable { status, reason } => {
            return Ok(candidates_empty(status, reason.as_deref(), observed_at));
        }
    };
    let slug = slug.expect("walk_board_pages returned Ready without slug");
    let mut reason = walk.reason;
    let (fields_report, chosen, ranks, ordered_schema, field_incomplete) =
        board_fields_with_order(walk.first_board.get("fields"));
    let first_schema_ok =
        !field_incomplete && recommendation_schema(walk.first_board.get("fields"), &chosen);
    let schema_consistent = walk.pages_data.iter().skip(1).all(|p| {
        let (_, _, _, page_ordered, page_partial) = board_fields_with_order(p.board.get("fields"));
        p.board.get("closed") == &V::Bool(walk.closed)
            && page_partial == field_incomplete
            && page_ordered == ordered_schema
            && recommendation_schema(p.board.get("fields"), &chosen) == first_schema_ok
    });
    if !schema_consistent {
        record_reason(&mut reason, "partial_response");
    }
    let schema_ok = schema_consistent && first_schema_ok;
    let planned = planned_issues(c, root, slug);
    if !planned.as_ref().is_some_and(|(_, complete)| *complete) {
        record_reason(&mut reason, "plan_unverified");
    } else if !schema_ok {
        record_reason(&mut reason, "field_schema_unverified");
    }

    let mut seen_numbers = BTreeSet::new();
    let mut duplicate_numbers = BTreeSet::new();
    for page_data in &walk.pages_data {
        for node in &page_data.nodes {
            let issue = node.get("content");
            if issue.get("__typename").text() == Some("Issue")
                && issue
                    .get("repository")
                    .get("nameWithOwner")
                    .text()
                    .and_then(|s| Slug::parse(s).ok())
                    .is_some_and(|r| r.matches(slug))
                && let Some(number) =
                    digits(issue.get("number")).and_then(|n| Number::parse(n).ok())
                && !seen_numbers.insert(number)
            {
                duplicate_numbers.insert(number);
            }
        }
    }

    let issue_sort_prefix = (
        slug.owner.to_ascii_lowercase(),
        slug.repo.to_ascii_lowercase(),
    );
    let mut emitted_duplicates = BTreeSet::new();
    let mut ranked_candidates: Vec<((usize, usize, Number), V)> = Vec::new();
    let mut all_recommendations: Vec<((u8, usize, usize, Number), V)> = Vec::new();

    for page_data in &walk.pages_data {
        let page_gap = card_evidence_gap(
            page_data.has_errors || page_data.truncated_nodes || !schema_consistent,
            &planned,
            schema_ok,
        );
        for node in &page_data.nodes {
            let content = node.get("content");
            let dup_number = (content.get("__typename").text() == Some("Issue")
                && content
                    .get("repository")
                    .get("nameWithOwner")
                    .text()
                    .and_then(|s| Slug::parse(s).ok())
                    .is_some_and(|r| r.matches(slug)))
            .then(|| digits(content.get("number")).and_then(|n| Number::parse(n).ok()))
            .flatten()
            .filter(|n| duplicate_numbers.contains(n));

            if let Some(number) = dup_number {
                match node.get("isArchived") {
                    V::Bool(true) => {
                        if node.get("type").text() != Some("ISSUE")
                            || work_item(content, slug, Kind::Issue, false).is_none()
                        {
                            record_reason(&mut reason, "partial_response");
                        }
                    }
                    V::Bool(false) => {
                        let row = board_item(node, slug, &chosen);
                        if row.get("incomplete") == &V::Bool(true)
                            || row.get("class").text() == Some("unknown")
                        {
                            record_reason(&mut reason, "partial_response");
                        }
                    }
                    _ => record_reason(&mut reason, "partial_response"),
                }
                if emitted_duplicates.insert(number) {
                    let issue_ref = IssueRef {
                        slug: slug.clone(),
                        number,
                    };
                    all_recommendations.push((
                        (2, usize::MAX, usize::MAX, number),
                        obj([
                            ("issue", issue_ref.to_string().into()),
                            ("number", V::Num(number.to_string())),
                            ("class", "duplicate".into()),
                            ("state", "excluded".into()),
                            ("reason", "duplicate".into()),
                        ]),
                    ));
                }
                continue;
            }

            if node.get("isArchived") == &V::Bool(true) {
                match (node.get("type").text(), content.get("__typename").text()) {
                    (Some("DRAFT_ISSUE"), _) => {}
                    (Some("ISSUE"), Some("Issue")) => {
                        let Some(repository) = content
                            .get("repository")
                            .get("nameWithOwner")
                            .text()
                            .and_then(|s| Slug::parse(s).ok())
                        else {
                            record_reason(&mut reason, "partial_response");
                            continue;
                        };
                        if !repository.matches(slug) {
                            continue;
                        }
                        let Some(number) =
                            work_item(content, slug, Kind::Issue, false).and_then(|r| {
                                digits(r.get("number")).and_then(|n| Number::parse(n).ok())
                            })
                        else {
                            record_reason(&mut reason, "partial_response");
                            continue;
                        };
                        let issue_ref = IssueRef {
                            slug: slug.clone(),
                            number,
                        };
                        all_recommendations.push((
                            (2, usize::MAX, usize::MAX, number),
                            obj([
                                ("issue", issue_ref.to_string().into()),
                                ("number", V::Num(number.to_string())),
                                ("class", "archived".into()),
                                ("state", "excluded".into()),
                                ("reason", "archived".into()),
                            ]),
                        ));
                    }
                    (Some("PULL_REQUEST"), Some("PullRequest")) => {
                        let Some(repository) = content
                            .get("repository")
                            .get("nameWithOwner")
                            .text()
                            .and_then(|s| Slug::parse(s).ok())
                        else {
                            record_reason(&mut reason, "partial_response");
                            continue;
                        };
                        if repository.matches(slug)
                            && work_item(content, slug, Kind::Pull, false).is_none()
                        {
                            record_reason(&mut reason, "partial_response");
                        }
                    }
                    _ => record_reason(&mut reason, "partial_response"),
                }
                continue;
            }

            let mut row = board_item(node, slug, &chosen);
            if row.get("incomplete") == &V::Bool(true) || row.get("class").text() == Some("unknown")
            {
                record_reason(&mut reason, "partial_response");
            }
            if row.get("kind").text() != Some("issue") {
                continue;
            }
            let Some(number_str) = digits(row.get("number")).map(str::to_owned) else {
                record_reason(&mut reason, "partial_response");
                continue;
            };
            let Ok(number) = Number::parse(&number_str) else {
                record_reason(&mut reason, "partial_response");
                continue;
            };
            if row.get("class").text() == Some("candidate") {
                refine_candidate_class(
                    &mut row,
                    false,
                    walk.closed,
                    &planned,
                    &number_str,
                    page_data.has_errors || !schema_consistent,
                );
            }
            let advice = recommendation(&row, node, page_gap, false);
            let priority_idx = verified_option_rank(&row, node, "Priority", &ranks);
            let phase_idx = verified_option_rank(&row, node, "Phase", &ranks);
            let state = advice
                .get("state")
                .text()
                .unwrap_or("unverified")
                .to_owned();
            let why = advice
                .get("reason")
                .text()
                .unwrap_or("item_evidence_incomplete")
                .to_owned();
            let issue_ref = IssueRef {
                slug: slug.clone(),
                number,
            };
            let state_rank = match state.as_str() {
                "recommended" => 0,
                "unverified" => 1,
                _ => 2,
            };
            all_recommendations.push((
                (state_rank, priority_idx, phase_idx, number),
                obj([
                    ("issue", issue_ref.to_string().into()),
                    ("number", V::Num(number.to_string())),
                    ("class", row.get("class").clone()),
                    ("state", state.as_str().into()),
                    ("reason", why.as_str().into()),
                ]),
            ));
            if state == "recommended" {
                row.set("issue", issue_ref.to_string());
                row.set("recommendation", advice);
                ranked_candidates.push(((priority_idx, phase_idx, number), row));
            }
        }
    }

    ranked_candidates.sort_by(|(a, _), (b, _)| {
        (&a.0, &a.1, &issue_sort_prefix, &a.2).cmp(&(&b.0, &b.1, &issue_sort_prefix, &b.2))
    });
    let total = ranked_candidates.len();
    let mut items: Vec<V> = ranked_candidates
        .into_iter()
        .enumerate()
        .map(|(idx, (_, mut row))| {
            row.set("rank", idx + 1);
            row
        })
        .collect();
    let mut truncated_items = items.len() > CANDIDATE_LIMIT;
    items.truncate(CANDIDATE_LIMIT);
    bound_rows(&mut items, CANDIDATES_BYTES, &mut truncated_items);
    let omitted = total.saturating_sub(items.len());
    if truncated_items {
        record_reason(&mut reason, "result_limit");
    }

    all_recommendations.sort_by(|(a, _), (b, _)| {
        (&a.0, &a.1, &a.2, &issue_sort_prefix, &a.3).cmp(&(
            &b.0,
            &b.1,
            &b.2,
            &issue_sort_prefix,
            &b.3,
        ))
    });
    let recommendations_total = all_recommendations.len();
    let mut recommendations: Vec<V> = all_recommendations.into_iter().map(|(_, r)| r).collect();
    let mut truncated_recs = recommendations.len() > RECOMMENDATION_LIMIT;
    recommendations.truncate(RECOMMENDATION_LIMIT);
    bound_rows(
        &mut recommendations,
        RECOMMENDATIONS_BYTES,
        &mut truncated_recs,
    );
    let recommendations_omitted = recommendations_total.saturating_sub(recommendations.len());
    if truncated_recs {
        record_reason(&mut reason, "result_limit");
    }

    let incomplete = !walk.exhausted || reason.is_some();
    if incomplete && reason.is_none() {
        reason = Some("partial_response");
    }
    let mut board_meta = walk.board_meta;
    board_meta.set("fields", fields_report);

    Ok(obj([
        ("status", "available".into()),
        ("source", "explicit_project".into()),
        ("observed_at", observed_at.into()),
        ("board", board_meta),
        ("items", V::Arr(items)),
        ("total", total.into()),
        ("omitted", omitted.into()),
        ("limit", CANDIDATE_LIMIT.into()),
        ("recommendations", V::Arr(recommendations)),
        ("recommendationsTotal", recommendations_total.into()),
        ("recommendationsOmitted", recommendations_omitted.into()),
        ("pages", walk.pages.into()),
        ("itemsRead", walk.items_read.into()),
        ("incomplete", incomplete.into()),
        ("reason", reason.map(V::from).unwrap_or(V::Null)),
    ]))
}

/// Read-only drift report for one explicitly identified Projects v2 board.
/// Walks board pagination plus open repository issues and pull requests without
/// mutating the board, issues or pull requests.
pub(super) fn drift(
    c: &Context<'_>,
    root: &Path,
    slug: Option<&Slug>,
    expected_id: &str,
    observed_at: &str,
) -> crate::Result<V> {
    let walk = match walk_board_pages(c, root, slug, expected_id, drift_error, None)? {
        BoardWalkOutcome::Ready(walk) => *walk,
        BoardWalkOutcome::Unavailable { status, reason } => {
            return Ok(drift_empty(status, reason.as_deref(), observed_at));
        }
    };
    let slug = slug.expect("walk_board_pages returned Ready without slug");
    let board_pages = walk.pages;
    let board_items_read = walk.items_read;
    let mut board_reason = walk.reason;
    let board_meta = walk.board_meta;

    let mut status_field_available = false;
    let mut status_field_id = String::new();
    let mut status_options_ordered: Vec<String> = Vec::new();
    let mut status_options_by_id: BTreeMap<String, String> = BTreeMap::new();
    let mut done_matched: Vec<String> = Vec::new();
    if let Some((fid, opts)) = single_select_field(walk.first_board.get("fields"), "Status") {
        status_field_available = true;
        status_field_id = fid;
        for (oid, name) in opts {
            if DONE_STATUS_NAMES.contains(&name.as_str()) && !done_matched.contains(&name) {
                done_matched.push(name.clone());
            }
            status_options_ordered.push(name.clone());
            status_options_by_id.insert(oid, name);
        }
    } else {
        record_reason(&mut board_reason, "status_field_unavailable");
    }

    let mut board_present: BTreeSet<(Kind, Number)> = BTreeSet::new();
    let mut seen_active: BTreeSet<(Kind, Number)> = BTreeSet::new();
    let mut status_drift_rows: Vec<(Reverse<Number>, Kind, V)> = Vec::new();
    let mut closed_not_done = 0usize;
    let mut merged_not_done = 0usize;
    let mut open_in_done = 0usize;
    let mut unverified_status = 0usize;
    let mut excluded_foreign = 0usize;
    let mut excluded_archived = 0usize;
    let mut excluded_draft = 0usize;
    let mut excluded_unknown = 0usize;

    for page_data in &walk.pages_data {
        for node in &page_data.nodes {
            let &V::Bool(archived) = node.get("isArchived") else {
                excluded_unknown += 1;
                record_reason(&mut board_reason, "partial_response");
                continue;
            };
            let item = node.get("content");
            let kind = match (node.get("type").text(), item.get("__typename").text()) {
                (Some("DRAFT_ISSUE"), _) => {
                    if archived {
                        excluded_archived += 1;
                    } else {
                        excluded_draft += 1;
                    }
                    continue;
                }
                (Some("ISSUE"), Some("Issue")) => Some(Kind::Issue),
                (Some("PULL_REQUEST"), Some("PullRequest")) => Some(Kind::Pull),
                _ => None,
            };
            let Some(kind) = kind else {
                if archived {
                    excluded_archived += 1;
                } else {
                    excluded_unknown += 1;
                }
                record_reason(&mut board_reason, "partial_response");
                continue;
            };
            let Some(repository) = item
                .get("repository")
                .get("nameWithOwner")
                .text()
                .and_then(|s| Slug::parse(s).ok())
            else {
                if archived {
                    excluded_archived += 1;
                } else {
                    excluded_unknown += 1;
                }
                record_reason(&mut board_reason, "partial_response");
                continue;
            };
            if !repository.matches(slug) {
                excluded_foreign += 1;
                continue;
            }
            if archived {
                excluded_archived += 1;
                if let Some(number) = digits(item.get("number")).and_then(|n| Number::parse(n).ok())
                {
                    board_present.insert((kind, number));
                } else {
                    record_reason(&mut board_reason, "partial_response");
                }
                continue;
            }
            let Some(mut row) = work_item(item, slug, kind, false) else {
                excluded_unknown += 1;
                record_reason(&mut board_reason, "partial_response");
                continue;
            };
            let Some(number) = digits(row.get("number")).and_then(|n| Number::parse(n).ok()) else {
                excluded_unknown += 1;
                record_reason(&mut board_reason, "partial_response");
                continue;
            };
            board_present.insert((kind, number));
            if !seen_active.insert((kind, number)) || !status_field_available {
                continue;
            }
            let Some(status_name) = verified_status(node, &status_field_id, &status_options_by_id)
            else {
                unverified_status += 1;
                record_reason(&mut board_reason, "partial_response");
                continue;
            };
            let is_done = status_name.is_some_and(|s| done_matched.iter().any(|d| d == s));
            let drift_kind = match row.get("state").text() {
                Some("OPEN") if is_done => {
                    open_in_done += 1;
                    Some("open_in_done")
                }
                Some("MERGED") if !is_done => {
                    merged_not_done += 1;
                    Some("merged_not_done")
                }
                Some("CLOSED") if !is_done => {
                    closed_not_done += 1;
                    Some("closed_not_done")
                }
                _ => None,
            };
            if let Some(drift_kind) = drift_kind {
                row.set("status", status_name.map(V::from).unwrap_or(V::Null));
                row.set("drift", drift_kind);
                status_drift_rows.push((Reverse(number), kind, row));
            }
        }
    }
    let board_complete = walk.exhausted && board_reason.is_none();

    let (issues_walk, pulls_walk) =
        if let Some(stop_reason @ ("rate_limited" | "insufficient_scope" | "forbidden")) =
            board_reason
        {
            (
                WorkWalk::skipped(stop_reason),
                WorkWalk::skipped(stop_reason),
            )
        } else {
            let issues_walk = walk_open_work(c, root, slug, "issues", Kind::Issue)?;
            let pulls_walk =
                if let Some(stop_reason @ ("rate_limited" | "insufficient_scope" | "forbidden")) =
                    issues_walk.reason
                {
                    WorkWalk::skipped(stop_reason)
                } else {
                    walk_open_work(c, root, slug, "pullRequests", Kind::Pull)?
                };
            (issues_walk, pulls_walk)
        };

    let mut unseen_rows: Vec<(Reverse<Number>, Kind, V)> = Vec::new();
    let mut unseen_issues_total = 0usize;
    let mut unseen_pulls_total = 0usize;
    for (number, row) in &issues_walk.items {
        if !board_present.contains(&(Kind::Issue, *number)) {
            unseen_issues_total += 1;
            unseen_rows.push((Reverse(*number), Kind::Issue, row.clone()));
        }
    }
    for (number, row) in &pulls_walk.items {
        if !board_present.contains(&(Kind::Pull, *number)) {
            unseen_pulls_total += 1;
            unseen_rows.push((Reverse(*number), Kind::Pull, row.clone()));
        }
    }
    unseen_rows.sort_by_key(|(rev, kind, _)| (*rev, *kind));
    let unseen_total = unseen_rows.len();
    let unseen_displayed: Vec<V> = unseen_rows
        .into_iter()
        .take(MISSING_LIMIT)
        .map(|(_, _, row)| row)
        .collect();
    let unseen_omitted = unseen_total.saturating_sub(unseen_displayed.len());
    let (missing_from_board, unverified_missing) = if board_complete {
        (
            missing_bucket(
                unseen_total,
                unseen_issues_total,
                unseen_pulls_total,
                unseen_displayed,
                unseen_omitted,
                Some(false),
            ),
            missing_bucket(0, 0, 0, vec![], 0, None),
        )
    } else {
        (
            missing_bucket(0, 0, 0, vec![], 0, Some(true)),
            missing_bucket(
                unseen_total,
                unseen_issues_total,
                unseen_pulls_total,
                unseen_displayed,
                unseen_omitted,
                None,
            ),
        )
    };

    status_drift_rows.sort_by_key(|(rev, kind, _)| (*rev, *kind));
    let status_drift_total = status_drift_rows.len();
    let status_drift_displayed: Vec<V> = status_drift_rows
        .into_iter()
        .take(STATUS_DRIFT_LIMIT)
        .map(|(_, _, row)| row)
        .collect();
    let status_drift_omitted = status_drift_total.saturating_sub(status_drift_displayed.len());

    let mut reasons: Vec<&'static str> = Vec::new();
    for r in [board_reason, issues_walk.reason, pulls_walk.reason]
        .into_iter()
        .flatten()
    {
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    }
    reasons.sort_by_key(|r| reason_rank(r));
    let complete =
        board_complete && issues_walk.complete && pulls_walk.complete && reasons.is_empty();
    let partial = !complete;
    let primary_reason = reasons.first().copied();
    let excluded_total = excluded_foreign + excluded_archived + excluded_draft + excluded_unknown;

    Ok(obj([
        ("status", "available".into()),
        ("observedAt", observed_at.into()),
        ("board", board_meta),
        ("incomplete", partial.into()),
        ("partial", partial.into()),
        ("reason", primary_reason.map(V::from).unwrap_or(V::Null)),
        (
            "statusField",
            obj([
                ("available", status_field_available.into()),
                (
                    "options",
                    V::Arr(status_options_ordered.into_iter().map(V::from).collect()),
                ),
                ("doneMatchRule", "exact_configured_names".into()),
                (
                    "doneConfiguredNames",
                    V::Arr(DONE_STATUS_NAMES.iter().map(|&s| s.into()).collect()),
                ),
                (
                    "doneMatchedOptions",
                    V::Arr(done_matched.iter().cloned().map(V::from).collect()),
                ),
                ("hasDoneOption", (!done_matched.is_empty()).into()),
            ]),
        ),
        (
            "statusDrift",
            obj([
                ("total", status_drift_total.into()),
                ("closedNotDone", closed_not_done.into()),
                ("mergedNotDone", merged_not_done.into()),
                ("openInDone", open_in_done.into()),
                ("unverifiedStatus", unverified_status.into()),
                ("items", V::Arr(status_drift_displayed)),
                ("omitted", status_drift_omitted.into()),
                ("limit", STATUS_DRIFT_LIMIT.into()),
            ]),
        ),
        ("missingFromBoard", missing_from_board),
        ("unverifiedMissing", unverified_missing),
        (
            "excluded",
            obj([
                ("foreignRepository", excluded_foreign.into()),
                ("archived", excluded_archived.into()),
                ("draft", excluded_draft.into()),
                ("unknown", excluded_unknown.into()),
                ("total", excluded_total.into()),
            ]),
        ),
        (
            "completeness",
            obj([
                ("complete", complete.into()),
                ("partial", partial.into()),
                ("reason", primary_reason.map(V::from).unwrap_or(V::Null)),
                (
                    "reasons",
                    V::Arr(reasons.into_iter().map(V::from).collect()),
                ),
                (
                    "board",
                    source_completeness(
                        board_complete,
                        board_pages,
                        board_items_read,
                        board_reason,
                    ),
                ),
                (
                    "openIssues",
                    source_completeness(
                        issues_walk.complete,
                        issues_walk.pages,
                        issues_walk.items.len(),
                        issues_walk.reason,
                    ),
                ),
                (
                    "openPullRequests",
                    source_completeness(
                        pulls_walk.complete,
                        pulls_walk.pages,
                        pulls_walk.items.len(),
                        pulls_walk.reason,
                    ),
                ),
            ]),
        ),
    ]))
}

fn num(v: &V) -> usize {
    digits(v).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn write_source_completeness(out: &mut String, label: &str, src: &V) {
    let state = if src.get("complete") == &V::Bool(true) {
        "complete"
    } else {
        "partial"
    };
    let items = num(src.get("itemsRead"));
    let pages = num(src.get("pages"));
    if let Some(reason) = src.get("reason").text() {
        let _ = writeln!(
            out,
            "  {label}: {state} ({reason}; {items} items / {pages} pages)"
        );
    } else {
        let _ = writeln!(out, "  {label}: {state} ({items} items / {pages} pages)");
    }
}

fn write_missing_bucket(out: &mut String, heading: &str, bucket: &V) {
    let total = num(bucket.get("total"));
    let issues = num(bucket.get("issuesTotal"));
    let pulls = num(bucket.get("pullRequestsTotal"));
    let omitted = num(bucket.get("omitted"));
    let shown = bucket.get("items").array().len();
    if omitted > 0 {
        let _ = writeln!(
            out,
            "{heading}: {total} ({issues} issues, {pulls} PRs; showing {shown}, {omitted} more omitted)"
        );
    } else {
        let _ = writeln!(out, "{heading}: {total} ({issues} issues, {pulls} PRs)");
    }
    for item in bucket.get("items").array() {
        let kind = item.get("kind").text().unwrap_or("item");
        let number = digits(item.get("number")).unwrap_or("?");
        let title = item.get("title").text().unwrap_or("");
        let _ = writeln!(out, "  - {kind} #{number} [OPEN] - {title}");
    }
    if omitted > 0 {
        let _ = writeln!(out, "  ... and {omitted} more not shown");
    }
}

pub(super) fn render_drift(report: &V, slug: Option<&Slug>, project_id: &str) -> String {
    let mut out = String::new();
    let status = report.get("status").text().unwrap_or("unavailable");
    let observed = report.get("observedAt").text().unwrap_or("");
    let board = report.get("board");
    if status == "available" && board != &V::Null {
        let title = board.get("title").text().unwrap_or("Board");
        let number = digits(board.get("number")).unwrap_or("?");
        let id = board.get("id").text().unwrap_or(project_id);
        let url = board.get("url").text().unwrap_or("");
        let state = board.get("state").text().unwrap_or("OPEN");
        let _ = writeln!(
            out,
            "Board drift: {title} (#{number}, {id}) - {url} [{state}]"
        );
    } else {
        let _ = writeln!(out, "Board drift: {status} ({project_id})");
    }
    match slug {
        Some(s) => {
            let _ = writeln!(out, "Repository: {s}");
        }
        None => {
            let _ = writeln!(out, "Repository: none");
        }
    }
    let _ = writeln!(out, "Observed at: {observed}");

    let completeness = report.get("completeness");
    let partial = completeness.get("partial") == &V::Bool(true);
    let reason = completeness.get("reason").text();
    let c_board = completeness.get("board");
    let c_issues = completeness.get("openIssues");
    let c_pulls = completeness.get("openPullRequests");
    if !partial {
        let _ = writeln!(out, "Completeness: complete");
    } else {
        let why = reason.map(reason_label).unwrap_or("incomplete read");
        let _ = writeln!(out, "Completeness: partial - {why}");
    }
    if status == "available" {
        write_source_completeness(&mut out, "board", c_board);
        write_source_completeness(&mut out, "open issues", c_issues);
        write_source_completeness(&mut out, "open PRs", c_pulls);
    }

    if status != "available" {
        return out;
    }

    let sf = report.get("statusField");
    let configured = DONE_STATUS_NAMES.join(", ");
    if sf.get("available") == &V::Bool(true) {
        let opts: Vec<_> = sf
            .get("options")
            .array()
            .iter()
            .filter_map(|v| v.text())
            .collect();
        let _ = writeln!(
            out,
            "Status options: {}",
            if opts.is_empty() {
                "(none)".to_owned()
            } else {
                opts.join(", ")
            }
        );
        let matched: Vec<_> = sf
            .get("doneMatchedOptions")
            .array()
            .iter()
            .filter_map(|v| v.text())
            .collect();
        if matched.is_empty() {
            let _ = writeln!(out, "Done-like options: none on board");
        } else {
            let _ = writeln!(out, "Done-like options: {}", matched.join(", "));
        }
        let _ = writeln!(
            out,
            "  rule: exact configured names only ({configured}; no fuzzy guess)"
        );
    } else {
        let _ = writeln!(out, "Status options: unavailable");
        let _ = writeln!(
            out,
            "  reason: Status single-select field missing, duplicated, or truncated"
        );
        let _ = writeln!(out, "Done-like options: none verified");
        let _ = writeln!(
            out,
            "  rule: exact configured names only ({configured}; no fuzzy guess)"
        );
    }

    let sd = report.get("statusDrift");
    let sd_total = num(sd.get("total"));
    let unverified = num(sd.get("unverifiedStatus"));
    let _ = write!(
        out,
        "Status drift: {sd_total} (closed not done: {}, merged not done: {}, open in done: {}",
        num(sd.get("closedNotDone")),
        num(sd.get("mergedNotDone")),
        num(sd.get("openInDone")),
    );
    if unverified > 0 {
        let _ = write!(out, ", unverified status: {unverified}");
    }
    let _ = writeln!(out, ")");
    for item in sd.get("items").array() {
        let kind = item.get("kind").text().unwrap_or("item");
        let number = digits(item.get("number")).unwrap_or("?");
        let state = item.get("state").text().unwrap_or("?");
        let status_val = match item.get("status").text() {
            Some(name) => format!("\"{name}\""),
            None => "(none)".to_owned(),
        };
        let drift_kind = item.get("drift").text().unwrap_or("drift");
        let title = item.get("title").text().unwrap_or("");
        let _ = writeln!(
            out,
            "  - {kind} #{number} [{state}] status={status_val} ({drift_kind}) - {title}"
        );
    }
    let sd_omitted = num(sd.get("omitted"));
    if sd_omitted > 0 {
        let _ = writeln!(
            out,
            "  ... and {sd_omitted} more status drift items omitted"
        );
    }

    let missing = report.get("missingFromBoard");
    let board_complete =
        c_board.get("complete") == &V::Bool(true) && missing.get("unverified") != &V::Bool(true);
    if board_complete {
        write_missing_bucket(&mut out, "Missing from board (open in repo)", missing);
    } else {
        let _ = writeln!(
            out,
            "Missing from board (open in repo): unverified (board read partial)"
        );
        let unverified_missing = report.get("unverifiedMissing");
        let open_work_read = num(unverified_missing.get("total")) > 0
            || num(c_issues.get("pages")) > 0
            || num(c_pulls.get("pages")) > 0;
        if open_work_read {
            write_missing_bucket(
                &mut out,
                "Not seen on read pages (board read partial)",
                unverified_missing,
            );
        }
    }

    let ex = report.get("excluded");
    let _ = write!(
        out,
        "Excluded (not drift): {} (foreign repository: {}, archived: {}",
        num(ex.get("total")),
        num(ex.get("foreignRepository")),
        num(ex.get("archived")),
    );
    let draft = num(ex.get("draft"));
    if draft > 0 {
        let _ = write!(out, ", draft: {draft}");
    }
    let unknown = num(ex.get("unknown"));
    if unknown > 0 {
        let _ = write!(out, ", unknown: {unknown}");
    }
    let _ = writeln!(out, ")");
    out
}
