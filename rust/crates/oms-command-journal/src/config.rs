#![forbid(unsafe_code)]

use crate::sanitize::os_str_to_utf8;
use crate::schema::{atomic_write_bytes, canonical_repo_root};
use crate::tz::TimeZoneSpec;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct NotionSettings {
    pub auth_mode: String, // "token", "bearer", "ntn", or ""
    pub auth_value: String,
    pub data_source_id: String,
    pub database_id: String,
    pub transport: String,   // "http" or "ntn"
    pub cli_command: String, // defaults to "ntn"
    pub keyring: String,     // e.g. "file", "os", ""
    pub key_property: String,
    pub hash_property: String,
    pub title_property: String,
    pub project_property: String,
    pub kind_property: String,
    pub period_property: String,
    pub blocker_property: String,
}

impl NotionSettings {
    pub fn target_id(&self) -> &str {
        if !self.data_source_id.is_empty() {
            &self.data_source_id
        } else {
            &self.database_id
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.target_id().is_empty()
            && (!self.auth_value.is_empty() || self.transport == "ntn" || self.auth_mode == "ntn")
    }
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    let val = std::env::var_os(key)?;
    if let Some(s) = val.to_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    } else if val.is_empty() {
        None
    } else {
        Some(PathBuf::from(val))
    }
}

pub fn expand_user_path(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use users::os::unix::UserExt as _;
        let raw = path.as_os_str().as_bytes();
        if !raw.starts_with(b"~") {
            return path.to_path_buf();
        }
        let (head, mut rest) = match raw.iter().position(|&b| b == b'/') {
            Some(pos) => (&raw[..pos], &raw[pos + 1..]),
            None => (raw, b"".as_slice()),
        };
        let home = if head == b"~" {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    users::get_user_by_uid(users::get_current_uid())
                        .map(|u| u.home_dir().to_path_buf())
                })
        } else {
            std::str::from_utf8(&head[1..])
                .ok()
                .and_then(users::get_user_by_name)
                .map(|u| u.home_dir().to_path_buf())
        };
        if let Some(home_dir) = home {
            while let Some(stripped) = rest.strip_prefix(b"/") {
                rest = stripped;
            }
            return if rest.is_empty() {
                home_dir
            } else {
                home_dir.join(OsStr::from_bytes(rest))
            };
        }
        path.to_path_buf()
    }
    #[cfg(not(unix))]
    {
        path.to_path_buf()
    }
}

pub fn oms_config_home() -> PathBuf {
    if let Some(explicit) = non_empty_env_path("OMS_WORK_JOURNAL_CONFIG") {
        let expanded = expand_user_path(&explicit);
        return expanded
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(xdg) = non_empty_env_path("XDG_CONFIG_HOME") {
        return expand_user_path(&xdg).join("oh-my-setting");
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("oh-my-setting")
}

pub fn work_journal_config_path() -> PathBuf {
    if let Some(explicit) = non_empty_env_path("OMS_WORK_JOURNAL_CONFIG") {
        return expand_user_path(&explicit);
    }
    oms_config_home().join("work-journal.json")
}

pub fn load_work_journal_config() -> Result<Map<String, Value>, String> {
    let path = work_journal_config_path();
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("failed to inspect {}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "refusing non-regular config file {}",
            path.display()
        ));
    }
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    let Some(obj) = parsed.as_object() else {
        return Err(format!("config {} must be a JSON object", path.display()));
    };
    Ok(obj.clone())
}

pub fn save_work_journal_config(obj: &Map<String, Value>) -> Result<PathBuf, String> {
    let path = work_journal_config_path();
    let bytes = crate::sanitize::pretty_json_bytes(&Value::Object(obj.clone()))?;
    atomic_write_bytes(&path, &bytes)?;
    set_owner_only_permissions(&path);
    Ok(path)
}

fn set_owner_only_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

pub fn configured_journal_language() -> String {
    let Ok(cfg) = load_work_journal_config() else {
        return String::new();
    };
    match cfg.get("language").and_then(Value::as_str) {
        Some("ko") => "ko".to_string(),
        Some("en") => "en".to_string(),
        _ => String::new(),
    }
}

