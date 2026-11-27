//! Provider identity, route selection, bounded native envelopes and account
//! selection. A model family is distinct from the executable transport.
//! Custom transports use only marked adapters and require explicit write opt-in.
//! Aider, Antigravity and custom read calls require disposable repository state;
//! private prompt and environment files are removed when execution returns.
use crate::{
    Context, Error, Result, V, fail, number,
    process::{Completed, Invocation},
    storage, text,
};
use sha2::{Digest, Sha256};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Target {
    pub provider: String,
    pub model: String,
    pub account: String,
    pub optional: bool,
    /// Explicit seat-level effort for an exact model, including provider default.
    effort: Option<&'static str>,
}
impl Target {
    pub fn parse(raw: &str) -> Result<Self> {
        let (raw, optional) = raw
            .strip_suffix(":optional")
            .map_or((raw, false), |s| (s, true));
        let (provider, rest) = match raw.find(':') {
            Some(at) => (&raw[..at], &raw[at..]),
            None => (raw, ""),
        };
        let provider = normalize(provider)?;
        let (mut model, mut account) = ("", "");
        let mut seen_model = false;
        // `:model=` may contain `:`, so it owns the text up to a later `:account=`.
        let (effort, mut rest) = if let Some(tail) = rest.strip_prefix(":effort=") {
            let end = tail
                .find(":model=")
                .ok_or_else(|| Error::new(2, "seat effort override requires an exact model"))?;
            let effort = std::iter::once("auto")
                .chain(crate::native_model::EFFORTS.iter().copied())
                .find(|effort| *effort == &tail[..end])
                .ok_or_else(|| Error::new(2, "invalid seat reasoning effort"))?;
            (Some(effort), &tail[end..])
        } else {
            (None, rest)
        };
        while !rest.is_empty() {
            if let Some(tail) = rest.strip_prefix(":account=") {
                if !account.is_empty() {
                    return fail("duplicate account selector");
                }
                let end = tail.find(":model=").unwrap_or(tail.len());
                account = &tail[..end];
                account_id(account)?;
                rest = &tail[end..];
            } else if let Some(tail) = rest.strip_prefix(":model=") {
                if seen_model {
                    return fail("duplicate model selector");
                }
                seen_model = true;
                let end = tail.find(":account=").unwrap_or(tail.len());
                model = &tail[..end];
                if model.is_empty() || model.contains(":model=") {
                    return fail("invalid model selector");
                }
                rest = &tail[end..];
            } else {
                return fail("unsupported provider target suffix");
            }
        }
        if !model.is_empty() {
            model_name(model)?;
        }
        if let Some(effort) = effort {
            crate::native_model::validate_effort(&provider, model, effort)
                .map_err(|message| Error::new(2, message))?;
        }
        Ok(Self {
            provider,
            model: model.into(),
            account: account.into(),
            optional,
            effort,
        })
    }
}
pub fn account_id(value: &str) -> Result<()> {
    if cached_regex!(r"\A[a-z0-9][a-z0-9_-]{0,63}\z").is_match(value) {
        Ok(())
    } else {
        fail("invalid provider account id")
    }
}
pub fn normalize(raw: &str) -> Result<String> {
    let value = canonical_provider(raw);
    if !provider_id(value) || (builtin_binary(value).is_none() && !discovered(value)) {
        return fail("unsupported provider name");
    }
    Ok(value.into())
}
fn canonical_provider(raw: &str) -> &str {
    match raw {
        "agy" => "antigravity",
        "cursor-agent" => "cursor",
        "grok-build" => "grok",
        "gemini-cli" => "gemini",
        "qwen-code" => "qwen",
        "opencode2" => "opencode",
        "dsh" | "deepseek-harness" => "deepseek",
        "mistral-vibe" => "vibe",
        "github-copilot" => "copilot",
        "factory-droid" => "droid",
        "kiro-cli" => "kiro",
        "cn" => "continue",
        _ => raw,
    }
}
pub fn model_name(value: &str) -> Result<()> {
    if value.len() > 256
        || value.starts_with('-')
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\')
    {
        fail("invalid model name")
    } else {
        Ok(())
    }
}
pub fn family(provider: &str, model: &str) -> String {
    let lower = model.to_lowercase();
    for (needle, family) in [
        ("gpt", "openai"),
        ("claude", "anthropic"),
        ("gemini", "google"),
        ("grok", "xai"),
        ("deepseek", "deepseek"),
        ("qwen", "alibaba"),
    ] {
        if lower.contains(needle) {
            return family.into();
        }
    }
    match provider {
        "codex" => "openai",
        "claude" => "anthropic",
        "antigravity" | "gemini" => "google",
        _ => provider,
    }
    .into()
}
fn provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.contains("..")
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
fn builtin_binary(provider: &str) -> Option<&str> {
    Some(match provider {
        "antigravity" => "agy",
        "cursor" => "cursor-agent",
        "deepseek" => "dsh",
        "kiro" => "kiro-cli",
        "continue" => "cn",
        "codex" | "claude" | "grok" | "gemini" | "qwen" | "opencode" | "vibe" | "pi"
        | "copilot" | "droid" | "aider" | "muse" | "kimi" | "goose" | "cline" | "kilo"
        | "openhands" | "auggie" => provider,
        _ => return None,
    })
}
fn executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .ok()
        .is_some_and(|m| m.is_file() && m.mode() & 0o111 != 0)
}
fn on_path(name: &OsStr) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|p| executable(&p.join(name)))
}
fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}
fn binary_path(provider: &str) -> PathBuf {
    let provider = canonical_provider(provider);
    if !provider_id(provider) {
        return PathBuf::new();
    }
    if let Some(name) = builtin_binary(provider) {
        return if provider == "opencode" && !on_path(name.as_ref()) && on_path("opencode2".as_ref())
        {
            "opencode2".into()
        } else {
            name.into()
        };
    }
    let name = format!("oms-agent-adapter-{provider}");
    let base = nonempty_env("OMS_PROVIDER_ADAPTER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            nonempty_env("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(nonempty_env("HOME").unwrap_or_else(|| "/nonexistent".into()))
                        .join(".config")
                })
                .join("oh-my-setting/provider-adapters")
        });
    let direct = base.join(&name);
    if executable(&direct) {
        // A relative adapter directory is relative to the owner, not the child worktree.
        std::path::absolute(&direct).unwrap_or(direct)
    } else {
        name.into()
    }
}
pub fn binary(provider: &str) -> String {
    binary_path(provider).to_string_lossy().into_owned()
}
pub fn discovered(provider: &str) -> bool {
    let path = binary_path(provider);
    !path.as_os_str().is_empty()
        && if path.is_absolute() {
            executable(&path)
        } else {
            on_path(path.as_os_str())
        }
}
fn requires_read_isolation(provider: &str) -> bool {
    matches!(provider, "antigravity" | "aider") || builtin_binary(provider).is_none()
}
/// True when `transport` invokes `provider` in a streaming event output mode
/// (`codex` `--json`, `claude` `--output-format stream-json`, `pi` `--mode json`).
pub(crate) fn streams_output(provider: &str) -> bool {
    matches!(provider, "codex" | "claude" | "pi")
}
pub(crate) fn config(context: &Context<'_>, name: &str) -> Result<V> {
    config_from(context.source, name)
}
fn config_from(source: &Path, name: &str) -> Result<V> {
    let bytes = storage::read(source, Path::new("config").join(name).as_path(), false)?;
    V::parse(&bytes).map_err(Error::from)
}
#[derive(Clone, Debug)]
pub struct Route {
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub explicit: bool,
    /// Bound provider account id; empty until pinned or bound.
    pub account: String,
    /// True only when the caller asked for the account (`--account`/`:account=`).
    pub account_pinned: bool,
    /// Public native search/fetch for an explicitly selected read-only research role.
    /// This does not enable connector apps, file writes, or permission bypasses.
    pub public_research: bool,
}
pub(crate) fn route(
    context: &Context<'_>,
    target: &Target,
    model: &str,
    effort: &str,
    role: &str,
) -> Result<Route> {
    route_with_source(context.source, target, model, effort, role)
}
pub fn resolve_route(
    source: &Path,
    provider: &str,
    model: &str,
    effort: &str,
    role: &str,
) -> Result<Route> {
    route_with_source(source, &Target::parse(provider)?, model, effort, role)
}
fn route_with_source(
    source: &Path,
    target: &Target,
    model: &str,
    effort: &str,
    role: &str,
) -> Result<Route> {
    let mut selected = if !model.is_empty() {
        model.to_owned()
    } else {
        target.model.clone()
    };
    let explicit = !selected.is_empty();
    let requested = crate::native_model::normalized_effort(effort);
    let effort = if let Some(effort) = target.effort {
        if selected != target.model {
            return fail("seat effort override requires its exact model");
        }
        if !requested.is_empty() && !crate::native_model::EFFORTS.contains(&requested) {
            return fail("invalid reasoning effort");
        }
        crate::native_model::normalized_effort(effort)
    } else {
        requested
    };
    let mut resolved = effort.to_owned();
    if selected.is_empty() {
        let policy = config_from(source, "panel-routing.json")?;
        let tier = policy
            .get("roles")
            .get(role)
            .get("tier")
            .text()
            .unwrap_or("deep");
        if let Some(row) = policy
            .get("tiers")
            .get(tier)
            .array()
            .iter()
            .find(|r| text(r, "provider") == target.provider && !r.get("public_only").truth())
        {
            selected = text(row, "model").into();
            if resolved.is_empty() {
                resolved = text(row, "effort").into();
            }
        } else {
            selected = "provider-default".into();
        }
    }
    model_name(&selected)?;
    crate::native_model::validate_effort(&target.provider, &selected, &resolved)
        .map_err(|message| Error::new(2, message))?;
    resolved = crate::native_model::normalized_effort(&resolved).to_owned();
    if !target.account.is_empty() && isolated(&target.provider).is_none() {
        return fail("provider account isolation is unproved");
    }
    // The route keeps the requested model so Plan and Autonomy identity checks
    // hold; the transport passes the exact Antigravity selector to the CLI.
    if target.provider == "antigravity" && selected != "provider-default" {
        let exact = crate::native_model::agy_selector(&selected, &resolved);
        model_name(&exact)?;
        if let Some(catalog) = antigravity_catalog()
            && !catalog.contains(&exact)
        {
            return fail("model_effort_unavailable");
        }
    }
    Ok(Route {
        provider: target.provider.clone(),
        model: selected,
        effort: resolved,
        explicit,
        account: target.account.clone(),
        account_pinned: !target.account.is_empty(),
        public_research: false,
    })
}

/// The `--model` value the provider CLI receives for `model` on this route.
fn cli_model(route: &Route, model: &str) -> String {
    if route.provider == "antigravity" {
        crate::native_model::agy_selector(model, &route.effort)
    } else {
        model.to_owned()
    }
}

fn antigravity_catalog() -> Option<BTreeSet<String>> {
    let dir = nonempty_env("OMS_CAPABILITY_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            nonempty_env("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| nonempty_env("HOME").map(|home| PathBuf::from(home).join(".cache")))
                .map(|cache| cache.join("oh-my-setting/capabilities"))
        })?;
    let bytes = storage::read(&dir, Path::new("antigravity.models"), true)
        .ok()
        .filter(|b| !b.is_empty() && b.len() <= 4 * 1024 * 1024)?;
    let raw = String::from_utf8_lossy(&bytes);
    let selectors: BTreeSet<String> = raw
        .lines()
        .filter_map(crate::native_model::agy_catalog_selector)
        .map(str::to_owned)
        .collect();
    (!selectors.is_empty()).then_some(selectors)
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SeatHealth {
    pub ok: u64,
    pub err: u64,
    pub latency_ms: u64,
    pub cooldown_until: u64,
    pub last_error_ts: u64,
}

const ERROR_DECAY_SECS: u64 = 1800;
fn penalty(h: Option<&SeatHealth>, now: u64) -> u64 {
    h.map_or(0, |h| {
        if h.last_error_ts == 0 || now.saturating_sub(h.last_error_ts) > ERROR_DECAY_SECS {
            0
        } else {
            h.err.saturating_sub(h.ok).min(3)
        }
    })
}

#[derive(Clone, Debug)]
pub struct Account {
    pub provider: String,
    pub id: String,
    pub home: PathBuf,
    pub used: Option<f64>,
    pub running: usize,
    pub usable: bool,
    pub models: Option<BTreeSet<String>>,
}
/// Health/spread key: `provider:id`, or the bare provider when no account is bound.
pub fn seat_key(provider: &str, account: &str) -> String {
    if account.is_empty() {
        provider.into()
    } else {
        format!("{provider}:{account}")
    }
}

/// Ranks eligible accounts by `(cooling down, penalty, 10% quota bucket, running
/// count when `spread`, registry order)`. `explicit` waives the weekly-quota cap.
pub fn select_account_adaptive(
    accounts: &[Account],
    spread: bool,
    explicit: bool,
    model: &str,
    health: &std::collections::BTreeMap<String, SeatHealth>,
    now: u64,
) -> Option<Account> {
    let mut eligible = accounts
        .iter()
        .filter(|a| {
            a.usable
                && (explicit || a.used.is_none_or(|v| v < 90.0))
                && (model == "provider-default"
                    || a.models.as_ref().is_none_or(|m| m.contains(model)))
        })
        .collect::<Vec<_>>();
    eligible.sort_by_key(|a| {
        let h = health.get(&seat_key(&a.provider, &a.id));
        (
            h.is_some_and(|h| h.cooldown_until > now),
            penalty(h, now),
            a.used.map_or(10, |u| (u.clamp(0.0, 100.0) / 10.0) as u64),
            if spread { a.running } else { 0 },
        )
    });
    eligible.first().map(|a| (*a).clone())
}

fn load_seat_health(repo: &Path) -> std::collections::BTreeMap<String, SeatHealth> {
    let Ok(bytes) = storage::read(repo, Path::new(".oms/seat-health.json"), true) else {
        return std::collections::BTreeMap::new();
    };
    if bytes.is_empty() || bytes.len() > 262_144 {
        return std::collections::BTreeMap::new();
    }
    let Ok(doc) = V::parse(&bytes) else {
        return std::collections::BTreeMap::new();
    };
    let mut map = std::collections::BTreeMap::new();
    if let V::Obj(fields) = doc.get("accounts") {
        for (key, val) in fields {
            let Ok(name) = std::str::from_utf8(key) else {
                continue;
            };
            map.insert(
                name.to_owned(),
                SeatHealth {
                    ok: number(val.get("ok")).map_or(0, |n| n.max(0.0) as u64),
                    err: number(val.get("err")).map_or(0, |n| n.max(0.0) as u64),
                    latency_ms: number(val.get("latency_ms")).map_or(0, |n| n.max(0.0) as u64),
                    cooldown_until: number(val.get("cooldown_until"))
                        .map_or(0, |n| n.max(0.0) as u64),
                    last_error_ts: number(val.get("last_error_ts"))
                        .map_or(0, |n| n.max(0.0) as u64),
                },
            );
        }
    }
    map
}

fn record_seat_health(
    repo: &Path,
    seat: &str,
    ok: bool,
    latency_ms: u64,
    capacity_hit: bool,
    provider_fault: bool,
    interrupted: &std::sync::atomic::AtomicBool,
) {
    if seat.is_empty() || !repo.join(".oms").is_dir() || (!ok && !provider_fault) {
        return;
    }
    let rel = Path::new(".oms/seat-health.json");
    let guard = storage::lock(repo, rel, interrupted).ok();
    let mut map = load_seat_health(repo);
    let entry = map.entry(seat.to_owned()).or_default();
    if ok {
        entry.ok = entry.ok.saturating_add(1);
        entry.cooldown_until = 0;
        entry.latency_ms = if entry.latency_ms == 0 {
            latency_ms.max(1)
        } else {
            (entry.latency_ms * 3 + latency_ms.max(1)) / 4
        };
    } else {
        entry.err = entry.err.saturating_add(1);
        entry.last_error_ts = crate::seconds();
        if capacity_hit {
            entry.cooldown_until = crate::seconds().saturating_add(300);
        }
    }
    let rows = map
        .into_iter()
        .filter(|(k, _)| k.contains(':') || builtin_binary(k).is_some())
        .map(|(k, h)| {
            (
                k,
                crate::obj([
                    ("ok", V::Num(h.ok.to_string())),
                    ("err", V::Num(h.err.to_string())),
                    ("latency_ms", V::Num(h.latency_ms.to_string())),
                    ("cooldown_until", V::Num(h.cooldown_until.to_string())),
                    ("last_error_ts", V::Num(h.last_error_ts.to_string())),
                ]),
            )
        })
        .collect::<Vec<_>>();
    let doc = crate::obj([
        ("schema", V::Num("1".into())),
        (
            "accounts",
            V::Obj(rows.into_iter().map(|(k, v)| (k.into_bytes(), v)).collect()),
        ),
    ]);
    if let Ok(encoded) = crate::json(&doc) {
        let _ = match &guard {
            Some(g) => storage::write_locked(repo, rel, g, encoded, true, interrupted),
            None => storage::write(repo, rel, encoded, true, interrupted),
        };
    }
}
fn unique_keys(value: &oms_context::Json) -> bool {
    match value {
        oms_context::Json::Object(fields) => {
            let mut keys = BTreeSet::new();
            fields.iter().all(|(k, v)| keys.insert(k) && unique_keys(v))
        }
        oms_context::Json::Array(a) => a.iter().all(unique_keys),
        _ => true,
    }
}
fn keys_are(value: &V, allowed: &[&str]) -> bool {
    matches!(value,V::Obj(fields) if fields.keys().all(|k|allowed.iter().any(|s|k==s.as_bytes())))
}
fn quota(home: &Path) -> Option<f64> {
    let mut pending = vec![(home.join("sessions"), 0)];
    let mut newest = None;
    let mut seen = 0;
    while let Some((dir, depth)) = pending.pop() {
        if depth > 3 {
            continue;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries {
            let entry = entry.ok()?;
            seen += 1;
            if seen > 8192 {
                return None;
            }
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl") {
                let modified = entry.metadata().ok()?.modified().ok()?;
                if newest.as_ref().is_none_or(|(stamp, _)| modified > *stamp) {
                    newest = Some((modified, entry.path()));
                }
            }
        }
    }
    let (_, path) = newest?;
    let bytes = storage::file(&path).ok()?;
    for line in bytes.split(|b| *b == b'\n').rev() {
        let Ok(row) = V::parse(line) else { continue };
        let limits = if row.has("rate_limits") {
            row.get("rate_limits")
        } else {
            row.get("payload").get("rate_limits")
        };
        if matches!(limits, V::Null) {
            continue;
        }
        if let Ok(observed) = chrono::DateTime::parse_from_rfc3339(text(&row, "timestamp")) {
            let age = crate::seconds() as i64 - observed.timestamp();
            if !(0..=604800).contains(&age) {
                return None;
            }
        } else {
            return None;
        }
        for key in ["primary", "secondary"] {
            let window = limits.get(key);
            if number(window.get("window_minutes")) != Some(10080.0) {
                continue;
            }
            let used = number(window.get("used_percent"))?;
            let reset = number(window.get("resets_at"))?;
            if !(0.0..=100.0).contains(&used) || reset <= crate::seconds() as f64 {
                return None;
            }
            return Some(used);
        }
        return None;
    }
    None
}
/// A provider whose per-account home is selected by one environment variable and
/// whose state stays inside that home. Anything else is refused as unproved.
struct Isolated {
    name: &'static str,
    env: &'static str,
    home_sub: &'static str,
    quota: Option<fn(&Path) -> Option<f64>>,
}
const ISOLATED: [Isolated; 3] = [
    Isolated {
        name: "codex",
        env: "CODEX_HOME",
        home_sub: ".codex",
        quota: Some(quota),
    },
    Isolated {
        name: "claude",
        env: "CLAUDE_CONFIG_DIR",
        home_sub: ".claude",
        quota: None,
    },
    Isolated {
        name: "pi",
        env: "PI_CODING_AGENT_DIR",
        home_sub: ".pi/agent",
        quota: None,
    },
];
fn isolated(provider: &str) -> Option<&'static Isolated> {
    ISOLATED.iter().find(|i| i.name == provider)
}
const UNPROVED: &str = "provider account isolation is unproved";
const CLAUDE_CREDENTIAL_ENV: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub id: String,
    pub home: PathBuf,
}
pub(crate) struct ProviderAccounts {
    pub spread: bool,
    pub entries: Vec<Entry>,
}
type Registry = std::collections::BTreeMap<String, ProviderAccounts>;

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(2, "account home is unavailable"))
}
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}
fn config_dir(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("oh-my-setting")
}
fn read_config(path: &Path) -> Result<Option<(V, Vec<u8>)>> {
    let bytes = match storage::file(path) {
        Ok(bytes) => bytes,
        Err(_) if !path.exists() => return Ok(None),
        Err(e) => return Err(e),
    };
    let parsed = oms_context::parse_json(&bytes)
        .map_err(|_| Error::new(2, "invalid provider accounts configuration"))?;
    if !unique_keys(&parsed) {
        return fail("duplicate provider accounts configuration key");
    }
    Ok(Some((V::parse(&bytes)?, bytes)))
}
fn parse_entries(
    settings: &V,
    homes: &mut BTreeSet<PathBuf>,
    entries: &mut Vec<Entry>,
) -> Result<()> {
    let id_re = cached_regex!(r"\A[a-z0-9][a-z0-9_-]{0,63}\z");
    let mut ids: BTreeSet<String> = entries.iter().map(|e| e.id.clone()).collect();
    for entry in settings.get("accounts").array() {
        if !keys_are(entry, &["id", "home"]) || !entry.has("id") || !entry.has("home") {
            return fail("unknown or missing provider account fields");
        }
        let ident = text(entry, "id");
        let raw = Path::new(text(entry, "home"));
        if !id_re.is_match(ident) || !raw.is_absolute() {
            return fail("invalid provider account id or home");
        }
        let location = canonical(raw);
        if !ids.insert(ident.to_owned()) || !homes.insert(location.clone()) {
            return fail("duplicate provider account id or canonical home");
        }
        entries.push(Entry {
            id: ident.into(),
            home: location,
        });
    }
    Ok(())
}
fn policy_of(settings: &V) -> Result<bool> {
    match settings.get("policy") {
        V::Null => Ok(false),
        other => match other.text() {
            Some("ordered") => Ok(false),
            Some("spread") => Ok(true),
            _ => fail("unsupported provider accounts policy"),
        },
    }
}
/// Registered accounts per isolated provider. Canonical homes must be unique
/// across all providers; ids are provider-local. Claude falls back to its own
/// registry file with the standard home as the implicit `default`.
pub(crate) fn load_registry(home: &Path) -> Result<Registry> {
    let dir = config_dir(home);
    let mut registry = Registry::new();
    let mut homes = BTreeSet::new();
    let mut claude_listed = false;
    if let Some((config, _)) = read_config(&dir.join("provider-accounts.json"))? {
        if !keys_are(&config, &["schema", "providers"])
            || !matches!(config.get("providers"), V::Obj(_))
        {
            return fail("unknown or unsupported provider accounts configuration keys");
        }
        if config.get("schema") != &1usize.into() {
            return fail("unsupported provider accounts schema");
        }
        if let V::Obj(fields) = config.get("providers") {
            for (key, val) in fields {
                if !ISOLATED.iter().any(|i| key == i.name.as_bytes()) {
                    return fail(UNPROVED);
                }
                if !keys_are(val, &["policy", "accounts"]) {
                    return fail("unknown account policy keys");
                }
            }
        }
        for spec in &ISOLATED {
            let settings = config.get("providers").get(spec.name);
            if matches!(settings, V::Null) {
                continue;
            }
            if !keys_are(settings, &["policy", "accounts"]) {
                return fail("unknown account policy keys");
            }
            let spread = policy_of(settings)?;
            if settings.get("accounts").array().is_empty() {
                return fail("provider accounts must be a non-empty list");
            }
            let mut entries = Vec::new();
            parse_entries(settings, &mut homes, &mut entries)?;
            claude_listed |= spec.name == "claude";
            registry.insert(spec.name.into(), ProviderAccounts { spread, entries });
        }
    }
    if !claude_listed
        && let Some((config, _)) = read_config(&dir.join("provider-accounts.claude.json"))?
    {
        if !keys_are(&config, &["schema", "provider", "policy", "accounts"])
            || text(&config, "provider") != "claude"
            || !matches!(config.get("accounts"), V::Arr(_))
        {
            return fail("unknown or unsupported provider accounts configuration keys");
        }
        if config.get("schema") != &1usize.into() {
            return fail("unsupported provider accounts schema");
        }
        let spread = policy_of(&config)?;
        if !config.get("accounts").array().is_empty() {
            let spec = isolated("claude").ok_or_else(|| Error::new(2, UNPROVED))?;
            let default = canonical(&home.join(spec.home_sub));
            // Reserve the implicit home so no provider can alias ~/.claude.
            if !homes.insert(default.clone()) {
                return fail("duplicate provider account id or canonical home");
            }
            let mut entries = vec![Entry {
                id: "default".into(),
                home: default,
            }];
            parse_entries(&config, &mut homes, &mut entries)?;
            registry.insert("claude".into(), ProviderAccounts { spread, entries });
        }
    }
    Ok(registry)
}
/// `(provider, home)` for every registered account, for session discovery.
/// Fails closed when an account registry exists and is invalid or unreadable.
pub(crate) fn configured_homes() -> Result<Vec<(String, PathBuf)>> {
    let Ok(home) = home_dir() else {
        return Ok(Vec::new());
    };
    Ok(load_registry(&home)?
        .into_iter()
        .flat_map(|(provider, list)| {
            list.entries
                .into_iter()
                .map(move |e| (provider.clone(), e.home))
        })
        .collect())
}

