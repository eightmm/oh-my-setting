//! Shared native-first resolution for panel reentry and job supervision.
use std::path::{Path, PathBuf};

pub(crate) struct Entry {
    pub path: PathBuf,
    pub script: bool,
}

pub(crate) fn resolve(source: &Path) -> Option<Entry> {
    resolve_entry(source, false)
}

pub(crate) fn resolve_for_job(source: &Path) -> Option<Entry> {
    resolve_entry(source, true)
}

fn resolve_entry(source: &Path, direct: bool) -> Option<Entry> {
    if let Ok(current) = std::env::current_exe() {
        if current.file_name().is_some_and(|name| name == "oms") && is_executable(&current) {
            return Some(Entry {
                path: current,
                script: false,
            });
        }
        if let Some(sibling) = current.parent().map(|dir| dir.join("oms"))
            && is_executable(&sibling)
        {
            return Some(Entry {
                path: sibling,
                script: false,
            });
        }
    }
    let script = source.join("scripts/oms");
    if script.is_file() && (!direct || is_executable(&script)) {
        return Some(Entry {
            path: source.join("scripts").canonicalize().ok()?.join("oms"),
            script: true,
        });
    }
    find_on_path("oms").map(|path| Entry {
        path,
        script: false,
    })
}

pub(crate) fn job_override(path: &Path) -> bool {
    if path.components().count() == 1 && !path.is_absolute() {
        return find_on_path(path).is_some();
    }
    is_executable(path)
}

fn find_on_path(binary: impl AsRef<Path>) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                &dir
            };
            let candidate = dir.join(binary.as_ref());
            if !is_executable(&candidate) {
                return None;
            }
            // Preserve the leaf name for panel discovery and shell reentry.
            dir.canonicalize()
                .ok()
                .map(|parent| parent.join(binary.as_ref()))
        })
    })
}

// Check execute access for the calling process, including filesystem restrictions.
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}
