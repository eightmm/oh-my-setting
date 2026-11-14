use super::*;
use oms_command_plan::{Action, Context, Plan, Request};
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

fn plan(
    repo: &Path,
    action: Action,
    id: &str,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
) -> Result<(i32, Value)> {
    let mut request = Request::new(action).flag("json");
    request.repo = Some(repo.into());
    if !id.is_empty() {
        request = request.with("id", id);
    }
    let context = Context::from_environment(&request, repo)?;
    let plan = Plan::open(context, services, cancel)?;
    let response = plan.execute(&request)?;
    if response.code != 0 {
        return Ok((response.code, Value::Null));
    }
    Ok((
        response.code,
        Value::parse(&response.stdout).map_err(Error::new)?,
    ))
}
pub fn task_view(
    repo: &Path,
    id: &str,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
) -> Result<Value> {
    let (code, value) = plan(repo, Action::Show, id, cancel, services)?;
    if code != 0 {
        return Err(Error::new(format!("plan task is not readable: {id}")));
    }
    Ok(value)
}
pub fn facts(
    repo: &Path,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
) -> Result<Value> {
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
    let (_, status) = plan(repo, Action::Status, "", cancel, services)?;
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
        let (_, listing) = plan(repo, Action::List, "", cancel, services)?;
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
pub fn view(
    repo: &Path,
    run: &str,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
) -> Result<(Value, Value, Value)> {
    let spec = events::load(repo, run)?;
    let rows = events::read(repo, run)?;
    let state = events::project(&rows, &spec)?;
    let mut facts = facts(repo, cancel, services)?;
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
    services: &(dyn oms_command_plan::Services + Sync),
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
                next = Some(plan(repo, Action::Next, "", cancel, services)?);
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
        let (code, view) = plan(repo, Action::Show, &task, cancel, services)?;
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
fn build_invocation(
    repo: &Path,
    node: &Value,
    task: &str,
    args: &Args,
    dry: bool,
) -> Result<(&'static str, Vec<String>)> {
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
    let mut command = vec![
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
    Ok(("plan-run", command))
}
fn claimed_outcome(node: &Value, task: &Value, facts: &Value) -> &'static str {
    let state = st(task, "state");
    let land = st(node, "mode") == "land";

    (if state == "blocked" {
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
    }) as _
}
fn outcome(node: &Value, task: &Value, facts: &Value) -> (String, Vec<Value>) {
    let claimed = claimed_outcome(node, task, facts);
    let absent = missing(ar(node, "proof"), facts);
    (
        if claimed == "completed" && !absent.is_empty() {
            "unverified".to_owned()
        } else {
            claimed.to_owned()
        },
        absent,
    )
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
    if oms_graph_python::sensitive_text(st(&row, "detail")) {
        row.set("detail", "detail-withheld");
    }
    if !row.has("facts") {
        row.set("facts", empty());
    }
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
fn finish(
    repo: &Path,
    run: &str,
    spec: &Value,
    route: Value,
    status: &str,
    reason: &str,
    cancel: &AtomicBool,
) -> Result<Value> {
    let rows = events::read(repo, run)?;
    events::append(
        repo,
        run,
        "run_finished",
        obj([
            (
                "idempotency_key",
                format!("finish:{}", rows.len() + 1).into(),
            ),
            ("node", route.get("primary").or("").clone()),
            ("status", status.into()),
            (
                "actor",
                obj([("kind", "runner".into()), ("name", "oms-graph".into())]),
            ),
            (
                "detail",
                reason.chars().take(400).collect::<String>().into(),
            ),
        ]),
    )?;
    let projection = project_write(repo, run, spec)?;
    mirror_journal(repo, run, status, cancel);
    Ok(result(run, spec, &projection, route, status, reason))
}
fn mirror_journal(repo: &Path, run: &str, status: &str, cancel: &AtomicBool) {
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if ["0", "false", "FALSE", "no", "NO", "off", "OFF"].contains(
        &std::env::var("OMS_WORK_JOURNAL")
            .unwrap_or_default()
            .as_str(),
    ) {
        return;
    }
    if std::env::var("OMS_WORK_JOURNAL_ACTIVE").is_ok_and(|v| v == "1")
        || std::env::var("OMS_WORK_JOURNAL_SUPPRESS").is_ok_and(|v| v == "1")
    {
        return;
    }
    let mirror = || -> Result<()> {
        use oms_command_journal::{
            adapters::{ObserveOverrides, build_source_payload},
            store::JournalStore,
        };
        let source = events::directory(repo, run)?.join("projection.json");
        let stamp = chrono::DateTime::parse_from_rfc3339(&util::now())
            .map_err(|e| Error::new(e.to_string()))?
            .to_utc();
        let options = ObserveOverrides {
            source_type: "graph-run".into(),
            source_file: Some(source),
            event_type: Some("phase_outcome".into()),
            source_id: Some(run.into()),
            outcome: Some(format!("graph run {status}")),
            outcome_status: Some(status.into()),
            operation: Some("update".into()),
            ..Default::default()
        };
        let payload = build_source_payload(repo, &options, stamp).map_err(Error::new)?;
        let mut settings = oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?;
        settings.cwd = repo.into();
        let lock = oms_lock::Lock::for_file(&repo.join(".oms/work-journal/state"), settings)
            .map_err(|e| Error::new(e.to_string()))?;
        let guard = lock.try_acquire().map_err(|e| Error::new(e.to_string()))?;
        let journal = JournalStore::open_for_write(repo, None).map_err(Error::new)?;
        guard.check().map_err(|e| Error::new(e.to_string()))?;
        journal.record_event(&payload, stamp).map_err(Error::new)?;
        guard.check().map_err(|e| Error::new(e.to_string()))?;
        journal.materialize().map_err(Error::new)?;
        Ok(())
    };
    let _ = mirror();
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
struct WorkOutput {
    exit: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}
impl WorkOutput {
    fn native((exit, stdout, stderr): (i32, Vec<u8>, Vec<u8>)) -> Self {
        Self {
            exit,
            stdout,
            stderr,
            timed_out: false,
        }
    }
}
fn node_env(
    run: &str,
    id: &str,
    attempt: i64,
    goal: &str,
    bindings: &Value,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("OMS_GRAPH_RUN_ID".into(), run.into()),
        ("OMS_GRAPH_NODE".into(), id.into()),
        ("OMS_GRAPH_ATTEMPT".into(), attempt.to_string()),
        ("OMS_GRAPH_GOAL".into(), goal.into()),
    ];
    for (name, entry) in map(bindings) {
        if validate::valid_binding(&name.clone().into()) {
            env.push((
                format!("OMS_GRAPH_TASK_{}", name.to_uppercase().replace('-', "_")),
                st(&entry, "task_id").into(),
            ));
        }
    }
    env
}

fn append_context_pack(argv: &mut Vec<String>, context: Option<&Value>) {
    if let Some(context) = context
        && st(context, "status") == "ready"
    {
        argv.extend(["--context-pack".into(), st(context, "path").into()]);
    }
}

#[allow(clippy::too_many_arguments)]
fn work(
    repo: &Path,
    run: &str,
    args: &Args,
    cancel: &AtomicBool,
    helper: Option<&Path>,
    job: &Job,
    services: &(dyn oms_command_plan::Services + Sync),
    host: &dyn crate::HostServices,
) -> Result<WorkOutput> {
    if let Some(cached) = &job.cached {
        return Ok(WorkOutput::native((
            0,
            st(cached, "stdout_tail").as_bytes().to_vec(),
            vec![],
        )));
    }
    let id = job.id.clone();
    let node = job.node.clone();
    let task = job.task.as_str();
    let attempt = job.attempt;
    let context = job.context.clone();
    let timeout = {
        let fallback = if st(&node, "kind") == "agent" {
            2700_i64
        } else {
            600_i64
        };
        let value = oms_command_operations::value::integer_value(&node.get("timeout").or(fallback))
            .map_err(Error::new)?;
        value
            .py()
            .parse::<u64>()
            .map_err(|_| Error::new("tool timeout is outside the supported duration range"))?
    };
    let native = native_deadline(Duration::from_secs(timeout), cancel, |cancel| {
        match st(&node, "tool") {
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
                Some(Ok(crate::run_with_host(
                    &argv, repo, cancel, helper, None, services, host,
                )))
            }
            "commit_bound" => Some(
                super::commit::bound_with_host(
                    repo,
                    st(&node, "binding"),
                    run,
                    "",
                    cancel,
                    services,
                    host,
                )
                .and_then(|v| {
                    v.json(false)
                        .map(|raw| (0, raw, vec![]))
                        .map_err(Error::new)
                }),
            ),
            _ => None,
        }
    });
    if let Some(result) = native {
        return result.map(WorkOutput::native);
    }
    let env = node_env(run, &id, attempt, args.text("goal"), &job.bindings);
    let host_call = |tool: &str, mut argv: Vec<String>| -> Result<WorkOutput> {
        append_context_pack(&mut argv, context.as_ref());
        let os_args: Vec<std::ffi::OsString> = argv.into_iter().map(Into::into).collect();
        host.invoke_command(
            tool,
            &os_args,
            repo,
            &env,
            Some(Duration::from_secs(timeout)),
            cancel,
        )
        .map(|(exit, stdout, stderr, timed_out)| WorkOutput {
            exit,
            stdout,
            stderr,
            timed_out,
        })
    };
    if st(&node, "kind") == "agent" {
        let (tool, argv) = build_invocation(repo, &node, task, args, false)?;
        return host_call(tool, argv);
    }
    if !st(&node, "tool").is_empty() {
        return match st(&node, "tool") {
            "plan_acceptance" => host_call(
                "agent-plan",
                vec![
                    "--repo".into(),
                    repo.to_string_lossy().into_owned(),
                    "accept".into(),
                ],
            ),
            _ => Err(Error::new(
                "tool capability requires its canonical service adapter",
            )),
        };
    }
    let mut command = vec!["-c".into(), st(&node, "command").into()];
    append_context_pack(&mut command, context.as_ref());
    let mut process = Command::new("bash");
    process.args(&command).current_dir(repo).envs(env);
    util::process_detail(&mut process, Duration::from_secs(timeout), cancel).map(
        |(exit, stdout, stderr, timed_out)| WorkOutput {
            exit,
            stdout,
            stderr,
            timed_out,
        },
    )
}
pub fn run_with_host(
    args: &Args,
    repo: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
    helper: Option<&Path>,
    services: &(dyn oms_command_plan::Services + Sync),
    host: &dyn crate::HostServices,
) -> Result<Value> {
    let mut spec = if args.action == "resume" {
        events::load(repo, args.text("run"))?
    } else {
        load_with_root(&args.text("spec").into(), cwd, host.install_root())?
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
        let facts = facts(repo, cancel, services)?;
        let state = route::state(&spec, &empty(), &empty(), &empty(), &empty())?;
        let route = route::evaluate(&spec, &state, &facts);
        let (tasks, unavailable, scopes, selectors) =
            resolve(repo, &spec, &state, &route, cancel, services)?;
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
                let (tool, invocation) = build_invocation(repo, node, st(&tasks, &id), args, true)?;
                let mut command = vec!["oms".to_owned(), tool.to_owned()];
                command.extend(invocation);
                commands.push(list(command));
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
        let (_, state, facts) = view(repo, &run, cancel, services)?;
        for (id, node_state) in map(state.get("nodes")) {
            if st(&node_state, "status") != "active" {
                continue;
            }
            let node = spec.get("nodes").get(&id);
            let mut fields = obj([
                ("claimed_outcome", "unverified".into()),
                ("outcome", "unverified".into()),
                ("detail", "resumed-without-outcome".into()),
                ("facts", proof(node, &facts)),
                (
                    "actor",
                    obj([("kind", "runner".into()), ("name", "resume".into())]),
                ),
            ]);
            if st(node, "kind") == "tool" && st(node, "effect") == "write" {
                fields.set("claimed_outcome", "blocked");
                fields.set("outcome", "blocked");
                fields.set("detail", "resumed-write-tool-uncertain");
            } else if st(node, "kind") == "agent" {
                let task = if !st(&node_state, "task_id").is_empty() {
                    st(&node_state, "task_id").to_owned()
                } else if !st(node, "plan_task_from").is_empty() {
                    bound(&state, st(node, "plan_task_from"))
                } else if st(node, "plan_task") != "next" {
                    st(node, "plan_task").to_owned()
                } else {
                    String::new()
                };
                if task.is_empty() {
                    fields.set("detail", "resumed-without-task");
                } else {
                    fields.set("task_id", task.as_str());
                    match plan(repo, Action::Show, &task, cancel, services) {
                        Err(error) if error.code == 130 => return Err(error),
                        Ok((0, task_view)) => {
                            let task_state = st(&task_view, "state");
                            if ["claimed", "running"].contains(&task_state)
                                && !task_view.get("claim_expired").truth()
                            {
                                continue;
                            }
                            let concrete = concrete(node, &task);
                            let (outcome, absent) = outcome(&concrete, &task_view, &facts);
                            fields.set("claimed_outcome", outcome.as_str());
                            fields.set("outcome", outcome);
                            if !st(node, "bind_task").is_empty() {
                                fields.set("binding", st(node, "bind_task"));
                            }
                            fields.set("facts", proof(&concrete, &facts));
                            fields.set(
                                "detail",
                                format!(
                                    "resumed-from-plan state={}{}{}",
                                    if task_state.is_empty() {
                                        "-"
                                    } else {
                                        task_state
                                    },
                                    if task_view.get("claim_expired").truth() {
                                        " claim-expired"
                                    } else {
                                        ""
                                    },
                                    if absent.is_empty() {
                                        String::new()
                                    } else {
                                        format!(
                                            " proof-missing={}",
                                            absent
                                                .iter()
                                                .map(Value::py)
                                                .collect::<Vec<_>>()
                                                .join(",")
                                        )
                                    }
                                ),
                            );
                        }
                        _ => {
                            fields.set("detail", "resumed-task-unreadable");
                        }
                    }
                }
            }
            record(
                repo,
                &run,
                &id,
                count(&node_state, "attempts").max(1),
                fields,
            )?;
        }
        project_write(repo, &run, &spec)?;
    }
    let requested = args.integer("max_steps");
    let declared = if requested.truth() {
        requested
    } else {
        spec.get("budget").default("max_steps", 20_i64)
    };
    let cap = if declared.integer(true) {
        declared
    } else {
        20_i64.into()
    };
    let mut step = 0_i64.into();
    let mut last = empty();
    while number_cmp(&step, &cap).is_lt() {
        step = increment(&step);
        util::check(cancel)?;
        let (_, state, facts) = view(repo, &run, cancel, services)?;
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
            if status == "gate" {
                let projection = project_write(repo, &run, &spec)?;
                return Ok(result(
                    &run,
                    &spec,
                    &projection,
                    route.clone(),
                    status,
                    st(&route, "reason"),
                ));
            }
            return finish(
                repo,
                &run,
                &spec,
                route.clone(),
                status,
                st(&route, "reason"),
                cancel,
            );
        }
        let (tasks, unavailable, scopes, selectors) =
            resolve(repo, &spec, &state, &route, cancel, services)?;
        if !map(&unavailable).is_empty() {
            for (id, reason) in map(&unavailable) {
                let attempt = count(state.get("nodes").get(&id), "attempts") + 1;
                events::append(
                    repo,
                    &run,
                    "node_started",
                    obj([
                        ("node", id.clone().into()),
                        ("attempt", attempt.into()),
                        ("idempotency_key", format!("start:{id}:{attempt}").into()),
                        (
                            "actor",
                            obj([("kind", "runner".into()), ("name", "oms-graph".into())]),
                        ),
                        ("wave_id", format!("wave-{}", rows.len() + 1).into()),
                        ("wave_index", 0_i64.into()),
                    ]),
                )?;
                record(
                    repo,
                    &run,
                    &id,
                    attempt,
                    obj([
                        ("facts", proof(spec.get("nodes").get(&id), &facts)),
                        ("claimed_outcome", "blocked".into()),
                        ("outcome", "blocked".into()),
                        ("detail", reason),
                    ]),
                )?;
            }
            project_write(repo, &run, &spec)?;
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
            let reason = reason.trim_end_matches(": ");
            return finish(repo, &run, &spec, route, "blocked", reason, cancel);
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
                (
                    "actor",
                    obj([("kind", "runner".into()), ("name", "oms-graph".into())]),
                ),
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
        std::thread::scope(|scope| -> Result<()> {
            let (sender, receiver) = std::sync::mpsc::channel();
            for (index, job) in jobs.iter().enumerate() {
                let sender = sender.clone();
                let run = &run;
                scope.spawn(move || {
                    let raw = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        work(repo, run, args, cancel, helper, job, services, host)
                    }))
                    .unwrap_or_else(|_| Err(Error::new("graph worker supervision failed")));
                    let _ = sender.send((index, raw));
                });
            }
            drop(sender);
            for (index, raw) in receiver {
                record_job(repo, &run, &jobs[index], raw, &facts, cancel, services)?;
            }
            Ok(())
        })?;
        project_write(repo, &run, &spec)?;
    }
    let reason = format!("step cap of {} reached", cap.py());
    finish(repo, &run, &spec, last, "exhausted", &reason, cancel)
}
fn record_job(
    repo: &Path,
    run: &str,
    job: &Job,
    raw: Result<WorkOutput>,
    before: &Value,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
) -> Result<()> {
    let id = job.id.clone();
    let original = &job.original;
    let task = job.task.as_str();
    let node = job.node.clone();
    let attempt = job.attempt;
    let mut detail = String::new();
    let mut runner_error = false;
    let output = match raw {
        Ok(value) => value,
        Err(error) if error.code == 130 => return Err(error),
        Err(error) => {
            let timed_out = error.code == 124;
            if !timed_out {
                detail = format!("runner-error: {}", error.message);
                runner_error = true;
            }
            WorkOutput {
                exit: -1,
                stdout: vec![],
                stderr: vec![],
                timed_out,
            }
        }
    };
    let WorkOutput {
        exit: finished,
        stdout,
        stderr,
        timed_out,
    } = output;
    if timed_out {
        detail = "timeout".into();
    }
    let after = if job.cached.is_some() || timed_out || runner_error {
        before.clone()
    } else {
        facts_after(repo, run, cancel, services)?
    };
    let (claimed, outcome, absent) = if let Some(cached) = &job.cached {
        detail = "cache-hit".into();
        (
            cached
                .default("claimed_outcome", cached.get("outcome").clone())
                .py(),
            st(cached, "outcome").to_owned(),
            vec![],
        )
    } else if timed_out || runner_error {
        ("unverified".into(), "unverified".into(), vec![])
    } else if st(&node, "kind") == "agent" {
        match finished {
            3 => {
                detail = "no-actionable-task".into();
                ("blocked".into(), "blocked".into(), vec![])
            }
            75 => {
                detail = "landing-lock-held".into();
                ("partial".into(), "partial".into(), vec![])
            }
            _ => match plan(repo, Action::Show, task, cancel, services) {
                Err(error) if error.code == 130 => return Err(error),
                Ok((0, view)) => {
                    let claimed = claimed_outcome(&node, &view, &after).to_owned();
                    let (mut outcome, absent) = outcome(&node, &view, &after);
                    let proves = if st(&node, "mode") == "land" {
                        st(&view, "state") == "done"
                            && after
                                .get(&format!("receipt.land.{}.present", st(&view, "id")))
                                .truth()
                    } else {
                        st(&view, "state") == "review" && view.get("patch").truth()
                    };
                    if finished != 0 && outcome == "completed" && !proves {
                        outcome = "failed".into();
                    }
                    if finished != 0 && outcome == "failed" {
                        detail = format!("plan-run-exit-{finished}");
                    }
                    (claimed, outcome, absent)
                }
                _ => {
                    detail = "task-unreadable".into();
                    ("unverified".into(), "unverified".into(), vec![])
                }
            },
        }
    } else {
        let claimed = if finished == 0 { "completed" } else { "failed" };
        let (outcome, absent) = effective(&node, claimed, &after);
        detail = format!("exit={finished}");
        (claimed.to_owned(), outcome, absent)
    };
    if !absent.is_empty() {
        if !detail.is_empty() {
            detail.push(' ');
        }
        detail.push_str(&format!(
            "proof-missing={}",
            absent.iter().map(Value::py).collect::<Vec<_>>().join(",")
        ));
    }
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
    let artifact = events::directory(repo, run)?
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
            detail
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(400)
                .collect::<String>()
                .into(),
        ),
        (
            "facts",
            if runner_error {
                empty()
            } else {
                proof(&node, &after)
            },
        ),
        ("cached", job.cached.is_some().into()),
    ]);
    if !task.is_empty() {
        fields.set("task_id", task);
    }
    if !st(original, "bind_task").is_empty() {
        fields.set("binding", st(original, "bind_task"));
    }
    record(repo, run, &id, attempt, fields)?;
    Ok(())
}
fn native_deadline<T>(
    timeout: Duration,
    parent: &AtomicBool,
    action: impl FnOnce(&AtomicBool) -> Option<Result<T>>,
) -> Option<Result<T>> {
    use std::sync::{Condvar, Mutex, atomic::Ordering};
    let cancel = AtomicBool::new(false);
    let done = (Mutex::new(false), Condvar::new());
    struct Completion<'a>(&'a (Mutex<bool>, Condvar));
    impl Drop for Completion<'_> {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
            self.0.1.notify_all();
        }
    }
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        let monitor = scope.spawn(|| {
            let mut finished = done.0.lock().unwrap_or_else(|e| e.into_inner());
            while !*finished {
                if parent.load(Ordering::SeqCst) || started.elapsed() >= timeout {
                    cancel.store(true, Ordering::SeqCst);
                    break;
                }
                finished = done
                    .1
                    .wait_timeout(finished, Duration::from_millis(10))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        });
        let completion = Completion(&done);
        let result = action(&cancel);
        drop(completion);
        let _ = monitor.join();
        if result.is_some() && parent.load(Ordering::SeqCst) {
            return Some(Err(Error {
                code: 130,
                message: "interrupted".into(),
            }));
        }
        if result.is_some() && started.elapsed() >= timeout {
            return Some(Err(Error {
                code: 124,
                message: "command timed out".into(),
            }));
        }
        result
    })
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
        if let Ok((key, _, _)) = predicate(&p.py())
            && facts.has(&key)
        {
            result.set(&key, facts.get(&key).clone());
        }
    }
    result
}
fn facts_after(
    repo: &Path,
    run: &str,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
) -> Result<Value> {
    Ok(view(repo, run, cancel, services)?.2)
}
pub fn decide(
    repo: &Path,
    run: &str,
    node: &str,
    outcome: &str,
    note: &str,
    cancel: &AtomicBool,
    services: &(dyn oms_command_plan::Services + Sync),
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
            (
                "detail",
                note.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(400)
                    .collect::<String>()
                    .into(),
            ),
        ]),
    )?;
    project_write(repo, run, &spec)?;
    let (spec, state, facts) = view(repo, run, cancel, services)?;
    Ok(obj([
        ("schema", 1_i64.into()),
        ("run_id", run.into()),
        ("event", row),
        ("route", strip_trace(route::evaluate(&spec, &state, &facts))),
    ]))
}
