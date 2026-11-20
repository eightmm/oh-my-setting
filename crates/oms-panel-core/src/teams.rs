//! Read-only Team views join canonical contracts to exact physical-intent
//! bindings. Neither a contract nor a retained intent proves execution.
#[cfg(test)]
use crate::NativeCommand;
use crate::{Error, Result, Services, Value, checkpoint, require};
use oms_command_plan::json::{self as plan_json, Value as PlanValue};
use oms_command_telemetry::store::{physical, resources};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const MAX_TEAMS: usize = 64;
pub const MAX_MEMBERS: usize = 32;
pub const MAX_INTENTS: usize = 512;
const MAX_MEMBER_INTENTS: usize = 8;

fn source(status: &str, reason: Option<&str>) -> Value {
    json!({"status":status,"reason":reason})
}
fn generation(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str()?.parse().ok())
}
fn display_value(value: &PlanValue) -> Result<Value> {
    Ok(match value {
        PlanValue::Null => Value::Null,
        PlanValue::Bool(value) => Value::Bool(*value),
        PlanValue::Number(value) if !value.contains(['.', 'e', 'E']) => {
            if let Ok(value) = value.parse::<i64>() {
                value.into()
            } else if let Ok(value) = value.parse::<u64>() {
                value.into()
            } else {
                value.clone().into()
            }
        }
        PlanValue::Number(value) => Value::Number(value.parse().map_err(Error::new)?),
        PlanValue::String(value) => String::from_utf8(value.clone()).map_err(Error::new)?.into(),
        PlanValue::Array(values) => {
            Value::Array(values.iter().map(display_value).collect::<Result<_>>()?)
        }
        PlanValue::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    Ok((
                        String::from_utf8(key.clone()).map_err(Error::new)?,
                        display_value(value)?,
                    ))
                })
                .collect::<Result<_>>()?,
        ),
    })
}
fn project_raw(raw: &[u8], intents: Option<&[physical::Intent]>) -> Result<Value> {
    let list = plan_json::parse_bounded(raw, 16 * 1024 * 1024).map_err(Error::new)?;
    let rows = list.array().map_err(Error::new)?;
    require(rows.len() <= 256, "Team source exceeds canonical bound")?;
    let mut roots: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    let mut unbound = Vec::new();
    let mut matched = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for native in rows.iter().take(MAX_TEAMS) {
        // Validate the contract before converting i128 generations to the
        // panel's JSON representation. Large integers display as decimal text.
        let raw = plan_json::canonical(native.get("spec")).map_err(Error::new)?;
        let valid = oms_command_plan::team::TeamSpec::parse(&raw).is_ok();
        let row = display_value(native)?;
        let Some(id) = row["id"].as_str() else {
            return Err(Error::new("Team row has no stable identity"));
        };
        require(
            !id.is_empty()
                && id.len() <= 160
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
            "invalid canonical Team identity",
        )?;
        require(seen.insert(id.to_owned()), "duplicate Team source identity")?;
        let spec = &row["spec"];
        if spec["id"] != id || !valid {
            unbound.push(json!({"id":id,"generation":null,"contract_phase":"unverified","reason":"Team contract missing or unverified","members":null}));
            continue;
        }
        let owner = spec["root_owner"].as_str().unwrap();
        let owner_generation = spec["root_owner_generation"].as_str().unwrap();
        let spec_sha = row["history"]
            .as_array()
            .and_then(|v| v.last())
            .and_then(|v| v["sha256"].as_str())
            .filter(|v| crate::hex(v, 64));
        let phase = row["phase"]
            .as_str()
            .filter(|v| ["planned", "held", "settled"].contains(v))
            .unwrap_or("unknown");
        let reason = row["reason"]
            .as_str()
            .filter(|value| value.chars().count() <= 300)
            .map(|value| {
                crate::redact(value.as_bytes()).unwrap_or_else(|_| "reason withheld".into())
            });
        let fact = |key: &str| row[key].as_bool().map(Value::Bool).unwrap_or(Value::Null);
        let members = spec["members"].as_array().unwrap();
        let mut ordered: Vec<_> = members.iter().collect();
        ordered.sort_by_key(|member| {
            (
                member["role"] != "lead",
                member["id"].as_str().unwrap_or_default(),
            )
        });
        let mut nodes = Vec::new();
        for member in ordered.into_iter().take(MAX_MEMBERS) {
            let member_id = member["id"].as_str().unwrap();
            let calls = intents
                .unwrap_or_default()
                .iter()
                .take(MAX_INTENTS)
                .filter(|intent| {
                    intent.resource_binding.as_ref().is_some_and(|binding| {
                        binding.root_owner == owner
                            && binding.root_generation == owner_generation
                            && binding.team.as_ref().is_some_and(|team| {
                                team.id == id
                                    && Some(team.spec_sha256.as_str()) == spec_sha
                                    && team.member == member_id
                                    && Some(team.member_generation)
                                        == generation(&member["generation"])
                            })
                    })
                })
                .collect::<Vec<_>>();
            for call in &calls {
                matched.insert(call.identity.call_id.clone());
            }
            let route = &member["route"];
            let calls_shown = calls.iter().take(MAX_MEMBER_INTENTS).map(|call| json!({
                "call_id":call.identity.call_id,"operation_id":call.identity.operation_id,
                "intent_phase":match call.phase {physical::Phase::Reserved=>"reserved",physical::Phase::Ready=>"ready"},
                "provider":call.identity.provider,"selected_model":call.identity.selected_model,
                "account":call.identity.account,"served_model":null,"running":null,"cleanup_complete":null,"usage_coverage":null
            })).collect::<Vec<_>>();
            nodes.push(json!({"id":member_id,"generation":member["generation"],"display_name":member["display_name"],
                "logical_parent":member["logical_parent"],"role":member["role"],"access":member["access"],
                "binding":"contract_only","enrollment":null,"execution":null,"native_children":null,
                "route":{"harness":route["harness"],"provider":route["provider"],"transport":route["transport"],"host":route["host"],
                    "requested_account":route["account"],"configured_model":route["configured_model"],"requested_model":route["requested_model"],
                    "resolved_model":route["resolved_model"],"requested_effort":route["requested_effort"],"served_model":null,"applied_effort":null},
                "physical_intents":if intents.is_some(){json!(calls_shown)}else{Value::Null},
                "omitted_intents":calls.len().saturating_sub(MAX_MEMBER_INTENTS)}));
        }
        roots.entry((owner.into(),owner_generation.into())).or_default().push(json!({
            "id":id,"generation":spec["generation"],"task_id":spec["task_id"],"mode":spec["mode"],"room_id":spec["room_id"],
            "spec_sha256":spec_sha,"contract_phase":phase,"reason":reason,"next_actor":null,
            "members":nodes,"omitted_members":members.len().saturating_sub(MAX_MEMBERS),
            "usage_coverage":null,"cleanup_coverage":null,
            "verified":fact("verified"),"admitted":fact("admitted"),"landed":fact("landed"),"published":fact("published")}));
    }
    let roots = roots
        .into_iter()
        .map(|((id, generation), mut teams)| {
            teams.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            json!({"id":id,"generation":generation,"binding":"contract_owner","teams":teams})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"roots":roots,"unbound_teams":unbound,"omitted_teams":rows.len().saturating_sub(MAX_TEAMS),
        "physical_intents":{"retained_count":intents.map(<[physical::Intent]>::len),"projected_count":intents.map(|v|v.len().min(MAX_INTENTS)),
            "matched_in_display":intents.map(|_|matched.len()),"unmatched_in_display":intents.map(|v|v.len().min(MAX_INTENTS).saturating_sub(matched.len())),
            "omitted_intents":intents.map(|v|v.len().saturating_sub(MAX_INTENTS)),"running_count":null}}),
    )
}
#[cfg(test)]
fn project(list: &Value, intents: Option<&[physical::Intent]>) -> Result<Value> {
    project_raw(&serde_json::to_vec(list).map_err(Error::new)?, intents)
}
fn resource_view(snapshot: Option<&resources::Snapshot>) -> Value {
    snapshot.map_or(Value::Null, |s| json!({"scope":"host_local_accounting_domain","domain":s.policy.domain,"epoch":s.policy.epoch,
        "host_generation_sha256":s.policy.host_generation_sha256,"active":s.active,
        "occupancy":{"teams":s.occupancy.teams,"invocations":s.occupancy.invocations,"open_process_claims":s.occupancy.open_processes,"write_wip":s.occupancy.write_wip},
        "limits":{"teams":s.policy.max_teams,"invocations":s.policy.max_invocations,"open_process_claims":s.policy.max_open_processes,"write_wip":s.policy.max_write_wip,"reviewer_headroom":s.policy.reviewer_headroom},
        "native_process_count":null,"native_usage_coverage":null,"native_global_hard_cap":"unsupported"}))
}
/// Uses existing read producers only. Sources are independently observed, not an
/// atomic global snapshot; source failures preserve nulls rather than zeroes.
pub fn read(repo: &Path, service: &dyn Services, stop: &AtomicBool) -> Result<Value> {
    read_at(repo, repo, service, stop)
}
pub fn read_at(
    execution: &Path,
    repo: &Path,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    checkpoint(stop)?;
    let list = service
        .team_contracts(execution, repo, stop)
        .and_then(|output| {
            output.success()?;
            Ok(output.stdout)
        });
    checkpoint(stop)?;
    let capacity = resources::snapshot(repo, stop);
    checkpoint(stop)?;
    let intents = physical::load(repo, stop);
    checkpoint(stop)?;
    let resource_status = match &capacity {
        Ok(Some(_)) => source("available", None),
        Ok(None) => source(
            "unconfigured",
            Some("No retained resource domain; occupancy unknown"),
        ),
        Err(_) => source("unavailable", Some("Resource snapshot could not be read")),
    };
    let physical_status = match &intents {
        Ok(v) if v.is_empty() => source(
            "no_retained_intents",
            Some("No retained intents; execution and native children remain unknown"),
        ),
        Ok(_) => source(
            "available",
            Some("Intent phases are not execution acknowledgements"),
        ),
        Err(_) => source("unavailable", Some("Physical intents could not be read")),
    };
    let projection = list.and_then(|list| project_raw(&list, intents.as_deref().ok()));
    let mut report = match projection {
        Ok(report) => report,
        Err(_) => {
            json!({"roots":null,"unbound_teams":null,"omitted_teams":null,"physical_intents":{"retained_count":intents.as_ref().ok().map(Vec::len),"running_count":null}})
        }
    };
    let available = report["roots"].is_array();
    let partial = report["omitted_teams"].as_u64().is_some_and(|n| n > 0)
        || report["unbound_teams"]
            .as_array()
            .is_some_and(|v| !v.is_empty());
    report["schema"] = 1.into();
    report["kind"] = "oms-panel-teams".into();
    report["executes"] = false.into();
    report["consistency"] = "independent_bounded_reads".into();
    report["sources"] = json!({"teams":source(if !available{"unavailable"}else if partial{"partial"}else{"available"},if !available{Some("Canonical Plan team-list or Team contracts could not be verified")}else if partial{Some("Some Team contracts are unverified or omitted by the display bound")}else{None}),"resources":resource_status,"physical_intents":physical_status});
    report["resources"] = resource_view(capacity.as_ref().ok().and_then(Option::as_ref));
    report["bounds"] = json!({"teams":MAX_TEAMS,"members_per_team":MAX_MEMBERS,"physical_intents":MAX_INTENTS,"intents_per_member":MAX_MEMBER_INTENTS,"plan_response_bytes":16*1024*1024});
    report["authority"] = "read_only_projection".into();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandOutput, PeerExecutor, services::ExecutionRequest};
    use oms_command_telemetry::store::usage;
    use std::cell::Cell;
    use std::ffi::OsString;

    fn row(id: &str) -> Value {
        let mut spec: Value = serde_json::from_str(include_str!(
            "../../oms-command-plan/tests/fixtures/team-spec.json"
        ))
        .unwrap();
        spec["id"] = id.into();
        for key in [
            "task_sha256",
            "objective_sha256",
            "criterion_sha256",
            "policy_sha256",
        ] {
            spec[key] = "a".repeat(64).into();
        }
        spec["base_sha"] = "b".repeat(40).into();
        for member in spec["members"].as_array_mut().unwrap() {
            member["route"]["policy_sha256"] = "a".repeat(64).into();
        }
        json!({"id":id,"spec":spec,"history":[{"sha256":"d".repeat(64)}],"phase":"held","reason":"base changed","verified":null,"admitted":null,"landed":null,"published":null,"usage_coverage":null,"cleanup_coverage":null})
    }
    fn intent(generation: i128) -> physical::Intent {
        physical::Intent {
            identity: usage::Identity {
                call_id: "call-a".into(),
                operation_id: "operation-a".into(),
                provider: "codex".into(),
                selected_model: Some("selected-from-intent".into()),
                account: Some("physical-account".into()),
                ..Default::default()
            },
            cumulative: None,
            write: false,
            phase: physical::Phase::Ready,
            usage_ordinal: Some(1),
            campaign_manifest_sha256: None,
            resource_binding: Some(resources::Binding {
                policy_sha256: "a".repeat(64),
                host_generation_sha256: "c".repeat(64),
                root_owner: "main-a".into(),
                root_generation: "evt-owner".into(),
                team: Some(resources::Team {
                    id: "team-a".into(),
                    spec_sha256: "d".repeat(64),
                    member: "worker-a".into(),
                    member_generation: generation,
                }),
                reviewer: false,
                persistent_session: false,
                cleanup_scopes: vec![oms_supervisor::process::Scope::Leader],
            }),
        }
    }
    #[test]
    fn contracts_duplicate_labels_and_intents_never_manufacture_execution() {
        for id in ["_team", "-team", ".team"] {
            assert!(
                project(&json!([row(id)]), None).is_ok(),
                "canonical Team ID {id}"
            );
        }
        let report = project(&json!([row("team-b"), row("team-a")]), Some(&[intent(1)])).unwrap();
        let teams = report["roots"][0]["teams"].as_array().unwrap();
        assert_eq!(teams[0]["id"], "team-a");
        assert_eq!(teams[1]["id"], "team-b");
        assert_eq!(teams[0]["contract_phase"], "held");
        assert_eq!(teams[0]["reason"], "base changed");
        let lead = &teams[0]["members"][0];
        let worker = &teams[0]["members"][1];
        assert_eq!(lead["display_name"], worker["display_name"]);
        assert_ne!(lead["id"], worker["id"]);
        assert_eq!(worker["logical_parent"], lead["id"]);
        assert_eq!(worker["enrollment"], Value::Null);
        assert_eq!(worker["execution"], Value::Null);
        assert_eq!(worker["native_children"], Value::Null);
        assert_eq!(
            worker["physical_intents"][0]["selected_model"],
            "selected-from-intent"
        );
        assert_eq!(worker["physical_intents"][0]["intent_phase"], "ready");
        assert_eq!(worker["physical_intents"][0]["running"], Value::Null);
        assert_eq!(worker["physical_intents"][0]["served_model"], Value::Null);
        assert_eq!(report["physical_intents"]["matched_in_display"], 1);
        let stale = project(&json!([row("team-a")]), Some(&[intent(2)])).unwrap();
        assert_eq!(stale["physical_intents"]["matched_in_display"], 0);
        assert_eq!(stale["physical_intents"]["unmatched_in_display"], 1);
        assert!(
            stale["roots"][0]["teams"][0]["members"][1]["physical_intents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn resource_claim_dimensions_and_unknown_native_scope_stay_separate() {
        let snapshot = resources::Snapshot {
            sha256: "d".repeat(64),
            policy: resources::Policy {
                domain: "domain-a".into(),
                epoch: "epoch-a".into(),
                host_generation_sha256: "c".repeat(64),
                max_teams: 2,
                max_invocations: 4,
                max_open_processes: 8,
                max_write_wip: 3,
                reviewer_headroom: 1,
            },
            active: true,
            occupancy: resources::Occupancy {
                teams: 1,
                invocations: 2,
                open_processes: 5,
                write_wip: 3,
            },
            teams: Vec::new(),
            writes: Vec::new(),
        };
        let view = resource_view(Some(&snapshot));
        assert_eq!(view["scope"], "host_local_accounting_domain");
        assert_eq!(view["occupancy"]["invocations"], 2);
        assert_eq!(view["occupancy"]["open_process_claims"], 5);
        assert_eq!(view["occupancy"]["write_wip"], 3);
        assert_eq!(view["limits"]["open_process_claims"], 8);
        assert_eq!(view["native_process_count"], Value::Null);
        assert_eq!(view["native_global_hard_cap"], "unsupported");
    }
    #[test]
    fn missing_contracts_and_display_bounds_are_explicit() {
        let invalid = json!([{"id":"missing","spec":null}]);
        let report = project(&invalid, None).unwrap();
        assert_eq!(report["unbound_teams"][0]["contract_phase"], "unverified");
        assert_eq!(report["physical_intents"]["retained_count"], Value::Null);
        let many = (0..MAX_TEAMS + 1)
            .map(|i| row(&format!("team-{i}")))
            .collect::<Vec<_>>();
        let report = project(&json!(many), Some(&[])).unwrap();
        assert_eq!(report["omitted_teams"], 1);
        assert_eq!(
            report["roots"][0]["teams"].as_array().unwrap().len(),
            MAX_TEAMS
        );
        assert_eq!(report["physical_intents"]["running_count"], Value::Null);
        assert!(project(&json!({}), None).is_err());
    }
    #[test]
    fn read_only_sources_keep_errors_absence_and_scope_visible() {
        struct Host {
            bad: bool,
            calls: Cell<usize>,
            raw: Option<Vec<u8>>,
        }
        impl PeerExecutor for Host {
            fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                panic!("no execution");
            }
        }
        impl Services for Host {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                args: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                assert_eq!(command, NativeCommand::Plan);
                assert_eq!(args, crate::services::args(&["team-list", "--json"]));
                self.calls.set(self.calls.get() + 1);
                if self.bad {
                    return Err(Error::new("source unavailable"));
                }
                Ok(CommandOutput {
                    stdout: self
                        .raw
                        .clone()
                        .unwrap_or_else(|| serde_json::to_vec(&json!([row("team-a")])).unwrap()),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }
        let root =
            std::env::temp_dir().join(format!("oms-team-view-{}", crate::value::unique().unwrap()));
        std::fs::create_dir(&root).unwrap();
        for bad in [false, true] {
            let host = Host {
                bad,
                calls: Cell::new(0),
                raw: None,
            };
            let report = read(&root, &host, &AtomicBool::new(false)).unwrap();
            assert_eq!(host.calls.get(), 1);
            assert_eq!(report["executes"], false);
            assert_eq!(report["resources"], Value::Null);
            assert_eq!(report["sources"]["resources"]["status"], "unconfigured");
            assert_eq!(report["physical_intents"]["running_count"], Value::Null);
            if bad {
                assert_eq!(report["roots"], Value::Null);
                assert_eq!(report["sources"]["teams"]["status"], "unavailable");
            } else {
                assert_eq!(report["roots"][0]["teams"][0]["contract_phase"], "held");
            }
            assert_eq!(
                std::fs::read_dir(&root).unwrap().count(),
                0,
                "view must create no state"
            );
        }
        let big = i128::from(u64::MAX) + 7;
        let mut native =
            oms_command_plan::json::parse(&serde_json::to_vec(&row("team-a")).unwrap()).unwrap();
        let mut spec = native.get("spec").clone();
        let mut members = spec.get("members").clone();
        members.array_mut().unwrap()[1]
            .set("generation", oms_command_plan::json::number(big))
            .unwrap();
        spec.set("members", members).unwrap();
        native.set("spec", spec).unwrap();
        let raw =
            oms_command_plan::json::canonical(&oms_command_plan::json::Value::Array(vec![native]))
                .unwrap();
        let report = read(
            &root,
            &Host {
                bad: false,
                calls: Cell::new(0),
                raw: Some(raw.clone()),
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            report["roots"].is_array(),
            "lossless native Team list must remain readable"
        );
        assert_eq!(report["unbound_teams"], json!([]));
        assert_eq!(
            report["roots"][0]["teams"][0]["members"][1]["generation"],
            big.to_string()
        );
        let matched = project_raw(&raw, Some(&[intent(big)])).unwrap();
        assert_eq!(matched["physical_intents"]["matched_in_display"], 1);
        let stale = project_raw(&raw, Some(&[intent(big - 1)])).unwrap();
        assert_eq!(stale["physical_intents"]["matched_in_display"], 0);
        assert_eq!(stale["physical_intents"]["unmatched_in_display"], 1);
        {
            let store = crate::Store::open(&root).unwrap();
            let tx = store
                .transaction("fixture-team-view", std::time::Duration::from_secs(1))
                .unwrap();
            tx.write_json(
                Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
                &json!({"schema":99,"calls":{}}),
                false,
                4 * 1024 * 1024,
            )
            .unwrap();
        }
        let registry = root.join(".oms/artifacts/panel-dispatch/reservations.json");
        let before = std::fs::read(&registry).unwrap();
        let report = read(
            &root,
            &Host {
                bad: false,
                calls: Cell::new(0),
                raw: None,
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report["sources"]["resources"]["status"], "unavailable");
        assert_eq!(
            report["sources"]["physical_intents"]["status"],
            "unavailable"
        );
        assert_eq!(report["resources"], Value::Null);
        assert_eq!(report["physical_intents"]["retained_count"], Value::Null);
        assert_eq!(
            report["roots"][0]["teams"][0]["members"][0]["physical_intents"],
            Value::Null
        );
        assert_eq!(std::fs::read(&registry).unwrap(), before);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
