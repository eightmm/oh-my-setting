use crate::Error;
use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn authority(meta: &Metadata, directory: bool, ancestor: bool) -> bool {
    let user = rustix::process::geteuid().as_raw();
    let owner = meta.uid() == user || meta.uid() == 0;
    let writable = meta.mode() & 0o022 != 0;
    owner
        && (!writable || (ancestor && directory && meta.uid() == 0 && meta.mode() & 0o1000 != 0))
        && meta.mode() & 0o6000 == 0
        && if directory {
            meta.is_dir()
        } else {
            meta.is_file() && meta.nlink() == 1
        }
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino() && a.uid() == b.uid() && a.mode() == b.mode()
}
fn stable(a: &Metadata, b: &Metadata) -> bool {
    same(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

struct Directory {
    name: OsString,
    file: File,
    initial: Metadata,
}
/// Holds every physical namespace component open, including the root anchor.
pub(crate) struct Anchor {
    path: PathBuf,
    chain: Vec<Directory>,
}
impl Anchor {
    /// Aliases are resolved only at an explicitly selected authority root.
    /// Descendant traversal uses openat without following links.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let path = std::fs::canonicalize(path)
            .map_err(|_| Error::integrity("cannot resolve authority directory"))?;
        let file = File::from(
            fs::open("/", directory_flags(), Mode::empty())
                .map_err(|_| Error::integrity("cannot open authority directory"))?,
        );
        let initial = file
            .metadata()
            .map_err(|_| Error::integrity("cannot inspect authority directory"))?;
        let mut anchor = Self {
            path: PathBuf::from("/"),
            chain: vec![Directory {
                name: OsString::from("/"),
                file,
                initial,
            }],
        };
        let names: Vec<_> = path
            .components()
            .filter_map(|c| {
                if let Component::Normal(v) = c {
                    Some(v.to_owned())
                } else {
                    None
                }
            })
            .collect();
        for (index, name) in names.iter().enumerate() {
            anchor.push(name, index + 1 != names.len())?;
        }
        anchor.check()?;
        Ok(anchor)
    }
    fn last(&self) -> &File {
        &self.chain.last().expect("anchor root").file
    }
    fn push(&mut self, name: &OsStr, ancestor: bool) -> Result<(), Error> {
        let file = File::from(
            fs::openat(self.last(), name, directory_flags(), Mode::empty())
                .map_err(|_| Error::integrity("foreign authority directory"))?,
        );
        let initial = file
            .metadata()
            .map_err(|_| Error::integrity("cannot inspect authority directory"))?;
        if !authority(&initial, true, ancestor) {
            return Err(Error::integrity(
                "unsafe authority directory ownership or permissions",
            ));
        }
        self.path.push(name);
        self.chain.push(Directory {
            name: name.to_owned(),
            file,
            initial,
        });
        Ok(())
    }
    fn clone_anchor(&self) -> Result<Self, Error> {
        let mut chain = Vec::new();
        for node in &self.chain {
            let file = node
                .file
                .try_clone()
                .map_err(|_| Error::integrity("cannot retain authority directory"))?;
            let initial = file
                .metadata()
                .map_err(|_| Error::integrity("cannot inspect authority directory"))?;
            if !same(&node.initial, &initial) {
                return Err(Error::integrity("authority directory changed"));
            }
            chain.push(Directory {
                name: node.name.clone(),
                file,
                initial,
            });
        }
        Ok(Self {
            path: self.path.clone(),
            chain,
        })
    }
    pub fn descend(&self, path: &Path) -> Result<Self, Error> {
        let mut anchor = self.clone_anchor()?;
        for component in path.components() {
            let Component::Normal(name) = component else {
                return Err(Error::integrity("invalid authority-relative path"));
            };
            anchor.push(name, false)?;
        }
        anchor.check()?;
        Ok(anchor)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn check(&self) -> Result<(), Error> {
        for (index, node) in self.chain.iter().enumerate() {
            let current = node
                .file
                .metadata()
                .map_err(|_| Error::integrity("authority directory unavailable"))?;
            if !same(&node.initial, &current) {
                return Err(Error::integrity("authority directory changed"));
            }
            if index > 0 {
                let stat = fs::statat(
                    &self.chain[index - 1].file,
                    &node.name,
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|_| Error::integrity("authority namespace unavailable"))?;
                if stat.st_dev as u64 != current.dev()
                    || stat.st_ino as u64 != current.ino()
                    || stat.st_mode as u32 != current.mode()
                    || stat.st_uid != current.uid()
                {
                    return Err(Error::integrity("authority namespace replaced"));
                }
            }
        }
        Ok(())
    }
    pub fn exists(&self, relative: &Path) -> Result<bool, Error> {
        let mut directory = self.clone_anchor()?;
        let parts: Vec<_> = relative.components().collect();
        if parts.is_empty() {
            return Err(Error::integrity("empty authority-relative path"));
        }
        for (index, part) in parts.iter().enumerate() {
            let Component::Normal(name) = part else {
                return Err(Error::integrity("invalid authority-relative path"));
            };
            directory.check()?;
            match fs::statat(directory.last(), *name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => {
                    if index + 1 == parts.len() {
                        return Ok(true);
                    }
                    directory.push(name, false)?;
                }
                Err(rustix::io::Errno::NOENT) => return Ok(false),
                Err(_) => return Err(Error::integrity("cannot inspect authority entry")),
            }
        }
        Ok(false)
    }
    pub fn file(&self, path: &Path, executable: bool) -> Result<Opened, Error> {
        let name = path
            .file_name()
            .ok_or_else(|| Error::integrity("invalid authority file path"))?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let dir = match parent {
            Some(path) => self.descend(path)?,
            None => self.clone_anchor()?,
        };
        let file = File::from(
            fs::openat(
                dir.last(),
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Error::integrity("authority file unavailable or linked"))?,
        );
        let initial = file
            .metadata()
            .map_err(|_| Error::integrity("cannot inspect authority file"))?;
        if !authority(&initial, false, false) || (executable && initial.mode() & 0o111 == 0) {
            return Err(Error::integrity(
                "unsafe authority file type, links, ownership or permissions",
            ));
        }
        let opened = Opened {
            dir,
            name: name.to_owned(),
            file,
            initial,
        };
        opened.check()?;
        Ok(opened)
    }
}

pub(crate) struct Opened {
    dir: Anchor,
    name: OsString,
    file: File,
    initial: Metadata,
}
impl Opened {
    pub fn path(&self) -> PathBuf {
        self.dir.path().join(&self.name)
    }
    pub fn identity_matches(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| same(&m, &self.initial))
    }
    pub fn check(&self) -> Result<(), Error> {
        self.dir.check()?;
        let current = self
            .file
            .metadata()
            .map_err(|_| Error::integrity("authority file unavailable"))?;
        let stat = fs::statat(self.dir.last(), &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::integrity("authority file namespace unavailable"))?;
        if !stable(&current, &self.initial)
            || stat.st_dev as u64 != current.dev()
            || stat.st_ino as u64 != current.ino()
            || stat.st_mode as u32 != current.mode()
            || stat.st_nlink as u64 != current.nlink()
            || stat.st_uid != current.uid()
        {
            return Err(Error::integrity(
                "authority file changed or namespace replaced",
            ));
        }
        Ok(())
    }
    pub fn read(&self, limit: u64) -> Result<Vec<u8>, Error> {
        if self.initial.len() > limit {
            return Err(Error::integrity("authority file exceeds read limit"));
        }
        let mut result = Vec::new();
        (&self.file)
            .take(limit + 1)
            .read_to_end(&mut result)
            .map_err(|_| Error::integrity("cannot read authority file"))?;
        self.check()?;
        if result.len() as u64 != self.initial.len() {
            return Err(Error::integrity("authority file size changed"));
        }
        Ok(result)
    }
    pub fn digest(&self, limit: u64) -> Result<String, Error> {
        use sha2::{Digest, Sha256};
        if self.initial.len() > limit {
            return Err(Error::integrity("authority file exceeds hash limit"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 32768];
        let mut reader = &self.file;
        let mut total = 0;
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|_| Error::integrity("cannot hash authority file"))?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > limit {
                return Err(Error::integrity("authority file exceeds hash limit"));
            }
            hash.update(&buffer[..count]);
        }
        self.check()?;
        if total != self.initial.len() {
            return Err(Error::integrity("authority file size changed"));
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

pub(crate) struct Stamp(Metadata);
impl Opened {
    pub fn stamp(&self) -> Stamp {
        Stamp(self.initial.clone())
    }
    pub fn matches_stamp(&self, stamp: &Stamp) -> bool {
        stable(&self.initial, &stamp.0)
    }
}
