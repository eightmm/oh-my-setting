//! Immutable local resources and explicit, idempotent recipe requests.
use super::*;
use crate::{epoch, timestamp};

pub(super) fn save(ctx: &Context<'_>, path: &Path, row: &Value) -> Result<()> {
    let ignore = ctx.repo.join(".oms/.gitignore");
    let _guard = storage::lock(&ignore)?;
    if storage::read(&ctx.repo, &ignore, storage::MAX_FILE, true)?.is_empty() {
        storage::replace_locked(&ctx.repo, &ignore, b"*\n")?;
    }
    storage::replace_locked(&ctx.repo, path, &json::encoded(row)?)
}
pub(super) fn history_with(
    ctx: &Context<'_>,
    kind: &str,
    id: &str,
    extra: &[(&str, Value)],
) -> Result<()> {
    let mut row = object([
        ("schema", number(1)),
        ("at", string(timestamp(epoch())?)),
        ("kind", string(kind)),
        ("id", string(id)),
    ]);
    for (k, v) in extra {
        row.set(k, v.clone())?;
    }
    storage::append(&ctx.repo, &Path::new(BASE).join("history.jsonl"), &row)
}
fn history(ctx: &Context<'_>, kind: &str, id: &str) -> Result<()> {
    history_with(ctx, kind, id, &[])
}
pub(super) fn existing(ctx: &Context<'_>, path: &Path) -> Result<Option<Value>> {
    let raw = storage::read(&ctx.repo, path, storage::MAX_FILE, true)?;
    if raw.is_empty() {
        Ok(None)
    } else {
        Ok(Some(json::parse(&raw)?))
    }
}
fn manifest(value: &Value) -> Result<()> {
    fields(
        value,
        &["schema", "kind", "id", "version", "resources"],
        &["description"],
    )?;
    require(
        value.get("schema").is_int(1)
            && value.text("kind") == "local-pack"
            && value.text("id").starts_with("oms-"),
        "expected oms-prefixed local-pack schema 1",
    )?;
    helpers::identifier(value.text("id"))?;
    helpers::identifier(value.text("version"))?;
    let resources = value.get("resources").object()?;
    require(
        !resources.is_empty() && resources.len() <= 64,
        "pack requires 1..64 resources",
    )?;
    let mut names = BTreeSet::new();
    for (name, row) in resources {
        helpers::identifier(
            std::str::from_utf8(name).map_err(|_| error(2, "invalid resource name"))?,
        )?;
        fields(row, &["kind", "path", "sha256"], &["executable"])?;
        require(
            ["recipe", "script", "renderer"].contains(&row.text("kind")),
            "unsupported resource kind",
        )?;
        helpers::relative(row.text("path"))?;
        json::digest(row.get("sha256"))?;
        require(
            row.text("path") != "manifest.json" && names.insert(row.text("path")),
            "duplicate or reserved resource path",
        )?;
        require(
            row.opt("executable")
                .is_none_or(|v| matches!(v, Value::Bool(_)))
                && !(row.text("kind") == "recipe" && row.get("executable").truth()),
            "invalid resource executable flag",
        )?;
    }
    helpers::scrub(value)
}
fn resources(root: &Path, value: &Value, selected: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut bytes = 0;
    require(
        !selected.is_empty() && selected.len() <= 64,
        "empty or oversized resource selection",
    )?;
    for name in selected {
        require(!out.contains_key(name), "duplicate resource selection")?;
        let row = value
            .get("resources")
            .opt(name)
            .ok_or_else(|| error(2, "unknown resource"))?;
        let data = storage::input(&helpers::absolute(root, row.text("path")), 4 * 1024 * 1024)?;
        bytes += data.len();
        require(
            bytes <= 16 * 1024 * 1024 && json::sha(&data) == row.text("sha256"),
            "resource digest mismatch or pack exceeds 16 MiB",
        )?;
        require(
            oms_scrub::scan_secret(&data, &[])
                .map_err(|e| error(2, format!("{e:?}")))?
                .is_empty(),
            "resource contains sensitive-looking material",
        )?;
        if row.text("kind") == "recipe" {
            validate(&json::parse(&data)?)?;
        }
        out.insert(name.clone(), data);
    }
    Ok(out)
}
pub(super) fn source(ctx: &Context<'_>, locator: &str) -> Result<Loaded> {
    if let Some(locator) = locator.strip_prefix("pack:") {
        let (id, tail) = locator
            .split_once('@')
            .ok_or_else(|| error(2, "expected pack:ID@VERSION/RESOURCE"))?;
        let (version, name) = tail
            .split_once('/')
            .ok_or_else(|| error(2, "expected pack resource"))?;
        helpers::identifier(id)?;
        helpers::identifier(version)?;
        helpers::identifier(name)?;
        let root = ctx.repo.join(BASE).join("packs").join(id).join(version);
        let receipt = load(ctx, &root.join("pack.json"))?;
        let value = receipt.get("manifest");
        manifest(value)?;
        require(
            value.text("id") == id
                && value.text("version") == version
                && json::hash(value)? == receipt.text("digest"),
            "stored pack manifest inconsistent",
        )?;
        let selected = receipt
            .get("selected")
            .strings()?
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        require(
            selected.iter().any(|n| n == name)
                && value.get("resources").get(name).text("kind") == "recipe",
            "recipe was not selected during import",
        )?;
        let contents = resources(&root.join("resources"), value, &selected)?;
        let definition = json::parse(
            contents
                .get(name)
                .ok_or_else(|| error(2, "recipe resource missing"))?,
        )?;
        let mut bindings = json::empty();
        for key in selected {
            bindings.set(
                &key,
                string(
                    root.join("resources")
                        .join(value.get("resources").get(&key).text("path"))
                        .to_string_lossy(),
                ),
            )?;
        }
        let origin = object([
            ("label", string(format!("pack:{locator}"))),
            ("sha256", string(json::hash(&definition)?)),
            ("pack_digest", receipt.get("digest").clone()),
        ]);
        return Ok(Loaded {
            definition,
            resources: bindings,
            source: origin,
        });
    }
    let path = helpers::absolute(&ctx.repo, locator);
    let definition = definition(ctx, &path)?;
    let label = path
        .strip_prefix(&ctx.repo)
        .unwrap_or_else(|_| Path::new(path.file_name().unwrap_or_default()))
        .to_string_lossy();
    let origin = object([
        ("label", string(label)),
        ("sha256", string(json::hash(&definition)?)),
    ]);
    Ok(Loaded {
        definition,
        resources: json::empty(),
        source: origin,
    })
}
fn pack(ctx: &Context<'_>, opts: &Options) -> Result<Output> {
    let action = opts.pos[1].as_str();
    if action == "list" {
        require(opts.pos.len() == 2, "list takes no directory")?;
        let mut rows = Vec::new();
        let root = ctx.repo.join(BASE).join("packs");
        if let Ok(packs) = std::fs::read_dir(&root) {
            for pack in packs {
                let pack = pack.map_err(|e| error(2, e))?;
                require(
                    pack.file_type().map_err(|e| error(2, e))?.is_dir(),
                    "pack directory must not be a symlink",
                )?;
                helpers::identifier(&pack.file_name().to_string_lossy())?;
                for version in std::fs::read_dir(pack.path()).map_err(|e| error(2, e))? {
                    let version = version.map_err(|e| error(2, e))?;
                    require(
                        version.file_type().map_err(|e| error(2, e))?.is_dir(),
                        "pack version must not be a symlink",
                    )?;
                    helpers::identifier(&version.file_name().to_string_lossy())?;
                    let receipt = load(ctx, &version.path().join("pack.json"))?;
                    manifest(receipt.get("manifest"))?;
                    let mut row = json::empty();
                    for key in ["id", "version", "digest", "selected", "imported_at"] {
                        row.set(key, receipt.get(key).clone())?;
                    }
                    rows.push(row);
                    require(rows.len() <= 1024, "pack capacity exceeded")?;
                }
            }
        }
        require(rows.len() <= 1024, "pack capacity exceeded")?;
        return json_output(
            0,
            &object([("schema", number(1)), ("packs", Value::Array(rows))]),
        );
    }
    require(
        ["inspect", "import"].contains(&action) && opts.pos.len() == 3,
        "pack inspect|import requires DIRECTORY",
    )?;
    let root = helpers::absolute(&ctx.cwd, &opts.pos[2]);
    require(
        root.symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.is_symlink()),
        "pack source must be a non-symlink directory",
    )?;
    let value = definition(ctx, &root.join("manifest.json"))?;
    manifest(&value)?;
    let digest = json::hash(&value)?;
    let mut selected = if opts.all("resource").is_empty() {
        value
            .get("resources")
            .object()?
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
            .collect::<Vec<_>>()
    } else {
        opts.all("resource").to_vec()
    };
    if action == "inspect" {
        selected = value
            .get("resources")
            .object()?
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
            .collect();
    }
    let contents = resources(&root, &value, &selected)?;
    if action == "inspect" {
        return json_output(
            0,
            &object([
                ("schema", number(1)),
                ("id", value.get("id").clone()),
                ("version", value.get("version").clone()),
                ("digest", string(digest)),
                ("resources", value.get("resources").clone()),
                (
                    "bytes",
                    number(contents.values().map(Vec::len).sum::<usize>()),
                ),
                ("verified", string("all-resources")),
                (
                    "execution",
                    string("import never executes scripts or installs dependencies"),
                ),
            ]),
        );
    }
    recipe_parent_only()?;
    require(
        opts.get("digest") == digest,
        "pack digest does not match --digest",
    )?;
    let destination = Path::new(BASE)
        .join("packs")
        .join(value.text("id"))
        .join(value.text("version"));
    let marker = destination.join("pack.json");
    let _guard = storage::lock(&ctx.repo.join(&marker))?;
    if let Some(previous) = existing(ctx, &marker)? {
        require(
            previous.text("digest") == digest,
            "different pack owns this id/version",
        )?;
        let old = previous
            .get("selected")
            .strings()?
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        resources(&ctx.repo.join(&destination).join("resources"), &value, &old)?;
        selected.extend(old);
    }
    for (name, data) in contents {
        let row = value.get("resources").get(&name);
        let path = destination.join("resources").join(row.text("path"));
        storage::replace_locked(&ctx.repo, &path, &data)?;
        if row.get("executable").truth() {
            use std::os::unix::fs::PermissionsExt;
            storage::input_file(&ctx.repo.join(&path))?
                .set_permissions(std::fs::Permissions::from_mode(0o700))
                .map_err(|e| error(2, e))?;
        }
    }
    selected.sort();
    selected.dedup();
    let row = object([
        ("schema", number(1)),
        ("id", value.get("id").clone()),
        ("version", value.get("version").clone()),
        ("digest", string(digest)),
        ("manifest", value.clone()),
        ("selected", json::list(selected.iter().map(String::as_str))),
        ("imported_at", string(timestamp(epoch())?)),
        ("execution", string("none")),
    ]);
    save(ctx, &marker, &row)?;
    history(ctx, "pack-imported", value.text("id"))?;
    json_output(0, &row)
}
fn workset(ctx: &Context<'_>, mut value: Value, pin: bool) -> Result<Value> {
    fields(
        &value,
        &["schema", "kind", "id", "spec", "repos"],
        &["title"],
    )?;
    require(
        value.get("schema").is_int(1) && value.text("kind") == "workset",
        "expected workset schema 1",
    )?;
    helpers::identifier(value.text("id"))?;
    let rows = value.get("repos").array()?;
    require(
        !rows.is_empty() && rows.len() <= 32,
        "workset requires 1..32 repositories",
    )?;
    let mut names = BTreeSet::new();
    let mut references = Vec::new();
    let mut spec_root = None;
    for row in rows {
        fields(row, &["id", "path", "tasks"], &[])?;
        helpers::identifier(row.text("id"))?;
        require(names.insert(row.text("id")), "duplicate repository id")?;
        let path = row.text("path");
        require(
            !path.is_empty()
                && !Path::new(path).is_absolute()
                && !path.starts_with('~')
                && !path.contains(['\\', ':']),
            "repo reference must be relative",
        )?;
        let tasks = row.get("tasks").strings()?;
        let unique: BTreeSet<_> = tasks.iter().collect();
        require(
            tasks.len() <= 64 && unique.len() == tasks.len(),
            "invalid task references",
        )?;
        for id in tasks {
            helpers::identifier(id)?;
        }
        let target = helpers::absolute(&ctx.repo, path);
        if row.text("id") == value.get("spec").text("repo") {
            spec_root = Some(target.clone());
        }
        let mut reference = row.clone();
        reference.set("available", Value::Bool(target.is_dir()))?;
        reference.set(
            "head",
            string(if target.is_dir() {
                oms_command_operations::storage::git_head(&target)
            } else {
                String::new()
            }),
        )?;
        references.push(reference);
    }
    let spec = value.get("spec");
    fields(spec, &["repo", "path"], &["sha256"])?;
    let root = spec_root.ok_or_else(|| error(2, "specification references unknown repository"))?;
    helpers::relative(spec.text("path"))?;
    require(
        !Path::new(spec.text("path"))
            .components()
            .any(|c| c.as_os_str() == ".oms"),
        "worksets cannot link private state",
    )?;
    let actual = storage::hash_input(&root.join(spec.text("path")), 4 * 1024 * 1024).ok();
    let mut updated = spec.clone();
    if pin {
        let digest = actual.ok_or_else(|| error(2, "shared specification is missing"))?;
        require(
            spec.opt("sha256").is_none_or(|v| v.str() == Ok(&digest)),
            "specification digest mismatch",
        )?;
        updated.set("sha256", string(digest))?;
        value.set("spec", updated)?;
        return Ok(value);
    }
    updated.set(
        "current_sha256",
        actual.as_ref().map(string).unwrap_or(Value::Null),
    )?;
    updated.set(
        "status",
        string(if actual.is_none() {
            "missing"
        } else if actual.as_deref() == Some(spec.text("sha256")) {
            "current"
        } else {
            "changed"
        }),
    )?;
    Ok(object([
        ("schema", number(1)),
        ("id", value.get("id").clone()),
        (
            "title",
            value
                .opt("title")
                .cloned()
                .unwrap_or_else(|| value.get("id").clone()),
        ),
        ("digest", string(json::hash(&value)?)),
        ("spec", updated),
        ("repos", Value::Array(references)),
        (
            "authority",
            string("inspection only; task references do not import state, leases or permissions"),
        ),
    ]))
}
fn trigger_definition(value: &Value) -> Result<()> {
    fields(
        value,
        &["schema", "kind", "id", "recipe"],
        &[
            "overlay",
            "inputs",
            "action",
            "ports",
            "companions",
            "schedule",
            "event",
        ],
    )?;
    require(
        value.get("schema").is_int(1) && value.text("kind") == "recipe-trigger",
        "expected recipe-trigger schema 1",
    )?;
    helpers::identifier(value.text("id"))?;
    require(
        value.opt("schedule").is_some() != value.opt("event").is_some(),
        "trigger requires exactly one schedule or event",
    )?;
    if let Some(schedule) = value.opt("schedule") {
        fields(schedule, &["at"], &["every_seconds"])?;
        require(
            crate::parse_timestamp(schedule.text("at")).is_some(),
            "invalid schedule instant",
        )?;
        if let Some(every) = schedule.opt("every_seconds") {
            bounded(every, 0, 1, 366 * 86400)?;
        }
    }
    if let Some(event) = value.opt("event") {
        fields(event, &["name"], &["match"])?;
        helpers::identifier(event.text("name"))?;
        flat(event.get("match"))?;
    }
    helpers::scrub(value)
}
fn flat(value: &Value) -> Result<()> {
    if matches!(value, Value::Null) {
        return Ok(());
    }
    require(value.object()?.len() <= 32, "event exceeds field bound")?;
    for (key, v) in value.object()? {
        helpers::identifier(
            std::str::from_utf8(key).map_err(|_| error(2, "invalid event field"))?,
        )?;
        require(
            !matches!(v, Value::Object(_) | Value::Array(_)),
            "event values must be scalar",
        )?;
    }
    helpers::scrub(value)
}
fn trigger_plan(ctx: &Context<'_>, definition: &Value, pins: Option<&Value>) -> Result<Value> {
    trigger_definition(definition)?;
    let mut opts = Options::default();
    for key in ["overlay"] {
        if !definition.text(key).is_empty() {
            opts.values
                .insert(key.into(), vec![definition.text(key).into()]);
        }
    }
    for key in ["inputs", "ports"] {
        if let Ok(values) = definition.get(key).object() {
            for (name, v) in values {
                let raw = match v {
                    Value::String(_) => v.str()?.to_owned(),
                    _ => String::from_utf8(json::canonical(v)?).map_err(|e| error(2, e))?,
                };
                opts.values
                    .entry(if key == "inputs" { "input" } else { "port" }.into())
                    .or_default()
                    .push(format!("{}={raw}", String::from_utf8_lossy(name)));
            }
        }
    }
    opts.values.insert(
        "with".into(),
        definition
            .get("companions")
            .strings()
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    );
    let plan = compile(
        ctx,
        source(ctx, definition.text("recipe"))?,
        &opts,
        definition.text("action"),
    )?;
    if let Some(pins) = pins {
        for key in ["definition_digest", "overlay_digest", "effective_digest"] {
            require(
                plan.get(key) == pins.get(key),
                "trigger recipe changed; register a new trigger id",
            )?;
        }
    }
    Ok(plan)
}
fn request(ctx: &Context<'_>, trigger: &Value, occurrence: &str, metadata: Value) -> Result<Value> {
    require(
        json::hash(trigger.get("definition"))? == trigger.text("digest"),
        "trigger digest mismatch",
    )?;
    let id = format!(
        "request-{}",
        &json::hash(&object([
            ("trigger", trigger.get("digest").clone()),
            ("occurrence", string(occurrence))
        ]))?[..32]
    );
    let path = record_path("requests", &id)?;
    if let Some(row) = existing(ctx, &path)? {
        return Ok(row);
    }
    require(
        records(ctx, "requests")?.len() < 1024,
        "request capacity reached",
    )?;
    let row = object([
        ("schema", number(1)),
        ("id", string(&id)),
        ("trigger_id", trigger.get("id").clone()),
        ("trigger_digest", trigger.get("digest").clone()),
        ("definition", trigger.get("definition").clone()),
        ("pins", trigger.get("pins").clone()),
        ("occurrence", string(occurrence)),
        ("created_at", string(timestamp(epoch())?)),
        ("status", string("pending")),
        ("metadata", metadata),
        (
            "authority",
            string("request only; explicit execution required"),
        ),
    ]);
    save(ctx, &path, &row)?;
    history(ctx, "recipe-requested", &id)?;
    Ok(row)
}
fn triggers(
    ctx: &Context<'_>,
    opts: &Options,
    services: &dyn oms_command_runs::Services,
) -> Result<Output> {
    let action = opts.pos[1].as_str();
    if ["list", "requests"].contains(&action) {
        require(opts.pos.len() == 2, "list takes no id")?;
        let group = if action == "list" {
            "triggers"
        } else {
            "requests"
        };
        let mut rows = records(ctx, group)?;
        if group == "requests" {
            for row in &mut rows {
                let status = if row.text("run_id").is_empty() {
                    row.get("status").clone()
                } else {
                    existing(ctx, &record_path("runs", row.text("run_id"))?)?
                        .map(|r| r.get("status").clone())
                        .unwrap_or_else(|| string("uncertain"))
                };
                row.set("effective_status", status)?;
            }
        }
        return json_output(
            0,
            &object([("schema", number(1)), (group, Value::Array(rows))]),
        );
    }
    if action == "plan" {
        require(opts.pos.len() == 3, "action requires request id")?;
        let row = load(ctx, &record_path("requests", &opts.pos[2])?)?;
        require(
            json::hash(row.get("definition"))? == row.text("trigger_digest"),
            "request definition digest mismatch",
        )?;
        let plan = trigger_plan(ctx, row.get("definition"), Some(row.get("pins")))?;
        return json_output(0, &public_plan(&plan)?);
    }
    recipe_parent_only()?;
    if action == "run" {
        require(opts.pos.len() == 3, "action requires request id")?;
        require(
            opts.flag("execute"),
            "execution requires explicit --execute; use plan to inspect the invocation",
        )?;
        let key = opts.pos[2].as_str();
        let req_path = record_path("requests", key)?;
        let _lock = storage::lock(&ctx.repo.join(&req_path))?;
        let mut row = load(ctx, &req_path)?;
        require(row.text("status") != "cancelled", "request was cancelled")?;
        require(
            json::hash(row.get("definition"))? == row.text("trigger_digest"),
            "request definition digest mismatch",
        )?;
        let plan = trigger_plan(ctx, row.get("definition"), Some(row.get("pins")))?;
        let (run_id, resume) = if row.text("run_id").is_empty() {
            (crate::random_id("run-")?, false)
        } else {
            let id = row.text("run_id").to_owned();
            let has_receipt = existing(ctx, &record_path("runs", &id)?)?.is_some();
            require(
                has_receipt || opts.flag("retry-uncertain"),
                "request has no run receipt after dispatch; --retry-uncertain is required",
            )?;
            (id, has_receipt)
        };
        row.set("status", string("running"))?;
        row.set("run_id", string(&run_id))?;
        row.set("updated_at", string(timestamp(epoch())?))?;
        save(ctx, &req_path, &row)?;
        history_with(
            ctx,
            "request-dispatched",
            key,
            &[("run_id", string(&run_id))],
        )?;
        let (run_value, exit_code) =
            execute_plan(ctx, &plan, opts, &run_id, resume, Some(key), services)?;
        row.set("status", run_value.get("status").clone())?;
        row.set("updated_at", string(timestamp(epoch())?))?;
        save(ctx, &req_path, &row)?;
        return json_output(exit_code, &object([("request", row), ("run", run_value)]));
    }
    let _coordinator = storage::lock(&ctx.repo.join(BASE).join("trigger-coordinator"))?;
    if action == "add" {
        require(opts.pos.len() == 3, "add requires FILE")?;
        let value = definition(ctx, helpers::relative(&opts.pos[2])?)?;
        let plan = trigger_plan(ctx, &value, None)?;
        let path = record_path("triggers", value.text("id"))?;
        if let Some(row) = existing(ctx, &path)? {
            require(
                row.text("digest") == json::hash(&value)?,
                "trigger definitions are immutable",
            )?;
            return json_output(0, &row);
        }
        require(
            records(ctx, "triggers")?.len() < 1024,
            "trigger capacity reached",
        )?;
        let mut pins = json::empty();
        for key in ["definition_digest", "overlay_digest", "effective_digest"] {
            pins.set(key, plan.get(key).clone())?;
        }
        let row = object([
            ("schema", number(1)),
            ("id", value.get("id").clone()),
            ("definition", value.clone()),
            ("digest", string(json::hash(&value)?)),
            ("enabled", Value::Bool(true)),
            ("created_at", string(timestamp(epoch())?)),
            ("last_index", number(-1)),
            ("pins", pins),
        ]);
        save(ctx, &path, &row)?;
        history(ctx, "trigger-added", value.text("id"))?;
        return json_output(0, &row);
    }
    if ["enable", "disable", "cancel"].contains(&action) {
        require(opts.pos.len() == 3, "action requires id")?;
        let path = record_path(
            if action == "cancel" {
                "requests"
            } else {
                "triggers"
            },
            &opts.pos[2],
        )?;
        let _guard = storage::lock(&ctx.repo.join(&path))?;
        let mut row = load(ctx, &path)?;
        if action == "cancel" {
            require(
                row.text("status") == "pending",
                "only pending requests may be cancelled",
            )?;
            row.set("status", string("cancelled"))?;
        } else {
            row.set("enabled", Value::Bool(action == "enable"))?;
        }
        save(ctx, &path, &row)?;
        history(ctx, action, &opts.pos[2])?;
        return json_output(0, &row);
    }
    if action == "tick" {
        require(opts.pos.len() == 2, "tick takes options only")?;
        let stamp = if opts.get("now").is_empty() {
            timestamp(epoch())?
        } else {
            opts.get("now").into()
        };
        let now = crate::parse_timestamp(&stamp).ok_or_else(|| error(2, "invalid tick instant"))?;
        let mut queued = Vec::new();
        for mut row in records(ctx, "triggers")? {
            let value = row.get("definition");
            trigger_definition(value)?;
            if !row.get("enabled").truth() || value.opt("schedule").is_none() {
                continue;
            }
            let due = crate::parse_timestamp(value.get("schedule").text("at"))
                .ok_or_else(|| error(2, "invalid schedule"))?;
            if now < due {
                continue;
            }
            let index = if let Some(interval) = value.get("schedule").opt("every_seconds") {
                (now - due) / bounded(interval, 0, 1, 366 * 86400)? as i64
            } else {
                0
            };
            let previous = row.get("last_index").int()?;
            if i128::from(index) <= previous {
                continue;
            }
            queued.push(request(
                ctx,
                &row,
                &format!("schedule:{index}"),
                object([
                    ("observed_at", string(&stamp)),
                    ("schedule_index", number(index)),
                    (
                        "coalesced_occurrences",
                        number(i128::from(index) - previous - 1),
                    ),
                ]),
            )?);
            row.set("last_index", number(index))?;
            save(ctx, &record_path("triggers", row.text("id"))?, &row)?;
        }
        return json_output(
            0,
            &object([
                ("schema", number(1)),
                ("at", string(stamp)),
                ("requests", Value::Array(queued)),
                ("executed", Value::Bool(false)),
            ]),
        );
    }
    if action == "event" {
        require(
            opts.pos.len() == 3 && !opts.get("event-id").is_empty(),
            "event requires NAME --event-id ID",
        )?;
        helpers::identifier(&opts.pos[2])?;
        helpers::identifier(opts.get("event-id"))?;
        let payload = if opts.get("data").is_empty() {
            json::empty()
        } else if opts.get("data").trim_start().starts_with('{') {
            json::parse(opts.get("data").as_bytes())?
        } else {
            load(ctx, helpers::relative(opts.get("data"))?)?
        };
        flat(&payload)?;
        let fingerprint = json::hash(&payload)?;
        let id = format!(
            "event-{}",
            &json::hash(&object([
                ("name", string(&opts.pos[2])),
                ("id", string(opts.get("event-id")))
            ]))?[..32]
        );
        let path = record_path("events", &id)?;
        let mut row = if let Some(previous) = existing(ctx, &path)? {
            require(
                previous.text("payload_digest") == fingerprint,
                "event id reused for different data",
            )?;
            if previous.text("status") == "submitted" {
                return json_output(0, &previous);
            }
            previous
        } else {
            require(
                records(ctx, "events")?.len() < 1024,
                "event capacity reached",
            )?;
            let mut matched = Vec::new();
            for trigger in records(ctx, "triggers")? {
                let filter = trigger.get("definition").get("event");
                let matches = filter
                    .get("match")
                    .object()
                    .unwrap_or(&[])
                    .iter()
                    .all(|(k, v)| {
                        payload
                            .opt(std::str::from_utf8(k).unwrap_or(""))
                            .is_some_and(|p| same_scalar_type(p, v) && p == v)
                    });
                if trigger.get("enabled").truth() && filter.text("name") == opts.pos[2] && matches {
                    matched.push(trigger.get("id").clone());
                }
            }
            let row = object([
                ("schema", number(1)),
                ("id", string(&id)),
                ("name", string(&opts.pos[2])),
                ("event_id", string(opts.get("event-id"))),
                ("payload_digest", string(&fingerprint)),
                ("matched", Value::Array(matched)),
                ("status", string("submitting")),
                ("submitted_at", string(timestamp(epoch())?)),
                ("requests", Value::Array(vec![])),
            ]);
            save(ctx, &path, &row)?;
            row
        };
        let mut requests = Vec::new();
        for key in row.get("matched").strings()? {
            let trigger = load(ctx, &record_path("triggers", key)?)?;
            requests.push(
                request(
                    ctx,
                    &trigger,
                    &format!("event:{}:{}", opts.pos[2], opts.get("event-id")),
                    object([
                        ("event", string(&opts.pos[2])),
                        ("event_id", string(opts.get("event-id"))),
                        ("payload_digest", string(&fingerprint)),
                    ]),
                )?
                .get("id")
                .clone(),
            );
        }
        row.set("requests", Value::Array(requests))?;
        row.set("status", string("submitted"))?;
        save(ctx, &path, &row)?;
        history(ctx, "event-submitted", &id)?;
        return json_output(0, &row);
    }
    Err(error(2, "unknown trigger action"))
}
pub(super) fn run(
    ctx: &Context<'_>,
    opts: &Options,
    services: &dyn oms_command_runs::Services,
) -> Result<Output> {
    require(opts.pos.len() >= 2, "catalog action required")?;
    match opts.pos[0].as_str() {
        "pack" => pack(ctx, opts),
        "trigger" => triggers(ctx, opts, services),
        "workset" => {
            let action = opts.pos[1].as_str();
            if action == "list" {
                return json_output(
                    0,
                    &object([
                        ("schema", number(1)),
                        ("worksets", Value::Array(records(ctx, "worksets")?)),
                    ]),
                );
            }
            require(opts.pos.len() == 3, "workset action requires FILE or ID")?;
            if action == "put" {
                recipe_parent_only()?;
                let definition = workset(
                    ctx,
                    definition(ctx, helpers::relative(&opts.pos[2])?)?,
                    true,
                )?;
                let id = definition.text("id");
                let path = record_path("worksets", id)?;
                let _guard = storage::lock(&ctx.repo.join(&path))?;
                let digest = json::hash(&definition)?;
                if let Some(previous) = existing(ctx, &path)? {
                    require(
                        previous.text("digest") == digest || opts.flag("replace"),
                        "workset changed; --replace required",
                    )?;
                } else {
                    require(
                        records(ctx, "worksets")?.len() < 1024,
                        "workset capacity reached",
                    )?;
                }
                save(
                    ctx,
                    &path,
                    &object([
                        ("schema", number(1)),
                        ("id", string(id)),
                        ("definition", definition.clone()),
                        ("digest", string(digest)),
                        ("updated_at", string(timestamp(epoch())?)),
                    ]),
                )?;
                history(ctx, "workset-linked", id)?;
                json_output(0, &workset(ctx, definition, false)?)
            } else {
                require(action == "show", "unknown workset action")?;
                let row = load(ctx, &record_path("worksets", &opts.pos[2])?)?;
                require(
                    json::hash(row.get("definition"))? == row.text("digest"),
                    "workset digest mismatch",
                )?;
                json_output(0, &workset(ctx, row.get("definition").clone(), false)?)
            }
        }
        _ => Err(error(2, "unknown recipe catalog")),
    }
}

fn same_scalar_type(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            a.contains(['.', 'e', 'E']) == b.contains(['.', 'e', 'E'])
        }
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}
