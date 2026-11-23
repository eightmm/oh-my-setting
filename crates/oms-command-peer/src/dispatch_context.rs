//! Dispatch metadata crosses the staged process boundary as environment values.
//! These locators are not proof of Plan, Room, lease or launch authority.
use crate::{V, providers};
use std::ffi::{OsStr, OsString};

#[derive(Clone, Debug, Default)]
pub struct DispatchContext {
    pub task_id: String,
    pub parent_attempt_id: Option<String>,
    pub room_id: Option<String>,
    pub panel: Option<PanelDispatch>,
}

#[derive(Clone, Debug, Default)]
pub struct PanelDispatch {
    pub role: Option<String>,
    pub strategy: Option<String>,
    pub owner: Option<String>,
    pub purpose: Option<String>,
    pub workload: Option<String>,
    pub access: Option<String>,
    pub label: Option<String>,
    pub alias: Option<String>,
    pub participant: Option<String>,
    pub lease_id: Option<String>,
    pub continues: Option<String>,
}

impl DispatchContext {
    pub(crate) fn from_environment(cli_task: &str) -> Self {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Self {
            task_id: if cli_task.is_empty() {
                read("OMS_TASK_ID").unwrap_or_default()
            } else {
                cli_task.to_owned()
            },
            parent_attempt_id: read("OMS_PANEL_MAIN_ATTEMPT"),
            room_id: read("OMS_ROOM_ID"),
            panel: (std::env::var_os("OMS_PANEL_DISPATCH").as_deref() == Some(OsStr::new("1")))
                .then(|| PanelDispatch {
                    role: read("OMS_PANEL_ROLE"),
                    strategy: read("OMS_PANEL_STRATEGY"),
                    owner: read("OMS_PANEL_OWNER"),
                    purpose: read("OMS_PANEL_PURPOSE"),
                    workload: read("OMS_PANEL_WORKLOAD"),
                    access: read("OMS_PANEL_ACCESS"),
                    label: read("OMS_PANEL_LABEL"),
                    alias: read("OMS_PANEL_ALIAS"),
                    participant: read("OMS_ROOM_PARTICIPANT"),
                    lease_id: read("OMS_PLAN_LEASE_ID"),
                    continues: read("OMS_PANEL_CONTINUES"),
                }),
        }
    }

    /// Projects only supplied metadata. The launch owner retains responsibility
    /// for inherited-value removal and authenticated admission/launch fields.
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        let mut values = vec![("OMS_TASK_ID".into(), self.task_id.clone().into())];
        for (name, value) in [
            ("OMS_PANEL_MAIN_ATTEMPT", self.parent_attempt_id.as_deref()),
            ("OMS_ROOM_ID", self.room_id.as_deref()),
        ] {
            if let Some(value) = value {
                values.push((name.into(), value.into()));
            }
        }
        if let Some(panel) = &self.panel {
            values.push(("OMS_PANEL_DISPATCH".into(), "1".into()));
            for (_, name, value) in panel.fields() {
                if let Some(value) = value {
                    values.push((name.into(), value.into()));
                }
            }
        }
        values
    }

    fn selected_refs(&self, refs: &mut V, route: &providers::Route) {
        refs.set("panel_model", route.model.as_str());
        if let Some(room) = &self.room_id {
            refs.set("panel_room_id", room.as_str());
        }
        let account = providers::panel_account(route);
        if !account.is_empty() {
            refs.set("panel_account", account);
        }
    }

    pub(crate) fn peer_refs(&self, refs: &mut V, route: &providers::Route, tool: &str) {
        refs.set("panel_alias", format!("{}-{tool}", route.provider));
        refs.set(
            "panel_role",
            if tool == "peer-review" {
                "reviewer"
            } else {
                "advisor"
            },
        );
        refs.set("panel_label", format!("{tool} ({})", route.provider));
        self.selected_refs(refs, route);
        if let Some(panel) = &self.panel {
            refs.set("panel_access", panel.access.as_deref().unwrap_or("read"));
            panel.apply_refs(refs, false);
        }
    }

    pub(crate) fn delegate_refs(
        &self,
        refs: &mut V,
        route: &providers::Route,
        provider: &str,
        read: bool,
        label: &str,
    ) {
        self.selected_refs(refs, route);
        refs.set("panel_label", label);
        if let Some(panel) = &self.panel {
            refs.set("panel_role", if read { "researcher" } else { "worker" });
            refs.set("panel_purpose", if read { "research" } else { "implement" });
            refs.set("panel_workload", "routine");
            refs.set("panel_access", if read { "read" } else { "write" });
            refs.set("panel_effort", route.effort.as_str());
            panel.apply_refs(refs, true);
        } else {
            refs.set("panel_alias", format!("{provider}-delegate"));
            refs.set("panel_role", if read { "researcher" } else { "delegate" });
        }
    }
}

impl PanelDispatch {
    fn fields(&self) -> [(&'static str, &'static str, Option<&str>); 11] {
        [
            ("panel_role", "OMS_PANEL_ROLE", self.role.as_deref()),
            (
                "panel_strategy",
                "OMS_PANEL_STRATEGY",
                self.strategy.as_deref(),
            ),
            ("panel_owner", "OMS_PANEL_OWNER", self.owner.as_deref()),
            (
                "panel_purpose",
                "OMS_PANEL_PURPOSE",
                self.purpose.as_deref(),
            ),
            (
                "panel_workload",
                "OMS_PANEL_WORKLOAD",
                self.workload.as_deref(),
            ),
            ("panel_access", "OMS_PANEL_ACCESS", self.access.as_deref()),
            ("panel_label", "OMS_PANEL_LABEL", self.label.as_deref()),
            ("panel_alias", "OMS_PANEL_ALIAS", self.alias.as_deref()),
            (
                "panel_room_participant",
                "OMS_ROOM_PARTICIPANT",
                self.participant.as_deref(),
            ),
            (
                "panel_lease_id",
                "OMS_PLAN_LEASE_ID",
                self.lease_id.as_deref(),
            ),
            (
                "panel_continues",
                "OMS_PANEL_CONTINUES",
                self.continues.as_deref(),
            ),
        ]
    }

    fn apply_refs(&self, refs: &mut V, delegate: bool) {
        for (key, _, value) in self.fields() {
            // Advisory peers historically project only the shared metadata;
            // delegation owns strategy, lease and continuation records.
            if !delegate && matches!(key, "panel_strategy" | "panel_lease_id" | "panel_continues") {
                continue;
            }
            if let Some(value) = value {
                if key == "panel_continues"
                    && !value.strip_prefix("att_").is_some_and(|hex| {
                        !hex.is_empty()
                            && hex
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    })
                {
                    continue;
                }
                refs.set(key, value);
            }
        }
    }
}
