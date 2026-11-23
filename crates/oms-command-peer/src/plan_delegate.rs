//! Exact Plan lease integration for the existing native delegate producer.
//! The host supplies the owning Plan services; read projections grant no writes.
use crate::{Context, Error, Result, V, cli::Args, fail, providers, storage};
use oms_command_plan::{Action, DelegateBinding, Plan, Request, json as plan_json};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn domain(error: oms_command_plan::Error) -> Error {
    Error::new(error.code, error.message)
}

pub(crate) struct BoundDelegate {
    repo: PathBuf,
    state: PathBuf,
    binding: DelegateBinding,
    issued: Vec<u8>,
    task: plan_json::Value,
}

impl BoundDelegate {
    fn plan<'a>(&self, context: &'a Context<'_>) -> Result<Plan<'a>> {
        let services = context.executor.plan_delegate_services().ok_or_else(|| {
            Error::new(
                2,
                "plan lease integration requires the owning native Plan adapter",
            )
        })?;
        let req = Request::new(Action::Status);
        let plan_context = oms_command_plan::Context::from_selected_state_before(
            &req,
            &self.repo,
            context.cwd,
            &self.state,
            context.interrupted,
            Instant::now() + Duration::from_secs(20),
        )
        .map_err(domain)?;
        Plan::open(plan_context, services, context.interrupted).map_err(domain)
    }

    pub(crate) fn prepare(
        args: &mut Args,
        repo: &Path,
        context: &Context<'_>,
    ) -> Result<Option<Self>> {
        if !args.has("plan-task") && !args.has("plan-binding") {
            if args.has("terminalize-resumed-repair") {
                return fail("resumed repair requires an exact Plan lease binding");
            }
            return Ok(None);
        }
        if args.all("plan-task").len() != 1 || args.all("plan-binding").len() != 1 {
            return fail("plan lease requires one --plan-task and its issued --plan-binding");
        }
        let binding =
            DelegateBinding::parse(args.required("plan-binding")?.as_bytes()).map_err(domain)?;
        if binding.task_id() != args.required("plan-task")? {
            return fail("Plan delegation task differs from its issued lease binding");
        }
        for flag in [
            "prompt",
            "prompt-file",
            "brief-file",
            "verify",
            "scope",
            "task-id",
            "read-only",
            "interactive",
            "no-verify",
            "allow-no-change",
        ] {
            if args.has(flag) {
                return fail(format!(
                    "--{flag} cannot override the frozen Plan delegation"
                ));
            }
        }
        if !args.positional.is_empty() || !args.command.is_empty() {
            return fail("Plan delegation takes its prompt and verifier from the issued task");
        }
        let mut bound = Self {
            repo: repo.to_owned(),
            state: crate::native::state_repo(repo)?,
            issued: binding.encode().map_err(domain)?,
            binding,
            task: plan_json::Value::Null,
        };
        let plan = bound.plan(context)?;
        let task = plan
            .check_delegate_binding(&bound.binding)
            .map_err(domain)?;
        if task.text("state") != "claimed" {
            return fail("Plan delegation has already started; missing acknowledgement is unknown");
        }
        let target = providers::Target::parse(args.required("to")?)?;
        if target.provider != task.text("provider") {
            return fail("Plan delegation provider differs from the issued claimant");
        }
        let assignment = task.get("assignment");
        if let Some(provider) = assignment.opt("provider")
            && provider.str().map_err(domain)? != target.provider
        {
            return fail("Plan delegation provider differs from the frozen assignment");
        }
        if args.has("role") && args.get("role") != task.text("role") {
            return fail("Plan delegation role differs from the frozen task");
        }
        if !task.text("role").is_empty() {
            args.set_value("role", task.text("role"));
        }
        let repair = task.get("repair_count").int().map_err(domain)?;
        let resumed = args.has("terminalize-resumed-repair");
        if resumed {
            let repair_artifact = if task.text("repair_artifact").is_empty() {
                task.text("artifact")
            } else {
                task.text("repair_artifact")
            };
            if repair != 1
                || task.text("review_lease_id") != bound.binding.lease_id()
                || task.text("artifact").is_empty()
                || task.text("patch").is_empty()
                || repair_artifact.is_empty()
                || args.get("review-artifact") != repair_artifact
                || args.has("repair")
            {
                return fail(
                    "resumed repair must retain its exact one-shot reviewed lease and artifact",
                );
            }
        } else if repair != 0 || args.has("review-artifact") {
            return fail("reviewed repair requires the canonical same-lease continuation");
        }
        let allowed = task.get("allowed_paths").strings().map_err(domain)?;
        let forbidden = task.get("forbidden_paths").strings().map_err(domain)?;
        if allowed.is_empty() || task.text("verify").trim().is_empty() {
            return fail("Plan delegation requires frozen paths and a verifier");
        }
        let prompt = format!(
            "Goal: {}\n\n{}\n\nAllowed paths: {}\nForbidden paths: {}\nRequired verification: {}\nDone when: the frozen task is implemented and its required check passes. Return evidence; the owner retains landing and acceptance.",
            task.text("title"),
            task.text("brief"),
            allowed.join(", "),
            forbidden.join(", "),
            task.text("verify"),
        );
        args.set_value("prompt", prompt);
        args.set_value("verify", task.text("verify"));
        args.set_value("task-id", bound.binding.task_id());
        for pattern in allowed {
            args.add_value("scope", pattern);
        }
        bound.task = task;
        Ok(Some(bound))
    }

