//! Provider names and executable discovery shared by native front doors.
//! Custom discovery requires the OMS adapter filename marker. It does not grant
//! permissions, validate target grammar, or establish provider/model usability.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

pub const BUILTINS: &[(&str, &str)] = &[
    ("codex", "codex"),
    ("claude", "claude"),
    ("antigravity", "agy"),
    ("cursor", "cursor-agent"),
    ("grok", "grok"),
    ("gemini", "gemini"),
    ("qwen", "qwen"),
    ("opencode", "opencode"),
    ("deepseek", "dsh"),
    ("vibe", "vibe"),
    ("pi", "pi"),
    ("copilot", "copilot"),
    ("droid", "droid"),
    ("aider", "aider"),
    ("muse", "muse"),
    ("kimi", "kimi"),
    ("kiro", "kiro-cli"),
    ("continue", "cn"),
    ("goose", "goose"),
    ("cline", "cline"),
    ("kilo", "kilo"),
    ("openhands", "openhands"),
    ("auggie", "auggie"),
];

pub fn canonical(raw: &str) -> &str {
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

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.contains("..")
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

pub fn builtin_binary(provider: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find_map(|(name, binary)| (*name == provider).then_some(*binary))
}

fn executable(path: &Path) -> bool {
    path.is_file() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

pub struct Discovery {
    adapter_dir: PathBuf,
    search_dirs: Vec<(PathBuf, bool)>,
}

impl Discovery {
    /// Resolve inherited relative paths once against the calling owner, before
    /// launching a worker in another worktree. No process-global cwd is changed.
    pub fn from_env(owner_cwd: &Path) -> io::Result<Self> {
        let cwd = std::path::absolute(owner_cwd)?;
        let nonempty = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty());
        let adapter = nonempty("OMS_PROVIDER_ADAPTER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                nonempty("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(nonempty("HOME").unwrap_or_else(|| "/nonexistent".into()))
                            .join(".config")
                    })
                    .join("oh-my-setting/provider-adapters")
            });
        let search_dirs = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|path| {
                let relative = path.is_relative();
                let joined = cwd.join(&path);
                let resolved = std::path::absolute(&joined).unwrap_or(joined);
                (resolved, relative)
            })
            .collect();
        Ok(Self {
            adapter_dir: cwd.join(adapter),
            search_dirs,
        })
    }

    fn on_path(&self, name: &str) -> Option<(PathBuf, bool)> {
        let mut saw_relative = false;
        self.search_dirs.iter().find_map(|(directory, relative)| {
            saw_relative |= *relative;
            let candidate = directory.join(name);
            executable(&candidate).then_some((candidate, saw_relative))
        })
    }

    fn resolve_on_path(&self, name: &str) -> Option<PathBuf> {
        let (path, bind_path) = self.on_path(name)?;
        Some(if bind_path { path } else { name.into() })
    }

    pub fn normalize<'a>(&self, raw: &'a str) -> Option<&'a str> {
        let provider = canonical(raw);
        (valid_id(provider) && (builtin_binary(provider).is_some() || self.discovered(provider)))
            .then_some(provider)
    }

    /// Resolve an executable strictly for real process launch/probing.
    /// Returns `None` when absent in owner discovery so callers fail closed instead
    /// of re-resolving a bare executable name in a different target cwd.
    pub fn executable_for_launch(&self, raw: &str) -> Option<PathBuf> {
        let provider = canonical(raw);
        if !valid_id(provider) {
            return None;
        }
        if let Some(binary) = builtin_binary(provider) {
            if let Some(resolved) = self.resolve_on_path(binary) {
                return Some(resolved);
            }
            if provider == "opencode" {
                return self.resolve_on_path("opencode2");
            }
            return None;
        }
        let name = format!("oms-agent-adapter-{provider}");
        let direct = self.adapter_dir.join(&name);
        if executable(&direct) {
            return Some(direct);
        }
        self.resolve_on_path(&name)
    }

    /// Display/inventory binary name: prefers the owner-resolved executable and falls
    /// back to the canonical bare name only for read-only inventory output.
    pub fn binary(&self, raw: &str) -> Option<PathBuf> {
        let provider = canonical(raw);
        if !valid_id(provider) {
            return None;
        }
        if let Some(resolved) = self.executable_for_launch(provider) {
            return Some(resolved);
        }
        if let Some(binary) = builtin_binary(provider) {
            return Some(binary.into());
        }
        Some(format!("oms-agent-adapter-{provider}").into())
    }

    pub fn discovered(&self, provider: &str) -> bool {
        self.executable_for_launch(provider).is_some()
    }

    pub fn providers(&self) -> Vec<String> {
        let mut providers = BUILTINS
            .iter()
            .map(|(name, _)| (*name).into())
            .collect::<Vec<_>>();
        for directory in self
            .search_dirs
            .iter()
            .map(|(path, _)| path)
            .chain(std::iter::once(&self.adapter_dir))
        {
            if let Ok(entries) = std::fs::read_dir(directory) {
                for entry in entries.flatten().take(4096) {
                    let filename: OsString = entry.file_name();
                    if let Some(raw) = filename
                        .to_str()
                        .and_then(|name| name.strip_prefix("oms-agent-adapter-"))
                        && let Some(provider) = self.normalize(raw)
                        && !providers.iter().any(|known| known == provider)
                    {
                        providers.push(provider.to_owned());
                    }
                }
            }
        }
        providers
    }
}
