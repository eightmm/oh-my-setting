//! `oms github`: an offline status over the local GitHub reference cache, the
//! network reads (`panel-status`, `refresh`, `adopt-merged`) and the only external write
//! (`post-evidence`) for references in the origin repository. Remote JSON is
//! untrusted: it is size-bounded, strictly parsed and checked against the
//! requested reference before any row is written. Errors are fixed text and
//! never echo remote bytes; only sanitized titles reach the terminal.
use crate::{
    Context, Error, Output, Result, V, cli::Options, digest, git::Git, git::process, obj, require,
    safe, storage, timestamp,
};
use oms_github::{
    IssueRef, Number, PullRef, Slug,
    declaration::{self, Declaration, TRACKED_REL},
    freshness::{self, CACHE_REL, Cached, Freshness, LABEL_CHARS, LABELS, TITLE_CHARS},
};
use oms_safe_fs::ByteReadOptions;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::Path,
    time::Duration,
};

#[path = "github_panel.rs"]
mod panel;

const WRITES_REL: &str = ".oms/github/writes.jsonl";
const PACKET_REL: &str = ".oms/task/current.md";
const PLAN_REL: &str = ".oms/plan/tasks.json";
const STATE_MAX: usize = 4 << 20;
const PLAN_MAX: usize = 16 << 20;
const RESPONSE_MAX: usize = 8 << 20;
const COMMENT_PAGES: usize = 10;
const TARGETS: usize = 50;
const ACCEPT: &str = "Accept: application/vnd.github+json";
const ACTION: &str = "post-evidence";
const API: &str = "https://api.github.com/repos/";
const WEB: &str = "https://github.com/";
const INCONSISTENT: &str = "evidence receipt is inconsistent; repair .oms/github/writes.jsonl";
const UNKNOWN: &str =
    "comment POST outcome is unknown; rerun post-evidence to check the marker before any retry";
const DECLARE: &str = "project mode is not active: requires PROJECT.md or a committed .github/oms-project.md declaring the origin repository as '- GitHub project: owner/repo' under ## Collaboration";
const RUN_REFRESH: &str =
    "cached pull request state is stale or unavailable; run `oms github refresh --pull REF`";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Issue,
    Pull,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Issue => "issue",
            Kind::Pull => "pull",
        }
    }
    fn api(self) -> &'static str {
        match self {
            Kind::Issue => "issues",
            Kind::Pull => "pulls",
        }
    }
    fn html(self) -> &'static str {
        match self {
            Kind::Issue => "issues",
            Kind::Pull => "pull",
        }
    }
}

pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    let action = o.action("");
    let allowed: &[&str] = match action {
        "status" => &["repo", "json"],
        "panel-status" => &[
            "repo",
            "json",
            "issue",
            "pull",
            "inventory",
            "cursor",
            "project-id",
            "drift",
        ],
        "refresh" => &["repo", "issue", "pull", "linked", "ttl-s"],
        "adopt-merged" => &["repo", "limit", "apply", "expected-sha256", "ttl-s"],
        "post-evidence" => &["repo", "issue", "pull", "yes"],
        _ => {
            return Err(Error::new(
                2,
                "github action must be status, panel-status, refresh, adopt-merged or post-evidence",
            ));
        }
    };
    require(o.words.len() == 1, "unexpected github argument")?;
    if let Some(k) = o.values.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::new(2, format!("--{k} does not apply to {action}")));
    }
    // `root` is the execution checkout (origin, declaration, task packet,
    // land receipts); `state` holds the shared cache, ledger and plan, so every
    // checkout of one repository sees one evidence ledger.
    if action == "panel-status" {
        return panel::status(c, o);
    }
    let root = c.repo(o)?;
    let state = c.state(&root);
    let git = Git::new(&root, c.interrupted);
    if action == "status" {
        return status(o, &root, state, &git);
    }
    let state = state?;
    match action {
        "refresh" => refresh(c, o, &root, &state, &git),
        "adopt-merged" => adopt(c, o, &root, &state, &git),
        _ => post(c, o, &root, &state, &git),
    }
}

fn read(root: &Path, rel: &str, max: usize) -> Result<Vec<u8>> {
    storage::root(root)?
        .read_no_follow(
            Path::new(rel),
            ByteReadOptions {
                missing_ok: true,
                max_bytes: max,
            },
        )
        .map_err(|_| {
            Error::new(
                2,
                format!("{rel} is unsafe, unreadable or exceeds its bound"),
            )
        })
}

fn origin(git: &Git<'_>) -> Result<Slug> {
    let out = git.output(
        &[
            OsStr::new("remote"),
            OsStr::new("get-url"),
            OsStr::new("origin"),
        ],
        None,
        &[],
    )?;
    require(out.code == 0, "no origin remote is configured")?;
    let url = std::str::from_utf8(&out.stdout)
        .map_err(|_| Error::new(2, "origin is not a github.com owner/repo remote"))?;
    Slug::from_remote(url.strip_suffix('\n').unwrap_or(url))
        .map_err(|_| Error::new(2, "origin is not a github.com owner/repo remote"))
}

