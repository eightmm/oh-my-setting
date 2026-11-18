use crate::completion::{CommitLanding, reference, relative};
use crate::json::{self, Value, number, object, string};
use crate::storage::{MAX_JSON, Store};
use crate::{Plan, Result, error, require};
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

fn checksum(raw: &[u8]) -> String {
    let step = |mut crc: u32, b: u8| {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x80000000 != 0 {
                (crc << 1) ^ 0x04c11db7
            } else {
                crc << 1
            };
        }
        crc
    };
    let mut crc = 0;
    for byte in raw {
        crc = step(crc, *byte);
    }
    let mut len = raw.len();
    while len > 0 {
        crc = step(crc, (len & 255) as u8);
        len >>= 8;
    }
    format!("{}-{}", !crc, raw.len())
}
pub(crate) fn canonical_land_directory(plan: &Plan<'_>) -> Result<PathBuf> {
    let common = plan.git().namespace_path()?;
    let root = common
        .parent()
        .ok_or_else(|| error("Git common parent unavailable"))?;
    let raw = root
        .file_name()
        .ok_or_else(|| error("Git common parent name unavailable"))?
        .as_bytes();
    let mut name = raw
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(b) {
                char::from(*b)
            } else {
                '_'
            }
        })
        .collect::<String>();
    name = name.replace("..", "_");
    if matches!(name.as_str(), "" | "." | "..") {
        name = "repo".into();
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| plan.context.lock_settings.home.join(".local/state"));
    Ok(base
        .join("oh-my-setting/land")
        .join(format!("{name}-{}", checksum(root.as_os_str().as_bytes()))))
}
fn directories(primary: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let mut found = vec![(primary.to_path_buf(), false)];
    let parent = primary
        .parent()
        .ok_or_else(|| error("land receipt namespace unavailable"))?;
    let entries = match std::fs::read_dir(parent) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(_) => return Err(error("cannot inspect land receipt parent")),
    };
    let mut names = Vec::new();
    for entry in entries {
        names.push(
            entry
                .map_err(|_| error("land receipt directory unavailable"))?
                .file_name(),
        );
    }
    names.sort();
    for name in names.into_iter().take(512) {
        let p = parent.join(name);
        let meta =
            std::fs::symlink_metadata(&p).map_err(|_| error("land receipt sibling unavailable"))?;
        if p != primary && meta.is_dir() && !meta.file_type().is_symlink() {
            found.push((p, true));
        }
    }
    Ok(found)
}
fn entries(
    directory: &Path,
    sibling: bool,
    wanted: Option<&str>,
    groups: Option<crate::services::GroupResolver>,
    snapshot: Option<std::sync::Arc<crate::storage::GroupProofSnapshot>>,
) -> Result<Vec<(String, Vec<u8>, Value)>> {
    let metadata = match std::fs::symlink_metadata(directory) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => return Err(error("cannot read oms land receipts")),
    };
    require(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "land receipts require a real directory",
    )?;
    let store = Store::open(directory)?
        .with_group_resolver(groups)
        .with_group_snapshot(snapshot);
    let mut out = Vec::new();
    for name in store.names(".", 65536, false)? {
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        let stem = name.split('-').next().unwrap_or("");
        if !crate::repository::commit(stem) || wanted.is_some_and(|sha| stem != sha) {
            continue;
        }
        let raw = match store.read(name, 1024 * 1024) {
            Ok(r) => r,
            Err(_) if sibling => continue,
            Err(e) => return Err(e),
        };
        let value = match json::parse_legacy(&raw, 1024 * 1024) {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push((name.into(), raw, value));
    }
    Ok(out)
}
fn sibling_binds(plan: &Plan<'_>, receipt: &Value, name: &str) -> Result<bool> {
    let remote = receipt.text("remote");
    let target = receipt.text("target");
    let command = receipt.get("gate").get("command");
    if remote.is_empty() || remote.starts_with('-') || target.is_empty() || command.str().is_err() {
        return Ok(false);
    }
    let url = plan.git().text(&["remote", "get-url", "--push", remote])?;
    let urls = plan
        .git()
        .text(&["remote", "get-url", "--push", "--all", remote])?;
    let lines = urls.lines().collect::<Vec<_>>();
    if lines.len() != 1 || lines[0].is_empty() {
        return Ok(false);
    }
    let destination = if !lines[0].contains(':') {
        std::fs::canonicalize(plan.context.execution_repo.join(lines[0]))
            .ok()
            .map(|p| p.as_os_str().as_bytes().to_vec())
    } else {
        Some(lines[0].as_bytes().to_vec())
    };
    if destination.as_deref() != Some(url.as_bytes()) {
        return Ok(false);
    }
    let payload = Value::Array(vec![
        string(&url),
        string(remote),
        string(target),
        command.clone(),
    ]);
    let digest = json::sha(json::default_bytes(&payload, true)?);
    let request = name.strip_suffix(".json").unwrap_or("");
    Ok(receipt.text("request") == request
        && request.ends_with(&format!("-{}", &digest[..24]))
        && name.starts_with(&format!("{}-", receipt.text("sha"))))
}
fn base(receipt: &Value) -> Result<Option<&str>> {
    let Some(base) = receipt.get("push").opt("base") else {
        return Ok(None);
    };
    require(
        crate::repository::commit(base.str()?),
        "invalid receipt push.base",
    )?;
    Ok(Some(base.str()?))
}
pub(crate) fn commit_landing(
    plan: &Plan<'_>,
    commit: &str,
    expected: Option<&str>,
) -> Result<CommitLanding> {
    require(
        crate::repository::commit(commit),
        "--landed-commit must be a full lowercase commit SHA",
    )?;
    plan.git().guard()?;
    let primary = canonical_land_directory(plan)?;
    let mut legacy = None;
    let mut passed = false;
    let mut unfinished = None;
    for (directory, sibling) in directories(&primary)? {
        for (name, raw, receipt) in entries(
            &directory,
            sibling,
            Some(commit),
            plan.services.group_membership_resolver(),
            plan.group_snapshot.clone(),
        )? {
            if !name.starts_with(&format!("{commit}-"))
                || expected.is_some_and(|d| d != json::sha(&raw))
            {
                continue;
            }
            if !receipt.get("schema").is_int(1)
                || receipt.text("sha") != commit
                || sibling && !sibling_binds(plan, &receipt, &name).unwrap_or(false)
            {
                continue;
            }
            if !receipt.get("gate").get("rc").is_int(0)
                || !receipt.get("push").get("rc").is_int(0)
                || receipt.get("ci").text("conclusion") != "success"
            {
                continue;
            }
            let update = receipt.get("update");
            let deferred =
                update.get("rc").is_int(76) && update.text("reason") == "deferred-live-workers";
            if receipt.text("state") != "passed" && !deferred {
                unfinished = Some(receipt.text("state").to_owned());
                continue;
            }
            passed = true;
            let remote = receipt.text("remote");
            let target = receipt.text("target");
            if remote.is_empty() || target.is_empty() {
                continue;
            }
            let Ok(base) = base(&receipt) else {
                continue;
            };
            if base == Some(commit) || receipt.get("push").get("moved").is_int(0) {
                continue;
            }
            let reference = format!("refs/remotes/{remote}/{target}");
            if plan.git().run(&["check-ref-format", &reference]).is_err()
                || plan.git().ancestor(commit, &reference).is_err()
                || base.is_some_and(|b| plan.git().ancestor(b, commit).is_err())
            {
                continue;
            }
            let landing = object([
                ("kind", string("commit")),
                ("sha", string(commit)),
                ("receipt_sha256", string(json::sha(&raw))),
            ]);
            let actual = CommitLanding {
                landing,
                receipt: receipt.clone(),
                raw,
                name,
                directory: directory.clone(),
            };
            if base.is_some() {
                return Ok(actual);
            }
            if legacy.is_none() {
                legacy = Some(actual);
            }
        }
    }
    if let Some(proof) = legacy {
        return Ok(proof);
    }
    if passed {
        return Err(error(format!(
            "landed commit {commit} is not reachable from a passed receipt's local pushed target ref; fetch its remote first"
        )));
    }
    if let Some(state) = unfinished {
        return Err(error(format!(
            "no passed oms land receipt for {commit}: gate, push and CI succeeded but receipt state is {state:?}; re-run oms land to finish installation"
        )));
    }
    Err(error(format!(
        "no passed oms land receipt for {commit} with gate/push ok and CI success; pass full head SHA that oms land pushed"
    )))
}
pub(crate) fn landed_paths(plan: &Plan<'_>, proof: &CommitLanding) -> Result<Vec<String>> {
    let commit = proof.landing.text("sha");
    let primary = canonical_land_directory(plan)?;
    let dirs = directories(&primary)?;
    let mut all = Vec::new();
    for (dir, sibling) in &dirs {
        for entry in entries(
            dir,
            *sibling,
            None,
            plan.services.group_membership_resolver(),
            plan.group_snapshot.clone(),
        )? {
            all.push((dir.clone(), *sibling, entry));
        }
    }
    require(
        all.iter()
            .any(|(_, _, (_, raw, _))| json::sha(raw) == proof.landing.text("receipt_sha256")),
        "landed commit receipt changed before range selection",
    )?;
    let mut base = base(&proof.receipt)?.map(str::to_owned);
    if let Some(mut current) = base.clone() {
        require(
            current != commit,
            "receipt push.base does not describe nonempty landed range",
        )?;
        plan.git().ancestor(&current, commit)?;
        let mut seen = BTreeSet::from([commit.to_owned()]);
        for _ in 0..32 {
            seen.insert(current.clone());
            let mut earlier = None;
            let mut prior_passed = false;
            for (_, sibling, (name, _, r)) in &all {
                if r.text("sha") != current
                    || !r.get("schema").is_int(1)
                    || *sibling && !sibling_binds(plan, r, name).unwrap_or(false)
                {
                    continue;
                }
                if r.text("state") == "passed" {
                    prior_passed = true;
                    break;
                }
                if earlier.is_none()
                    && r.get("push").get("rc").is_int(0)
                    && !r.get("push").get("moved").is_int(0)
                    && let Ok(Some(b)) = crate::product::base(r)
                    && b != current
                {
                    earlier = Some(b.to_owned());
                }
            }
            if prior_passed {
                break;
            }
            let Some(next) = earlier else {
                break;
            };
            if seen.contains(&next) || plan.git().ancestor(&next, &current).is_err() {
                break;
            }
            current = next;
        }
        base = Some(current);
    } else {
        let pushed = all
            .iter()
            .filter(|(directory, _, (_, _, r))| {
                directory == &proof.directory
                    && r.text("sha") != commit
                    && r.get("push").get("rc").is_int(0)
                    && !r.get("push").get("moved").is_int(0)
            })
            .map(|(_, _, (_, _, r))| r.text("sha"))
            .collect::<BTreeSet<_>>();
        let chain = plan
            .git()
            .text(&["rev-list", "--first-parent", "--max-count=1000", commit])?;
        let chain = chain.split_whitespace().collect::<Vec<_>>();
        base = chain
            .iter()
            .skip(1)
            .find(|s| pushed.contains(**s))
            .copied()
            .or_else(|| chain.get(1).copied())
            .map(str::to_owned);
    }
    let mut args = vec!["diff-tree", "-r", "-z", "--name-only", "--no-commit-id"];
    if let Some(ref b) = base {
        args.extend([b, commit]);
    } else {
        args.extend(["--root", commit]);
    }
    paths(&plan.git().run(&args)?)
}
fn paths(raw: &[u8]) -> Result<Vec<String>> {
    raw.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8(s.to_vec()).map_err(|_| error("product path is not UTF-8")))
        .collect()
}
pub(crate) fn legacy_relative(plan: &Plan<'_>, value: &str) -> Result<String> {
    let path = Path::new(value);
    let rel = if path.is_absolute() {
        path.strip_prefix(&plan.context.state_repo)
            .map_err(|_| error("historical evidence path is outside repository"))?
            .to_str()
            .ok_or_else(|| error("historical evidence path is not UTF-8"))?
    } else {
        value
    };
    relative(rel)?;
    Ok(rel.into())
}
pub(crate) fn satisfaction(
    plan: &Plan<'_>,
    bundle: &Value,
    tasks: &Value,
    plan_id: &str,
) -> Result<Value> {
    let successor = bundle.get("successor");
    let kind = successor.text("product_kind");
    let mut fields = vec![
        "plan_id",
        "task_id",
        "task_sha256",
        "original_contract",
        "product_kind",
    ];
    match kind {
        "patch-land" => fields.extend(["landing_id", "admission_event_id", "land_event_id"]),
        "commit" => fields.extend(["commit_sha", "land_receipt_sha256"]),
        _ => return Err(error("unsupported successor product kind")),
    };
    json::keys(successor, &fields, &[])?;
    require(
        successor.text("plan_id") == plan_id && successor.get("task_id") != bundle.get("task_id"),
        "archived/cross-plan successors and cycles are unsupported",
    )?;
    let task = tasks
        .opt(successor.get("task_id").str()?)
        .ok_or_else(|| error("unknown successor identity"))?;
    require(
        task.text("state") == "done"
            && !task.get("completion_kind").truth()
            && json::receipt_digest(task)? == successor.text("task_sha256")
            && json::projection(task)? == *successor.get("original_contract"),
        "successor must independently complete exact actual landing contract",
    )?;
    let mut pending = vec![successor.text("task_id").to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        require(
            current != bundle.text("task_id"),
            "successor depends on original task",
        )?;
        if visited.insert(current.clone())
            && let Some(t) = tasks.opt(&current)
            && let Some(deps) = t.opt("depends")
        {
            pending.extend(deps.strings()?.iter().map(|s| s.to_string()));
        }
    }
    if kind == "commit" {
        return commit_product(plan, bundle, task);
    }
    require(
        !task.get("landing").truth() && !task.get("landed_commit").truth(),
        "patch successor has contradictory commit landing fields",
    )?;
    let rows = plan.store.rows(".oms/landings.jsonl", false)?;
    let entries = rows
        .iter()
        .filter(|r| r.get("landing_id") == successor.get("landing_id"))
        .collect::<Vec<_>>();
    let intents = entries
        .iter()
        .filter(|r| r.text("event") == "intent")
        .collect::<Vec<_>>();
    require(
        intents.len() == 1,
        "exact immutable landing intent unavailable",
    )?;
    let intent = *intents[0];
    let fields = [
        "landing_id",
        "patch",
        "patch_sha",
        "base_sha",
        "task",
        "lease",
        "plan_receipt_sha",
        "plan_done_receipt_sha",
        "approval",
        "approval_version",
    ];
    let mut canonical = object([("schema", number(1))]);
    for field in fields {
        intent.get(field).str()?;
        canonical.set(field, intent.get(field).clone())?;
    }
    let hash = json::hash(&canonical)?;
    let completed = entries
        .iter()
        .filter(|r| {
            r.text("event") == "complete"
                && r.get("schema").is_int(1)
                && r.text("plan_id") == plan_id
                && fields.iter().all(|f| r.get(f) == intent.get(f))
                && r.text("receipt_sha") == hash
        })
        .collect::<Vec<_>>();
    require(
        !completed.is_empty()
            && !entries.iter().any(|r| r.text("event") == "abandoned")
            && intent.text("plan_id") == plan_id
            && intent.get("task") == successor.get("task_id")
            && intent.get("lease") == task.get("lease_id")
            && intent.text("plan_done_receipt_sha") == json::receipt_digest(task)?
            && intent.get("patch") == task.get("patch")
            && intent.get("base_sha") == bundle.get("base_commit"),
        "successor lacks exact terminal successful patch-land receipt",
    )?;
    let patch_ref = object([
        ("path", string(legacy_relative(plan, intent.text("patch"))?)),
        ("sha256", string(json::digest(intent.get("patch_sha"))?)),
    ]);
    let patch = reference(plan, &patch_ref)?;
    require(
        !patch.iter().all(u8::is_ascii_whitespace),
        "no-op patch cannot satisfy obligations",
    )?;
    let index = plan.store.rows(".oms/artifacts/index.jsonl", false)?;
    let indexed = |event: &str, kind: &str| -> Result<Value> {
        let rows = index
            .iter()
            .filter(|r| r.text("event_id") == event)
            .collect::<Vec<_>>();
        require(rows.len() == 1, "successor indexed receipt missing")?;
        let row = rows[0];
        require(
            row.text("kind") == kind
                && row.get("exit").is_int(0)
                && row.text("plan_id") == plan_id
                && row.get("task_id") == successor.get("task_id")
                && row.get("patch_sha256") == patch_ref.get("sha256"),
            "successor admission/land index mismatch",
        )?;
        if kind == "patch-admit" && row.opt("patch_external").is_some() {
            let ext = row.get("patch_external");
            json::keys(ext, &["name", "owned", "sha256"], &[])?;
            crate::completion::text(ext.get("name").str()?, "external patch name", 1000)?;
            require(
                row.opt("patch").is_none()
                    && ext.get("owned") == &Value::Bool(false)
                    && ext.get("sha256") == patch_ref.get("sha256"),
                "successor admission external patch mismatch",
            )?;
        } else {
            require(
                row.opt("patch_external").is_none()
                    && legacy_relative(plan, row.get("patch").str()?)? == patch_ref.text("path"),
                "successor admission/land index mismatch",
            )?;
        }
        Ok(row.clone())
    };
    let admission = indexed(successor.get("admission_event_id").str()?, "patch-admit")?;
    let landed = indexed(successor.get("land_event_id").str()?, "patch-land")?;
    let report = object([
        ("path", admission.get("artifact").clone()),
        ("sha256", admission.get("artifact_sha256").clone()),
    ]);
    require(
        reference(plan, &report)?.starts_with(b"# Patch admission: ADMIT\n"),
        "successor admission verdict is not successful",
    )?;
    crate::approval::landing_authority(plan, intent, completed.last().unwrap())?;
    let product = crate::repository::ProductIndex::new(&plan.git())?;
    product.run(&plan.git(), &["read-tree", intent.text("base_sha")], None)?;
    product.run(
        &plan.git(),
        &["apply", "--cached", "--binary", "-"],
        Some(&patch),
    )?;
    let changed = paths(&product.run(
        &plan.git(),
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            intent.text("base_sha"),
        ],
        None,
    )?)?;
    let envelope = bundle.get("source_envelope").strings()?;
    require(
        !changed.is_empty() && changed.iter().all(|p| envelope.contains(&p.as_str())),
        "successor patch empty or product manifest omits changed paths",
    )?;
    for item in bundle.get("source_manifest").array()? {
        let listing = product.run(
            &plan.git(),
            &["ls-files", "--stage", "-z", "--", item.text("path")],
            None,
        )?;
        validate_product_entry(
            &listing,
            item,
            |oid| product.run(&plan.git(), &["cat-file", "blob", oid], None),
            false,
        )?;
    }
    require(
        bundle.get("requirements").array()?.iter().any(|r| {
            r.get("evidence")
                .array()
                .is_ok_and(|e| e.contains(&report) || e.contains(&patch_ref))
        }),
        "actual successor evidence missing from obligation map",
    )?;
    Ok(object([
        ("intent", intent.clone()),
        ("complete", (***completed.last().unwrap()).clone()),
        ("admission", admission),
        ("land", landed),
    ]))
}
fn validate_product_entry(
    raw: &[u8],
    item: &Value,
    read: impl FnOnce(&str) -> Result<Vec<u8>>,
    tree: bool,
) -> Result<()> {
    if item.text("state") == "absent" {
        return require(
            raw.is_empty(),
            "source deletion differs from successor product",
        );
    }
    let entries = raw.split(|b| *b == 0).collect::<Vec<_>>();
    require(
        entries.len() == 2 && !entries[0].is_empty(),
        "product source absent or ambiguous",
    )?;
    let at = entries[0]
        .iter()
        .position(|b| *b == b'\t')
        .ok_or_else(|| error("product source metadata missing"))?;
    require(
        &entries[0][at + 1..] == item.text("path").as_bytes(),
        "product source path differs",
    )?;
    let fields = std::str::from_utf8(&entries[0][..at])
        .map_err(|_| error("product metadata not ASCII"))?
        .split_whitespace()
        .collect::<Vec<_>>();
    require(
        fields.len() == 3
            && fields[0] == item.text("mode")
            && fields[if tree { 1 } else { 2 }] == if tree { "blob" } else { "0" },
        "source mode/type differs from successor product",
    )?;
    let oid = fields[if tree { 2 } else { 1 }];
    require(
        json::sha(read(oid)?) == item.text("sha256"),
        "source content differs from successor product",
    )
}
fn commit_product(plan: &Plan<'_>, bundle: &Value, task: &Value) -> Result<Value> {
    let successor = bundle.get("successor");
    let commit = successor.get("commit_sha").str()?;
    let wanted = object([
        ("kind", string("commit")),
        ("sha", string(commit)),
        (
            "receipt_sha256",
            string(json::digest(successor.get("land_receipt_sha256"))?),
        ),
    ]);
    require(
        task.get("landing") == &wanted
            && (!task.get("landed_commit").truth() || task.text("landed_commit") == commit),
        "successor exact terminal commit landing mismatch",
    )?;
    let proof = commit_landing(plan, commit, Some(wanted.text("receipt_sha256")))?;
    require(
        proof.landing == wanted,
        "successor commit receipt differs from existing landing proof",
    )?;
    plan.git().ancestor(bundle.text("base_commit"), commit)?;
    let changed = paths(&plan.git().run(&[
        "diff",
        "--name-only",
        "-z",
        bundle.text("base_commit"),
        commit,
    ])?)?;
    let envelope = bundle.get("source_envelope").strings()?;
    require(
        !changed.is_empty() && changed.iter().all(|p| envelope.contains(&p.as_str())),
        "commit product empty, unrelated or incompletely covered",
    )?;
    for item in bundle.get("source_manifest").array()? {
        let raw = plan
            .git()
            .run(&["ls-tree", "-z", commit, "--", item.text("path")])?;
        validate_product_entry(
            &raw,
            item,
            |oid| plan.git().run(&["cat-file", "blob", oid]),
            true,
        )?;
    }
    let mut product = object([
        ("product_kind", string("commit")),
        ("landing", wanted),
        ("receipt_name", string(proof.name)),
        ("receipt", proof.receipt),
        ("receipt_raw_sha256", string(json::sha(proof.raw))),
    ]);
    if !task.text("artifact").is_empty() {
        let path = legacy_relative(plan, task.text("artifact"))?;
        let artifact = object([
            ("path", string(&path)),
            (
                "sha256",
                string(json::sha(plan.store.read(path, MAX_JSON)?)),
            ),
        ]);
        require(
            bundle.get("requirements").array()?.iter().any(|r| {
                r.get("evidence")
                    .array()
                    .is_ok_and(|e| e.contains(&artifact))
            }),
            "actual committed successor evidence missing from obligation map",
        )?;
        product.set("artifact", artifact)?;
    }
    Ok(product)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::GroupMembership;
    use crate::storage::GroupProofSnapshot;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn receipt_entries_keep_the_bounded_group_readset() {
        static ORDINARY_QUERIES: AtomicUsize = AtomicUsize::new(0);
        fn ordinary(_: u32) -> Result<GroupMembership> {
            ORDINARY_QUERIES.fetch_add(1, Ordering::Relaxed);
            Err(error(
                "ordinary group query must not run in bounded admission",
            ))
        }
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(std::env::temp_dir().join(format!(
            "oms-plan-group-receipts-{}",
            crate::storage::random_hex(16).unwrap()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o770)).unwrap();
        let name = format!("{}-fixture.json", "a".repeat(40));
        std::fs::write(directory.0.join(&name), b"{}").unwrap();
        std::fs::set_permissions(
            directory.0.join(&name),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let gid = std::fs::metadata(&directory.0).unwrap().gid();
        let snapshot = Arc::new(GroupProofSnapshot::new());
        assert!(
            entries(
                &directory.0,
                false,
                None,
                Some(ordinary),
                Some(snapshot.clone())
            )
            .is_err()
        );
        assert_eq!(snapshot.missing().unwrap(), Some(gid));
        assert_eq!(ORDINARY_QUERIES.load(Ordering::Relaxed), 0);
        snapshot
            .insert(
                gid,
                GroupMembership {
                    gid,
                    explicit_member_uids: vec![],
                    primary_member_uids: vec![rustix::process::getuid().as_raw()],
                },
            )
            .unwrap();
        let rows = entries(
            &directory.0,
            false,
            None,
            Some(ordinary),
            Some(snapshot.clone()),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, name);
        assert_eq!(snapshot.proofs().unwrap().len(), 1);
        assert_eq!(ORDINARY_QUERIES.load(Ordering::Relaxed), 0);
        assert!(entries(&directory.0, false, None, Some(ordinary), None).is_err());
        assert_eq!(ORDINARY_QUERIES.load(Ordering::Relaxed), 1);
    }
}
