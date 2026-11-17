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
            "antigravity" => Ok("google"),
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
        let catalog = inputs
            .catalogs
            .get(&c.provider)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let Ok(model) = routable_route_model(&c.provider, &c.model, catalog) else {
            continue;
        };
        c.model = model;
        let mut account = None;
        let mut account_reason = None;
        let mut used = weekly_used(&inputs.status, &c.provider);
        if c.provider == "codex"
            && let Some(accounts) = &inputs.accounts
        {
            match accounts.select(
                &c.model,
                &inputs.status["codex"]["accounts"],
                &inputs.running_accounts,
                inputs.explicit_target,
            ) {
                Ok((entry, reason)) => {
                    used = account_weekly_used(
                        &inputs.status["codex"]["accounts"][&entry.id],
                        crate::timestamp(),
                    );
                    account = Some(entry.id);
                    account_reason = Some(reason);
                }
                Err(_) => continue,
            }
        }
        if alternatives
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
                if all_known {
                    *inputs.recent.get(&c.provider).unwrap_or(&0)
                } else {
                    0
                },
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
pub struct Accounts {
    pub policy: String,
    pub accounts: Vec<Account>,
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
        let Some(value) = root.json(Path::new(path.file_name().unwrap()), 1024 * 1024, true)?
        else {
            return Ok(None);
        };
        require(
            value["schema"].as_u64() == Some(1)
                && value.as_object().is_some_and(|v| {
                    v.keys()
                        .all(|k| ["schema", "providers"].contains(&k.as_str()))
                }),
            "invalid provider accounts schema",
        )?;
        require(
            value["providers"].is_object()
                && value["providers"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .all(|v| v == "codex"),
            "unsupported provider account configuration",
        )?;
        let Some(codex) = value["providers"].get("codex") else {
            return Ok(None);
        };
        let mut settings = codex.clone();
        if settings.get("policy").is_none() {
            settings["policy"] = "ordered".into();
        }
        let mut account: Self = serde_json::from_value(settings)
            .map_err(|_| Error::new("invalid provider accounts"))?;
        require(
            ["ordered", "spread"].contains(&account.policy.as_str())
                && !account.accounts.is_empty(),
            "invalid provider account policy/list",
        )?;
        let mut ids = BTreeSet::new();
        let mut homes = BTreeSet::new();
        for entry in &mut account.accounts {
            require(
                !entry.id.is_empty()
                    && entry.id.len() <= 64
                    && entry.id.as_bytes()[0].is_ascii_lowercase()
                    && entry.id.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)
                    })
                    && ids.insert(entry.id.clone()),
                "invalid or duplicate account id",
            )?;
            require(entry.home.is_absolute(), "account home must be absolute")?;
            if let Ok(home) = std::fs::canonicalize(&entry.home) {
                entry.home = home;
            }
            require(homes.insert(entry.home.clone()), "duplicate account home")?;
        }
        Ok(Some(account))
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
    pub fn select(
        &self,
        model: &str,
        readings: &Value,
        running: &BTreeMap<String, usize>,
        explicit: bool,
    ) -> Result<(Account, String)> {
        let mut eligible = Vec::new();
        for (index, entry) in self.accounts.iter().enumerate() {
            if !entry.home.is_dir() {
                continue;
            }
            let used = account_weekly_used(&readings[&entry.id], crate::timestamp());
            if used.is_some_and(|v| v >= 90) {
                continue;
            }
            let root = crate::Store::open(&entry.home)?;
            if let Some(models) =
                root.json(Path::new("models_cache.json"), 4 * 1024 * 1024, true)?
            {
                let models: Vec<_> = array(&models["models"])
                    .iter()
                    .map(|v| text(v, "slug"))
                    .collect();
                if model != "provider-default"
                    && !models.is_empty()
                    && !models.iter().any(|v| v == model)
                {
                    continue;
                }
            }
            eligible.push((
                used.unwrap_or(100),
                if self.policy == "spread" && !explicit {
                    *running.get(&entry.id).unwrap_or(&0)
                } else {
                    0
                },
                index,
                entry,
            ));
        }
        eligible.sort_by_key(|v| (v.0, v.1, v.2));
        if let Some((_, _, _, entry)) = eligible.first() {
            return Ok((
                (*entry).clone(),
                if explicit {
                    "explicit target".into()
                } else {
                    self.policy.clone()
                },
            ));
        }
        if explicit {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join(".codex")
                });
            require(
                home.is_absolute() && home.is_dir(),
                "default current account home unavailable",
            )?;
            return Ok((
                Account {
                    id: "default".into(),
                    home,
                },
                "explicit target; default current home".into(),
            ));
        }
        Err(Error::new("no eligible Codex account"))
    }
    pub fn home(&self, ident: &str) -> Result<&Path> {
        self.accounts
            .iter()
            .find(|v| v.id == ident)
            .map(|v| v.home.as_path())
            .ok_or_else(|| Error::new("unknown Codex account"))
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
