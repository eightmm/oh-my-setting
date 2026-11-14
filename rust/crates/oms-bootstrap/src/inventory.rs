//! Shared build/runtime definition of the native source closure.
use std::path::{Path, PathBuf};

pub fn paths(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn walk(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if relative.components().count() > 64 || out.len() > 8192 {
            return Err(std::io::Error::other(
                "native source inventory exceeds limit",
            ));
        }
        for entry in std::fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &path, out)?;
            } else if kind.is_file() {
                if out.len() >= 8192 {
                    return Err(std::io::Error::other(
                        "native source inventory exceeds limit",
                    ));
                }
                out.push(path);
            } else {
                return Err(std::io::Error::other("linked native source"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        out.push(Path::new("rust").join(name));
    }
    for entry in std::fs::read_dir(root.join("rust/crates"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Err(std::io::Error::other("invalid native crate directory"));
        }
        let relative = Path::new("rust/crates").join(entry.file_name());
        out.push(relative.join("Cargo.toml"));
        for name in ["Cargo.lock", "build.rs"] {
            let path = relative.join(name);
            match std::fs::symlink_metadata(root.join(&path)) {
                Ok(_) => out.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        walk(root, &relative.join("src"), &mut out)?;
    }
    if out.len() > 8192 {
        return Err(std::io::Error::other(
            "native source inventory exceeds limit",
        ));
    }
    out.sort();
    Ok(out)
}
