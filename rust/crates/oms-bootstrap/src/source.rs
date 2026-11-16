use crate::filesystem::{Anchor, Opened};
use crate::{Error, member};
use oms_context::Json;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryKind {
    Checkout,
    Symlink,
    CopyReceipt,
}

pub struct Discovery {
    pub(crate) root: Anchor,
    pub(crate) receipt: Option<Opened>,
    pub(crate) marker: Opened,
    pub kind: DiscoveryKind,
}
impl Discovery {
    /// Explicit private dispatch roots are already chosen by the trusted
    /// entry. Resolve their logical alias once, then protect descendants.
    pub fn from_source_root(path: &Path) -> Result<Self, Error> {
        let root = Anchor::open(path)?;
        let marker = checkout_marker(&root, true)?;
        let source = Self {
            root,
            receipt: None,
            marker,
            kind: DiscoveryKind::Checkout,
        };
        source.check()?;
        Ok(source)
    }
    pub fn source_root(&self) -> &Path {
        self.root.path()
    }
    pub fn scripts(&self) -> PathBuf {
        self.source_root().join("scripts")
    }
    pub fn check(&self) -> Result<(), Error> {
        self.root.check()?;
        self.marker.check()?;
        if let Some(receipt) = &self.receipt {
            receipt.check()?;
        }
        Ok(())
    }
}

/// Markers anchoring an OMS checkout, strongest first. `scripts/oms` is the
/// launcher a caller may already be executing, so adjacency discovery
/// (`launcher == false`) never accepts it as independent evidence.
const CHECKOUT_MARKERS: [&str; 3] = ["scripts/agent-task.sh", "rust/Cargo.toml", "scripts/oms"];

fn find_marker(root: &Anchor, launcher: bool) -> Result<Option<Opened>, Error> {
    for marker in CHECKOUT_MARKERS {
        if marker == "scripts/oms" && !launcher {
            continue;
        }
        if root.exists(Path::new(marker))? {
            return root.file(Path::new(marker), false).map(Some);
        }
    }
    Ok(None)
}
/// Legacy shell markers are transitional: a native checkout is anchored by
/// `rust/Cargo.toml` or the `scripts/oms` launcher alone.
fn checkout_marker(root: &Anchor, launcher: bool) -> Result<Opened, Error> {
    match find_marker(root, launcher)? {
        Some(marker) => Ok(marker),
        None => root.file(Path::new(CHECKOUT_MARKERS[0]), false),
    }
}

#[derive(Debug, Clone)]
pub struct ReceiptLocation {
    pub authority_root: PathBuf,
    pub relative: PathBuf,
}
fn nonempty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|v| !v.is_empty())
}
pub fn receipt_location() -> Result<ReceiptLocation, Error> {
    if let Some(path) = nonempty(std::env::var_os("OMS_INSTALL_RECEIPT")) {
        let path = PathBuf::from(path);
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| Error::discovery("invalid install receipt path"))?;
        return Ok(ReceiptLocation {
            authority_root: parent.into(),
            relative: name.into(),
        });
    }
    let config = if let Some(config) = nonempty(std::env::var_os("XDG_CONFIG_HOME")) {
        PathBuf::from(config)
    } else {
        PathBuf::from(
            nonempty(std::env::var_os("HOME"))
                .ok_or_else(|| Error::discovery("cannot locate install receipt without HOME"))?,
        )
        .join(".config")
    };
    Ok(ReceiptLocation {
        authority_root: config,
        relative: PathBuf::from("oh-my-setting/install.json"),
    })
}

fn entry_path(entry: &Path) -> Result<PathBuf, Error> {
    if entry.as_os_str().is_empty() {
        return Err(Error::discovery("empty dispatcher path"));
    }
    if entry.components().count() == 1 && !entry.is_absolute() {
        if let Some(paths) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&paths) {
                let candidate = directory.join(entry);
                if rustix::fs::access(&candidate, rustix::fs::Access::EXEC_OK).is_ok()
                    && candidate.is_file()
                {
                    return Ok(candidate);
                }
            }
        }
        return Err(Error::discovery("cannot locate dispatcher on PATH"));
    }
    Ok(entry.to_path_buf())
}

