use crate::Error;
use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn authority(meta: &Metadata, directory: bool, ancestor: bool) -> bool {
    let user = rustix::process::geteuid().as_raw();
    let owner = meta.uid() == user || meta.uid() == 0;
    let writable = meta.mode() & 0o002 != 0;
    owner
        && (!writable || (ancestor && directory && meta.uid() == 0 && meta.mode() & 0o1000 != 0))
        && meta.mode() & if directory { 0o4000 } else { 0o6000 } == 0
        && if directory {
            meta.is_dir()
        } else {
            meta.is_file() && meta.nlink() == 1
        }
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.mode() == b.mode()
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
    chain: Vec<Arc<Directory>>,
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
            chain: vec![Arc::new(Directory {
                name: OsString::from("/"),
                file,
                initial,
            })],
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
    pub(crate) fn last(&self) -> &File {
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
        self.chain.push(Arc::new(Directory {
            name: name.to_owned(),
            file,
            initial,
        }));
        Ok(())
    }
    pub fn clone_anchor(&self) -> Result<Self, Error> {
        Ok(Self {
            path: self.path.clone(),
            chain: self.chain.clone(),
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
                    || stat.st_gid != current.gid()
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
        let opened = dir.leaf(name, executable)?;
        opened.check()?;
        Ok(opened)
    }
    /// Source scanning has already retained and checked this parent. Its
    /// namespace is checked once for the entire source snapshot, not per leaf.
    pub fn leaf(&self, name: &OsStr, executable: bool) -> Result<Opened, Error> {
        if Path::new(name).components().count() != 1
            || !matches!(
                Path::new(name).components().next(),
                Some(Component::Normal(_))
            )
        {
            return Err(Error::integrity("invalid authority file path"));
        }
        let file = File::from(
            fs::openat(
                self.last(),
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
            dir: self.clone_anchor()?,
            name: name.to_owned(),
            file,
            initial,
        };
        Ok(opened)
    }
    pub fn child(&self, name: &OsStr) -> Result<Self, Error> {
        let mut child = self.clone_anchor()?;
        child.push(name, false)?;
        Ok(child)
    }
    pub fn generation(&self) -> Result<Metadata, Error> {
        self.last()
            .metadata()
            .map_err(|_| Error::integrity("authority directory unavailable"))
    }
    pub fn check_generation(&self, before: &Metadata, contents: bool) -> Result<(), Error> {
        let current = self.generation()?;
        let node = self.chain.last().expect("anchor root");
        let parent = &self.chain[self.chain.len().saturating_sub(2)];
        let named = fs::statat(&parent.file, &node.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::integrity("authority namespace unavailable"))?;
        if !(if contents {
            stable(before, &current)
        } else {
            same(before, &current)
        }) || !stat_matches(&named, &current)
        {
            return Err(Error::integrity(
                "native source directory changed during validation",
            ));
        }
        Ok(())
    }
}

pub(crate) struct Opened {
    dir: Anchor,
    name: OsString,
    file: File,
    initial: Metadata,
}
impl Opened {
    /// Build the Reference snapshot from this descriptor, never current_exe's
    /// mutable pathname. A separate launcher may validate a different image.
    pub fn controller(&self) -> Result<Option<oms_reference::controller::Controller>, Error> {
        if self.initial.len() > 1024 * 1024 * 1024 {
            return Err(Error::integrity("authority file exceeds hash limit"));
        }
        #[cfg(target_os = "linux")]
        {
            use oms_reference::controller::Controller;
            let identity = Arc::new(kernel_image::Image::open().map_err(Error::integrity)?);
            if !identity.matches(&self.file).map_err(Error::integrity)? {
                return Ok(None);
            }
            let image = self
                .file
                .try_clone()
                .map_err(|_| Error::integrity("cannot retain native executable"))?;
            Controller::from_authenticated_descriptor(image, identity)
                .map(Some)
                .map_err(Error::integrity)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Reference requires a genuine native descriptor proof on other
            // hosts; pathname equality cannot substitute for that provider.
            Ok(None)
        }
    }
    pub fn path(&self) -> PathBuf {
        self.dir.path().join(&self.name)
    }
    pub fn check(&self) -> Result<(), Error> {
        self.dir.check()?;
        self.check_local()
    }
    fn check_local(&self) -> Result<(), Error> {
        let current = self
            .file
            .metadata()
            .map_err(|_| Error::integrity("authority file unavailable"))?;
        let stat = fs::statat(self.dir.last(), &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::integrity("authority file namespace unavailable"))?;
        if !stable(&current, &self.initial) || !stat_matches(&stat, &current) {
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
        let mut reader = Positional {
            file: &self.file,
            offset: 0,
        };
        (&mut reader)
            .take(limit.saturating_add(1))
            .read_to_end(&mut result)
            .map_err(|_| Error::integrity("cannot read authority file"))?;
        self.check()?;
        if result.len() as u64 != self.initial.len() {
            return Err(Error::integrity("authority file size changed"));
        }
        Ok(result)
    }
    pub fn digest(&self, limit: u64) -> Result<String, Error> {
        let digest = self.digest_local(limit)?;
        self.dir.check()?;
        Ok(digest)
    }
    pub fn digest_local(&self, limit: u64) -> Result<String, Error> {
        use sha2::{Digest, Sha256};
        if self.initial.len() > limit {
            return Err(Error::integrity("authority file exceeds hash limit"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 32768];
        let mut reader = Positional {
            file: &self.file,
            offset: 0,
        };
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
        self.check_local()?;
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
}

pub(crate) fn check_stamp(directory: &Anchor, name: &OsStr, stamp: &Stamp) -> Result<(), Error> {
    let stat = fs::statat(directory.last(), name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| Error::integrity("native source unavailable during validation"))?;
    if !stat_matches(&stat, &stamp.0) {
        return Err(Error::integrity("native source changed during validation"));
    }
    Ok(())
}
fn stat_matches(stat: &fs::Stat, meta: &Metadata) -> bool {
    stat.st_dev == meta.dev()
        && stat.st_ino == meta.ino()
        && stat.st_mode == meta.mode()
        && stat.st_uid == meta.uid()
        && stat.st_gid == meta.gid()
        && stat.st_nlink == meta.nlink()
        && stat.st_size as u64 == meta.len()
        && stat.st_mtime == meta.mtime()
        && stat.st_mtime_nsec as i64 == meta.mtime_nsec()
        && stat.st_ctime == meta.ctime()
        && stat.st_ctime_nsec as i64 == meta.ctime_nsec()
}
struct Positional<'a> {
    file: &'a File,
    offset: u64,
}
impl Read for Positional<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read_at(buffer, self.offset)?;
        self.offset += count as u64;
        Ok(count)
    }
}

#[cfg(target_os = "linux")]
mod kernel_image {
    use super::*;
    use oms_reference::controller::ExecutingImage;

    pub(super) struct Image {
        proc_root: File,
        process_dir: File,
        process: u32,
    }
    impl Image {
        pub(super) fn open() -> Result<Self, String> {
            let proc_root: File = fs::open("/proc", directory_flags(), Mode::empty())
                .map_err(|e| e.to_string())?
                .into();
            if fs::fstatfs(&proc_root).map_err(|e| e.to_string())?.f_type != fs::PROC_SUPER_MAGIC {
                return Err("native executing-artifact procfs proof unavailable".into());
            }
            let process = std::process::id();
            let process_dir: File = fs::openat(
                &proc_root,
                process.to_string(),
                directory_flags(),
                Mode::empty(),
            )
            .map_err(|e| e.to_string())?
            .into();
            if fs::fstatfs(&process_dir).map_err(|e| e.to_string())?.f_type != fs::PROC_SUPER_MAGIC
            {
                return Err("native executing-artifact process directory is not procfs".into());
            }
            Ok(Self {
                proc_root,
                process_dir,
                process,
            })
        }
        pub(super) fn matches(&self, image: &File) -> Result<bool, String> {
            if self.process != std::process::id()
                || fs::readlinkat(&self.proc_root, "self", Vec::new())
                    .map_err(|e| e.to_string())?
                    .to_bytes()
                    != self.process.to_string().as_bytes()
            {
                return Err("native executing-artifact process identity changed".into());
            }
            let current: File = fs::openat(
                &self.process_dir,
                "exe",
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| e.to_string())?
            .into();
            Ok(stable(
                &image.metadata().map_err(|e| e.to_string())?,
                &current.metadata().map_err(|e| e.to_string())?,
            ))
        }
    }
    impl ExecutingImage for Image {
        fn verify_current(&self, image: &File) -> Result<(), String> {
            if self.matches(image)? {
                Ok(())
            } else {
                Err("retained descriptor is not the current native executing artifact".into())
            }
        }
    }
}