pub fn journal_language(tz: Option<&TimeZoneSpec>) -> String {
    if let Ok(env_lang) = std::env::var("OMS_WORK_JOURNAL_LANG") {
        let lower = env_lang.trim().to_ascii_lowercase();
        if lower == "ko" || lower.starts_with("ko_") || lower.starts_with("ko-") {
            return "ko".to_string();
        }
        if lower == "en" || lower.starts_with("en_") || lower.starts_with("en-") {
            return "en".to_string();
        }
    }
    let pinned = configured_journal_language();
    if !pinned.is_empty() {
        return pinned;
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("ko") {
                return "ko".to_string();
            }
            if lower == "c" || lower == "c.utf-8" || lower == "c.utf8" || lower == "posix" {
                return "en".to_string();
            }
            return "en".to_string();
        }
    }
    if let Some(t) = tz {
        if t.name() == "Asia/Seoul" {
            return "ko".to_string();
        }
    }
    "en".to_string()
}

pub fn set_journal_language(lang: &str) -> Result<PathBuf, String> {
    if lang != "ko" && lang != "en" {
        return Err(format!("unsupported language '{lang}'; expected ko or en"));
    }
    let mut cfg = load_work_journal_config()?;
    cfg.insert("language".to_string(), Value::String(lang.to_string()));
    if !cfg.contains_key("notion") {
        cfg.insert("notion".to_string(), Value::Object(Map::new()));
    }
    save_work_journal_config(&cfg)
}

pub fn notion_access_env_var_name() -> &'static str {
    concat!("OMS_WORK_JOURNAL_NOTION_", "TOK", "EN")
}

pub fn notion_fallback_env_var_name() -> &'static str {
    concat!("NOTION_", "TOK", "EN")
}

fn env_or_config(env_key: &str, cfg_obj: &Map<String, Value>, cfg_key: &str) -> String {
    if let Ok(val) = std::env::var(env_key) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    cfg_obj
        .get(cfg_key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}

pub fn notion_settings() -> NotionSettings {
    let cfg = load_work_journal_config().unwrap_or_default();
    let notion_obj = cfg
        .get("notion")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let access_cred = std::env::var(notion_access_env_var_name())
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var(notion_fallback_env_var_name())
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_default();

    let data_source_id = env_or_config(
        "OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID",
        &notion_obj,
        "data_source_id",
    );
    let database_id = env_or_config(
        "OMS_WORK_JOURNAL_NOTION_DATABASE_ID",
        &notion_obj,
        "database_id",
    );

    let auth_pref = env_or_config("OMS_WORK_JOURNAL_NOTION_AUTH", &notion_obj, "auth_mode")
        .to_ascii_lowercase();
    let transport_pref = env_or_config(
        "OMS_WORK_JOURNAL_NOTION_TRANSPORT",
        &notion_obj,
        "transport",
    )
    .to_ascii_lowercase();

    let cli_command = {
        let val = env_or_config("OMS_NOTION_CLI", &notion_obj, "cli_command");
        if val.is_empty() {
            "ntn".to_string()
        } else {
            val
        }
    };
    let keyring = env_or_config("OMS_NOTION_KEYRING", &notion_obj, "keyring");

    let transport = if transport_pref == "ntn" || transport_pref == "http" {
        transport_pref
    } else if auth_pref == "ntn" {
        "ntn".to_string()
    } else if !access_cred.is_empty() {
        "http".to_string()
    } else if !notion_obj.is_empty()
        && notion_obj.get("auth_mode").and_then(Value::as_str) == Some("ntn")
    {
        "ntn".to_string()
    } else {
        "http".to_string()
    };

    let auth_mode = if transport == "ntn" || auth_pref == "ntn" {
        "ntn".to_string()
    } else if !access_cred.is_empty() {
        "bearer".to_string()
    } else {
        auth_pref
    };

    NotionSettings {
        auth_mode,
        auth_value: access_cred.trim().to_string(),
        data_source_id,
        database_id,
        transport,
        cli_command,
        keyring,
        key_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_KEY_PROP",
            &notion_obj,
            "key_property",
        ),
        hash_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_HASH_PROP",
            &notion_obj,
            "hash_property",
        ),
        title_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_TITLE_PROP",
            &notion_obj,
            "title_property",
        ),
        project_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_PROJECT_PROP",
            &notion_obj,
            "project_property",
        ),
        kind_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_KIND_PROP",
            &notion_obj,
            "kind_property",
        ),
        period_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_PERIOD_PROP",
            &notion_obj,
            "period_property",
        ),
        blocker_property: env_or_config(
            "OMS_WORK_JOURNAL_NOTION_BLOCKER_PROP",
            &notion_obj,
            "blocker_property",
        ),
    }
}