/// The shared PROJECT.md / committed `.github/oms-project.md` declaration.
fn project(root: &Path) -> Result<Option<Declaration>> {
    declaration::inspect(root).map_err(|e| Error::new(2, e.message))
}

/// Origin plus the active project; a local declaration for another repo is
/// refused, a fork's inherited tracked one just leaves the mode inactive.
fn gate(root: &Path, git: &Git<'_>) -> Result<(Slug, Option<Slug>)> {
    let origin = origin(git)?;
    let mode = declaration::active(project(root)?.as_ref(), Some(&origin))
        .map_err(|e| Error::new(2, e.message))?;
    Ok((origin, mode))
}

/// The packet's `github_issue` header row, revalidated against origin.
fn task_issue(root: &Path, origin: &Slug) -> Result<Option<IssueRef>> {
    let raw = read(root, PACKET_REL, STATE_MAX)?;
    let text = String::from_utf8_lossy(&raw);
    let Some(value) = text
        .lines()
        .take_while(|l| !l.starts_with("## "))
        .find_map(|l| l.strip_prefix("- github_issue:"))
    else {
        return Ok(None);
    };
    IssueRef::parse(value.trim(), origin)
        .map(Some)
        .map_err(|_| Error::new(2, "task packet github_issue is not an origin reference"))
}

/// `(task id, canonical issue ref)` for plan tasks that carry `issue`.
fn plan_issues(root: &Path, origin: &Slug) -> Result<Vec<(String, IssueRef)>> {
    let raw = read(root, PLAN_REL, PLAN_MAX)?;
    if raw.is_empty() {
        return Ok(vec![]);
    }
    let plan = V::parse(&raw).map_err(|_| Error::new(2, "plan tasks.json is not JSON"))?;
    let V::Obj(tasks) = plan.get("tasks") else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for (id, task) in tasks {
        if !task.has("issue") {
            continue;
        }
        let id = freshness::sanitize(&String::from_utf8_lossy(id), 100);
        let issue = task
            .get("issue")
            .text()
            .and_then(|t| IssueRef::parse(t, origin).ok())
            .ok_or_else(|| {
                Error::new(
                    2,
                    format!("plan task {id} carries an invalid issue reference"),
                )
            })?;
        out.push((id, issue));
    }
    Ok(out)
}

fn cached(cache: &[u8], reference: &str, kind: Kind) -> (Option<Cached>, Freshness) {
    match freshness::lookup(cache, reference, timestamp() as i64) {
        (Some(row), f) if row.kind == kind.name() => (Some(row), f),
        _ => (None, Freshness::Unavailable),
    }
}

