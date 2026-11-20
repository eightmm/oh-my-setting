//! Reusable provider account selection receipt and private binding persistence.

use crate::claude_lead_usage::AutoLaunch;
use crate::cli::valid_account_id;
use crate::{Error, Result, require};
use oms_panel_core::native::reference::process::Owner;
use oms_panel_core::{Store, Value, metrics, routing, terminals};
use rustix::fd::OwnedFd;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const BINDING_SCHEMA: u64 = 1;
const MAX_BINDING_BYTES: usize = 65536;
const CLAUDE_CREDENTIAL_OVERRIDES: &[&str] = &[
    concat!("ANTHROPIC_API_", "KEY"),
    concat!("ANTHROPIC_AUTH_", "TOKEN"),
    concat!("CLAUDE_CODE_OAUTH_", "TOKEN"),
];
const CODEX_MAIN_CREDENTIAL_OVERRIDES: &[&str] = &[
    concat!("CODEX_API_", "KEY"),
    concat!("CODEX_ACCESS_", "TOKEN"),
    concat!("OPENAI_API_", "KEY"),
    "OPENAI_BASE_URL",
];
const CLAUDE_MAIN_CREDENTIAL_OVERRIDES: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
];
const PI_SCRUB_ENV: &[&str] = &["PI_CODING_AGENT_SESSION_DIR"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountVerification {
    Verified,
    UnpinnedAmbient,
    Unverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionTarget<'a> {
    pub role: &'static str,
    pub provider: &'a str,
    pub model: &'a str,
}

/// In-memory selection receipt. `home_env` must stay in memory and never reach
/// persisted receipts, tmux options, or outbound JSON payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSelectionReceipt {
    pub target_role: &'static str,
    pub provider: String,
    pub verification: AccountVerification,
    pub account_id: Option<String>,
    pub account_pinned: bool,
    pub selection_reason: String,
    pub home_env: Option<(&'static str, PathBuf)>,
    pub home_digest: Option<String>,
    codex_home_identity: Option<String>,
    pub(crate) claude_observation_identity: Option<String>,
    pub(crate) claude_auto_observation: Option<AutoLaunch>,
    pub scrub_env: &'static [&'static str],
}

pub trait AccountHost {
    fn load_accounts(&self) -> Result<Option<routing::Accounts>>;
    fn state_home(&self) -> Result<PathBuf>;
    fn standard_home(&self, provider: &str) -> Option<PathBuf>;
    fn active_home(&self, provider: &str) -> Option<PathBuf>;
    fn env_nonempty(&self, key: &str) -> bool;
    fn process_owner(&self, pid: u32) -> Result<Owner>;
    fn uid(&self) -> u32;
    fn fresh_account_readings(&self, provider: &str, now: u64) -> Option<Value>;
}

pub(crate) fn load_configured_accounts_from_roots(
    home: Option<&Path>,
    xdg_config_home: Option<&Path>,
) -> Result<Option<routing::Accounts>> {
    let home = home
        .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
        .ok_or_else(|| Error::new("home unavailable"))?;
    let config = xdg_config_home
        .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".config"));
    match std::fs::symlink_metadata(&config) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::new(e)),
        Ok(_) => require(
            std::fs::metadata(&config).map_err(Error::new)?.is_dir(),
            "account config root is not a directory",
        )?,
    }
    let dir = config.join("oh-my-setting");
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::new(e)),
        Ok(_) => routing::Accounts::load(&dir.join("provider-accounts.json")),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAccountHost;