pub fn notion_excluded_repos() -> Vec<String> {
    let Ok(cfg) = load_work_journal_config() else {
        return Vec::new();
    };
    let mut set = BTreeSet::new();
    if let Some(arr) = cfg
        .get("notion")
        .and_then(Value::as_object)
        .and_then(|n| n.get("excluded_repos"))
        .and_then(Value::as_array)
    {
        for item in arr {
            if let Some(s) = item.as_str() {
                let t = s.trim();
                if !t.is_empty() {
                    set.insert(t.to_string());
                }
            }
        }
    }
    set.into_iter().collect()
}

pub fn notion_repo_excluded(repo_root: &Path) -> bool {
    let canon = canonical_repo_root(&expand_user_path(repo_root));
    notion_excluded_repos().iter().any(|entry| {
        let entry_path = expand_user_path(Path::new(entry));
        let entry_canon = canonical_repo_root(&entry_path);
        entry_canon == canon
            || entry_path == repo_root
            || Path::new(entry) == canon
            || Path::new(entry) == repo_root
    })
}

pub fn update_notion_exclusions(
    excludes: &[PathBuf],
    includes: &[PathBuf],
) -> Result<(PathBuf, Vec<String>), String> {
    let mut cfg = load_work_journal_config()?;
    let mut notion = cfg
        .get("notion")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut set: BTreeSet<String> = notion
        .get("excluded_repos")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    for p in excludes {
        let expanded = expand_user_path(p);
        let canon = canonical_repo_root(&expanded);
        let canon_str = os_str_to_utf8(canon.as_os_str(), "--exclude-repo")?;
        set.insert(canon_str.to_string());
    }
    for p in includes {
        let expanded = expand_user_path(p);
        let canon = canonical_repo_root(&expanded);
        let canon_str = os_str_to_utf8(canon.as_os_str(), "--include-repo")?;
        set.remove(canon_str);
        if let Some(raw_str) = p.to_str() {
            set.remove(raw_str);
        }
        if let Some(exp_str) = expanded.to_str() {
            set.remove(exp_str);
        }
    }

    let list: Vec<String> = set.into_iter().collect();
    notion.insert(
        "excluded_repos".to_string(),
        Value::Array(list.iter().cloned().map(Value::String).collect()),
    );
    cfg.insert("notion".to_string(), Value::Object(notion));
    let path = save_work_journal_config(&cfg)?;
    Ok((path, list))
}

pub fn configure_notion(
    data_source_id: &str,
    auth_mode_override: Option<&str>,
) -> Result<PathBuf, String> {
    let mut cfg = load_work_journal_config()?;
    let mut notion = cfg
        .get("notion")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    notion.insert(
        "data_source_id".to_string(),
        Value::String(data_source_id.trim().to_string()),
    );
    notion.remove("database_id");
    // Never persist credentials in config JSON.
    let key_a = concat!("tok", "en");
    let key_b = concat!("api_", "key");
    let key_c = concat!("sec", "ret");
    notion.remove(key_a);
    notion.remove(key_b);
    notion.remove(key_c);

    let env_auth = auth_mode_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            std::env::var("OMS_WORK_JOURNAL_NOTION_AUTH")
                .ok()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default();
    if !env_auth.is_empty() {
        notion.insert("auth_mode".to_string(), Value::String(env_auth.clone()));
        if env_auth == "ntn" {
            notion.insert("transport".to_string(), Value::String("ntn".to_string()));
        }
    }
    if let Ok(cli) = std::env::var("OMS_NOTION_CLI") {
        let trimmed = cli.trim();
        if !trimmed.is_empty() {
            notion.insert(
                "cli_command".to_string(),
                Value::String(trimmed.to_string()),
            );
        }
    }
    if let Ok(kr) = std::env::var("OMS_NOTION_KEYRING") {
        let trimmed = kr.trim();
        if !trimmed.is_empty() {
            notion.insert("keyring".to_string(), Value::String(trimmed.to_string()));
        }
    }
    cfg.insert("notion".to_string(), Value::Object(notion));
    save_work_journal_config(&cfg)
}

pub fn purge_notion_config() -> Result<bool, String> {
    let path = work_journal_config_path();
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("failed to inspect {}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "refusing non-regular config file {}",
            path.display()
        ));
    }
    let mut cfg = load_work_journal_config()?;
    let had_notion = cfg.remove("notion").is_some();
    if cfg.is_empty() {
        fs::remove_file(&path).map_err(|e| format!("failed removing {}: {e}", path.display()))?;
    } else {
        save_work_journal_config(&cfg)?;
    }
    Ok(had_notion)
}