/// An unresolvable state repository is reported, not fatal: status then reads
/// the execution checkout's own cache and plan.
fn status(
    o: &Options,
    root: &Path,
    state: Result<std::path::PathBuf>,
    git: &Git<'_>,
) -> Result<Output> {
    let mut errors: Vec<String> = vec![];
    let state = state.unwrap_or_else(|e| {
        errors.push(e.message);
        root.to_owned()
    });
    let state = state.as_path();
    let origin = origin(git).map_err(|e| e.message);
    let declared = project(root).map_err(|e| e.message);
    let mode = declared
        .clone()
        .and_then(|d| declaration::active(d.as_ref(), origin.as_ref().ok()).map_err(|e| e.message));
    if let Err(e) = &mode {
        errors.push(e.clone());
    }
    let cache = read(state, CACHE_REL, STATE_MAX * 2)?;
    let entry = |reference: IssueRef| {
        let (row, f) = cached(&cache, &reference.to_string(), Kind::Issue);
        let freshness = match f {
            Freshness::Fresh => "fresh",
            Freshness::Stale => "stale",
            Freshness::Unavailable => "unavailable",
        };
        (reference, freshness, row)
    };
    let mut linked = vec![];
    if let Ok(origin) = &origin {
        match task_issue(root, origin) {
            Ok(Some(r)) => linked.push((None, entry(r))),
            Ok(None) => {}
            Err(e) => errors.push(e.message),
        }
        match plan_issues(state, origin) {
            Ok(rows) => linked.extend(rows.into_iter().map(|(id, r)| (Some(id), entry(r)))),
            Err(e) => errors.push(e.message),
        }
    }
    let code = i32::from(!errors.is_empty());
    let slug = |s: &Slug| V::from(s.to_string());
    let active = matches!(mode, Ok(Some(_)));
    // Use only validated local origin data. These destinations convey no live availability.
    let repository_url = origin
        .as_ref()
        .ok()
        .filter(|_| active)
        .map(|s| format!("{WEB}{s}"));
    let destinations = [
        ("repository", "repository", ""),
        ("issues", "issues", "/issues"),
        ("pull_requests", "pull requests", "/pulls"),
        ("actions", "actions", "/actions"),
        ("wiki", "wiki", "/wiki"),
    ];
    let issue_url = |issue: &IssueRef| {
        repository_url
            .as_ref()
            .map(|url| format!("{url}/issues/{}", issue.number))
    };
    let declared = declared.as_ref().ok().and_then(Option::as_ref);
    // Only a tracked-only declaration is ever inactive (see `active`).
    let inactive = (matches!(mode, Ok(None)) && declared.is_some())
        .then(|| if origin.is_ok() { "fork" } else { "origin" });
    let mut out = if o.has("json") {
        let rows = linked
            .iter()
            .map(|(task, (r, f, row))| {
                obj([
                    (
                        "source",
                        if task.is_some() { "plan" } else { "task" }.into(),
                    ),
                    ("task", task.clone().map_or(V::Null, V::from)),
                    ("reference", r.to_string().into()),
                    ("url", issue_url(r).map_or(V::Null, V::from)),
                    ("freshness", (*f).into()),
                    (
                        "state",
                        row.as_ref().map_or(V::Null, |c| c.state.clone().into()),
                    ),
                    (
                        "title",
                        row.as_ref().map_or(V::Null, |c| c.title.clone().into()),
                    ),
                ])
            })
            .collect();
        Output::json(&obj([
            ("project", declared.map_or(V::Null, |d| slug(&d.slug))),
            (
                "project_source",
                declared.map_or(V::Null, |d| d.source.name().into()),
            ),
            ("project_active", V::Bool(active)),
            ("project_inactive_reason", inactive.map_or(V::Null, V::from)),
            ("origin", origin.as_ref().map_or(V::Null, slug)),
            (
                "navigation",
                repository_url.as_ref().map_or(V::Null, |url| {
                    obj(destinations.map(|(key, _, suffix)| (key, format!("{url}{suffix}").into())))
                }),
            ),
            ("linked", V::Arr(rows)),
            (
                "errors",
                V::Arr(errors.iter().map(|e| e.as_str().into()).collect()),
            ),
        ]))?
    } else {
        let mut text = match (&mode, declared) {
            (Ok(Some(_)), Some(d)) => {
                format!("project mode: on ({}, {})\n", d.slug, d.source.name())
            }
            (Ok(None), Some(d)) => format!(
                "project mode: inactive ({TRACKED_REL} names {}; origin is {})\n",
                d.slug,
                origin
                    .as_ref()
                    .map_or("missing or not a github.com remote".to_owned(), |s| s
                        .to_string())
            ),
            (Ok(_), None) => "project mode: off\n".to_owned(),
            (Err(_), _) => "project mode: invalid\n".to_owned(),
        };
        match &origin {
            Ok(s) => text.push_str(&format!("origin: {s}\n")),
            Err(e) => text.push_str(&format!("origin: none ({e})\n")),
        }
        if let Some(url) = &repository_url {
            for (_, label, suffix) in destinations {
                text.push_str(&format!("{label}: {url}{suffix}\n"));
            }
        }
        for (task, (r, f, row)) in &linked {
            let source = task
                .as_ref()
                .map_or("task".to_owned(), |id| format!("plan {id}"));
            text.push_str(&format!("{source}: {r} [{f}]"));
            if let Some(row) = row {
                text.push_str(&format!(" {} {}", row.state, row.title));
            }
            if let Some(url) = issue_url(r) {
                text.push_str(&format!(" {url}"));
            }
            text.push('\n');
        }
        for e in &errors {
            text.push_str(&format!("error: {e}\n"));
        }
        Output::text(text)
    };
    out.code = code;
    Ok(out)
}

/// The raw run: Err(130) means no child was started, while a started child
/// that was cancelled returns Ok with code 130.
fn spawn(
    c: &Context<'_>,
    root: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<crate::git::Execution> {
    let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| OsString::from("gh"));
    // `git::process` strips only GIT_*, so terminal forcing and pagers are
    // neutralized by override: gh ignores an empty GH_FORCE_TTY and
    // CLICOLOR_FORCE=0, and GH_PAGER takes precedence over PAGER.
    let env = [
        ("GH_HOST".into(), "github.com".into()),
        ("GH_PROMPT_DISABLED".into(), "1".into()),
        ("GH_PAGER".into(), "cat".into()),
        ("PAGER".into(), "cat".into()),
        ("NO_COLOR".into(), "1".into()),
        ("GH_NO_UPDATE_NOTIFIER".into(), "1".into()),
        ("GH_FORCE_TTY".into(), "".into()),
        ("CLICOLOR_FORCE".into(), "0".into()),
    ];
    let argv = std::iter::once(bin)
        .chain(args.iter().map(OsString::from))
        .collect();
    process(root, argv, None, timeout, c.interrupted, &env).map_err(|e| {
        if e.code == 130 {
            e
        } else {
            Error::new(2, "gh could not be run")
        }
    })
}

/// A successful `gh` call whose stdout must be bounded strict JSON.
fn gh(c: &Context<'_>, root: &Path, args: &[&str]) -> Result<V> {
    gh_with_timeout(c, root, args, Duration::from_secs(30))
}

fn gh_with_timeout(c: &Context<'_>, root: &Path, args: &[&str], timeout: Duration) -> Result<V> {
    let out = spawn(c, root, args, timeout)?;
    if out.code == 130 {
        return Err(Error::new(130, "interrupted"));
    }
    require(out.code != 124, "gh timed out")?;
    require(
        out.code == 0,
        "gh not authenticated, unreachable or refused the request",
    )?;
    strict(&out.stdout)
}

