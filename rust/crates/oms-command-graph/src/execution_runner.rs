use super::*;
use oms_command_plan::{Action, Context, Plan, Request, UnavailableServices};
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

fn plan(repo: &Path, action: Action, id: &str, cancel: &AtomicBool) -> Result<(i32, Value)> {
    let mut request = Request::new(action).flag("json");
    request.repo = Some(repo.into());
    if !id.is_empty() {
        request = request.with("id", id);
    }
    let context = Context::from_environment(&request, repo)?;
    let services = UnavailableServices;
    let plan = Plan::open(context, &services, cancel)?;
    let response = plan.execute(&request)?;
    if response.code != 0 {
        return Ok((response.code, Value::Null));
    }
    Ok((
        response.code,
        Value::parse(&response.stdout).map_err(Error::new)?,
    ))
}
pub fn task_view(repo: &Path, id: &str, cancel: &AtomicBool) -> Result<Value> {
    let (code, value) = plan(repo, Action::Show, id, cancel)?;
    if code != 0 {
        return Err(Error::new(format!("plan task is not readable: {id}")));
    }
    Ok(value)
}
pub fn facts(repo: &Path, cancel: &AtomicBool) -> Result<Value> {
    let head = util::git(repo, &["rev-parse", "HEAD"], cancel);
    let mut result = obj([
        ("git.head", head.clone().into()),
        (
            "git.branch",
            util::git(repo, &["symbolic-ref", "--short", "HEAD"], cancel).into(),
        ),
        (
            "git.dirty",
            (!util::git(repo, &["status", "--porcelain"], cancel).is_empty()).into(),
        ),
    ]);
    let (_, status) = plan(repo, Action::Status, "", cancel)?;
    if !matches!(status, Value::Obj(_)) {
        return Err(Error::new(
            "canonical plan status projection is unavailable",
        ));
    }
    for key in ["present", "all_done", "all_closed", "has_unfinished"] {
        if !matches!(status.get(key), Value::Bool(_)) {
            return Err(Error::new(format!(
                "canonical plan status has invalid {key}"
            )));
        }
        result.set(&format!("plan.{key}"), status.get(key).clone());
    }
    let actionable = status.get("actionable");
    if !matches!(actionable, Value::Arr(_))
        || actionable
            .array()
            .iter()
            .any(|v| !matches!(v, Value::Str(_)))
    {
        return Err(Error::new(
            "canonical plan status has invalid actionable tasks",
        ));
    }
    result.set("plan.actionable", actionable.clone());
    if !matches!(status.get("contract").get("satisfied"), Value::Bool(_)) {
        return Err(Error::new("canonical plan status has invalid contract"));
    }
    result.set(
        "plan.contract.satisfied",
        status.get("contract").get("satisfied").clone(),
    );
    if status.get("present").truth() {
        let (_, listing) = plan(repo, Action::List, "", cancel)?;
        if !matches!(listing.get("tasks"), Value::Arr(_)) {
            return Err(Error::new("canonical plan task projection is unavailable"));
        }
        for task in ar(&listing, "tasks") {
            let id = st(task, "id");
            if id.is_empty() {
                return Err(Error::new(
                    "canonical plan task projection contains an invalid task",
                ));
            }
            let prefix = format!("plan.task.{id}.");
            result.set(&(prefix.clone() + "state"), st(task, "state"));
            for (field, key) in [
                ("patch", "patch_present"),
                ("artifact", "artifact_present"),
                ("lease_id", "lease_present"),
                ("claim_expired", "claim_expired"),
            ] {
                result.set(&(prefix.clone() + key), task.get(field).truth());
            }
            result.set(
                &(prefix.clone() + "repair_count"),
                if task.get("repair_count").integer(false) {
                    task.get("repair_count").clone()
                } else {
                    0_i64.into()
                },
            );
            result.set(&(prefix + "reason"), st(task, "reason"));
        }
    }
    let store = oms_command_plan::storage::Store::open(repo)?;
    let mut latest_admit = BTreeMap::new();
    let mut lands: BTreeMap<String, bool> = BTreeMap::new();
    let mut acceptance = Value::Null;
    for path in [
        ".oms/artifacts/index.jsonl",
        ".oms/plan/progress.jsonl",
        ".oms/lifecycle/events.jsonl",
    ] {
        let raw = store.optional(path, 256 * 1024 * 1024)?.unwrap_or_default();
        for line in raw
            .split(|b| *b == b'\n')
            .filter(|b| !b.iter().all(u8::is_ascii_whitespace))
        {
            let row = Value::parse(line).map_err(Error::new)?;
            let task = st(&row, "task_id");
            let zero = row.get("exit") == &Value::from(0_i64)
                && !matches!(row.get("exit"), Value::Bool(_))
                || row.get("exit") == &Value::from("0");
            match st(&row, "kind") {
                "patch-admit" if !task.is_empty() => {
                    latest_admit.insert(task.to_owned(), if zero { "verified" } else { "failed" });
                }
                "patch-land" if !task.is_empty() => {
                    *lands.entry(task.into()).or_default() |= zero;
                }
                "acceptance" if ["pass", "fail", "error"].contains(&st(&row, "status")) => {
                    acceptance = row
                }
                _ => {}
            }
        }
    }
    for (task, status) in latest_admit {
        result.set(&format!("receipt.admit.{task}.latest"), status);
    }
    for (task, present) in lands {
        result.set(&format!("receipt.land.{task}.present"), present);
    }
    if !matches!(acceptance, Value::Null) {
        result.set(
            "receipt.acceptance.latest",
            acceptance.get("status").clone(),
        );
        result.set(
            "receipt.acceptance.base_sha",
            acceptance.default("base_sha", ""),
        );
        result.set(
            "receipt.acceptance.fresh",
            !head.is_empty() && st(&acceptance, "base_sha") == head,
        );
    }
    Ok(result)
}
pub fn view(repo: &Path, run: &str, cancel: &AtomicBool) -> Result<(Value, Value, Value)> {
    let spec = events::load(repo, run)?;
    let rows = events::read(repo, run)?;
    let state = events::project(&rows, &spec)?;
    let mut facts = facts(repo, cancel)?;
    let mut task = String::new();
    for row in &rows {
        if ["node_started", "node_outcome"].contains(&st(row, "event"))
            && st(spec.get("nodes").get(st(row, "node")), "plan_task") == "next"
            && !st(row, "task_id").is_empty()
        {
            task = st(row, "task_id").into();
        }
    }
    if !task.is_empty() {
        for (key, value) in map(&facts) {
            if let Some(rest) = key.strip_prefix(&format!("plan.task.{task}.")) {
                facts.set(&format!("plan.task.next.{rest}"), value);
            } else {
                let parts: Vec<_> = key.split('.').collect();
                if parts.len() >= 4 && parts[0] == "receipt" && parts[2] == task {
                    facts.set(
                        &format!("receipt.{}.next.{}", parts[1], parts[3..].join(".")),
                        value,
                    );
                }
            }
        }
    }
    Ok((spec, state, facts))
}
fn strip_trace(mut route: Value) -> Value {
    if let Value::Obj(fields) = &mut route {
        fields.remove(b"trace".as_slice());
    }
    route
}
fn result(
    run: &str,
    spec: &Value,
    state: &Value,
    route: Value,
    status: &str,
    reason: &str,
) -> Value {
    obj([
        ("schema", 1_i64.into()),
        ("run_id", run.into()),
        ("spec_id", spec.get("id").clone()),
        ("status", status.into()),
        ("primary", route.get("primary").clone()),
        ("reason", reason.into()),
        ("bindings", state.get("bindings").clone()),
        ("route", strip_trace(route)),
    ])
}
fn resolve(
    repo: &Path,
    spec: &Value,
    state: &Value,
    route: &Value,
    cancel: &AtomicBool,
) -> Result<(Value, Value, Value, Vec<String>)> {
    let mut tasks = empty();
    let mut unavailable = empty();
    let mut scopes = empty();
    let mut selectors = vec![];
    let mut next = None;
    for candidate in std::iter::once(route.get("primary")).chain(ar(route, "alternatives")) {
        let id = candidate.text().unwrap_or("");
        let node = spec.get("nodes").get(id);
        if st(node, "kind") != "agent" {
            continue;
        }
        let source = st(node, "plan_task_from");
        let task = if !source.is_empty() {
            bound(state, source)
        } else if st(node, "plan_task") == "next" {
            selectors.push(id.into());
            if next.is_none() {
                next = Some(plan(repo, Action::Next, "", cancel)?);
            }
            if next.as_ref().unwrap().0 == 3 {
                unavailable.set(id, "no-actionable-task");
                continue;
            }
            st(&next.as_ref().unwrap().1, "id").into()
        } else {
            st(node, "plan_task").into()
        };
        if task.is_empty() {
            unavailable.set(id, "missing-task");
            continue;
        }
        let (code, view) = plan(repo, Action::Show, &task, cancel)?;
        if code != 0 {
            return Err(Error::new(format!("plan task is not readable: {task}")));
        }
        tasks.set(id, task.as_str());
        scopes.set(
            &task,
            obj([
                (
                    "allowed",
                    view.default(
                        "allowed_paths",
                        view.default("allowed", list(Vec::<Value>::new())),
                    ),
                ),
                (
                    "forbidden",
                    view.default(
                        "forbidden_paths",
                        view.default("forbidden", list(Vec::<Value>::new())),
                    ),
                ),
            ]),
        );
    }
    Ok((tasks, unavailable, scopes, selectors))
}
fn build_command(
    repo: &Path,
    node: &Value,
    task: &str,
    args: &Args,
    dry: bool,
) -> Result<Vec<String>> {
    if !oms_command_operations::projection::id(task) || task == "next" {
        return Err(Error::new(
            "an agent node reaches plan-run only with a concrete plan task",
        ));
    }
    let provider = node.get("provider").or(args.text("worker")).py();
    if provider.starts_with('-')
        || !regex::Regex::new(r"^[A-Za-z0-9._-]{1,40}$")
            .unwrap()
            .is_match(&provider)
    {
        return Err(Error::new("provider must be a registered transport name"));
    }
    for (value, pattern, label) in [
        (
            args.text("model"),
            r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,80}$",
            "model",
        ),
        (
            args.text("reasoning_effort"),
            r"^[A-Za-z][A-Za-z0-9_-]{0,20}$",
            "reasoning effort",
        ),
    ] {
        if !value.is_empty() && !regex::Regex::new(pattern).unwrap().is_match(value) {
            return Err(Error::new(format!("{label} is not a supported value")));
        }
    }
    let root = install_root()
        .ok_or_else(|| Error::new("packaged OMS installation root was not supplied"))?;
    let mut command = vec![
        "bash".into(),
        root.join("scripts/plan-run.sh")
            .to_string_lossy()
            .into_owned(),
        "--repo".into(),
        repo.to_string_lossy().into_owned(),
        "--to".into(),
        provider,
        "--id".into(),
        task.into(),
    ];
    if st(node, "mode") == "land" {
        command.push("--land".into());
    }
    for (key, flag) in [
        ("model", "--model"),
        ("reasoning_effort", "--reasoning-effort"),
    ] {
        if !args.text(key).is_empty() {
            command.extend([flag.into(), args.text(key).into()]);
        }
    }
    if dry {
        command.push("--dry-run".into());
    }
    Ok(command)
}
fn outcome(node: &Value, task: &Value, facts: &Value) -> (String, Vec<Value>) {
    let state = st(task, "state");
    let land = st(node, "mode") == "land";
    let claimed = if state == "blocked" {
        "blocked"
    } else if (!land && state == "review" && task.get("patch").truth())
        || (!land && state == "done")
        || (land
            && state == "done"
            && facts
                .get(&format!("receipt.land.{}.present", st(task, "id")))
                .truth())
    {
        "completed"
    } else if ["claimed", "running"].contains(&state) {
        "unverified"
    } else {
        "failed"
    };
    effective(node, claimed, facts)
}
fn concrete(node: &Value, task: &str) -> Value {
    let mut node = node.clone();
    node.set("plan_task", task);
    let name = node
        .get("bind_task")
        .or(node.get("plan_task_from").clone())
        .py();
    if let Value::Obj(fields) = &mut node {
        fields.remove(b"plan_task_from".as_slice());
    }
    if !name.is_empty() && name != "None" {
        for field in ["proof", "requires"] {
            let predicates = ar(&node, field)
                .iter()
                .map(|p| {
                    let text = p.py();
                    let Ok((key, op, value)) = predicate(&text) else {
                        return p.clone();
                    };
                    let prefix = format!("binding.{name}.");
                    let key = if let Some(rest) = key.strip_prefix(&prefix) {
                        if let Some(receipt) = rest.strip_prefix("receipt.") {
                            receipt
                                .split_once('.')
                                .map(|(kind, rest)| format!("receipt.{kind}.{task}.{rest}"))
                                .unwrap_or(key)
                        } else if rest == "task_id" {
                            key
                        } else {
                            format!("plan.task.{task}.{rest}")
                        }
                    } else {
                        key
                    };
                    match op {
                        "falsy" => format!("!{key}"),
                        "eq" => format!("{key}={value}"),
                        "ne" => format!("{key}!={value}"),
                        _ => key,
                    }
                    .into()
                })
                .collect::<Vec<Value>>();
            node.set(field, list(predicates));
        }
    }
    node
}
fn record(repo: &Path, run: &str, id: &str, attempt: i64, fields: Value) -> Result<()> {
    let mut row = fields;
    if !row.has("actor") {
        row.set(
            "actor",
            obj([("kind", "runner".into()), ("name", "oms-graph".into())]),
        );
    }
    if !row.has("cached") {
        row.set("cached", false);
    }
    row.set("node", id);
    row.set("attempt", attempt);
    row.set("idempotency_key", format!("outcome:{id}:{attempt}"));
    events::append(repo, run, "node_outcome", row)?;
    Ok(())
}
fn project_write(repo: &Path, run: &str, spec: &Value) -> Result<Value> {
    let state = events::project(&events::read(repo, run)?, spec)?;
    write(
        &events::directory(repo, run)?.join("projection.json"),
        &state,
    )?;
    Ok(state)
}
fn prepare_context(
    repo: &Path,
    node: &Value,
    id: &str,
    goal: &str,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Option<Value> {
    if !matches!(node.get("context"), Value::Obj(_)) {
        return None;
    }
    let build = || -> Result<Value> {
        let directory = crate::project::state_dir(repo)?;
        crate::project::ensure(repo, &directory, 20000, cancel, helper)?;
        let graph = crate::project::index::Graph::new(crate::project::load(&directory)?)?;
        let task = node
            .get("context")
            .default("task", "")
            .py()
            .replace("${goal}", goal);
        let task = if !task.trim().is_empty() {
            task
        } else if !goal.trim().is_empty() {
            goal.into()
        } else {
            node.get("title").or(id).py()
        };
        let manifest = util::read_json(&directory.join("manifest.json"), 256 * 1024 * 1024)?
            .unwrap_or_default();
        let mut pack = crate::project::context::select(
            repo,
            &graph,
            &task,
            count(node.get("context"), "max_files").max(1),
            &[],
            "",
            &manifest["coverage"],
            cancel,
        )?;
        crate::project::context::persist(&directory, repo, &mut pack)?;
        let path = directory
            .join("context")
            .join(format!("{}.json", util::s(&pack, "pack_digest")));
        if path.as_os_str().to_string_lossy().contains(['\n', '\0']) {
            return Err(Error::new("context pack path is not a plain file path"));
        }
        let raw = util::bytes(&pack, false)?;
        Ok(obj([
            ("status", "ready".into()),
            ("path", path.to_string_lossy().into_owned().into()),
            (
                "project_graph_revision",
                util::s(&graph.graph, "revision").into(),
            ),
            ("context_pack_sha256", util::sha(raw).into()),
            (
                "context_file_count",
                util::array(&pack, "files").len().into(),
            ),
        ]))
    };
    Some(build().unwrap_or_else(|e| {
        obj([
            ("status", "unavailable".into()),
            (
                "reason",
                util::scrub(&e.message)
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .into(),
            ),
        ])
    }))
}
struct Job {
    id: String,
    original: Value,
    task: String,
    node: Value,
    attempt: i64,
    context: Option<Value>,
    bindings: Value,
    cache_key: String,
    cached: Option<Value>,
}
fn work(
    repo: &Path,
    run: &str,
    args: &Args,
    cancel: &AtomicBool,
    helper: Option<&Path>,
    job: &Job,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    if let Some(cached) = &job.cached {
        return Ok((0, st(cached, "stdout_tail").as_bytes().to_vec(), vec![]));
    }
    let id = job.id.clone();
    let node = job.node.clone();
    let task = job.task.as_str();
    let attempt = job.attempt;
    let context = job.context.clone();
    let timeout = if st(&node, "kind") == "agent" {
        2700_u64
    } else {
        let value = oms_command_operations::value::integer_value(&node.get("timeout").or(600_i64))
            .map_err(Error::new)?;
        value
            .py()
            .parse::<u64>()
            .map_err(|_| Error::new("tool timeout is outside the supported duration range"))?
    };
    let native = match st(&node, "tool") {
        "project_context" => {
            let goal = args.text("goal");
            let task = node
                .default("task", "${goal}")
                .py()
                .replace("${goal}", goal);
            let task = if !task.trim().is_empty() {
                task
            } else if !goal.trim().is_empty() {
                goal.into()
            } else {
                node.get("title").or(id.as_str()).py()
            };
            let mut argv = vec![
                "project".into(),
                "context".into(),
                "--task".into(),
                task.into(),
                "--max-files".into(),
                node.default("max_files", 12_i64).py().into(),
                "--json".into(),
                "--repo".into(),
            ];
            argv.push(repo.as_os_str().to_owned());
            Some(Ok(crate::run_with_helper(&argv, repo, cancel, helper)))
        }
        "commit_bound" => Some(
            super::commit::bound(repo, st(&node, "binding"), &run, "", cancel).and_then(|v| {
                v.json(false)
                    .map(|raw| (0, raw, vec![]))
                    .map_err(Error::new)
            }),
        ),
        _ => None,
    };
    if let Some(result) = native {
        return result;
    }
    let mut command = if st(&node, "kind") == "agent" {
        build_command(repo, &node, task, args, false)?
    } else if !st(&node, "tool").is_empty() {
        match st(&node, "tool") {
            "plan_acceptance" => {
                let root = install_root()
                    .ok_or_else(|| Error::new("packaged OMS installation root was not supplied"))?;
                vec![
                    "bash".into(),
                    root.join("scripts/agent-plan.sh")
                        .to_string_lossy()
                        .into_owned(),
                    "--repo".into(),
                    repo.to_string_lossy().into_owned(),
                    "accept".into(),
                ]
            }
            _ => {
                return Err(Error::new(
                    "tool capability requires its canonical service adapter",
                ));
            }
        }
    } else {
        vec!["bash".into(), "-c".into(), st(&node, "command").into()]
    };
    if let Some(context) = &context {
        if st(context, "status") == "ready" {
            command.extend(["--context-pack".into(), st(context, "path").into()]);
        }
    }
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(repo)
        .env("OMS_GRAPH_RUN_ID", &run)
        .env("OMS_GRAPH_NODE", &id)
        .env("OMS_GRAPH_ATTEMPT", attempt.to_string())
        .env("OMS_GRAPH_GOAL", args.text("goal"));
    for (name, entry) in map(&job.bindings) {
        if validate::valid_binding(&name.clone().into()) {
            process.env(
                format!("OMS_GRAPH_TASK_{}", name.to_uppercase().replace('-', "_")),
                st(&entry, "task_id"),
            );
        }
    }
    util::process(&mut process, Duration::from_secs(timeout), cancel)
}
pub fn run(
    args: &Args,
    repo: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<Value> {
    let mut spec = if args.action == "resume" {
        events::load(repo, args.text("run"))?
    } else {
        load(&args.text("spec").into(), cwd)?
    };
    let verdict = validate(&spec);
    if !verdict.get("ok").truth() {
        return Err(Error::new(format!(
            "cannot run an invalid graph: {}",
            ar(&verdict, "errors")
                .iter()
                .map(|r| st(r, "code"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if args.flag("dry_run") {
        let facts = facts(repo, cancel)?;
        let state = route::state(&spec, &empty(), &empty(), &empty(), &empty())?;
        let route = route::evaluate(&spec, &state, &facts);
        let (tasks, unavailable, scopes, selectors) = resolve(repo, &spec, &state, &route, cancel)?;
        let selection = scheduler::eligible(
            &spec,
            &facts,
            &route,
            &scopes,
            args.int("jobs"),
            &[],
            &tasks,
            &selectors,
        );
        let mut commands = vec![];
        for id in ar(&selection, "eligible") {
            let id = id.py();
            let node = spec.get("nodes").get(&id);
            if st(node, "kind") == "agent" {
                commands.push(list(build_command(
                    repo,
                    node,
                    st(&tasks, &id),
                    args,
                    true,
                )?));
            }
        }
        return Ok(obj([
            ("schema", 1_i64.into()),
            ("dry_run", true.into()),
            ("spec_id", spec.get("id").clone()),
            ("route", strip_trace(route)),
            ("resolved_tasks", tasks),
            ("unavailable", unavailable),
            ("eligible", selection.get("eligible").clone()),
            ("deferred", selection.get("deferred").clone()),
            ("conflicts", selection.get("conflicts").clone()),
            ("commands", list(commands)),
        ]));
    }
    let run = if args.action == "resume" {
        args.text("run").into()
    } else if !args.text("run").is_empty()
        && events::directory(repo, args.text("run"))?
            .join("graph.json")
            .is_file()
    {
        spec = events::load(repo, args.text("run"))?;
        args.text("run").into()
    } else {
        events::start(
            repo,
            &spec,
            args.text("run"),
            obj([
                ("worker", args.text("worker").into()),
                ("model", args.text("model").into()),
                ("goal", args.text("goal").into()),
            ]),
        )?
    };
    let mut settings = oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?;
    settings.cwd = repo.into();
    let lock = oms_lock::Lock::for_file(&events::directory(repo, &run)?.join("runner"), settings)
        .map_err(|e| Error::new(e.to_string()))?;
    let _guard = lock.try_acquire().map_err(|e| Error::new(e.to_string()))?;
    if args.action == "resume" {
        let (_, state, facts) = view(repo, &run, cancel)?;
        for (id, node_state) in map(state.get("nodes")) {
            if st(&node_state, "status") != "active" {
                continue;
            }
            let node = spec.get("nodes").get(&id);
            let mut claimed = "unverified".to_owned();
            let mut detail = "resumed-without-outcome";
            if st(node, "kind") == "tool" && st(node, "effect") == "write" {
                claimed = "blocked".into();
                detail = "resumed-write-tool-uncertain";
            } else if st(node, "kind") == "agent" {
                let task = st(&node_state, "task_id");
                if !task.is_empty() {
                    let (code, task_view) = plan(repo, Action::Show, task, cancel)?;
                    if code == 0 {
                        if ["claimed", "running"].contains(&st(&task_view, "state"))
                            && !task_view.get("claim_expired").truth()
                        {
                            continue;
                        }
                        claimed = outcome(&concrete(node, task), &task_view, &facts).0;
                        detail = "resumed-from-plan";
                    }
                }
            }
            record(
                repo,
                &run,
                &id,
                count(&node_state, "attempts").max(1),
                obj([
                    ("claimed_outcome", claimed.clone().into()),
                    ("outcome", claimed.into()),
                    ("detail", detail.into()),
                ]),
            )?;
        }
    }
    let cap = if args.int("max_steps") > 0 {
        args.int("max_steps")
    } else {
        count(spec.get("budget"), "max_steps").max(1)
    };
    let mut last = empty();
    for _ in 0..cap {
        util::check(cancel)?;
        let (_, state, facts) = view(repo, &run, cancel)?;
        let route = route::evaluate(&spec, &state, &facts);
        last = route.clone();
        let rows = events::read(repo, &run)?;
        events::append(
            repo,
            &run,
            "route_evaluated",
            obj([
                (
                    "idempotency_key",
                    format!("route:{}", rows.len() + 1).into(),
                ),
                ("route", strip_trace(route.clone())),
                ("budget", route.get("budget").clone()),
                (
                    "actor",
                    obj([("kind", "runner".into()), ("name", "oms-graph".into())]),
                ),
            ]),
        )?;
        let status = st(&route, "status");
        if status != "actionable" {
            let projection = project_write(repo, &run, &spec)?;
            if status != "gate" {
                events::append(
                    repo,
                    &run,
                    "run_finished",
                    obj([
                        ("status", status.into()),
                        ("detail", route.get("reason").clone()),
                    ]),
                )?;
            }
            return Ok(result(
                &run,
                &spec,
                &projection,
                route.clone(),
                status,
                st(&route, "reason"),
            ));
        }
        let (tasks, unavailable, scopes, selectors) = resolve(repo, &spec, &state, &route, cancel)?;
        if !map(&unavailable).is_empty() {
            for (id, reason) in map(&unavailable) {
                let attempt = count(state.get("nodes").get(&id), "attempts") + 1;
                record(
                    repo,
                    &run,
                    &id,
                    attempt,
                    obj([
                        ("claimed_outcome", "blocked".into()),
                        ("outcome", "blocked".into()),
                        ("detail", reason),
                    ]),
                )?;
            }
            continue;
        }
        let active = map(state.get("nodes"))
            .into_iter()
            .filter(|(_, n)| st(n, "status") == "active")
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let selection = scheduler::eligible(
            &spec,
            &binding_facts(&state, &facts),
            &route,
            &scopes,
            args.int("jobs"),
            &active,
            &tasks,
            &selectors,
        );
        if ar(&selection, "eligible").is_empty() {
            let reason = format!(
                "nothing is eligible: {}",
                ar(&selection, "conflicts")
                    .iter()
                    .chain(ar(&selection, "deferred"))
                    .map(|r| format!("{}:{}", st(r, "node"), st(r, "reason")))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            events::append(
                repo,
                &run,
                "run_finished",
                obj([
                    ("status", "blocked".into()),
                    ("detail", reason.clone().into()),
                ]),
            )?;
            project_write(repo, &run, &spec)?;
            return Ok(result(&run, &spec, &state, route, "blocked", &reason));
        }
        let workspace = workspace::fingerprint(repo, 200, 2 * 1024 * 1024, cancel)
            .ok()
            .filter(workspace::cache_allowed)
            .map(|v| st(&v, "digest").to_owned())
            .unwrap_or_default();
        let mut jobs = vec![];
        // The coordinator freezes identities before any subprocess starts.
        for (wave_index, candidate) in ar(&selection, "eligible").iter().enumerate() {
            let id = candidate.py();
            let original = spec.get("nodes").get(&id);
            let task = st(&tasks, &id);
            let node = if task.is_empty() {
                original.clone()
            } else {
                concrete(original, task)
            };
            let attempt = count(state.get("nodes").get(&id), "attempts") + 1;
            let mut start = obj([
                ("node", id.clone().into()),
                ("attempt", attempt.into()),
                ("idempotency_key", format!("start:{id}:{attempt}").into()),
                ("wave_id", format!("wave-{}", rows.len() + 1).into()),
                ("wave_index", wave_index.into()),
            ]);
            if !task.is_empty() {
                start.set("task_id", task);
            }
            if !st(original, "bind_task").is_empty() {
                start.set("binding", st(original, "bind_task"));
            }
            let context = if st(&node, "kind") == "agent" {
                prepare_context(repo, &node, &id, args.text("goal"), cancel, helper)
            } else {
                None
            };
            if let Some(context) = &context {
                if st(context, "status") == "ready" {
                    for key in [
                        "project_graph_revision",
                        "context_pack_sha256",
                        "context_file_count",
                    ] {
                        start.set(key, context.get(key).clone());
                    }
                } else {
                    start.set("context", context.clone());
                }
            }
            events::append(repo, &run, "node_started", start)?;
            let cache_key = if st(&node, "kind") == "tool"
                && st(&node, "effect") == "read"
                && node.get("cacheable").truth()
            {
                workspace::key(&node, &state, &facts, &id, &workspace)?
            } else {
                String::new()
            };
            let cached = if cache_key.is_empty() {
                None
            } else {
                read_cache(repo, &cache_key)
            };
            jobs.push(Job {
                id,
                original: original.clone(),
                task: task.into(),
                node,
                attempt,
                context,
                bindings: state.get("bindings").clone(),
                cache_key,
                cached,
            });
        }
        let completed = std::thread::scope(|scope| {
            let handles = jobs
                .iter()
                .map(|job| scope.spawn(|| work(repo, &run, args, cancel, helper, job)))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err(Error::new("graph worker supervision failed")))
                })
                .collect::<Vec<_>>()
        });
        for (job, raw) in jobs.into_iter().zip(completed) {
            let id = job.id;
            let original = &job.original;
            let task = job.task.as_str();
            let node = job.node;
            let attempt = job.attempt;
            let (finished, stdout, stderr) = match raw {
                Ok(v) => v,
                Err(e) if e.code == 130 => return Err(e),
                Err(_) => (-1, vec![], vec![]),
            };
            let after = facts_after(repo, &run, cancel)?;
            let (claimed, outcome) = if let Some(cached) = &job.cached {
                (
                    cached
                        .default("claimed_outcome", cached.get("outcome").clone())
                        .py(),
                    st(cached, "outcome").into(),
                )
            } else if st(&node, "kind") == "agent" {
                if finished == 3 {
                    ("blocked".into(), "blocked".into())
                } else if finished == 75 {
                    ("partial".into(), "partial".into())
                } else if finished < 0 {
                    ("unverified".into(), "unverified".into())
                } else {
                    let (_, view) = plan(repo, Action::Show, task, cancel)?;
                    let value = outcome(&node, &view, &after).0;
                    (value.clone(), value)
                }
            } else {
                let claimed = if finished == 0 {
                    "completed"
                } else if finished < 0 {
                    "unverified"
                } else {
                    "failed"
                };
                (claimed.into(), effective(&node, claimed, &after).0)
            };
            let stdout = util::scrub(
                &String::from_utf8_lossy(&stdout)
                    .chars()
                    .rev()
                    .take(4000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
            );
            let stderr = util::scrub(
                &String::from_utf8_lossy(&stderr)
                    .chars()
                    .rev()
                    .take(4000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
            );
            let artifact = events::directory(repo, &run)?
                .join("artifacts")
                .join(format!("{}-{attempt}.txt", {
                    let name = id
                        .chars()
                        .map(|c| {
                            if c.is_alphanumeric() || "._-".contains(c) {
                                c
                            } else {
                                '_'
                            }
                        })
                        .collect::<String>();
                    if name.is_empty() { "node".into() } else { name }
                }));
            util::write(
                &artifact,
                if stdout.is_empty() {
                    stderr.as_bytes()
                } else {
                    stdout.as_bytes()
                },
            )?;
            if !job.cache_key.is_empty() && job.cached.is_none() && outcome == "completed" {
                write(
                    &repo
                        .join(".oms/graph/cache")
                        .join(format!("{}.json", job.cache_key)),
                    &obj([
                        ("outcome", outcome.clone().into()),
                        ("claimed_outcome", claimed.clone().into()),
                        (
                            "stdout_tail",
                            stdout
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .chars()
                                .take(4000)
                                .collect::<String>()
                                .into(),
                        ),
                        ("ts", util::now().into()),
                    ]),
                )?;
            }
            let mut fields = obj([
                ("claimed_outcome", Value::from(claimed)),
                ("outcome", Value::from(outcome)),
                (
                    "detail",
                    if job.cached.is_some() {
                        "cache-hit".into()
                    } else {
                        format!("exit={finished}").into()
                    },
                ),
                ("facts", proof(&node, &after)),
                ("cached", job.cached.is_some().into()),
            ]);
            if !task.is_empty() {
                fields.set("task_id", task);
            }
            if !st(original, "bind_task").is_empty() {
                fields.set("binding", st(original, "bind_task"));
            }
            record(repo, &run, &id, attempt, fields)?;
        }
        project_write(repo, &run, &spec)?;
    }
    let state = project_write(repo, &run, &spec)?;
    let reason = format!("step cap of {cap} reached");
    events::append(
        repo,
        &run,
        "run_finished",
        obj([
            ("status", "exhausted".into()),
            ("detail", reason.clone().into()),
        ]),
    )?;
    Ok(result(&run, &spec, &state, last, "exhausted", &reason))
}
fn read_cache(repo: &Path, key: &str) -> Option<Value> {
    let value = super::read(&repo.join(".oms/graph/cache").join(format!("{key}.json"))).ok()?;
    if OUTCOMES.contains(&st(&value, "outcome")) {
        Some(value)
    } else {
        None
    }
}
fn proof(node: &Value, facts: &Value) -> Value {
    let mut result = empty();
    for p in ar(node, "proof") {
        if let Ok((key, _, _)) = predicate(&p.py()) {
            if facts.has(&key) {
                result.set(&key, facts.get(&key).clone());
            }
        }
    }
    result
}
fn facts_after(repo: &Path, run: &str, cancel: &AtomicBool) -> Result<Value> {
    Ok(view(repo, run, cancel)?.2)
}
pub fn decide(
    repo: &Path,
    run: &str,
    node: &str,
    outcome: &str,
    note: &str,
    cancel: &AtomicBool,
) -> Result<Value> {
    let spec = events::load(repo, run)?;
    let node_spec = spec.get("nodes").get(node);
    if st(node_spec, "kind") != "gate" {
        return Err(Error::new(format!(
            "only a gate node records a decision, not {node}"
        )));
    }
    let decisions = ar(node_spec, "decisions");
    if !decisions.iter().any(|v| v.text() == Some(outcome)) {
        return Err(Error::new(format!(
            "gate {node} accepts {}, not {outcome}",
            decisions
                .iter()
                .map(Value::py)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let state = events::project(&events::read(repo, run)?, &spec)?;
    let attempt = count(state.get("nodes").get(node), "attempts") + 1;
    let row = events::append(
        repo,
        run,
        "gate_decision",
        obj([
            ("node", node.into()),
            ("attempt", attempt.into()),
            ("idempotency_key", format!("gate:{node}:{attempt}").into()),
            ("outcome", outcome.into()),
            (
                "actor",
                obj([("kind", "parent".into()), ("name", "decide".into())]),
            ),
            ("detail", note.chars().take(400).collect::<String>().into()),
        ]),
    )?;
    project_write(repo, run, &spec)?;
    let (spec, state, facts) = view(repo, run, cancel)?;
    Ok(obj([
        ("schema", 1_i64.into()),
        ("run_id", run.into()),
        ("event", row),
        ("route", strip_trace(route::evaluate(&spec, &state, &facts))),
    ]))
}
