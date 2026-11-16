//! One source walk/hash pass; later checks use retained parents and statat.
use crate::filesystem::{Anchor, Stamp, check_stamp};
use crate::{Error, SourceFile, hex, limits};
use rustix::fs::{self, AtFlags, FileType};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::Metadata;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

struct Directory {
    anchor: Anchor,
    generation: Metadata,
    contents: bool,
}
pub(crate) struct Snapshot {
    directories: BTreeMap<PathBuf, Directory>,
    files: Vec<(PathBuf, OsString, Stamp)>,
    missing: Vec<(PathBuf, OsString)>,
}
impl Snapshot {
    pub fn capture(root: &Anchor, files: &[SourceFile<'_>]) -> Result<(String, Self), Error> {
        if files.is_empty() || files.len() > limits::MAX_FILES {
            return Err(Error::integrity(
                "invalid reviewed native source inventory size",
            ));
        }
        let mut expected = BTreeMap::new();
        let mut trees = BTreeSet::new();
        let mut crates = BTreeSet::new();
        let mut hash = Sha256::new();
        for (label, digest) in files {
            let path = Path::new(OsStr::from_bytes(label));
            if path.components().count() > limits::MAX_DEPTH
                || path.as_os_str().is_empty()
                || !path.components().all(|c| matches!(c, Component::Normal(_)))
                || !hex(digest.as_bytes(), 64)
                || expected.insert(path.to_owned(), *digest).is_some()
            {
                return Err(Error::integrity("invalid reviewed native source inventory"));
            }
            let parts: Vec<_> = path.iter().collect();
            if parts.len() >= 4 && parts[0] == "rust" && parts[1] == "crates" {
                let base: PathBuf = parts[..3].iter().collect();
                crates.insert(base.clone());
                trees.insert(base.join("src"));
            }
        }
        for (path, digest) in &expected {
            let label = path.as_os_str().as_bytes();
            hash.update((label.len() as u64).to_be_bytes());
            hash.update(label);
            hash.update(digest.as_bytes());
        }
        let mut snapshot = Self {
            directories: BTreeMap::new(),
            files: Vec::new(),
            missing: Vec::new(),
        };
        snapshot.directories.insert(
            PathBuf::new(),
            Directory {
                anchor: root.clone_anchor()?,
                generation: root.generation()?,
                contents: false,
            },
        );
        for name in limits::DATA_ROOTS {
            if snapshot.present(Path::new(name))? {
                trees.insert(PathBuf::from(name));
            }
        }
        for base in crates {
            let build = base.join("build.rs");
            if !expected.contains_key(&build) && snapshot.present(&build)? {
                return Err(Error::integrity(
                    "native source inventory differs from the reviewed build",
                ));
            }
        }
        for tree in trees {
            snapshot.walk(&tree, &mut expected)?;
        }
        // Manifests, workspace lock and toolchain are fixed leaves outside the
        // recursive trees. A changed dependency declaration fails its hash.
        for (path, digest) in expected {
            snapshot.capture_file(&path, digest)?;
        }
        Ok((format!("{:x}", hash.finalize()), snapshot))
    }
    fn directory(&mut self, path: &Path) -> Result<&Directory, Error> {
        if !self.directories.contains_key(path) {
            let parent = path
                .parent()
                .ok_or_else(|| Error::integrity("invalid native source directory"))?;
            let name = path
                .file_name()
                .ok_or_else(|| Error::integrity("invalid native source directory"))?;
            let anchor = self.directory(parent)?.anchor.child(name)?;
            let generation = anchor.generation()?;
            self.directories.insert(
                path.into(),
                Directory {
                    anchor,
                    generation,
                    contents: false,
                },
            );
        }
        Ok(self
            .directories
            .get(path)
            .expect("retained source directory"))
    }
    fn present(&mut self, path: &Path) -> Result<bool, Error> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::integrity("invalid native source path"))?;
        let name = path
            .file_name()
            .ok_or_else(|| Error::integrity("invalid native source path"))?;
        match fs::statat(
            self.directory(parent)?.anchor.last(),
            name,
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => {
                self.missing.push((parent.into(), name.into()));
                Ok(false)
            }
            Err(_) => Err(Error::integrity("cannot inspect native source entry")),
        }
    }
    fn walk(&mut self, path: &Path, expected: &mut BTreeMap<PathBuf, &str>) -> Result<(), Error> {
        if path.components().count() > limits::MAX_DEPTH
            || self.directories.len() >= limits::MAX_FILES
        {
            return Err(Error::integrity("native source inventory exceeds limit"));
        }
        let directory = self.directory(path)?;
        let entries = fs::Dir::read_from(directory.anchor.last())
            .map_err(|_| Error::integrity("cannot enumerate current native source"))?;
        self.directories
            .get_mut(path)
            .expect("retained source directory")
            .contents = true;
        for entry in entries {
            let entry =
                entry.map_err(|_| Error::integrity("cannot enumerate current native source"))?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            let child = path.join(name);
            match entry.file_type() {
                FileType::Directory => self.walk(&child, expected)?,
                FileType::RegularFile => {
                    let digest = expected.remove(&child).ok_or_else(|| {
                        Error::integrity("native source inventory differs from the reviewed build")
                    })?;
                    self.capture_file(&child, digest)?;
                }
                // Some NFS servers omit d_type. Opening the entry without
                // following links still establishes its actual type safely.
                FileType::Unknown => {
                    let stat = fs::statat(
                        self.directory(path)?.anchor.last(),
                        name,
                        AtFlags::SYMLINK_NOFOLLOW,
                    )
                    .map_err(|_| Error::integrity("cannot inspect native source entry"))?;
                    if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                        self.walk(&child, expected)?;
                    } else {
                        let digest = expected.remove(&child).ok_or_else(|| {
                            Error::integrity(
                                "native source inventory differs from the reviewed build",
                            )
                        })?;
                        self.capture_file(&child, digest)?;
                    }
                }
                _ => return Err(Error::integrity("linked or special native source")),
            }
        }
        Ok(())
    }
    fn capture_file(&mut self, path: &Path, expected: &str) -> Result<(), Error> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::integrity("invalid native source path"))?;
        let name = path
            .file_name()
            .ok_or_else(|| Error::integrity("invalid native source path"))?;
        let file = self.directory(parent)?.anchor.leaf(name, false)?;
        if file.digest_local(64 * 1024 * 1024)? != expected {
            return Err(Error::integrity(
                "native source content differs from the reviewed build",
            ));
        }
        self.files.push((parent.into(), name.into(), file.stamp()));
        Ok(())
    }
    pub fn check(&self, root: &Anchor) -> Result<(), Error> {
        root.check()?;
        for (path, directory) in &self.directories {
            // The physical authority root's ancestors are checked above.
            if !path.as_os_str().is_empty() {
                directory
                    .anchor
                    .check_generation(&directory.generation, directory.contents)?;
            }
        }
        for (parent, name, stamp) in &self.files {
            check_stamp(&self.directories[parent].anchor, name, stamp)?;
        }
        for (parent, name) in &self.missing {
            match fs::statat(
                self.directories[parent].anchor.last(),
                name,
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Err(rustix::io::Errno::NOENT) => {}
                _ => {
                    return Err(Error::integrity(
                        "native source inventory changed during validation",
                    ));
                }
            }
        }
        Ok(())
    }
}
