#![forbid(unsafe_code)]

//! Descriptor-relative, bounded reads for Unix repositories.
//! This detects observed changes; it cannot provide an atomic filesystem snapshot.

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// A path-free failure suitable for mapping into a caller's diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPath,
    Missing,
    Unreadable,
    Unsafe,
    Oversize,
    Changed,
}

/// Constraints on the opened file. `None` accepts any owner.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub single_link: bool,
    pub owner: Option<u32>,
    /// Maximum lexical path components, or no additional limit.
    pub max_components: Option<usize>,
}
impl Policy {
    pub fn manifest() -> Self {
        Self {
            single_link: true,
            owner: Some(rustix::process::getuid().as_raw()),
            max_components: Some(64),
        }
    }
    pub fn source() -> Self {
        Self {
            single_link: false,
            owner: None,
            max_components: None,
        }
    }
}

fn map_open(e: rustix::io::Errno) -> Error {
    match e {
        rustix::io::Errno::NOENT => Error::Missing,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Error::Unsafe,
        _ => Error::Unreadable,
    }
}
fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn identity(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn stable(a: &Stat, b: &Stat) -> bool {
    identity(a, b)
        && a.st_mode == b.st_mode
        && a.st_size == b.st_size
        && a.st_uid == b.st_uid
        && a.st_nlink == b.st_nlink
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn valid_file(s: &Stat, p: Policy) -> bool {
    FileType::from_raw_mode(s.st_mode) == FileType::RegularFile
        && (!p.single_link || s.st_nlink == 1)
        && p.owner.is_none_or(|uid| uid == s.st_uid)
}
fn size_matches(stat: &Stat, count: usize) -> bool {
    u64::try_from(stat.st_size).ok() == u64::try_from(count).ok()
}

/// Holds a physical repository directory open across reads.
pub struct Root {
    path: PathBuf,
    fd: OwnedFd,
    initial: Stat,
}
impl Root {
    /// Resolves the repository path physically, as the legacy inspector does.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = std::fs::canonicalize(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Missing
            } else {
                Error::Unreadable
            }
        })?;
        let fd = fs::open(&path, directory_flags(), Mode::empty()).map_err(map_open)?;
        let initial = fs::fstat(&fd).map_err(|_| Error::Unreadable)?;
        Ok(Self { path, fd, initial })
    }

    /// Physical spelling captured when this root was opened; it may later be replaced.
    pub fn physical_root(&self) -> &Path {
        &self.path
    }

    /// Read an explicit manifest using pathlib-style lexical normalization.
    /// Absolute paths must be under this root's captured physical spelling.
    pub fn read_manifest(&self, path: &Path, limit: usize) -> Result<Vec<u8>, Error> {
        let raw = path.as_os_str().as_bytes();
        if raw.starts_with(b"//") && !raw.starts_with(b"///") {
            return Err(Error::InvalidPath);
        }
        if path.components().any(|part| part == Component::ParentDir) {
            return Err(Error::InvalidPath);
        }
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.path)
                .map_err(|_| Error::InvalidPath)?
        } else {
            path
        };
        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                _ => return Err(Error::InvalidPath),
            }
        }
        self.read_path(&normalized, limit, Policy::manifest())
    }

    /// Reads a slash-separated relative label without following links.
    pub fn read(&self, label: &str, limit: usize, policy: Policy) -> Result<Vec<u8>, Error> {
        self.read_os(OsStr::new(label), limit, policy)
    }

    /// Reads raw Unix path bytes, rejecting empty, absolute, dot and traversal components.
    pub fn read_os(&self, label: &OsStr, limit: usize, policy: Policy) -> Result<Vec<u8>, Error> {
        self.read_impl(label, limit, policy, || {})
    }

    /// Like `read_os`, without normalizing the supplied path.
    pub fn read_path(&self, label: &Path, limit: usize, policy: Policy) -> Result<Vec<u8>, Error> {
        self.read_os(label.as_os_str(), limit, policy)
    }

    fn read_impl(
        &self,
        label: impl AsRef<OsStr>,
        limit: usize,
        policy: Policy,
        hook: impl FnOnce(),
    ) -> Result<Vec<u8>, Error> {
        self.read_with_hooks(label, limit, policy, || {}, |_| hook())
    }

    fn read_with_hooks(
        &self,
        label: impl AsRef<OsStr>,
        limit: usize,
        policy: Policy,
        before_open: impl FnOnce(),
        hook: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, Error> {
        let parts: Vec<_> = label.as_ref().as_bytes().split(|b| *b == b'/').collect();
        if policy.max_components.is_some_and(|max| parts.len() > max)
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == b"." || *p == b".." || p.contains(&0))
        {
            return Err(Error::InvalidPath);
        }
        let mut dirs = Vec::new();
        for part in &parts[..parts.len() - 1] {
            let parent = dirs.last().unwrap_or(&self.fd);
            dirs.push(
                fs::openat(
                    parent,
                    OsStr::from_bytes(part),
                    directory_flags(),
                    Mode::empty(),
                )
                .map_err(map_open)?,
            );
        }
        let parent = dirs.last().unwrap_or(&self.fd);
        let name = OsStr::from_bytes(parts[parts.len() - 1]);
        let named = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_open)?;
        if !valid_file(&named, policy) {
            return Err(Error::Unsafe);
        }
        before_open();
        let fd = fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
            Mode::empty(),
        )
        .map_err(map_open)?;
        let before = fs::fstat(&fd).map_err(|_| Error::Unreadable)?;
        if !stable(&named, &before) {
            return Err(Error::Changed);
        }
        if !valid_file(&before, policy) {
            return Err(Error::Unsafe);
        }
        if before.st_size < 0 {
            return Err(Error::Unsafe);
        }
        if u64::try_from(before.st_size).map_err(|_| Error::Unsafe)? > limit as u64 {
            return Err(Error::Oversize);
        }
        let mut file = std::fs::File::from(fd);
        let mut bytes = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            let remaining = limit.saturating_sub(bytes.len());
            let count = buf.len().min(remaining.saturating_add(1));
            let n = match file.read(&mut buf[..count]) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Unreadable),
            };
            if n == 0 {
                break;
            }
            if n > remaining {
                return Err(Error::Oversize);
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        hook(&mut bytes);
        let after = fs::fstat(&file).map_err(|_| Error::Changed)?;
        let named_after =
            fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| Error::Changed)?;
        if !stable(&before, &after)
            || !size_matches(&after, bytes.len())
            || !identity(&after, &named_after)
            || !valid_file(&named_after, policy)
        {
            return Err(Error::Changed);
        }
        let root_now =
            fs::open(&self.path, directory_flags(), Mode::empty()).map_err(|_| Error::Changed)?;
        if !identity(
            &self.initial,
            &fs::fstat(root_now).map_err(|_| Error::Changed)?,
        ) {
            return Err(Error::Changed);
        }
        for (i, dir) in dirs.iter().enumerate() {
            let parent = if i == 0 { &self.fd } else { &dirs[i - 1] };
            let named = fs::statat(
                parent,
                OsStr::from_bytes(parts[i]),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| Error::Changed)?;
            let opened = fs::fstat(dir).map_err(|_| Error::Changed)?;
            if FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || !identity(&named, &opened)
            {
                return Err(Error::Changed);
            }
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "oms-safe-fs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn rejects_invalid_label() {
        let root = Root::open(std::env::temp_dir()).unwrap();
        for label in ["", ".", "..", "../file", "/file", "a//b", "a/./b", "x\0y"] {
            assert_eq!(
                root.read(label, 0, Policy::source()),
                Err(Error::InvalidPath)
            );
        }
        assert_eq!(
            root.read(&vec!["x"; 65].join("/"), 0, Policy::manifest()),
            Err(Error::InvalidPath)
        );
    }
    #[test]
    fn deep_sources_and_manifest_limit() {
        let f = Fixture::new();
        let mut dir = f.0.clone();
        let root = Root::open(&f.0).unwrap();
        for depth in 1..=69 {
            dir.push("d");
            std::fs::create_dir(&dir).unwrap();
            if [63, 64, 69].contains(&depth) {
                std::fs::write(dir.join("f"), b"x").unwrap();
                let mut parts = vec!["d"; depth];
                parts.push("f");
                let label = parts.join("/");
                assert_eq!(root.read(&label, 1, Policy::source()).unwrap(), b"x");
                let manifest = root.read(&label, 1, Policy::manifest());
                if depth == 63 {
                    assert_eq!(manifest.unwrap(), b"x");
                    assert_eq!(root.read_manifest(Path::new(&label), 1).unwrap(), b"x");
                } else {
                    assert_eq!(manifest, Err(Error::InvalidPath));
                    assert_eq!(
                        root.read_manifest(Path::new(&label), 1),
                        Err(Error::InvalidPath)
                    );
                }
            }
        }
    }
    #[test]
    fn raw_path_preserves_bytes_and_checks_lexical_components() {
        let f = Fixture::new();
        let name = OsString::from_vec(vec![b'f', 0xff]);
        let result = std::fs::write(f.0.join(&name), b"ok");
        #[cfg(target_os = "macos")]
        if result
            .as_ref()
            .is_err_and(|e| matches!(e.raw_os_error(), Some(22 | 92)))
        {
            return;
        }
        result.unwrap();
        let root = Root::open(&f.0).unwrap();
        assert_eq!(root.read_os(&name, 2, Policy::manifest()).unwrap(), b"ok");
        assert_eq!(root.read_manifest(Path::new(&name), 2).unwrap(), b"ok");
        assert_eq!(
            root.read_path(Path::new(&name), 2, Policy::source())
                .unwrap(),
            b"ok"
        );
        assert_eq!(
            root.read_os(&name, 1, Policy::manifest()),
            Err(Error::Oversize)
        );
        for raw in [
            b"".as_slice(),
            b"/f",
            b"./f",
            b"d//f",
            b"d/../f",
            b"f/",
            b"a\0b",
        ] {
            assert_eq!(
                root.read_path(Path::new(OsStr::from_bytes(raw)), 4, Policy::manifest()),
                Err(Error::InvalidPath)
            );
        }
        let link = OsString::from_vec(vec![b'l', 0xfe]);
        let result = symlink(&name, f.0.join(&link));
        #[cfg(target_os = "macos")]
        if result
            .as_ref()
            .is_err_and(|e| matches!(e.raw_os_error(), Some(22 | 92)))
        {
            return;
        }
        result.unwrap();
        assert_eq!(
            root.read_os(&link, 2, Policy::manifest()),
            Err(Error::Unsafe)
        );
        let mut policy = Policy::source();
        policy.max_components = Some(0);
        assert_eq!(root.read_os(&name, 2, policy), Err(Error::InvalidPath));
    }
    #[test]
    fn bounded_binary_reads_and_policies() {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), [0xff, 0]).unwrap();
        std::fs::write(f.0.join("empty"), []).unwrap();
        let root = Root::open(&f.0).unwrap();
        assert_eq!(root.read("file", 2, Policy::manifest()).unwrap(), [0xff, 0]);
        assert_eq!(root.read("file", 1, Policy::source()), Err(Error::Oversize));
        assert_eq!(root.read("file", 0, Policy::source()), Err(Error::Oversize));
        assert_eq!(root.read("empty", 0, Policy::source()).unwrap(), []);
        assert_eq!(
            root.read("missing", 1, Policy::source()),
            Err(Error::Missing)
        );
        assert_eq!(
            root.read(
                "file",
                2,
                Policy {
                    single_link: false,
                    owner: Some(rustix::process::getuid().as_raw().wrapping_add(1)),
                    max_components: None,
                }
            ),
            Err(Error::Unsafe)
        );
        std::fs::hard_link(f.0.join("file"), f.0.join("hard")).unwrap();
        assert_eq!(root.read("hard", 2, Policy::manifest()), Err(Error::Unsafe));
        assert_eq!(root.read("hard", 2, Policy::source()).unwrap(), [0xff, 0]);
        symlink("file", f.0.join("link")).unwrap();
        symlink(".", f.0.join("dirlink")).unwrap();
        assert_eq!(root.read("link", 2, Policy::source()), Err(Error::Unsafe));
        assert_eq!(
            root.read("dirlink/file", 2, Policy::source()),
            Err(Error::Unsafe)
        );
        std::fs::create_dir(f.0.join("dir")).unwrap();
        assert_eq!(root.read("dir", 2, Policy::source()), Err(Error::Unsafe));
        assert!(
            std::process::Command::new("mkfifo")
                .arg(f.0.join("fifo"))
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(root.read("fifo", 2, Policy::source()), Err(Error::Unsafe));
        assert_eq!(map_open(rustix::io::Errno::ACCESS), Error::Unreadable);
        let mut directory = f.0.clone();
        for _ in 0..63 {
            directory.push("d");
            std::fs::create_dir(&directory).unwrap();
        }
        std::fs::write(directory.join("f"), []).unwrap();
        let mut parts = vec!["d"; 63];
        parts.push("f");
        assert_eq!(
            root.read(&parts.join("/"), 0, Policy::source()).unwrap(),
            []
        );
    }
    #[test]
    fn detects_changes_and_replacements() {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), b"one").unwrap();
        let root = Root::open(&f.0).unwrap();
        assert_eq!(
            root.read_impl("file", 32, Policy::source(), || std::fs::write(
                f.0.join("file"),
                b"changed"
            )
            .unwrap()),
            Err(Error::Changed)
        );
        assert_eq!(
            root.read_impl("file", 32, Policy::source(), || {
                std::fs::rename(f.0.join("file"), f.0.join("old")).unwrap();
                std::fs::write(f.0.join("file"), b"new").unwrap();
            }),
            Err(Error::Changed)
        );
        std::fs::create_dir(f.0.join("dir")).unwrap();
        std::fs::write(f.0.join("dir/file"), b"x").unwrap();
        assert_eq!(
            root.read_impl("dir/file", 32, Policy::source(), || {
                std::fs::rename(f.0.join("dir"), f.0.join("old-dir")).unwrap();
                std::fs::create_dir(f.0.join("dir")).unwrap();
            }),
            Err(Error::Changed)
        );
    }

    #[test]
    fn rejects_socket_as_unsafe() {
        let f = Fixture::new();
        let socket = std::os::unix::net::UnixListener::bind(f.0.join("socket"));
        let _socket = match socket {
            Ok(socket) => socket,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(e) => panic!("socket fixture failed: {e}"),
        };
        let root = Root::open(&f.0).unwrap();
        assert_eq!(root.read("socket", 8, Policy::source()), Err(Error::Unsafe));
    }

    #[test]
    fn manifest_paths_and_root_identity() {
        let f = Fixture::new();
        let repo = f.0.join("repo");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(repo.join("dir")).unwrap();
        std::fs::write(repo.join("dir/file"), b"{}").unwrap();
        let root = Root::open(&repo).unwrap();
        assert_eq!(root.physical_root(), std::fs::canonicalize(&repo).unwrap());
        for path in [
            PathBuf::from("./dir//./file"),
            root.physical_root().join("dir/./file"),
        ] {
            assert_eq!(root.read_manifest(&path, 2).unwrap(), b"{}");
        }
        for path in [
            PathBuf::new(),
            PathBuf::from("."),
            PathBuf::from("dir/../dir/file"),
            f.0.join("other"),
            PathBuf::from("//invalid"),
        ] {
            assert_eq!(root.read_manifest(&path, 2), Err(Error::InvalidPath));
        }
        assert_eq!(
            root.read("./dir/file", 2, Policy::source()),
            Err(Error::InvalidPath)
        );
        assert_eq!(
            root.read_manifest(Path::new("dir/file"), 1),
            Err(Error::Oversize)
        );
        std::fs::rename(&repo, f.0.join("old")).unwrap();
        std::fs::create_dir(&repo).unwrap();
        assert_eq!(
            root.read_manifest(Path::new("dir/file"), 2),
            Err(Error::Changed)
        );
    }

    #[test]
    fn detects_preopen_replacement_and_size_mismatch() {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), b"one").unwrap();
        let root = Root::open(&f.0).unwrap();
        let fd = fs::openat(&root.fd, "file", OFlags::RDONLY, Mode::empty()).unwrap();
        let stat = fs::fstat(fd).unwrap();
        assert!(size_matches(&stat, 3));
        assert!(!size_matches(&stat, 0));
        assert!(!size_matches(&stat, 2));
        assert!(!size_matches(&stat, 4));
        assert_eq!(
            root.read_with_hooks("file", 8, Policy::source(), || {}, |body| body.clear()),
            Err(Error::Changed)
        );
        assert_eq!(
            root.read_with_hooks(
                "file",
                8,
                Policy::source(),
                || {
                    std::fs::rename(f.0.join("file"), f.0.join("old")).unwrap();
                    std::fs::write(f.0.join("file"), b"two").unwrap();
                },
                |_| {}
            ),
            Err(Error::Changed)
        );
    }
}
