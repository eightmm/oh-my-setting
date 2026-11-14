//! Phase-controlled context assembly and native inspection consumers.
use crate::parser::Request;
use crate::storage::{MAX_STATE, Runtime};
use crate::value::*;
use crate::{Result, error, require};
use oms_safe_fs::{Policy, Root};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
const PHASES: &[&str] = &[
    "orientation",
    "implementation",
    "verification",
    "review",
    "research",
];
#[derive(Debug, Clone)]
pub struct ContextRequest {
    pub targets: Vec<OsString>,
    pub explicit: Vec<(OsString, String)>,
    pub required: Vec<OsString>,
    pub max_bytes: usize,
    pub bundle: Option<PathBuf>,
    pub manifest: Option<PathBuf>,
    pub allow_external: bool,
    pub phase: String,
}
struct Candidate {
    path: PathBuf,
    label: String,
    reason: String,
    priority: i32,
    policy: &'static str,
}
fn label(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| error("context label is not UTF-8"))
}
fn acquire(rt: &Runtime<'_>, raw: &OsStr, external: bool) -> Result<(PathBuf, String, Vec<u8>)> {
    let path = rt.user_path(raw)?;
    if path.is_absolute() {
        require(
            external,
            "external context source requires --allow-external",
        )?;
        let parent = path.parent().ok_or_else(|| error("invalid source path"))?;
        let root = Root::open("/").map_err(|e| error(format!("context source: {e:?}")))?;
        let raw = root
            .read_no_follow(
                &path,
                oms_safe_fs::ByteReadOptions {
                    missing_ok: false,
                    max_bytes: MAX_STATE,
                },
            )
            .map_err(|e| error(format!("context source: {e:?}")))?;
        let _ = parent;
        let name = path
            .file_name()
            .ok_or_else(|| error("invalid source path"))?;
        Ok((path.clone(), label(Path::new(name))?, raw))
    } else {
        let bytes = rt
            .bytes(&path, MAX_STATE)?
            .ok_or_else(|| error("context source does not exist"))?;
        Ok((path.clone(), label(&path)?, bytes.to_vec()))
    }
}
fn phase_rules(rt: &Runtime<'_>) -> Result<(Value, String)> {
    let store = oms_command_plan::storage::Store::open(&rt.install_root)?
        .with_group_resolver(rt.services.group_resolver());
    let raw = store.read("config/context-phases.json", MAX_STATE)?;
    let v = parse(&raw)?;
    keys(&v, &["schema", "phases"], &["note"])?;
    require(
        v.get("schema").is_int(1),
        "invalid context phase rules schema",
    )?;
    keys(v.get("phases"), PHASES, &[])?;
    for (_, r) in v.get("phases").object()? {
        keys(r, &[], &["nonrequired_divisor", "priority"])?;
        if let Some(n) = r.opt("nonrequired_divisor") {
            require(
                n.int().is_ok_and(|n| (2..=16).contains(&n)),
                "context phase nonrequired_divisor must be an integer from 2 to 16",
            )?;
        }
        if let Some(p) = r.opt("priority") {
            keys(
                p,
                &[],
                &[
                    "task-packet",
                    "pins",
                    "memory-summary",
                    "work-journal",
                    "related",
                    "related-test",
                    "explicit",
                ],
            )?;
            for (_, delta) in p.object()? {
                require(
                    delta.int().is_ok_and(|n| (-60..=60).contains(&n)),
                    "context phase priority must be an integer from -60 to 60",
                )?;
            }
        }
    }
    Ok((v, sha(raw)))
}
fn moved(priority: i32, class: &str, rule: &Value) -> i32 {
    let delta = rule.get("priority").get(class).int().unwrap_or(0) as i32;
    if delta == 0 {
        priority
    } else {
        (priority + delta).clamp(1, 119)
    }
}
pub fn slice(data: &[u8], budget: usize, policy: &str) -> (Vec<u8>, usize, String) {
    if data.len() <= budget {
        return (data.to_vec(), 0, "full".into());
    }
    if budget == 0 {
        return (vec![], data.len(), "omitted".into());
    }
    let marker = format!(
        "\n[TRUNCATED: {} bytes omitted]\n",
        data.len().saturating_sub(budget)
    )
    .into_bytes();
    let usable = budget.saturating_sub(marker.len());
    let mut out = Vec::with_capacity(budget);
    match policy {
        "tail" => {
            out.extend(&marker);
            out.extend(&data[data.len().saturating_sub(usable)..]);
        }
        "head" => {
            out.extend(&data[..usable.min(data.len())]);
            out.extend(&marker)
        }
        _ => {
            let left = usable / 2;
            out.extend(&data[..left]);
            out.extend(&marker);
            out.extend(&data[data.len() - (usable - left)..]);
        }
    }
    out.truncate(budget);
    (out, data.len() - usable.min(data.len()), policy.into())
}
pub fn plan(rt: &Runtime<'_>, request: &ContextRequest) -> Result<Value> {
    rt.parent_write()?;
    require(
        PHASES.contains(&request.phase.as_str()),
        "unsupported context phase",
    )?;
    require(
        (4096..=524288).contains(&request.max_bytes),
        "context budget must be between 4096 and 524288 bytes",
    )?;
    let (table, table_hash) = phase_rules(rt)?;
    let rule = table.get("phases").get(&request.phase);
    let divisor = rule.get("nonrequired_divisor").int().unwrap_or(3) as usize;
    let max_optional = 4096.max(request.max_bytes / divisor);
    let mut candidates = Vec::<Candidate>::new();
    let mut bytes = BTreeMap::<PathBuf, Vec<u8>>::new();
    let mut required = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    let mut targets = vec![];
    let mut target_paths = vec![];
    for (path, reason, priority, policy, class) in [
        ("PROJECT.md", "project contract", 125, "head", ""),
        (
            ".oms/task/current.md",
            "active task packet",
            100,
            "tail",
            "task-packet",
        ),
        (
            ".oms/memory/pins.md",
            "pinned project facts",
            96,
            "tail",
            "pins",
        ),
        (
            ".oms/memory/summary.md",
            "compacted project memory",
            55,
            "tail",
            "memory-summary",
        ),
        (
            ".oms/work-journal/today.md",
            "current work journal",
            35,
            "tail",
            "work-journal",
        ),
    ] {
        if let Ok(Some(raw)) = rt.bytes(path, MAX_STATE) {
            let path = PathBuf::from(path);
            bytes.insert(path.clone(), raw.to_vec());
            candidates.push(Candidate {
                label: label(&path)?,
                path,
                reason: reason.into(),
                priority: moved(priority, class, rule),
                policy,
            });
        }
    }
    for raw in &request.targets {
        if raw.is_empty() {
            continue;
        }
        let (path, name, data) = acquire(rt, raw, request.allow_external)?;
        if !targets.contains(&name) {
            targets.push(name.clone())
        }
        if !target_paths.contains(&path) {
            target_paths.push(path.clone())
        }
        required.insert(name.clone());
        bytes.entry(path.clone()).or_insert(data);
        candidates.push(Candidate {
            path,
            label: name,
            reason: "direct target".into(),
            priority: 120,
            policy: "middle",
        });
    }
    if !target_paths.is_empty() {
        for related in rt.services.context_dependencies(&rt.repo, &target_paths)? {
            let (path, name, data) = acquire(rt, related.path.as_os_str(), false)?;
            require(
                hex(&related.source_sha256, 64) && sha(&data) == related.source_sha256,
                "context dependency changed since native discovery",
            )?;
            let class = if related.reason.ends_with("related test")
                || name.starts_with("tests/")
                || name.starts_with("test/")
                || name.contains("/tests/")
                || Path::new(&name)
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|s| {
                        s.starts_with("test_")
                            || s.ends_with("_test.py")
                            || s.ends_with("-smoke.sh")
                            || s.ends_with("_test.sh")
                    }) {
                "related-test"
            } else {
                "related"
            };
            bytes.entry(path.clone()).or_insert(data);
            candidates.push(Candidate {
                path,
                label: name,
                reason: related.reason,
                priority: moved(related.priority, class, rule),
                policy: "middle",
            });
        }
    }
    for (raw, reason) in &request.explicit {
        let (path, name, data) = acquire(rt, raw, request.allow_external)?;
        bytes.entry(path.clone()).or_insert(data);
        candidates.push(Candidate {
            path,
            label: name,
            reason: if reason.is_empty() {
                "explicit context".into()
            } else {
                reason.clone()
            },
            priority: moved(110, "explicit", rule),
            policy: "middle",
        });
    }
    let mut dirs = BTreeSet::from([PathBuf::new()]);
    for target in &target_paths {
        if target.is_absolute() {
            continue;
        }
        let mut cursor = target.parent();
        while let Some(path) = cursor {
            dirs.insert(path.to_path_buf());
            cursor = path.parent();
        }
    }
    let mut needed = request.required.clone();
    for dir in dirs {
        let path = dir.join("AGENTS.md");
        match rt.bytes(&path, MAX_STATE) {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => needed.push(path.into_os_string()),
        }
    }
    for raw in needed {
        match acquire(rt, &raw, request.allow_external) {
            Ok((path, name, data)) => {
                required.insert(if path.is_absolute() {
                    label(&path)?
                } else {
                    name.clone()
                });
                bytes.entry(path.clone()).or_insert(data);
                candidates.push(Candidate {
                    path,
                    label: name,
                    reason: "required context".into(),
                    priority: 130,
                    policy: "middle",
                });
            }
            Err(_) => {
                unresolved.insert(raw.to_string_lossy().into_owned());
            }
        }
    }
    if std::fs::symlink_metadata(rt.repo.join("PROJECT.md")).is_ok() {
        required.insert("PROJECT.md".into());
    }
    let mut best = BTreeMap::<PathBuf, Candidate>::new();
    for c in candidates {
        if best.get(&c.path).is_none_or(|p| c.priority > p.priority) {
            best.insert(c.path.clone(), c);
        }
    }
    let mut ordered = best.into_values().collect::<Vec<_>>();
    ordered.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.label.cmp(&b.label)));
    let mut selected = vec![];
    let mut omitted = vec![];
    let mut bundle = vec![];
    let mut included = BTreeSet::new();
    let mut truncated = BTreeSet::new();
    for candidate in ordered {
        rt.check()?;
        let data = bytes
            .get(&candidate.path)
            .ok_or_else(|| error("missing source snapshot"))?;
        let why = match std::str::from_utf8(data) {
            Err(_) => Some("non-UTF-8 source"),
            Ok(t) if crate::capsule::sensitive(t) => Some("sensitive-looking content"),
            _ => None,
        };
        let mut row = object([
            ("path", string(&candidate.label)),
            ("reason", string(why.unwrap_or(&candidate.reason))),
            ("priority", number(candidate.priority)),
            ("bytes", number(data.len())),
        ]);
        if why.is_some() {
            omitted.push(row);
            continue;
        }
        let header = format!(
            "\n--- begin context: {} ({}) ---\n",
            candidate.label, candidate.reason
        );
        let footer = format!("\n--- end context: {} ---\n", candidate.label);
        let remaining = request.max_bytes - bundle.len();
        let mut allocation = remaining
            .saturating_sub(header.len() + footer.len())
            .min(4096.max(request.max_bytes / 2));
        if !required.contains(&candidate.label) {
            allocation = allocation.min(max_optional)
        }
        if allocation == 0 {
            row.set("reason", string("budget exhausted"))?;
            omitted.push(row);
            continue;
        }
        let (part, omitted_bytes, policy) = slice(data, allocation, candidate.policy);
        if header.len() + part.len() + footer.len() > remaining {
            row.set("reason", string("budget exhausted"))?;
            omitted.push(row);
            continue;
        }
        bundle.extend(header.as_bytes());
        bundle.extend(&part);
        bundle.extend(footer.as_bytes());
        included.insert(candidate.label.clone());
        if omitted_bytes > 0 && required.contains(&candidate.label) {
            truncated.insert(candidate.label.clone());
        }
        selected.push(object([
            ("path", string(candidate.label)),
            ("reason", string(candidate.reason)),
            ("priority", number(candidate.priority)),
            ("bytes_before", number(data.len())),
            ("bytes_selected", number(part.len())),
            ("omitted_bytes", number(omitted_bytes)),
            ("selection", string(policy)),
            ("sha256", string(sha(data))),
        ]));
    }
    let missing = unresolved
        .into_iter()
        .chain(required.difference(&included).cloned())
        .chain(truncated.iter().cloned())
        .collect::<BTreeSet<_>>();
    require(
        !crate::capsule::sensitive(&String::from_utf8_lossy(&bundle)),
        "compiled context contains sensitive-looking content",
    )?;
    let mut manifest = object([
        ("schema", number(1)),
        ("generated_at", string(&rt.now)),
        ("phase", string(&request.phase)),
        ("phase_rules_sha256", string(table_hash)),
        ("budget_bytes", number(request.max_bytes)),
        ("targets", list(targets.iter().map(String::as_str))),
        ("selected_bytes", number(bundle.len())),
        ("remaining_bytes", number(request.max_bytes - bundle.len())),
        ("max_nonrequired_source_bytes", number(max_optional)),
        ("selected", a(selected)),
        ("omitted", a(omitted)),
        ("context_debt", number(missing.len())),
        ("missing_required", list(missing.iter().map(String::as_str))),
        (
            "truncated_required",
            list(truncated.iter().map(String::as_str)),
        ),
        ("sufficient", b(missing.is_empty())),
        ("bundle_sha256", string(sha(&bundle))),
        (
            "contract_state_digest",
            crate::projection::envelope(rt)?.get("state_digest").clone(),
        ),
    ]);
    let digest = hash(&manifest)?;
    let bundle_path = request
        .bundle
        .clone()
        .unwrap_or_else(|| format!(".oms/runtime/context/{digest}.txt").into());
    let manifest_path = request
        .manifest
        .clone()
        .unwrap_or_else(|| format!(".oms/runtime/context/{digest}.json").into());
    if request.bundle.is_some() {
        rt.write_output(&bundle_path, &bundle, MAX_STATE)?;
    } else {
        rt.write(&bundle_path, &bundle, MAX_STATE)?;
    }
    manifest.set("bundle_path", rt.output_label(&bundle_path))?;
    if request.manifest.is_some() {
        rt.write_json_output(&manifest_path, &manifest)?;
    } else {
        rt.write_json(&manifest_path, &manifest)?;
    }
    manifest.set("manifest_path", rt.output_label(&manifest_path))?;
    Ok(manifest)
}
pub fn inspect(rt: &Runtime<'_>, request: &Request) -> Result<Value> {
    let injections = request.has("injections");
    let session = request.os("session");
    let manifest = request.positionals.first();
    let limit = request.integer("limit", 10)?;
    let limit = usize::try_from(limit).unwrap_or(0);
    let modes =
        usize::from(injections) + usize::from(session.is_some()) + usize::from(manifest.is_some());
    require(
        modes == 1
            && (manifest.is_some() || !request.has("compare") && !request.has("verify-current")),
        "context-inspect takes one of: MANIFEST, --injections, --session TRANSCRIPT (--compare and --verify-current need MANIFEST)",
    )?;
    require(
        !request.has("session-hash") || injections,
        "--session-hash needs --injections",
    )?;
    require(
        !request.has("timeline") || session.is_some(),
        "--timeline needs --session TRANSCRIPT",
    )?;
    require(
        !(request.has("compare") && request.has("verify-current")),
        "--verify-current cannot be combined with --compare",
    )?;
    let cancel = || rt.interrupted.load(std::sync::atomic::Ordering::Relaxed);
    if injections {
        return from_serde(
            &oms_context_source::inspect_injections(
                &rt.state_repo,
                request
                    .os("session-hash")
                    .map(|s| s.to_str().unwrap_or("\0")),
                limit,
                cancel,
            )
            .map_err(error)?,
        );
    }
    if let Some(session) = session {
        let path = rt.user_path(session)?;
        let path =
            if Path::new(session).is_absolute() || session.as_encoded_bytes().starts_with(b"~") {
                if path.is_absolute() {
                    path
                } else {
                    rt.repo.join(path)
                }
            } else {
                rt.cwd.join(session)
            };
        return from_serde(
            &if request.has("timeline") {
                oms_context_source::inspect_timeline(&path, 200, cancel)
            } else {
                oms_context_source::inspect_session(&rt.repo, &path, limit, cancel)
            }
            .map_err(error)?,
        );
    }
    let left_path = PathBuf::from(manifest.unwrap());
    let root = Root::open(rt.source_root(&left_path))
        .map_err(|e| error(format!("context manifest: {e:?}")))?;
    let left = root
        .read_manifest(&left_path, 4 * 1024 * 1024)
        .map_err(|e| error(format!("context manifest: {e:?}")))?;
    let mut result = if let Some(right) = request.os("compare") {
        let path = PathBuf::from(right);
        let right_root = Root::open(rt.source_root(&path))
            .map_err(|e| error(format!("context manifest: {e:?}")))?;
        let right = right_root
            .read_manifest(&path, 4 * 1024 * 1024)
            .map_err(|e| error(format!("context manifest: {e:?}")))?;
        oms_context::compare_manifests(&left, &right, limit)
    } else {
        oms_context::inspect_manifest(&left, limit)
    }
    .map_err(error)?;
    if request.has("verify-current") {
        let v = parse(&left)?;
        let mut sources = rows(v.get("selected")).to_vec();
        sources.sort_by(|a, b| a.text("path").cmp(b.text("path")));
        let mut counts = [0usize; 4];
        let mut changed = vec![];
        let mut missing = vec![];
        for source in sources {
            rt.check()?;
            let path = crate::value::path(source.get("path"))?;
            let sources = Root::open(rt.source_root(Path::new(&path)))
                .map_err(|e| error(format!("context sources: {e:?}")))?;
            let state = match sources.read_os(&path, 2 * 1024 * 1024, Policy::source()) {
                Ok(raw) => {
                    if sha(raw) == source.text("sha256") {
                        0
                    } else {
                        1
                    }
                }
                Err(oms_safe_fs::Error::Missing) => 2,
                Err(_) => 3,
            };
            counts[state] += 1;
            if state == 1 {
                changed.push(source.text("path").to_owned())
            } else if state == 2 {
                missing.push(source.text("path").to_owned())
            }
        }
        let bounded = |v: Vec<String>| {
            let total = v.len();
            serde_json::json!({"total":total,"items":v.into_iter().take(limit).collect::<Vec<_>>(),"omitted":total.saturating_sub(limit)})
        };
        result["sources_current"] = serde_json::json!({"unchanged":counts[0],"changed":counts[1],"missing":counts[2],"unreadable":counts[3],"changed_paths":bounded(changed),"missing_paths":bounded(missing)});
        result["note"] = serde_json::json!(
            "Not the provider's full context; the bundle was not read. Repeated source hashes do not prove duplicate selected bytes. No token or billing savings are inferred."
        );
    }
    from_serde(&result)
}