#[derive(Default)]
struct Bound {
    environment: Vec<(OsString, OsString)>,
    remove: Vec<OsString>,
    id: Option<String>,
    home: Option<PathBuf>,
}
fn bind(spec: &Isolated, home: &Path, id: &str, location: PathBuf) -> Result<Bound> {
    if spec.name == "claude"
        && canonical(&location) != canonical(&home.join(spec.home_sub))
        && CLAUDE_CREDENTIAL_ENV
            .iter()
            .any(|name| nonempty_env(name).is_some())
    {
        return fail("credential environment overrides the selected Claude account");
    }
    Ok(Bound {
        environment: vec![(spec.env.into(), location.as_os_str().to_owned())],
        // A session directory override would leave transcripts outside the account.
        remove: if spec.name == "pi" {
            vec!["PI_CODING_AGENT_SESSION_DIR".into()]
        } else {
            Vec::new()
        },
        id: Some(id.into()),
        home: Some(location),
    })
}

/// Keep the selected directory open through execution. Birth time is needed
/// for a persisted generation, while the open directory proves this call's
/// identity even on filesystems that do not report birth time.
struct AccountHome {
    named: PathBuf,
    physical: PathBuf,
    directory: std::fs::File,
    sha256: Option<String>,
}
impl AccountHome {
    fn capture(provider: &str, named: &Path) -> Result<Self> {
        let invalid = || {
            Error::new(
                75,
                "selected provider account home is unavailable; no request was sent",
            )
        };
        if !owned_private(named) {
            return Err(invalid());
        }
        let physical = named.canonicalize().map_err(|_| invalid())?;
        let directory = std::fs::File::from(
            rustix::fs::open(
                &physical,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| invalid())?,
        );
        let metadata = directory.metadata().map_err(|_| invalid())?;
        let proof = Self {
            sha256: Self::digest(provider, &physical, &metadata),
            named: named.into(),
            physical,
            directory,
        };
        proof.current().then_some(proof).ok_or_else(invalid)
    }

    fn digest(provider: &str, path: &Path, metadata: &std::fs::Metadata) -> Option<String> {
        let birth = metadata
            .created()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        if birth.is_zero() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(format!("oms-account-home-v1\0{provider}\0"));
        digest.update(path.as_os_str().as_bytes());
        digest.update(format!(
            "\0{}\0{}\0{}\0{}\0{}",
            metadata.dev(),
            metadata.ino(),
            metadata.uid(),
            birth.as_secs(),
            birth.subsec_nanos(),
        ));
        Some(format!("{:x}", digest.finalize()))
    }

    fn current(&self) -> bool {
        let (Ok(opened), Ok(visible)) = (
            self.directory.metadata(),
            std::fs::symlink_metadata(&self.physical),
        ) else {
            return false;
        };
        owned_private(&self.named)
            && self.named.canonicalize().ok().as_ref() == Some(&self.physical)
            && owned_private_directory(&opened)
            && owned_private_directory(&visible)
            && opened.ino() != 0
            && (opened.dev(), opened.ino()) == (visible.dev(), visible.ino())
    }
}
pub(crate) type SessionEnv = (Vec<(OsString, OsString)>, Vec<OsString>, Option<String>);

pub(crate) fn session_environment(provider: &str, location: &Path) -> Result<SessionEnv> {
    let spec = isolated(provider).ok_or_else(|| Error::new(2, UNPROVED))?;
    if !owned_private(location) {
        return fail("requested provider account is unavailable");
    }
    let home = home_dir()?;
    let registry = load_registry(&home)?;
    let location = canonical(location);
    let account = registry
        .get(provider)
        .and_then(|accounts| {
            accounts
                .entries
                .iter()
                .find(|entry| canonical(&entry.home) == location)
        })
        .map(|entry| entry.id.clone());
    let bound = bind(spec, &home, account.as_deref().unwrap_or(""), location)?;
    Ok((bound.environment, bound.remove, account))
}
fn owned_private(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .as_ref()
        .is_ok_and(owned_private_directory)
}
fn owned_private_directory(metadata: &std::fs::Metadata) -> bool {
    metadata.is_dir()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & 0o022 == 0
}

fn codex_models(home: &Path) -> Option<BTreeSet<String>> {
    storage::read(home, Path::new("models_cache.json"), true)
        .ok()
        .filter(|b| b.len() <= 4 * 1024 * 1024)
        .and_then(|b| V::parse(&b).ok())
        .and_then(|v| {
            if !matches!(v.get("models"), V::Arr(_)) {
                return None;
            }
            let mut values = BTreeSet::new();
            for r in v.get("models").array() {
                let slug = text(r, "slug");
                if slug.is_empty() {
                    return None;
                }
                values.insert(slug.to_owned());
            }
            (!values.is_empty()).then_some(values)
        })
}
fn account_serves_model(spec: &Isolated, home: &Path, model: &str) -> bool {
    model == "provider-default"
        || spec.name != "codex"
        || codex_models(home).is_none_or(|m| m.contains(model))
}

fn home_claimed_by_other(registry: &Registry, provider: &str, location: &Path) -> bool {
    let location = canonical(location);
    registry.iter().any(|(other_provider, accounts)| {
        other_provider != provider
            && accounts
                .entries
                .iter()
                .any(|entry| canonical(&entry.home) == location)
    })
}

fn account_environment(route: &Route, excluded: &BTreeSet<String>, repo: &Path) -> Result<Bound> {
    let Some(spec) = isolated(&route.provider) else {
        return if route.account.is_empty() {
            Ok(Bound::default())
        } else {
            fail(UNPROVED)
        };
    };
    let home = home_dir()?;
    let registry = load_registry(&home)?;
    let pinned = route.account_pinned;
    let listed = registry.get(spec.name);
    let resuming = nonempty_env("OMS_RESUME_SESSION").is_some();
    if !pinned && let Some(current) = nonempty_env(spec.env) {
        let current_loc = canonical(Path::new(&current));
        if current_loc != canonical(&home.join(spec.home_sub)) {
            if home_claimed_by_other(&registry, spec.name, &current_loc) {
                return fail("requested provider account is unavailable");
            }
            if let Some(entry) =
                listed.and_then(|l| l.entries.iter().find(|e| canonical(&e.home) == current_loc))
            {
                if excluded.contains(&entry.id)
                    || !owned_private(&entry.home)
                    || !account_serves_model(spec, &entry.home, &route.model)
                {
                    return fail("requested provider account is unavailable");
                }
                return bind(spec, &home, &entry.id, entry.home.clone());
            }
            if spec.name == "claude"
                && CLAUDE_CREDENTIAL_ENV
                    .iter()
                    .any(|name| nonempty_env(name).is_some())
            {
                return fail("credential environment overrides the selected Claude account");
            }
            return Ok(Bound {
                remove: if spec.name == "pi" {
                    vec!["PI_CODING_AGENT_SESSION_DIR".into()]
                } else {
                    Vec::new()
                },
                ..Bound::default()
            });
        }
    }
    if resuming && !pinned {
        // A native session lives in one account home; guessing could resume nothing.
        return match listed {
            Some(l)
                if l.entries.len() == 1
                    && canonical(&l.entries[0].home) == canonical(&home.join(spec.home_sub)) =>
            {
                if !owned_private(&l.entries[0].home) {
                    return fail("requested provider account is unavailable");
                }
                bind(spec, &home, &l.entries[0].id, l.entries[0].home.clone())
            }
            Some(_) => fail("session resume is ambiguous across provider accounts; pass --account"),
            None if home_claimed_by_other(&registry, spec.name, &home.join(spec.home_sub)) => {
                Err(Error::new(2, format!("no eligible {} account", spec.name)))
            }
            None => Ok(Bound::default()),
        };
    }
    let wanted = (!route.account.is_empty() && !excluded.contains(&route.account))
        .then_some(route.account.as_str());
    if pinned && wanted.is_none() {
        return fail("requested provider account is unavailable");
    }
    if let Some(id) = wanted {
        let found = listed.and_then(|l| l.entries.iter().find(|e| e.id == id));
        let location = match found {
            Some(entry) => Some(entry.home.clone()),
            None if id == "default" => {
                let candidate = canonical(&home.join(spec.home_sub));
                (!home_claimed_by_other(&registry, spec.name, &candidate)).then_some(candidate)
            }
            None => None,
        };
        match location {
            Some(location)
                if owned_private(&location)
                    && account_serves_model(spec, &location, &route.model) =>
            {
                return bind(spec, &home, id, location);
            }
            _ if pinned => return fail("requested provider account is unavailable"),
            _ => {}
        }
    }
    let Some(listed) = listed else {
        if home_claimed_by_other(&registry, spec.name, &home.join(spec.home_sub)) {
            return Err(Error::new(2, format!("no eligible {} account", spec.name)));
        }
        return Ok(Bound::default());
    };
    let spread = listed.spread && !pinned;
    let running = if spread {
        running_accounts(repo)?
    } else {
        std::collections::BTreeMap::new()
    };
    let accounts = listed
        .entries
        .iter()
        .map(|entry| {
            let models = (spec.name == "codex")
                .then(|| codex_models(&entry.home))
                .flatten();
            Account {
                provider: spec.name.into(),
                id: entry.id.clone(),
                used: spec.quota.and_then(|read| read(&entry.home)),
                running: running
                    .get(&seat_key(spec.name, &entry.id))
                    .copied()
                    .unwrap_or(0),
                usable: owned_private(&entry.home) && !excluded.contains(&entry.id),
                home: entry.home.clone(),
                models,
            }
        })
        .collect::<Vec<_>>();
    let health = load_seat_health(repo);
    match select_account_adaptive(
        &accounts,
        spread,
        route.explicit,
        &route.model,
        &health,
        crate::seconds(),
    ) {
        Some(account) => bind(spec, &home, &account.id, account.home),
        None if route.explicit
            && !pinned
            && excluded.is_empty()
            && !listed.entries.iter().any(|e| e.id == "default") =>
        {
            let fallback = canonical(&home.join(spec.home_sub));
            if owned_private(&fallback)
                && !home_claimed_by_other(&registry, spec.name, &fallback)
                && account_serves_model(spec, &fallback, &route.model)
            {
                bind(spec, &home, "default", fallback)
            } else {
                Err(Error::new(2, format!("no eligible {} account", spec.name)))
            }
        }
        None => Err(Error::new(2, format!("no eligible {} account", spec.name))),
    }
}
/// Binds the account before an attempt is recorded so its `panel_account` ref
/// reflects the home that will run. Unconfigured providers stay unbound.
pub(crate) fn bind_account(route: &mut Route, repo: &Path) -> Result<()> {
    if let Some(id) = account_environment(route, &BTreeSet::new(), repo)?.id {
        route.account = id;
    }
    Ok(())
}
/// `panel_account` ref for the bound account, or empty when none is bound.
pub(crate) fn panel_account(route: &Route) -> String {
    if route.account.is_empty() {
        String::new()
    } else {
        seat_key(&route.provider, &route.account)
    }
}
pub(crate) fn panel_implicit_target() -> bool {
    std::env::var_os("OMS_PANEL_DISPATCH").as_deref() == Some(OsStr::new("1"))
        && std::env::var_os("OMS_PANEL_EXPLICIT_TARGET").as_deref() == Some(OsStr::new("0"))
}
pub fn answer_quality(raw: &str) -> &'static str {
    let mut from = 0;
    for (at, line) in raw.lines().enumerate() {
        if line == "## Output" {
            from = at + 1;
        }
    }
    let timeout = cached_regex!(
        r"^\[agy\] print timeout after .+ with turn in progress; returning partial output$",
    );
    if raw
        .lines()
        .skip(from)
        .any(|line| timeout.is_match(line.trim()))
    {
        return "truncated";
    }
    let stop = cached_regex!(
        r"^stop-reason: provider=\S+ reason=(\S+) subtype=\S+ is_error=([01])(?: category=\S+)?$",
    );
    for line in raw.lines().skip(from) {
        if let Some(capture) = stop.captures(line.trim()) {
            if &capture[2] == "1" || &capture[1] == "refusal" {
                return "blocked";
            }
            if [concat!("max_", "tok", "ens"), "stream_truncated"].contains(&&capture[1]) {
                return "truncated";
            }
            break;
        }
    }
    let noise = cached_regex!(concat!(
        r"^\s*(model-route:|model-result:|usage detail:|",
        r"tok",
        r"ens used|\[REDACTED|OpenAI Codex|\[untrusted |\[end untrusted |",
        r"workdir:|model:|provider:|approval:|sandbox:|reasoning effort:|",
        r"reasoning summaries:|session id:|-{3,}$|user$|DRY RUN)"
    ));
    let headings = cached_regex!(
        r"(?i)^(?:#{1,6}\s*)?(?:Answer|Alternatives|Tradeoffs|Evidence|Counterargument|Risks|Recommendation|Verification|Changed from previous round|Remaining disagreements):\s*$"
    );
    let refusal = cached_regex!(
        r"(?i)^(jetski: no output produced\b|add an allow-rule\b|alternatively, re-run with\b|.*\bcommand not found\b|(error|fatal)[: ].*\b(permission|not authenticated|not logged in|unauthorized|invalid api key|quota|rate limit)\b)"
    );
    let mut skip_next = false;
    let mut content = false;
    let mut only_headings = true;
    let mut only_refusals = true;
    for line in raw.lines() {
        if skip_next {
            skip_next = false;
            continue;
        }
        let trimmed = line.trim();
        if [
            "served model",
            "configured model",
            concat!("tok", "ens used"),
            "cost usd",
        ]
        .contains(&trimmed)
        {
            skip_next = true;
            continue;
        }
        if !trimmed.is_empty() && !noise.is_match(line) && !stop.is_match(trimmed) {
            content = true;
            only_headings = only_headings && headings.is_match(trimmed);
            only_refusals = only_refusals && refusal.is_match(trimmed);
        }
    }
    if !content || only_headings {
        "empty"
    } else if only_refusals {
        "blocked"
    } else {
        "ok"
    }
}
fn incomplete_stop(reason: &str) -> bool {
    matches!(
        reason,
        "length" | "max_output" | "refusal" | "error" | "aborted"
    ) || reason.starts_with("max_")
}

#[derive(Clone, Copy)]
pub(crate) enum OutputMode<'a> {
    Legacy,
    Observed(
        &'a std::cell::Cell<Option<crate::lead_observation::Observation>>,
        Option<&'a crate::process::StreamTap>,
    ),
    Native,
}

pub(crate) fn require_native_output(provider: &str) -> Result<()> {
    if !oms_command_plan::discussion::supports_native_output(provider) {
        return fail("provider transport cannot prove native output completion");
    }
    if !discovered(provider) {
        return Err(Error::new(127, "provider command not found"));
    }
    Ok(())
}

fn native_text(row: &oms_json::Value, key: &str) -> Option<String> {
    row.get(key)?.as_string()?.to_str_checked()
}

pub(crate) fn native_stopped_early(row: &oms_json::Value) -> bool {
    [
        native_text(row, "stop_reason"),
        native_text(row, "finish_reason"),
        row.get("message")
            .and_then(|m| native_text(m, "stop_reason")),
        row.get("event")
            .and_then(|e| e.get("delta"))
            .and_then(|d| native_text(d, "stop_reason")),
    ]
    .into_iter()
    .flatten()
    .any(|reason| incomplete_stop(&reason))
}

