use crate::engine::{
    assignment, clean_paths, ensure_lineage, lint, new_task, plain, project_snapshot, receipt_id,
    screen, split_list, valid_id,
};
use crate::json::{self, Value, empty, list, number, object, string};
use crate::{Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::BTreeSet;

impl Plan<'_> {
    pub(crate) fn proposal(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let path = req
            .path("proposal")
            .ok_or_else(|| error("apply-proposal requires --proposal"))?;
        let absolute = crate::cli::physical_leaf(&self.context.cwd, path.as_os_str());
        let root = crate::storage::Store::open(
            absolute
                .parent()
                .ok_or_else(|| error("proposal parent unavailable"))?,
        )?
        .with_group_resolver(self.services.group_membership_resolver());
        let raw = root.read(
            absolute
                .file_name()
                .ok_or_else(|| error("proposal file name unavailable"))?,
            1024 * 1024,
        )?;
        screen(&raw)?;
        require(
            json::hex(req.value("expected-proposal-sha256")?, 64)
                && json::sha(&raw) == req.value("expected-proposal-sha256")?,
            "proposal bytes changed after review",
        )?;
        let p = json::parse_legacy(&raw, 1024 * 1024)?;
        json::keys(
            &p,
            &[
                "schema",
                "kind",
                "spec_sha256",
                "plan_sha256",
                "base_sha",
                "id_prefix",
                "allowed_envelope",
                "acceptance_files",
                "tasks",
            ],
            &[],
        )?;
        require(
            p.get("schema").is_int(1) && p.text("kind") == "agent-plan-proposal",
            "proposal has an unsupported schema/kind",
        )?;
        let spec = json::digest(p.get("spec_sha256"))?;
        require(
            crate::repository::commit(p.text("base_sha")),
            "proposal base_sha is invalid",
        )?;
        require(
            p.text("plan_sha256") == req.value("expected-plan-sha256")?,
            "proposal plan digest does not match apply CAS",
        )?;
        let prefix = p.get("id_prefix").str()?;
        require(
            prefix.is_empty() || valid_id(prefix),
            "proposal id prefix is invalid",
        )?;
        let envelope = clean_paths(&split_list(req.value("allowed-envelope")?))?;
        require(!envelope.is_empty(), "allowed envelope is empty")?;
        let mut reviewed = clean_paths(
            &p.get("allowed_envelope")
                .strings()?
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )?;
        reviewed.sort();
        reviewed.dedup();
        if reviewed.is_empty() {
            reviewed.push(".".into());
        }
        let mut supplied = envelope.clone();
        supplied.sort();
        supplied.dedup();
        require(
            reviewed == supplied,
            "proposal allowed envelope does not match apply boundary",
        )?;
        let files = p.get("acceptance_files").strings()?;
        require(
            files.len() <= 64 && files.iter().all(|s| s.len() <= 240),
            "proposal acceptance_files must contain at most 64 paths of 240 UTF-8 bytes",
        )?;
        let mut reviewed_files =
            clean_paths(&files.iter().map(|s| s.to_string()).collect::<Vec<_>>())?;
        reviewed_files.sort();
        reviewed_files.dedup();
        require(
            files
                .iter()
                .copied()
                .eq(reviewed_files.iter().map(String::as_str)),
            "proposal acceptance_files must be sorted and unique",
        )?;
        let mut supplied_files = clean_paths(
            &req.text("accept-files")?
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )?;
        supplied_files.sort();
        supplied_files.dedup();
        require(
            supplied_files == reviewed_files,
            "proposal acceptance_files do not match --accept-files",
        )?;
        let mut manifest = Vec::new();
        for name in &reviewed_files {
            require(name != ".", "acceptance file must name a regular file")?;
            manifest.push(object([
                ("path", string(name)),
                (
                    "sha256",
                    string(json::sha(self.source.read(name, 8 * 1024 * 1024)?)),
                ),
            ]));
        }
        let project = project_snapshot(&self.source)?;
        require(
            matches!(project.text("state"), "confirmed" | "legacy-active")
                && project.text("sha256") == spec,
            "PROJECT.md changed or is not confirmed after proposal review",
        )?;
        let entries = p.get("tasks").array()?;
        let max = req.text("max-tasks")?.parse::<usize>().unwrap_or(12);
        require(
            (1..=12).contains(&max) && !entries.is_empty() && entries.len() <= max,
            format!("proposal tasks must contain 1..{max} entries"),
        )?;
        let mut seen = Vec::new();
        let mut prepared = Vec::new();
        for row in entries {
            // Match the actual exact reviewed schema, including the legacy
            // parser's rejection of unreviewed forbidden/role additions.
            json::keys(
                row,
                &["id", "title", "allowed", "verify", "depends"],
                &["assignment", "covers"],
            )?;
            let route = row.opt("assignment").cloned().unwrap_or_else(empty);
            assignment(&route)?;
            let id = row.get("id").str()?;
            require(
                valid_id(id) && !seen.contains(&id.to_owned()),
                "proposal task ids must be unique valid ids",
            )?;
            require(
                prefix.is_empty() || id.starts_with(prefix),
                "proposal task does not match id prefix",
            )?;
            let title = row.get("title").str()?;
            require(!title.trim().is_empty(), "proposal task has no title")?;
            plain(title, "proposal task title")?;
            let verify = row.get("verify").str()?;
            require(
                !verify.trim().is_empty(),
                "proposal task has no verify command",
            )?;
            plain(verify, "proposal task verify")?;
            let allowed = clean_paths(
                &row.get("allowed")
                    .strings()?
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
            )?;
            require(!allowed.is_empty(), "proposal task has no allowed paths")?;
            for candidate in &allowed {
                require(
                    oms_contract_markdown::scope::within_envelope(
                        candidate,
                        &envelope.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                    .map_err(|e| error(e.to_string()))?,
                    "proposal task widens allowed path envelope",
                )?;
            }
            lint(verify, &allowed)?;
            let depends = row.get("depends").strings()?;
            let unique = depends.iter().copied().collect::<BTreeSet<_>>();
            require(
                unique.len() == depends.len() && depends.iter().all(|s| valid_id(s)),
                "proposal dependencies must be unique ids",
            )?;
            for dep in &depends {
                if !seen.iter().any(|s| s == dep) {
                    require(
                        s.task(dep).is_ok_and(|t| s.verified_done(t)),
                        "proposal depends on unknown/later or unfinished existing task",
                    )?;
                }
            }
            let covers = row.opt("covers").cloned().unwrap_or(Value::Array(vec![]));
            let cover_ids = covers.strings()?;
            require(
                (row.opt("covers").is_none() || !cover_ids.is_empty())
                    && cover_ids.len() <= 15
                    && cover_ids.iter().all(|s| receipt_id(s, 160))
                    && cover_ids.iter().copied().collect::<BTreeSet<_>>().len() == cover_ids.len(),
                "proposal covers must be 1..15 unique criterion ids",
            )?;
            let mut t = new_task(
                id,
                title.trim(),
                depends.iter().map(|s| s.to_string()).collect(),
                allowed,
                vec![],
                verify,
                "",
                route,
                &self.context.ts,
            )?;
            if !cover_ids.is_empty() {
                t.set("covers", covers)?;
            }
            seen.push(id.to_owned());
            prepared.push(t);
        }
        let contract = object([
            ("schema", number(1)),
            ("spec_sha256", string(spec)),
            (
                "allowed_envelope",
                list(reviewed.iter().map(String::as_str)),
            ),
            (
                "acceptance_files",
                list(reviewed_files.iter().map(String::as_str)),
            ),
            ("acceptance_manifest", Value::Array(manifest)),
        ]);
        let existing = s.plan.get("project_contract");
        let bound = !matches!(existing, Value::Null);
        if bound {
            for field in [
                "schema",
                "spec_sha256",
                "allowed_envelope",
                "acceptance_files",
                "acceptance_manifest",
            ] {
                let default = Value::Array(vec![]);
                require(
                    existing.opt(field).unwrap_or(&default) == contract.get(field),
                    "existing plan project contract does not match reviewed proposal",
                )?;
            }
        }
        let present = prepared
            .iter()
            .filter(|t| s.task(t.text("id")).is_ok())
            .count();
        if present > 0 {
            require(
                present == prepared.len(),
                "proposal is partially present; refusing non-atomic recovery",
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
                    "covers",
                ] {
                    let default = if field == "assignment" {
                        empty()
                    } else if ["verify", "role"].contains(&field) {
                        string("")
                    } else {
                        Value::Array(vec![])
                    };
                    require(
                        current.opt(field).unwrap_or(&default)
                            == row.opt(field).unwrap_or(&default),
                        "existing task does not match reviewed proposal",
                    )?;
                }
            }
            for name in ["goal", "accept"] {
                require(
                    req.text(name)?.is_empty() || s.plan.text(name) == req.text(name)?,
                    "existing plan goal/acceptance does not match proposal replay",
                )?;
            }
            if bound {
                return Ok(Response::text(format!(
                    "plan: proposal already applied ({})",
                    seen.join(",")
                )));
            }
        }
        require(
            self.git().text(&["rev-parse", "HEAD"])? == p.text("base_sha"),
            "repository HEAD changed after proposal review",
        )?;
        let expected = req.value("expected-plan-sha256")?;
        if let Some(raw) = &s.raw {
            require(
                json::hex(expected, 64) && json::sha(raw) == expected,
                "plan changed after proposal review",
            )?;
            for name in ["goal", "accept"] {
                require(
                    req.text(name)?.is_empty() || s.plan.text(name) == req.text(name)?,
                    "existing plan goal/acceptance changed after proposal review",
                )?;
            }
        } else {
            require(expected == "absent", "proposal expected an existing plan")?;
            let goal = req.value("goal")?;
            let accept = req.value("accept")?;
            s.plan = object([
                ("schema", number(3)),
                (
                    "plan_id",
                    string(format!("plan_{}", crate::storage::random_hex(16)?)),
                ),
                ("goal", string(goal)),
                ("accept", string(accept)),
                ("project_contract", contract.clone()),
                ("tasks", empty()),
            ]);
        }
        if !bound {
            for (_, task) in s.tasks()? {
                require(
                    seen.iter().any(|s| s == task.text("id")),
                    "legacy plan has tasks absent from reviewed proposal",
                )?;
                let allowed = task.get("allowed_paths").strings()?;
                require(
                    !allowed.is_empty(),
                    "existing task is outside reviewed project contract",
                )?;
                for candidate in allowed {
                    require(
                        oms_contract_markdown::scope::within_envelope(
                            candidate,
                            &envelope.iter().map(String::as_str).collect::<Vec<_>>(),
                        )
                        .map_err(|e| error(e.to_string()))?,
                        "existing task is outside reviewed project contract",
                    )?;
                }
            }
            s.plan.set("project_contract", contract)?;
            ensure_lineage(&mut s.plan)?;
            if present == prepared.len() {
                self.save(s)?;
                return Ok(Response::text(format!(
                    "plan: proposal already applied ({})",
                    seen.join(",")
                )));
            }
        }
        for task in prepared {
            let id = task.text("id").to_owned();
            s.put(&id, task)?;
        }
        self.save(s)?;
        Ok(Response::text(format!(
            "plan: proposal applied {}",
            seen.join(",")
        )))
    }
}
