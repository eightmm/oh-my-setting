//! Build-time definition of the native runtime source and policy closure.
use crate::limits::{DATA_ROOTS, MAX_DEPTH, MAX_FILES};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub struct Inventory {
    pub files: BTreeSet<PathBuf>,
    pub watches: BTreeSet<PathBuf>,
}
impl Inventory {
    fn add(&mut self, path: PathBuf) -> std::io::Result<()> {
        if self.files.len() >= MAX_FILES && !self.files.contains(&path) {
            return Err(std::io::Error::other(
                "native source inventory exceeds limit",
            ));
        }
        self.files.insert(path);
        Ok(())
    }
    fn walk(&mut self, root: &Path, relative: &Path) -> std::io::Result<()> {
        if relative.components().count() > MAX_DEPTH {
            return Err(std::io::Error::other(
                "native source inventory exceeds limit",
            ));
        }
        for entry in std::fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.walk(root, &path)?;
            } else if kind.is_file() {
                self.add(path)?;
            } else {
                return Err(std::io::Error::other("linked native source"));
            }
        }
        Ok(())
    }
}

pub fn paths(root: &Path) -> std::io::Result<Inventory> {
    let mut out = Inventory {
        files: BTreeSet::new(),
        watches: BTreeSet::new(),
    };
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        out.add(Path::new("rust").join(name))?;
    }
    // Standalone native service roots also contribute to the runtime closure.
    // Dependency lock files and target directories do not affect compilation.
    let mut pending: Vec<PathBuf> = [
        "oms",
        "oms-bootstrap",
        "oms-runtime",
        "oms-command-graph",
        "oms-command-peer",
        "oms-command-landing",
    ]
    .iter()
    .map(|name| Path::new("rust/crates").join(name))
    .collect();
    let mut seen = BTreeSet::new();
    while let Some(relative) = pending.pop() {
        if !seen.insert(relative.clone()) {
            continue;
        }
        let manifest = relative.join("Cargo.toml");
        let raw = std::fs::read_to_string(root.join(&manifest))?;
        let mut dependencies = false;
        for line in raw.lines().map(str::trim) {
            if line.starts_with('[') {
                if line.contains("dependencies.") {
                    return Err(std::io::Error::other("unsupported local dependency table"));
                }
                dependencies =
                    line.ends_with("dependencies]") && !line.ends_with("dev-dependencies]");
            } else if dependencies && line.contains("workspace") && !line.starts_with('#') {
                return Err(std::io::Error::other("unsupported inherited dependency"));
            } else if dependencies && line.contains("path") && !line.starts_with('#') {
                // Local manifests use inline double-quoted dependency paths.
                // Refuse unsupported syntax rather than silently losing closure.
                let (_, field) = line
                    .split_once("path")
                    .ok_or_else(|| std::io::Error::other("invalid local dependency"))?;
                let field = field
                    .trim_start()
                    .strip_prefix('=')
                    .map(str::trim_start)
                    .and_then(|v| v.strip_prefix('"'))
                    .ok_or_else(|| std::io::Error::other("unsupported local dependency path"))?;
                let (path, _) = field
                    .split_once('"')
                    .ok_or_else(|| std::io::Error::other("invalid local dependency path"))?;
                let name = Path::new(path)
                    .strip_prefix("..")
                    .ok()
                    .filter(|p| {
                        p.components().count() == 1
                            && matches!(p.components().next(), Some(Component::Normal(_)))
                    })
                    .ok_or_else(|| std::io::Error::other("foreign local dependency path"))?;
                pending.push(Path::new("rust/crates").join(name));
            }
        }
        out.add(manifest)?;
        let build = relative.join("build.rs");
        out.watches.insert(build.clone());
        match std::fs::symlink_metadata(root.join(&build)) {
            Ok(meta) if meta.is_file() => out.add(build)?,
            Ok(_) => return Err(std::io::Error::other("linked native build script")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let src = relative.join("src");
        out.watches.insert(src.clone());
        out.walk(root, &src)?;
    }
    for name in DATA_ROOTS {
        let path = Path::new(name);
        out.watches.insert(path.into());
        match std::fs::symlink_metadata(root.join(path)) {
            Ok(meta) if meta.is_dir() => out.walk(root, path)?,
            Ok(_) => return Err(std::io::Error::other("linked native runtime data")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    out.watches.extend(out.files.iter().cloned());
    Ok(out)
}
