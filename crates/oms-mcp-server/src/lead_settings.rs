//! Settings are owned by a single private kickoff operation. The same lock
//! orders a preference change with the reservation of the next user message.
use super::{Conversation, INVALID};
use crate::panel::PanelResult;
use crate::{Server, peer::valid_lead_route, storage::hash, value::Value};
use oms_command_agent_role::native_model::{EFFORTS, agy_selector, fixed_effort_model};
use std::collections::BTreeMap;

const FILE: &str = "lead-settings.json";
const INVALID_CHOICE: &str =
    "The selected model or effort is not available for this Lead. Refresh its settings.";
fn canonical(provider: &str) -> &str {
    if provider == "agy" {
        "antigravity"
    } else {
        provider
    }
}

pub(super) fn selection<'a>(
    provider: &str,
    route: &'a Value,
) -> PanelResult<Option<(&'a str, Option<&'a str>)>> {
    if !route.as_object().is_some_and(|o| o.iter().count() == 2) {
        return Err(INVALID_CHOICE.into());
    }
    let model = route.get("model").ok_or(INVALID_CHOICE)?;
    let effort = route.get("effort").ok_or(INVALID_CHOICE)?;
    if model.is_null() && effort.is_null() {
        return Ok(None);
    }
    let model = model.as_str().ok_or(INVALID_CHOICE)?;
    let effort = if effort.is_null() {
        None
    } else {
        Some(effort.as_str().ok_or(INVALID_CHOICE)?)
    };
    crate::output::outbound(model.as_bytes(), &Value::Null).map_err(|_| INVALID_CHOICE)?;
    valid_lead_route(canonical(provider), model, effort)
        .then_some(Some((model, effort)))
        .ok_or(INVALID_CHOICE.into())
}

fn policy_aliases(server: &Server<'_>) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::<String, Vec<String>>::new();
    let raw = oms_safe_fs::Root::open(&server.source_root).and_then(|root| {
        root.read(
            "config/panel-routing.json",
            128 * 1024,
            oms_safe_fs::Policy::source(),
        )
    });
    let Some(v) = raw
        .ok()
        .filter(|b| oms_context::parse_json_bounded(b, 128 * 1024).is_ok())
        .and_then(|b| crate::value::from_slice(&b).ok())
    else {
        return result;
    };
    if v["schema"] != 2 {
        return result;
    }
    for row in v["tiers"]
        .as_object()
        .into_iter()
        .flat_map(|o| o.iter())
        .filter_map(|(_, r)| r.as_array())
        .flatten()
        .filter(|r| r["provider"] == "claude" && r["public_only"] != true)
    {
        if let Some(model) = row["model"]
            .as_str()
            .filter(|s| valid_lead_route("claude", s, None))
        {
            let list = result.entry(model.to_owned()).or_default();
            if let Some(e) = row["effort"]
                .as_str()
                .filter(|s| valid_lead_route("claude", model, Some(s)))
                && !list.iter().any(|v| v == e)
            {
                list.push(e.to_owned());
            }
        }
    }
    result
}

fn option(model: &str, source: &str, efforts: &[String], fixed: bool, requires: bool) -> Value {
    json!({"model":model,"source":source,"efforts":efforts,"fixed":fixed,"requiresEffort":requires})
}