/// Resolve only the launcher chain. Every protected source descendant is
/// subsequently opened relative to its physical authority directory.
pub fn discover(entry: &Path, receipt: Option<&ReceiptLocation>) -> Result<Discovery, Error> {
    let original = entry_path(entry)?;
    let (resolved, linked) = resolve_launcher(&original)?;
    let parent = resolved
        .parent()
        .ok_or_else(|| Error::discovery("dispatcher has no directory"))?;
    let candidate = parent
        .parent()
        .ok_or_else(|| Error::discovery("dispatcher has no checkout parent"))?;
    // Only scripts/oms can claim a checkout through adjacency. An arbitrary
    // copied file beside an unrelated marker cannot select that root.
    if parent.file_name() == Some(OsStr::new("scripts"))
        && resolved.file_name() == Some(OsStr::new("oms"))
    {
        let source = Anchor::open(candidate)?;
        if source.exists(Path::new("scripts"))? {
            // The launcher itself is not independent evidence of a checkout.
            if let Some(marker) = find_marker(&source, false)? {
                let kind = if linked {
                    DiscoveryKind::Symlink
                } else {
                    DiscoveryKind::Checkout
                };
                return Ok(Discovery {
                    root: source,
                    receipt: None,
                    marker,
                    kind,
                });
            }
        }
    }
    let default;
    let location = match receipt {
        Some(v) => v,
        None => {
            default = receipt_location()?;
            &default
        }
    };
    let authority =
        Anchor::open(&location.authority_root).map_err(|_| unavailable(parent, location))?;
    let record = authority
        .file(&location.relative, false)
        .map_err(|_| unavailable(parent, location))?;
    let bytes = record
        .read(4 * 1024 * 1024)
        .map_err(|_| unavailable(parent, location))?;
    let row = oms_context::parse_json(&bytes).map_err(|_| unavailable(parent, location))?;
    if member(&row, b"schema") != Some(&Json::Number("2".into())) {
        return Err(unavailable(parent, location));
    }
    let Some(Json::String(raw)) = member(&row, b"source_root") else {
        return Err(unavailable(parent, location));
    };
    let selected = PathBuf::from(json_os_string(raw).map_err(|_| unavailable(parent, location))?);
    if !selected.is_absolute() {
        return Err(unavailable(parent, location));
    }
    let root = Anchor::open(&selected).map_err(|_| unavailable(parent, location))?;
    let marker = checkout_marker(&root, true).map_err(|_| unavailable(parent, location))?;
    let discovery = Discovery {
        root,
        receipt: Some(record),
        marker,
        kind: DiscoveryKind::CopyReceipt,
    };
    discovery.check()?;
    Ok(discovery)
}

