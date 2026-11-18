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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}
#[derive(Clone, Debug, Default)]
pub struct RoutingInputs {
    pub installed: Option<BTreeSet<String>>,
    pub catalogs: BTreeMap<String, Vec<String>>,
    pub status: Value,
    pub recent: BTreeMap<String, usize>,
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
        (inputs.explicit_target || role == "security-auditor" || c.model != "claude-fable-5-1")
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
        let mut account = None;
        let mut account_reason = None;
        let mut used = weekly_used(&inputs.status, &c.provider);
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
            let readings = &inputs.status[c.provider.as_str()]["accounts"];
            match accounts.select(
                &c.provider,
                &c.model,
                readings,
                &inputs.running_accounts,
                inputs.explicit_target,
                inputs.requested_account.as_deref(),
            ) {
                Ok((entry, reason)) => {
                    used = account_weekly_used(&readings[&entry.id], crate::timestamp());
                    account = Some(entry.id);
                    account_reason = Some(reason);
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
        eligible.push((c, used, account, account_reason));
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
        .filter(|(c, _, _, _)| {
            c.provider != "pi" || inputs.status["pi"]["cooldown"].get(&c.model).is_none()
        })
        .cloned()
        .collect();
    if !kept.is_empty() {
        eligible = kept;
    }
    let readings: Vec<_> = eligible
        .iter()
        .filter(|(c, _, _, _)| ["codex", "claude"].contains(&c.provider.as_str()))
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
        .filter(|(c, _, _, _)| ["codex", "claude"].contains(&c.provider.as_str()))
        .cloned()
        .collect();
    native.sort_by_key(|(c, used, _, _)| {
        if all_known && !tied {
            (usize::from(used.unwrap_or(100)), false)
        } else {
            (
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
            .rposition(|(c, u, _, _)| {
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
    let (mut candidate, _, account, account_reason) = eligible.remove(0);
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
    Ok(Route {
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
    })
}
pub fn weekly_used(status: &Value, provider: &str) -> Option<u8> {
    crate::metrics::percent(&status[provider]["weekly_used"])
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
        let config = self
            .providers
            .get(provider)
            .ok_or_else(|| Error::new("no accounts configured for provider"))?;
        let now = crate::timestamp();
        let mut eligible = Vec::new();
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
            let spread = config.policy == "spread" && requested.is_none();
            let load = running
                .get(&format!("{provider}:{}", entry.id))
                .or_else(|| running.get(&entry.id))
                .copied()
                .unwrap_or(0);
            eligible.push((
                used.map_or(100, |v| usize::from(v) / 10 * 10),
                if spread { load } else { 0 },
                index,
                entry,
            ));
        }
        eligible.sort_by_key(|v| (v.0, v.1, v.2));
        if let Some((_, _, _, entry)) = eligible.first() {
            return Ok((
                (*entry).clone(),
                if requested.is_some() {
                    "requested account".into()
                } else if explicit {
                    "explicit target".into()
                } else {
                    config.policy.clone()
                },
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
pub fn account_weekly_used(row: &Value, now: u64) -> Option<u8> {
    if row
        .get("resets_at")
        .is_some_and(|v| !v.is_null() && v.as_f64().is_none_or(|n| n <= now as f64))
    {
        return None;
    }
    if let Some(v) = row.get("observed_at").or_else(|| row.get("timestamp")) {
        let observed = crate::metrics::stamp(v)?;
        if observed > now as f64 || now as f64 - observed > 10080.0 * 60.0 {
            return None;
        }
    }
    crate::metrics::percent(&row["weekly_used"])
}