fn strict(stdout: &[u8]) -> Result<V> {
    require(
        stdout.len() <= RESPONSE_MAX,
        "GitHub response exceeds its size bound",
    )?;
    require(
        oms_json::parse_strict(stdout).is_ok(),
        "GitHub response is not strict JSON",
    )?;
    V::parse(stdout).map_err(|_| Error::new(2, "GitHub response is not strict JSON"))
}

fn get(c: &Context<'_>, root: &Path, endpoint: &str) -> Result<V> {
    gh(c, root, &["api", "--method", "GET", "-H", ACCEPT, endpoint])
}

fn digits(v: &V) -> Option<&str> {
    match v {
        V::Num(s) if !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()) => {
            Some(s)
        }
        _ => None,
    }
}

/// `prefix/owner/repo/segment/N` naming exactly this origin reference.
fn names(url: Option<&str>, prefix: &str, origin: &Slug, segment: &str, number: Number) -> bool {
    let Some(path) = url.and_then(|u| u.strip_prefix(prefix)) else {
        return false;
    };
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 4
        && Slug::parse(&format!("{}/{}", parts[0], parts[1])).is_ok_and(|s| s.matches(origin))
        && parts[2] == segment
        && parts[3] == number.to_string()
}

/// Validate one remote issue or pull object into a cache row, or refuse.
fn validate(
    item: &V,
    kind: Kind,
    origin: &Slug,
    wanted: Option<Number>,
    ttl: &V,
) -> Result<(Number, V)> {
    let mismatch = || Error::new(2, "GitHub response does not match the requested reference");
    let number = digits(item.get("number"))
        .and_then(|d| Number::parse(d).ok())
        .ok_or_else(mismatch)?;
    require(wanted.is_none_or(|w| w == number), mismatch().message)?;
    require(
        names(
            item.get("html_url").text(),
            WEB,
            origin,
            kind.html(),
            number,
        ) && names(item.get("url").text(), API, origin, kind.api(), number),
        mismatch().message,
    )?;
    let state = item.get("state").text().unwrap_or_default();
    require(
        ["open", "closed"].contains(&state),
        "GitHub response carries an unknown state",
    )?;
    let state = match kind {
        Kind::Issue => {
            require(
                !item.has("pull_request"),
                "reference is a pull request; use --pull",
            )?;
            state
        }
        Kind::Pull => {
            let merged = match item.get("merged_at") {
                V::Null => false,
                V::Str(s) if !s.is_empty() => true,
                _ => {
                    return Err(Error::new(
                        2,
                        "GitHub response carries an invalid merged_at",
                    ));
                }
            };
            require(
                !item.has("merged") || item.get("merged") == &V::Bool(merged),
                "GitHub response merge fields disagree",
            )?;
            require(
                !merged || state == "closed",
                "GitHub response merge fields disagree",
            )?;
            if merged { "merged" } else { state }
        }
    };
    let title = item
        .get("title")
        .text()
        .ok_or_else(|| Error::new(2, "GitHub response carries no title"))?;
    let V::Arr(raw_labels) = item.get("labels") else {
        return Err(Error::new(2, "GitHub response carries invalid labels"));
    };
    let mut labels = vec![];
    for label in raw_labels {
        let name = label
            .get("name")
            .text()
            .ok_or_else(|| Error::new(2, "GitHub response carries invalid labels"))?;
        let name = freshness::sanitize(name, LABEL_CHARS);
        if !name.is_empty() && labels.len() < LABELS {
            labels.push(V::from(name));
        }
    }
    let body = match item.get("body") {
        V::Null => &[][..],
        V::Str(s) => s.as_slice(),
        _ => return Err(Error::new(2, "GitHub response carries an invalid body")),
    };
    let reference = match kind {
        Kind::Issue => IssueRef {
            slug: origin.clone(),
            number,
        }
        .to_string(),
        Kind::Pull => PullRef {
            slug: origin.clone(),
            number,
        }
        .to_string(),
    };
    let row = obj([
        ("reference", reference.into()),
        ("kind", kind.name().into()),
        ("state", state.into()),
        ("title", freshness::sanitize(title, TITLE_CHARS).into()),
        ("labels", V::Arr(labels)),
        ("body_sha256", digest(body).into()),
        ("captured_at", (timestamp() as i64).into()),
        ("ttl_s", ttl.clone()),
    ]);
    Ok((number, row))
}

fn ttl(o: &Options) -> Result<V> {
    let raw = o.value("ttl-s", "900");
    let ok = raw.len() <= 6
        && !raw.starts_with('0')
        && raw.parse::<u32>().is_ok_and(|n| (1..=604_800).contains(&n));
    require(ok, "--ttl-s must be an integer from 1 to 604800")?;
    Ok(V::Num(raw.into()))
}

/// Append rows to a JSONL ledger under the caller's lock with compare-and-set.
fn publish(root: &Path, rel: &str, rows: &[V], compact: bool) -> Result<()> {
    let path = Path::new(rel);
    let exists = root.join(path).try_exists()?;
    let old = read(root, rel, STATE_MAX * 2)?;
    require(
        old.is_empty() || old.ends_with(b"\n"),
        format!("{rel} has an incomplete tail; repair it before writing"),
    )?;
    let mut next = old.clone();
    for row in rows {
        next.extend(row.json(false)?);
        next.push(b'\n');
    }
    if compact && next.len() > STATE_MAX {
        next = newest(&next, timestamp() as i64);
    }
    require(
        next.len() <= STATE_MAX,
        format!("{rel} would exceed its bound"),
    )?;
    storage::replace(root, path, exists.then_some(old.as_slice()), &next)
}