fn resolve_launcher(original: &Path) -> Result<(PathBuf, bool), Error> {
    let mut path = original.to_path_buf();
    for depth in 0..=40 {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // Resolve logical root aliases, but never a protected scripts link.
        if parent.file_name() == Some(OsStr::new("scripts")) {
            let root = parent
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Anchor::open(root)?.descend(Path::new("scripts"))?;
        }
        let physical = std::fs::canonicalize(parent)
            .map_err(|_| Error::discovery("cannot resolve dispatcher directory"))?;
        if physical.file_name() == Some(OsStr::new("scripts"))
            && parent.file_name() != Some(OsStr::new("scripts"))
        {
            return Err(Error::integrity(
                "foreign protected scripts directory alias",
            ));
        }
        let name = path
            .file_name()
            .ok_or_else(|| Error::discovery("invalid dispatcher path"))?;
        let selected = physical.join(name);
        let metadata = std::fs::symlink_metadata(&selected)
            .map_err(|_| Error::discovery("cannot resolve dispatcher symlink chain"))?;
        if !metadata.file_type().is_symlink() {
            if !metadata.is_file() {
                return Err(Error::discovery("dispatcher is not a regular file"));
            }
            return Ok((selected, depth > 0));
        }
        if depth == 40 {
            break;
        }
        let target = std::fs::read_link(&selected)
            .map_err(|_| Error::discovery("cannot read dispatcher symlink"))?;
        path = if target.is_absolute() {
            target
        } else {
            physical.join(target)
        };
    }
    Err(Error::discovery("dispatcher symlink chain exceeds limit"))
}
fn unavailable(scripts: &Path, location: &ReceiptLocation) -> Error {
    let mut output = b"error: oms cannot locate its checkout from ".to_vec();
    output.extend_from_slice(scripts.as_os_str().as_bytes());
    output.extend_from_slice(
        b"\nerror: no OMS checkout markers there and no usable install receipt at ",
    );
    output.extend_from_slice(
        location
            .authority_root
            .join(&location.relative)
            .as_os_str()
            .as_bytes(),
    );
    output.extend_from_slice(b"\nerror: reinstall, or run oms from the checkout directly");
    Error::bytes(1, output)
}

/// Convert Python's surrogateescape paths without replacing invalid UTF-8.
fn json_os_string(bytes: &[u8]) -> Result<OsString, Error> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0 {
            return Err(Error::discovery("NUL in source path"));
        }
        if index + 2 < bytes.len()
            && bytes[index] == 0xed
            && matches!(bytes[index + 1], 0xb2 | 0xb3)
            && matches!(bytes[index + 2], 0x80..=0xbf)
        {
            output.push(((bytes[index + 1] - 0xb2) * 64) + bytes[index + 2]);
            index += 3;
        } else {
            let size = if bytes[index] < 0x80 {
                1
            } else if bytes[index] < 0xe0 {
                2
            } else if bytes[index] < 0xf0 {
                3
            } else {
                4
            };
            let end = index
                .checked_add(size)
                .filter(|&n| n <= bytes.len())
                .ok_or_else(|| Error::discovery("invalid source path encoding"))?;
            std::str::from_utf8(&bytes[index..end])
                .map_err(|_| Error::discovery("invalid source path encoding"))?;
            output.extend_from_slice(&bytes[index..end]);
            index = end;
        }
    }
    Ok(OsString::from_vec(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("oms-source-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        root
    }

    #[test]
    fn native_markers_anchor_explicit_roots() {
        for (name, marker) in [
            ("legacy", "scripts/agent-task.sh"),
            ("cargo", "rust/Cargo.toml"),
            ("launcher", "scripts/oms"),
        ] {
            let dir = root(name);
            std::fs::create_dir_all(dir.join("rust")).unwrap();
            std::fs::write(dir.join(marker), "").unwrap();
            assert!(Discovery::from_source_root(&dir).is_ok(), "{marker}");
            std::fs::remove_dir_all(dir).unwrap();
        }
        let dir = root("none");
        assert!(Discovery::from_source_root(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adjacency_ignores_the_launcher_as_its_own_marker() {
        let dir = root("adjacent");
        std::fs::write(dir.join("scripts/oms"), "").unwrap();
        let receipt = ReceiptLocation {
            authority_root: dir.clone(),
            relative: PathBuf::from("missing.json"),
        };
        assert!(discover(&dir.join("scripts/oms"), Some(&receipt)).is_err());
        std::fs::create_dir_all(dir.join("rust")).unwrap();
        std::fs::write(dir.join("rust/Cargo.toml"), "").unwrap();
        let found = discover(&dir.join("scripts/oms"), Some(&receipt)).unwrap();
        assert_eq!(found.kind, DiscoveryKind::Checkout);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