    pub(crate) fn start(&mut self, context: &Context<'_>) -> Result<()> {
        let request = Request::new(Action::Start)
            .with("id", self.binding.task_id())
            .with("lease-id", self.binding.lease_id());
        self.binding = self
            .plan(context)?
            .execute_bound_task(&request, &self.binding, None)
            .map_err(domain)?;
        Ok(())
    }

    pub(crate) fn route(&self, args: &Args, route: &providers::Route) -> Result<()> {
        for (field, flag, supplied) in [
            ("model", "model", route.model.as_str()),
            (
                "fallback_model",
                "fallback-model",
                args.get("fallback-model"),
            ),
            (
                "reasoning_effort",
                "reasoning-effort",
                route.effort.as_str(),
            ),
            ("workload", "workload", args.value("workload", "standard")),
        ] {
            let frozen = self.task.get("assignment").text(field);
            if !frozen.is_empty() && frozen != "auto" && supplied != frozen {
                return fail(format!("--{flag} differs from the frozen Plan assignment"));
            }
        }
        Ok(())
    }

    pub(crate) fn check(&self, context: &Context<'_>) -> Result<()> {
        self.plan(context)?
            .check_delegate_binding(&self.binding)
            .map_err(domain)?;
        Ok(())
    }

    pub(crate) fn launch_environment(&self) -> Result<Vec<(OsString, OsString)>> {
        Ok(vec![
            (
                "OMS_DELEGATE_PLAN_BINDING".into(),
                String::from_utf8(self.binding.encode().map_err(domain)?)
                    .map_err(|_| Error::new(2, "invalid Plan binding encoding"))?
                    .into(),
            ),
            (
                "OMS_DELEGATE_PLAN_REPO".into(),
                self.repo.as_os_str().to_owned(),
            ),
            (
                "OMS_DELEGATE_PLAN_STATE".into(),
                self.state.as_os_str().to_owned(),
            ),
        ])
    }