/// Keep the newest readable row per reference and kind; a later line wins a
/// tie, and a row dated after `now` is dropped like a malformed one.
fn newest(raw: &[u8], now: i64) -> Vec<u8> {
    let mut keep: BTreeMap<(String, String), (i64, &[u8])> = BTreeMap::new();
    for line in raw.split(|b| *b == b'\n') {
        let Some(reference) = V::parse(line)
            .ok()
            .and_then(|v| v.get("reference").text().map(str::to_owned))
        else {
            continue;
        };
        if let (Some(row), _) = freshness::lookup(line, &reference, now) {
            let slot = keep
                .entry((reference, row.kind))
                .or_insert((row.captured_at, line));
            if row.captured_at >= slot.0 {
                *slot = (row.captured_at, line);
            }
        }
    }
    let mut out = vec![];
    for (_, line) in keep.into_values() {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

fn cache(c: &Context<'_>, state: &Path, rows: &[V]) -> Result<()> {
    storage::ensure_ignore(state, c.interrupted)?;
    let _guard = storage::lock(&state.join(CACHE_REL), c.interrupted)?;
    publish(state, CACHE_REL, rows, true)
}

fn refresh(
    c: &Context<'_>,
    o: &Options,
    root: &Path,
    state: &Path,
    git: &Git<'_>,
) -> Result<Output> {
    let (origin, mode) = gate(root, git)?;
    let ttl = ttl(o)?;
    let mut targets: Vec<(Kind, Number)> = vec![];
    for raw in o.all("issue") {
        let r =
            IssueRef::parse(&raw, &origin).map_err(|e| Error::new(2, format!("--issue: {e}")))?;
        targets.push((Kind::Issue, r.number));
    }
    for raw in o.all("pull") {
        let r = PullRef::parse(&raw, &origin).map_err(|e| Error::new(2, format!("--pull: {e}")))?;
        targets.push((Kind::Pull, r.number));
    }
    if o.has("linked") {
        require(mode.is_some(), format!("--linked: {DECLARE}"))?;
        let packet = task_issue(root, &origin)?;
        let planned = plan_issues(state, &origin)?;
        targets.extend(
            packet
                .into_iter()
                .chain(planned.into_iter().map(|(_, issue)| issue))
                .map(|issue| (Kind::Issue, issue.number)),
        );
    }
    targets.sort_by_key(|(k, n)| (k.name(), *n));
    targets.dedup_by_key(|(k, n)| (k.name(), *n));
    require(
        !targets.is_empty(),
        "refresh needs --issue REF, --pull REF or --linked with linked references",
    )?;
    require(
        targets.len() <= TARGETS,
        "refresh accepts at most 50 references",
    )?;
    let mut rows = vec![];
    for (kind, number) in &targets {
        let item = get(
            c,
            root,
            &format!(
                "repos/{}/{}/{}/{number}",
                origin.owner,
                origin.repo,
                kind.api()
            ),
        )?;
        rows.push(validate(&item, *kind, &origin, Some(*number), &ttl)?.1);
    }
    cache(c, state, &rows)?;
    let text: String = rows
        .iter()
        .map(|r| {
            format!(
                "refreshed {} ({}, {})\n",
                crate::string(r, "reference"),
                crate::string(r, "kind"),
                crate::string(r, "state")
            )
        })
        .collect();
    Ok(Output::text(text))
}

fn adopt(c: &Context<'_>, o: &Options, root: &Path, state: &Path, git: &Git<'_>) -> Result<Output> {
    // Curated adoption is an explicit approval, unlike a child's refresh.
    if o.has("apply") {
        crate::parent()?;
    }
    let (origin, mode) = gate(root, git)?;
    require(mode.is_some(), format!("adopt-merged: {DECLARE}"))?;
    let raw = o.value("limit", "20");
    let limit = raw
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=TARGETS).contains(n) && !raw.starts_with('0'))
        .ok_or_else(|| Error::new(2, "--limit must be an integer from 1 to 50"))?;
    let ttl = ttl(o)?;
    require(
        !o.has("expected-sha256") || o.has("apply"),
        "--expected-sha256 applies only with --apply",
    )?;
    let list = get(
        c,
        root,
        &format!(
            "repos/{}/{}/pulls?state=closed&sort=updated&direction=desc&per_page={limit}",
            origin.owner, origin.repo
        ),
    )?;
    let V::Arr(items) = list else {
        return Err(Error::new(2, "GitHub pull request list is not a list"));
    };
    require(
        items.len() <= limit,
        "GitHub returned more pull requests than requested",
    )?;
    let mut merged = vec![];
    for item in &items {
        if item.get("merged_at") != &V::Null {
            let (number, row) = validate(item, Kind::Pull, &origin, None, &ttl)?;
            merged.push((std::cmp::Reverse(number), row));
        }
    }
    merged.sort_by_key(|(n, _)| *n);
    merged.dedup_by_key(|(n, _)| *n);
    let rows: Vec<V> = merged.into_iter().map(|(_, row)| row).collect();
    // Everything cached except captured_at/ttl_s, so any remote edit between
    // the preview and --apply changes the selection. One compact JSON object
    // per row keeps titles and labels from colliding across field boundaries.
    let mut selection = vec![];
    for r in &rows {
        let fields = ["reference", "state", "title", "labels", "body_sha256"];
        selection.extend(obj(fields.map(|k| (k, r.get(k).clone()))).json(false)?);
        selection.push(b'\n');
    }
    let sha = digest(&selection);
    if !o.has("apply") {
        let mut text = format!(
            "adopt-merged preview: {} merged pull request(s)\n",
            rows.len()
        );
        for r in &rows {
            text.push_str(&format!(
                "  {}  {}\n",
                crate::string(r, "reference"),
                crate::string(r, "title")
            ));
        }
        text.push_str(&format!(
            "selection-sha256: {sha}\napply: oms github adopt-merged --limit {limit} --apply --expected-sha256 {sha}\n"
        ));
        return Ok(Output::text(text));
    }
    require(
        o.get("expected-sha256") == sha,
        "--apply needs --expected-sha256 matching the current preview; rerun the preview",
    )?;
    if !rows.is_empty() {
        cache(c, state, &rows)?;
    }
    Ok(Output::text(format!(
        "adopted {} merged pull request(s); selection-sha256: {sha}\n",
        rows.len()
    )))
}