impl AccountHost for SystemAccountHost {
    fn load_accounts(&self) -> Result<Option<routing::Accounts>> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        load_configured_accounts_from_roots(home.as_deref(), xdg.as_deref())
    }

    fn state_home(&self) -> Result<PathBuf> {
        terminals::state_home()
    }

    fn standard_home(&self, provider: &str) -> Option<PathBuf> {
        let (_, default) = routing::account_home_env(provider)?;
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|home| home.join(default))
    }

    fn active_home(&self, provider: &str) -> Option<PathBuf> {
        routing::active_account_home(provider)
    }

    fn env_nonempty(&self, key: &str) -> bool {
        std::env::var_os(key).is_some_and(|val| !val.is_empty())
    }

    fn process_owner(&self, pid: u32) -> Result<Owner> {
        terminals::process(pid)
    }

    fn uid(&self) -> u32 {
        rustix::process::getuid().as_raw()
    }

    fn fresh_account_readings(&self, provider: &str, now: u64) -> Option<Value> {
        if provider != "codex" {
            return None;
        }
        metrics::collect_local_with(&json!({}), None, now as f64, None)
            .get("codex")?
            .get("accounts")
            .cloned()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BindingAnchor<'a> {
    pub repo: &'a Path,
    pub room: &'a str,
    pub main_attempt: &'a str,
    pub main_owner: &'a str,
    pub session: &'a terminals::SessionProof,
    pub provider: &'a str,
    pub pane_id: &'a str,
    pub pid: u32,
    pub birth: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedAccountBinding {
    pub schema: u64,
    pub target_role: String,
    pub repo: String,
    pub uid: u32,
    pub room: String,
    pub main_attempt: String,
    pub main_owner: String,
    pub session: terminals::SessionProof,
    pub provider: String,
    pub pane_id: String,
    pub pid: u32,
    pub birth: String,
    pub verification: AccountVerification,
    pub account_id: Option<String>,
    pub account_pinned: bool,
    pub selection_reason: String,
    pub home_env_key: Option<String>,
    pub home_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_home_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_observation_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) claude_auto_observation: Option<AutoLaunch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_main_child: Option<ClaudeMainChild>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alternate_lead: Option<AlternateLeadScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AlternateLeadScope {
    pub main_pane: String,
    pub main_pid: u32,
    pub main_birth: String,
    pub window: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeMainChild {
    pid: u32,
    birth: String,
}

impl PersistedAccountBinding {
    /// Absent for legacy records and when stable directory identity was unavailable.
    pub(crate) fn has_codex_chat_home_identity(&self) -> bool {
        self.codex_home_identity.is_some()
    }

    pub(crate) fn claude_main_child_owner(&self, host: &dyn AccountHost) -> Option<Owner> {
        let child = self.claude_main_child.as_ref()?;
        let owner = host.process_owner(child.pid).ok()?;
        (owner.pid == child.pid && owner.birth == child.birth && owner.uid == self.uid)
            .then_some(owner)
    }
}

pub(crate) fn path_digest(path: &Path) -> String {
    oms_panel_core::digest(path.as_os_str().as_encoded_bytes())
}

pub(crate) fn claude_binding_still_mapped(
    host: &dyn AccountHost,
    binding: &PersistedAccountBinding,
) -> bool {
    mapped_claude_home(host, binding).is_some()
}

fn mapped_claude_home(
    host: &dyn AccountHost,
    binding: &PersistedAccountBinding,
) -> Option<PathBuf> {
    let accounts = host.load_accounts().ok()??;
    let expected = binding.home_digest.as_deref()?;
    let home = resolved_claude_home(&accounts, binding.account_id.as_deref()?, host.uid())?;
    (path_digest(&home) == expected).then_some(home)
}

fn resolved_claude_home(accounts: &routing::Accounts, alias: &str, uid: u32) -> Option<PathBuf> {
    let path = accounts.home("claude", alias).ok()?;
    let canonical = path.canonicalize().ok()?;
    check_private_dir(path, uid).ok()?;
    check_private_dir(&canonical, uid).ok()?;
    (!accounts.home_claimed_by_other("claude", &canonical)).then_some(canonical)
}

#[cfg(target_os = "linux")]
pub(crate) fn inspect_claude_accounts(
    host: &dyn AccountHost,
    now: f64,
) -> BTreeMap<String, crate::claude_lead_usage::InspectedObservation> {
    use crate::claude_lead_usage::inspect_observation;
    let Some(accounts) = host.load_accounts().ok().flatten() else {
        return BTreeMap::new();
    };
    let Some(claude) = accounts.provider("claude") else {
        return BTreeMap::new();
    };
    let uid = host.uid();
    let observations: Vec<_> = claude
        .accounts
        .iter()
        .filter_map(|entry| {
            let alias = &entry.id;
            if !valid_account_id(alias) {
                return None;
            }
            let home = resolved_claude_home(&accounts, alias, uid)?;
            let receipt = inspect_observation(&home, alias, uid, now)?;
            Some((alias, home, receipt))
        })
        .collect();
    if observations.is_empty() {
        return BTreeMap::new();
    }
    let Some(current) = host.load_accounts().ok().flatten() else {
        return BTreeMap::new();
    };
    observations
        .into_iter()
        .filter_map(|(alias, home, receipt)| {
            let current_home = resolved_claude_home(&current, alias, uid)?;
            (current_home == home && receipt.still_named(&home, uid))
                .then(|| (alias.to_owned(), receipt.reading))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn inspect_claude_accounts(
    _: &dyn AccountHost,
    _: f64,
) -> BTreeMap<String, crate::claude_lead_usage::InspectedObservation> {
    BTreeMap::new()
}

#[cfg(target_os = "linux")]
pub(crate) fn claude_observation_home<'a>(
    host: &dyn AccountHost,
    binding: &'a PersistedAccountBinding,
) -> Option<(PathBuf, &'a str)> {
    let identity = binding.claude_observation_identity.as_deref()?;
    let home = mapped_claude_home(host, binding)?;
    (crate::claude_lead_usage::claude_observation_identity(&home, host.uid()).as_deref()
        == Some(identity))
    .then_some((home, identity))
}

#[cfg(target_os = "linux")]
pub(crate) fn claude_auto_observation_home(
    host: &dyn AccountHost,
    binding: &PersistedAccountBinding,
) -> Option<PathBuf> {
    let launch = binding.claude_auto_observation.as_ref()?;
    let home = mapped_claude_home(host, binding)?;
    launch.matches_home(&home, host.uid()).then_some(home)
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_private_dir(path: &Path, uid: u32) -> Result<()> {
    require(path.is_absolute(), "account home must be absolute")?;
    let leaf = path.components().as_path();
    let meta = std::fs::symlink_metadata(leaf)
        .map_err(|_| Error::new("requested account home is unavailable"))?;
    require(
        !meta.file_type().is_symlink()
            && meta.file_type().is_dir()
            && meta.uid() == uid
            && meta.mode() & 0o022 == 0,
        "account home must be an owned private directory",
    )
}

fn check_claude_credentials(
    host: &dyn AccountHost,
    canon_home: &Path,
    tmux_env_lines: &[&str],
) -> Result<()> {
    let default_canon = host
        .standard_home("claude")
        .and_then(|path| std::fs::canonicalize(path).ok());
    if default_canon.as_deref() == Some(canon_home) {
        return Ok(());
    }
    for key in CLAUDE_CREDENTIAL_OVERRIDES {
        let in_host = host.env_nonempty(key);
        let in_tmux = tmux_env_lines.iter().any(|line| {
            line.split_once('=')
                .is_some_and(|(name, value)| name == *key && !value.is_empty())
        });
        require(
            !in_host && !in_tmux,
            "credential environment overrides the selected Claude account",
        )?;
    }
    Ok(())
}

fn codex_serves_model(home: &Path, model: &str) -> Result<bool> {
    let root = Store::open(home)?;
    if let Some(models) = root.json(Path::new("models_cache.json"), 4 * 1024 * 1024, true)? {
        let slugs: Vec<_> = models["models"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|entry| {
                entry
                    .get("slug")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
            .collect();
        if model != "provider-default" && !slugs.is_empty() && !slugs.contains(&model) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn build_verified_receipt(
    host: &dyn AccountHost,
    target: SelectionTarget<'_>,
    accounts: &routing::Accounts,
    selected: routing::Account,
    reason: String,
    pinned: bool,
    env_key: &'static str,
    scrub_env: &'static [&'static str],
    tmux_env_lines: &[&str],
) -> Result<AccountSelectionReceipt> {
    check_private_dir(&selected.home, host.uid())?;
    let canon_home = std::fs::canonicalize(&selected.home)
        .map_err(|_| Error::new("requested account home is unavailable"))?;
    check_private_dir(&canon_home, host.uid())?;
    require(
        !accounts.home_claimed_by_other(target.provider, &canon_home),
        "account home is claimed by another provider",
    )?;
    if target.provider == "claude" {
        check_claude_credentials(host, &canon_home, tmux_env_lines)?;
    }
    let digest = path_digest(&canon_home);
    let codex_home_identity = (target.role == "orchestrator" && target.provider == "codex")
        .then(|| open_codex_home(&canon_home, host.uid()).map(|(_, identity)| identity))
        .flatten();
    let claude_observation_identity = (target.role == "orchestrator"
        && target.provider == "claude"
        && pinned
        && crate::claude_lead_usage::OBSERVATION_OVERRIDES
            .iter()
            .all(|key| {
                !host.env_nonempty(key)
                    && !tmux_env_lines.iter().any(|row| {
                        row.split_once('=')
                            .is_some_and(|(name, value)| name == *key && !value.is_empty())
                    })
            }))
    .then(|| crate::claude_lead_usage::claude_observation_identity(&canon_home, host.uid()))
    .flatten();
    Ok(AccountSelectionReceipt {
        target_role: target.role,
        provider: target.provider.to_owned(),
        verification: AccountVerification::Verified,
        account_id: Some(selected.id),
        account_pinned: pinned,
        selection_reason: reason,
        home_env: Some((env_key, canon_home)),
        home_digest: Some(digest),
        codex_home_identity,
        claude_observation_identity,
        claude_auto_observation: None,
        scrub_env,
    })
}

fn normalized_fresh_codex_readings(
    host: &dyn AccountHost,
    provider_accounts: &routing::ProviderAccounts,
    model: &str,
    readings: &Value,
    now: u64,
) -> Result<Option<Value>> {
    let Some(map) = readings.as_object() else {
        return Ok(None);
    };
    let mut normalized = serde_json::Map::new();
    let mut eligible = 0usize;
    for entry in &provider_accounts.accounts {
        let leaf = entry.home.components().as_path();
        let Ok(sym_meta) = std::fs::symlink_metadata(leaf) else {
            continue;
        };
        require(
            !sym_meta.file_type().is_symlink(),
            "account home must be an owned private directory",
        )?;
        if !sym_meta.file_type().is_dir()
            || sym_meta.uid() != host.uid()
            || sym_meta.mode() & 0o022 != 0
        {
            continue;
        }
        if !codex_serves_model(&entry.home, model)? {
            continue;
        }
        eligible += 1;
        let Some(row) = map.get(&entry.id) else {
            return Ok(None);
        };
        let Some(observed) = row.get("observed_at").and_then(metrics::stamp) else {
            return Ok(None);
        };
        if observed > now as f64 || (now as f64 - observed) > metrics::READING_FRESH_SECONDS {
            return Ok(None);
        }
        let Some(resets_at) = row.get("resets_at").and_then(metrics::stamp) else {
            return Ok(None);
        };
        if resets_at <= now as f64 {
            return Ok(None);
        }
        let Some(used) = routing::account_weekly_used(row, now) else {
            return Ok(None);
        };
        // Forward validated usage without synthetic timestamps so Accounts::select
        // ranks identically regardless of wall-clock skew in tests.
        normalized.insert(entry.id.clone(), json!({ "weekly_used": used }));
    }
    Ok((eligible > 0).then_some(Value::Object(normalized)))
}

pub fn resolve_selection(
    host: &dyn AccountHost,
    target: SelectionTarget<'_>,
    requested_account: Option<&str>,
    tmux_env_lines: &[&str],
    now: u64,
) -> Result<AccountSelectionReceipt> {
    if !routing::ACCOUNT_PROVIDERS.contains(&target.provider) {
        require(
            requested_account.is_none(),
            &format!(
                "provider account isolation is unproved for {}",
                target.provider
            ),
        )?;
        return Ok(AccountSelectionReceipt {
            target_role: target.role,
            provider: target.provider.to_owned(),
            verification: AccountVerification::Unverified,
            account_id: None,
            account_pinned: false,
            selection_reason: "unverified provider account isolation".into(),
            home_env: None,
            home_digest: None,
            codex_home_identity: None,
            claude_observation_identity: None,
            claude_auto_observation: None,
            scrub_env: &[],
        });
    }

    let (env_key, _) = routing::account_home_env(target.provider)
        .ok_or_else(|| Error::new("provider account home variable unavailable"))?;
    let scrub_env = if target.provider == "pi" {
        PI_SCRUB_ENV
    } else {
        &[]
    };

    if let Some(alias) = requested_account {
        require(valid_account_id(alias), "invalid account id")?;
        let accounts = host
            .load_accounts()?
            .ok_or_else(|| Error::new("no accounts configured for provider"))?;
        let _ = accounts.home(target.provider, alias)?;
        let (selected, reason) = accounts.select(
            target.provider,
            target.model,
            &Value::Null,
            &BTreeMap::new(),
            true,
            Some(alias),
        )?;
        return build_verified_receipt(
            host,
            target,
            &accounts,
            selected,
            reason,
            true,
            env_key,
            scrub_env,
            tmux_env_lines,
        );
    }

    let accounts = host.load_accounts()?;
    if let Some(accounts) = &accounts {
        check_active_home(host, target.provider, accounts)?;
        if target.provider == "codex"
            && let Some(provider_accounts) = accounts.provider("codex")
            && !provider_accounts.accounts.is_empty()
            && let Some(raw_readings) = host.fresh_account_readings("codex", now)
            && let Some(readings) = normalized_fresh_codex_readings(
                host,
                provider_accounts,
                target.model,
                &raw_readings,
                now,
            )?
            && let Ok((selected, reason)) = accounts.select(
                "codex",
                target.model,
                &readings,
                &BTreeMap::new(),
                false,
                None,
            )
        {
            return build_verified_receipt(
                host,
                target,
                accounts,
                selected,
                reason,
                false,
                env_key,
                scrub_env,
                tmux_env_lines,
            );
        }
    }

    Ok(ambient_receipt(target, scrub_env))
}

fn check_active_home(
    host: &dyn AccountHost,
    provider: &str,
    accounts: &routing::Accounts,
) -> Result<()> {
    require(
        host.active_home(provider)
            .is_none_or(|active| !accounts.home_claimed_by_other(provider, &active)),
        "active account home is claimed by another provider",
    )
}

fn ambient_receipt(
    target: SelectionTarget<'_>,
    scrub_env: &'static [&'static str],
) -> AccountSelectionReceipt {
    AccountSelectionReceipt {
        target_role: target.role,
        provider: target.provider.to_owned(),
        verification: AccountVerification::UnpinnedAmbient,
        account_id: None,
        account_pinned: false,
        selection_reason: "ambient provider home".into(),
        home_env: None,
        home_digest: None,
        codex_home_identity: None,
        claude_observation_identity: None,
        claude_auto_observation: None,
        scrub_env,
    }
}

/// Native Claude selection uses the already-authorized repository Events projection.
pub(crate) fn resolve_claude_spread(
    host: &dyn AccountHost,
    repo: &Path,
    model: &str,
    worker_accounts: &BTreeMap<&str, usize>,
    tmux_env_lines: &[&str],
) -> Result<AccountSelectionReceipt> {
    let target = SelectionTarget {
        role: "orchestrator",
        provider: "claude",
        model,
    };
    let Some(accounts) = host.load_accounts()? else {
        return Ok(ambient_receipt(target, &[]));
    };
    check_active_home(host, "claude", &accounts)?;
    let tmux_home = tmux_env_lines
        .iter()
        .rev()
        .find_map(|row| {
            if *row == "-CLAUDE_CONFIG_DIR" {
                Some(None)
            } else {
                row.strip_prefix("CLAUDE_CONFIG_DIR=").map(Some)
            }
        })
        .flatten();
    if let Some(home) = tmux_home {
        // An empty line-delimited assignment can also be the start of a multiline value.
        require(
            home.is_empty() || !accounts.home_claimed_by_other("claude", &repo.join(home)),
            "active account home is claimed by another provider",
        )?;
        return Ok(ambient_receipt(target, &[]));
    }
    let Some(config) = accounts
        .provider("claude")
        .filter(|config| config.policy == "spread")
    else {
        return Ok(ambient_receipt(target, &[]));
    };
    if host.env_nonempty("CLAUDE_CONFIG_DIR")
        || crate::claude_lead_usage::OBSERVATION_OVERRIDES
            .iter()
            .any(|key| {
                host.env_nonempty(key)
                    || tmux_env_lines.iter().any(|row| {
                        row.split_once('=')
                            .is_some_and(|(name, value)| name == *key && !value.is_empty())
                    })
            })
    {
        return Ok(ambient_receipt(target, &[]));
    }
    let selected = config
        .accounts
        .iter()
        .filter(|entry| {
            valid_account_id(&entry.id)
                && resolved_claude_home(&accounts, &entry.id, host.uid()).is_some()
        })
        .min_by_key(|entry| worker_accounts.get(entry.id.as_str()).copied().unwrap_or(0));
    let Some(selected) = selected else {
        return Ok(ambient_receipt(target, &[]));
    };
    let mut receipt = build_verified_receipt(
        host,
        target,
        &accounts,
        selected.clone(),
        "native Claude account spread".into(),
        false,
        "CLAUDE_CONFIG_DIR",
        &[],
        tmux_env_lines,
    )?;
    let current = host
        .load_accounts()?
        .ok_or_else(|| Error::new("Claude spread account changed before launch"))?;
    check_active_home(host, "claude", &current)?;
    require(
        current
            .provider("claude")
            .is_some_and(|config| config.policy == "spread")
            && resolved_claude_home(&current, &selected.id, host.uid()).as_ref()
                == receipt.home_env.as_ref().map(|(_, home)| home),
        "Claude spread account changed before launch",
    )?;
    receipt.claude_auto_observation = receipt
        .home_env
        .as_ref()
        .and_then(|(_, home)| AutoLaunch::new(home, host.uid()));
    Ok(receipt)
}

pub(crate) fn resolve_main_selection(
    host: &dyn AccountHost,
    provider: &str,
    model: Option<&str>,
    alias: &str,
    tmux_env: &str,
) -> Result<AccountSelectionReceipt> {
    let overrides: &[&str] = match provider {
        "codex" => CODEX_MAIN_CREDENTIAL_OVERRIDES,
        "claude" => CLAUDE_MAIN_CREDENTIAL_OVERRIDES,
        _ => return Err(Error::new("--launch --account requires codex or claude")),
    };
    let claude_keys = CLAUDE_CREDENTIAL_OVERRIDES
        .iter()
        .filter(|_| provider == "claude");
    require(
        overrides.iter().chain(claude_keys).all(|key| {
            !host.env_nonempty(key)
                && !tmux_env.lines().any(|line| {
                    line.split_once('=')
                        .is_some_and(|(name, value)| name == *key && !value.is_empty())
                })
        }),
        "credential environment overrides the selected Main account",
    )?;
    resolve_selection(
        host,
        SelectionTarget {
            role: "main",
            provider,
            model: model.unwrap_or("provider-default"),
        },
        Some(alias),
        &[],
        0,
    )
    .map_err(|_| Error::new("requested Main account could not be verified"))
}

pub(crate) fn resolve_codex_alternate_selection(
    host: &dyn AccountHost,
    alias: &str,
    model: &str,
    tmux_env_lines: &[&str],
) -> Result<AccountSelectionReceipt> {
    require(
        CODEX_MAIN_CREDENTIAL_OVERRIDES.iter().all(|key| {
            !host.env_nonempty(key)
                && !tmux_env_lines.iter().any(|line| {
                    line.split_once('=')
                        .is_some_and(|(name, value)| name == *key && !value.is_empty())
                })
        }),
        "credential environment overrides the selected Codex account",
    )?;
    let mut receipt = resolve_selection(
        host,
        SelectionTarget {
            role: "orchestrator",
            provider: "codex",
            model,
        },
        Some(alias),
        tmux_env_lines,
        0,
    )?;
    receipt.scrub_env = CODEX_MAIN_CREDENTIAL_OVERRIDES;
    Ok(receipt)
}

fn binding_dir(canon_repo: &Path, state_home: &Path) -> Result<PathBuf> {
    require(
        state_home.is_absolute()
            && !state_home.starts_with(canon_repo)
            && !state_home
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
        "private state must be normalized and outside repository",
    )?;
    let namespace = path_digest(canon_repo);
    Ok(state_home
        .join("oh-my-setting/panel-control")
        .join(namespace)
        .join("account-bindings"))
}

fn binding_filename(target_role: &str, anchor: &BindingAnchor<'_>) -> String {
    let key = oms_panel_core::digest(
        format!(
            "{target_role}\0{}\0{}\0{}\0{}",
            anchor.room, anchor.main_attempt, anchor.provider, anchor.pane_id
        )
        .as_bytes(),
    );
    format!("{target_role}-{key}.json")
}

fn alternate_binding_filename(anchor: &BindingAnchor<'_>) -> String {
    let key = oms_panel_core::digest(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            anchor.room,
            anchor.main_attempt,
            anchor.session.session,
            anchor.session.server_birth,
            anchor.session.socket_digest,
            anchor.pane_id,
            anchor.pid,
            anchor.birth,
        )
        .as_bytes(),
    );
    format!("orchestrator-alternate-{key}.json")
}

fn binding_directory(
    host: &dyn AccountHost,
    canon_repo: &Path,
    create: bool,
) -> Result<(PathBuf, OwnedFd)> {
    let state_home = host.state_home()?;
    let dir = binding_dir(canon_repo, &state_home)?;
    let base_count = state_home
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    let fd = open_private_dir(&dir, create, base_count + 1, host.uid())?;
    Ok((dir, fd))
}

fn open_private_dir(
    path: &Path,
    create: bool,
    private_start: usize,
    uid: u32,
) -> Result<rustix::fd::OwnedFd> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = fs::open("/", flags, Mode::empty()).map_err(Error::new)?;
    for (index, name) in path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .enumerate()
    {
        let child = match fs::openat(&parent, name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if create => {
                match fs::mkdirat(&parent, name, Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(Error::new(e)),
                }
                fs::openat(&parent, name, flags, Mode::empty())
                    .map_err(|_| Error::new("private directory unavailable or unsafe"))?
            }
            Err(_) => return Err(Error::new("private directory unavailable or unsafe")),
        };
        let named = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(Error::new)?;
        let info = fs::fstat(&child).map_err(Error::new)?;
        require(
            FileType::from_raw_mode(info.st_mode) == FileType::Directory
                && named.st_dev == info.st_dev
                && named.st_ino == info.st_ino,
            "private directory changed",
        )?;
        if index >= private_start {
            require(
                info.st_uid == uid && info.st_mode & 0o077 == 0,
                "unsafe private directory",
            )?;
        } else if index + 1 == private_start {
            require(
                info.st_uid == uid && info.st_mode & 0o022 == 0,
                "unsafe state directory",
            )?;
        }
        parent = child;
    }
    Ok(parent)
}

fn open_codex_home(path: &Path, uid: u32) -> Option<(OwnedFd, String)> {
    let mut parts = path.components();
    if parts.next()? != Component::RootDir
        || parts
            .clone()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let count = parts.count();
    if count == 0 {
        return None;
    }
    let dir = open_private_dir(path, false, count, uid).ok()?;
    let identity = codex_home_identity(&dir, uid)?;
    Some((dir, identity))
}

fn codex_home_identity(dir: &OwnedFd, uid: u32) -> Option<String> {
    let metadata = std::fs::File::from(dir.try_clone().ok()?).metadata().ok()?;
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
        || metadata.ino() == 0
    {
        return None;
    }
    // An inode can be reused. Birth time survives ordinary changes to the
    // directory contents, and a filesystem without it cannot prove this root.
    let birth = metadata
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    if birth.is_zero() {
        return None;
    }
    let parts = format!(
        "codex-home\0{}\0{}\0{}\0{}",
        metadata.dev(),
        metadata.ino(),
        birth.as_secs(),
        birth.subsec_nanos(),
    );
    Some(oms_panel_core::digest(parts.as_bytes()))
}

fn enforce_file_mode_0600(dir_fd: &rustix::fd::OwnedFd, filename: &str, uid: u32) -> Result<()> {
    use rustix::fs::{self, FileType, Mode, OFlags};
    let fd = fs::openat(
        dir_fd,
        filename,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(Error::new)?;
    let info = fs::fstat(&fd).map_err(Error::new)?;
    require(
        FileType::from_raw_mode(info.st_mode) == FileType::RegularFile
            && info.st_nlink == 1
            && info.st_uid == uid,
        "unsafe account binding file",
    )?;
    if info.st_mode & 0o777 != 0o600 {
        fs::fchmod(&fd, Mode::from_raw_mode(0o600)).map_err(Error::new)?;
    }
    Ok(())
}

fn read_private_file(dir_fd: &rustix::fd::OwnedFd, filename: &str, uid: u32) -> Result<Vec<u8>> {
    use rustix::fs::{self, FileType, Mode, OFlags};
    use std::io::Read;
    let fd = fs::openat(
        dir_fd,
        filename,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(Error::new)?;
    let before = fs::fstat(&fd).map_err(Error::new)?;
    require(
        FileType::from_raw_mode(before.st_mode) == FileType::RegularFile
            && before.st_nlink == 1
            && before.st_uid == uid
            && before.st_mode & 0o077 == 0
            && before.st_size >= 0
            && before.st_size as u64 <= MAX_BINDING_BYTES as u64,
        "unsafe or invalid account binding file",
    )?;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_BINDING_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::new)?;
    let after = fs::fstat(&file).map_err(Error::new)?;
    require(
        before.st_dev == after.st_dev
            && before.st_ino == after.st_ino
            && before.st_size == after.st_size
            && after.st_nlink == 1
            && bytes.len() as u64 == before.st_size as u64
            && bytes.len() <= MAX_BINDING_BYTES,
        "account binding file changed while reading",
    )?;
    Ok(bytes)
}

pub fn save_binding(
    host: &dyn AccountHost,
    target_role: &str,
    anchor: &BindingAnchor<'_>,
    receipt: &AccountSelectionReceipt,
) -> Result<()> {
    save_binding_record(host, target_role, anchor, receipt, None, None)
}

fn require_empty_binding_slot(dir: &OwnedFd, filename: &str) -> Result<()> {
    use rustix::fs::{AtFlags, statat};
    use rustix::io::Errno;
    match statat(dir, filename, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) => Err(Error::new(
            "the Codex alternate lead slot already has a binding",
        )),
        Err(error) => Err(Error::new(error)),
    }
}

pub(crate) fn save_alternate_binding(
    host: &dyn AccountHost,
    anchor: &BindingAnchor<'_>,
    receipt: &AccountSelectionReceipt,
    scope: &AlternateLeadScope,
) -> Result<()> {
    save_binding_record(host, "orchestrator", anchor, receipt, None, Some(scope))
}

/// The caller holds the Main launch lock and passes the PID from its unreaped Child.
/// Missing process proof or a write failure leaves the original reuse receipt usable.
pub(crate) fn record_claude_main_child(
    host: &dyn AccountHost,
    anchor: &BindingAnchor<'_>,
    receipt: &AccountSelectionReceipt,
    pid: u32,
) {
    if anchor.provider == "claude"
        && anchor.main_owner == "claude"
        && receipt.verification == AccountVerification::Verified
        && receipt.account_pinned
        && pid != anchor.pid
        && (1..=i32::MAX as u32).contains(&pid)
        && let Ok(owner) = host.process_owner(pid)
        && owner.pid == pid
        && owner.uid == host.uid()
        && !owner.birth.is_empty()
    {
        let child = ClaudeMainChild {
            pid,
            birth: owner.birth,
        };
        let _ = save_binding_record(host, "main", anchor, receipt, Some(child), None);
    }
}

fn save_binding_record(
    host: &dyn AccountHost,
    target_role: &str,
    anchor: &BindingAnchor<'_>,
    receipt: &AccountSelectionReceipt,
    claude_main_child: Option<ClaudeMainChild>,
    alternate_lead: Option<&AlternateLeadScope>,
) -> Result<()> {
    require(
        valid_account_id(target_role)
            && receipt.target_role == target_role
            && receipt.provider == anchor.provider
            && anchor.pid > 0
            && !anchor.birth.is_empty(),
        "invalid account binding anchor or receipt",
    )?;
    if receipt.verification == AccountVerification::Verified {
        require(
            routing::ACCOUNT_PROVIDERS.contains(&anchor.provider)
                && receipt.account_id.as_deref().is_some_and(valid_account_id)
                && receipt.home_digest.as_deref().is_some_and(is_hex64)
                && receipt.home_env.as_ref().map(|(k, _)| *k)
                    == routing::account_home_env(anchor.provider).map(|(k, _)| k),
            "verified account receipt is incomplete",
        )?;
    } else {
        require(
            receipt.account_id.is_none()
                && !receipt.account_pinned
                && receipt.home_digest.is_none()
                && receipt.home_env.is_none(),
            "unverified account receipt cannot carry bound credentials",
        )?;
    }
    let canon_repo = std::fs::canonicalize(anchor.repo).map_err(Error::new)?;
    let repo_str = canon_repo
        .to_str()
        .ok_or_else(|| Error::new("repository path must be UTF-8"))?
        .to_owned();
    let (dir, dir_fd) = binding_directory(
        host,
        &canon_repo,
        claude_main_child.is_none() && alternate_lead.is_none(),
    )?;
    let filename = if alternate_lead.is_some() {
        alternate_binding_filename(anchor)
    } else {
        binding_filename(target_role, anchor)
    };
    let codex_home_identity = receipt
        .codex_home_identity
        .as_ref()
        .filter(|_| {
            target_role == "orchestrator"
                && anchor.provider == "codex"
                && receipt.verification == AccountVerification::Verified
        })
        .and_then(|selected| {
            let (_, home) = receipt.home_env.as_ref()?;
            if receipt.home_digest.as_deref() != Some(path_digest(home).as_str()) {
                return None;
            }
            let (_, current) = open_codex_home(home, host.uid())?;
            (selected == &current).then_some(current)
        });
    if alternate_lead.is_some() {
        require(
            receipt.account_pinned && codex_home_identity.is_some(),
            "the Codex alternate account binding could not be verified",
        )?;
    }
    let mut record = PersistedAccountBinding {
        schema: BINDING_SCHEMA,
        target_role: target_role.to_owned(),
        repo: repo_str,
        uid: host.uid(),
        room: anchor.room.to_owned(),
        main_attempt: anchor.main_attempt.to_owned(),
        main_owner: anchor.main_owner.to_owned(),
        session: anchor.session.clone(),
        provider: anchor.provider.to_owned(),
        pane_id: anchor.pane_id.to_owned(),
        pid: anchor.pid,
        birth: anchor.birth.to_owned(),
        verification: receipt.verification,
        account_id: receipt.account_id.clone(),
        account_pinned: receipt.account_pinned,
        selection_reason: receipt.selection_reason.clone(),
        home_env_key: receipt.home_env.as_ref().map(|(k, _)| (*k).to_owned()),
        home_digest: receipt.home_digest.clone(),
        codex_home_identity,
        claude_observation_identity: receipt
            .claude_observation_identity
            .as_ref()
            .filter(|identity| {
                receipt.home_env.as_ref().is_some_and(|(_, path)| {
                    crate::claude_lead_usage::claude_observation_identity(path, host.uid()).as_ref()
                        == Some(*identity)
                })
            })
            .cloned(),
        claude_auto_observation: receipt
            .claude_auto_observation
            .as_ref()
            .filter(|launch| {
                target_role == "orchestrator"
                    && anchor.provider == "claude"
                    && receipt.verification == AccountVerification::Verified
                    && !receipt.account_pinned
                    && receipt
                        .home_env
                        .as_ref()
                        .is_some_and(|(_, home)| launch.matches_home(home, host.uid()))
            })
            .cloned(),
        claude_main_child: None,
        alternate_lead: alternate_lead.cloned(),
    };
    let store = Store::open(&dir)?;
    let timeout = if claude_main_child.is_some() {
        Duration::from_millis(10)
    } else {
        Duration::from_secs(3)
    };
    let tx = store.lock_at(Path::new("lock"), timeout)?;
    if alternate_lead.is_some() {
        require_empty_binding_slot(&dir_fd, &filename)?;
    }
    if claude_main_child.is_some() {
        let existing = read_private_file(&dir_fd, &filename, host.uid())?;
        let binding: PersistedAccountBinding =
            serde_json::from_value(oms_panel_core::decode(&existing, MAX_BINDING_BYTES)?)
                .map_err(Error::new)?;
        require(
            binding == record,
            "private Main account binding changed during launch",
        )?;
        record.claude_main_child = claude_main_child;
    }
    let value = serde_json::to_value(&record).map_err(Error::new)?;
    tx.write_json(
        Path::new(&filename),
        &value,
        alternate_lead.is_some(),
        MAX_BINDING_BYTES,
    )?;
    enforce_file_mode_0600(&dir_fd, &filename, host.uid())?;
    Ok(())
}

pub fn load_verified_binding(
    host: &dyn AccountHost,
    target_role: &str,
    anchor: &BindingAnchor<'_>,
) -> Option<PersistedAccountBinding> {
    load_binding_record(host, target_role, anchor, None)
}

pub(crate) fn load_alternate_binding(
    host: &dyn AccountHost,
    anchor: &BindingAnchor<'_>,
    scope: &AlternateLeadScope,
) -> Option<PersistedAccountBinding> {
    let binding = load_binding_record(host, "orchestrator", anchor, Some(scope))?;
    (binding.provider == "codex"
        && binding.account_pinned
        && binding.has_codex_chat_home_identity())
    .then_some(binding)
}

fn load_binding_record(
    host: &dyn AccountHost,
    target_role: &str,
    anchor: &BindingAnchor<'_>,
    alternate_lead: Option<&AlternateLeadScope>,
) -> Option<PersistedAccountBinding> {
    if !valid_account_id(target_role) || anchor.pid == 0 || anchor.birth.is_empty() {
        return None;
    }
    let canon_repo = std::fs::canonicalize(anchor.repo).ok()?;
    let repo_str = canon_repo.to_str()?;
    let (_, dir_fd) = binding_directory(host, &canon_repo, false).ok()?;
    let filename = if alternate_lead.is_some() {
        alternate_binding_filename(anchor)
    } else {
        binding_filename(target_role, anchor)
    };
    let bytes = read_private_file(&dir_fd, &filename, host.uid()).ok()?;
    let value = oms_panel_core::decode(&bytes, MAX_BINDING_BYTES).ok()?;
    let binding: PersistedAccountBinding = serde_json::from_value(value).ok()?;
    if binding.schema != BINDING_SCHEMA
        || binding.target_role != target_role
        || binding.repo != repo_str
        || binding.uid != host.uid()
        || binding.room != anchor.room
        || binding.main_attempt != anchor.main_attempt
        || binding.main_owner != anchor.main_owner
        || binding.session != *anchor.session
        || binding.provider != anchor.provider
        || binding.pane_id != anchor.pane_id
        || binding.pid != anchor.pid
        || binding.birth != anchor.birth
        || binding.alternate_lead.as_ref() != alternate_lead
        || binding
            .claude_observation_identity
            .as_deref()
            .is_some_and(|identity| {
                target_role != "orchestrator"
                    || anchor.provider != "claude"
                    || binding.verification != AccountVerification::Verified
                    || !binding.account_pinned
                    || !is_hex64(identity)
            })
        || binding.claude_main_child.as_ref().is_some_and(|child| {
            target_role != "main"
                || anchor.provider != "claude"
                || anchor.main_owner != "claude"
                || binding.verification != AccountVerification::Verified
                || !binding.account_pinned
                || child.pid == anchor.pid
                || !(1..=i32::MAX as u32).contains(&child.pid)
                || child.birth.is_empty()
        })
        || binding
            .claude_auto_observation
            .as_ref()
            .is_some_and(|launch| {
                target_role != "orchestrator"
                    || anchor.provider != "claude"
                    || binding.verification != AccountVerification::Verified
                    || binding.account_pinned
                    || !launch.valid()
            })
        || binding
            .codex_home_identity
            .as_deref()
            .is_some_and(|identity| {
                target_role != "orchestrator"
                    || anchor.provider != "codex"
                    || binding.verification != AccountVerification::Verified
                    || !is_hex64(identity)
            })
    {
        return None;
    }
    match binding.verification {
        AccountVerification::Verified => {
            let expected_env = routing::account_home_env(anchor.provider).map(|(k, _)| k);
            if !routing::ACCOUNT_PROVIDERS.contains(&anchor.provider)
                || !binding.account_id.as_deref().is_some_and(valid_account_id)
                || !binding.home_digest.as_deref().is_some_and(is_hex64)
                || expected_env.is_none()
                || binding.home_env_key.as_deref() != expected_env
            {
                return None;
            }
        }
        AccountVerification::UnpinnedAmbient | AccountVerification::Unverified => {
            if binding.account_id.is_some()
                || binding.account_pinned
                || binding.home_digest.is_some()
                || binding.home_env_key.is_some()
            {
                return None;
            }
        }
    }
    Some(binding)
}

pub(crate) struct CodexChatHome {
    home: PathBuf,
    directory: OwnedFd,
    identity: String,
    uid: u32,
}

impl CodexChatHome {
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn directory(&self) -> &OwnedFd {
        &self.directory
    }

    pub(crate) fn same_directory(&self, other: &Self) -> bool {
        self.identity == other.identity
    }

    /// Checks the open object. Re-resolve the binding to also verify the current name.
    pub(crate) fn matches_opened(&self, opened: &OwnedFd) -> bool {
        codex_home_identity(opened, self.uid).as_deref() == Some(self.identity.as_str())
    }
}

pub(crate) fn resolve_codex_chat_home(
    host: &dyn AccountHost,
    anchor: &BindingAnchor<'_>,
) -> Option<CodexChatHome> {
    if anchor.provider != "codex" {
        return None;
    }
    let uid = host.uid();
    let owner = host.process_owner(anchor.pid).ok()?;
    if owner.uid != uid || owner.pid != anchor.pid || owner.birth != anchor.birth {
        return None;
    }
    let binding = load_verified_binding(host, "orchestrator", anchor)?;
    let home = bound_codex_home(host, &binding)?;
    (host.process_owner(anchor.pid).ok()? == owner).then_some(home)
}

pub(crate) fn bound_codex_home(
    host: &dyn AccountHost,
    binding: &PersistedAccountBinding,
) -> Option<CodexChatHome> {
    configured_codex_home(
        host,
        binding.account_id.as_deref()?,
        binding.home_digest.as_deref()?,
        binding.codex_home_identity.as_deref()?,
    )
}

pub(crate) fn selected_codex_home(
    host: &dyn AccountHost,
    receipt: &AccountSelectionReceipt,
) -> Option<CodexChatHome> {
    configured_codex_home(
        host,
        receipt.account_id.as_deref()?,
        receipt.home_digest.as_deref()?,
        receipt.codex_home_identity.as_deref()?,
    )
}

fn configured_codex_home(
    host: &dyn AccountHost,
    account: &str,
    digest: &str,
    expected: &str,
) -> Option<CodexChatHome> {
    let uid = host.uid();
    let accounts = host.load_accounts().ok()??;
    let configured = accounts.home("codex", account).ok()?;
    check_private_dir(configured, uid).ok()?;
    let home = configured.canonicalize().ok()?;
    if digest != path_digest(&home) || accounts.home_claimed_by_other("codex", &home) {
        return None;
    }
    let (directory, identity) = open_codex_home(&home, uid)?;
    if identity != expected {
        return None;
    }
    Some(CodexChatHome {
        home,
        directory,
        identity,
        uid,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new(label: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "oms-acct-{}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed),
                label
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn private_dir(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    #[derive(Clone)]
    pub(crate) struct FakeHost {
        pub(crate) accounts: Option<routing::Accounts>,
        pub(crate) accounts_err: Option<String>,
        pub(crate) state_home: PathBuf,
        pub(crate) standard_homes: BTreeMap<String, PathBuf>,
        pub(crate) active_homes: BTreeMap<String, PathBuf>,
        pub(crate) env_set: BTreeSet<String>,
        pub(crate) owners: BTreeMap<u32, Owner>,
        pub(crate) reused_pid: std::cell::Cell<Option<u32>>,
        pub(crate) uid: u32,
        pub(crate) readings: BTreeMap<String, Value>,
    }

    impl FakeHost {
        pub(crate) fn new(state_home: PathBuf) -> Self {
            Self {
                accounts: None,
                accounts_err: None,
                state_home,
                standard_homes: BTreeMap::new(),
                active_homes: BTreeMap::new(),
                env_set: BTreeSet::new(),
                owners: BTreeMap::new(),
                reused_pid: Default::default(),
                uid: rustix::process::getuid().as_raw(),
                readings: BTreeMap::new(),
            }
        }
    }

    impl AccountHost for FakeHost {
        fn load_accounts(&self) -> Result<Option<routing::Accounts>> {
            if let Some(msg) = &self.accounts_err {
                return Err(Error::new(msg.clone()));
            }
            Ok(self.accounts.clone())
        }

        fn state_home(&self) -> Result<PathBuf> {
            Ok(self.state_home.clone())
        }

        fn standard_home(&self, provider: &str) -> Option<PathBuf> {
            self.standard_homes.get(provider).cloned()
        }

        fn active_home(&self, provider: &str) -> Option<PathBuf> {
            self.active_homes.get(provider).cloned()
        }

        fn env_nonempty(&self, key: &str) -> bool {
            self.env_set.contains(key)
        }

        fn process_owner(&self, pid: u32) -> Result<Owner> {
            let mut owner = self
                .owners
                .get(&pid)
                .cloned()
                .ok_or_else(|| Error::new("native process generation unavailable"))?;
            if self.reused_pid.get() == Some(pid) {
                owner.birth.push_str("-reused");
            }
            Ok(owner)
        }

        fn uid(&self) -> u32 {
            self.uid
        }

        fn fresh_account_readings(&self, provider: &str, _now: u64) -> Option<Value> {
            self.readings.get(provider).cloned()
        }
    }

    pub(crate) type AccountSpec<'a> = (&'a str, &'a str, Vec<(&'a str, PathBuf)>);

    pub(crate) fn sample_accounts(entries: Vec<AccountSpec<'_>>) -> routing::Accounts {
        let mut providers = BTreeMap::new();
        for (provider, policy, list) in entries {
            providers.insert(
                provider.to_owned(),
                routing::ProviderAccounts {
                    policy: policy.to_owned(),
                    accounts: list
                        .into_iter()
                        .map(|(id, home)| routing::Account {
                            id: id.to_owned(),
                            home,
                        })
                        .collect(),
                },
            );
        }
        routing::Accounts { providers }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeHost, TempDir, private_dir, sample_accounts};
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn session_proof() -> terminals::SessionProof {
        terminals::SessionProof {
            session: "$1".into(),
            server_pid: 4242,
            server_birth: "99999".into(),
            socket_digest: "a".repeat(64),
        }
    }

    #[test]
    fn native_claude_spread_defers_without_proof_and_rejects_foreign_homes() {
        let tmp = TempDir::new("native-claude-spread-selection");
        let repo = private_dir(&tmp.0.join("repo"));
        let first = private_dir(&tmp.0.join("first"));
        let second = private_dir(&tmp.0.join("second"));
        let foreign = private_dir(&repo.join("foreign"));
        let unsafe_home = private_dir(&tmp.0.join("unsafe"));
        std::fs::set_permissions(&unsafe_home, std::fs::Permissions::from_mode(0o775)).unwrap();
        let mut host = FakeHost::new(tmp.0.join("state"));
        let registry = |policy: &str| {
            sample_accounts(vec![
                (
                    "claude",
                    policy,
                    vec![
                        ("unsafe", unsafe_home.clone()),
                        ("one", first.clone()),
                        ("two", second.clone()),
                    ],
                ),
                ("codex", "ordered", vec![("codex-home", foreign.clone())]),
            ])
        };
        let load = BTreeMap::from([("one", 1), ("unknown", 7)]);
        let select =
            |host: &FakeHost, env: &[&str]| resolve_claude_spread(host, &repo, "opus", &load, env);
        let ambient = |receipt: AccountSelectionReceipt| {
            assert_eq!(receipt.verification, AccountVerification::UnpinnedAmbient);
            assert!(
                !receipt.account_pinned
                    && receipt.account_id.is_none()
                    && receipt.home_env.is_none()
            );
        };
        ambient(select(&host, &[]).unwrap());
        host.accounts = Some(registry("ordered"));
        ambient(select(&host, &[]).unwrap());
        host.accounts = Some(registry("spread"));
        host.readings.insert(
            "claude".into(),
            json!({"one":{"weekly_used":0},"two":{"weekly_used":99}}),
        );
        let chosen = select(&host, &[]).unwrap();
        assert_eq!(chosen.account_id.as_deref(), Some("two"));
        assert_eq!(chosen.verification, AccountVerification::Verified);
        assert!(!chosen.account_pinned && chosen.claude_observation_identity.is_none());
        assert_eq!(
            resolve_claude_spread(&host, &repo, "opus", &BTreeMap::new(), &[])
                .unwrap()
                .account_id
                .as_deref(),
            Some("one")
        );
        let own = format!("CLAUDE_CONFIG_DIR={}", first.display());
        ambient(select(&host, &[&own]).unwrap());
        assert!(select(&host, &["CLAUDE_CONFIG_DIR=foreign"]).is_err());
        ambient(select(&host, &["CLAUDE_CONFIG_DIR=foreign", &own]).unwrap());
        ambient(select(&host, &["CLAUDE_CONFIG_DIR=foreign", "CLAUDE_CONFIG_DIR="]).unwrap());
        assert_eq!(
            select(&host, &["CLAUDE_CONFIG_DIR=foreign", "-CLAUDE_CONFIG_DIR"])
                .unwrap()
                .account_id
                .as_deref(),
            Some("two")
        );
        for key in [
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_USE_VERTEX",
            CLAUDE_CREDENTIAL_OVERRIDES[0],
        ] {
            host.env_set.insert(key.into());
            ambient(select(&host, &[]).unwrap());
            host.env_set.clear();
            ambient(select(&host, &[&format!("{key}=fixture")]).unwrap());
        }
        host.active_homes.insert("claude".into(), foreign);
        assert!(select(&host, &[]).is_err());
        host.active_homes.clear();
        host.accounts = Some(sample_accounts(vec![(
            "claude",
            "spread",
            vec![("unsafe", unsafe_home)],
        )]));
        ambient(select(&host, &[]).unwrap());
        host.accounts_err = Some("fixture malformed registry".into());
        assert_eq!(
            select(&host, &[]).err().unwrap().message,
            "fixture malformed registry"
        );
    }

    #[test]
    fn explicit_main_account_requires_config_and_no_known_credential_override() {
        let tmp = TempDir::new("main-selection");
        let mut host = FakeHost::new(tmp.0.join("state"));
        let first = private_dir(&tmp.0.join("first"));
        let second = private_dir(&tmp.0.join("second"));
        let claude = private_dir(&tmp.0.join("claude"));
        assert!(resolve_main_selection(&host, "codex", None, "first", "").is_err());
        host.accounts_err = Some("private account configuration contents".into());
        let result = resolve_main_selection(&host, "codex", None, "first", "");
        assert_eq!(
            result.err().unwrap().message,
            "requested Main account could not be verified"
        );
        host.accounts_err = None;
        host.accounts = Some(sample_accounts(vec![
            (
                "codex",
                "manual",
                vec![("first", first.clone()), ("second", second.clone())],
            ),
            ("claude", "manual", vec![("first", claude.clone())]),
        ]));
        host.standard_homes.insert("claude".into(), claude);
        for (alias, home) in [("first", first), ("second", second)] {
            let receipt = resolve_main_selection(&host, "codex", None, alias, "").unwrap();
            assert_eq!(receipt.target_role, "main");
            assert_eq!(receipt.account_id.as_deref(), Some(alias));
            assert_eq!(receipt.verification, AccountVerification::Verified);
            assert!(
                receipt
                    .home_env
                    .is_some_and(|(key, selected)| key == "CODEX_HOME" && selected == home)
            );
        }
        for (provider, key) in [
            ("codex", CODEX_MAIN_CREDENTIAL_OVERRIDES[0]),
            ("codex", concat!("CODEX_ACCESS_", "TOKEN")),
            ("claude", CLAUDE_CREDENTIAL_OVERRIDES[0]),
            ("claude", CLAUDE_MAIN_CREDENTIAL_OVERRIDES[0]),
        ] {
            assert!(
                resolve_main_selection(&host, provider, None, "first", &format!("{key}=fixture"))
                    .is_err()
            );
            host.env_set.insert(key.into());
            assert!(resolve_main_selection(&host, provider, None, "first", "").is_err());
            host.env_set.remove(key);
        }
    }

    #[test]
    fn pi_main_and_orchestrator_share_selection_receipt_contract() {
        let tmp = TempDir::new("pi-main");
        let pi_work = private_dir(&tmp.0.join("pi-work"));
        let mut host = FakeHost::new(tmp.0.join("state"));
        host.accounts = Some(sample_accounts(vec![(
            "pi",
            "ordered",
            vec![("work", pi_work.clone())],
        )]));

        let receipt = resolve_selection(
            &host,
            SelectionTarget {
                role: "main",
                provider: "pi",
                model: "vendor/model-x",
            },
            Some("work"),
            &[],
            10_000,
        )
        .unwrap();
        assert_eq!(receipt.target_role, "main");
        assert_eq!(receipt.provider, "pi");
        assert_eq!(receipt.verification, AccountVerification::Verified);
        assert_eq!(receipt.account_id.as_deref(), Some("work"));
        assert!(receipt.account_pinned);
        assert_eq!(
            receipt.home_env,
            Some(("PI_CODING_AGENT_DIR", pi_work.clone()))
        );
        assert_eq!(receipt.home_digest, Some(path_digest(&pi_work)));
        assert_eq!(receipt.scrub_env, &["PI_CODING_AGENT_SESSION_DIR"]);

        let unpinned_pi = resolve_selection(
            &host,
            SelectionTarget {
                role: "main",
                provider: "pi",
                model: "vendor/model-x",
            },
            None,
            &[],
            10_000,
        )
        .unwrap();
        assert_eq!(
            unpinned_pi.verification,
            AccountVerification::UnpinnedAmbient
        );
        assert_eq!(unpinned_pi.home_env, None);
        assert_eq!(unpinned_pi.scrub_env, &["PI_CODING_AGENT_SESSION_DIR"]);
    }

    #[test]
    fn prelaunch_refusals_cover_aliases_modes_models_claude_overrides_and_agy() {
        let tmp = TempDir::new("refusals");
        let codex_a = private_dir(&tmp.0.join("codex-a"));
        let codex_open = tmp.0.join("codex-open");
        std::fs::create_dir_all(&codex_open).unwrap();
        std::fs::set_permissions(&codex_open, std::fs::Permissions::from_mode(0o775)).unwrap();
        let codex_link = tmp.0.join("codex-link");
        std::os::unix::fs::symlink(&codex_a, &codex_link).unwrap();

        let claude_std = private_dir(&tmp.0.join("claude-std"));
        let claude_work = private_dir(&tmp.0.join("claude-work"));

        let mut host = FakeHost::new(tmp.0.join("state"));
        host.standard_homes
            .insert("claude".into(), claude_std.clone());
        host.accounts = Some(sample_accounts(vec![
            (
                "codex",
                "ordered",
                vec![
                    ("acct_a", codex_a.clone()),
                    ("open_dir", codex_open),
                    ("sym_dir", codex_link),
                ],
            ),
            (
                "claude",
                "ordered",
                vec![("default", claude_std), ("work", claude_work)],
            ),
        ]));

        // Antigravity rejects --account; allows unpinned as Unverified.
        assert!(
            resolve_selection(
                &host,
                SelectionTarget {
                    role: "orchestrator",
                    provider: "antigravity",
                    model: "gemini-pro-orchestrator",
                },
                Some("acct_a"),
                &[],
                10_000,
            )
            .unwrap_err()
            .message
            .contains("unproved")
        );
        let agy = resolve_selection(
            &host,
            SelectionTarget {
                role: "orchestrator",
                provider: "antigravity",
                model: "gemini-pro-orchestrator",
            },
            None,
            &[],
            10_000,
        )
        .unwrap();
        assert_eq!(agy.verification, AccountVerification::Unverified);
        assert_eq!(agy.home_env, None);

        // Unknown alias and unregistered "default" for codex are refused.
        for alias in ["missing", "default"] {
            assert!(
                resolve_selection(
                    &host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider: "codex",
                        model: "gpt-6.2-sol",
                    },
                    Some(alias),
                    &[],
                    10_000,
                )
                .is_err(),
                "{alias}"
            );
        }

        // Non-private directory mode (0o755) and symlink home are refused.
        for alias in ["open_dir", "sym_dir"] {
            assert!(
                resolve_selection(
                    &host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider: "codex",
                        model: "gpt-6.2-sol",
                    },
                    Some(alias),
                    &[],
                    10_000,
                )
                .is_err(),
                "{alias}"
            );
        }

        // Foreign UID is refused.
        host.uid = rustix::process::getuid().as_raw() + 1;
        assert!(
            resolve_selection(
                &host,
                SelectionTarget {
                    role: "orchestrator",
                    provider: "codex",
                    model: "gpt-6.2-sol",
                },
                Some("acct_a"),
                &[],
                10_000,
            )
            .is_err()
        );
        host.uid = rustix::process::getuid().as_raw();

        // Missing model in models_cache.json is refused.
        std::fs::write(
            codex_a.join("models_cache.json"),
            br#"{"models":[{"slug":"gpt-6.1-sol"}]}"#,
        )
        .unwrap();
        assert!(
            resolve_selection(
                &host,
                SelectionTarget {
                    role: "orchestrator",
                    provider: "codex",
                    model: "gpt-6.2-sol",
                },
                Some("acct_a"),
                &[],
                10_000,
            )
            .is_err()
        );
        std::fs::write(
            codex_a.join("models_cache.json"),
            br#"{"models":[{"slug":"gpt-6.2-sol"}]}"#,
        )
        .unwrap();
        assert!(
            resolve_selection(
                &host,
                SelectionTarget {
                    role: "orchestrator",
                    provider: "codex",
                    model: "gpt-6.2-sol",
                },
                Some("acct_a"),
                &[],
                10_000,
            )
            .is_ok()
        );

        // Claude non-default credential overrides in host env and tmux env lines.
        for key in CLAUDE_CREDENTIAL_OVERRIDES {
            host.env_set.insert((*key).into());
            assert!(
                resolve_selection(
                    &host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider: "claude",
                        model: "opus",
                    },
                    Some("work"),
                    &[],
                    10_000,
                )
                .is_err()
            );
            // Default home stays allowed even with credential env set.
            assert!(
                resolve_selection(
                    &host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider: "claude",
                        model: "opus",
                    },
                    Some("default"),
                    &[],
                    10_000,
                )
                .is_ok()
            );
            host.env_set.clear();

            let line = format!("{key}=fixture-present");
            assert!(
                resolve_selection(
                    &host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider: "claude",
                        model: "opus",
                    },
                    Some("work"),
                    &[&line],
                    10_000,
                )
                .is_err()
            );
            // Empty value in tmux env does not override.
            let empty_line = format!("{key}=");
            assert!(
                resolve_selection(
                    &host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider: "claude",
                        model: "opus",
                    },
                    Some("work"),
                    &[&empty_line],
                    10_000,
                )
                .is_ok()
            );
        }

        // Cross-provider active home claim in unpinned mode is refused.
        host.active_homes.insert("claude".into(), codex_a.clone());
        assert!(
            resolve_selection(
                &host,
                SelectionTarget {
                    role: "orchestrator",
                    provider: "claude",
                    model: "opus",
                },
                None,
                &[],
                10_000,
            )
            .is_err()
        );
    }

    #[test]
    fn unpinned_codex_uses_fresh_exact_ranking_or_falls_back_to_ambient() {
        let tmp = TempDir::new("codex-ranking");
        let a = private_dir(&tmp.0.join("codex-a"));
        let b = private_dir(&tmp.0.join("codex-b"));
        let c = private_dir(&tmp.0.join("codex-c"));
        std::fs::write(
            c.join("models_cache.json"),
            br#"{"models":[{"slug":"gpt-6.1-sol"}]}"#,
        )
        .unwrap();

        let mut host = FakeHost::new(tmp.0.join("state"));
        host.accounts = Some(sample_accounts(vec![(
            "codex",
            "ordered",
            vec![("acct-a", a), ("acct-b", b.clone()), ("acct-c", c)],
        )]));

        let now = 1_700_000_000u64;
        let target = SelectionTarget {
            role: "orchestrator",
            provider: "codex",
            model: "gpt-6.2-sol",
        };

        // Missing readings -> UnpinnedAmbient.
        let ambient = resolve_selection(&host, target, None, &[], now).unwrap();
        assert_eq!(ambient.verification, AccountVerification::UnpinnedAmbient);
        assert_eq!(ambient.home_env, None);

        // One stale reading (> 600s) -> UnpinnedAmbient.
        host.readings.insert(
            "codex".into(),
            json!({
                "acct-a": { "weekly_used": 65, "observed_at": now - 60, "resets_at": now + 3600 },
                "acct-b": { "weekly_used": 12, "observed_at": now - 601, "resets_at": now + 3600 },
            }),
        );
        assert_eq!(
            resolve_selection(&host, target, None, &[], now)
                .unwrap()
                .verification,
            AccountVerification::UnpinnedAmbient
        );

        // Expired resets_at (<= now) -> UnpinnedAmbient.
        host.readings.insert(
            "codex".into(),
            json!({
                "acct-a": { "weekly_used": 65, "observed_at": now - 60, "resets_at": now + 3600 },
                "acct-b": { "weekly_used": 12, "observed_at": now - 60, "resets_at": now },
            }),
        );
        assert_eq!(
            resolve_selection(&host, target, None, &[], now)
                .unwrap()
                .verification,
            AccountVerification::UnpinnedAmbient
        );

        // Fresh readings for every eligible account (acct-c skipped due to model filter).
        host.readings.insert(
            "codex".into(),
            json!({
                "acct-a": { "weekly_used": 65, "observed_at": now - 60, "resets_at": now + 3600 },
                "acct-b": { "weekly_used": 12, "observed_at": now - 600, "resets_at": now + 3600 },
            }),
        );
        let picked = resolve_selection(&host, target, None, &[], now).unwrap();
        assert_eq!(picked.verification, AccountVerification::Verified);
        assert!(!picked.account_pinned);
        assert_eq!(picked.account_id.as_deref(), Some("acct-b"));
        assert_eq!(picked.home_env, Some(("CODEX_HOME", b.clone())));
        assert_eq!(picked.home_digest, Some(path_digest(&b)));
    }

    #[test]
    fn persisted_bindings_enforce_private_modes_no_path_leak_and_full_anchor_match() {
        let tmp = TempDir::new("binding-store");
        let repo = private_dir(&tmp.0.join("repo"));
        let state_home = private_dir(&tmp.0.join("state"));
        let acct_home = private_dir(&tmp.0.join("secret-codex-home"));

        let mut host = FakeHost::new(state_home.clone());
        host.accounts = Some(sample_accounts(vec![(
            "codex",
            "ordered",
            vec![("work", acct_home.clone())],
        )]));

        let receipt = resolve_selection(
            &host,
            SelectionTarget {
                role: "orchestrator",
                provider: "codex",
                model: "gpt-6.2-sol",
            },
            Some("work"),
            &[],
            10_000,
        )
        .unwrap();

        let proof = session_proof();
        let anchor = BindingAnchor {
            repo: &repo,
            room: "room-1",
            main_attempt: "att_11111111111111111111111111111111",
            main_owner: "codex",
            session: &proof,
            provider: "codex",
            pane_id: "%4",
            pid: 5555,
            birth: "88888",
        };
        save_binding(&host, "orchestrator", &anchor, &receipt).unwrap();

        let dir = binding_dir(&repo, &state_home).unwrap();
        let filename = binding_filename("orchestrator", &anchor);
        let file_path = dir.join(&filename);
        let meta = std::fs::symlink_metadata(&file_path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        let raw_bytes = std::fs::read_to_string(&file_path).unwrap();
        assert!(!raw_bytes.contains(acct_home.to_str().unwrap()));
        if std::fs::metadata(&acct_home)
            .unwrap()
            .created()
            .is_ok_and(|created| created > std::time::UNIX_EPOCH)
        {
            let private: Value = serde_json::from_str(&raw_bytes).unwrap();
            assert!(
                private["codex_home_identity"]
                    .as_str()
                    .is_some_and(is_hex64)
            );
        }

        let loaded = load_verified_binding(&host, "orchestrator", &anchor).unwrap();
        assert_eq!(loaded.verification, AccountVerification::Verified);
        assert_eq!(loaded.account_id.as_deref(), Some("work"));
        assert!(loaded.account_pinned);
        assert_eq!(loaded.home_env_key.as_deref(), Some("CODEX_HOME"));
        assert_eq!(loaded.home_digest, Some(path_digest(&acct_home)));

        // Every anchor mismatch must invalidate.
        let other_repo = private_dir(&tmp.0.join("other-repo"));
        let mut wrong_session = proof.clone();
        wrong_session.server_birth = "77777".into();
        let mut wrong_socket = proof.clone();
        wrong_socket.socket_digest = "b".repeat(64);
        assert!(load_verified_binding(&host, "main", &anchor).is_none());
        for bad_anchor in [
            BindingAnchor {
                repo: &other_repo,
                ..anchor
            },
            BindingAnchor {
                room: "room-2",
                ..anchor
            },
            BindingAnchor {
                main_attempt: "att_22222222222222222222222222222222",
                ..anchor
            },
            BindingAnchor {
                main_owner: "claude",
                ..anchor
            },
            BindingAnchor {
                session: &wrong_session,
                ..anchor
            },
            BindingAnchor {
                session: &wrong_socket,
                ..anchor
            },
            BindingAnchor {
                provider: "claude",
                ..anchor
            },
            BindingAnchor {
                pane_id: "%5",
                ..anchor
            },
            BindingAnchor {
                pid: 5556,
                ..anchor
            },
            BindingAnchor {
                birth: "88889",
                ..anchor
            },
        ] {
            assert!(load_verified_binding(&host, "orchestrator", &bad_anchor).is_none());
        }

        // Foreign UID on host rejects load.
        let mut foreign_host = host.clone();
        foreign_host.uid += 1;
        assert!(load_verified_binding(&foreign_host, "orchestrator", &anchor).is_none());

        // Group/world readable file mode rejects load.
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_none());
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_some());

        let linked = dir.join("linked-copy.json");
        std::fs::hard_link(&file_path, &linked).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_none());
        std::fs::remove_file(linked).unwrap();

        // Unknown JSON field rejects load.
        let mut parsed: Value = serde_json::from_str(&raw_bytes).unwrap();
        parsed["extra_authority"] = json!(true);
        std::fs::write(&file_path, serde_json::to_vec(&parsed).unwrap()).unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_none());

        // Verified missing account_id or home_digest rejects load.
        let mut incomplete: Value = serde_json::from_str(&raw_bytes).unwrap();
        incomplete["account_id"] = Value::Null;
        std::fs::write(&file_path, serde_json::to_vec(&incomplete).unwrap()).unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_none());

        // Symlinked binding file rejects load.
        let real_copy = dir.join("real-copy.json");
        std::fs::write(&real_copy, &raw_bytes).unwrap();
        std::fs::set_permissions(&real_copy, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&file_path).unwrap();
        std::os::unix::fs::symlink(&real_copy, &file_path).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_none());
        std::fs::remove_file(&file_path).unwrap();
        std::fs::rename(&real_copy, &file_path).unwrap();

        // Loose directory permissions on account-bindings reject load.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_none());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(load_verified_binding(&host, "orchestrator", &anchor).is_some());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn claude_observation_checks_private_launch_home_without_changing_routing() {
        let tmp = TempDir::new("claude-launch-origin");
        let repo = private_dir(&tmp.0.join("repo"));
        let home = private_dir(&tmp.0.join("selected"));
        let other = private_dir(&tmp.0.join("alternate"));
        let mut host = FakeHost::new(private_dir(&tmp.0.join("state")));
        let registry = |home: &Path| {
            sample_accounts(vec![("claude", "manual", vec![("chosen", home.into())])])
        };
        host.accounts = Some(registry(&home));
        let select = |host: &FakeHost, env: &[&str]| {
            resolve_selection(
                host,
                SelectionTarget {
                    role: "orchestrator",
                    provider: "claude",
                    model: "opus",
                },
                Some("chosen"),
                env,
                1_000_000,
            )
            .unwrap()
        };
        let receipt = select(&host, &[]);
        let with_gateway = select(&host, &["CLAUDE_CODE_USE_BEDROCK=1"]);
        assert!(with_gateway.claude_observation_identity.is_none());
        assert_eq!(with_gateway.home_env, receipt.home_env);
        let Some(identity) = receipt.claude_observation_identity.as_deref() else {
            return;
        };
        let session = session_proof();
        let anchor = BindingAnchor {
            repo: &repo,
            room: "room-fixture",
            main_attempt: "main-fixture",
            main_owner: "codex",
            session: &session,
            provider: "claude",
            pane_id: "%2",
            pid: 4242,
            birth: "fixture:123",
        };
        save_binding(&host, "orchestrator", &anchor, &receipt).unwrap();
        let binding = load_verified_binding(&host, "orchestrator", &anchor).unwrap();
        assert_eq!(
            binding.claude_observation_identity.as_deref(),
            Some(identity)
        );
        assert!(
            !serde_json::to_string(&binding)
                .unwrap()
                .contains(home.to_str().unwrap())
        );
        assert!(claude_observation_home(&host, &binding).is_some());
        private_dir(&home.join("sessions"));
        assert!(claude_observation_home(&host, &binding).is_some());
        host.accounts = Some(registry(&other));
        assert!(claude_observation_home(&host, &binding).is_none());
        host.accounts = Some(registry(&home));
        std::fs::rename(&home, tmp.0.join("renamed")).unwrap();
        private_dir(&home);
        assert!(claude_observation_home(&host, &binding).is_none());
        assert!(claude_binding_still_mapped(&host, &binding));
    }

    #[test]
    fn alternate_codex_chat_requires_live_owner_registry_and_launch_directory() {
        let tmp = TempDir::new("codex-chat-proof");
        let repo = private_dir(&tmp.0.join("repo"));
        let home = private_dir(&tmp.0.join("bound-home"));
        let other = private_dir(&tmp.0.join("other-home"));
        let mut host = FakeHost::new(private_dir(&tmp.0.join("state")));
        let configured = |path: &Path| {
            Some(sample_accounts(vec![(
                "codex",
                "ordered",
                vec![("work", path.to_path_buf())],
            )]))
        };
        host.accounts = configured(&home);
        let session = session_proof();
        let anchor = BindingAnchor {
            repo: &repo,
            room: "room-1",
            main_attempt: "att_11111111111111111111111111111111",
            main_owner: "claude",
            session: &session,
            provider: "codex",
            pane_id: "%4",
            pid: 5555,
            birth: "launch-birth",
        };
        let owner = Owner {
            pid: anchor.pid,
            birth: anchor.birth.into(),
            uid: host.uid,
        };
        host.owners.insert(owner.pid, owner.clone());
        let target = SelectionTarget {
            role: "orchestrator",
            provider: "codex",
            model: "gpt-6.2-sol",
        };
        let receipt = resolve_selection(&host, target, Some("work"), &[], 10_000).unwrap();
        std::fs::create_dir(home.join("sessions")).unwrap();
        save_binding(&host, "orchestrator", &anchor, &receipt).unwrap();
        let store = binding_dir(&repo, &host.state_home).unwrap();
        let record = store.join(binding_filename("orchestrator", &anchor));
        let original = std::fs::read(&record).unwrap();
        let mut legacy: Value = serde_json::from_slice(&original).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("codex_home_identity");
        std::fs::write(&record, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(
            !load_verified_binding(&host, "orchestrator", &anchor)
                .unwrap()
                .has_codex_chat_home_identity()
        );
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        std::fs::write(&record, &original).unwrap();

        if std::fs::metadata(&home)
            .unwrap()
            .created()
            .ok()
            .is_none_or(|created| created <= std::time::UNIX_EPOCH)
        {
            assert!(receipt.codex_home_identity.is_none());
            assert!(resolve_codex_chat_home(&host, &anchor).is_none());
            return;
        }
        let proof = resolve_codex_chat_home(&host, &anchor).expect("current verified account home");
        assert!(proof.home() == home);
        assert!(proof.matches_opened(proof.directory()));
        std::fs::write(home.join("sessions/rollout-fixture.jsonl"), b"{}\n").unwrap();
        assert!(resolve_codex_chat_home(&host, &anchor).is_some());

        for unsupported in [
            BindingAnchor {
                room: "room-other",
                ..anchor
            },
            BindingAnchor {
                provider: "claude",
                ..anchor
            },
            BindingAnchor {
                birth: "other-birth",
                ..anchor
            },
        ] {
            assert!(resolve_codex_chat_home(&host, &unsupported).is_none());
        }
        assert!(
            resolve_codex_chat_home(
                &host,
                &BindingAnchor {
                    pane_id: "%5",
                    ..anchor
                }
            )
            .is_none()
        );
        host.owners.insert(
            owner.pid,
            Owner {
                uid: owner.uid.wrapping_add(1),
                ..owner.clone()
            },
        );
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        host.owners.insert(
            owner.pid,
            Owner {
                birth: "replacement-birth".into(),
                ..owner.clone()
            },
        );
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        host.owners.insert(owner.pid, owner);

        host.accounts = configured(&other);
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        host.accounts = Some(sample_accounts(vec![(
            "codex",
            "ordered",
            vec![("renamed-alias", home.clone())],
        )]));
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        host.accounts = None;
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        host.accounts = configured(&home);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        assert!(!proof.matches_opened(proof.directory()));
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();

        let original_home = tmp.0.join("original-home");
        std::fs::rename(&home, &original_home).unwrap();
        private_dir(&home);
        assert!(proof.matches_opened(proof.directory()));
        let (replacement, _) = open_codex_home(&home, host.uid).unwrap();
        assert!(!proof.matches_opened(&replacement));
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        // A save delayed past replacement retains only the legacy binding.
        save_binding(&host, "orchestrator", &anchor, &receipt).unwrap();
        assert!(
            load_verified_binding(&host, "orchestrator", &anchor)
                .unwrap()
                .codex_home_identity
                .is_none()
        );
        std::fs::write(&record, &original).unwrap();
        std::fs::remove_dir(&home).unwrap();

        std::os::unix::fs::symlink(&original_home, &home).unwrap();
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        std::fs::remove_file(&home).unwrap();
        let impostor = tmp.0.join("regular-file");
        std::fs::write(&impostor, b"fixture").unwrap();
        std::fs::hard_link(&impostor, &home).unwrap();
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
        std::fs::remove_file(&home).unwrap();
        std::fs::rename(&original_home, &home).unwrap();
        assert!(resolve_codex_chat_home(&host, &anchor).is_some());

        let unbound = resolve_selection(&host, target, None, &[], 10_000).unwrap();
        assert_eq!(unbound.verification, AccountVerification::UnpinnedAmbient);
        save_binding(&host, "orchestrator", &anchor, &unbound).unwrap();
        assert!(resolve_codex_chat_home(&host, &anchor).is_none());
    }

    #[test]
    fn load_configured_accounts_from_roots_handles_missing_broken_malformed_and_valid() {
        let tmp = TempDir::new("config-roots");
        let home_missing_cfg = private_dir(&tmp.0.join("home-missing-cfg"));
        let home_missing_oms = private_dir(&tmp.0.join("home-missing-oms"));
        std::fs::create_dir_all(home_missing_oms.join(".config")).unwrap();

        assert!(load_configured_accounts_from_roots(None, None).is_err());
        assert!(load_configured_accounts_from_roots(Some(Path::new("")), None).is_err());
        assert!(load_configured_accounts_from_roots(Some(Path::new("rel")), None).is_err());

        assert!(
            load_configured_accounts_from_roots(Some(&home_missing_cfg), None)
                .unwrap()
                .is_none()
        );
        assert!(
            load_configured_accounts_from_roots(Some(&home_missing_oms), None)
                .unwrap()
                .is_none()
        );

        // Broken symlink at .config fails closed.
        let home_broken_cfg = private_dir(&tmp.0.join("home-broken-cfg"));
        std::os::unix::fs::symlink(
            tmp.0.join("nonexistent-target"),
            home_broken_cfg.join(".config"),
        )
        .unwrap();
        assert!(load_configured_accounts_from_roots(Some(&home_broken_cfg), None).is_err());

        // Non-directory file at .config fails closed.
        let home_file_cfg = private_dir(&tmp.0.join("home-file-cfg"));
        std::fs::write(home_file_cfg.join(".config"), b"not a dir").unwrap();
        assert!(load_configured_accounts_from_roots(Some(&home_file_cfg), None).is_err());

        // Broken symlink at .config/oh-my-setting fails closed.
        let home_broken_oms = private_dir(&tmp.0.join("home-broken-oms"));
        std::fs::create_dir_all(home_broken_oms.join(".config")).unwrap();
        std::os::unix::fs::symlink(
            tmp.0.join("nonexistent-oms"),
            home_broken_oms.join(".config/oh-my-setting"),
        )
        .unwrap();
        assert!(load_configured_accounts_from_roots(Some(&home_broken_oms), None).is_err());

        // Malformed provider-accounts.json fails closed.
        let home_bad_json = private_dir(&tmp.0.join("home-bad-json"));
        let bad_oms = home_bad_json.join(".config/oh-my-setting");
        std::fs::create_dir_all(&bad_oms).unwrap();
        std::fs::write(bad_oms.join("provider-accounts.json"), b"{not valid json").unwrap();
        assert!(load_configured_accounts_from_roots(Some(&home_bad_json), None).is_err());

        // Valid provider-accounts.json loads via default .config and via XDG_CONFIG_HOME.
        let acct_home = private_dir(&tmp.0.join("acct-work"));
        let xdg_root = private_dir(&tmp.0.join("xdg-cfg"));
        let oms_dir = xdg_root.join("oh-my-setting");
        std::fs::create_dir_all(&oms_dir).unwrap();
        let cfg = json!({
            "schema": 1,
            "providers": {
                "codex": {
                    "accounts": [
                        { "id": "work", "home": acct_home }
                    ]
                }
            }
        });
        std::fs::write(
            oms_dir.join("provider-accounts.json"),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
        let loaded = load_configured_accounts_from_roots(Some(&home_missing_cfg), Some(&xdg_root))
            .unwrap()
            .expect("accounts must load from xdg_config_home");
        assert_eq!(loaded.home("codex", "work").unwrap(), acct_home.as_path());
    }
}