    pub(crate) fn forbidden(&self, name: &str) -> Result<bool> {
        for pattern in self.task.get("forbidden_paths").strings().map_err(domain)? {
            if oms_contract_markdown::scope::matches(name, pattern)
                .map_err(|e| Error::new(2, e.to_string()))?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn issued(&self) -> Result<V> {
        V::parse(&self.issued).map_err(Error::from)
    }

    pub(crate) fn review(
        &self,
        index: &V,
        operation: &str,
        attempt: &str,
        context: &Context<'_>,
    ) -> Result<()> {
        if index.get("kind").text() != Some("delegate")
            || index.get("task_id").text() != Some(self.binding.task_id())
            || index.get("provider").text() != Some(self.task.text("provider"))
            || index.get("base_sha").text() != Some(self.binding.base_sha())
            || index.get("operation_id").text() != Some(operation)
            || index.get("attempt_id").text() != Some(attempt)
            || index.get("exit") != &V::Num("0".into())
            || index.get("verification").text() != Some("passed")
            || index.get("verify_exit") != &V::Num("0".into())
            || index.get("state").text() != Some("review")
            || index.get("no_change") != &V::Bool(false)
            || index.get("plan_binding") != &self.issued()?
        {
            return fail("native delegation result lacks its exact verified Plan binding");
        }
        let raw = storage::read(&self.repo, Path::new(".oms/artifacts/index.jsonl"), false)?;
        let matching = storage::rows(&raw)?
            .into_iter()
            .filter(|row| row.get("event_id") == index.get("event_id"))
            .collect::<Vec<_>>();
        if matching.len() != 1 || matching[0] != *index {
            return fail("native delegation result index changed before Plan Review");
        }
        for (field, digest) in [("artifact", "artifact_sha256"), ("patch", "patch_sha256")] {
            let path = index
                .get(field)
                .text()
                .ok_or_else(|| Error::new(2, "native result path missing"))?;
            let bytes = storage::read(&self.repo, Path::new(path), false)?;
            if bytes.is_empty() || Some(storage::hash(&bytes).as_str()) != index.get(digest).text()
            {
                return fail("native delegation artifact bytes changed before Plan Review");
            }
        }
        let request = Request::new(Action::Review)
            .with("id", self.binding.task_id())
            .with("lease-id", self.binding.lease_id())
            .with(
                "artifact",
                self.repo
                    .join(index.get("artifact").text().unwrap())
                    .as_os_str(),
            )
            .with(
                "patch",
                self.repo
                    .join(index.get("patch").text().unwrap())
                    .as_os_str(),
            );
        let proof = plan_json::parse(&index.json(false)?).map_err(domain)?;
        self.plan(context)?
            .execute_bound_task(&request, &self.binding, Some(&proof))
            .map_err(domain)?;
        Ok(())
    }
}

/// Internal locators are checked before each physical provider attempt,
/// including fallback. They are removed from the actual child's environment.
pub(crate) fn launch_key(key: &OsStr) -> bool {
    [
        "OMS_DELEGATE_PLAN_BINDING",
        "OMS_DELEGATE_PLAN_REPO",
        "OMS_DELEGATE_PLAN_STATE",
    ]
    .iter()
    .any(|name| key == *name)
}

pub(crate) fn check_launch(
    extra: &[(OsString, OsString)],
    provider: &str,
    context: &Context<'_>,
) -> Result<()> {
    if !extra.iter().any(|(key, _)| launch_key(key)) {
        return Ok(());
    }
    let one = |name: &str| -> Result<&OsStr> {
        let values = extra
            .iter()
            .filter(|(key, _)| key == name)
            .collect::<Vec<_>>();
        if values.len() != 1 {
            return fail("incomplete Plan provider launch binding");
        }
        Ok(values[0].1.as_os_str())
    };
    let binding = DelegateBinding::parse(one("OMS_DELEGATE_PLAN_BINDING")?.as_encoded_bytes())
        .map_err(domain)?;
    let bound = BoundDelegate {
        repo: PathBuf::from(one("OMS_DELEGATE_PLAN_REPO")?),
        state: PathBuf::from(one("OMS_DELEGATE_PLAN_STATE")?),
        issued: binding.encode().map_err(domain)?,
        binding,
        task: plan_json::Value::Null,
    };
    let task = bound
        .plan(context)?
        .check_delegate_binding(&bound.binding)
        .map_err(domain)?;
    if task.text("state") != "running" || task.text("provider") != provider {
        return fail("Plan provider launch no longer owns its exact running lease");
    }
    Ok(())
}