/// The newest land receipt for this pull request's commit, when one is local.
fn land_sha(git: &Git<'_>, pull: &PullRef) -> Option<String> {
    let directory = crate::delivery::state_directory(git).ok()?;
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(directory).ok()?.flatten().take(1000) {
        if entry.path().extension() != Some(OsStr::new("json")) {
            continue;
        }
        let Ok(raw) = storage::read_external(&entry.path(), false) else {
            continue;
        };
        let Ok(row) = V::parse(&raw) else { continue };
        let sha = crate::string(&row, "sha");
        // The number alone could come from another remote's pull request.
        if digits(row.get("pr_number")) == Some(pull.number.to_string().as_str())
            && names(
                row.get("pr_url").text(),
                WEB,
                &pull.slug,
                "pull",
                pull.number,
            )
            && (crate::hex(&sha, 40) || crate::hex(&sha, 64))
        {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| modified >= *t) {
                best = Some((modified, sha));
            }
        }
    }
    best.map(|(_, sha)| sha)
}

/// Case-folded so an origin spelling change or another checkout of the same
/// repository maps to the same evidence key; the body is deliberately excluded.
fn key(issue: &IssueRef, pull: &PullRef) -> String {
    let (issue, pull) = (issue.to_string(), pull.to_string());
    digest(
        format!(
            "{ACTION}|{}|{}",
            issue.to_ascii_lowercase(),
            pull.to_ascii_lowercase()
        )
        .as_bytes(),
    )
}

fn marker(key: &str) -> String {
    format!("<!-- oms-evidence:{key} -->")
}

fn login(raw: &str) -> Option<&str> {
    (!raw.is_empty()
        && raw.len() <= 39
        && raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then_some(raw)
}

/// The only comment body post-evidence writes.
fn evidence(target: &str, subject: &str, receipt: &str) -> String {
    format!(
        "OMS merge evidence for {target}\n\n- Pull request: {subject}\n- Merge state: merged\n- Land receipt: {receipt}\n"
    )
}

/// Whether a stored body is one `evidence` can render for this target and
/// subject, so an edited ledger can never choose the posted bytes.
fn producible(body: &str, target: &str, subject: &str) -> bool {
    let template = evidence(target, subject, "");
    body.strip_prefix(&template[..template.len() - 1])
        .and_then(|r| r.strip_suffix('\n'))
        .is_some_and(|r| {
            r == "not recorded locally"
                || r.strip_prefix('`')
                    .and_then(|h| h.strip_suffix('`'))
                    .is_some_and(|h| crate::hex(h, 40) || crate::hex(h, 64))
        })
}

/// Every receipt row for this issue and pull, oldest first, matched by
/// repository (case-insensitively, through origin) and number rather than raw
/// spelling. All rows must carry this key and one producible body.
fn receipts<'a>(
    ledger: &'a [V],
    origin: &Slug,
    issue: &IssueRef,
    pull: &PullRef,
    key: &str,
) -> Result<Vec<&'a V>> {
    let rows: Vec<&V> = ledger
        .iter()
        .filter(|r| {
            crate::string(r, "action") == ACTION
                && IssueRef::parse(&crate::string(r, "target"), origin)
                    .is_ok_and(|t| t.number == issue.number)
                && PullRef::parse(&crate::string(r, "pull"), origin)
                    .is_ok_and(|p| p.number == pull.number)
        })
        .collect();
    // The body is checked against the row's own reference spellings, so an
    // origin respelling still maps to the same receipt; those spellings must
    // name origin explicitly and stay identical across rows.
    let named = |raw: &str| {
        raw.split_once('#')
            .is_some_and(|(slug, _)| Slug::parse(slug).is_ok_and(|s| s.matches(origin)))
    };
    let consistent = rows.first().is_none_or(|first| {
        let fields = |r: &V| ["key", "body", "target", "pull"].map(|k| crate::string(r, k));
        let [_, body, target, subject] = fields(first);
        named(&target) && named(&subject) && producible(&body, &target, &subject) && {
            let expected = [key.to_owned(), body, target, subject];
            rows.iter().all(|r| fields(r) == expected)
        }
    });
    require(consistent, INCONSISTENT)?;
    Ok(rows)
}

