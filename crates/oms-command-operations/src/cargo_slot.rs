//! Temporary-worktree Cargo target isolation with bounded per-repository slot locks.
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub struct CargoSlot {
    _guard: oms_lock::Guard,
    target_dir: PathBuf,
}

#[must_use = "slot lock must outlive processes running in the temporary worktree"]
pub struct CargoTargetGuard {
    _slot: Option<CargoSlot>,
}

const CARGO_SLOTS: usize = 12;

pub fn is_cargo_repo(repo: &Path) -> bool {
    repo.join("Cargo.toml").is_file() || repo.join("rust/Cargo.toml").is_file()
}

pub fn cargo_slot_opted_out() -> bool {
    std::env::var_os("OMS_DELEGATE_ISOLATED_BUILD").as_deref() == Some(OsStr::new("1"))
        || std::env::var_os("OMS_CARGO_SLOT_CACHE").as_deref() == Some(OsStr::new("0"))
}

pub fn acquire_cargo_slot(repo: &Path, tree: &Path, provider: &str) -> Option<CargoSlot> {
    if !(is_cargo_repo(repo) || is_cargo_repo(tree)) || cargo_slot_opted_out() {
        return None;
    }
    let key = crate::digest(repo.to_string_lossy().as_bytes());
    let base = if provider == "codex" {
        std::env::temp_dir()
            .join(format!(
                "oms-cargo-slots-{}/cargo-slots",
                rustix::process::geteuid().as_raw()
            ))
            .join(&key[..16])
    } else {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".cache"))
            })?;
        cache_root
            .join("oh-my-setting/cargo-slots")
            .join(&key[..16])
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(&base).ok()?;
    let mut settings = oms_lock::Settings::from_env().ok()?;
    settings.cwd = repo.to_owned();
    for slot in 0..CARGO_SLOTS {
        let slot_dir = base.join(format!("slot-{slot}"));
        let _ = builder.create(&slot_dir);
        let lock = oms_lock::Lock::for_file(&slot_dir.join("slot.lock"), settings.clone()).ok()?;
        if let Ok(guard) = lock.try_acquire() {
            let target_dir = slot_dir.join("target");
            if builder.create(&target_dir).is_ok() {
                return Some(CargoSlot {
                    _guard: guard,
                    target_dir,
                });
            }
        }
    }
    None
}

/// Never inherit the caller's `CARGO_TARGET_DIR` into a temporary tree: older
/// source mtimes let later real-checkout builds reuse temp-tree binaries.
pub fn cargo_env(
    repo: &Path,
    tree: &Path,
    provider: &str,
    env: &mut Vec<(OsString, OsString)>,
) -> CargoTargetGuard {
    let caller_has_target = std::env::var_os("CARGO_TARGET_DIR").is_some_and(|v| !v.is_empty());
    let cargo_repo = is_cargo_repo(repo) || is_cargo_repo(tree);
    let opted_out = cargo_slot_opted_out();
    let slot = acquire_cargo_slot(repo, tree, provider);
    env.retain(|(k, _)| k != "CARGO_TARGET_DIR");
    if let Some(slot) = &slot {
        env.push((
            "CARGO_TARGET_DIR".into(),
            slot.target_dir.as_os_str().to_owned(),
        ));
    } else if caller_has_target || (cargo_repo && !opted_out) {
        env.push((
            "CARGO_TARGET_DIR".into(),
            tree.join("target").into_os_string(),
        ));
    }
    CargoTargetGuard { _slot: slot }
}