impl Conversation<'_> {
    fn catalog(&self, server: &Server<'_>) -> (Vec<Value>, Value) {
        let provider = canonical(self.provider);
        // Bare 'models' reads the native capability cache; never pass --refresh/--live-models.
        let args = vec![
            "models".into(),
            "--providers".into(),
            provider.into(),
            "--json".into(),
        ];
        let report = server
            .panel_exec(self.repo, args, false)
            .ok()
            .map(|(v, _)| v);
        let report = report
            .as_ref()
            .filter(|r| r["schema"] == 1)
            .and_then(|r| r["providers"].as_array())
            .and_then(|rows| match rows.as_slice() {
                [row]
                    if row["provider"] == provider
                        && row["present"] == true
                        && row["exact_model_override"] == true =>
                {
                    Some(row)
                }
                _ => None,
            });
        let mut options = BTreeMap::new();
        if let Some(r) = report.filter(|r| r["catalog_probe"] == "ok") {
            let names = |field: &str| {
                r[field]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
            };
            let routable = names("routable");
            let catalog = names("catalog_models");
            let eligible = |name: &str| routable.contains(name) && catalog.contains(name);
            // AGY's transport turns a base and effort into a native selector.
            // Do not advertise a base by itself when only its effort variants exist.
            let mut grouped = std::collections::BTreeSet::new();
            if provider == "antigravity"
                && let Some(bases) = r["selectors"].as_object()
            {
                for (name, map) in bases.iter() {
                    let Some(model) = name
                        .as_str()
                        .filter(|m| valid_lead_route(provider, m, None))
                    else {
                        continue;
                    };
                    let mut efforts = Vec::new();
                    for &e in EFFORTS {
                        if valid_lead_route(provider, model, Some(e))
                            && let Some(id) = map[e].as_str()
                            && id == agy_selector(model, e)
                            && eligible(id)
                        {
                            grouped.insert(id);
                            efforts.push(e.to_owned());
                        }
                    }
                    if !efforts.is_empty() {
                        options.insert(
                            model.to_owned(),
                            option(model, "catalog", &efforts, false, !eligible(model)),
                        );
                    }
                }
            }
            for model in r["routable"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if options.contains_key(model)
                    || grouped.contains(model)
                    || !eligible(model)
                    || !valid_lead_route(provider, model, None)
                {
                    continue;
                }
                let fixed = fixed_effort_model(provider, model).is_some();
                let efforts: Vec<_> = EFFORTS
                    .iter()
                    .copied()
                    .filter(|e| {
                        !fixed
                            && valid_lead_route(provider, model, Some(e))
                            && r["model_effort_scales"][model]
                                .as_array()
                                .is_some_and(|a| a.iter().any(|v| v == *e))
                    })
                    .map(str::to_owned)
                    .collect();
                options.insert(
                    model.to_owned(),
                    option(model, "catalog", &efforts, fixed, false),
                );
            }
        } else if provider == "claude"
            && report.is_some_and(|r| r["catalog_probe"] == "unsupported")
        {
            for (model, efforts) in policy_aliases(server) {
                options.insert(
                    model.clone(),
                    option(&model, "policy", &efforts, false, false),
                );
            }
        }
        (
            options.into_values().take(64).collect(),
            json!(report.and_then(|r| r["catalog_age_seconds"].as_u64())),
        )
    }

    fn stored(&self, generation: u64, route: &Value) -> Value {
        let revision = hash(
            json!([
                "oms-lead-settings-v1",
                self.scope,
                self.thread,
                generation,
                route["model"],
                route["effort"]
            ])
            .to_string(),
        );
        json!({"scope":self.scope,"generation":generation,"selection":route,"revision":revision})
    }

    pub(super) fn current_settings(&self) -> PanelResult<Value> {
        match self.read(FILE)? {
            None => Ok(self.stored(0, &json!({"model":Value::Null,"effort":Value::Null}))),
            Some(v) => {
                let g = v["generation"].as_u64().filter(|g| *g > 0).ok_or(INVALID)?;
                selection(self.provider, &v["selection"])?;
                (self.stored(g, &v["selection"]) == v)
                    .then_some(v)
                    .ok_or(INVALID.into())
            }
        }
    }
    fn allowed(&self, route: &Value, options: &[Value]) -> PanelResult<()> {
        if let Some((model, effort)) = selection(self.provider, route)?
            && !options.iter().any(|o| {
                o["model"] == model
                    && match effort {
                        Some(e) => o["efforts"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v == e)),
                        None => o["requiresEffort"] == false,
                    }
            })
        {
            return Err(INVALID_CHOICE.into());
        }
        Ok(())
    }
    pub(super) fn check_selection(&self, server: &Server<'_>, route: &Value) -> PanelResult<()> {
        self.allowed(route, &self.catalog(server).0)
    }
    pub(in crate::panel) fn settings(
        &self,
        server: &Server<'_>,
        update: bool,
        args: &Value,
        check: &impl Fn() -> PanelResult<()>,
    ) -> PanelResult<Value> {
        let (options, age) = self.catalog(server);
        // Model availability and policy are observations. Only the original
        // turn/room owner may change the saved selection, even with a valid revision.
        let _lock = update
            .then(|| self.dir.lock())
            .transpose()
            .map_err(|_| "This Lead is changing; refresh its settings.")?;
        self.bound()?;
        check()?;
        let mut saved = self.current_settings()?;
        if update {
            if args["revision"] != saved["revision"] {
                return Err("Lead settings changed. Refresh before saving.".into());
            }
            let route = json!({"model":args.get("model").ok_or(INVALID_CHOICE)?,"effort":args.get("effort").ok_or(INVALID_CHOICE)?});
            self.allowed(&route, &options)?;
            if route != saved["selection"] {
                let g = saved["generation"]
                    .as_u64()
                    .and_then(|n| n.checked_add(1))
                    .ok_or(INVALID)?;
                saved = self.stored(g, &route);
                self.bound()?;
                check()?;
                self.dir
                    .atomic(FILE, saved.to_string().as_bytes())
                    .map_err(|_| INVALID)?;
                if self.current_settings()? != saved {
                    return Err(INVALID.into());
                }
            }
        }
        let (native, served) = self
            .native_invocation(server)
            .unwrap_or((Value::Null, Value::Null));
        self.bound()?;
        check()?;
        Ok(
            json!({"revision":saved["revision"],"selection":saved["selection"],"options":options,
        "catalogAgeSeconds":age,"lastServed":served,"nativeSession":native,"nativeTitle":Value::Null}),
        )
    }
}