/// Whether a GitHub comment body is exactly the prepared comment; GitHub may
/// drop its final newline.
fn carries(body: &str, comment: &str) -> bool {
    body == comment || Some(body) == comment.strip_suffix('\n')
}

/// The id of an existing comment by `author` carrying `marker`, scanning
/// bounded pages. A marker posted by anyone else is ignored, so a forged
/// marker cannot suppress the evidence comment; the author's own marker must
/// name the target issue, or the list is not the target's, and must carry the
/// exact prepared `comment`, or it is refused rather than reposted or adopted.
fn find(
    c: &Context<'_>,
    root: &Path,
    origin: &Slug,
    issue: &IssueRef,
    marker: &str,
    comment: &str,
    author: &str,
) -> Result<Option<String>> {
    for page in 1..=COMMENT_PAGES {
        let list = get(
            c,
            root,
            &format!(
                "repos/{}/{}/issues/{}/comments?per_page=100&page={page}",
                issue.slug.owner, issue.slug.repo, issue.number
            ),
        )?;
        let V::Arr(items) = list else {
            return Err(Error::new(2, "GitHub comment list is not a list"));
        };
        require(
            items.len() <= 100,
            "GitHub returned an oversized comment page",
        )?;
        for item in &items {
            let by = item.get("user").get("login").text();
            let body = item.get("body").text().unwrap_or_default();
            if by.is_some_and(|l| l.eq_ignore_ascii_case(author)) && body.contains(marker) {
                require(
                    names(
                        item.get("issue_url").text(),
                        API,
                        origin,
                        "issues",
                        issue.number,
                    ),
                    "GitHub comment list does not belong to the target issue; nothing was posted",
                )?;
                let id = digits(item.get("id"))
                    .ok_or_else(|| Error::new(2, "GitHub comment carries an invalid id"))?;
                require(
                    carries(body, comment),
                    format!(
                        "evidence marker comment {id} does not carry the prepared receipt body; nothing was posted; edit or delete that comment, then rerun post-evidence"
                    ),
                )?;
                return Ok(Some(id.to_owned()));
            }
        }
        if items.len() < 100 {
            return Ok(None);
        }
    }
    Err(Error::new(
        2,
        "issue has more comments than can be checked for the evidence marker; nothing was posted",
    ))
}

/// The new comment id only when the POST response names the target issue and
/// carries the exact body and the authenticated author.
fn confirmed(
    v: &V,
    origin: &Slug,
    issue: &IssueRef,
    comment: &str,
    author: &str,
) -> Option<String> {
    let id = digits(v.get("id"))?;
    let body = v.get("body").text()?;
    let page = v
        .get("html_url")
        .text()?
        .strip_suffix(&format!("#issuecomment-{id}"))?;
    (carries(body, comment)
        && v.get("user")
            .get("login")
            .text()
            .is_some_and(|l| l.eq_ignore_ascii_case(author))
        && names(
            v.get("issue_url").text(),
            API,
            origin,
            "issues",
            issue.number,
        )
        // A pull request number takes issue comments under its `/pull/N` page.
        && ["issues", "pull"]
            .iter()
            .any(|segment| names(Some(page), WEB, origin, segment, issue.number)))
    .then(|| id.to_owned())
}