fn decode_native_output(provider: &str, raw: &[u8]) -> Option<String> {
    if provider == "antigravity" {
        // Unlike JSONL, print mode emits one envelope. Completion and accounting
        // have separate contracts, including for continued conversations.
        let row = oms_json::parse_strict(raw).ok()?;
        let id = native_text(&row, "conversation_id")?;
        if native_text(&row, "status").as_deref() != Some("SUCCESS")
            || id.is_empty()
            || id.chars().any(|c| c.is_whitespace() || c.is_control())
            || row
                .get("error")
                .is_some_and(|v| !matches!(v, oms_json::Value::Null))
            || native_stopped_early(&row)
        {
            return None;
        }
        return native_text(&row, "response");
    }
    let mut answer = None;
    let mut terminal = false;
    for line in std::str::from_utf8(raw)
        .ok()?
        .lines()
        .filter(|s| !s.trim().is_empty())
    {
        let row = oms_json::parse_strict(line.as_bytes()).ok()?;
        let kind = native_text(&row, "type")?;
        // The terminal event closes the stream. Trailing partial events and
        // explicit errors invalidate the proof even after an answer was seen.
        if terminal || native_stopped_early(&row) {
            return None;
        }
        match (provider, kind.as_str()) {
            ("codex", "turn.failed" | "error") | ("claude", "error") => return None,
            ("codex", "turn.started") => answer = None,
            ("codex", "item.completed") => {
                let item = row.get("item")?;
                if native_text(item, "type").as_deref() == Some("agent_message") {
                    answer = Some(native_text(item, "text")?);
                }
            }
            ("codex", "turn.completed") => terminal = true,
            ("claude", "result") => {
                if native_text(&row, "subtype").as_deref() != Some("success")
                    || !matches!(row.get("is_error"), Some(oms_json::Value::Bool(false)))
                {
                    return None;
                }
                answer = Some(native_text(&row, "result")?);
                terminal = true;
            }
            _ => {}
        }
    }
    answer.filter(|_| terminal)
}

pub(crate) fn decode_output(provider: &str, raw: &[u8]) -> (String, bool) {
    let input = String::from_utf8_lossy(raw);
    decode_provider_events(provider, &input, false).unwrap_or_else(|| (input.into_owned(), true))
}

pub(crate) fn native_result_footer(provider: &str, kind: &str, row: &V) -> bool {
    match (provider, kind) {
        ("codex" | "claude", "usage") => ["input_tokens", "output_tokens", "total_tokens"]
            .iter()
            .any(|key| matches!(row.get("usage").get(key), V::Num(n) if n.parse::<u64>().is_ok())),
        ("claude", "rate_limit_event") => matches!(
            text(row.get("rate_limit_info"), "status"),
            "allowed" | "allowed_warning" | "rejected"
        ),
        ("claude", "system") => {
            text(row, "subtype") == "hook_response"
                && matches!(text(row, "hook_event"), "Stop" | "SessionEnd")
                && text(row, "outcome") == "success"
                // Emitted after `result` once a turn used tools.
                || text(row, "subtype") == "task_summary"
        }
        _ => false,
    }
}

fn decode_bound_output(provider: &str, raw: &[u8]) -> (String, bool) {
    if provider == "antigravity" {
        // AGY print mode emits a single JSON document, which may span lines.
        if let Ok(row) = oms_json::parse_strict(raw)
            && let Some(answer) = native_text(&row, "response")
        {
            let terminal = native_text(&row, "status").as_deref() == Some("SUCCESS")
                && row
                    .get("error")
                    .is_none_or(|v| matches!(v, oms_json::Value::Null))
                && !native_stopped_early(&row);
            return (answer, terminal);
        }
    } else if let Ok(input) = std::str::from_utf8(raw)
        && let Some(decoded) = decode_provider_events(provider, input, true)
    {
        return decoded;
    }
    (String::from_utf8_lossy(raw).into_owned(), false)
}

