//! Role allocation and runtime account selection. Access is independent of tier.
use crate::value::{array, text};
use crate::{Error, Result, Value, decode, require};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const TIERS: &[&str] = &["light", "routine", "deep", "frontier"];
pub const INDEPENDENT: &[&str] = &["reviewer", "verifier", "security-auditor", "critic"];
pub const WRITERS: &[&str] = &[
    "worker",
    "builder",
    "refactorer",
    "documenter",
    "integrator",
    "performance-engineer",
];
pub const ROLES: &[&str] = &[
    "researcher",
    "scholar",
    "worker",
    "builder",
    "refactorer",
    "documenter",
    "release-engineer",
    "reviewer",
    "verifier",
    "security-auditor",
    "architect",
    "critic",
    "integrator",
    "performance-engineer",
    "advisor",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub provider: String,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub public_only: bool,
    #[serde(default)]
    pub access: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleTier {
    pub tier: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieBand {
    pub gap: u8,
}
impl Default for TieBand {
    fn default() -> Self {
        Self { gap: 15 }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Council {
    pub extra: Vec<Candidate>,
}
fn enabled() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: u64,
    pub main: BTreeMap<String, Candidate>,
    pub tiers: BTreeMap<String, Vec<Candidate>>,
    pub roles: BTreeMap<String, RoleTier>,
    #[serde(default)]
    pub tie_band: TieBand,
    #[serde(default = "enabled")]
    pub auto_review: bool,
    #[serde(default)]
    pub council: Council,
}
impl Policy {
    pub fn load(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::new("routing policy has no parent"))?;
        let root = crate::Store::open(parent)?;
        let bytes = root.read(
            Path::new(
                path.file_name()
                    .ok_or_else(|| Error::new("invalid routing policy path"))?,
            ),
            1024 * 1024,
            false,
        )?;
        Self::parse(&bytes)
    }
    pub fn bundled() -> Result<Self> {
        Self::parse(include_bytes!("../../../config/panel-routing.json"))
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = decode(bytes, 1024 * 1024)?;
        let policy: Self = serde_json::from_value(value)
            .map_err(|_| Error::new("unsupported panel routing policy"))?;
        require(
            policy.schema == 2 && policy.tie_band.gap <= 100,
            "unsupported panel routing policy",
        )?;
        require(
            policy
                .main
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == BTreeSet::from(["codex", "claude"]),
            "invalid main routes",
        )?;
        for (provider, route) in &policy.main {
            valid_route(route)?;
            require(&route.provider == provider, "invalid main provider")?;
        }
        require(
            policy
                .tiers
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == TIERS.iter().copied().collect(),
            "invalid panel tiers",
        )?;
        for routes in policy.tiers.values() {
            require(!routes.is_empty(), "empty panel tier")?;
            let mut providers = BTreeSet::new();
            for route in routes {
                valid_route(route)?;
                require(providers.insert(&route.provider), "duplicate tier provider")?;
                require(
                    route.provider != "antigravity" || route.access.as_deref() == Some("read"),
                    "automatic Antigravity routes must be read-only",
                )?;
            }
        }
        require(
            policy
                .roles
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == ROLES.iter().copied().collect(),
            "roles must map every panel role",
        )?;
        for role in policy.roles.values() {
            require(TIERS.contains(&role.tier.as_str()), "invalid role tier")?;
        }
        let mut extra = BTreeSet::new();
        for route in &policy.council.extra {
            valid_route(route)?;
            require(extra.insert(&route.provider), "duplicate council provider")?;
        }
        Ok(policy)
    }
}
/// Bounded metadata returned by the existing catalog reader. A cached probe
/// or identifier never establishes live model capability, entitlement or price.
#[derive(Clone, Debug, Default)]
pub struct ProviderEvidence {
    pub source: String,
    pub read_at: Option<u64>,
    pub reported_probe: Option<String>,
    pub catalog_age_seconds: Option<u64>,
    pub probe_age_seconds: Option<u64>,
    pub harness_version: Option<String>,
    pub effort_mechanism: Option<String>,
    pub provider_reported_efforts: Vec<String>,
    pub model_effort_scales: BTreeMap<String, Vec<String>>,
    pub capability_evidence: Option<CapabilityEvidence>,
}
/// Optional producer-reported scope and lifetime. This record supplies no
/// endpoint selection, authentication, entitlement or launch authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEvidence {
    pub schema: u64,
    pub source: String,
    pub account_alias: Option<String>,
    pub endpoint: Option<String>,
    pub observed_at: Option<u64>,
    pub expires_at: Option<u64>,
}
impl CapabilityEvidence {
    fn parse(value: &Value) -> Result<Self> {
        let evidence: Self = serde_json::from_value(value.clone())
            .map_err(|_| Error::new("invalid capability evidence"))?;
        require(
            evidence.schema == 1
                && !evidence.source.is_empty()
                && evidence.source.len() <= 256
                && !evidence.source.chars().any(char::is_control),
            "unsupported capability evidence source or schema",
        )?;
        if let Some(account) = &evidence.account_alias {
            require(
                !account.is_empty()
                    && account.len() <= 64
                    && account.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)
                    }),
                "invalid capability account alias",
            )?;
        }
        if let Some(endpoint) = &evidence.endpoint {
            // Endpoint labels/URLs are display identities. Refuse credentials,
            // query strings and fragments rather than persisting private URLs.
            require(
                !endpoint.is_empty()
                    && endpoint.len() <= 256
                    && endpoint
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:/-[]".contains(&b))
                    && !endpoint.starts_with('/')
                    && (!endpoint.contains("://")
                        || ["https://", "http://"].iter().any(|prefix| {
                            endpoint
                                .strip_prefix(prefix)
                                .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
                        })),
                "invalid or private capability endpoint",
            )?;
        }
        require(
            !evidence
                .observed_at
                .zip(evidence.expires_at)
                .is_some_and(|(observed, expiry)| expiry <= observed),
            "capability expiry must follow its observation",
        )?;
        Ok(evidence)
    }
    fn freshness(&self, read_at: Option<u64>) -> CapabilityFreshness {
        match (self.observed_at, self.expires_at, read_at) {
            (Some(observed), Some(expiry), Some(read))
                if self.account_alias.is_some()
                    && self.endpoint.is_some()
                    && observed <= read
                    && observed < expiry =>
            {
                if read < expiry {
                    CapabilityFreshness::ReportedFresh
                } else {
                    CapabilityFreshness::ReportedStale
                }
            }
            _ => CapabilityFreshness::Unknown,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFreshness {
    ReportedFresh,
    ReportedStale,
    #[default]
    Unknown,
}
impl CapabilityFreshness {
    fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}
impl ProviderEvidence {
    pub fn from_catalog_row(row: &Value) -> Result<Self> {
        let bounded_text = |value: Option<&Value>| -> Result<Option<String>> {
            let Some(value) = value.filter(|v| !v.is_null()) else {
                return Ok(None);
            };
            let value = value
                .as_str()
                .ok_or_else(|| Error::new("invalid catalog evidence text"))?;
            require(
                !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
                "catalog evidence text exceeds bounds",
            )?;
            Ok(Some(value.into()))
        };
        let efforts = |value: Option<&Value>| -> Result<Vec<String>> {
            let Some(value) = value.filter(|v| !v.is_null()) else {
                return Ok(vec![]);
            };
            let values = value
                .as_array()
                .ok_or_else(|| Error::new("invalid catalog effort evidence"))?;
            require(values.len() <= 16, "catalog effort evidence exceeds bounds")?;
            values
                .iter()
                .map(|v| bounded_text(Some(v))?.ok_or_else(|| Error::new("invalid catalog effort")))
                .collect()
        };
        let age = |key| -> Result<Option<u64>> {
            row.get(key)
                .filter(|v| !v.is_null())
                .map(|v| {
                    v.as_u64()
                        .ok_or_else(|| Error::new("invalid catalog evidence age"))
                })
                .transpose()
        };
        let mut scales = BTreeMap::new();
        if let Some(value) = row.get("model_effort_scales").filter(|v| !v.is_null()) {
            let values = value
                .as_object()
                .ok_or_else(|| Error::new("invalid model effort scales"))?;
            require(values.len() <= 2048, "model effort scales exceed bounds")?;
            for (model, scale) in values {
                require(
                    !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control),
                    "invalid model effort identity",
                )?;
                scales.insert(model.clone(), efforts(Some(scale))?);
            }
        }
        Ok(Self {
            source: "oms-models".into(),
            read_at: Some(crate::timestamp()),
            reported_probe: bounded_text(row.get("catalog_probe"))?,
            catalog_age_seconds: age("catalog_age_seconds")?,
            probe_age_seconds: age("snapshot_age_seconds")?,
            harness_version: bounded_text(row.get("version"))?,
            effort_mechanism: bounded_text(row.get("effort_mechanism"))?,
            provider_reported_efforts: efforts(row.get("effort_values"))?,
            model_effort_scales: scales,
            capability_evidence: row
                .get("capability_evidence")
                .filter(|v| !v.is_null())
                .map(CapabilityEvidence::parse)
                .transpose()?,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub models_sha256: Option<String>,
    pub count: usize,
    pub resolved_present: Option<bool>,
    pub source: Option<String>,
    /// The current catalog producer supplies no account/endpoint binding.
    pub account_alias: Option<String>,
    pub endpoint: Option<String>,
    pub reported_probe: Option<String>,
    /// Reported file age at read time, not a refresh time or freshness verdict.
    pub catalog_age_seconds: Option<u64>,
    pub read_at: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffortSupport {
    ReportedSupported,
    ReportedUnsupported,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySnapshot {
    pub source: Option<String>,
    pub account_alias: Option<String>,
    pub endpoint: Option<String>,
    pub exact_model_efforts: Option<Vec<String>>,
    pub provider_reported_efforts: Vec<String>,
    pub effort_mechanism: Option<String>,
    pub probe_age_seconds: Option<u64>,
    pub harness_version: Option<String>,
    pub effort_support: EffortSupport,
    /// Freshness applies to the reported scope at selection read time. It does
    /// not establish that the selected route uses that endpoint or account.
    #[serde(default, skip_serializing_if = "CapabilityFreshness::is_unknown")]
    pub freshness: CapabilityFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CapabilityEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_account_matches: Option<bool>,
}
/// Immutable selection evidence, independent of permission and admission. The
/// launch writer retains this snapshot; later policy/catalog changes do not
/// rewrite an in-flight call. Unreported served identity remains null.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteSnapshot {
    pub schema: u64,
    pub policy_schema: Option<u64>,
    pub policy_sha256: Option<String>,
    pub configured: Option<Candidate>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub requested_account: Option<String>,
    pub selected_account: Option<String>,
    pub account_pinned: bool,
    pub served_account: Option<String>,
    pub resolved: Candidate,
    pub served_model: Option<String>,
    pub served_effort: Option<String>,
    pub served_source: Option<String>,
    pub binding_sha256: String,
    pub catalog: CatalogSnapshot,
    pub capabilities: CapabilitySnapshot,
    pub model_family: Option<String>,
    /// Identifier-derived family is diagnostic, never verified served-family
    /// or composite membership evidence.
    pub family_evidence: String,
    /// No existing catalog row proves single/composite execution or fanout.
    pub execution_profile: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    #[serde(flatten)]
    pub candidate: Candidate,
    pub role: String,
    pub kind: String,
    pub owner: String,
    pub workload: String,
    pub access: String,
    pub purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judged_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_reason: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub account_pinned: bool,
    #[serde(default, skip_serializing)]
    pub explicit_target: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<RouteSnapshot>,
}
#[derive(Clone, Debug, Default)]
pub struct RoutingInputs {
    pub installed: Option<BTreeSet<String>>,
    pub catalogs: BTreeMap<String, Vec<String>>,
    pub provider_evidence: BTreeMap<String, ProviderEvidence>,
    pub status: Value,
    pub recent: BTreeMap<String, usize>,
    pub in_flight: BTreeMap<String, usize>,
    pub writer: Option<(String, String)>,
    pub owner_model: Option<String>,
    pub public_repository: bool,
    pub codex_sandbox_blocked: bool,
    pub preferred: Option<String>,
    pub explicit_target: bool,
    pub accounts: Option<Accounts>,
    pub running_accounts: BTreeMap<String, usize>,
    pub requested_account: Option<String>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
}
impl Route {
    fn binding_sha256(&self) -> Result<String> {
        let value = serde_json::json!({"candidate":self.candidate,"role":self.role,"kind":self.kind,"owner":self.owner,"workload":self.workload,"access":self.access,"purpose":self.purpose,"judged_family":self.judged_family,"judge_note":self.judge_note,"account":self.account,"account_pinned":self.account_pinned});
        Ok(crate::digest(&crate::value::encoded(&value)?))
    }
    /// Legacy/manual routes retain explicit unknown provenance. A supplied
    /// snapshot must bind the final route; it cannot survive a model/account/
    /// access change or turn a rank into authority.
    pub fn freeze_snapshot(&mut self) -> Result<()> {
        valid_route(&self.candidate)?;
        if self.snapshot.is_none() {
            self.snapshot = Some(self.selection_snapshot(None, None, None)?);
        }
        let snapshot = self.snapshot.as_ref().expect("snapshot initialized");
        require(
            snapshot.schema == 1
                && snapshot.resolved == self.candidate
                && snapshot.selected_account == self.account
                && snapshot.account_pinned == self.account_pinned
                && snapshot.binding_sha256 == self.binding_sha256()?,
            "route snapshot differs from final route",
        )?;
        require(
            snapshot.capabilities.effort_support != EffortSupport::ReportedUnsupported,
            "resolved model does not support requested effort",
        )?;
        let capability = &snapshot.capabilities;
        if let Some(evidence) = &capability.evidence {
            CapabilityEvidence::parse(&serde_json::to_value(evidence).map_err(Error::new)?)?;
        }
        require(
            capability.freshness
                == capability
                    .evidence
                    .as_ref()
                    .map_or(CapabilityFreshness::Unknown, |evidence| {
                        evidence.freshness(snapshot.catalog.read_at)
                    })
                && capability.selected_account_matches
                    == capability
                        .evidence
                        .as_ref()
                        .and_then(|v| v.account_alias.as_ref())
                        .zip(self.account.as_ref())
                        .map(|(observed, selected)| observed == selected)
                && match &capability.evidence {
                    Some(evidence) => {
                        capability.account_alias == evidence.account_alias
                            && capability.endpoint == evidence.endpoint
                    }
                    None => capability.account_alias.is_none() && capability.endpoint.is_none(),
                },
            "capability freshness or scope differs from its source evidence",
        )?;
        require(
            snapshot.served_model.is_none()
                && snapshot.served_account.is_none()
                && snapshot.served_effort.is_none()
                && snapshot.served_source.is_none(),
            "launch snapshot cannot reuse served observations",
        )?;
        require(
            serde_json::to_vec(snapshot).map_err(Error::new)?.len() <= 32768,
            "route snapshot exceeds bounds",
        )?;
        Ok(())
    }
    pub fn snapshot_sha256(&self) -> Result<Option<String>> {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                let value = serde_json::to_value(snapshot).map_err(Error::new)?;
                Ok(crate::digest(&crate::value::encoded(&value)?))
            })
            .transpose()
    }
    fn selection_snapshot(
        &self,
        policy: Option<&Policy>,
        configured: Option<Candidate>,
        inputs: Option<&RoutingInputs>,
    ) -> Result<RouteSnapshot> {
        let evidence = inputs.and_then(|v| v.provider_evidence.get(&self.candidate.provider));
        let capability = evidence.and_then(|v| v.capability_evidence.as_ref());
        let catalog = inputs.and_then(|v| v.catalogs.get(&self.candidate.provider));
        let exact_efforts = evidence
            .and_then(|e| e.model_effort_scales.get(&self.candidate.model))
            .filter(|v| !v.is_empty())
            .cloned();
        let effort_support = match &exact_efforts {
            Some(efforts) if efforts.contains(&self.candidate.effort) => {
                EffortSupport::ReportedSupported
            }
            Some(_) => EffortSupport::ReportedUnsupported,
            None => EffortSupport::Unknown,
        };
        let model_family = route_family(&self.candidate.provider, &self.candidate.model)
            .and_then(|_| model_family(&self.candidate.provider, &self.candidate.model).ok())
            .map(str::to_owned);
        Ok(RouteSnapshot {
            schema: 1,
            policy_schema: policy.map(|p| p.schema),
            policy_sha256: policy
                .map(|p| -> Result<String> {
                    Ok(crate::digest(&crate::value::encoded(
                        &serde_json::to_value(p).map_err(Error::new)?,
                    )?))
                })
                .transpose()?,
            configured,
            requested_model: inputs.and_then(|v| v.requested_model.clone()),
            requested_effort: inputs.and_then(|v| v.requested_effort.clone()),
            requested_account: inputs.and_then(|v| v.requested_account.clone()),
            selected_account: self.account.clone(),
            account_pinned: self.account_pinned,
            served_account: None,
            resolved: self.candidate.clone(),
            served_model: None,
            served_effort: None,
            served_source: None,
            binding_sha256: self.binding_sha256()?,
            catalog: CatalogSnapshot {
                models_sha256: catalog
                    .map(|v| -> Result<String> {
                        Ok(crate::digest(&crate::value::encoded(
                            &serde_json::to_value(v).map_err(Error::new)?,
                        )?))
                    })
                    .transpose()?,
                count: catalog.map_or(0, Vec::len),
                resolved_present: catalog.map(|v| v.contains(&self.candidate.model)),
                source: evidence.map(|v| v.source.clone()),
                account_alias: None,
                endpoint: None,
                reported_probe: evidence.and_then(|v| v.reported_probe.clone()),
                catalog_age_seconds: evidence.and_then(|v| v.catalog_age_seconds),
                read_at: evidence.and_then(|v| v.read_at),
            },
            capabilities: CapabilitySnapshot {
                source: evidence.map(|v| v.source.clone()),
                account_alias: capability.and_then(|v| v.account_alias.clone()),
                endpoint: capability.and_then(|v| v.endpoint.clone()),
                exact_model_efforts: exact_efforts,
                provider_reported_efforts: evidence
                    .map(|v| v.provider_reported_efforts.clone())
                    .unwrap_or_default(),
                effort_mechanism: evidence.and_then(|v| v.effort_mechanism.clone()),
                probe_age_seconds: evidence.and_then(|v| v.probe_age_seconds),
                harness_version: evidence.and_then(|v| v.harness_version.clone()),
                effort_support,
                freshness: capability.map_or(CapabilityFreshness::Unknown, |v| {
                    v.freshness(evidence.and_then(|v| v.read_at))
                }),
                evidence: capability.cloned(),
                selected_account_matches: capability
                    .and_then(|v| v.account_alias.as_ref())
                    .zip(self.account.as_ref())
                    .map(|(observed, selected)| observed == selected),
            },
            family_evidence: if model_family.is_some() {
                "identifier_derived_unverified"
            } else {
                "unknown"
            }
            .into(),
            model_family,
            execution_profile: None,
        })
    }
}
pub fn kind(role: &str) -> Result<&'static str> {
    match role {
        "worker"
        | "builder"
        | "refactorer"
        | "documenter"
        | "release-engineer"
        | "integrator"
        | "performance-engineer" => Ok("worker"),
        "researcher" | "scholar" => Ok("researcher"),
        "advisor" | "architect" | "critic" => Ok("advisor"),
        "reviewer" | "verifier" | "security-auditor" => Ok("reviewer"),
        _ => Err(Error::new("unsupported panel role")),
    }
}
pub fn route_family(provider: &str, model: &str) -> Option<(String, Vec<u64>)> {
    let pattern = match provider {
        "codex" => r"^gpt-(\d+(?:\.\d+)?)-(astra|sol|terra|luna)$",
        "claude" => r"^claude-(opus|sonnet|fable|haiku)-(\d+(?:-\d+)*)$",
        "antigravity" => {
            r"^gemini-(\d+(?:\.\d+)?)-([a-z][a-z0-9]*(?:-[a-z0-9]+)*)-(low|medium|high)$"
        }
        _ => return None,
    };
    let regex = regex::Regex::new(pattern).ok()?;
    let found = regex.captures(model)?;
    let (family, number) = if provider == "claude" {
        (found[1].to_owned(), &found[2])
    } else if provider == "antigravity" {
        (format!("{}-{}", &found[2], &found[3]), &found[1])
    } else {
        (found[2].to_owned(), &found[1])
    };
    let mut version: Vec<u64> = number
        .split(['.', '-'])
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    while version.len() > 1 && version.last() == Some(&0) {
        version.pop();
    }
    Some((family, version))
}
pub fn model_family(provider: &str, model: &str) -> Result<&'static str> {
    let lower = model.to_ascii_lowercase();
    if lower.starts_with("orchestrator") || lower.starts_with("gemini ") {
        return Ok("google");
    }
    if lower.starts_with("claude ") {
        return Ok("anthropic");
    }
    if lower.starts_with("gpt ") {
        return Ok("openai");
    }
    let base = model.rsplit('/').next().unwrap_or(model);
    if base.starts_with("gpt-") || base.starts_with("o3") || base.starts_with("o4") {
        return Ok("openai");
    }
    if base.starts_with("claude-") {
        return Ok("anthropic");
    }
    if base.starts_with("gemini-") {
        return Ok("google");
    }
    if model.is_empty() {
        return match provider {
            "codex" => Ok("openai"),
            "claude" => Ok("anthropic"),
            _ => Err(Error::new("model family unverified")),
        };
    }
    Err(Error::new("model family unverified"))
}
pub fn valid_route(route: &Candidate) -> Result<()> {
    require(
        ["low", "medium", "high"].contains(&route.effort.as_str())
            && route
                .access
                .as_deref()
                .is_none_or(|v| ["read", "write"].contains(&v)),
        "invalid panel route effort/access",
    )?;
    if ["pi", "opencode"].contains(&route.provider.as_str()) {
        let pieces: Vec<_> = route.model.splitn(2, '/').collect();
        return require(
            pieces.len() == 2
                && !pieces[0].is_empty()
                && !pieces[1].is_empty()
                && !route.model.starts_with('-')
                && !route.model.chars().any(|c| c.is_whitespace() || c == '\0'),
            "invalid exact model route",
        );
    }
    require(
        route_family(&route.provider, &route.model).is_some(),
        "role route requires a versioned model family",
    )?;
    if route.provider == "codex" {
        require(
            route_family("codex", &route.model).unwrap().1[0] >= 6,
            "Codex generations before 6 are not routed",
        )?;
    }
    Ok(())
}
pub fn routable_route_model(
    provider: &str,
    configured: &str,
    catalog: &[String],
) -> Result<String> {
    if ["pi", "opencode"].contains(&provider) {
        require(
            catalog.is_empty() || catalog.iter().any(|v| v == configured),
            "exact model is absent from catalog",
        )?;
        return Ok(configured.into());
    }
    let (family, _) = route_family(provider, configured)
        .ok_or_else(|| Error::new("versioned model family required"))?;
    let families: Vec<_> = catalog
        .iter()
        .filter_map(|v| route_family(provider, v).map(|f| (v, f)))
        .collect();
    let mut candidates: Vec<_> = families.iter().filter(|(_, f)| f.0 == family).collect();
    if provider == "antigravity" {
        if candidates.is_empty() {
            let level = family.rsplit('-').next();
            candidates = families
                .iter()
                .filter(|(_, f)| f.0.rsplit('-').next() == level)
                .collect();
            require(
                candidates
                    .iter()
                    .map(|(_, f)| &f.0)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == 1,
                "configured family absent from routable catalog",
            )?;
        }
        return candidates
            .into_iter()
            .max_by(|a, b| a.1.1.cmp(&b.1.1).then(a.0.cmp(b.0)))
            .map(|(v, _)| (*v).clone())
            .ok_or_else(|| Error::new("Antigravity requires a routable catalog"));
    }
    let mut choice = configured.to_owned();
    for (v, f) in candidates {
        if f.1 > route_family(provider, &choice).unwrap().1 {
            choice = (*v).clone();
        }
    }
    if provider == "codex" {
        let newest = families.iter().map(|(_, f)| f.1[0]).max().unwrap_or(6);
        require(
            route_family(provider, &choice).unwrap().1[0] >= newest,
            "Codex route is a major generation behind catalog",
        )?;
    }
    Ok(choice)
}

pub fn allocate(
    policy: &Policy,
    owner: &str,
    role: &str,
    tier: Option<&str>,
    access: &str,
    purpose: Option<&str>,
    inputs: &RoutingInputs,
) -> Result<Route> {
    allocate_with_account_rank(policy, owner, role, tier, access, purpose, inputs)
        .map(|(route, _)| route)
}

/// The preview rank comes from the same comparison that selects the proposed
/// account. It stays outside the persisted route and selection snapshot.
pub fn allocate_with_account_rank(
    policy: &Policy,
    owner: &str,
    role: &str,
    tier: Option<&str>,
    access: &str,
    purpose: Option<&str>,
    inputs: &RoutingInputs,
) -> Result<(Route, Option<AccountRank>)> {
    crate::identifier(owner)?;
    let board_kind = kind(role)?;
    let workload = tier.unwrap_or(
        policy
            .roles
            .get(role)
            .ok_or_else(|| Error::new("unsupported role"))?
            .tier
            .as_str(),
    );
    require(
        TIERS.contains(&workload) && ["read", "write"].contains(&access),
        "invalid workload/access",
    )?;
    require(
        access != "write" || WRITERS.contains(&role),
        "role is read-only",
    )?;
    let purpose = if role == "worker" {
        let p = purpose.unwrap_or(if access == "write" {
            "implement"
        } else {
            "explain"
        });
        require(
            ["explain", "investigate", "implement"].contains(&p)
                && (p != "implement" || access == "write")
                && (p != "explain" || access == "read"),
            "worker purpose must match access",
        )?;
        p.to_owned()
    } else {
        require(purpose.is_none(), "purpose applies only to plain workers")?;
        match role {
            "advisor" => "advise",
            "reviewer" => "review",
            "researcher" | "scholar" => "research",
            r => r,
        }
        .into()
    };
    require(
        role != "researcher" || workload == "light",
        "researchers use the light tier",
    )?;
    if let Some(effort) = &inputs.requested_effort {
        require(
            ["low", "medium", "high"].contains(&effort.as_str()),
            "invalid requested route effort",
        )?;
        require(
            role != "researcher" || effort == "low",
            "researcher effort must remain low",
        )?;
    }
    let independent = INDEPENDENT.contains(&role);
    let judged_family = if independent {
        let (provider, model) = inputs
            .writer
            .as_ref()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .unwrap_or((owner, inputs.owner_model.as_deref().unwrap_or_default()));
        Some(model_family(provider, model)?.to_owned())
    } else {
        None
    };
    let mut candidates = policy
        .tiers
        .get(workload)
        .ok_or_else(|| Error::new("tier missing"))?
        .clone();
    if independent && workload == "routine" {
        candidates.extend(
            policy.tiers["deep"]
                .iter()
                .filter(|v| v.provider == "antigravity")
                .cloned(),
        );
    }
    let configured = candidates.clone();
    if let Some(effort) = &inputs.requested_effort {
        for candidate in &mut candidates {
            candidate.effort.clone_from(effort);
        }
    }
    if let Some(req) = &inputs.requested_model {
        for c in &mut candidates {
            c.model.clone_from(req);
        }
        if let Some(pref) = &inputs.preferred
            && let Some(c) = candidates.iter().find(|c| &c.provider == pref)
        {
            valid_route(c)?;
        }
        candidates.retain(|c| valid_route(c).is_ok());
    }
    candidates.retain(|c| {
        (!inputs.explicit_target || inputs.preferred.as_ref().is_none_or(|p| &c.provider == p))
            && (inputs.explicit_target
                || role == "security-auditor"
                || c.model != "claude-fable-5-1")
            && (access != "write" || c.access.as_deref() != Some("read"))
            && (!(independent || board_kind == "advisor")
                || ["codex", "claude", "antigravity"].contains(&c.provider.as_str()))
            && (role != "researcher"
                || route_family(&c.provider, &c.model)
                    .is_some_and(|f| ["luna", "haiku"].contains(&f.0.as_str())))
            && (inputs.explicit_target
                || judged_family
                    .as_deref()
                    .is_none_or(|f| model_family(&c.provider, &c.model).ok() != Some(f)))
            && inputs
                .installed
                .as_ref()
                .is_none_or(|v| v.contains(&c.provider))
            && (!c.public_only || inputs.public_repository)
            && !(access == "write" && c.provider == "codex" && inputs.codex_sandbox_blocked)
            && !daily_capped(&inputs.status, c)
    });
    let alternatives = candidates.len() > 1 || judged_family.is_some();
    let mut eligible = Vec::new();
    let now = crate::timestamp();
    for mut c in candidates {
        if inputs.requested_model.is_none() {
            let catalog = inputs
                .catalogs
                .get(&c.provider)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let Ok(model) = routable_route_model(&c.provider, &c.model, catalog) else {
                continue;
            };
            c.model = model;
        }
        if valid_route(&c).is_err() || daily_capped(&inputs.status, &c) {
            continue;
        }
        // Apply eligibility to the resolved identity as well. A new catalog
        // revision of Fable cannot escape the existing explicit-use restriction.
        if !(inputs.explicit_target || role == "security-auditor")
            && route_family(&c.provider, &c.model)
                .is_some_and(|f| c.provider == "claude" && f.0 == "fable")
        {
            continue;
        }
        if inputs
            .provider_evidence
            .get(&c.provider)
            .and_then(|e| e.model_effort_scales.get(&c.model))
            .filter(|efforts| !efforts.is_empty())
            .is_some_and(|efforts| !efforts.contains(&c.effort))
        {
            continue;
        }
        let mut account = None;
        let mut account_reason = None;
        let mut account_rank = None;
        let mut used = weekly_used_at(&inputs.status, &c.provider, now);
        if inputs.requested_account.is_some()
            && !inputs
                .accounts
                .as_ref()
                .is_some_and(|a| a.provider(&c.provider).is_some())
        {
            continue;
        }
        if let Some(accounts) = &inputs.accounts
            && accounts.provider(&c.provider).is_some()
        {
            let readings = account_readings(&inputs.status, &c.provider, accounts, now);
            match accounts.select_ranked(
                &c.provider,
                &c.model,
                &readings,
                &inputs.running_accounts,
                inputs.explicit_target,
                inputs.requested_account.as_deref(),
                now,
            ) {
                Ok((entry, reason, rank)) => {
                    used = account_weekly_used(&readings[&entry.id], now);
                    account = Some(entry.id);
                    account_reason = Some(reason);
                    account_rank = rank;
                }
                Err(_) => continue,
            }
        } else if let Some(accounts) = &inputs.accounts
            && ACCOUNT_PROVIDERS.contains(&c.provider.as_str())
            && active_account_home(&c.provider)
                .is_some_and(|home| accounts.home_claimed_by_other(&c.provider, &home))
        {
            continue;
        }
        if alternatives
            && !inputs.explicit_target
            && inputs.requested_account.is_none()
            && !["pi", "opencode", "antigravity"].contains(&c.provider.as_str())
            && used.is_some_and(|v| v >= 90)
        {
            continue;
        }
        eligible.push((c, used, account, account_reason, account_rank));
    }
    require(
        !eligible.is_empty(),
        if independent {
            "independent review unavailable for judged family"
        } else {
            "no eligible installed provider for tier"
        },
    )?;
    let kept: Vec<_> = eligible
        .iter()
        .filter(|(c, _, _, _, _)| {
            c.provider != "pi" || inputs.status["pi"]["cooldown"].get(&c.model).is_none()
        })
        .cloned()
        .collect();
    if !kept.is_empty() {
        eligible = kept;
    }
    let readings: Vec<_> = eligible
        .iter()
        .filter(|(c, _, _, _, _)| ["codex", "claude"].contains(&c.provider.as_str()))
        .map(|v| v.1)
        .collect();
    let all_known = readings.iter().all(Option::is_some);
    let spread = readings
        .iter()
        .flatten()
        .max()
        .zip(readings.iter().flatten().min())
        .map(|(a, b)| a - b)
        .unwrap_or(0);
    let tied = spread < policy.tie_band.gap;
    let mut native: Vec<_> = eligible
        .iter()
        .filter(|(c, _, _, _, _)| ["codex", "claude"].contains(&c.provider.as_str()))
        .cloned()
        .collect();
    native.sort_by_key(|(c, used, _, _, _)| {
        if all_known && !tied {
            (usize::from(used.unwrap_or(100)), 0, false)
        } else {
            (
                *inputs.in_flight.get(&c.provider).unwrap_or(&0),
                *inputs.recent.get(&c.provider).unwrap_or(&0),
                c.provider == owner,
            )
        }
    });
    let mut ordered_native = native.into_iter();
    for entry in &mut eligible {
        if ["codex", "claude"].contains(&entry.0.provider.as_str()) {
            *entry = ordered_native.next().unwrap();
        }
    }
    if let Some(index) = eligible.iter().position(|v| v.0.provider == "antigravity") {
        let entry = eligible.remove(index);
        let after_known = eligible
            .iter()
            .rposition(|(c, u, _, _, _)| {
                ["codex", "claude"].contains(&c.provider.as_str()) && u.is_some()
            })
            .map(|v| v + 1)
            .unwrap_or(0);
        eligible.insert(index.max(after_known).min(eligible.len()), entry);
    }
    if access == "write" {
        eligible.sort_by_key(|v| ["pi", "opencode"].contains(&v.0.provider.as_str()));
    }
    if role == "worker"
        && access == "read"
        && purpose == "investigate"
        && eligible.iter().any(|v| v.0.provider != "pi")
    {
        eligible.retain(|v| v.0.provider != "pi");
    }
    if independent
        && judged_family
            .as_deref()
            .is_some_and(|v| ["openai", "anthropic"].contains(&v))
    {
        eligible.sort_by_key(|v| v.0.provider != "antigravity");
    }
    if !independent && let Some(preferred) = &inputs.preferred {
        eligible.sort_by_key(|v| &v.0.provider != preferred);
    }
    let (mut candidate, _, account, account_reason, account_rank) = eligible.remove(0);
    if role == "researcher" {
        candidate.effort = "low".into();
    }
    let judge_note = if independent
        && inputs.explicit_target
        && model_family(&candidate.provider, &candidate.model).ok() == judged_family.as_deref()
    {
        Some("same-family judge (owner-pinned)".into())
    } else {
        None
    };
    let account_pinned = inputs.requested_account.is_some() && account.is_some();
    let configured = configured
        .into_iter()
        .find(|v| v.provider == candidate.provider);
    let mut route = Route {
        candidate,
        role: role.into(),
        kind: board_kind.into(),
        owner: owner.into(),
        workload: workload.into(),
        access: access.into(),
        purpose,
        judged_family,
        judge_note,
        account,
        account_reason,
        account_pinned,
        explicit_target: inputs.explicit_target,
        snapshot: None,
    };
    route.snapshot = Some(route.selection_snapshot(Some(policy), configured, Some(inputs))?);
    route.freeze_snapshot()?;
    Ok((route, account_rank))
}
/// Providers whose provider-level reading comes from the active account home.
/// Claude's row comes from the room main's HUD session, whose home is unproven.
const ACTIVE_HOME_READINGS: &[&str] = &["codex"];
/// Per-account readings, with a fresh provider-level reading lent only to the
/// configured account labelled as the active home when its own is unusable.
fn account_readings(status: &Value, provider: &str, accounts: &Accounts, now: u64) -> Value {
    let row = &status[provider];
    let mut readings = row["accounts"].clone();
    let Some(label) = row["account_label"].as_str().filter(|label| {
        ACTIVE_HOME_READINGS.contains(&provider)
            && accounts
                .provider(provider)
                .is_some_and(|c| c.accounts.iter().any(|e| e.id == *label))
            && account_weekly_used(&readings[*label], now).is_none()
            && row.get("observed_at").is_some_and(|v| !v.is_null())
            && weekly_used_at(status, provider, now).is_some()
            && (readings.is_null() || readings.is_object())
    }) else {
        return readings;
    };
    let mut lent = serde_json::json!({
        "weekly_used": row["weekly_used"],
        "observed_at": row["observed_at"],
    });
    if let Some(reset) = weekly_reset(row) {
        lent["resets_at"] = reset.clone();
    }
    if account_weekly_used(&lent, now).is_some() {
        readings[label] = lent;
    }
    readings
}
pub fn weekly_used(status: &Value, provider: &str) -> Option<u8> {
    weekly_used_at(status, provider, crate::timestamp())
}

fn weekly_used_at(status: &Value, provider: &str, now: u64) -> Option<u8> {
    let row = &status[provider];
    let max_age = if matches!(provider, "claude" | "antigravity") {
        crate::metrics::READING_FRESH_SECONDS
    } else {
        ACCOUNT_QUOTA_MAX_AGE
    };
    let observed = [
        "weekly_lead_observed_at",
        "weekly_native_observed_at",
        "observed_at",
        "timestamp",
    ]
    .into_iter()
    .find_map(|key| row.get(key).filter(|value| !value.is_null()));
    checked_weekly_used(row, now, max_age, observed)
}
fn daily_capped(status: &Value, c: &Candidate) -> bool {
    status[&c.provider]["daily_cap"][&c.model]
        .as_str()
        .is_some_and(|v| crate::now().starts_with(v) && v.len() == 10)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub id: String,
    pub home: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccounts {
    #[serde(default = "ordered")]
    pub policy: String,
    pub accounts: Vec<Account>,
}

#[derive(Clone, Copy, Debug)]
pub struct AccountRank {
    pub policy: &'static str,
    pub eligible_accounts: usize,
    pub weekly_bucket: Option<u8>,
    pub next_weekly_bucket: Option<u8>,
    pub working_attempts: Option<usize>,
    pub next_working_attempts: Option<usize>,
    pub decided_by: &'static str,
}
fn ordered() -> String {
    "ordered".into()
}
/// Account-isolated providers: each has a proven per-account home variable.
pub const ACCOUNT_PROVIDERS: &[&str] = &["codex", "claude", "pi"];
/// Home variable and default home (relative to `$HOME`) for an account provider.
pub fn account_home_env(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "codex" => Some(("CODEX_HOME", ".codex")),
        "claude" => Some(("CLAUDE_CONFIG_DIR", ".claude")),
        "pi" => Some(("PI_CODING_AGENT_DIR", ".pi/agent")),
        _ => None,
    }
}
/// The provider's currently active home (environment override or default).
pub fn active_account_home(provider: &str) -> Option<PathBuf> {
    let (var, default) = account_home_env(provider)?;
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(default)))
}
fn standard_account_home(provider: &str) -> Option<PathBuf> {
    let (_, default) = account_home_env(provider)?;
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(default))
}
pub(crate) fn private_home(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().is_some_and(|m| {
        m.is_dir() && m.uid() == rustix::process::getuid().as_raw() && m.mode() & 0o022 == 0
    })
}
#[derive(Clone, Debug, Default)]
pub struct Accounts {
    pub providers: BTreeMap<String, ProviderAccounts>,
}
fn parse_provider(
    settings: &Value,
    homes: &mut BTreeSet<PathBuf>,
    implicit_default: Option<PathBuf>,
) -> Result<ProviderAccounts> {
    let mut settings = settings.clone();
    if settings.get("policy").is_none() {
        settings["policy"] = "ordered".into();
    }
    let mut account: ProviderAccounts =
        serde_json::from_value(settings).map_err(|_| Error::new("invalid provider accounts"))?;
    require(
        ["ordered", "spread"].contains(&account.policy.as_str()),
        "invalid provider account policy/list",
    )?;
    let mut ids = BTreeSet::new();
    let implicit_home = if let Some(home) = implicit_default {
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        ids.insert("default".into());
        require(homes.insert(home.clone()), "duplicate account home")?;
        Some(home)
    } else {
        None
    };
    for entry in &mut account.accounts {
        require(
            !entry.id.is_empty()
                && entry.id.len() <= 64
                && (entry.id.as_bytes()[0].is_ascii_lowercase()
                    || entry.id.as_bytes()[0].is_ascii_digit())
                && entry
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
                && ids.insert(entry.id.clone()),
            "invalid or duplicate account id",
        )?;
        require(entry.home.is_absolute(), "account home must be absolute")?;
        if let Ok(home) = std::fs::canonicalize(&entry.home) {
            entry.home = home;
        }
        require(homes.insert(entry.home.clone()), "duplicate account home")?;
    }
    if let Some(home) = implicit_home
        && private_home(&home)
    {
        account.accounts.insert(
            0,
            Account {
                id: "default".into(),
                home,
            },
        );
    }
    Ok(account)
}
impl Accounts {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        require(
            path.is_absolute(),
            "provider accounts path must be absolute",
        )?;
        let root = crate::Store::open(
            path.parent()
                .ok_or_else(|| Error::new("account path parent unavailable"))?,
        )?;
        let value = root.json(Path::new(path.file_name().unwrap()), 1024 * 1024, true)?;
        let mut sections: Vec<(String, Value, bool)> = Vec::new();
        if let Some(value) = value {
            require(
                value["schema"].as_u64() == Some(1)
                    && value.as_object().is_some_and(|v| {
                        v.keys()
                            .all(|k| ["schema", "providers"].contains(&k.as_str()))
                    })
                    && value["providers"].is_object(),
                "invalid provider accounts schema",
            )?;
            for (name, settings) in value["providers"].as_object().unwrap() {
                require(
                    ACCOUNT_PROVIDERS.contains(&name.as_str()),
                    "provider account isolation is unproved",
                )?;
                sections.push((name.clone(), settings.clone(), false));
            }
        }
        if !sections.iter().any(|(n, _, _)| n == "claude")
            && let Some(file) = root.json(
                Path::new("provider-accounts.claude.json"),
                1024 * 1024,
                true,
            )?
        {
            require(
                file["schema"].as_u64() == Some(1)
                    && file["provider"] == "claude"
                    && file.as_object().is_some_and(|v| {
                        v.keys().all(|k| {
                            ["schema", "provider", "accounts", "policy"].contains(&k.as_str())
                        })
                    })
                    && file["accounts"].is_array(),
                "invalid Claude provider accounts schema",
            )?;
            if !array(&file["accounts"]).is_empty() {
                let mut settings = serde_json::json!({"accounts": file["accounts"]});
                if let Some(policy) = file.get("policy") {
                    settings["policy"] = policy.clone();
                }
                sections.push(("claude".into(), settings, true));
            }
        }
        if sections.is_empty() {
            return Ok(None);
        }
        let mut homes = BTreeSet::new();
        let mut out = Self::default();
        for (name, settings, implicit) in sections {
            let default = if implicit {
                Some(
                    standard_account_home(&name)
                        .filter(|p| p.is_absolute())
                        .ok_or_else(|| Error::new("home unavailable"))?,
                )
            } else {
                None
            };
            let parsed = parse_provider(&settings, &mut homes, default)?;
            require(
                !parsed.accounts.is_empty(),
                "invalid provider account policy/list",
            )?;
            out.providers.insert(name, parsed);
        }
        Ok(Some(out))
    }
    pub fn configured() -> Result<Option<Self>> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Error::new("home unavailable"))?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        Self::load(&config.join("oh-my-setting/provider-accounts.json"))
    }
    pub fn provider(&self, provider: &str) -> Option<&ProviderAccounts> {
        self.providers.get(provider)
    }
    fn codex_serves_model(home: &Path, model: &str) -> Result<bool> {
        let root = crate::Store::open(home)?;
        if let Some(models) = root.json(Path::new("models_cache.json"), 4 * 1024 * 1024, true)? {
            let models: Vec<_> = array(&models["models"])
                .iter()
                .map(|v| text(v, "slug"))
                .collect();
            if model != "provider-default"
                && !models.is_empty()
                && !models.iter().any(|v| v == model)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Pick an account for `provider`. `requested` pins one account (validated,
    /// quota ignored); otherwise rank by bucketed weekly use, then running
    /// count (spread policy, unpinned), then registry order.
    pub fn select(
        &self,
        provider: &str,
        model: &str,
        readings: &Value,
        running: &BTreeMap<String, usize>,
        explicit: bool,
        requested: Option<&str>,
    ) -> Result<(Account, String)> {
        self.select_ranked(
            provider,
            model,
            readings,
            running,
            explicit,
            requested,
            crate::timestamp(),
        )
        .map(|(account, reason, _)| (account, reason))
    }

    #[allow(clippy::too_many_arguments)]
    fn select_ranked(
        &self,
        provider: &str,
        model: &str,
        readings: &Value,
        running: &BTreeMap<String, usize>,
        explicit: bool,
        requested: Option<&str>,
        now: u64,
    ) -> Result<(Account, String, Option<AccountRank>)> {
        let config = self
            .providers
            .get(provider)
            .ok_or_else(|| Error::new("no accounts configured for provider"))?;
        let mut eligible = Vec::new();
        let spread = config.policy == "spread" && requested.is_none();
        for (index, entry) in config.accounts.iter().enumerate() {
            if requested.is_some_and(|r| r != entry.id) || !private_home(&entry.home) {
                continue;
            }
            let used = account_weekly_used(&readings[&entry.id], now);
            if used.is_some_and(|v| v >= 90) && !explicit && requested.is_none() {
                continue;
            }
            if provider == "codex" && !Self::codex_serves_model(&entry.home, model)? {
                continue;
            }
            let bucket = used.map(|v| v / 10 * 10);
            let load = running
                .get(&format!("{provider}:{}", entry.id))
                .or_else(|| running.get(&entry.id))
                .copied()
                .unwrap_or(0);
            eligible.push((
                bucket.unwrap_or(100),
                if spread { load } else { 0 },
                index,
                entry,
                bucket,
            ));
        }
        eligible.sort_by_key(|v| (v.0, v.1, v.2));
        if let Some((quota, load, _, entry, bucket)) = eligible.first() {
            let next = eligible.get(1);
            let decided_by = match next {
                _ if requested.is_some() => "requested_account",
                None => "only_eligible",
                Some(next) if next.0 != *quota => "weekly_bucket",
                Some(next) if next.1 != *load => "working_attempts",
                _ => "registry_order",
            };
            return Ok((
                (*entry).clone(),
                if requested.is_some() {
                    "requested account".into()
                } else if explicit {
                    "explicit target".into()
                } else {
                    config.policy.clone()
                },
                Some(AccountRank {
                    policy: if config.policy == "spread" {
                        "spread"
                    } else {
                        "ordered"
                    },
                    eligible_accounts: eligible.len(),
                    weekly_bucket: *bucket,
                    next_weekly_bucket: next.and_then(|next| next.4),
                    working_attempts: spread.then_some(*load),
                    next_working_attempts: next.filter(|_| spread).map(|next| next.1),
                    decided_by,
                }),
            ));
        }
        let has_default = config.accounts.iter().any(|e| e.id == "default");
        if has_default
            || requested.is_some() && requested != Some("default")
            || requested.is_none() && !explicit
        {
            return Err(Error::new(if requested.is_some() {
                "requested account unavailable"
            } else {
                "no eligible provider account"
            }));
        }
        let home = if requested == Some("default") {
            standard_account_home(provider)
        } else {
            active_account_home(provider)
        }
        .unwrap_or_default();
        require(
            home.is_absolute() && private_home(&home),
            "default current account home unavailable",
        )?;
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        if self.home_claimed_by_other(provider, &home) {
            return Err(Error::new(if requested.is_some() {
                "requested account unavailable"
            } else {
                "no eligible provider account"
            }));
        }
        if provider == "codex" && !Self::codex_serves_model(&home, model)? {
            return Err(Error::new(if requested.is_some() {
                "requested account unavailable"
            } else {
                "no eligible provider account"
            }));
        }
        Ok((
            Account {
                id: "default".into(),
                home,
            },
            if requested.is_some() {
                "requested account; default home".into()
            } else {
                "explicit target; default current home".into()
            },
            None,
        ))
    }
    pub fn home_claimed_by_other(&self, provider: &str, candidate_home: &Path) -> bool {
        let candidate =
            std::fs::canonicalize(candidate_home).unwrap_or_else(|_| candidate_home.to_path_buf());
        self.providers.iter().any(|(other, cfg)| {
            other != provider
                && cfg.accounts.iter().any(|entry| {
                    std::fs::canonicalize(&entry.home).unwrap_or_else(|_| entry.home.clone())
                        == candidate
                })
        })
    }
    pub fn home(&self, provider: &str, ident: &str) -> Result<&Path> {
        self.providers
            .get(provider)
            .and_then(|c| c.accounts.iter().find(|v| v.id == ident))
            .map(|v| v.home.as_path())
            .ok_or_else(|| Error::new("unknown provider account"))
    }
}
const ACCOUNT_QUOTA_MAX_AGE: f64 = 7.0 * 24.0 * 60.0 * 60.0;

fn weekly_reset(row: &Value) -> Option<&Value> {
    ["weekly_resets_at", "resets_at"]
        .into_iter()
        .find_map(|key| row.get(key).filter(|value| !value.is_null()))
}

fn checked_weekly_used(
    row: &Value,
    now: u64,
    max_age: f64,
    observed: Option<&Value>,
) -> Option<u8> {
    let now = now as f64;
    if row["source"] == "cache stale"
        || weekly_reset(row)
            .is_some_and(|reset| crate::metrics::stamp(reset).is_none_or(|reset| reset <= now))
    {
        return None;
    }
    if let Some(observed) = observed {
        let stamp = crate::metrics::stamp(observed)?;
        if stamp < 0.0 || stamp > now || now - stamp > max_age {
            return None;
        }
    }
    crate::metrics::percent(&row["weekly_used"])
}

pub fn account_weekly_used(row: &Value, now: u64) -> Option<u8> {
    checked_weekly_used(
        row,
        now,
        ACCOUNT_QUOTA_MAX_AGE,
        row.get("observed_at").or_else(|| row.get("timestamp")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn weekly_ranking_excludes_expired_and_unfresh_observations() {
        let now = 1_700_000_000;
        let base = json!({
            "weekly_used": 0, "weekly_resets_at": now + 300,
            "observed_at": now - 600, "source": "HUD cache"
        });
        let claude = |row: &Value| weekly_used_at(&json!({"claude":row}), "claude", now);
        assert_eq!(claude(&base), Some(0));
        for (key, value) in [
            ("weekly_resets_at", json!(now)),
            ("weekly_resets_at", json!("")),
            ("observed_at", json!(now - 601)),
            ("observed_at", json!(now + 1)),
            ("source", json!("cache stale")),
        ] {
            let mut row = base.clone();
            row[key] = value;
            assert_eq!(claude(&row), None, "{row}");
        }
        let mut agy = base;
        agy["weekly_native_observed_at"] = json!(now - 601);
        assert_eq!(
            weekly_used_at(&json!({"antigravity":agy}), "antigravity", now),
            None
        );
        let expires = chrono::DateTime::from_timestamp(i64::try_from(now + 120).unwrap(), 0)
            .unwrap()
            .to_rfc3339();
        assert_eq!(
            account_weekly_used(
                &json!({
                    "weekly_used":0,"observed_at":now - 60,"resets_at":expires
                }),
                now
            ),
            Some(0)
        );
        assert_eq!(
            account_weekly_used(
                &json!({
                    "weekly_used":0,"observed_at":now - 60,"weekly_resets_at":null,"resets_at":now
                }),
                now
            ),
            None
        );
    }

    fn claude_inputs() -> RoutingInputs {
        RoutingInputs {
            installed: Some(["claude".into()].into()),
            catalogs: BTreeMap::from([("claude".into(), vec!["claude-sonnet-5-6".into()])]),
            ..Default::default()
        }
    }
    #[test]
    fn explicit_reviewer_provider_is_required_and_unpinned_routing_stays_independent() {
        let policy = Policy::bundled().unwrap();
        let mut inputs = RoutingInputs {
            installed: Some(["claude".into(), "codex".into()].into()),
            catalogs: BTreeMap::from([
                ("claude".into(), vec!["claude-opus-5-5".into()]),
                ("codex".into(), vec!["gpt-6.1-sol".into()]),
            ]),
            preferred: Some("codex".into()),
            explicit_target: true,
            ..Default::default()
        };
        let allocate_reviewer = |inputs: &RoutingInputs| {
            allocate(&policy, "codex", "reviewer", None, "read", None, inputs)
        };
        assert_eq!(
            allocate_reviewer(&inputs).unwrap().candidate.provider,
            "codex"
        );

        let home = std::env::temp_dir().join(format!("oms-routing-account-{}", std::process::id()));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        inputs.accounts = Some(Accounts {
            providers: BTreeMap::from([(
                "codex".into(),
                ProviderAccounts {
                    policy: "ordered".into(),
                    accounts: vec![Account {
                        id: "snu".into(),
                        home: home.clone(),
                    }],
                },
            )]),
        });
        inputs.requested_account = Some("snu".into());
        let route = allocate_reviewer(&inputs).unwrap();
        assert_eq!(route.candidate.provider, "codex");
        assert_eq!(route.account.as_deref(), Some("snu"));
        assert!(route.account_pinned);
        inputs.requested_account = None;
        inputs.accounts = None;
        std::fs::remove_dir(&home).unwrap();

        inputs.installed = Some(["claude".into()].into());
        assert!(allocate_reviewer(&inputs).is_err());
        inputs.installed = Some(["claude".into(), "codex".into()].into());
        inputs.provider_evidence.insert(
            "codex".into(),
            ProviderEvidence::from_catalog_row(&json!({
                "model_effort_scales": {"gpt-6.1-sol": ["low"]}
            }))
            .unwrap(),
        );
        assert!(allocate_reviewer(&inputs).is_err());
        inputs.provider_evidence.clear();
        inputs.explicit_target = false;
        inputs.preferred = None;
        assert_eq!(
            allocate_reviewer(&inputs).unwrap().candidate.provider,
            "claude"
        );
        inputs.preferred = Some("codex".into());
        assert_eq!(
            allocate_reviewer(&inputs).unwrap().candidate.provider,
            "claude"
        );
        assert!(allocate(&policy, "codex", "reviewer", None, "write", None, &inputs).is_err());
    }
    #[test]
    fn schema_two_remains_strict_and_snapshots_separate_model_identities() {
        let policy = Policy::bundled().unwrap();
        let original = serde_json::to_value(&policy).unwrap();
        for path in ["policy", "candidate", "role", "schema", "effort"] {
            let mut changed = original.clone();
            match path {
                "policy" => changed["execution_profile"] = "composite".into(),
                "candidate" => changed["tiers"]["routine"][0]["served_model"] = "other".into(),
                "role" => changed["roles"]["worker"]["risk"] = "low".into(),
                "schema" => changed["schema"] = 3.into(),
                _ => changed["tiers"]["routine"][0]["effort"] = "native-fixed".into(),
            }
            assert!(
                Policy::parse(&serde_json::to_vec(&changed).unwrap()).is_err(),
                "{path}"
            );
        }
        let mut inputs = claude_inputs();
        inputs.provider_evidence.insert(
            "claude".into(),
            ProviderEvidence::from_catalog_row(&json!({
                "catalog_probe":"failed", "catalog_age_seconds":86401,
                "snapshot_age_seconds":172801, "effort_values":["low","medium","high"]
            }))
            .unwrap(),
        );
        let route = allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
        let snapshot = route.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.policy_schema, Some(2));
        assert_eq!(
            snapshot.configured.as_ref().unwrap().model,
            "claude-sonnet-5-5"
        );
        assert_eq!(snapshot.resolved.model, "claude-sonnet-5-6");
        assert_eq!(snapshot.requested_model, None);
        assert_eq!(snapshot.served_model, None);
        assert_eq!(snapshot.served_effort, None);
        assert_eq!(snapshot.capabilities.effort_support, EffortSupport::Unknown);
        assert_eq!(snapshot.catalog.reported_probe.as_deref(), Some("failed"));
        assert_eq!(snapshot.catalog.catalog_age_seconds, Some(86401));
        assert_eq!(snapshot.capabilities.probe_age_seconds, Some(172801));
        assert_eq!(snapshot.catalog.resolved_present, Some(true));
        assert_eq!(snapshot.family_evidence, "identifier_derived_unverified");
        assert_eq!(snapshot.execution_profile, None);
        assert_eq!(snapshot.selected_account, None);
        assert_eq!(snapshot.served_account, None);
        assert_eq!(snapshot.catalog.account_alias, None);
        assert_eq!(snapshot.capabilities.endpoint, None);
        assert_eq!(
            snapshot.capabilities.freshness,
            CapabilityFreshness::Unknown
        );
        let saved = serde_json::to_value(snapshot).unwrap();
        let old_digest = route.snapshot_sha256().unwrap();
        inputs.requested_model = Some("claude-sonnet-5-5".into());
        inputs.requested_effort = Some("low".into());
        let pinned = allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
        let pin = pinned.snapshot.unwrap();
        assert_eq!(pin.configured.unwrap().effort, "medium");
        assert_eq!(pin.requested_effort.as_deref(), Some("low"));
        assert_eq!(pin.requested_model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(pin.resolved.model, "claude-sonnet-5-5");
        assert_eq!(pin.catalog.resolved_present, Some(false));
        assert_eq!(pin.served_model, None);
        let mut rolled_back = policy.clone();
        rolled_back.tiers.get_mut("routine").unwrap()[0].model = "gpt-6.2-terra".into();
        inputs.catalogs.clear();
        inputs.provider_evidence.clear();
        assert_eq!(
            serde_json::to_value(route.snapshot.as_ref().unwrap()).unwrap(),
            saved
        );
        assert_eq!(route.snapshot_sha256().unwrap(), old_digest);
        assert_ne!(
            route.snapshot.as_ref().unwrap().policy_sha256,
            allocate(&rolled_back, "codex", "worker", None, "read", None, &inputs)
                .unwrap()
                .snapshot
                .unwrap()
                .policy_sha256
        );
        let mut loaded: Route =
            serde_json::from_value(serde_json::to_value(&route).unwrap()).unwrap();
        loaded.freeze_snapshot().unwrap();
        assert_eq!(loaded.snapshot, route.snapshot);
        // Captured from 30c1a329's existing canonical fixture, with its read
        // time pinned to 1000. New unknown fields must preserve stored bytes.
        let previous = concat!(
            r#"{"account_pinned":false,"binding_sha256":"20d176ca3ee026bf1be0066d8089a53265b12d33556454b3a88640491c2c201d","capabilities":{"account_alias":null,"effort_mechanism":null,"effort_support":"unknown","endpoint":null,"exact_model_efforts":null,"harness_version":null,"probe_age_seconds":172801,"provider_reported_efforts":["low","medium","high"],"source":"oms-models"},"catalog":{"account_alias":null,"catalog_age_seconds":86401,"count":1,"endpoint":null,"models_sha256":"10c29b6cd22cedfd053a31874f4994ec5a5bcaeb2c0bde084f95a66dc86ca77d","read_at":1000,"reported_probe":"failed","resolved_present":true,"source":"oms-models"},"configured":{"access":null,"effort":"medium","model":"claude-sonnet-5-5","provider":"claude","public_only":false},"execution_profile":null,"family_evidence":"identifier_derived_unverified","model_family":"anthropic","policy_schema":2,"policy_sha256":"667f37d3f54583628e818455a224c0acdbfc39d2a9e75a51e035f0eb3c94c601","requested_account":null,"requested_effort":null,"requested_model":null,"resolved":{"access":null,"effort":"medium","model":"claude-sonnet-5-6","provider":"claude","public_only":false},"schema":1,"selected_account":null,"served_account":null,"served_effort":null,"served_model":null,"served_source":null}"#,
            "\n"
        );
        loaded.snapshot = Some(serde_json::from_str(previous).unwrap());
        loaded.freeze_snapshot().unwrap();
        assert_eq!(
            crate::value::encoded(
                &serde_json::to_value(loaded.snapshot.as_ref().unwrap()).unwrap()
            )
            .unwrap(),
            previous.as_bytes()
        );
        assert_eq!(
            loaded.snapshot_sha256().unwrap().as_deref(),
            Some("b4330450d6991f266119c046a5969aed883a06e5bd1ff495e7a9f7b601738e48")
        );
    }
    #[test]
    fn exact_effort_evidence_and_final_fable_eligibility_are_checked() {
        let policy = Policy::bundled().unwrap();
        let mut inputs = claude_inputs();
        inputs.requested_effort = Some("high".into());
        inputs.provider_evidence.insert(
            "claude".into(),
            ProviderEvidence::from_catalog_row(&json!({
                "model_effort_scales":{"claude-sonnet-5-6":["low","medium"]},
                "effort_values":["low","medium","high"], "catalog_probe":"ok"
            }))
            .unwrap(),
        );
        assert!(allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());
        inputs.requested_effort = Some("medium".into());
        let route = allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
        assert_eq!(
            route.snapshot.unwrap().capabilities.effort_support,
            EffortSupport::ReportedSupported
        );
        inputs.status =
            json!({"claude":{"daily_cap":{"claude-sonnet-5-6":crate::now()[..10].to_owned()}}});
        assert!(allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());
        inputs.status = Value::Null;
        inputs
            .provider_evidence
            .get_mut("claude")
            .unwrap()
            .model_effort_scales
            .insert("claude-sonnet-5-6".into(), vec!["native-fixed".into()]);
        assert!(allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());
        inputs.requested_effort = Some("max".into());
        assert!(allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());
        inputs.requested_effort = None;
        inputs.requested_model = Some("claude-fable-5-2".into());
        inputs.provider_evidence.clear();
        assert!(
            allocate(
                &policy,
                "codex",
                "worker",
                Some("frontier"),
                "read",
                None,
                &inputs
            )
            .is_err()
        );
        inputs.explicit_target = true;
        let explicit = allocate(
            &policy,
            "codex",
            "worker",
            Some("frontier"),
            "read",
            None,
            &inputs,
        )
        .unwrap();
        assert_eq!(explicit.candidate.model, "claude-fable-5-2");
        inputs.requested_model = None;
        inputs.explicit_target = false;
        inputs
            .catalogs
            .insert("claude".into(), vec!["claude-fable-5-2".into()]);
        let auditor = allocate(
            &policy,
            "codex",
            "security-auditor",
            None,
            "read",
            None,
            &inputs,
        )
        .unwrap();
        let snapshot = auditor.snapshot.unwrap();
        assert_eq!(snapshot.configured.unwrap().model, "claude-fable-5-1");
        assert_eq!(snapshot.resolved.model, "claude-fable-5-2");
        assert_eq!(snapshot.served_model, None);
    }
    #[test]
    fn capability_freshness_keeps_reported_scope_and_selected_account_separate() {
        let policy = Policy::bundled().unwrap();
        let observed = json!({"schema":1,"source":"fixture-model-list",
            "account_alias":"work","endpoint":"https://models.example.invalid/v1",
            "observed_at":900,"expires_at":1100});
        let snapshot = |observation: Value, read_at: Option<u64>, account: Option<&str>| {
            let mut inputs = claude_inputs();
            let mut evidence =
                ProviderEvidence::from_catalog_row(&json!({"capability_evidence":observation}))
                    .unwrap();
            evidence.read_at = read_at;
            inputs.provider_evidence.insert("claude".into(), evidence);
            let mut route =
                allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
            // This pure projection fixture supplies a selected alias; it does
            // not attest a native account or endpoint producer.
            route.account = account.map(str::to_owned);
            route.snapshot = Some(route.selection_snapshot(None, None, Some(&inputs)).unwrap());
            route.freeze_snapshot().unwrap();
            route
        };
        for (read_at, expected) in [
            (Some(1000), CapabilityFreshness::ReportedFresh),
            (Some(1100), CapabilityFreshness::ReportedStale),
            (Some(899), CapabilityFreshness::Unknown),
            (None, CapabilityFreshness::Unknown),
        ] {
            let route = snapshot(observed.clone(), read_at, Some("work"));
            let cap = &route.snapshot.as_ref().unwrap().capabilities;
            assert_eq!(cap.freshness, expected);
            assert_eq!(cap.selected_account_matches, Some(true));
            assert_eq!(cap.account_alias.as_deref(), Some("work"));
            assert_eq!(cap.endpoint.as_deref(), observed["endpoint"].as_str());
            assert_eq!(route.snapshot.as_ref().unwrap().served_account, None);
        }
        let mismatch = snapshot(observed.clone(), Some(1000), Some("other"));
        let cap = &mismatch.snapshot.as_ref().unwrap().capabilities;
        assert_eq!(cap.freshness, CapabilityFreshness::ReportedFresh);
        assert_eq!(cap.selected_account_matches, Some(false));
        assert_eq!(
            snapshot(observed.clone(), Some(1000), None)
                .snapshot
                .unwrap()
                .capabilities
                .selected_account_matches,
            None
        );
        for field in ["account_alias", "endpoint", "observed_at", "expires_at"] {
            let mut incomplete = observed.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert_eq!(
                snapshot(incomplete, Some(1000), Some("work"))
                    .snapshot
                    .unwrap()
                    .capabilities
                    .freshness,
                CapabilityFreshness::Unknown,
                "{field} cannot be inferred"
            );
        }
        let mut tampered = mismatch.clone();
        tampered
            .snapshot
            .as_mut()
            .unwrap()
            .capabilities
            .selected_account_matches = Some(true);
        assert!(tampered.freeze_snapshot().is_err());
        for (field, forged) in [
            ("freshness", json!("reported_stale")),
            ("account_alias", json!("other")),
            ("endpoint", json!("https://other.example.invalid")),
        ] {
            let mut tampered = serde_json::to_value(&mismatch).unwrap();
            tampered["snapshot"]["capabilities"][field] = forged;
            let mut tampered: Route = serde_json::from_value(tampered).unwrap();
            assert!(tampered.freeze_snapshot().is_err(), "{field} rebinding");
        }
        let mut unbound = serde_json::to_value(mismatch).unwrap();
        let cap = unbound["snapshot"]["capabilities"].as_object_mut().unwrap();
        for field in ["freshness", "evidence", "selected_account_matches"] {
            cap.remove(field);
        }
        let mut unbound: Route = serde_json::from_value(unbound).unwrap();
        assert!(unbound.freeze_snapshot().is_err());
    }
    #[test]
    fn legacy_composite_and_family_evidence_stay_unknown_without_a_producer() {
        let mut policy = Policy::bundled().unwrap();
        policy.tiers.insert(
            "routine".into(),
            vec![Candidate {
                provider: "pi".into(),
                model: "vendor/Orchestrator".into(),
                effort: "medium".into(),
                public_only: true,
                access: None,
            }],
        );
        let mut inputs = RoutingInputs {
            installed: Some(["pi".into()].into()),
            explicit_target: true,
            ..Default::default()
        };
        assert!(allocate(&policy, "codex", "worker", None, "read", None, &inputs).is_err());
        inputs.public_repository = true;
        let mut route = allocate(&policy, "codex", "worker", None, "read", None, &inputs).unwrap();
        let snapshot = route.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.model_family, None);
        assert_eq!(snapshot.family_evidence, "unknown");
        assert_eq!(snapshot.execution_profile, None);
        assert_eq!(snapshot.capabilities.effort_support, EffortSupport::Unknown);
        assert_eq!(snapshot.catalog.models_sha256, None);
        route.snapshot = None;
        route.freeze_snapshot().unwrap();
        let legacy = route.snapshot.as_ref().unwrap();
        assert_eq!(legacy.policy_sha256, None);
        assert_eq!(legacy.policy_schema, None);
        assert_eq!(legacy.configured, None);
        assert_eq!(legacy.served_model, None);
        assert!(allocate(&policy, "codex", "reviewer", None, "write", None, &inputs).is_err());
    }
    #[test]
    fn snapshots_reject_rebinding_served_replay_and_malformed_evidence() {
        let policy = Policy::bundled().unwrap();
        let route = allocate(
            &policy,
            "codex",
            "worker",
            None,
            "read",
            None,
            &claude_inputs(),
        )
        .unwrap();
        for change in ["account", "model", "effort", "access", "schema", "served"] {
            let mut altered = route.clone();
            match change {
                "account" => altered.account = Some("other".into()),
                "model" => altered.candidate.model = "claude-sonnet-5-7".into(),
                "effort" => altered.candidate.effort = "low".into(),
                "access" => altered.access = "write".into(),
                "schema" => altered.snapshot.as_mut().unwrap().schema = 2,
                _ => {
                    altered.snapshot.as_mut().unwrap().served_model =
                        Some("reported elsewhere".into())
                }
            }
            assert!(altered.freeze_snapshot().is_err(), "{change}");
        }
        let mut legacy = route.clone();
        legacy.snapshot = None;
        legacy.candidate.provider = "pi".into();
        legacy.candidate.model = format!("vendor/{}", "x".repeat(32768));
        assert!(
            legacy
                .freeze_snapshot()
                .unwrap_err()
                .message
                .contains("exceeds bounds")
        );
        let mut snapshot = serde_json::to_value(route.snapshot.unwrap()).unwrap();
        snapshot["new_authority"] = true.into();
        assert!(serde_json::from_value::<RouteSnapshot>(snapshot).is_err());
        for row in [
            json!({"catalog_age_seconds":-1}),
            json!({"effort_values":vec!["high";17]}),
            json!({"model_effort_scales":{"model":true}}),
            json!({"version":"bad\nversion"}),
            json!({"capability_evidence":{"schema":2,"source":"fixture"}}),
            json!({"capability_evidence":{"schema":1,"source":"fixture","observed_at":10,"expires_at":10}}),
            json!({"capability_evidence":{"schema":1,"source":"fixture","account_alias":"private/home"}}),
            json!({"capability_evidence":{"schema":1,"source":"fixture","endpoint":concat!("https://user:", "password", "@example.invalid")}}),
            json!({"capability_evidence":{"schema":1,"source":"fixture","endpoint":"https://example.invalid/?private=value"}}),
            json!({"capability_evidence":{"schema":1,"source":"fixture","expires_at":-1}}),
        ] {
            assert!(ProviderEvidence::from_catalog_row(&row).is_err());
        }
    }
}