fn post(c: &Context<'_>, o: &Options, root: &Path, state: &Path, git: &Git<'_>) -> Result<Output> {
    crate::parent()?;
    require(
        o.has("yes"),
        "post-evidence writes a GitHub comment; pass --yes",
    )?;
    let (origin, mode) = gate(root, git)?;
    require(mode.is_some(), format!("post-evidence: {DECLARE}"))?;
    require(
        o.has("issue") && o.has("pull"),
        "post-evidence needs --issue REF and --pull REF",
    )?;
    let issue = IssueRef::parse(o.get("issue"), &origin)
        .map_err(|e| Error::new(2, format!("--issue: {e}")))?;
    let pull = PullRef::parse(o.get("pull"), &origin)
        .map_err(|e| Error::new(2, format!("--pull: {e}")))?;
    let (target, subject) = (issue.to_string(), pull.to_string());
    let key = key(&issue, &pull);
    let ledger = |state: &Path| -> Result<Vec<V>> {
        storage::parse_rows(&read(state, WRITES_REL, STATE_MAX * 2)?)
    };
    let done = |newest: Option<&V>| -> Result<Option<Output>> {
        let Some(row) = newest.filter(|r| crate::string(r, "phase") == "complete") else {
            return Ok(None);
        };
        let id = crate::string(row, "comment_id");
        require(
            (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit()),
            INCONSISTENT,
        )?;
        Ok(Some(Output::text(format!(
            "evidence already posted on {target} (comment {id})\n"
        ))))
    };
    // A completed receipt is a no-op before any freshness, lock or network step.
    let before = ledger(state)?;
    let earlier = receipts(&before, &origin, &issue, &pull, &key)?;
    if let Some(out) = done(earlier.last().copied())? {
        return Ok(out);
    }
    // Cache keys are exact, so after an origin respelling a retry also reads
    // the receipt's own pull spelling; the newest row found must qualify, and
    // a tie keeps the current spelling.
    let cache = read(state, CACHE_REL, STATE_MAX * 2)?;
    let found = std::iter::once(subject.clone())
        .chain(earlier.first().map(|r| crate::string(r, "pull")))
        .filter_map(|reference| match cached(&cache, &reference, Kind::Pull) {
            (Some(row), f) => Some((row, f)),
            (None, _) => None,
        })
        .reduce(|a, b| {
            if b.0.captured_at > a.0.captured_at {
                b
            } else {
                a
            }
        });
    match found {
        Some((row, Freshness::Fresh)) => require(
            row.state == "merged",
            "pull request is not merged per its fresh cache row",
        )?,
        _ => return Err(Error::new(2, RUN_REFRESH)),
    }
    storage::ensure_ignore(state, c.interrupted)?;
    let _guard = storage::lock(&state.join(WRITES_REL), c.interrupted)?;
    let rows = ledger(state)?;
    let previous = receipts(&rows, &origin, &issue, &pull, &key)?;
    if let Some(out) = done(previous.last().copied())? {
        return Ok(out);
    }
    // A retry keeps the receipt's own spellings, so every row stays identical.
    let (target, subject) = match previous.first() {
        Some(r) => (crate::string(r, "target"), crate::string(r, "pull")),
        None => (target.clone(), subject.clone()),
    };
    let body = match previous.first() {
        Some(r) => crate::string(r, "body"),
        None => {
            let receipt = land_sha(git, &pull)
                .map_or("not recorded locally".to_owned(), |s| format!("`{s}`"));
            evidence(&target, &subject, &receipt)
        }
    };
    // Only the currently authenticated account's own marker comment proves a
    // prior post, and a retry never continues another account's receipt.
    let user = get(c, root, "user")?;
    let author = login(user.get("login").text().unwrap_or_default())
        .ok_or_else(|| Error::new(2, "GitHub user response carries an invalid login"))?
        .to_owned();
    require(
        previous
            .iter()
            .all(|r| crate::string(r, "login").eq_ignore_ascii_case(&author)),
        "authenticated GitHub account differs from the one that prepared this evidence; nothing was posted",
    )?;
    let marker = marker(&key);
    let comment = format!("{body}\n{marker}\n");
    safe(comment.as_bytes())?;
    let row = |phase: &str, id: Option<&str>| {
        obj([
            ("key", key.as_str().into()),
            ("action", ACTION.into()),
            ("target", target.as_str().into()),
            ("pull", subject.as_str().into()),
            ("phase", phase.into()),
            ("login", author.as_str().into()),
            ("body", body.as_str().into()),
            ("comment_id", id.map_or(V::Null, V::from)),
            ("at", crate::now().into()),
        ])
    };
    if previous.is_empty() {
        publish(state, WRITES_REL, &[row("prepared", None)], false)?;
    }
    // Absence of the marker is proved before every POST, including retries.
    if let Some(id) = find(c, root, &origin, &issue, &marker, &comment, &author)? {
        publish(
            state,
            WRITES_REL,
            &[row("complete", Some(id.as_str()))],
            false,
        )?;
        return Ok(Output::text(format!(
            "evidence already present on {target} (comment {id}); receipt completed\n"
        )));
    }
    let endpoint = format!(
        "repos/{}/{}/issues/{}/comments",
        issue.slug.owner, issue.slug.repo, issue.number
    );
    let field = format!("body={comment}");
    crate::check(c.interrupted)?;
    let posted = match spawn(
        c,
        root,
        &[
            "api", "--method", "POST", "-H", ACCEPT, &endpoint, "-f", &field,
        ],
        Duration::from_secs(30),
    ) {
        Ok(out) if out.code == 0 => strict(&out.stdout)
            .ok()
            .and_then(|v| confirmed(&v, &origin, &issue, &comment, &author))
            .ok_or_else(|| {
                Error::new(
                    1,
                    "comment POST response did not confirm the target issue, body and author; rerun post-evidence to check the marker before any retry",
                )
            }),
        // Refused before any child started: nothing was sent.
        Err(e) if e.code == 130 => return Err(e),
        Ok(out) if out.code == 130 => Err(Error::new(130, format!("interrupted; {UNKNOWN}"))),
        _ => Err(Error::new(1, UNKNOWN)),
    };
    match posted {
        Ok(id) => {
            publish(
                state,
                WRITES_REL,
                &[row("complete", Some(id.as_str()))],
                false,
            )?;
            Ok(Output::text(format!(
                "posted evidence comment {id} on {target}\n"
            )))
        }
        Err(e) => {
            publish(state, WRITES_REL, &[row("unknown", None)], false)?;
            Err(e)
        }
    }
}
