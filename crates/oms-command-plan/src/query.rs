use crate::engine::{STATES, age, parse_ts, task_ttl};
use crate::json::{self, Value, empty, list, number, object, string};
use crate::{Action, Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::{BTreeMap, BTreeSet};

impl Plan<'_> {
    pub(crate) fn view(&self, s: &mut Snapshot, t: &Value, details: bool) -> Result<Value> {
        let mut v = t.clone();
        let stale = crate::contracts::stale(&s.registry, t)?;
        v.set("contract_stale", Value::Bool(!stale.is_empty()))?;
        v.set("stale_contracts", list(stale.iter().map(String::as_str)))?;
        if s.unverified.contains(t.text("id")) {
            v.set("state", string("blocked"))?;
            v.set("stored_state", t.get("state").clone())?;
            v.set("completion_verified", Value::Bool(false))?;
            v.set("reason",string("research completion not verified: evicted artifact unavailable or digest changed"))?;
        }
        if details {
            v.set("task_sha256", string(json::hash(t)?))?;
            v.set("claim_expired", Value::Bool(self.expired(s, t, false)?))?;
            if ["claimed", "running", "review"].contains(&t.text("state"))
                && let Some(age) = age(t, &self.context.ts)
            {
                v.set("claim_age_s", number(age))?;
            }
        }
        Ok(v)
    }
    /// Cached status for a bound task from the local snapshot cache only;
    /// nothing here contacts GitHub. Unbound tasks have none.
    pub(crate) fn issue_cache(&self, s: &mut Snapshot, t: &Value) -> Option<Value> {
        let issue = t.text("issue");
        if issue.is_empty() {
            return None;
        }
        let cache = s.github_cache.get_or_insert_with(|| {
            self.store
                .optional(oms_github::freshness::CACHE_REL, 4 * 1024 * 1024)
                .ok()
                .flatten()
                .unwrap_or_default()
        });
        let now = parse_ts(&self.context.ts).unwrap_or(0) as i64;
        let (cached, freshness) = match oms_github::freshness::lookup(cache, issue, now) {
            // A pull row never reports as issue state.
            (Some(c), _) if c.kind != "issue" => {
                (None, oms_github::freshness::Freshness::Unavailable)
            }
            found => found,
        };
        let freshness = match freshness {
            oms_github::freshness::Freshness::Fresh => "fresh",
            oms_github::freshness::Freshness::Stale => "stale",
            oms_github::freshness::Freshness::Unavailable => "unavailable",
        };
        Some(match cached {
            Some(c) => object([
                ("state", string(c.state)),
                ("freshness", string(freshness)),
                ("captured_at", number(c.captured_at)),
                ("title", string(c.title)),
            ]),
            None => object([
                ("state", Value::Null),
                ("freshness", string(freshness)),
                ("captured_at", Value::Null),
                ("title", Value::Null),
            ]),
        })
    }
    /// Human token for a bound task; the remote title stays JSON-only.
    fn issue_note(&self, s: &mut Snapshot, t: &Value) -> Option<String> {
        let cache = self.issue_cache(s, t)?;
        let state = match cache.get("state") {
            Value::Null => "unknown",
            v => v.str().unwrap_or("unknown"),
        };
        Some(format!(
            "{} ({state}, {})",
            t.text("issue"),
            cache.text("freshness")
        ))
    }
    fn expiry_note(&self, t: &Value) -> String {
        format!(
            "claim EXPIRED (age {}s >= ttl {}s, was @{})",
            age(t, &self.context.ts).unwrap_or(0),
            task_ttl(t, self.context.claim_ttl),
            if t.text("provider").is_empty() {
                "?"
            } else {
                t.text("provider")
            }
        )
    }
    fn brief(&self, s: &mut Snapshot, t: &Value) -> Result<String> {
        let v = self.view(s, t, false)?;
        let mut state = v.text("state").to_owned();
        if self.expired(s, t, false)? {
            state.push_str(&format!(" [{}; claimable]", self.expiry_note(t)));
        }
        let mut lines = vec![
            format!("# Task {}: {}", t.text("id"), t.text("title")),
            format!("state: {state}"),
        ];
        if v.get("completion_verified") == &Value::Bool(false) {
            lines.push("completion: not verified (stored state: done)".into());
        }
        let joined = |key: &str| -> Result<String> { Ok(t.get(key).strings()?.join(", ")) };
        let depends = joined("depends")?;
        lines.push(format!(
            "depends: {}",
            if depends.is_empty() {
                "(none)"
            } else {
                &depends
            }
        ));
        let allowed = joined("allowed_paths")?;
        lines.push(format!(
            "allowed_paths: {}",
            if allowed.is_empty() {
                "(unrestricted)"
            } else {
                &allowed
            }
        ));
        if t.get("forbidden_paths").truth() {
            lines.push(format!("forbidden_paths: {}", joined("forbidden_paths")?));
        }
        lines.push(format!(
            "verify: {}",
            if t.text("verify").is_empty() {
                "(none)"
            } else {
                t.text("verify")
            }
        ));
        if t.get("assignment").truth() {
            lines.push(format!(
                "assignment: {}",
                String::from_utf8(json::default_bytes(t.get("assignment"), true)?)
                    .map_err(|_| error("assignment encoding unavailable"))?
            ));
        }
        if let Some(note) = self.issue_note(s, t) {
            lines.push(format!("issue: {note}"));
        }
        for name in ["covers", "discovered_from", "role"] {
            if t.get(name).truth() {
                lines.push(format!(
                    "{name}: {}",
                    if name == "covers" {
                        t.get(name).strings()?.join(", ")
                    } else {
                        t.text(name).into()
                    }
                ));
            }
        }
        Ok(lines.join("\n"))
    }
    pub(crate) fn query(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        use Action::*;
        match req.action {
            Show | EvidenceSnapshot => {
                let t = s.task(req.value("id")?)?.clone();
                let mut v = self.view(s, &t, false)?;
                if req.action == EvidenceSnapshot {
                    self.verify_review_evidence(&t)?;
                }
                if matches!(s.plan.get("project_contract"), Value::Object(_)) {
                    v.set("project_contract", s.plan.get("project_contract").clone())?;
                }
                if req.action == EvidenceSnapshot {
                    v.set(
                        "plan_id",
                        s.plan.opt("plan_id").cloned().unwrap_or(string("")),
                    )?;
                }
                v.set("claim_expired", Value::Bool(self.expired(s, &t, false)?))?;
                v.set("task_sha256", string(json::hash(&t)?))?;
                if req.action == Show
                    && let Some(cache) = self.issue_cache(s, &t)
                {
                    v.set("issue_cache", cache)?;
                }
                if matches!(t.text("state"), "claimed" | "running" | "review")
                    && let Some(age) = age(&t, &self.context.ts)
                {
                    v.set("claim_age_s", number(age))?;
                }
                Response::json(&v)
            }
            Brief => {
                let t = s.task(req.value("id")?)?.clone();
                Ok(Response::text(self.brief(s, &t)?))
            }
            Next => self.next(req, s),
            Ready => self.ready(req, s),
            List => self.list(req, s),
            Discovered => self.discovered(req, s),
            Status => self.status(req, s),
            Retro => self.retro(req, s),
            _ => Err(error("not a read action")),
        }
    }
    fn scheduled(
        &self,
        req: &Request,
        s: &mut Snapshot,
    ) -> Result<(Vec<Value>, bool, bool, usize)> {
        let tasks = s.ordered()?;
        let mut active = Vec::new();
        let mut review = 0;
        for task in &tasks {
            if task.text("state") == "review" {
                review += 1;
            }
            if task.text("state") == "landing"
                || (matches!(task.text("state"), "claimed" | "running")
                    && !self.expired(s, task, false)?)
            {
                active.push(task.clone());
            }
        }
        let capacity = req.has("max-writers")
            && active.len() as u64
                >= req
                    .value("max-writers")?
                    .parse::<u64>()
                    .map_err(|_| error("invalid max-writers"))?;
        let backpressure = req.has("max-review-backlog")
            && review as u64
                >= req
                    .value("max-review-backlog")?
                    .parse::<u64>()
                    .map_err(|_| error("invalid max-review-backlog"))?;
        let mut eligible = Vec::new();
        if !capacity && !backpressure {
            for task in tasks {
                if !self.actionable(s, &task)? {
                    continue;
                }
                let paths = task.get("allowed_paths").strings()?;
                let resources = task
                    .opt("resources")
                    .map(Value::strings)
                    .transpose()?
                    .unwrap_or_default();
                let mut conflict = false;
                for writer in &active {
                    let writer_paths = writer.get("allowed_paths").strings()?;
                    let writer_resources = writer
                        .opt("resources")
                        .map(Value::strings)
                        .transpose()?
                        .unwrap_or_default();
                    if oms_contract_markdown::scope::scopes_overlap(
                        Some(&paths),
                        Some(&writer_paths),
                    ) || resources.iter().any(|r| writer_resources.contains(r))
                    {
                        conflict = true;
                        break;
                    }
                }
                if !conflict {
                    eligible.push(task);
                }
            }
        }
        let now = parse_ts(&self.context.ts).unwrap_or(0);
        let age = |t: &Value| (now - parse_ts(t.text("created")).unwrap_or(now)).max(0);
        let score = |t: &Value| -> i128 {
            let p = t
                .opt("priority")
                .and_then(|v| v.int().ok())
                .unwrap_or(0)
                .clamp(-100, 100);
            let boost = (age(t) / 3600).min(200);
            p + boost
        };
        let mut ranked: Vec<(usize, Value)> = eligible.into_iter().enumerate().collect();
        ranked.sort_by(|(ia, a), (ib, b)| {
            score(b)
                .cmp(&score(a))
                .then_with(|| age(b).cmp(&age(a)))
                .then_with(|| ia.cmp(ib))
        });
        let eligible: Vec<Value> = ranked.into_iter().map(|(_, t)| t).collect();
        Ok((eligible, capacity, backpressure, review))
    }
    pub(crate) fn next(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let (eligible, capacity, backpressure, _) = self.scheduled(req, s)?;
        if capacity || backpressure {
            return Err(crate::drift(if backpressure {
                "review backlog backpressure"
            } else {
                "capacity_full"
            }));
        }
        let selected = eligible.into_iter().next();
        let Some(mut t) = selected else {
            let mut response = Response {
                code: 3,
                ..Default::default()
            };
            if s.contract.get("satisfied") != &Value::Bool(true) {
                response.stderr.extend(
                    format!(
                        "plan: PROJECT.md contract blocks new task authority: {}\n",
                        s.contract.text("blocker")
                    )
                    .as_bytes(),
                );
            }
            response
                .stderr
                .extend_from_slice(b"plan: no actionable task\n");
            return Ok(response);
        };
        let expired = self.expired(s, &t, false)?;
        let note = if expired {
            format!(
                "plan: {} {}; {}\n",
                t.text("id"),
                self.expiry_note(&t),
                if req.has("claim") {
                    "re-claiming under a new lease"
                } else {
                    "offered as claimable"
                }
            )
        } else {
            String::new()
        };
        if req.has("claim") {
            self.claim(req, &mut t, false)?;
            t.set("updated", string(&self.context.ts))?;
            s.put(t.text("id"), t.clone())?;
            self.save(s)?;
        }
        let mut r = if req.has("json") {
            let mut view = self.view(s, &t, true)?;
            view.set("plan_id", s.plan.get("plan_id").clone())?;
            Response::json(&view)?
        } else {
            Response::text(self.brief(s, &t)?)
        };
        r.stderr.extend(note.as_bytes());
        Ok(r)
    }
    fn ready(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let mut rows = Vec::new();
        let mut stderr = Vec::new();
        let (eligible, capacity, backpressure, review) = self.scheduled(req, s)?;
        for t in eligible {
            let expired = self.expired(s, &t, false)?;
            if expired {
                stderr.extend(
                    format!(
                        "plan: {} {}; counted as ready\n",
                        t.text("id"),
                        self.expiry_note(&t)
                    )
                    .as_bytes(),
                );
            }
            let deps = t.get("depends").strings()?;
            let why = format!(
                "{}{}",
                if expired {
                    "claim expired; "
                } else {
                    "state ready; "
                },
                if deps.is_empty() {
                    "no dependencies".to_owned()
                } else {
                    format!("dependencies done: {}", deps.join(", "))
                }
            );
            rows.push((t, why));
        }
        let total = rows.len();
        if req.has("limit") {
            rows.truncate(
                req.value("limit")?
                    .parse()
                    .map_err(|_| error("--limit must be a positive integer"))?,
            );
        }
        let mut response = if req.has("json") {
            let mut items = Vec::new();
            for (t, why) in rows {
                let mut v = object([
                    ("id", t.get("id").clone()),
                    ("title", t.get("title").clone()),
                    ("allowed", t.get("allowed_paths").clone()),
                    ("verify", t.get("verify").clone()),
                    ("depends", t.get("depends").clone()),
                    ("why_ready", string(why)),
                    ("role", string(t.text("role"))),
                    (
                        "resources",
                        t.opt("resources")
                            .cloned()
                            .unwrap_or_else(|| Value::Array(Vec::new())),
                    ),
                ]);
                if t.get("discovered_from").truth() {
                    v.set("discovered_from", t.get("discovered_from").clone())?;
                }
                items.push(v);
            }
            Response::json(&object([
                ("capacity_full", Value::Bool(capacity)),
                ("backpressure", Value::Bool(backpressure)),
                ("review_backlog", number(review)),
                ("schema", number(1)),
                (
                    "plan_id",
                    s.plan.opt("plan_id").cloned().unwrap_or(string("")),
                ),
                ("total", number(total)),
                ("tasks", Value::Array(items)),
            ]))?
        } else {
            Response {
                stdout: rows
                    .iter()
                    .map(|(t, _)| format!("{}\n", t.text("id")))
                    .collect::<String>()
                    .into_bytes(),
                ..Default::default()
            }
        };
        response.stderr = stderr;
        Ok(response)
    }
    fn list(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let filter = req.text("state")?;
        require(
            filter.is_empty() || STATES.contains(&filter),
            format!("unknown --state: {filter}"),
        )?;
        let mut views = Vec::new();
        let mut lines = Vec::new();
        for t in s.ordered()? {
            let mut view = self.view(s, &t, true)?;
            if !filter.is_empty() && view.text("state") != filter {
                continue;
            }
            if req.has("json") {
                if let Some(cache) = self.issue_cache(s, &t) {
                    view.set("issue_cache", cache)?;
                }
                views.push(view);
                continue;
            }
            let issue = self
                .issue_note(s, &t)
                .map(|note| format!(" issue {note}"))
                .unwrap_or_default();
            let deps = t.get("depends").strings()?;
            let dep = if deps.is_empty() {
                String::new()
            } else {
                format!(" depends={}", deps.join(","))
            };
            let provider = if t.text("provider").is_empty() {
                String::new()
            } else {
                format!(" @{}", t.text("provider"))
            };
            let tag = if view.get("completion_verified") == &Value::Bool(false) {
                " NOT VERIFIED".into()
            } else if t.text("state") == "review"
                && age(&t, &self.context.ts).is_some_and(|n| n >= 86400)
            {
                format!(
                    " STALE(review {}h old)",
                    age(&t, &self.context.ts).unwrap() / 3600
                )
            } else if self.expired(s, &t, false)? {
                format!(" EXPIRED({})", self.expiry_note(&t))
            } else {
                String::new()
            };
            lines.push(format!(
                "{:<10} {:<9} {}{provider}{dep}{issue}{tag}",
                t.text("id"),
                view.text("state"),
                t.text("title")
            ));
        }
        if req.has("json") {
            Response::json(&object([
                ("schema", number(1)),
                (
                    "plan_id",
                    s.plan.opt("plan_id").cloned().unwrap_or(string("")),
                ),
                ("tasks", Value::Array(views)),
            ]))
        } else {
            Ok(Response {
                stdout: if lines.is_empty() {
                    vec![]
                } else {
                    format!("{}\n", lines.join("\n")).into_bytes()
                },
                ..Default::default()
            })
        }
    }
    fn discovered(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let groups = groups(s)?;
        let mut parents = Vec::new();
        let mut count = 0;
        for (id, kids) in groups {
            let parent = s.task(&id).ok().cloned();
            let state = if let Some(t) = parent {
                self.view(s, &t, false)?.text("state").to_owned()
            } else {
                "missing".into()
            };
            let mut items = Vec::new();
            for t in kids {
                count += 1;
                items.push(object([
                    ("id", t.get("id").clone()),
                    ("title", t.get("title").clone()),
                    ("state", self.view(s, &t, false)?.get("state").clone()),
                ]));
            }
            parents.push(object([
                ("id", string(id)),
                ("state", string(state)),
                ("discovered", Value::Array(items)),
            ]));
        }
        if req.has("json") {
            return Response::json(&object([
                ("schema", number(1)),
                (
                    "plan_id",
                    s.plan.opt("plan_id").cloned().unwrap_or(string("")),
                ),
                ("count", number(count)),
                ("parents", Value::Array(parents)),
            ]));
        }
        if parents.is_empty() {
            return Ok(Response::text("discovered: (none)"));
        }
        let mut lines = Vec::new();
        for p in parents {
            lines.push(format!("{} ({}):", p.text("id"), p.text("state")));
            for k in p.get("discovered").array()? {
                lines.push(format!(
                    "  {:<10} {:<9} {}",
                    k.text("id"),
                    k.text("state"),
                    k.text("title")
                ));
            }
        }
        Ok(Response::text(lines.join("\n")))
    }
    pub(crate) fn status_value(&self, s: &mut Snapshot) -> Result<Value> {
        let tasks = s.ordered()?;
        let count = tasks.len();
        let mut by = BTreeMap::<String, usize>::new();
        let mut actionable = Vec::new();
        let mut stale = Vec::new();
        let mut reviews = Vec::new();
        let mut open = Vec::new();
        let mut contract_stale = Vec::new();
        for t in &tasks {
            let view = self.view(s, t, false)?;
            if view.get("contract_stale").truth() {
                contract_stale.push(t.text("id").to_owned());
            }
            *by.entry(view.text("state").into()).or_default() += 1;
            if self.actionable(s, t)? {
                actionable.push(t.text("id").to_owned());
            }
            if !["done", "cancelled"].contains(&view.text("state")) {
                open.push(t.clone());
            }
            if self.expired(s, t, false)? {
                stale.push(object([
                    ("id", t.get("id").clone()),
                    ("provider", t.get("provider").clone()),
                    ("age_seconds", number(age(t, &self.context.ts).unwrap_or(0))),
                    ("ttl_seconds", number(task_ttl(t, self.context.claim_ttl))),
                ]));
            }
            if t.text("state") == "review" && age(t, &self.context.ts).is_some_and(|n| n >= 86400) {
                reviews.push(object([
                    ("id", t.get("id").clone()),
                    ("provider", t.get("provider").clone()),
                    ("age_seconds", number(age(t, &self.context.ts).unwrap_or(0))),
                    ("ttl_seconds", number(86400)),
                ]));
            }
        }
        let verified = *by.get("done").unwrap_or(&0);
        let cancelled = *by.get("cancelled").unwrap_or(&0);
        let closed = verified + cancelled;
        let unfinished = count - closed;
        let states = Value::Object(
            by.into_iter()
                .map(|(k, v)| (k.into_bytes(), number(v)))
                .collect(),
        );
        let mut waves = BTreeMap::<String, u64>::new();
        let mut placed = Vec::<(&Value, u64)>::new();
        for task in &open {
            let mut wave = task
                .get("depends")
                .strings()?
                .iter()
                .filter_map(|d| waves.get(*d))
                .max()
                .copied()
                .unwrap_or(0)
                + 1;
            while placed
                .iter()
                .any(|(other, n)| *n == wave && wave_overlap(task, other))
            {
                wave += 1;
            }
            waves.insert(task.text("id").into(), wave);
            placed.push((task, wave));
        }
        let max = waves.values().max().copied().unwrap_or(0);
        let wave_tasks = Value::Object(
            waves
                .into_iter()
                .map(|(k, v)| (k.into_bytes(), number(v)))
                .collect(),
        );
        let mut v = object([
            ("schema", number(1)),
            ("present", Value::Bool(s.raw.is_some())),
            (
                "plan_sha256",
                string(s.raw.as_ref().map(json::sha).unwrap_or_default()),
            ),
            ("task_count", number(count)),
            ("nonempty", Value::Bool(count > 0)),
            (
                "all_done",
                Value::Bool(count > 0 && tasks.iter().all(|t| s.verified_done(t))),
            ),
            ("all_closed", Value::Bool(count > 0 && unfinished == 0)),
            ("has_unfinished", Value::Bool(unfinished > 0)),
            ("verified_count", number(verified)),
            ("cancelled_count", number(cancelled)),
            ("closed_count", number(closed)),
            ("unfinished_count", number(unfinished)),
            ("by_state", states),
            ("actionable", list(actionable.iter().map(String::as_str))),
            ("contract", s.contract.clone()),
            ("stale", Value::Array(stale)),
            ("stale_review", Value::Array(reviews)),
            (
                "contract_stale_tasks",
                list(contract_stale.iter().map(String::as_str)),
            ),
            (
                "contract_count",
                number(s.registry.get("contracts").object().map_or(0, <[_]>::len)),
            ),
            (
                "waves",
                object([("count", number(max)), ("tasks", wave_tasks)]),
            ),
        ]);
        let groups = groups(s)?;
        if !groups.is_empty() {
            let mut found = empty();
            for (id, kids) in groups {
                found.set(&id, list(kids.iter().map(|t| t.text("id"))))?;
            }
            v.set("discovered", found)?;
        }
        Ok(v)
    }
    fn status(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let v = self.status_value(s)?;
        if req.has("json") {
            return Response::json_line(&v, false, false);
        }
        let mut lines = Vec::new();
        if s.plan.get("goal").truth() {
            lines.push(format!("goal: {}", s.plan.text("goal")));
        }
        let contract = s.plan.get("project_contract");
        if !contract.text("spec_sha256").is_empty() {
            lines.push(format!(
                "contract: PROJECT.md {} scope={}",
                &contract.text("spec_sha256")[..12],
                contract.get("allowed_envelope").strings()?.join(",")
            ));
        }
        if !s.plan.text("accept").is_empty() {
            lines.push(format!("accept: {}", s.plan.text("accept")));
            let digest = json::sha(s.plan.text("accept"));
            let (rows, _) = retro_rows(self)?;
            let last = rows
                .iter()
                .rev()
                .find(|r| r.text("kind") == "acceptance" && r.text("accept_sha256_full") == digest);
            if let Some(r) = last {
                lines.push(format!(
                    "acceptance: {} at {} (base {})",
                    r.text("status"),
                    r.text("ts"),
                    r.text("base_sha").chars().take(12).collect::<String>()
                ));
            } else {
                lines.push("acceptance: not run for this command".into());
            }
        }
        let states = v.get("by_state");
        let counts = STATES
            .iter()
            .filter_map(|name| {
                states
                    .opt(name)
                    .map(|n| format!("{name}={}", n.int().unwrap_or(0)))
            })
            .collect::<Vec<_>>();
        lines.push(format!(
            "tasks: {}  [{}]",
            v.get("task_count").int()?,
            counts.join(" ")
        ));
        lines.push(format!(
            "closed: {}/{}  verified: {}  cancelled: {}  unfinished: {}",
            v.get("closed_count").int()?,
            v.get("task_count").int()?,
            v.get("verified_count").int()?,
            v.get("cancelled_count").int()?,
            v.get("unfinished_count").int()?
        ));
        let ready = v.get("actionable").strings()?;
        lines.push(format!(
            "ready now: {}",
            if ready.is_empty() {
                "(none)".into()
            } else {
                ready.join(" ")
            }
        ));
        if !s.contract.text("blocker").is_empty() {
            lines.push(format!("contract blocker: {}", s.contract.text("blocker")));
        }
        for t in s.ordered()? {
            if self.expired(s, &t, false)? {
                lines.push(format!(
                    "expired claim {}: {}",
                    t.text("id"),
                    self.expiry_note(&t)
                ));
            }
            let view = self.view(s, &t, false)?;
            if view.text("state") == "blocked" {
                lines.push(format!("blocked {}: {}", t.text("id"), view.text("reason")));
            }
        }
        for r in v.get("stale_review").array()? {
            lines.push(format!(
                "stale review {}: {}h without a verdict (oms agent-plan reclaim --include-review)",
                r.text("id"),
                r.get("age_seconds").int()? / 3600
            ));
        }
        let waiting = s
            .ordered()?
            .into_iter()
            .filter(|t| t.text("state") == "ready" && !s.deps_done(t))
            .map(|t| t.text("id").to_owned())
            .collect::<Vec<_>>();
        if !waiting.is_empty() {
            lines.push(format!("waiting on deps: {}", waiting.join(" ")));
        }
        let found = groups(s)?;
        if !found.is_empty() {
            lines.push(format!(
                "discovered: {}",
                found
                    .iter()
                    .map(|(id, k)| format!(
                        "{id} <- {}",
                        k.iter().map(|t| t.text("id")).collect::<Vec<_>>().join(",")
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(Response::text(lines.join("\n")))
    }
    fn retro(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        require(
            s.raw.is_some(),
            "no readable plan at .oms/plan/tasks.json; a retired plan has no retro (run it before agent-plan retire)",
        )?;
        let (rows, complete) = retro_rows(self)?;
        let tasks = s.ordered()?;
        let ids = tasks.iter().map(|t| t.text("id")).collect::<BTreeSet<_>>();
        let mut cycles = BTreeMap::<String, u64>::new();
        let mut runs = BTreeSet::new();
        for r in &rows {
            if matches!(r.text("kind"), "goal-cycle" | "autopilot-cycle")
                && ids.contains(r.text("task_id"))
            {
                *cycles.entry(r.text("task_id").into()).or_default() += 1;
                if r.get("run_id").str().is_ok() {
                    runs.insert(r.text("run_id").to_owned());
                }
            }
        }
        let mut states = empty();
        let mut retried = Vec::new();
        let mut blocked = Vec::new();
        let mut cancelled = Vec::new();
        for t in &tasks {
            let state = clean(t.text("state"), 24);
            let n = states.get(&state).int().unwrap_or(0);
            states.set(&state, number(n + 1))?;
            let history = t.get("history").array().unwrap_or(&[]);
            let amended = history
                .iter()
                .filter(|r| r.text("kind") == "amended")
                .count();
            let reopened = history
                .iter()
                .filter(|r| r.text("kind") == "completion-reactivated")
                .count();
            let repairs = t.get("repair_count").int().unwrap_or(0).max(0);
            let spent = cycles.get(t.text("id")).copied().unwrap_or(0);
            if repairs > 0 || amended > 0 || reopened > 0 || spent > 1 {
                retried.push(object([
                    ("id", t.get("id").clone()),
                    ("title", string(clean(t.text("title"), 80))),
                    ("cycles", number(spent)),
                    ("repairs", number(repairs)),
                    ("amended", number(amended)),
                    ("reopened", number(reopened)),
                ]));
            }
            let held = object([
                ("id", t.get("id").clone()),
                ("title", string(clean(t.text("title"), 80))),
                ("reason", string(clean(t.text("reason"), 160))),
            ]);
            if state == "blocked" {
                blocked.push(held);
            } else if state == "cancelled" {
                cancelled.push(held);
            }
        }
        retried.sort_by(|a, b| {
            let score = |v: &Value| {
                ["cycles", "repairs", "amended", "reopened"]
                    .iter()
                    .map(|k| v.get(k).int().unwrap_or(0))
                    .sum::<i128>()
            };
            score(b).cmp(&score(a)).then(a.text("id").cmp(b.text("id")))
        });
        let mut pass = 0;
        let mut fail = 0;
        let mut other = 0;
        let mut reasons = empty();
        let mut parks = empty();
        let mut finished = 0;
        for r in &rows {
            if r.text("kind") == "acceptance"
                && (r.opt("run_id").is_none() || runs.contains(r.text("run_id")))
            {
                match r.text("status") {
                    "pass" => pass += 1,
                    "fail" => fail += 1,
                    _ => other += 1,
                };
                if r.text("status") != "pass" && !r.text("reason").is_empty() {
                    let reason = clean(r.text("reason"), 80);
                    let n = reasons.get(&reason).int().unwrap_or(0);
                    reasons.set(&reason, number(n + 1))?;
                }
            }
            if r.text("kind") == "terminal" && runs.contains(r.text("run_id")) {
                if r.text("status") == "park" {
                    let reason = if r.text("reason").is_empty() {
                        "unstated".into()
                    } else {
                        clean(r.text("reason"), 80)
                    };
                    let n = parks.get(&reason).int().unwrap_or(0);
                    parks.set(&reason, number(n + 1))?;
                } else if r.text("status") == "done" {
                    finished += 1;
                }
            }
        }
        let omitted = [
            retried.len().saturating_sub(20),
            blocked.len().saturating_sub(20),
            cancelled.len().saturating_sub(20),
        ];
        retried.truncate(20);
        blocked.truncate(20);
        cancelled.truncate(20);
        let report = object([
            ("schema", number(1)),
            ("kind", string("oms-plan-retro")),
            ("goal", string(clean(s.plan.text("goal"), 200))),
            ("plan_id", s.plan.get("plan_id").clone()),
            (
                "tasks",
                object([("total", number(tasks.len())), ("states", states)]),
            ),
            ("retried", Value::Array(retried)),
            ("retried_omitted", number(omitted[0])),
            ("blocked", Value::Array(blocked)),
            ("blocked_omitted", number(omitted[1])),
            ("cancelled", Value::Array(cancelled)),
            ("cancelled_omitted", number(omitted[2])),
            (
                "drive",
                object([
                    ("runs", number(runs.len())),
                    ("cycles", number(cycles.values().sum::<u64>())),
                    ("finished", number(finished)),
                    ("parks", parks),
                ]),
            ),
            (
                "acceptance",
                object([
                    ("pass", number(pass)),
                    ("fail", number(fail)),
                    ("other", number(other)),
                    ("reasons", reasons),
                ]),
            ),
            ("ledger_complete", Value::Bool(complete)),
            (
                "not_counted",
                list([
                    "review finding verdicts",
                    "criteria whose evidence went stale",
                ]),
            ),
        ]);
        if req.has("json") {
            return Response::json_line(&report, false, false);
        }
        Ok(Response::text(render_retro(&report)?))
    }
    /// Internal plan-from-spec, goal-drive, autopilot and panel consumers use
    /// this validated status, never unchecked persisted state or a worker exit.
    pub fn status_snapshot(&self) -> Result<Value> {
        self.status_value(&mut self.load()?)
    }
    pub fn work_status_snapshot(&self, ids: &[&str]) -> Result<oms_context::Json> {
        let value = self.status_snapshot()?;
        oms_work_inspect::plan_status_snapshot(&json::canonical(&value)?, ids)
            .map_err(|e| error(e.to_string()))
    }
}
fn groups(s: &Snapshot) -> Result<Vec<(String, Vec<Value>)>> {
    let mut groups = Vec::<(String, Vec<Value>)>::new();
    for t in s.ordered()? {
        let id = t.text("discovered_from");
        if id.is_empty() {
            continue;
        }
        if let Some((_, kids)) = groups.iter_mut().find(|(p, _)| p == id) {
            kids.push(t);
        } else {
            groups.push((id.into(), vec![t]));
        }
    }
    Ok(groups)
}
fn wave_overlap(a: &Value, b: &Value) -> bool {
    fn roots(task: &Value) -> Vec<String> {
        let paths = task
            .opt("allowed_paths")
            .or_else(|| task.opt("allowed"))
            .and_then(|v| v.strings().ok())
            .unwrap_or_default();
        if paths.is_empty() {
            return vec![String::new()];
        }
        paths
            .into_iter()
            .map(|s| {
                let mut p = s.trim().replace('\\', "/");
                if let Some(at) = p.find(['*', '?', '[']) {
                    let prefix = &p[..at];
                    p = prefix
                        .rsplit_once('/')
                        .map(|(parent, _)| parent.to_owned())
                        .unwrap_or_default();
                }
                p = p.trim_matches('/').to_owned();
                while p.starts_with("./") {
                    p = p[2..].into();
                }
                if p == "." {
                    p.clear();
                }
                p
            })
            .collect()
    }
    let a = roots(a);
    let b = roots(b);
    a.iter().any(|a| {
        b.iter().any(|b| {
            a.is_empty()
                || b.is_empty()
                || a == b
                || a.starts_with(&format!("{b}/"))
                || b.starts_with(&format!("{a}/"))
        })
    })
}
fn retro_rows(plan: &Plan<'_>) -> Result<(Vec<Value>, bool)> {
    let path = plan.progress_path()?;
    let store = plan.plan_storage();
    if store.optional(&path, 0).is_ok_and(|v| v.is_none()) {
        return Ok((vec![], true));
    }
    let (raw, complete) = match store.tail(&path, 8 * 1024 * 1024) {
        Ok(value) => value,
        Err(_) => return Ok((vec![], false)),
    };
    let raw = if complete {
        raw.as_slice()
    } else {
        raw.iter()
            .position(|b| *b == b'\n')
            .map(|at| &raw[at + 1..])
            .unwrap_or(&[])
    };
    let mut rows = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if let Ok(row) = json::parse_legacy(line, 8 * 1024 * 1024)
            && row.object().is_ok()
        {
            rows.push(row);
        }
    }
    Ok((rows, complete))
}
fn clean(text: &str, limit: usize) -> String {
    let value = text
        .chars()
        .map(|c| {
            use unicode_general_category::{GeneralCategory, get_general_category};
            if c != ' '
                && matches!(
                    get_general_category(c),
                    GeneralCategory::Control
                        | GeneralCategory::Format
                        | GeneralCategory::Surrogate
                        | GeneralCategory::PrivateUse
                        | GeneralCategory::Unassigned
                        | GeneralCategory::SpaceSeparator
                        | GeneralCategory::LineSeparator
                        | GeneralCategory::ParagraphSeparator
                )
            {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.chars().count() <= limit {
        value
    } else {
        format!("{}…", value.chars().take(limit - 1).collect::<String>())
    }
}
fn render_retro(r: &Value) -> Result<String> {
    let mut lines = vec![format!(
        "plan retro: {}",
        if r.text("goal").is_empty() {
            "(no goal recorded)"
        } else {
            r.text("goal")
        }
    )];
    let states = r
        .get("tasks")
        .get("states")
        .object()?
        .iter()
        .map(|(k, v)| format!("{}={}", String::from_utf8_lossy(k), v.int().unwrap_or(0)))
        .collect::<Vec<_>>();
    lines.push(format!(
        "tasks: {} ({})",
        r.get("tasks").get("total").int()?,
        if states.is_empty() {
            "none".into()
        } else {
            states.join(", ")
        }
    ));
    let d = r.get("drive");
    lines.push(format!(
        "drive: {} run(s), {} cycle(s), {} finished",
        d.get("runs").int()?,
        d.get("cycles").int()?,
        d.get("finished").int()?
    ));
    for (reason, n) in d.get("parks").object()? {
        lines.push(format!(
            "  parked {}x: {}",
            n.int()?,
            String::from_utf8_lossy(reason)
        ));
    }
    let a = r.get("acceptance");
    lines.push(format!(
        "acceptance: {} pass, {} fail{}",
        a.get("pass").int()?,
        a.get("fail").int()?,
        if a.get("other").int()? > 0 {
            format!(", {} other", a.get("other").int()?)
        } else {
            String::new()
        }
    ));
    for (reason, n) in a.get("reasons").object()? {
        lines.push(format!(
            "  not passing {}x: {}",
            n.int()?,
            String::from_utf8_lossy(reason)
        ));
    }
    lines.push(format!(
        "took more than one try: {}",
        if r.get("retried").array()?.is_empty() {
            "none recorded"
        } else {
            ""
        }
    ));
    for item in r.get("retried").array()? {
        let counts = ["cycles", "repairs", "amended", "reopened"]
            .iter()
            .filter_map(|k| {
                item.get(k)
                    .int()
                    .ok()
                    .filter(|n| *n > 0)
                    .map(|n| format!("{k}={n}"))
            })
            .collect::<Vec<_>>();
        lines.push(format!(
            "  {}  {}  {}",
            item.text("id"),
            counts.join(", "),
            item.text("title")
        ));
    }
    if r.get("retried_omitted").int()? > 0 {
        lines.push(format!("  +{} more", r.get("retried_omitted").int()?));
    }
    for label in ["blocked", "cancelled"] {
        if !r.get(label).array()?.is_empty() {
            lines.push(format!("{label}:"));
            for item in r.get(label).array()? {
                lines.push(format!(
                    "  {}  {}  [{}]",
                    item.text("id"),
                    item.text("title"),
                    if item.text("reason").is_empty() {
                        "no reason recorded"
                    } else {
                        item.text("reason")
                    }
                ));
            }
            let n = r.get(&format!("{label}_omitted")).int()?;
            if n > 0 {
                lines.push(format!("  +{n} more"));
            }
        }
    }
    lines.push("not counted: review finding verdicts; criteria whose evidence went stale".into());
    if r.get("ledger_complete") == &Value::Bool(false) {
        lines.push("incomplete: the progress ledger is longer than the read cap; only its newest part was counted".into());
    }
    lines.push("counts of recorded rows only: not approval or verification evidence, and nothing was written; record what should outlive this plan yourself (PROJECT.md Decisions, oms agent-memory)".into());
    Ok(lines.join("\n"))
}
