use crate::engine::{
    assignment, clean_paths, ensure_lineage, lint, new_task, plain, project_snapshot, receipt_id,
    scheduling_metadata, screen, split_list, valid_id, validate_graph,
};
use crate::json::{self, Value, empty, list, number, object, string};
use crate::{Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::BTreeSet;

/// Process-local result of a bounded issue read by the trusted autonomy adapter.
pub struct IssueObservation {
    proposal: String,
    execution: std::path::PathBuf,
    state: std::path::PathBuf,
    observed: std::time::Instant,
    github_id: Value,
    revision: String,
}

fn valid_revision(s: &str) -> bool {
    s.len() == 20
        && s.bytes()
            .zip(b"0000-00-00T00:00:00Z")
            .all(|(actual, expected)| {
                if expected.is_ascii_digit() {
                    actual.is_ascii_digit()
                } else {
                    actual == *expected
                }
            })
        && chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

/// The exact canonical GitHub fields pinned by intent, planning and first admission.
/// Parsing these bytes grants no evidence of a live fetch.
pub fn issue_document(
    raw: &[u8],
    url: &str,
    number: u64,
    require_open: bool,
) -> Result<(Value, String)> {
    let row = json::parse_bounded(raw, 512 * 1024)?;
    let observed_url = row.text("html_url");
    require(
        (!require_open || (row.opt("body").is_some() && row.text("state") == "open"))
            && row.opt("pull_request").is_none()
            && row.get("number").int()? == i128::from(number)
            && u64::try_from(row.get("id").int()?).is_ok_and(|id| id > 0)
            && observed_url.eq_ignore_ascii_case(url)
            && (!require_open
                || observed_url.starts_with("https://github.com/")
                    && observed_url.ends_with(&format!("/issues/{number}")))
            && valid_revision(row.text("updated_at"))
            && !row.text("title").is_empty()
            && row.text("title").chars().count() <= 1024,
        "reviewed GitHub issue closed or identity/revision changed",
    )?;
    let body = if row.get("body") == &Value::Null {
        ""
    } else {
        row.get("body").str()?
    };
    let frozen = object([
        ("id", row.get("id").clone()),
        ("number", row.get("number").clone()),
        ("url", string(url.to_ascii_lowercase())),
        ("updated_at", row.get("updated_at").clone()),
        ("title", row.get("title").clone()),
        ("body", string(body)),
    ]);
    Ok((frozen, observed_url.to_owned()))
}

impl Plan<'_> {
    pub fn observe_issue(
        &self,
        proposal: &[u8],
        fetch: impl FnOnce(&str) -> Result<Vec<u8>>,
    ) -> Result<IssueObservation> {
        self.context.parent()?;
        let p = json::parse_bounded(proposal, 1024 * 1024)?;
        let source = p.get("issue_source");
        self.validate_issue_source(&p, source)?;
        let (slug, n) = p
            .text("issue")
            .split_once('#')
            .ok_or_else(|| error("invalid issue identity"))?;
        let number: u64 = n.parse().map_err(|_| error("invalid issue number"))?;
        let url = format!("https://github.com/{slug}/issues/{number}");
        let raw = fetch(&format!("repos/{slug}/issues/{number}"))?;
        let (frozen, _) = issue_document(&raw, &url, number, true)?;
        require(
            json::hash(&frozen)? == source.text("sha256"),
            "reviewed GitHub issue source changed",
        )?;
        Ok(IssueObservation {
            proposal: json::sha(proposal),
            execution: self.context.execution_repo.clone(),
            state: self.context.state_repo.clone(),
            observed: std::time::Instant::now(),
            github_id: frozen.get("id").clone(),
            revision: frozen.text("updated_at").to_owned(),
        })
    }

    pub fn with_issue_observation(mut self, proof: Option<IssueObservation>) -> Self {
        self.observed_issue = proof;
        self
    }

    fn validate_issue_source(&self, proposal: &Value, source: &Value) -> Result<()> {
        json::keys(source, &["issue", "sha256"], &["with_requirements"])?;
        let issues = crate::engine::Issues::load(self);
        require(
            self.context.canonical()
                && source
                    .opt("with_requirements")
                    .is_none_or(|v| v == &Value::Bool(true))
                && (self.source.repo() == self.store.repo()
                    || self.git().namespace_path()? == self.state_git().namespace_path()?)
                && proposal.get("schema").is_int(2)
                && proposal.text("issue") == source.text("issue")
                && json::hex(source.text("sha256"), 64)
                && issues.required()?
                && issues.canonical(self, source.text("issue"))? == source.text("issue"),
            "invalid reviewed Project issue or repository binding",
        )
    }

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
        if p.opt("discussion").is_some()
            || p.opt("issue_source").is_some()
            || p.get("schema").is_int(2)
        {
            // Typed native lineage has one unambiguous interpretation at both
            // entry points; retain legacy parsing only for legacy proposals.
            json::parse_bounded(&raw, 1024 * 1024)?;
        }
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
            &["issue", "discussion", "issue_source"],
        )?;
        require(
            p.get("schema").is_int(if p.opt("issue_source").is_some() {
                2
            } else {
                1
            }) && p.text("kind") == "agent-plan-proposal",
            "proposal has an unsupported schema/kind",
        )?;
        crate::discussion::validate_proposal(&self.store, &p)?;
        let spec = json::digest(p.get("spec_sha256"))?;
        require(
            crate::repository::commit(p.text("base_sha")),
            "proposal base_sha is invalid",
        )?;
        require(
            p.text("plan_sha256") == req.value("expected-plan-sha256")?,
            "proposal plan digest does not match apply CAS",
        )?;
        let strict = p.opt("issue_source");
        if let Some(source) = strict {
            self.validate_issue_source(&p, source)?;
        }
        let proposal_sha = req.value("expected-proposal-sha256")?;
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
        let issues = crate::engine::Issues::load(self);
        let mut bare = false;
        for row in entries {
            // Match the actual exact reviewed schema, including the legacy
            // parser's rejection of unreviewed forbidden/role additions.
            json::keys(
                row,
                &["id", "title", "allowed", "verify", "depends"],
                &[
                    "assignment",
                    "brief",
                    "covers",
                    "resources",
                    "contracts",
                    "priority",
                    "issue",
                ],
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
            if let Some(brief) = row.opt("brief") {
                let brief = brief.str()?;
                require(
                    !brief.trim().is_empty() && brief.len() <= 65536,
                    "proposal task brief must be nonempty and at most 65536 bytes",
                )?;
                screen(brief.as_bytes())?;
                t.set("brief", string(brief))?;
            }
            if let Some(reference) = p.opt("discussion") {
                t.set("discussion", reference.clone())?;
            }
            if row.opt("resources").is_some()
                || row.opt("contracts").is_some()
                || row.opt("priority").is_some()
            {
                let resources = row
                    .opt("resources")
                    .map(Value::strings)
                    .transpose()?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|v| v.trim().to_owned())
                    .collect::<Vec<_>>();
                let contracts = row
                    .opt("contracts")
                    .map(Value::strings)
                    .transpose()?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|v| v.trim().to_owned())
                    .collect::<Vec<_>>();
                let priority = row
                    .opt("priority")
                    .map(Value::int)
                    .transpose()?
                    .unwrap_or(0);
                let priority = i32::try_from(priority)
                    .map_err(|_| error("proposal priority must be an i32"))?;
                scheduling_metadata(&mut t, &resources, &contracts, priority)?;
            }
            if !cover_ids.is_empty() {
                t.set("covers", covers)?;
            }
            match row.opt("issue") {
                Some(issue) => t.set("issue", string(issues.canonical(self, issue.str()?)?))?,
                None => bare = true,
            }
            seen.push(id.to_owned());
            prepared.push(t);
        }
        if let Some(issue) = p.opt("issue") {
            let issue = issues.canonical(self, issue.str()?)?;
            require(
                prepared.iter().all(|t| t.text("issue") == issue),
                "proposal issue must match every task issue",
            )?;
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
            let intake = s.task(prepared[0].text("id"))?.opt("issue_intake");
            if let Some(source) = strict {
                let record = intake.ok_or_else(|| error("missing accepted issue intake"))?;
                json::keys(
                    record,
                    &["source", "proposal_sha256", "github_id", "revision"],
                    &[],
                )?;
                require(
                    record.get("source") == source
                        && record.text("proposal_sha256") == proposal_sha
                        && record.get("github_id").int()? > 0
                        && valid_revision(record.text("revision")),
                    "existing task differs from exact reviewed issue intake",
                )?;
            } else {
                require(
                    intake.is_none(),
                    "existing task was accepted from a pinned issue",
                )?;
            }
            for row in &prepared {
                let current = s.task(row.text("id"))?;
                require(
                    current.opt("issue_intake") == intake,
                    "existing task has different issue intake",
                )?;
                require(
                    current.get("discussion") == row.get("discussion"),
                    "existing task discussion differs from reviewed proposal",
                )?;
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
                    "resources",
                    "contracts",
                    "priority",
                    "issue",
                    "brief",
                ] {
                    let default = if field == "priority" {
                        number(0)
                    } else if field == "assignment" {
                        empty()
                    } else if ["verify", "role", "issue", "brief"].contains(&field) {
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
                if strict.is_some() {
                    self.git().ancestor(p.text("base_sha"), "HEAD")?;
                }
                return Ok(Response::text(format!(
                    "plan: proposal already applied ({})",
                    seen.join(",")
                )));
            }
        }
        require(
            strict.is_none() || present == 0,
            "existing issue intake lacks its reviewed project contract",
        )?;
        require(
            !bare || !issues.required()?,
            "GitHub project mode: every proposal task requires an issue",
        )?;
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
        if let Some(source) = strict {
            let proof = self.observed_issue.as_ref().ok_or_else(|| {
                error(
                    "first issue intake requires a fresh open-source check through plan-from-spec",
                )
            })?;
            require(
                proof.proposal == proposal_sha
                    && proof.execution == self.context.execution_repo
                    && proof.state == self.context.state_repo
                    && proof.observed.elapsed().as_secs() < 30,
                "issue observation expired or bound to another proposal or repository",
            )?;
            let record = object([
                ("source", source.clone()),
                ("proposal_sha256", string(proposal_sha)),
                ("github_id", proof.github_id.clone()),
                ("revision", string(&proof.revision)),
            ]);
            for task in &mut prepared {
                task.set("issue_intake", record.clone())?;
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