fn decode_provider_events(
    provider: &str,
    input: &str,
    require_terminal_json: bool,
) -> Option<(String, bool)> {
    let mut output = Vec::new();
    let mut recognized = false;
    let mut terminal = false;
    let mut stopped_early = false;
    let mut malformed = false;
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(row) = V::parse(line.as_bytes()) else {
            malformed = true;
            continue;
        };
        if require_terminal_json
            && (!matches!(row, V::Obj(_)) || oms_json::parse_strict(line.as_bytes()).is_err())
        {
            malformed = true;
        }
        let kind = text(&row, "type");
        if require_terminal_json && !native_result_footer(provider, kind, &row) {
            terminal = false;
        }
        for reason in [
            row.get("stop_reason"),
            row.get("finish_reason"),
            row.get("message").get("stop_reason"),
            row.get("event").get("delta").get("stop_reason"),
        ] {
            if reason.text().is_some_and(incomplete_stop) {
                stopped_early = true;
            }
        }
        if provider == "codex" {
            if matches!(
                kind,
                "thread.started" | "turn.started" | "item.started" | "error"
            ) {
                recognized = true;
            }
            if kind == "turn.started" {
                terminal = false;
            }
            if kind == "item.completed" {
                let item = row.get("item");
                if text(item, "type") == "agent_message" {
                    output.push(text(item, "text").to_owned());
                    recognized = true;
                }
            }
            if kind == "turn.completed" {
                terminal = true;
                recognized = true;
            }
            if kind == "turn.failed" {
                recognized = true;
                output.push(text(row.get("error"), "message").to_owned());
            }
        } else if provider == "claude" {
            if matches!(kind, "system" | "stream_event" | "user") {
                recognized = true;
            }
            if kind == "assistant" {
                for part in row.get("message").get("content").array() {
                    if text(part, "type") == "text" {
                        output.push(text(part, "text").to_owned());
                        recognized = true;
                    }
                }
            }
            if kind == "result" {
                recognized = true;
                terminal = !row.get("is_error").truth()
                    && !matches!(
                        text(&row, "subtype"),
                        "error_max_turns" | "error_during_execution"
                    );
                if require_terminal_json {
                    terminal &= row.get("is_error") == &V::Bool(false)
                        && text(&row, "subtype") == "success";
                }
                let result = text(&row, "result");
                if !result.is_empty() && output.last().is_none_or(|v| v != result) {
                    output.push(result.into());
                }
            }
        } else if provider == "pi" {
            if matches!(kind, "message_start" | "message_update") {
                recognized = true;
                terminal = false;
            }
            if kind == "message_end" || kind == "agent_end" {
                recognized = true;
            }
            let last = if kind == "agent_end" {
                row.get("messages")
                    .array()
                    .iter()
                    .rev()
                    .find(|m| text(m, "role") == "assistant")
            } else if kind == "message_end" && text(row.get("message"), "role") == "assistant" {
                Some(row.get("message"))
            } else {
                None
            };
            if let Some(message) = last {
                let answer = message
                    .get("content")
                    .array()
                    .iter()
                    .filter(|m| text(m, "type") == "text")
                    .map(|m| text(m, "text"))
                    .collect::<String>();
                if !answer.is_empty() && output.last() != Some(&answer) {
                    output.push(answer);
                }
                terminal = !matches!(
                    text(message, "stopReason"),
                    "error" | "aborted" | "length" | "max_output"
                ) && !text(message, "stopReason").starts_with("max_");
                if !terminal {
                    output.push(text(message, "errorMessage").into());
                }
            }
        } else if matches!(provider, "antigravity" | "gemini" | "qwen") {
            let value = row
                .get("response")
                .text()
                .or_else(|| row.get("result").text());
            if let Some(value) = value {
                recognized = true;
                terminal = true;
                output.push(value.into());
            }
        }
    }
    recognized.then(|| {
        (
            output.join("\n\n"),
            terminal && !stopped_early && (!require_terminal_json || !malformed),
        )
    })
}
struct ProviderScratch(PathBuf);
impl ProviderScratch {
    fn new(provider: &str, outside: Option<&Path>) -> Result<Self> {
        let mut base = std::env::temp_dir().canonicalize()?;
        if let Some(cwd) = outside {
            let cwd = cwd.canonicalize()?;
            if base.starts_with(&cwd) {
                base = Path::new("/tmp").canonicalize()?;
            }
            if base.starts_with(cwd) {
                return fail("private provider prompt state must be outside the worktree");
            }
        }
        let path = base.join(crate::id(&format!("oms-{provider}-"))?);
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let scratch = Self(path);
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))?;
        Ok(scratch)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        Ok(path)
    }
}
impl Drop for ProviderScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Transport {
    argv: Vec<OsString>,
    selected_effort: Option<String>,
    input: Option<Vec<u8>>,
    environment: Vec<(OsString, OsString)>,
    scratch: Option<ProviderScratch>,
}
impl Transport {
    fn args(&mut self, values: &[&str]) {
        self.argv.extend(values.iter().map(OsString::from));
    }
    fn option(&mut self, name: &str, value: impl AsRef<OsStr>) {
        self.argv.extend([name.into(), value.as_ref().into()]);
    }
    fn model(&mut self, route: &Route) {
        if route.model != "provider-default" {
            self.option("--model", cli_model(route, &route.model));
        }
    }
    fn effort(&mut self, route: &Route, flag: &str) {
        let effort = crate::native_model::normalized_effort(&route.effort);
        if !effort.is_empty() {
            self.option(flag, effort);
            self.selected_effort = Some(effort.into());
        }
    }
    fn prompt_file(&mut self, provider: &str, prompt: &[u8]) -> Result<PathBuf> {
        let scratch = ProviderScratch::new(provider, None)?;
        let path = scratch.file("prompt.md", prompt)?;
        self.scratch = Some(scratch);
        Ok(path)
    }
}
fn transport(
    route: &Route,
    prompt: &[u8],
    cwd: &Path,
    write: bool,
    timeout: u64,
) -> Result<Transport> {
    let provider = route.provider.as_str();
    crate::native_model::validate_effort(provider, &route.model, &route.effort)
        .map_err(|message| Error::new(2, message))?;
    let mut cmd = Transport {
        argv: vec![binary_path(provider).into_os_string()],
        selected_effort: None,
        input: Some(prompt.to_vec()),
        environment: Vec::new(),
        scratch: None,
    };
    let argv_prompt = if matches!(
        provider,
        "grok"
            | "opencode"
            | "deepseek"
            | "vibe"
            | "pi"
            | "copilot"
            | "muse"
            | "kimi"
            | "kiro"
            | "continue"
            | "goose"
            | "cline"
            | "kilo"
            | "openhands"
            | "auggie"
    ) || provider == "antigravity" && prompt.len() <= 120000
    {
        if prompt.len() > 120000 {
            return fail("provider argv prompt exceeds 120000-byte transport limit");
        }
        let value = std::str::from_utf8(prompt)
            .map_err(|_| Error::new(2, "provider argv prompt must be UTF-8"))?;
        if value.contains('\0') {
            return fail("provider argv prompt contains NUL");
        }
        value
    } else {
        ""
    };
    if matches!(
        provider,
        "deepseek"
            | "vibe"
            | "pi"
            | "copilot"
            | "droid"
            | "aider"
            | "muse"
            | "kimi"
            | "kiro"
            | "continue"
            | "goose"
            | "cline"
            | "kilo"
            | "openhands"
            | "auggie"
    ) {
        cmd.input = None;
    }
    match provider {
        "codex" => {
            cmd.args(&["exec"]);
            cmd.model(route);
            let effort = crate::native_model::normalized_effort(&route.effort);
            if !effort.is_empty() {
                cmd.option("-c", format!("model_reasoning_effort=\"{effort}\""));
                cmd.selected_effort = Some(effort.into());
            }
            cmd.args(&["--json"]);
            if !write {
                cmd.args(&[
                    "-c",
                    if route.public_research {
                        "web_search=\"live\""
                    } else {
                        "web_search=\"disabled\""
                    },
                    "-c",
                    "features.apps=false",
                ]);
            }
            cmd.args(&[
                "--sandbox",
                if write {
                    "workspace-write"
                } else {
                    "read-only"
                },
            ]);
            if !write {
                cmd.args(&["--skip-git-repo-check"]);
            }
            if let Some(resume) = nonempty_env("OMS_RESUME_SESSION") {
                cmd.args(&["resume", "--all"]);
                cmd.argv.push(resume);
            }
            cmd.args(&["-"]);
        }
        "claude" => {
            if let Some(resume) = nonempty_env("OMS_RESUME_SESSION") {
                cmd.option("--resume", resume);
            }
            if !write {
                cmd.args(&["--permission-mode", "plan"]);
            }
            cmd.model(route);
            cmd.effort(route, "--effort");
            if !write {
                cmd.args(&[
                    "--strict-mcp-config",
                    "--setting-sources",
                    "",
                    "--tools",
                    if route.public_research {
                        "Read,Grep,Glob,Bash,WebSearch,WebFetch"
                    } else {
                        "Read,Grep,Glob,Bash"
                    },
                ]);
                let plans = format!(
                    "{{\"plansDirectory\":\".oms/tmp/claude-plans-{}\"}}",
                    crate::id("")?
                );
                cmd.option("--settings", plans);
            }
            cmd.args(&["--output-format", "stream-json", "--verbose", "-p"]);
        }
        "antigravity" => {
            if let Some(resume) = nonempty_env("OMS_RESUME_SESSION") {
                cmd.option("--conversation", resume);
            }
            cmd.model(route);
            cmd.option("--add-dir", cwd);
            cmd.effort(route, "--effort");
            cmd.args(&["--sandbox"]);
            cmd.option("--print-timeout", format!("{timeout}s"));
            cmd.args(&["--output-format", "json"]);
            if prompt.len() > 120000 {
                if std::env::var("OMS_PEER_INTERACTIVE").as_deref() == Ok("1") {
                    return fail("provider argv prompt exceeds 120000-byte transport limit");
                }
                let scratch = ProviderScratch::new("agy-prompt", Some(cwd))?;
                let path = scratch.file("prompt.md", prompt)?;
                cmd.option("--add-dir", &scratch.0);
                cmd.option(
                    "--print",
                    format!(
                        "Read {} in full and follow it as your complete instructions.",
                        path.display()
                    ),
                );
                cmd.scratch = Some(scratch);
            } else {
                cmd.option("--print", argv_prompt);
            }
        }
        "cursor" => {
            cmd.args(&["--print", "--output-format", "text"]);
            cmd.option("--workspace", cwd);
            cmd.args(&["--sandbox", "enabled"]);
            cmd.model(route);
            cmd.args(if write {
                &["--force"]
            } else {
                &["--mode", "ask"]
            });
        }
        "grok" => {
            cmd.args(&["--no-auto-update"]);
            cmd.option("--cwd", cwd);
            cmd.args(&["--output-format", "plain"]);
            cmd.model(route);
            cmd.effort(route, "--effort");
            cmd.args(if write {
                &[
                    "--always-approve",
                    "--sandbox",
                    "workspace",
                    "--deny",
                    "Bash(git push*)",
                ]
            } else {
                &[
                    "--permission-mode",
                    "dontAsk",
                    "--allow",
                    "Read",
                    "--allow",
                    "Grep",
                    "--allow",
                    "Bash(git *)",
                    "--deny",
                    "Edit",
                    "--deny",
                    "WebFetch",
                    "--deny",
                    "WebSearch",
                    "--sandbox",
                    "strict",
                    "--no-memory",
                    "--no-subagents",
                    "--disable-web-search",
                ]
            });
            cmd.option("-p", argv_prompt);
        }
        "gemini" | "qwen" => {
            cmd.args(&["--output-format", "text", "--sandbox"]);
            cmd.model(route);
            if provider == "qwen" && !write {
                cmd.args(&["--safe-mode"]);
            }
            cmd.args(&["--approval-mode", if write { "yolo" } else { "plan" }]);
        }
        "opencode" => {
            cmd.args(&["run", "--format", "default"]);
            cmd.model(route);
            cmd.args(if write {
                &["--agent", "build", "--auto"]
            } else {
                &["--agent", "plan"]
            });
            cmd.argv.push(argv_prompt.into());
        }
        "deepseek" => {
            cmd.environment.extend([
                (
                    "DSH_PERMISSION_MODE".into(),
                    if write {
                        "workspace-write"
                    } else {
                        "read-only"
                    }
                    .into(),
                ),
                ("DSH_TELEMETRY_DISABLED".into(), "1".into()),
            ]);
            cmd.args(&["--profile", "headless", argv_prompt]);
        }
        "vibe" => {
            cmd.args(&[
                "--trust",
                "--max-turns",
                "24",
                "--output",
                "text",
                "--agent",
                if write { "accept-edits" } else { "plan" },
                "--enabled-tools",
                "read_file",
                "--enabled-tools",
                "grep",
            ]);
            if write {
                cmd.args(&["--enabled-tools", "write_file", "--enabled-tools", "edit"]);
            }
            cmd.option("--prompt", argv_prompt);
        }
        "pi" => {
            cmd.environment
                .push(("PI_SKIP_VERSION_CHECK".into(), "1".into()));
            cmd.args(&[
                "--no-session",
                "--no-approve",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--mode",
                "json",
            ]);
            cmd.model(route);
            cmd.effort(route, "--thinking");
            cmd.args(&[
                "--tools",
                if write {
                    "read,bash,edit,write,grep,find,ls"
                } else {
                    "read,grep,find,ls"
                },
            ]);
            cmd.option("--print", argv_prompt);
        }
        "copilot" => {
            cmd.args(&[
                "-s",
                "--no-ask-user",
                "--disable-builtin-mcps",
                "--disallow-temp-dir",
            ]);
            cmd.model(route);
            cmd.effort(route, "--effort");
            cmd.args(&["--available-tools"]);
            cmd.args(if write {
                &[
                    "view,grep,glob,edit,create,apply_patch",
                    "--allow-tool",
                    "read,write",
                    "--deny-tool",
                    "shell",
                    "--deny-tool",
                    "url",
                    "--deny-tool",
                    "memory",
                ]
            } else {
                &[
                    "view,grep,glob",
                    "--deny-tool",
                    "write",
                    "--deny-tool",
                    "shell",
                    "--deny-tool",
                    "url",
                    "--deny-tool",
                    "memory",
                ]
            });
            cmd.option("-p", argv_prompt);
        }
        "droid" => {
            cmd.args(&["exec"]);
            cmd.option("--cwd", cwd);
            cmd.args(&["--output-format", "text", "--disable-builtin-skills"]);
            cmd.model(route);
            cmd.effort(route, "--reasoning-effort");
            if write {
                cmd.args(&["--auto", "low"]);
            }
            let path = cmd.prompt_file(provider, prompt)?;
            cmd.option("-f", path);
        }
        "aider" => {
            let path = cmd.prompt_file(provider, prompt)?;
            let scratch = cmd.scratch.as_ref().expect("prompt scratch");
            let empty = scratch.file("empty.env", b"")?;
            let input = scratch.0.join("input.history");
            let chat = scratch.0.join("chat.history");
            cmd.args(&[
                "--no-auto-commits",
                "--no-dirty-commits",
                "--no-gitignore",
                "--no-suggest-shell-commands",
                "--no-auto-lint",
                "--no-auto-test",
                "--no-check-update",
                "--no-detect-urls",
                "--no-pretty",
                "--no-stream",
                "--no-analytics",
                "--no-restore-chat-history",
            ]);
            cmd.option("--env-file", empty);
            cmd.option("--input-history-file", input);
            cmd.option("--chat-history-file", chat);
            cmd.model(route);
            cmd.effort(route, "--reasoning-effort");
            cmd.args(if write {
                &["--chat-mode", "code", "--yes-always", "--no-dry-run"]
            } else {
                &["--chat-mode", "ask", "--dry-run"]
            });
            cmd.option("--message-file", path);
        }
        "muse" => {
            cmd.args(&["exec"]);
            cmd.model(route);
            cmd.effort(route, "--reasoning-effort");
            if write {
                cmd.args(&["--disable-approval"]);
            }
            cmd.argv.push(argv_prompt.into());
        }
        "kimi" => {
            cmd.model(route);
            cmd.option("-p", argv_prompt);
        }
        "kiro" => {
            cmd.args(&["chat", "--v3", "--no-interactive"]);
            cmd.model(route);
            cmd.effort(route, "--effort");
            if write {
                cmd.args(&["--trust-all-tools"]);
            }
            cmd.argv.push(argv_prompt.into());
        }
        "continue" => {
            cmd.option("-p", argv_prompt);
            cmd.model(route);
            cmd.args(if write { &["--auto"] } else { &["--readonly"] });
        }
        "goose" => {
            cmd.environment.push(("GOOSE_MODE".into(), "auto".into()));
            cmd.args(&["run", "--no-session"]);
            cmd.model(route);
            cmd.option("-t", argv_prompt);
        }
        "cline" => {
            cmd.model(route);
            cmd.effort(route, "--thinking");
            cmd.args(&["--auto-approve", "true", argv_prompt]);
        }
        "kilo" => {
            cmd.args(&["run", "--auto"]);
            cmd.model(route);
            cmd.effort(route, "--variant");
            cmd.argv.push(argv_prompt.into());
        }
        "openhands" => cmd.args(&["--headless", "-t", argv_prompt]),
        "auggie" => {
            cmd.environment
                .push(("AUGMENT_DISABLE_AUTO_UPDATE".into(), "1".into()));
            cmd.args(&["--print"]);
            cmd.model(route);
            cmd.args(&[
                "--permission",
                "edit:allow",
                "--permission",
                "write:allow",
                "--permission",
                "terminal:allow",
                argv_prompt,
            ]);
        }
        _ => {
            let path = cmd.prompt_file("adapter", prompt)?;
            cmd.args(&["run", "--access", if write { "write" } else { "read" }]);
            cmd.option("--workdir", cwd);
            cmd.option("--prompt-file", path);
            cmd.model(route);
            cmd.effort(route, "--effort");
        }
    }
    Ok(cmd)
}
/// Capability declarations are selection evidence, never session credentials.
pub(crate) fn team_protocol_file(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(2, "adapter parent unavailable"))?;
    let info = std::fs::symlink_metadata(path)?;
    if !info.is_file()
        || info.file_type().is_symlink()
        || info.nlink() != 1
        || info.uid() != rustix::process::geteuid().as_raw()
        || info.mode() & 0o022 != 0
        || info.len() > maximum as u64
    {
        return fail("unsafe Team request adapter declaration");
    }
    let root =
        oms_safe_fs::Root::open(parent).map_err(|_| Error::new(2, "adapter root unavailable"))?;
    let bytes = root
        .read_no_follow(
            Path::new(
                path.file_name()
                    .ok_or_else(|| Error::new(2, "adapter file unavailable"))?,
            ),
            oms_safe_fs::ByteReadOptions {
                missing_ok: false,
                max_bytes: maximum,
            },
        )
        .map_err(|_| Error::new(2, "adapter declaration read refused"))?;
    let after = std::fs::symlink_metadata(path)?;
    if (
        info.dev(),
        info.ino(),
        info.size(),
        info.mtime(),
        info.mtime_nsec(),
        info.ctime(),
        info.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.size(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return fail("Team request adapter declaration changed");
    }
    Ok(bytes)
}
fn team_protocol(route: &Route, argv: &[OsString]) -> Result<crate::process::TeamProtocol> {
    if builtin_binary(&route.provider).is_some() {
        return fail("built-in transport does not support OMS Team request protocol");
    }
    let binary = PathBuf::from(
        argv.first()
            .ok_or_else(|| Error::new(2, "adapter binary missing"))?,
    );
    if !binary.is_absolute() {
        return fail("Team request adapter requires an explicit registered absolute binary");
    }
    let raw = team_protocol_file(&binary, 16 * 1024 * 1024)?;
    let manifest = PathBuf::from(format!("{}.oms-team-request-v1.json", binary.display()));
    let declaration = team_protocol_file(&manifest, 8192)?;
    if declaration.len() > 8192 {
        return fail("Team request manifest exceeds bound");
    }
    let parsed =
        oms_command_plan::json::parse(&declaration).map_err(|e| Error::new(e.code, e.message))?;
    oms_command_plan::json::keys(&parsed, &["schema", "protocol", "binary_sha256"], &[])
        .map_err(|e| Error::new(e.code, e.message))?;
    if !parsed.get("schema").is_int(1)
        || parsed.text("protocol") != "oms-team-request-v1"
        || parsed.text("binary_sha256") != storage::hash(&raw)
    {
        return fail("Team request adapter manifest mismatch");
    }
    Ok(crate::process::TeamProtocol {
        binary,
        binary_sha256: storage::hash(&raw),
        manifest,
        manifest_sha256: storage::hash(&declaration),
    })
}
/// Writable dirs named by the child environment: the cargo target and every
/// isolated provider's account home.
fn env_rw_dirs(environment: &[(OsString, OsString)]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for (k, v) in environment {
        if k == "CARGO_TARGET_DIR" || ISOLATED.iter().any(|i| k == i.env) {
            let p = PathBuf::from(v);
            if p.is_dir() && !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    }
    dirs
}
fn repo_state(context: &Context<'_>, cwd: &Path) -> Option<[u8; 32]> {
    const MAX_PATHS: usize = 4096;
    const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
    const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
    let head = crate::process::git(context, cwd, &["rev-parse", "HEAD"]).ok()?;
    let staged = crate::process::git(
        context,
        cwd,
        &["diff", "--binary", "--no-ext-diff", "--cached"],
    )
    .ok()?;
    let status = crate::process::git(
        context,
        cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .ok()?;
    let mut hasher = Sha256::new();
    hasher.update((head.len() as u64).to_le_bytes());
    hasher.update(&head);
    hasher.update((staged.len() as u64).to_le_bytes());
    hasher.update(&staged);
    hasher.update((status.len() as u64).to_le_bytes());
    hasher.update(&status);
    let mut count = 0usize;
    let mut total = 0usize;
    let mut parts = status.split(|&b| b == 0);
    while let Some(record) = parts.next() {
        if record.is_empty() {
            continue;
        }
        if record.len() < 4 || record[2] != b' ' {
            return None;
        }
        let (x, y, rel) = (record[0], record[1], &record[3..]);
        if rel.is_empty() {
            return None;
        }
        count += 1;
        if count > MAX_PATHS {
            return None;
        }
        hasher.update([x, y, b' ']);
        hasher.update((rel.len() as u64).to_le_bytes());
        hasher.update(rel);
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            let orig = parts.next()?;
            hasher.update((orig.len() as u64).to_le_bytes());
            hasher.update(orig);
        }
        let full = cwd.join(OsStr::from_bytes(rel));
        match std::fs::symlink_metadata(&full) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => hasher.update(b"D\0"),
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&full).ok()?;
                let raw = target.as_os_str().as_bytes();
                total = total.checked_add(raw.len())?;
                if total > MAX_TOTAL_BYTES {
                    return None;
                }
                hasher.update(b"L\0");
                hasher.update((raw.len() as u64).to_le_bytes());
                hasher.update(raw);
            }
            Ok(meta) if meta.file_type().is_file() => {
                if meta.len() > MAX_FILE_BYTES {
                    return None;
                }
                let bytes = std::fs::read(&full).ok()?;
                total = total.checked_add(bytes.len())?;
                if total > MAX_TOTAL_BYTES {
                    return None;
                }
                hasher.update(b"F\0");
                hasher.update((meta.mode() & 0o7777).to_le_bytes());
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            _ => return None,
        }
    }
    Some(hasher.finalize().into())
}
fn is_clean_retry(snapshot: &Option<Option<[u8; 32]>>, context: &Context<'_>, cwd: &Path) -> bool {
    match snapshot {
        None => true,
        Some(Some(before)) => repo_state(context, cwd).as_ref() == Some(before),
        Some(None) => false,
    }
}
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_env<P: crate::request_context::Input + ?Sized>(
    route: &mut Route,
    prompt: &P,
    cwd: &Path,
    write: bool,
    timeout: u64,
    account_repo: &Path,
    extra_env: &[(OsString, OsString)],
    context: &Context<'_>,
) -> Result<Completed> {
    execute_env_with_output(
        route,
        prompt,
        cwd,
        write,
        timeout.into(),
        account_repo,
        extra_env,
        OutputMode::Legacy,
        context,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_env_with_output<P: crate::request_context::Input + ?Sized>(
    route: &mut Route,
    prompt: &P,
    cwd: &Path,
    write: bool,
    timeouts: crate::peer::Timeouts,
    account_repo: &Path,
    extra_env: &[(OsString, OsString)],
    output_mode: OutputMode<'_>,
    context: &Context<'_>,
) -> Result<Completed> {
    prompt.validate()?;
    crate::safety::outbound_prompt(prompt.bytes())?;
    if matches!(output_mode, OutputMode::Native) {
        require_native_output(&route.provider)?;
    } else if !discovered(&route.provider) {
        return Err(Error::new(127, "provider command not found"));
    }
    if write
        && builtin_binary(&route.provider).is_none()
        && !std::env::var("OMS_PROVIDER_WRITE_ADAPTERS")
            .unwrap_or_default()
            .split(',')
            .any(|name| name.split_whitespace().collect::<String>() == route.provider)
    {
        return fail("custom provider does not support write access without explicit opt-in");
    }
    if route.model != "provider-default"
        && !oms_command_agent_role::native_model::supports_exact_model_override(&route.provider)
    {
        return fail("provider has no documented per-invocation model override");
    }
    // Every read caller that can snapshot a repository already passes a disposable
    // worktree. Fail closed when a caller falls back to its original checkout.
    if !write
        && requires_read_isolation(&route.provider)
        && cwd.canonicalize()? == account_repo.canonicalize()?
    {
        return fail("provider read access requires a disposable repository worktree");
    }
    if !write
        && [
            "muse",
            "kimi",
            "kiro",
            "goose",
            "cline",
            "kilo",
            "openhands",
            "auggie",
        ]
        .contains(&route.provider.as_str())
    {
        return fail("provider does not support read access through this transport");
    }
    let stall = streams_output(&route.provider)
        .then_some(timeouts.stall)
        .flatten()
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs);
    let mut transport = transport(route, prompt.bytes(), cwd, write, timeouts.wall)?;
    let stream = if route.provider == "claude" {
        match output_mode {
            OutputMode::Observed(_, Some(tap)) if !write => {
                // Partial events precede `-p`; completion still comes from `result`.
                transport.argv.iter().rposition(|a| a == "-p").map(|at| {
                    transport
                        .argv
                        .insert(at, "--include-partial-messages".into());
                    tap.clone()
                })
            }
            _ => {
                if write
                    && stall.is_some()
                    && let Some(at) = transport.argv.iter().rposition(|a| a == "-p")
                {
                    // Write turns emit nothing during generation without partial events,
                    // which would trip the stall check; result decoding ignores `stream_event`.
                    transport
                        .argv
                        .insert(at, "--include-partial-messages".into());
                }
                None
            }
        }
    } else {
        None
    };
    let team_protocol = if extra_env
        .iter()
        .any(|(k, v)| k == "OMS_TEAM_REQUEST_PROTOCOL" && v == "oms-team-request-v1")
    {
        if write {
            return fail("Team request channels are read-only");
        }
        if matches!(output_mode, OutputMode::Native) {
            return fail("Team request channels do not emit typed native output");
        }
        Some(team_protocol(route, &transport.argv)?)
    } else {
        None
    };
    let team_actions = crate::process::TeamRequestAction::parse_list(
        extra_env
            .iter()
            .find(|(key, _)| key == "OMS_TEAM_REQUEST_ACTIONS")
            .and_then(|(_, value)| value.to_str())
            .unwrap_or(""),
    )?;
    let extra_env = crate::usage::operation_env(extra_env)?;
    let plan_bound = crate::plan_delegate::has_launch_binding(&extra_env);
    let usage_repo = crate::usage::state_repository(account_repo, &extra_env)?;
    let remove_environment: Vec<OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            k.to_str().is_some_and(|s| {
                s.starts_with("GIT_")
                    || s.starts_with("OMS_APPROVAL_")
                    || s.starts_with("OMS_LANDING_")
                    || s.starts_with("OMS_LEASE_")
                    || s == "OMS_PEER_LEAD_OBSERVATION"
                    || s == "SSH_AUTH_SOCK"
            })
        })
        .collect();
    let mut excluded = BTreeSet::new();
    let mut rebound = None;
    for overflow in 0..2 {
        let write_snapshot = (!plan_bound && overflow == 0 && write && !route.account_pinned)
            .then(|| repo_state(context, cwd));
        let bound = match rebound.take() {
            Some(bound) => bound,
            None => account_environment(route, &excluded, account_repo)?,
        };
        let Bound {
            mut environment,
            remove,
            id: account,
            home: account_home,
        } = bound;
        if let Some(id) = &account {
            route.account.clone_from(id);
        }
        let mut remove_environment = remove_environment.clone();
        remove_environment.extend(remove);
        environment.extend([
            ("OMS_HARNESS_CHILD".into(), "1".into()),
            ("OMS_HARNESS_DELEGATE_DEPTH".into(), "1".into()),
        ]);
        environment.extend(transport.environment.clone());
        environment.extend(
            extra_env
                .iter()
                .filter(|(key, _)| !crate::plan_delegate::launch_key(key))
                .cloned(),
        );
        let named_account = account
            .as_deref()
            .filter(|id| !id.is_empty() && *id != "default");
        let account_home = if let (Some(home), Some(spec)) =
            (&account_home, isolated(&route.provider))
        {
            let (_, value) = environment
                .iter_mut()
                .rev()
                .find(|(key, _)| key == spec.env)
                .filter(|(_, value)| canonical(Path::new(value.as_os_str())) == canonical(home))
                .ok_or_else(|| Error::new(2, "provider account home changed after selection"))?;
            let proof = named_account
                .map(|_| AccountHome::capture(&route.provider, home))
                .transpose()?;
            if let Some(proof) = &proof {
                *value = proof.physical.as_os_str().to_owned();
            }
            proof
        } else {
            None
        };
        if !plan_bound && let Ok(base) = crate::process::git(context, cwd, &["rev-parse", "HEAD"]) {
            environment.push((
                "OMS_BASE_SHA".into(),
                String::from_utf8_lossy(&base).trim().into(),
            ));
        }
        let configured = configured_model(
            route,
            environment
                .iter()
                .find(|(key, _)| key == "CODEX_HOME")
                .map(|(_, value)| value.as_os_str()),
        );
        let mut exec_argv = transport.argv.clone();
        let mut sandbox_backend = None;
        let sandbox_mode = match std::env::var("OMS_WORKER_SANDBOX").as_deref() {
            Ok("strict") => Some(oms_supervisor::sandbox::SandboxMode::Strict),
            Ok("auto") => Some(oms_supervisor::sandbox::SandboxMode::Auto),
            _ => None,
        };
        if let Some(mode) = sandbox_mode
            && !(mode == oms_supervisor::sandbox::SandboxMode::Auto && route.provider == "codex")
            && let Some((program, rest)) = exec_argv.split_first()
        {
            if let Some(runtime) = nonempty_env("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                && let Ok(meta) = std::fs::symlink_metadata(&runtime)
                && meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.uid() == rustix::process::geteuid().as_raw()
            {
                let _ = std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(runtime.join("oms-ssh"));
            }
            let mut view = environment.clone();
            if let Some(spec) = isolated(&route.provider)
                && !view.iter().any(|(key, _)| key == spec.env)
                && !remove_environment.iter().any(|key| key == spec.env)
                && let Some(value) = nonempty_env(spec.env)
            {
                view.push((spec.env.into(), value));
            }
            let mut extra_rw_dirs = env_rw_dirs(&view);
            if write {
                extra_rw_dirs.extend(oms_supervisor::sandbox::git_writable_dirs(cwd));
            }
            if let Some(home) = std::env::var_os("HOME") {
                for sub in [".claude", ".codex", ".gemini", ".config", ".cache"] {
                    let p = PathBuf::from(&home).join(sub);
                    if p.is_dir() {
                        extra_rw_dirs.push(p);
                    }
                }
                if route.provider == "pi" {
                    let p = PathBuf::from(&home).join(".pi");
                    if p.is_dir() && !extra_rw_dirs.contains(&p) {
                        extra_rw_dirs.push(p);
                    }
                }
            }
            let spec = oms_supervisor::sandbox::SandboxSpec {
                worktree: cwd.to_owned(),
                extra_rw_dirs,
                allow_network: true,
            };
            let wrapped = oms_supervisor::sandbox::wrap_command(program, rest, &spec, mode)
                .map_err(|e| Error::new(2, e))?;
            sandbox_backend = Some(wrapped.backend);
            for key in wrapped.remove_env {
                if !remove_environment.contains(&key) {
                    remove_environment.push(key);
                }
            }
            exec_argv = std::iter::once(wrapped.program)
                .chain(wrapped.args)
                .collect();
        }
        let deadline = std::time::Instant::now() + crate::process::NATIVE_ADMISSION_BUDGET;
        let team = crate::process::TeamLocator::from_extra(&extra_env)?;
        let physical_task = crate::usage::env(&extra_env, "OMS_TASK_ID");
        let attempt = crate::process::PhysicalAttempt {
            state_root: &usage_repo,
            contract_repo: account_repo,
            execution_repo: cwd,
            team: team.as_ref(),
            root: None,
            provider: &route.provider,
            account: account.as_deref().filter(|id| *id != "default"),
            selected_model: (route.model != "provider-default").then_some(route.model.as_str()),
            selected_effort: transport.selected_effort.as_deref(),
            task_id: physical_task.as_deref(),
            write,
            persistent_session: false,
        };
        let mut registered = None;
        context.executor.with_physical_contract(
            &attempt,
            deadline,
            context.interrupted,
            Box::new(|binding| {
                let pending = crate::usage::begin_before(
                    &usage_repo,
                    route,
                    account.as_deref(),
                    write,
                    &extra_env,
                    context.interrupted,
                    deadline,
                    binding,
                )?;
                let call = pending.physical_call()?;
                registered = Some(pending);
                Ok(call)
            }),
        )?;
        let pending_usage = registered.ok_or_else(|| {
            Error::new(
                75,
                "native admission returned without a physical acknowledgement",
            )
        })?;
        environment.push((
            "OMS_PROVIDER_CALL_ID".into(),
            pending_usage.receipt.identity.call_id.clone().into(),
        ));
        pending_usage.fence(context.interrupted)?;
        pending_usage.qualify(&attempt, context.executor, context.interrupted)?;
        let usage_receipt = pending_usage.receipt.clone();
        let request_context = crate::request_context::Prepared::new(
            &usage_repo,
            prompt,
            &usage_receipt.identity,
            &pending_usage.launch_fence(),
            context.interrupted,
        );
        pending_usage.fence(context.interrupted)?;
        if account_home.as_ref().is_some_and(|home| !home.current()) {
            let _ = crate::usage::finish(&usage_repo, usage_receipt, &[], false, None);
            return Err(Error::new(
                75,
                "provider account home changed before launch; reconcile before retrying",
            ));
        }
        let started = std::time::Instant::now();
        let direct = matches!(route.provider.as_str(), "codex" | "claude" | "antigravity")
            && exec_argv == transport.argv;
        let mut invocation = Invocation {
            argv: exec_argv,
            cwd: cwd.to_owned(),
            input: transport.input.clone(),
            environment,
            remove_environment,
            timeout: Duration::from_secs(timeouts.wall),
            stall,
            max_bytes: 64 * 1024 * 1024,
            merge: false,
            launch_fence: Some(pending_usage.launch_fence()),
            physical_call: Some(
                oms_supervisor::process::PhysicalCall::new(&usage_receipt.identity.call_id)
                    .map_err(|e| Error::new(2, e))?,
            ),
            heartbeat: context.heartbeat.cloned(),
            stream: stream.clone(),
        };
        let result = crate::plan_delegate::check_launch(&extra_env, &route.provider, cwd, context)
            .and_then(|plan| {
                if let Some(plan) = plan {
                    invocation
                        .environment
                        .push(("OMS_BASE_SHA".into(), plan.base_sha().into()));
                    invocation.launch_fence =
                        Some(pending_usage.launch_fence().with_plan_checkout(plan, cwd)?);
                }
                if let Some(protocol) = &team_protocol {
                    context.executor.execute_team_requests(
                        invocation,
                        crate::process::TeamLaunchBinding {
                            pending: &pending_usage,
                            attempt: &attempt,
                            protocol,
                            actions: &team_actions,
                        },
                        context.interrupted,
                    )
                } else {
                    context.executor.execute_provider(
                        invocation,
                        &usage_receipt.identity,
                        direct,
                        context.interrupted,
                    )
                }
            });
        let mut result = match result {
            Ok(result) => result,
            Err(error) => {
                let _ = crate::usage::finish(&usage_repo, usage_receipt, &[], false, None);
                if let Ok(mut request_context) = request_context {
                    let _ = request_context.returned(&usage_repo, false, context.interrupted);
                }
                return Err(error);
            }
        };
        let elapsed = started.elapsed();
        let elapsed_ms = elapsed.as_millis() as u64;
        if result.elapsed.is_zero() {
            result.elapsed = elapsed;
        }
        let (decoded, terminal) = if team_protocol.is_some() {
            (
                String::from_utf8_lossy(&result.stdout).into_owned(),
                result.complete,
            )
        } else if matches!(output_mode, OutputMode::Native) {
            match decode_native_output(&route.provider, &result.stdout) {
                Some(answer) => (answer, result.code == 0),
                None => {
                    result.stderr.extend_from_slice(
                        b"\nnative output completion could not be established\n",
                    );
                    // Preserve decoded provider diagnostics for capacity retries.
                    if let Some((diagnostic, _)) = decode_provider_events(
                        &route.provider,
                        &String::from_utf8_lossy(&result.stdout),
                        false,
                    ) {
                        result.stderr.extend_from_slice(diagnostic.as_bytes());
                    }
                    (String::new(), false)
                }
            }
        } else if plan_bound
            && matches!(route.provider.as_str(), "codex" | "claude" | "antigravity")
        {
            decode_bound_output(&route.provider, &result.stdout)
        } else {
            decode_output(&route.provider, &result.stdout)
        };
        let home_changed = account_home.as_ref().is_some_and(|home| !home.current());
        let account_observed = named_account.is_none()
            || account_home
                .as_ref()
                .is_some_and(|home| home.sha256.is_some());
        let accounting = if home_changed {
            crate::usage::finish_unobserved(
                &usage_repo,
                usage_receipt,
                Some(&result.stdout),
                result.cleanup_complete,
            )
        } else {
            crate::usage::finish(
                &usage_repo,
                usage_receipt,
                &result.stdout,
                result.complete
                    && terminal
                    && account_observed
                    && (route.provider != "antigravity" || result.code == 0),
                result.cleanup_complete,
            )
        }
        .map_err(|error| {
            Error::new(
                error.code,
                format!("provider usage accounting failed; reconcile before retrying: {error}"),
            )
        });
        let settlement = pending_usage.settle_execution(result.owned_cleanup.as_ref());
        accounting?;
        settlement?;
        // Optional context evidence cannot delay canonical settlement or turn a
        // completed invocation into a retry-eligible failure.
        let context_record = request_context
            .and_then(|mut prepared| prepared.returned(&usage_repo, true, context.interrupted));
        if home_changed {
            // The provider may have consumed the request. Preserve its unknown
            // receipt and cleanup evidence, but do not retry overflow/fallback.
            return Err(Error::new(
                75,
                "provider account home changed during invocation; reconcile before retrying",
            ));
        }
        if !account_observed {
            result.stderr.extend_from_slice(b"\nwarning: account-home continuity unavailable; usage attribution remains unknown\n");
        }
        if let OutputMode::Observed(slot, _) = output_mode {
            let observation = (result.code == 0
                && result.complete
                && terminal
                && account_observed
                && team_protocol.is_none()
                && !write)
                .then(|| crate::lead_observation::observe(&route.provider, &result.stdout))
                .flatten();
            slot.set(observation);
        }
        result.stdout = decoded.into_bytes();
        if !matches!(output_mode, OutputMode::Native)
            && !result.stdout.is_empty()
            && let Some(model) = configured
        {
            result
                .stdout
                .extend_from_slice(format!("\nconfigured model\n{model}\n").as_bytes());
        }
        result.complete &= terminal;
        let cap_hit = capacity(&result);
        record_seat_health(
            account_repo,
            &seat_key(&route.provider, account.as_deref().unwrap_or_default()),
            result.code == 0 && result.complete && !cap_hit,
            elapsed_ms,
            cap_hit,
            provider_fault(&result, cap_hit),
            context.interrupted,
        );
        // One same-provider retry on another account when the worktree stays untouched.
        if !plan_bound
            && overflow == 0
            && cap_hit
            && !route.account_pinned
            && let Some(account) = account
            && is_clean_retry(&write_snapshot, context, cwd)
        {
            excluded.insert(account);
            match account_environment(route, &excluded, account_repo) {
                Ok(next) if next.id.is_some() => {
                    rebound = Some(next);
                    continue;
                }
                _ => {}
            }
        }
        if context_record.is_err() {
            result.stderr.extend_from_slice(
                b"\nwarning: context request evidence unavailable; delivery remains unknown\n",
            );
        }
        if sandbox_backend == Some("unsandboxed-fallback") {
            result.stderr.extend_from_slice(
                b"\nwarning: worker ran without OS isolation (unsandboxed-fallback)\n",
            );
        }
        return Ok(result);
    }
    fail("account capacity overflow exhausted")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_with_fallback_env<P: crate::request_context::Input + ?Sized>(
    route: &mut Route,
    fallback: &str,
    prompt: &P,
    cwd: &Path,
    write: bool,
    timeouts: impl Into<crate::peer::Timeouts>,
    account_repo: &Path,
    extra_env: &[(OsString, OsString)],
    context: &Context<'_>,
) -> Result<Completed> {
    execute_with_fallback_output(
        route,
        fallback,
        prompt,
        cwd,
        write,
        timeouts.into(),
        account_repo,
        extra_env,
        OutputMode::Legacy,
        context,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_with_fallback_output<P: crate::request_context::Input + ?Sized>(
    route: &mut Route,
    fallback: &str,
    prompt: &P,
    cwd: &Path,
    write: bool,
    timeouts: crate::peer::Timeouts,
    account_repo: &Path,
    extra_env: &[(OsString, OsString)],
    output_mode: OutputMode<'_>,
    context: &Context<'_>,
) -> Result<Completed> {
    let extra_env = crate::usage::operation_env(extra_env)?;
    let plan_bound = crate::plan_delegate::has_launch_binding(&extra_env);
    // A fallback that reaches the CLI as the same selector is not a retry.
    let distinct =
        !fallback.is_empty() && cli_model(route, fallback) != cli_model(route, &route.model);
    let write_snapshot = (!plan_bound && write && distinct).then(|| repo_state(context, cwd));
    let result = execute_env_with_output(
        route,
        prompt,
        cwd,
        write,
        timeouts,
        account_repo,
        &extra_env,
        output_mode,
        context,
    )?;
    if !plan_bound && distinct && capacity(&result) && is_clean_retry(&write_snapshot, context, cwd)
    {
        model_name(fallback)?;
        route.model = fallback.into();
        route.explicit = true;
        if !route.account_pinned {
            route.account.clear();
        }
        execute_env_with_output(
            route,
            prompt,
            cwd,
            write,
            timeouts,
            account_repo,
            &extra_env,
            output_mode,
            context,
        )
    } else {
        Ok(result)
    }
}

pub(crate) fn capacity(result: &Completed) -> bool {
    let body = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    )
    .to_lowercase();
    (result.code != 0 || !result.complete)
        && [
            "quota",
            "rate limit",
            "rate_limit",
            "capacity",
            "usage limit",
            "spend cap",
        ]
        .iter()
        .any(|word| body.contains(word))
}

/// Provider/transport failure, as opposed to an ordinary worker task exit.
fn provider_fault(result: &Completed, capacity_hit: bool) -> bool {
    if capacity_hit || !result.complete || result.code == 124 {
        return true;
    }
    if result.code == 0 {
        return false;
    }
    let body = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    )
    .to_lowercase();
    [
        "429",
        "too many requests",
        "overloaded",
        "unavailable",
        "unauthorized",
        "authentication",
    ]
    .iter()
    .any(|word| body.contains(word))
}

fn configured_model(route: &Route, profile: Option<&std::ffi::OsStr>) -> Option<String> {
    if route.provider != "codex" || route.model != "provider-default" {
        return None;
    }
    let location = std::env::var_os("OMS_CODEX_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            profile
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex")
                })
                .join("config.toml")
        });
    let parent = location
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let raw = storage::root(parent)
        .ok()?
        .read_no_follow(
            Path::new(location.file_name()?),
            oms_safe_fs::ByteReadOptions {
                missing_ok: false,
                max_bytes: 4 * 1024 * 1024,
            },
        )
        .ok()?;
    let config = oms_toml::parse_bounded(&raw, 4 * 1024 * 1024).ok()?;
    let model = config.get("model")?.as_str()?;
    if !cached_regex!(r"\A[A-Za-z0-9][A-Za-z0-9._:+/-]{0,159}\z").is_match(model) {
        return None;
    }
    Some(model.to_owned())
}
fn running_accounts(repo: &Path) -> Result<std::collections::BTreeMap<String, usize>> {
    let state =
        oms_command_room::Services::state_repository(&oms_command_room::LocalServices, repo)
            .map_err(|e| Error::new(2, e.to_string()))?;
    let (_, rows) = oms_command_operations::load_projection(&state)?;
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        let refs = row.get("refs");
        let provider = if text(refs, "panel_provider").is_empty() {
            text(&row, "provider")
        } else {
            text(refs, "panel_provider")
        };
        if isolated(provider).is_none()
            || !["worker", "researcher", "advisor", "reviewer", "delegate"]
                .contains(&text(refs, "panel_role"))
            || ![
                "queued",
                "starting",
                "working",
                "running",
                "waiting_input",
                "waiting_approval",
                "verifying",
            ]
            .contains(&text(&row, "state"))
        {
            continue;
        }
        let account = text(refs, "panel_account");
        if !account.is_empty() {
            let key = if account.contains(':') {
                account.to_owned()
            } else {
                seat_key(provider, account)
            };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod bridge_route_tests {
    use super::*;

    #[test]
    fn capacity_includes_workspace_spend_cap() {
        let stop = |code, complete, text: &str| Completed {
            code,
            stdout: text.as_bytes().to_vec(),
            stderr: Vec::new(),
            complete,
            stalled: None,
            cleanup_complete: Some(true),
            owned_cleanup: None,
            elapsed: Duration::ZERO,
        };
        let cap = "You hit your spend cap set by the owner of your workspace.";
        assert!(capacity(&stop(0, false, cap)));
        assert!(capacity(&stop(1, true, cap)));
        assert!(!capacity(&stop(0, true, cap)));
    }
    use crate::process::Executor;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Command,
        sync::{Mutex, atomic::AtomicBool},
    };

    fn isolated_transport(name: &str, mode: &str, body: impl FnOnce(&Path, &Path)) {
        if std::env::var("TRANSPORT_CASE").as_deref() == Ok(name) {
            if std::env::var("TRANSPORT_MODE").as_deref() == Ok(mode) {
                let root = PathBuf::from(std::env::var_os("HOME").unwrap());
                body(&root.join("repo"), &root.join("isolated"));
            }
            return;
        }
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!(
            "oms-native-transport-{}-{name}-{mode}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Fixture(root);
        for dir in ["repo", "isolated", "tmp", "bin"] {
            fs::create_dir(fixture.0.join(dir)).unwrap();
        }
        for bin in [
            "codex",
            "claude",
            "agy",
            "cursor-agent",
            "grok",
            "gemini",
            "qwen",
            "opencode",
            "dsh",
            "vibe",
            "pi",
            "copilot",
            "droid",
            "aider",
            "muse",
            "kimi",
            "kiro-cli",
            "cn",
            "goose",
            "cline",
            "kilo",
            "openhands",
            "auggie",
            "oms-agent-adapter-fixture",
            "oms-agent-adapter-blocked",
            "phantom",
        ] {
            let path = fixture.0.join("bin").join(bin);
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .args([
                "--exact",
                &format!("providers::bridge_route_tests::{name}"),
                "--nocapture",
            ])
            .env("TRANSPORT_CASE", name)
            .env("TRANSPORT_MODE", mode)
            .env("HOME", &fixture.0)
            .env("PATH", fixture.0.join("bin"))
            .env(
                "OMS_RESUME_SESSION",
                if mode == "resume" {
                    "fixture-session"
                } else {
                    ""
                },
            )
            .env(
                "TMPDIR",
                fixture
                    .0
                    .join(if mode == "inside" { "isolated" } else { "tmp" }),
            )
            .env(
                "OMS_PEER_INTERACTIVE",
                if mode == "interactive" { "1" } else { "0" },
            )
            .env("OMS_PROVIDER_WRITE_ADAPTERS", "other, fixture ,")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
    }
    #[derive(Default)]
    struct Capture(Mutex<Vec<Invocation>>, Mutex<Vec<Option<String>>>);
    impl Executor for Capture {
        fn with_physical_contract(
            &self,
            attempt: &crate::process::PhysicalAttempt<'_>,
            _: std::time::Instant,
            stop: &AtomicBool,
            register: crate::process::RegisterPhysical<'_>,
        ) -> Result<()> {
            crate::check(stop)?;
            assert!(attempt.team.is_none());
            self.1
                .lock()
                .unwrap()
                .push(attempt.selected_effort.map(str::to_owned));
            register(crate::process::PhysicalContract::default()).map(|_| ())
        }
        fn execute(&self, request: Invocation, _: &AtomicBool) -> Result<Completed> {
            let git = request.argv[0] == "git";
            if !git {
                for pair in request.argv.windows(2) {
                    if ["--prompt-file", "--message-file", "-f", "--env-file"]
                        .iter()
                        .any(|flag| pair[0] == *flag)
                    {
                        let path = Path::new(&pair[1]);
                        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
                        assert_eq!(
                            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
                            0o700
                        );
                    }
                }
                self.0.lock().unwrap().push(request);
            }
            Ok(Completed {
                code: i32::from(git),
                stdout: b"fixture answer\n".to_vec(),
                stderr: Vec::new(),
                complete: true,
                stalled: None,
                cleanup_complete: Some(true),
                owned_cleanup: None,
                elapsed: Duration::ZERO,
            })
        }
    }
    fn direct(
        route: &Route,
        prompt: &[u8],
        write: bool,
        repo: &Path,
        cwd: &Path,
        capture: &Capture,
    ) -> Result<Completed> {
        let stop = AtomicBool::new(false);
        let context = Context {
            cwd,
            source: repo,
            interrupted: &stop,
            executor: capture,
            heartbeat: None,
        };
        let mut route = route.clone();
        execute_env(&mut route, prompt, cwd, write, 12, repo, &[], &context)
    }
    #[test]
    fn plan_bound_native_output_requires_a_terminal_envelope() {
        let cases: &[(&str, &[u8], bool)] = &[
            ("codex", br#"{"type":"turn.completed"}"#, true),
            ("codex", br#"{"type":"turn.failed"}"#, false),
            ("codex", br#"{"type":"bad","type":"turn.completed"}"#, false),
            (
                "claude",
                br#"{"type":"result","subtype":"success","is_error":false}"#,
                true,
            ),
            (
                "claude",
                br#"{"type":"result","subtype":"error_during_execution","is_error":true}"#,
                false,
            ),
            ("claude", br#"{"type":"result"}"#, false),
            (
                "antigravity",
                b"{\n\"status\":\"SUCCESS\",\n\"response\":\"done\"\n}",
                true,
            ),
            (
                "antigravity",
                br#"{"status":"ERROR","response":"failed"}"#,
                false,
            ),
            (
                "antigravity",
                br#"{"status":"SUCCESS","response":"short","stop_reason":"length"}"#,
                false,
            ),
            (
                "antigravity",
                br#"{"status":"SUCCESS","response":"bad","error":{}}"#,
                false,
            ),
            (
                "antigravity",
                br#"{"status":"SUCCESS","response":"truncated""#,
                false,
            ),
        ];
        let codex_open = concat!(
            "{\"type\":\"turn.completed\"}\n",
            "{\"type\":\"thread.started\",\"thread_id\":\"next\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"pending\"}}\n",
        );
        let claude_open = concat!(
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}\n",
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"pending\"}]}}\n",
        );
        for (provider, stream, terminal) in [
            ("codex", codex_open, "{\"type\":\"turn.completed\"}\n"),
            (
                "claude",
                claude_open,
                "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}\n",
            ),
        ] {
            assert!(
                !decode_bound_output(provider, stream.as_bytes()).1,
                "{provider}"
            );
            assert!(decode_output(provider, stream.as_bytes()).1, "{provider}");
            assert!(decode_bound_output(provider, format!("{stream}{terminal}").as_bytes()).1);
        }
        let codex_footer = concat!(
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":2}}\n",
            "{\"type\":\"usage\",\"usage\":{\"output_tokens\":1}}\n",
        );
        let claude_footer = concat!(
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}\n",
            "{\"type\":\"system\",\"subtype\":\"hook_response\",\"hook_event\":\"Stop\",\"outcome\":\"success\"}\n",
            "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\"}}\n",
        );
        assert!(decode_bound_output("codex", codex_footer.as_bytes()).1);
        assert!(decode_bound_output("claude", claude_footer.as_bytes()).1);
        let claude_partial_truncated = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"draft chunk\"}}}\n",
        );
        assert_eq!(
            decode_output("claude", claude_partial_truncated.as_bytes()),
            (String::new(), false)
        );
        assert_eq!(
            decode_bound_output("claude", claude_partial_truncated.as_bytes()),
            (String::new(), false)
        );
        let claude_partial_complete = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Inspecting \"}}}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":1}\"}}}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Inspecting files.\"},{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"a\":1}}],\"stop_reason\":\"tool_use\"}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"content\":\"ok\"}]}}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"final \"}}}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}}\n",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"final answer\"}],\"stop_reason\":\"end_turn\"}}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"final answer\"}\n",
        );
        let claude_without_partials = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Inspecting files.\"},{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"a\":1}}],\"stop_reason\":\"tool_use\"}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"content\":\"ok\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"final answer\"}],\"stop_reason\":\"end_turn\"}}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"final answer\"}\n",
        );
        assert_eq!(
            decode_output("claude", claude_partial_complete.as_bytes()),
            ("Inspecting files.\n\nfinal answer".into(), true)
        );
        assert_eq!(
            decode_output("claude", claude_partial_complete.as_bytes()),
            decode_output("claude", claude_without_partials.as_bytes())
        );
        assert_eq!(
            decode_bound_output("claude", claude_partial_complete.as_bytes()),
            decode_bound_output("claude", claude_without_partials.as_bytes())
        );
        assert!(
            !decode_bound_output(
                "codex",
                b"{\"type\":\"turn.completed\"}\n{\"type\":\"usage\"}\n",
            )
            .1
        );
        for &(provider, bytes, terminal) in cases {
            assert_eq!(
                decode_bound_output(provider, bytes).1,
                terminal,
                "{provider}: {bytes:?}"
            );
        }
        for provider in ["codex", "claude", "antigravity", "fixture", "cursor"] {
            assert_eq!(
                decode_output(provider, b"legacy explicit text"),
                ("legacy explicit text".into(), true)
            );
        }
    }

    #[test]
    fn physical_contracts_fence_each_retry_and_settle_owned_execution() {
        isolated_transport(
            "physical_contracts_fence_each_retry_and_settle_owned_execution",
            "normal",
            |repo, cwd| {
                use crate::process::{PhysicalAttempt, PhysicalContract, RegisterPhysical};
                use oms_command_telemetry::store::{resources, usage};
                use std::sync::atomic::{AtomicUsize, Ordering};
                struct Qualified {
                    binding: resources::Binding,
                    starts: AtomicUsize,
                    hooks: AtomicUsize,
                    mode: &'static str,
                    state: PathBuf,
                    context_guard: Mutex<Option<oms_lock::Guard>>,
                }
                impl Executor for Qualified {
                    fn with_physical_contract(
                        &self,
                        attempt: &PhysicalAttempt<'_>,
                        _: std::time::Instant,
                        _: &AtomicBool,
                        register: RegisterPhysical<'_>,
                    ) -> Result<()> {
                        let count = self.hooks.fetch_add(1, Ordering::Relaxed);
                        assert_eq!(attempt.provider, "claude");
                        assert_eq!(
                            attempt.selected_model,
                            Some(if count < 2 { "first" } else { "second" })
                        );
                        assert!(attempt.team.is_none() && attempt.root.is_none());
                        assert_eq!(attempt.selected_effort, Some("high"));
                        assert_eq!(attempt.owned().borrow().selected_effort, Some("high"));
                        if self.mode == "missing" || (self.mode == "missing-final" && count == 1) {
                            return Ok(());
                        }
                        let expires_at =
                            (self.mode == "expires").then(|| crate::seconds() as i64 + 1);
                        let call = register(PhysicalContract {
                            resource: Some(&self.binding),
                            expires_at,
                        })?;
                        if self.mode == "context-prepare" && count == 1 {
                            let parent = self.state.join(".oms/runtime/context");
                            fs::create_dir_all(&parent).unwrap();
                            fs::write(parent.join("calls"), b"fixture blocks evidence directory")
                                .unwrap();
                        }
                        if self.mode == "context-busy" && count == 1 {
                            let path = PathBuf::from(format!(
                                ".oms/runtime/context/calls/{}.json",
                                call.id()
                            ));
                            *self.context_guard.lock().unwrap() = Some(
                                storage::lock(attempt.state_root, &path, &AtomicBool::new(false))
                                    .unwrap(),
                            );
                        }
                        assert!(
                            usage::load(attempt.state_root)
                                .unwrap()
                                .calls
                                .iter()
                                .any(|entry| entry.receipt.identity.call_id == call.id())
                        );
                        if let Some(expiry) = expires_at {
                            let start = std::time::Instant::now();
                            while (crate::seconds() as i64) < expiry {
                                assert!(start.elapsed() < Duration::from_secs(3));
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                        Ok(())
                    }
                    fn execute(
                        &self,
                        mut request: Invocation,
                        stop: &AtomicBool,
                    ) -> Result<Completed> {
                        if request.argv[0] == "git" {
                            return Ok(Completed {
                                code: 0,
                                stdout: Vec::new(),
                                stderr: Vec::new(),
                                complete: true,
                                stalled: None,
                                cleanup_complete: Some(true),
                                owned_cleanup: None,
                                elapsed: Duration::ZERO,
                            });
                        }
                        let count = self.starts.fetch_add(1, Ordering::Relaxed);
                        assert_eq!(self.hooks.load(Ordering::Relaxed), (count + 1) * 2);
                        let retry = self.mode == "retry" && count == 0;
                        let physical_call = request.physical_call.as_ref().unwrap().id().to_owned();
                        request.argv = vec!["/bin/sh".into(), "-c".into(), format!(
                            "printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"local fixture\"}}'; {}",
                            if retry { "printf 'rate limit' >&2; exit 1" } else { "exit 0" }).into()];
                        let result = crate::process::Native.execute(request, stop)?;
                        assert!(result.owned_cleanup.is_some());
                        if self.mode == "context-returned" {
                            fs::write(
                                self.state.join(format!(
                                    ".oms/runtime/context/calls/{physical_call}.json"
                                )),
                                b"changed fixture evidence",
                            )
                            .unwrap();
                        }
                        if self.mode == "cancel" {
                            stop.store(true, Ordering::Release);
                        }
                        Ok(result)
                    }
                }
                for mode in [
                    "retry",
                    "cancel",
                    "expires",
                    "missing",
                    "missing-final",
                    "context-prepare",
                    "context-returned",
                    "context-busy",
                ] {
                    let state = repo.join(format!("contract-{mode}"));
                    fs::create_dir(&state).unwrap();
                    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
                    let stop = AtomicBool::new(false);
                    let policy = resources::Policy {
                        domain: "fixture-domain".into(),
                        epoch: "one".into(),
                        host_generation_sha256: "a".repeat(64),
                        max_teams: 1,
                        max_invocations: 1,
                        max_open_processes: 1,
                        max_write_wip: 1,
                        reviewer_headroom: 0,
                    };
                    resources::activate(
                        &state,
                        &policy,
                        None,
                        &stop,
                        std::time::Instant::now() + Duration::from_secs(2),
                    )
                    .unwrap();
                    let executor = Qualified {
                        state: state.clone(),
                        context_guard: Mutex::new(None),
                        binding: resources::Binding {
                            policy_sha256: policy.sha256().unwrap(),
                            host_generation_sha256: policy.host_generation_sha256.clone(),
                            root_owner: "fixture-main".into(),
                            root_generation: "fixture-creation".into(),
                            team: None,
                            reviewer: false,
                            persistent_session: false,
                            cleanup_scopes: vec![
                                oms_supervisor::process::Scope::ProcessGroup,
                                oms_supervisor::process::Scope::WholeTree,
                            ],
                        },
                        starts: AtomicUsize::new(0),
                        hooks: AtomicUsize::new(0),
                        mode,
                    };
                    let context = Context {
                        cwd,
                        source: repo,
                        interrupted: &stop,
                        executor: &executor,
                        heartbeat: None,
                    };
                    let mut route = Route {
                        provider: "claude".into(),
                        model: "first".into(),
                        effort: "high".into(),
                        explicit: true,
                        account: String::new(),
                        account_pinned: false,
                        public_research: false,
                    };
                    let extra = vec![
                        ("OMS_STATE_REPO".into(), state.as_os_str().into()),
                        ("OMS_OPERATION_ID".into(), "op_scoped".into()),
                    ];
                    let mut prompt = crate::request_context::Prompt::framing("Question:\n");
                    prompt.push_layer("task", "검증 🙂");
                    prompt
                        .untrusted(
                            "retrieved",
                            "fixture",
                            "selected context",
                            b"selected source",
                        )
                        .unwrap();
                    let request_started = std::time::Instant::now();
                    let result = execute_with_fallback_env(
                        &mut route, "second", &prompt, cwd, true, 10, repo, &extra, &context,
                    );
                    let occupancy = resources::occupancy(&state, &AtomicBool::new(false))
                        .unwrap()
                        .unwrap();
                    let calls = usage::load(&state).unwrap().calls;
                    match mode {
                        "retry" => {
                            assert_eq!(result.unwrap().code, 0);
                            assert_eq!(executor.starts.load(Ordering::Relaxed), 2);
                            assert_eq!(executor.hooks.load(Ordering::Relaxed), 4);
                            assert_eq!(calls.len(), 2);
                            let mut manifests = Vec::new();
                            for call in &calls {
                                let path = state.join(format!(
                                    ".oms/runtime/context/calls/{}.json",
                                    call.receipt.identity.call_id
                                ));
                                let raw = fs::read(&path).unwrap();
                                let recorded = V::parse(&raw).unwrap();
                                assert_eq!(recorded.get("operation_id").text(), Some("op_scoped"));
                                assert_eq!(
                                    recorded.get("call_id").text(),
                                    Some(call.receipt.identity.call_id.as_str())
                                );
                                let projection = oms_context::inspect_manifest(&raw, 100).unwrap();
                                assert_eq!(projection["dispatch"], "executor-returned");
                                assert!(projection["provider_delivery"].is_null());
                                let a = &projection["attribution"];
                                assert_eq!(a["request_bytes"], prompt.len());
                                assert_eq!(a["request_sha256"], storage::hash(prompt.as_bytes()));
                                assert_eq!(a["measured_bytes"], prompt.len());
                                assert_eq!(a["layers"]["task"]["bytes"], "검증 🙂".len());
                                assert!(a["layers"]["task"]["tokens"].is_null());
                                assert!(a["provider_added_unknown"]["bytes"].is_null());
                                manifests.push(raw);
                            }
                            let mut correction = calls[0].receipt.clone();
                            correction.revision += 1;
                            correction.phase = usage::Phase::Final;
                            correction.usage.input_tokens = Some(17);
                            correction.source_sha256 =
                                Some(storage::hash(b"late fixture usage correction"));
                            usage::record(&state, &correction, &AtomicBool::new(false)).unwrap();
                            assert_eq!(
                                usage::load(&state).unwrap().calls[0]
                                    .receipt
                                    .usage
                                    .input_tokens,
                                Some(17)
                            );
                            for (call, raw) in calls.iter().zip(manifests) {
                                assert_eq!(
                                    fs::read(state.join(format!(
                                        ".oms/runtime/context/calls/{}.json",
                                        call.receipt.identity.call_id
                                    )))
                                    .unwrap(),
                                    raw
                                );
                            }
                            assert_eq!(
                                (
                                    occupancy.invocations,
                                    occupancy.open_processes,
                                    occupancy.write_wip
                                ),
                                (0, 0, 1)
                            );
                        }
                        "cancel" => {
                            assert_eq!(result.unwrap().code, 0);
                            assert!(stop.load(Ordering::Acquire));
                            assert_eq!(calls.len(), 1);
                            assert_eq!(
                                (
                                    occupancy.invocations,
                                    occupancy.open_processes,
                                    occupancy.write_wip
                                ),
                                (0, 0, 1)
                            );
                        }
                        "context-prepare" | "context-returned" | "context-busy" => {
                            let result = result.unwrap();
                            assert_eq!(result.code, 0);
                            assert!(result.complete);
                            assert!(!capacity(&result));
                            assert!(
                                String::from_utf8_lossy(&result.stderr)
                                    .contains("context request evidence unavailable")
                            );
                            assert_eq!(route.model, "first");
                            assert_eq!(executor.starts.load(Ordering::Relaxed), 1);
                            assert_eq!(executor.hooks.load(Ordering::Relaxed), 2);
                            assert_eq!(calls.len(), 1);
                            assert!(request_started.elapsed() < Duration::from_secs(3));
                            assert_eq!(
                                (
                                    occupancy.invocations,
                                    occupancy.open_processes,
                                    occupancy.write_wip
                                ),
                                (0, 0, 1)
                            );
                        }
                        "expires" | "missing-final" => {
                            assert_eq!(result.unwrap_err().code, 75);
                            assert_eq!(executor.starts.load(Ordering::Relaxed), 0);
                            assert_eq!(calls.len(), 1);
                            assert_eq!(calls[0].receipt.phase, usage::Phase::Pending);
                            assert_eq!(
                                (
                                    occupancy.invocations,
                                    occupancy.open_processes,
                                    occupancy.write_wip
                                ),
                                (1, 1, 1)
                            );
                        }
                        "missing" => {
                            assert_eq!(result.unwrap_err().code, 75);
                            assert_eq!(executor.starts.load(Ordering::Relaxed), 0);
                            assert!(calls.is_empty());
                            assert_eq!(occupancy, resources::Occupancy::default());
                        }
                        _ => unreachable!(),
                    }
                    assert!(
                        calls
                            .iter()
                            .all(|entry| entry.tokens.is_none() && entry.cost_microusd.is_none())
                    );
                }
            },
        );
    }
    #[test]
    fn antigravity_receipt_requires_successful_process_exit() {
        isolated_transport(
            "antigravity_receipt_requires_successful_process_exit",
            "normal",
            |repo, cwd| {
                use oms_command_telemetry::store::usage::{self, Phase, Usage};

                struct Report(Completed);
                impl Executor for Report {
                    fn execute(&self, request: Invocation, stop: &AtomicBool) -> Result<Completed> {
                        if request.argv[0] == "git" {
                            return Capture::default().execute(request, stop);
                        }
                        assert_eq!(request.argv[0], "agy");
                        Ok(self.0.clone())
                    }
                }

                let success = concat!(
                    r#"{"conversation_id":"fixture-one","status":"SUCCESS","response":" fixture answer\n","#,
                    r#""duration_seconds":0.1,"num_turns":1,"usage":{"input_tokens":10,"output_tokens":2,"#,
                    r#""thinking_tokens":1,"cache_read_tokens":9,"total_tokens":12}}"#,
                    "\n"
                );
                let answer = " fixture answer\n";
                let failed = success.replace("SUCCESS", "ERROR");
                let truncated = success.strip_suffix("}\n").unwrap();
                let stopped = success.replacen('{', r#"{"stop_reason":"length","#, 1);
                let cases = [
                    (1, (true, true), success, answer, None),
                    (0, (true, true), success, answer, Some(12)),
                    (0, (false, false), success, answer, None),
                    (0, (true, true), failed.as_str(), answer, None),
                    (0, (true, true), truncated, truncated, None),
                    (0, (true, false), stopped.as_str(), answer, None),
                ];
                let mut route = Route {
                    provider: "antigravity".into(),
                    model: "provider-default".into(),
                    effort: String::new(),
                    explicit: false,
                    account: String::new(),
                    account_pinned: false,
                    public_research: false,
                };
                let stop = AtomicBool::new(false);
                for (i, (code, (complete, decoded_complete), raw, stdout, tokens)) in
                    cases.into_iter().enumerate()
                {
                    let report = Report(Completed {
                        code,
                        stdout: raw.as_bytes().to_vec(),
                        stderr: vec![],
                        complete,
                        stalled: None,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: Duration::ZERO,
                    });
                    let context = Context {
                        cwd,
                        source: repo,
                        interrupted: &stop,
                        executor: &report,
                        heartbeat: None,
                    };
                    let result =
                        execute_env(&mut route, b"fixture", cwd, false, 12, repo, &[], &context)
                            .unwrap();
                    assert_eq!(result.code, code);
                    assert_eq!(result.complete, decoded_complete);
                    assert_eq!(result.stdout, stdout.as_bytes());
                    assert_eq!(result.cleanup_complete, Some(true));

                    let calls = usage::load(repo).unwrap().calls;
                    assert_eq!(calls.len(), i + 1);
                    let call = &calls[i];
                    assert_eq!(
                        call.receipt.phase,
                        tokens.map_or(Phase::Unknown, |_| Phase::Final),
                        "case {i}"
                    );
                    assert_eq!(call.tokens, tokens);
                    assert_eq!(call.receipt.cleanup_complete, Some(true));
                    if tokens.is_none() {
                        assert_eq!(call.receipt.usage, Usage::default());
                    }
                }
            },
        );
    }

    #[test]
    fn physical_provider_calls_record_fallback_and_timeout_usage() {
        isolated_transport(
            "physical_provider_calls_record_fallback_and_timeout_usage",
            "normal",
            |repo, cwd| {
                use oms_command_telemetry::store::usage::{self, Phase};
                struct Reports(std::sync::atomic::AtomicUsize, PathBuf);
                impl Executor for Reports {
                    fn execute(
                        &self,
                        request: Invocation,
                        interrupted: &AtomicBool,
                    ) -> Result<Completed> {
                        if request.argv[0] == "git" {
                            return Ok(Completed {
                                code: 0,
                                stdout: vec![],
                                stderr: vec![],
                                complete: true,
                                stalled: None,
                                cleanup_complete: Some(true),
                                owned_cleanup: None,
                                elapsed: Duration::ZERO,
                            });
                        }
                        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let calls = usage::load(&self.1).unwrap().calls;
                        assert_eq!(calls.last().unwrap().receipt.phase, Phase::Pending);
                        let intents =
                            oms_command_telemetry::store::physical::load(&self.1, interrupted)
                                .unwrap();
                        assert_eq!(intents.len(), n + 1);
                        let current = &calls.last().unwrap().receipt;
                        let intent = intents
                            .iter()
                            .find(|intent| intent.identity.call_id == current.identity.call_id)
                            .unwrap();
                        assert_eq!(intent.identity, current.identity);
                        assert_eq!(intent.usage_ordinal, Some(current.ordinal));
                        assert_eq!(
                            intent.phase,
                            oms_command_telemetry::store::physical::Phase::Ready
                        );
                        assert!(intent.write);
                        if let Some((_, digest)) = request
                            .environment
                            .iter()
                            .find(|(key, _)| key == "OMS_CAMPAIGN_MANIFEST_SHA256")
                        {
                            let digest = digest.to_str().unwrap();
                            assert_eq!(intent.campaign_manifest_sha256.as_deref(), Some(digest));
                            let directory =
                                oms_command_telemetry::store::campaign::directory(digest).unwrap();
                            let campaign_guard = oms_command_telemetry::store::lock_file_before(
                                &self.1.join(directory).join("events.jsonl"),
                                interrupted,
                                std::time::Instant::now() + Duration::from_millis(100),
                            )
                            .unwrap();
                            drop(campaign_guard);
                        } else {
                            assert_eq!(intent.campaign_manifest_sha256, None);
                        }
                        let _guard = oms_command_telemetry::store::dispatch::Transaction::acquire(
                            &self.1,
                            Duration::from_millis(100),
                            interrupted,
                        )
                        .unwrap();
                        assert!(
                            request
                                .environment
                                .iter()
                                .any(|(k, _)| k == "OMS_PROVIDER_CALL_ID")
                        );
                        assert_eq!(
                            request.physical_call.as_ref().map(|call| call.id()),
                            Some(intent.identity.call_id.as_str())
                        );
                        Ok(Completed {
                        code:if n == 0 {1} else if n == 2 {124} else {0},
                        stdout:format!("{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"fixture answer\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":0}},\"total_cost_usd\":0.0000{}}}\n",n+1).into_bytes(),
                        stderr:if n == 0 {b"rate limit".to_vec()} else {vec![]},
                        complete:n != 2,stalled:None,cleanup_complete:Some(true),owned_cleanup:None,elapsed:Duration::ZERO,
                    })
                    }
                }
                let stop = AtomicBool::new(false);
                let state = repo.join("shared-state");
                std::fs::create_dir(&state).unwrap();
                let reports = Reports(std::sync::atomic::AtomicUsize::new(0), state.clone());
                let context = Context {
                    cwd,
                    source: repo,
                    interrupted: &stop,
                    executor: &reports,
                    heartbeat: None,
                };
                let mut route = Route {
                    provider: "claude".into(),
                    model: "first".into(),
                    effort: String::new(),
                    explicit: true,
                    account: String::new(),
                    account_pinned: false,
                    public_research: false,
                };
                let extra = vec![
                    ("OMS_STATE_REPO".into(), state.as_os_str().to_owned()),
                    ("OMS_OPERATION_ID".into(), "op_campaign".into()),
                    ("OMS_GOAL_ID".into(), "goal_campaign".into()),
                    ("OMS_TASK_ID".into(), "task_one".into()),
                    ("OMS_ATTEMPT_ID".into(), "att_one".into()),
                ];
                let result = execute_with_fallback_env(
                    &mut route, "second", b"fixture", cwd, true, 12, repo, &extra, &context,
                )
                .unwrap();
                assert_eq!(result.code, 0);
                let calls = usage::load(&state).unwrap().calls;
                assert!(usage::load(repo).unwrap().calls.is_empty());
                assert_eq!(calls.len(), 2);
                assert_ne!(
                    calls[0].receipt.identity.call_id,
                    calls[1].receipt.identity.call_id
                );
                assert!(
                    calls
                        .iter()
                        .all(|c| c.receipt.identity.operation_id == "op_campaign"
                            && c.receipt.identity.attempt_id.as_deref() == Some("att_one"))
                );
                assert_eq!(
                    calls.iter().map(|c| c.cost_microusd.unwrap()).sum::<u64>(),
                    30
                );
                assert!(calls.iter().all(|c| c.receipt.phase == Phase::Final));
                let timed = execute_env(
                    &mut route, b"fixture", cwd, true, 12, repo, &extra, &context,
                )
                .unwrap();
                assert_eq!(timed.code, 124);
                let calls = usage::load(&state).unwrap().calls;
                assert_eq!(calls[2].receipt.phase, Phase::Unknown);
                assert_eq!(calls[2].cost_microusd, Some(30));
                assert_eq!(calls[2].receipt.cleanup_complete, Some(true));

                let bounded = repo.join("bounded-state");
                std::fs::create_dir(&bounded).unwrap();
                std::fs::set_permissions(&bounded, std::fs::Permissions::from_mode(0o700)).unwrap();
                let raw = format!(
                    r#"{{"problem":{{"file":"problem.txt","sha256":"{}"}},"base":"{}",
                    "verify":{{"command":"true","timeout_s":30,"inputs":[]}},"candidate_formats":["text"],
                    "groups":[{{"id":"group","formulation":"fixture","k":1,"routes":[{{"provider":"claude","model":"first"}}]}}],
                    "budget":{{"max_concurrent":4,"total_calls":10,"deadline_s":60,"max_waves":2,"stall_waves":2,
                    "verifier":{{"concurrency":1,"timeout_s":30,"max_artifact_bytes":1024}},
                    "usage":{{"max_tokens":60,"per_call":{{"tokens":60}}}}}}}}"#,
                    "a".repeat(64),
                    "b".repeat(40),
                );
                let manifest = oms_json::parse_strict(raw.as_bytes()).unwrap();
                let raw = oms_json::canonical_json(&manifest).unwrap();
                let digest = crate::storage::hash(&raw);
                let directory = format!(".oms/plan/campaigns/{}", &digest[..16]);
                let store = oms_command_plan::storage::Store::open(&bounded).unwrap();
                store
                    .publish_once(format!("{directory}/manifest.json"), &raw, 1024 * 1024)
                    .unwrap();
                let budget = String::from_utf8(
                    oms_json::canonical_json(manifest.get("budget").unwrap()).unwrap(),
                )
                .unwrap();
                let started = format!(
                    r#"{{"type":"campaign_started","budget":{budget},"started_at":{:?}}}"#,
                    crate::now()
                );
                store
                    .append(
                        format!("{directory}/events.jsonl"),
                        &oms_command_plan::json::parse(started.as_bytes()).unwrap(),
                    )
                    .unwrap();
                let operation = crate::id("res_").unwrap();
                let reservation = format!(
                    r#"{{"type":"reservation","id":"{operation}","provider":"claude","kind":"seat","reserved_usage":{{"tokens":60,"cost_microusd":null}}}}"#
                );
                store
                    .append(
                        format!("{directory}/events.jsonl"),
                        &oms_command_plan::json::parse(reservation.as_bytes()).unwrap(),
                    )
                    .unwrap();
                let bounded_reports =
                    Reports(std::sync::atomic::AtomicUsize::new(0), bounded.clone());
                let bounded_context = Context {
                    executor: &bounded_reports,
                    ..context
                };
                let extra = vec![
                    ("OMS_STATE_REPO".into(), bounded.as_os_str().to_owned()),
                    ("OMS_OPERATION_ID".into(), operation.clone().into()),
                    ("OMS_GOAL_ID".into(), digest[..16].into()),
                    ("OMS_CAMPAIGN_MANIFEST_SHA256".into(), digest.clone().into()),
                ];
                route.model = "first".into();
                let error = execute_with_fallback_env(
                    &mut route,
                    "second",
                    b"fixture",
                    cwd,
                    true,
                    12,
                    repo,
                    &extra,
                    &bounded_context,
                )
                .unwrap_err();
                assert_eq!(
                    error.code, 3,
                    "each physical fallback must pass campaign admission"
                );
                assert_eq!(
                    bounded_reports.0.load(std::sync::atomic::Ordering::Relaxed),
                    1
                );
                assert_eq!(usage::load(&bounded).unwrap().calls.len(), 1);
                assert_eq!(
                    oms_command_telemetry::store::physical::load(&bounded, &stop)
                        .unwrap()
                        .len(),
                    1
                );
                let registry = bounded.join(oms_command_telemetry::store::dispatch::REGISTRY);
                let before = std::fs::read(&registry).unwrap();
                for (key, value) in [
                    ("OMS_CAMPAIGN_MANIFEST_SHA256", String::new()),
                    (
                        "OMS_CAMPAIGN_MANIFEST_SHA256",
                        format!("{}{}", &digest[..16], "e".repeat(48)),
                    ),
                    ("OMS_GOAL_ID", "other_goal".into()),
                    ("OMS_OPERATION_ID", format!("res_{}", "0".repeat(32))),
                ] {
                    let mut invalid = extra.clone();
                    invalid.push((key.into(), value.into()));
                    let error = execute_env(
                        &mut route,
                        b"fixture",
                        cwd,
                        true,
                        12,
                        repo,
                        &invalid,
                        &bounded_context,
                    )
                    .unwrap_err();
                    assert_eq!(
                        error.code, 2,
                        "invalid explicit campaign binding must not become unbudgeted: {error}"
                    );
                    assert_eq!(
                        bounded_reports.0.load(std::sync::atomic::Ordering::Relaxed),
                        1
                    );
                    assert_eq!(std::fs::read(&registry).unwrap(), before);
                    assert_eq!(usage::load(&bounded).unwrap().calls.len(), 1);
                }
                store
                    .append(
                        format!("{directory}/events.jsonl"),
                        &oms_command_plan::json::parse(
                            format!(
                                r#"{{"type":"settled","id":"{operation}","outcome":"refused"}}"#
                            )
                            .as_bytes(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                let error = execute_env(
                    &mut route,
                    b"fixture",
                    cwd,
                    true,
                    12,
                    repo,
                    &extra,
                    &bounded_context,
                )
                .unwrap_err();
                assert_eq!(
                    error.code, 2,
                    "settled reservation cannot authorize another physical call"
                );
                assert_eq!(
                    bounded_reports.0.load(std::sync::atomic::Ordering::Relaxed),
                    1
                );

                let guarded = repo.join("guarded-state");
                fs::create_dir(&guarded).unwrap();
                fs::set_permissions(&guarded, fs::Permissions::from_mode(0o700)).unwrap();
                let guarded_store = oms_command_plan::storage::Store::open(&guarded).unwrap();
                guarded_store
                    .publish_once(format!("{directory}/manifest.json"), &raw, 1024 * 1024)
                    .unwrap();
                for row in [&started, &reservation] {
                    guarded_store
                        .append(
                            format!("{directory}/events.jsonl"),
                            &oms_command_plan::json::parse(row.as_bytes()).unwrap(),
                        )
                        .unwrap();
                }
                let receipt_target = oms_command_telemetry::store::canonical_index(
                    &guarded,
                    Some(Path::new(usage::RECEIPTS)),
                    true,
                    false,
                )
                .unwrap();
                let settings = oms_lock::Settings::from_env().unwrap();
                let held = oms_lock::Lock::for_file(&receipt_target, settings.clone())
                    .unwrap()
                    .try_acquire()
                    .unwrap();
                let events_lock = oms_lock::Lock::for_file(
                    &guarded_store.repo().join(&directory).join("events.jsonl"),
                    settings,
                )
                .unwrap();
                let guarded_reports =
                    Reports(std::sync::atomic::AtomicUsize::new(0), guarded.clone());
                let guarded_context = Context {
                    executor: &guarded_reports,
                    ..bounded_context
                };
                let mut guarded_env = extra.clone();
                guarded_env.push(("OMS_STATE_REPO".into(), guarded.as_os_str().to_owned()));
                let mut guarded_route = route.clone();
                std::thread::scope(|scope| {
                    let worker = scope.spawn(|| {
                        execute_env(
                            &mut guarded_route,
                            b"fixture",
                            cwd,
                            true,
                            12,
                            repo,
                            &guarded_env,
                            &guarded_context,
                        )
                    });
                    let deadline = std::time::Instant::now() + Duration::from_secs(3);
                    let mut reserved = false;
                    while std::time::Instant::now() < deadline {
                        if oms_command_telemetry::store::physical::load(&guarded, &stop)
                            .is_ok_and(|intents| !intents.is_empty())
                        {
                            reserved = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(
                        reserved,
                        "physical registration reached the nested receipt wait"
                    );
                    let moved = events_lock.path().with_extension("fixture-moved");
                    assert!(!moved.exists());
                    fs::rename(events_lock.path(), &moved).unwrap();
                    drop(held);
                    let outcome = worker.join().unwrap();
                    if moved.is_dir() {
                        fs::remove_dir_all(moved).unwrap();
                    } else {
                        fs::remove_file(moved).unwrap();
                    }
                    let error = outcome.unwrap_err();
                    assert_eq!(error.code, 75, "{error}");
                    assert!(error.message.contains("campaign guard changed"), "{error}");
                    assert_eq!(
                        guarded_reports.0.load(std::sync::atomic::Ordering::Relaxed),
                        0
                    );
                    let pending = usage::load(&guarded).unwrap().calls;
                    assert_eq!(pending.len(), 1);
                    assert_eq!(pending[0].receipt.phase, Phase::Pending);
                    let intents =
                        oms_command_telemetry::store::physical::load(&guarded, &stop).unwrap();
                    assert_eq!(intents.len(), 1);
                    assert_eq!(intents[0].usage_ordinal, Some(pending[0].receipt.ordinal));
                });
            },
        );
    }
    #[test]
    fn team_protocol_capability_is_strict_owned_and_rechecked() {
        let scratch = ProviderScratch::new("team-protocol", None).unwrap();
        let binary = scratch
            .file("oms-agent-adapter-fixture", b"fixture adapter bytes")
            .unwrap();
        let manifest = PathBuf::from(format!("{}.oms-team-request-v1.json", binary.display()));
        let route = Route {
            provider: "fixture".into(),
            model: "fixture".into(),
            effort: "high".into(),
            explicit: true,
            account: String::new(),
            account_pinned: false,
            public_research: false,
        };
        let declare = |schema: u8| {
            std::fs::write(&manifest,format!("{{\"schema\":{schema},\"protocol\":\"oms-team-request-v1\",\"binary_sha256\":\"{}\"}}",storage::hash(b"fixture adapter bytes"))).unwrap();
            std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        declare(1);
        let selected = team_protocol(&route, &[binary.clone().into_os_string()]).unwrap();
        selected.check().unwrap();
        std::fs::write(&binary, b"changed adapter bytes").unwrap();
        assert!(selected.check().is_err());
        std::fs::write(&binary, b"fixture adapter bytes").unwrap();
        declare(2);
        assert!(team_protocol(&route, &[binary.clone().into_os_string()]).is_err());
        declare(1);
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(team_protocol(&route, &[binary.clone().into_os_string()]).is_err());
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        let linked = binary.with_file_name("linked");
        std::fs::hard_link(&binary, &linked).unwrap();
        assert!(team_protocol(&route, &[binary.clone().into_os_string()]).is_err());
        let builtin = Route {
            provider: "codex".into(),
            ..route
        };
        assert!(team_protocol(&builtin, &[binary.into_os_string()]).is_err());
    }

    #[test]
    fn native_write_transports() {
        for mode in ["normal", "resume"] {
            isolated_transport("native_write_transports", mode, |repo, cwd| {
                let cases = [
                    (
                        "codex",
                        "exec|{model}|{effort}|--json|--sandbox|workspace-write|-",
                        "-c|model_reasoning_effort=\"high\"",
                    ),
                    (
                        "claude",
                        "{model}|{effort}|--output-format|stream-json|--verbose|-p",
                        "--effort|high",
                    ),
                    (
                        "antigravity",
                        "{model}|--add-dir|{cwd}|{effort}|--sandbox|--print-timeout|12s|--output-format|json|--print|complete fixture",
                        "--effort|high",
                    ),
                    (
                        "cursor",
                        "--print|--output-format|text|--workspace|{cwd}|--sandbox|enabled|{model}|--force",
                        "",
                    ),
                    (
                        "grok",
                        "--no-auto-update|--cwd|{cwd}|--output-format|plain|{model}|{effort}|--always-approve|--sandbox|workspace|--deny|Bash(git push*)|-p|complete fixture",
                        "--effort|high",
                    ),
                    (
                        "gemini",
                        "--output-format|text|--sandbox|{model}|--approval-mode|yolo",
                        "",
                    ),
                    (
                        "qwen",
                        "--output-format|text|--sandbox|{model}|--approval-mode|yolo",
                        "",
                    ),
                    (
                        "opencode",
                        "run|--format|default|{model}|--agent|build|--auto|complete fixture",
                        "",
                    ),
                    ("deepseek", "--profile|headless|complete fixture", ""),
                    (
                        "vibe",
                        "--trust|--max-turns|24|--output|text|--agent|accept-edits|--enabled-tools|read_file|--enabled-tools|grep|--enabled-tools|write_file|--enabled-tools|edit|--prompt|complete fixture",
                        "",
                    ),
                    (
                        "pi",
                        "--no-session|--no-approve|--no-extensions|--no-skills|--no-prompt-templates|--mode|json|{model}|{effort}|--tools|read,bash,edit,write,grep,find,ls|--print|complete fixture",
                        "--thinking|high",
                    ),
                    (
                        "copilot",
                        "-s|--no-ask-user|--disable-builtin-mcps|--disallow-temp-dir|{model}|{effort}|--available-tools|view,grep,glob,edit,create,apply_patch|--allow-tool|read,write|--deny-tool|shell|--deny-tool|url|--deny-tool|memory|-p|complete fixture",
                        "--effort|high",
                    ),
                    (
                        "droid",
                        "exec|--cwd|{cwd}|--output-format|text|--disable-builtin-skills|{model}|{effort}|--auto|low|-f|{file}",
                        "--reasoning-effort|high",
                    ),
                    (
                        "aider",
                        "--no-auto-commits|--no-dirty-commits|--no-gitignore|--no-suggest-shell-commands|--no-auto-lint|--no-auto-test|--no-check-update|--no-detect-urls|--no-pretty|--no-stream|--no-analytics|--no-restore-chat-history|--env-file|{env}|--input-history-file|{input}|--chat-history-file|{chat}|{model}|{effort}|--chat-mode|code|--yes-always|--no-dry-run|--message-file|{file}",
                        "--reasoning-effort|high",
                    ),
                    (
                        "muse",
                        "exec|{model}|{effort}|--disable-approval|complete fixture",
                        "--reasoning-effort|high",
                    ),
                    ("kimi", "{model}|-p|complete fixture", ""),
                    (
                        "kiro",
                        "chat|--v3|--no-interactive|{model}|{effort}|--trust-all-tools|complete fixture",
                        "--effort|high",
                    ),
                    ("continue", "-p|complete fixture|{model}|--auto", ""),
                    ("goose", "run|--no-session|{model}|-t|complete fixture", ""),
                    (
                        "cline",
                        "{model}|{effort}|--auto-approve|true|complete fixture",
                        "--thinking|high",
                    ),
                    (
                        "kilo",
                        "run|--auto|{model}|{effort}|complete fixture",
                        "--variant|high",
                    ),
                    ("openhands", "--headless|-t|complete fixture", ""),
                    (
                        "auggie",
                        "--print|{model}|--permission|edit:allow|--permission|write:allow|--permission|terminal:allow|complete fixture",
                        "",
                    ),
                    (
                        "fixture",
                        "run|--access|write|--workdir|{cwd}|--prompt-file|{file}|{model}|{effort}",
                        "--effort|high",
                    ),
                ];
                for (provider, pattern, flag) in cases {
                    for exact in [false, true] {
                        let native_model = ["deepseek", "vibe", "openhands"].contains(&provider);
                        let route = Route {
                            provider: provider.into(),
                            model: if exact && !native_model {
                                "fixture-model"
                            } else {
                                "provider-default"
                            }
                            .into(),
                            effort: if exact { "high" } else { "" }.into(),
                            explicit: true,
                            account: String::new(),
                            account_pinned: false,
                            public_research: false,
                        };
                        let capture = Capture::default();
                        assert_eq!(
                            direct(&route, b"complete fixture", true, repo, cwd, &capture)
                                .unwrap()
                                .code,
                            0
                        );
                        let calls = capture.0.lock().unwrap();
                        assert_eq!(calls.len(), 1);
                        let request = &calls[0];
                        let file = request
                            .argv
                            .windows(2)
                            .find(|p| {
                                ["--prompt-file", "--message-file", "-f"]
                                    .iter()
                                    .any(|f| p[0] == *f)
                            })
                            .map(|p| PathBuf::from(&p[1]));
                        let scratch = file
                            .as_ref()
                            .and_then(|p| p.parent())
                            .unwrap_or(Path::new(""));
                        let mut expanded = pattern
                            .replace(
                                "{model}",
                                if exact && provider == "antigravity" {
                                    // The CLI receives the exact effort selector.
                                    "--model|fixture-model-high"
                                } else if exact && !native_model {
                                    "--model|fixture-model"
                                } else {
                                    ""
                                },
                            )
                            .replace("{effort}", if exact { flag } else { "" });
                        if mode == "resume" {
                            if provider == "codex" {
                                expanded = expanded.strip_suffix('-').unwrap().to_owned()
                                    + "resume|--all|fixture-session|-";
                            } else if provider == "claude" {
                                expanded = "--resume|fixture-session|".to_owned() + &expanded;
                            } else if provider == "antigravity" {
                                expanded = "--conversation|fixture-session|".to_owned() + &expanded;
                            }
                        }
                        let expected: Vec<OsString> = expanded
                            .split('|')
                            .filter(|s| !s.is_empty())
                            .map(|s| match s {
                                "{cwd}" => cwd.as_os_str().into(),
                                "{file}" => file.as_ref().unwrap().as_os_str().into(),
                                "{env}" => scratch.join("empty.env").into_os_string(),
                                "{input}" => scratch.join("input.history").into_os_string(),
                                "{chat}" => scratch.join("chat.history").into_os_string(),
                                _ => s.into(),
                            })
                            .collect();
                        assert_eq!(request.argv[1..], expected, "{provider} exact={exact}");
                        let selected_effort = (exact && !flag.is_empty()).then(|| "high".into());
                        assert_eq!(
                            *capture.1.lock().unwrap(),
                            vec![selected_effort.clone(), selected_effort],
                            "{provider} exact={exact}"
                        );
                        if file.is_some() {
                            assert!(!scratch.exists());
                        }
                        for (key, value) in match provider {
                            "deepseek" => vec![
                                ("DSH_PERMISSION_MODE", "workspace-write"),
                                ("DSH_TELEMETRY_DISABLED", "1"),
                            ],
                            "pi" => vec![("PI_SKIP_VERSION_CHECK", "1")],
                            "goose" => vec![("GOOSE_MODE", "auto")],
                            "auggie" => vec![("AUGMENT_DISABLE_AUTO_UPDATE", "1")],
                            _ => vec![],
                        } {
                            assert!(
                                request.environment.contains(&(key.into(), value.into())),
                                "{provider}"
                            );
                        }
                        if provider == "claude" {
                            let stop = AtomicBool::new(false);
                            let stall_capture = Capture::default();
                            let stall_ctx = Context {
                                cwd,
                                source: repo,
                                interrupted: &stop,
                                executor: &stall_capture,
                                heartbeat: None,
                            };
                            let mut stall_route = route.clone();
                            execute_env_with_output(
                                &mut stall_route,
                                b"complete fixture".as_slice(),
                                cwd,
                                true,
                                crate::peer::Timeouts {
                                    wall: 86_400,
                                    stall: Some(1_200),
                                },
                                repo,
                                &[],
                                OutputMode::Legacy,
                                &stall_ctx,
                            )
                            .unwrap();
                            let stall_calls = stall_capture.0.lock().unwrap();
                            let mut expected_with_partial = expected.clone();
                            let pos = expected_with_partial
                                .iter()
                                .rposition(|a| a == "-p")
                                .unwrap();
                            expected_with_partial.insert(pos, "--include-partial-messages".into());
                            assert_eq!(
                                stall_calls[0].argv[1..],
                                expected_with_partial,
                                "claude write with stall active exact={exact}"
                            );
                            assert_eq!(stall_calls[0].stall, Some(Duration::from_secs(1_200)));
                        }
                    }
                }
                for provider in ["deepseek", "vibe", "openhands", "blocked", "phantom"] {
                    let route = Route {
                        provider: provider.into(),
                        model: "fixture-model".into(),
                        effort: String::new(),
                        explicit: true,
                        account: String::new(),
                        account_pinned: false,
                        public_research: false,
                    };
                    let capture = Capture::default();
                    assert!(
                        direct(&route, b"inspect", true, repo, cwd, &capture).is_err(),
                        "{provider}"
                    );
                    assert!(capture.0.lock().unwrap().is_empty());
                }
                for provider in ["antigravity", "aider", "fixture"] {
                    let route = Route {
                        provider: provider.into(),
                        model: "provider-default".into(),
                        effort: String::new(),
                        explicit: false,
                        account: String::new(),
                        account_pinned: false,
                        public_research: false,
                    };
                    let capture = Capture::default();
                    assert!(direct(&route, b"inspect", false, repo, repo, &capture).is_err());
                    let alias = cwd.join(format!("{provider}-alias"));
                    std::os::unix::fs::symlink(repo, &alias).unwrap();
                    assert!(direct(&route, b"inspect", false, repo, &alias, &capture).is_err());
                    assert!(capture.0.lock().unwrap().is_empty());
                }
                for provider in ["droid", "aider", "fixture"] {
                    let route = Route {
                        provider: provider.into(),
                        model: "provider-default".into(),
                        effort: String::new(),
                        explicit: false,
                        account: String::new(),
                        account_pinned: false,
                        public_research: false,
                    };
                    let capture = Capture::default();
                    std::thread::scope(|scope| {
                        for _ in 0..2 {
                            scope.spawn(|| {
                                direct(&route, b"inspect", true, repo, cwd, &capture).unwrap();
                            });
                        }
                    });
                    let calls = capture.0.lock().unwrap();
                    let dirs: BTreeSet<_> = calls
                        .iter()
                        .flat_map(|call| call.argv.windows(2))
                        .filter(|pair| {
                            ["-f", "--message-file", "--prompt-file"]
                                .iter()
                                .any(|flag| pair[0] == *flag)
                        })
                        .map(|pair| PathBuf::from(&pair[1]).parent().unwrap().to_owned())
                        .collect();
                    assert_eq!(dirs.len(), 2);
                    assert!(dirs.iter().all(|path| !path.exists()));
                }
            });
        }
    }
    #[test]
    fn native_prompt_limits_and_cleanup() {
        for mode in ["normal", "interactive", "inside"] {
            isolated_transport("native_prompt_limits_and_cleanup", mode, |repo, cwd| {
                for provider in [
                    "grok",
                    "opencode",
                    "deepseek",
                    "vibe",
                    "pi",
                    "copilot",
                    "muse",
                    "kimi",
                    "kiro",
                    "continue",
                    "goose",
                    "cline",
                    "kilo",
                    "openhands",
                    "auggie",
                ] {
                    let route = Route {
                        provider: provider.into(),
                        model: "provider-default".into(),
                        effort: String::new(),
                        explicit: false,
                        account: String::new(),
                        account_pinned: false,
                        public_research: false,
                    };
                    for prompt in [
                        vec![],
                        vec![b'x'; 120000],
                        vec![b'x'; 120001],
                        vec![255],
                        b"a\0b".to_vec(),
                    ] {
                        let capture = Capture::default();
                        let result = direct(&route, &prompt, true, repo, cwd, &capture);
                        let valid = prompt.len() <= 120000
                            && std::str::from_utf8(&prompt).is_ok()
                            && !prompt.contains(&0);
                        assert_eq!(
                            result.is_ok(),
                            valid,
                            "{provider}, {} bytes: {result:?}",
                            prompt.len()
                        );
                        assert_eq!(capture.0.lock().unwrap().len(), usize::from(valid));
                    }
                }
                let route = Route {
                    provider: "antigravity".into(),
                    model: "provider-default".into(),
                    effort: String::new(),
                    explicit: false,
                    account: String::new(),
                    account_pinned: false,
                    public_research: false,
                };
                for size in [0, 120000, 120001] {
                    let capture = Capture::default();
                    let result = direct(&route, &vec![b'x'; size], true, repo, cwd, &capture);
                    assert_eq!(
                        result.is_ok(),
                        mode != "interactive" || size <= 120000,
                        "{mode}, {size} bytes: {result:?}"
                    );
                    let calls = capture.0.lock().unwrap();
                    if let Some(call) = calls.first() {
                        let dirs: Vec<_> = call
                            .argv
                            .windows(2)
                            .filter(|p| p[0] == "--add-dir")
                            .map(|p| PathBuf::from(&p[1]))
                            .collect();
                        if size > 120000 {
                            assert_eq!(dirs.len(), 2);
                            assert!(!dirs[1].starts_with(cwd));
                            assert!(!dirs[1].exists());
                        } else {
                            assert_eq!(dirs, vec![cwd.to_owned()]);
                        }
                    }
                }
            });
        }
    }
    #[test]
    fn route_uses_policy_and_target() {
        isolated_transport("route_uses_policy_and_target", "normal", |_, _| {
            let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            let tier_agy = resolve_route(source, "agy", "", "", "reviewer").unwrap();
            assert_eq!(tier_agy.provider, "antigravity");
            // The route keeps the base model; only the CLI argument is the exact selector.
            assert_eq!(tier_agy.model, "gemini-4-carbon");
            assert_eq!(tier_agy.effort, "high");
            let command = transport(&tier_agy, b"inspect", source, false, 30).unwrap();
            assert!(
                command
                    .argv
                    .windows(2)
                    .any(|w| w[0] == "--model" && w[1] == "gemini-4-carbon-high")
            );
            assert!(!tier_agy.explicit);
            let tier_agy_override = resolve_route(source, "agy", "", "medium", "reviewer").unwrap();
            assert_eq!(tier_agy_override.model, "gemini-4-carbon");
            assert_eq!(tier_agy_override.effort, "medium");
            let route = resolve_route(source, "agy", "custom", "high", "worker").unwrap();
            assert_eq!(route.provider, "antigravity");
            assert_eq!(route.model, "custom");
            assert!(route.explicit);
            let default_agy = resolve_route(source, "agy", "", "high", "worker").unwrap();
            assert_eq!(default_agy.model, "provider-default");
            assert_eq!(default_agy.effort, "high");
            let automatic = resolve_route(source, "codex", "", "auto", "worker").unwrap();
            assert_eq!(automatic.model, "gpt-6.1-terra");
            assert_eq!(automatic.effort, "medium");
            assert!(!automatic.explicit);
            let claude_tier = resolve_route(source, "claude", "", "auto", "reviewer").unwrap();
            assert_eq!(claude_tier.model, "opus");
            assert_eq!(claude_tier.effort, "high");
            assert!(!claude_tier.explicit);
            for effort in ["xhigh", "max"] {
                let claude =
                    resolve_route(source, "claude", "claude-opus-5-5", effort, "worker").unwrap();
                assert_eq!(claude.effort, effort);
                assert_eq!(claude.model, "claude-opus-5-5");
                assert!(claude.explicit);
            }
            for model in crate::native_model::AGY_FIXED_EFFORT_MODELS {
                let fixed = resolve_route(source, "agy", model, "", "worker").unwrap();
                assert_eq!(fixed.model, model);
                assert!(fixed.effort.is_empty());
                let command = transport(&fixed, b"inspect", source, false, 30).unwrap();
                assert!(!command.argv.iter().any(|arg| arg == "--effort"));
                assert_eq!(command.selected_effort, None);
                assert!(resolve_route(source, "agy", model, "high", "worker").is_err());
                let seat = format!("agy:effort=auto:model={model}");
                let override_route = resolve_route(source, &seat, "", "high", "worker").unwrap();
                assert_eq!(override_route.model, model);
                assert!(override_route.effort.is_empty());
                assert!(resolve_route(source, &seat, "", "bad", "worker").is_err());
                assert!(resolve_route(source, &seat, "other", "high", "worker").is_err());
                let mut invalid = fixed;
                invalid.effort = "high".into();
                assert!(transport(&invalid, b"inspect", source, false, 30).is_err());
            }
            for model in ["gemini-4-neon-xhigh", "gemini-4-hydrogen-xhigh"] {
                let route = resolve_route(source, "agy", model, "xhigh", "worker").unwrap();
                assert_eq!(route.model, model);
                let command = transport(&route, b"inspect", source, false, 30).unwrap();
                assert_eq!(command.selected_effort.as_deref(), Some("xhigh"));
                assert!(resolve_route(source, "agy", model, "max", "worker").is_err());
            }
            assert_eq!(
                crate::native_model::effort_support(
                    "fixture-provider",
                    "unknown-model",
                    "high",
                    None,
                ),
                crate::native_model::EffortSupport::Unknown
            );
            let pinned = resolve_route(
                source,
                "codex:account=work",
                "gpt-6.2-sol",
                "ultra",
                "worker",
            )
            .unwrap();
            assert_eq!(pinned.effort, "ultra");
            assert_eq!(pinned.account, "work");
            assert!(pinned.account_pinned);
            assert!(resolve_route(source, "claude", "claude-opus-5-5", "ultra", "worker").is_err());
            assert!(resolve_route(source, "", "", "", "worker").is_err());
            assert!(resolve_route(source, "codex", "", "bad", "worker").is_err());

            // With a cached Antigravity catalog present, the derived selector must be listed.
            let cache_dir = home.join(".cache/oh-my-setting/capabilities");
            fs::create_dir_all(&cache_dir).unwrap();
            fs::write(
                cache_dir.join("antigravity.models"),
                "gemini-4-carbon-low\tGemini 4 Carbon (Low)\n\
                 gemini-4-carbon-high\tGemini 4 Carbon (High)\n\
                 gemini-pro-orchestrator\tOrchestrator (Carbon)\n",
            )
            .unwrap();
            let listed = resolve_route(source, "agy", "", "", "reviewer").unwrap();
            assert_eq!(listed.model, "gemini-4-carbon");
            let listed_explicit =
                resolve_route(source, "agy", "gemini-4-carbon-high", "high", "worker").unwrap();
            assert_eq!(listed_explicit.model, "gemini-4-carbon-high");
            let listed_fixed =
                resolve_route(source, "agy", "gemini-pro-orchestrator", "", "worker").unwrap();
            assert_eq!(listed_fixed.model, "gemini-pro-orchestrator");
            let missing_tier = resolve_route(source, "agy", "", "medium", "reviewer").unwrap_err();
            assert_eq!(missing_tier.message, "model_effort_unavailable");
            let missing_explicit =
                resolve_route(source, "agy", "gemini-4-carbon", "xhigh", "worker").unwrap_err();
            assert_eq!(missing_explicit.message, "model_effort_unavailable");
            let missing_suffixed =
                resolve_route(source, "agy", "gemini-4-neon-xhigh", "xhigh", "worker").unwrap_err();
            assert_eq!(missing_suffixed.message, "model_effort_unavailable");
            // Codex and Claude routes are unaffected by the Antigravity catalog.
            assert_eq!(
                resolve_route(source, "codex", "", "auto", "worker")
                    .unwrap()
                    .model,
                "gpt-6.1-terra"
            );
            assert_eq!(
                resolve_route(source, "claude", "", "auto", "reviewer")
                    .unwrap()
                    .model,
                "opus"
            );
        });
    }

    fn seat(id: &str, used: Option<f64>, running: usize) -> Account {
        Account {
            provider: "codex".into(),
            id: id.into(),
            home: PathBuf::from("$HOME/seat").join(id),
            used,
            running,
            usable: true,
            models: None,
        }
    }
    fn pick(
        accounts: &[Account],
        spread: bool,
        explicit: bool,
        health: &std::collections::BTreeMap<String, SeatHealth>,
        now: u64,
    ) -> Option<String> {
        select_account_adaptive(accounts, spread, explicit, "provider-default", health, now)
            .map(|a| a.id)
    }
    #[test]
    fn adaptive_ranking_orders_cooldown_penalty_quota_spread_then_registry() {
        let mut health = std::collections::BTreeMap::new();
        let accounts = [seat("a", Some(20.0), 0), seat("b", Some(20.0), 0)];
        // Equal everything: registry order decides, latency is not a tie-break.
        health.insert(
            "codex:a".into(),
            SeatHealth {
                latency_ms: 900,
                ..SeatHealth::default()
            },
        );
        health.insert(
            "codex:b".into(),
            SeatHealth {
                latency_ms: 10,
                ..SeatHealth::default()
            },
        );
        assert_eq!(pick(&accounts, true, false, &health, 1000).unwrap(), "a");
        // Cooldown outranks everything and expires at `now`.
        health.get_mut("codex:a").unwrap().cooldown_until = 2000;
        assert_eq!(pick(&accounts, true, false, &health, 1000).unwrap(), "b");
        assert_eq!(pick(&accounts, true, false, &health, 2000).unwrap(), "a");
        // Bare legacy keys are not seat keys.
        let mut bare = std::collections::BTreeMap::new();
        bare.insert(
            "a".into(),
            SeatHealth {
                cooldown_until: 9,
                ..SeatHealth::default()
            },
        );
        assert_eq!(pick(&accounts, true, false, &bare, 1).unwrap(), "a");
        // Penalty is bounded, decays, and ranks above quota.
        let mut decay = std::collections::BTreeMap::new();
        decay.insert(
            "codex:a".into(),
            SeatHealth {
                err: 50,
                last_error_ts: 100,
                ..SeatHealth::default()
            },
        );
        assert_eq!(pick(&accounts, true, false, &decay, 200).unwrap(), "b");
        assert_eq!(penalty(decay.get("codex:a"), 200), 3);
        assert_eq!(penalty(decay.get("codex:a"), 100 + 1801), 0);
        assert_eq!(pick(&accounts, true, false, &decay, 5000).unwrap(), "a");
        let none = std::collections::BTreeMap::new();
        // Quota buckets are 10 points wide; unknown quota ranks as the worst bucket.
        let quota = [
            seat("unknown", None, 0),
            seat("heavy", Some(89.0), 0),
            seat("light", Some(31.0), 0),
            seat("same-bucket", Some(35.0), 0),
        ];
        assert_eq!(pick(&quota, false, false, &none, 0).unwrap(), "light");
        let tied = [seat("x", Some(31.0), 0), seat("y", Some(39.0), 0)];
        assert_eq!(pick(&tied, false, false, &none, 0).unwrap(), "x");
        // Spread breaks equal buckets by running count; ordered/pinned ignores it.
        let busy = [seat("busy", Some(20.0), 3), seat("idle", Some(20.0), 1)];
        assert_eq!(pick(&busy, true, false, &none, 0).unwrap(), "idle");
        assert_eq!(pick(&busy, false, false, &none, 0).unwrap(), "busy");
        // Weekly quota gate at 90 is waived only by an explicit route.
        let full = [seat("full", Some(90.0), 0), seat("ok", Some(10.0), 0)];
        assert_eq!(pick(&full[..1], false, false, &none, 0), None);
        assert_eq!(pick(&full[..1], false, true, &none, 0).unwrap(), "full");
        assert_eq!(pick(&full, false, false, &none, 0).unwrap(), "ok");
        // Unusable and model-mismatched accounts are never eligible.
        let mut closed = seat("closed", Some(0.0), 0);
        closed.usable = false;
        assert_eq!(pick(&[closed], false, true, &none, 0), None);
        let mut cached = seat("cached", Some(0.0), 0);
        cached.models = Some(BTreeSet::from(["m-1".to_owned()]));
        assert!(
            select_account_adaptive(&[cached.clone()], false, false, "m-2", &none, 0).is_none()
        );
        assert!(select_account_adaptive(&[cached], false, false, "m-1", &none, 0).is_some());
        assert_eq!(pick(&[], false, false, &none, 0), None);
    }
    #[test]
    fn seat_keys_and_target_account_syntax() {
        assert_eq!(seat_key("codex", ""), "codex");
        assert_eq!(seat_key("claude", "work"), "claude:work");
        let t = Target::parse("codex:account=work:model=gpt-x:optional").unwrap();
        assert_eq!(
            (t.account.as_str(), t.model.as_str(), t.optional),
            ("work", "gpt-x", true)
        );
        let t = Target::parse("claude:model=m:1:account=a_1").unwrap();
        assert_eq!((t.account.as_str(), t.model.as_str()), ("a_1", "m:1"));
        let t = Target::parse("codex:model=namespace:effort=auto").unwrap();
        assert_eq!(t.model, "namespace:effort=auto");
        assert_eq!(t.effort, None);
        assert_eq!(
            Target::parse("codex:effort=medium:model=gpt-x")
                .unwrap()
                .effort,
            Some("medium")
        );
        assert!(Target::parse("codex:effort=bad:model=gpt-x").is_err());
        assert!(Target::parse("codex:effort=medium").is_err());
        let t = Target::parse("pi:account=x").unwrap();
        assert_eq!((t.provider.as_str(), t.model.as_str()), ("pi", ""));
        for bad in [
            "codex:account=",
            "codex:account=Bad",
            "codex:account=-x",
            "codex:account=a:account=b",
            "codex:bogus=1",
            "codex:model=",
            "codex:model=:model=x",
            "codex:model=x:model=y",
        ] {
            assert!(Target::parse(bad).is_err(), "{bad}");
        }
        assert!(Target::parse(&format!("codex:account={}", "a".repeat(65))).is_err());
        assert!(Target::parse(&format!("codex:account={}", "a".repeat(64))).is_ok());
        let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let pinned = resolve_route(source, "codex:account=w", "", "auto", "worker").unwrap();
        assert!(pinned.account_pinned && pinned.account == "w" && !pinned.explicit);
        let free = resolve_route(source, "codex", "", "auto", "worker").unwrap();
        assert!(!free.account_pinned && free.account.is_empty());
        let err =
            resolve_route(source, "antigravity:account=w", "m", "auto", "worker").unwrap_err();
        assert_eq!(err.message, UNPROVED);
    }
    #[test]
    fn account_home_revalidates_directory_without_persisted_generation() {
        isolated_transport(
            "account_home_revalidates_directory_without_persisted_generation",
            "custody",
            |_, home| {
                fs::set_permissions(home, fs::Permissions::from_mode(0o700)).unwrap();
                let mut proof = AccountHome::capture("codex", home).unwrap();
                proof.sha256 = None;
                fs::write(home.join("settings"), "ordinary update").unwrap();
                assert!(proof.current());
                let displaced = home.with_extension("displaced");
                fs::rename(home, &displaced).unwrap();
                fs::create_dir(home).unwrap();
                fs::set_permissions(home, fs::Permissions::from_mode(0o700)).unwrap();
                assert!(!proof.current());
                fs::remove_dir(home).unwrap();
                std::os::unix::fs::symlink(displaced, home).unwrap();
                assert!(AccountHome::capture("codex", home).is_err());
            },
        );
    }

    #[test]
    fn sandbox_rw_dirs_cover_every_isolated_account_home() {
        let root = std::env::temp_dir().join(format!("oms-rw-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut env = Vec::new();
        for (name, sub) in [
            ("CODEX_HOME", "c"),
            ("CLAUDE_CONFIG_DIR", "l"),
            ("PI_CODING_AGENT_DIR", "p"),
            ("CARGO_TARGET_DIR", "t"),
        ] {
            fs::create_dir_all(root.join(sub)).unwrap();
            env.push((OsString::from(name), root.join(sub).into_os_string()));
        }
        env.push(("CODEX_HOME_MISSING".into(), root.join("c").into_os_string()));
        env.push((
            "PI_CODING_AGENT_DIR".into(),
            root.join("absent").into_os_string(),
        ));
        env.push(("CODEX_HOME".into(), root.join("c").into_os_string()));
        let dirs = env_rw_dirs(&env);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            dirs,
            ["c", "l", "p", "t"].map(|s| root.join(s)).to_vec(),
            "unrelated, missing and duplicate entries are dropped"
        );
        assert!(env_rw_dirs(&[]).is_empty());
    }
    #[test]
    fn adaptive_routing_enforces_strict_sandbox_without_backend() {
        if let Some(mode) = std::env::var_os("SANDBOX_STRICT_PROBE") {
            let dir = std::env::temp_dir().join(format!("oms-sbx-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let init = Command::new("git")
                .args(["init", "-q"])
                .current_dir(&dir)
                .status()
                .unwrap();
            assert!(init.success());
            let route = Route {
                provider: "claude".into(),
                model: "provider-default".into(),
                effort: String::new(),
                explicit: false,
                account: String::new(),
                account_pinned: false,
                public_research: false,
            };
            let capture = Capture::default();
            if mode == "auto" {
                assert!(std::env::var_os("SSH_AUTH_SOCK").is_some());
                let res = direct(&route, b"probe", true, &dir, &dir, &capture).unwrap();
                let _ = fs::remove_dir_all(&dir);
                assert_eq!(res.code, 0, "{}", String::from_utf8_lossy(&res.stderr));
                let stderr = String::from_utf8_lossy(&res.stderr);
                assert!(stderr.contains("unsandboxed-fallback"), "{stderr}");
                assert!(stderr.contains("without OS isolation"), "{stderr}");
                return;
            }
            let err = direct(&route, b"probe", true, &dir, &dir, &capture).unwrap_err();
            let _ = fs::remove_dir_all(&dir);
            assert!(err.message.contains("sandbox backend"), "{}", err.message);
            return;
        }
        let probe_bin = std::env::temp_dir().join(format!("oms-sbx-bin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&probe_bin);
        fs::create_dir_all(&probe_bin).unwrap();
        let claude_stub = probe_bin.join("claude");
        fs::write(
            &claude_stub,
            "#!/bin/sh\nif [ -n \"${SSH_AUTH_SOCK+x}\" ]; then echo leaked >&2; exit 9; fi\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\"}\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&claude_stub, fs::Permissions::from_mode(0o755)).unwrap();
        let path_env = format!(
            "{}:{}",
            probe_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        for (probe, sandbox_env) in [("1", "strict"), ("auto", "auto")] {
            let out = Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .args([
                    "--exact",
                    "providers::bridge_route_tests::adaptive_routing_enforces_strict_sandbox_without_backend",
                ])
                .env("HOME", &probe_bin)
                .env("PATH", &path_env)
                .env("TMPDIR", std::env::temp_dir())
                .env("SSH_AUTH_SOCK", probe_bin.join("agent.sock"))
                .env("SANDBOX_STRICT_PROBE", probe)
                .env("OMS_WORKER_SANDBOX", sandbox_env)
                .env("OMS_BWRAP_BIN", "/nonexistent-oms-bwrap-bin")
                .env("OMS_SANDBOX_EXEC_BIN", "/nonexistent-oms-sandbox-exec")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "mode={sandbox_env}\n{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = fs::remove_dir_all(&probe_bin);
    }
}
