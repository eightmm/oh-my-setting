use crate::engine::{
    assignment, clean_paths, ensure_lineage, new_task, plain, scheduling_metadata, screen,
    valid_id, validate_graph,
};
use crate::json::{self, Value, empty, number, string};
use crate::{Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::BTreeSet;

const MAX_BATCH: usize = 12;

fn scope_paths(items: &[String], label: &str) -> Result<Vec<String>> {
    for item in items {
        let trimmed = item.trim();
        require(
            !trimmed.is_empty() && !trimmed.starts_with('/') && !trimmed.starts_with('\\'),
            format!("batch {label} path must be a non-empty repo-relative path"),
        )?;
    }
    let cleaned = clean_paths(items)?;
    for path in &cleaned {
        oms_contract_markdown::scope::validate_pattern(path).map_err(|e| error(e.to_string()))?;
    }
    Ok(cleaned)
}

impl Plan<'_> {
    /// `add` for several explicit, reviewed typed rows under one plan lock and
    /// one save. The rows come from a digest-pinned file, never from guessed prose.
    pub(crate) fn add_batch(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        require(
            matches!(s.plan.get("project_contract"), Value::Null),
            "contract-bound plans accept new tasks only through a reviewed proposal",
        )?;
        let path = req
            .path("tasks")
            .ok_or_else(|| error("add-batch requires --tasks"))?;
        let absolute = crate::cli::physical_leaf(&self.context.cwd, path.as_os_str());
        let store = crate::storage::Store::open(
            absolute
                .parent()
                .ok_or_else(|| error("tasks file parent unavailable"))?,
        )?
        .with_group_resolver(self.services.group_membership_resolver());
        let raw = store.read(
            absolute
                .file_name()
                .ok_or_else(|| error("tasks file name unavailable"))?,
            1024 * 1024,
        )?;
        screen(&raw)?;
        let digest = req.value("expected-tasks-sha256")?;
        require(
            json::hex(digest, 64) && json::sha(&raw) == digest,
            "tasks file bytes changed after review",
        )?;
        let expected = req.value("expected-plan-sha256")?;
        require(
            expected == "absent" || json::hex(expected, 64),
            "expected-plan-sha256 must be a 64-hex SHA-256 or absent",
        )?;
        let doc = json::parse_bounded(&raw, 1024 * 1024)?;
        json::keys(&doc, &["schema", "kind", "tasks"], &[])?;
        require(
            doc.get("schema").is_int(1) && doc.text("kind") == "agent-plan-batch",
            "tasks file has an unsupported schema/kind",
        )?;
        let rows = doc.get("tasks").array()?;
        require(
            (1..=MAX_BATCH).contains(&rows.len()),
            format!("tasks file must contain 1..{MAX_BATCH} tasks"),
        )?;
        let mut seen = BTreeSet::new();
        let mut prepared = Vec::new();
        let mut verifies = Vec::new();
        let issues = crate::engine::Issues::load(self);
        let mut bare = false;
        for row in rows {
            json::keys(
                row,
                &["id", "title"],
                &[
                    "depends",
                    "allowed",
                    "forbidden",
                    "verify",
                    "role",
                    "assignment",
                    "resources",
                    "contracts",
                    "priority",
                    "issue",
                    "brief",
                ],
            )?;
            let id = row.get("id").str()?;
            require(
                valid_id(id) && seen.insert(id.to_owned()),
                "batch task ids must be unique valid ids",
            )?;
            let title = row.get("title").str()?;
            require(!title.trim().is_empty(), "batch task has no title")?;
            let text = |name: &str| row.opt(name).map(Value::str).transpose();
            let verify = text("verify")?.unwrap_or("");
            let role = text("role")?.unwrap_or("");
            plain(title, "task title")?;
            plain(verify, "task verify")?;
            plain(role, "task role")?;
            let route = row.opt("assignment").cloned().unwrap_or_else(empty);
            assignment(&route)?;
            let list = |name: &str| -> Result<Vec<String>> {
                Ok(match row.opt(name) {
                    Some(v) => v
                        .strings()?
                        .into_iter()
                        .map(|s| s.trim().to_owned())
                        .collect(),
                    None => vec![],
                })
            };
            let raw_list = |name: &str| -> Result<Vec<String>> {
                Ok(match row.opt(name) {
                    Some(v) => v.strings()?.into_iter().map(str::to_owned).collect(),
                    None => vec![],
                })
            };
            let depends = list("depends")?;
            require(
                depends.iter().all(|d| valid_id(d))
                    && depends.iter().collect::<BTreeSet<_>>().len() == depends.len(),
                "batch dependencies must be unique valid ids",
            )?;
            let allowed = scope_paths(&raw_list("allowed")?, "allowed")?;
            let forbidden = scope_paths(&raw_list("forbidden")?, "forbidden")?;
            let mut task = new_task(
                id,
                title.trim(),
                depends,
                allowed,
                forbidden,
                verify,
                role,
                route,
                &self.context.ts,
            )?;
            if ["resources", "contracts", "priority"]
                .iter()
                .any(|n| row.opt(n).is_some())
            {
                let priority = row
                    .opt("priority")
                    .map(Value::int)
                    .transpose()?
                    .unwrap_or(0);
                scheduling_metadata(
                    &mut task,
                    &list("resources")?,
                    &list("contracts")?,
                    i32::try_from(priority).map_err(|_| error("batch priority must be an i32"))?,
                )?;
            }
            if let Some(brief) = row.opt("brief") {
                let brief = brief.str()?;
                require(
                    !brief.trim().is_empty() && brief.len() <= crate::engine::MAX_BRIEF,
                    "batch task brief must be nonempty and at most 65536 bytes",
                )?;
                // The raw-file screen ran before JSON unescaping.
                screen(brief.as_bytes())?;
                task.set("brief", string(brief))?;
            }
            match row.opt("issue") {
                Some(issue) => task.set("issue", string(issues.canonical(self, issue.str()?)?))?,
                None => bare = true,
            }
            verifies.push(verify.to_owned());
            prepared.push(task);
        }
        let mut graph = s.ordered()?;
        for task in &prepared {
            if let Some(existing) = graph.iter_mut().find(|t| t.text("id") == task.text("id")) {
                *existing = task.clone();
            } else {
                graph.push(task.clone());
            }
        }
        validate_graph(&graph)?;
        let present = prepared
            .iter()
            .filter(|t| s.task(t.text("id")).is_ok())
            .count();
        let ids = seen.iter().cloned().collect::<Vec<_>>().join(",");
        if present > 0 {
            require(
                present == prepared.len(),
                "batch is partially present; refusing non-atomic recovery",
            )?;
            for row in &prepared {
                let current = s.task(row.text("id"))?;
                for field in [
                    "id",
                    "title",
                    "depends",
                    "allowed_paths",
                    "forbidden_paths",
                    "verify",
                    "role",
                    "assignment",
                    "discovered_from",
                    "covers",
                    "resources",
                    "contracts",
                    "priority",
                    "issue",
                    "brief",
                ] {
                    let default = match field {
                        "priority" => number(0),
                        "assignment" => empty(),
                        "verify" | "role" | "discovered_from" | "issue" | "brief" => string(""),
                        _ => Value::Array(vec![]),
                    };
                    require(
                        current.opt(field).unwrap_or(&default)
                            == row.opt(field).unwrap_or(&default),
                        "existing task does not match batch",
                    )?;
                }
            }
            return Ok(Response::text(format!(
                "plan: batch already applied ({ids})"
            )));
        }
        require(
            !bare || !issues.required()?,
            "GitHub project mode: every batch task requires an issue",
        )?;
        match &s.raw {
            Some(raw) => {
                require(
                    expected != "absent" && json::sha(raw) == expected,
                    "plan changed after batch was reviewed",
                )?;
            }
            None => {
                require(expected == "absent", "batch expected an existing plan")?;
                ensure_lineage(&mut s.plan)?;
            }
        }
        let mut warnings = Vec::new();
        for verify in verifies.iter().filter(|v| !v.is_empty()) {
            warnings.extend(
                self.services
                    .verify_selector_warnings(&self.context.execution_repo, verify)?,
            );
        }
        for task in prepared {
            let id = task.text("id").to_owned();
            s.put(&id, task)?;
        }
        self.save(s)?;
        let mut response = Response::text(format!("plan: batch added {ids}"));
        for warning in warnings {
            response.stderr.extend(
                format!(
                    "warning: unknown verify selector: {warning} (fine if this task creates it)\n"
                )
                .as_bytes(),
            );
        }
        Ok(response)
    }
}
