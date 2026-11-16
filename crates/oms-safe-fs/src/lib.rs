#![forbid(unsafe_code)]

//! Descriptor-relative, bounded byte storage for Unix repositories.
//! This detects observed changes; it cannot provide an atomic filesystem snapshot.

mod atomic_bytes;
pub use atomic_bytes::{
    ByteError, ByteReadOptions, MutationDisposition, MutationError, MutationOptions,
    MutationOutcome, MutationPhase,
};

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

/// Manifest-specific diagnostics captured during the descriptor-relative read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    External,
    Path,
    Open(Error),
    Policy,
    InitialOversize,
    Overflow,
    Changed,
}
impl From<Error> for ManifestError {
    fn from(error: Error) -> Self {
        match error {
            Error::InvalidPath => Self::Path,
            Error::Changed => Self::Changed,
            other => Self::Open(other),
        }
    }
}
impl ManifestError {
    fn coarse(self) -> Error {
        match self {
            Self::External | Self::Path => Error::InvalidPath,
            Self::Open(e) => e,
            Self::Policy => Error::Unsafe,
            Self::InitialOversize | Self::Overflow => Error::Oversize,
            Self::Changed => Error::Changed,
        }
    }
}

/// Path-free diagnostics for a stored output capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureReadError {
    InvalidPath,
    InvalidLimit,
    Missing,
    Unreadable,
    Unsafe,
    InitialOversize,
    ChangedBeforeRead,
    FileChanged,
    PathChanged,
    RootChanged,
    UnsafeOpen,
}

#[derive(Debug)]
enum ReadDetail {
    Manifest(ManifestError),
    Open(Error),
    PostIo(Error),
    ChangedBeforeRead,
    FileChanged,
    PathChanged,
    RootChanged,
}
impl From<ManifestError> for ReadDetail {
    fn from(value: ManifestError) -> Self {
        Self::Manifest(value)
    }
}
impl From<Error> for ReadDetail {
    fn from(value: Error) -> Self {
        Self::Manifest(value.into())
    }
}
impl ReadDetail {
    fn manifest(self) -> ManifestError {
        match self {
            Self::Manifest(e) => e,
            Self::Open(e) => ManifestError::Open(e),
            Self::PostIo(_)
            | Self::ChangedBeforeRead
            | Self::FileChanged
            | Self::PathChanged
            | Self::RootChanged => ManifestError::Changed,
        }
    }
    fn capture(self) -> CaptureReadError {
        match self {
            Self::FileChanged
            | Self::Manifest(ManifestError::Changed | ManifestError::Overflow) => {
                CaptureReadError::FileChanged
            }
            Self::ChangedBeforeRead => CaptureReadError::ChangedBeforeRead,
            Self::PathChanged => CaptureReadError::PathChanged,
            Self::RootChanged => CaptureReadError::RootChanged,
            Self::Open(Error::Unsafe) | Self::PostIo(Error::Unsafe) => CaptureReadError::UnsafeOpen,
            Self::Open(Error::Missing) | Self::PostIo(Error::Missing) => CaptureReadError::Missing,
            Self::Open(_) | Self::PostIo(_) => CaptureReadError::Unreadable,
            Self::Manifest(ManifestError::InitialOversize) => CaptureReadError::InitialOversize,
            Self::Manifest(ManifestError::Open(Error::Missing)) => CaptureReadError::Missing,
            Self::Manifest(ManifestError::Open(Error::Unreadable)) => CaptureReadError::Unreadable,
            Self::Manifest(
                ManifestError::External
                | ManifestError::Path
                | ManifestError::Open(Error::InvalidPath),
            ) => CaptureReadError::InvalidPath,
            Self::Manifest(_) => CaptureReadError::Unsafe,
        }
    }
}

fn valid_capture_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 22
        && b[0..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15..17] == *b"Z-"
        && b.ends_with(b".log")
        && b[17..b.len() - 4].iter().all(u8::is_ascii_digit)
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

/// Holds a physical repository directory open across byte operations.
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
        self.read_manifest_detailed(path, limit)
            .map_err(ManifestError::coarse)
    }

    /// Reads a manifest while retaining the cause needed for legacy diagnostics.
    pub fn read_manifest_detailed(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<u8>, ManifestError> {
        let raw = path.as_os_str().as_bytes();
        if raw.starts_with(b"//") && !raw.starts_with(b"///") {
            return Err(ManifestError::External);
        }
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.path)
                .map_err(|_| ManifestError::External)?
        } else {
            path
        };
        if relative
            .components()
            .any(|part| part == Component::ParentDir)
        {
            return Err(ManifestError::Path);
        }
        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                _ => return Err(ManifestError::Path),
            }
        }
        self.read_with_hooks_detailed(
            normalized.as_os_str(),
            limit,
            Policy::manifest(),
            || {},
            || {},
            |_| {},
        )
    }

    /// Reads a named capture beneath the fixed output directory. Observed changes
    /// fail closed; this does not provide an atomic snapshot against writers.
    /// Metadata stability and single-link rechecks intentionally reject more
    /// changes than the legacy capture reader.
    pub fn read_capture(&self, name: &str, max_bytes: usize) -> Result<Vec<u8>, CaptureReadError> {
        if !valid_capture_name(name) {
            return Err(CaptureReadError::InvalidPath);
        }
        if !(1..=64 * 1024 * 1024).contains(&max_bytes) {
            return Err(CaptureReadError::InvalidLimit);
        }
        self.read_capture_with_hooks(name, max_bytes, || {}, || {}, |_| {})
    }

    fn read_capture_with_hooks(
        &self,
        name: &str,
        limit: usize,
        before_open: impl FnOnce(),
        before_read: impl FnOnce(),
        hook: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, CaptureReadError> {
        let label = format!(".oms/artifacts/output-digest/{name}");
        self.read_detail(
            &label,
            limit,
            (
                Policy {
                    single_link: true,
                    owner: None,
                    max_components: None,
                },
                true,
            ),
            before_open,
            before_read,
            hook,
        )
        .map_err(ReadDetail::capture)
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
        self.read_with_hooks(label, limit, policy, || {}, || {}, |_| hook())
    }

    fn read_with_hooks(
        &self,
        label: impl AsRef<OsStr>,
        limit: usize,
        policy: Policy,
        before_open: impl FnOnce(),
        before_read: impl FnOnce(),
        hook: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, Error> {
        self.read_with_hooks_detailed(label, limit, policy, before_open, before_read, hook)
            .map_err(ManifestError::coarse)
    }

    fn read_with_hooks_detailed(
        &self,
        label: impl AsRef<OsStr>,
        limit: usize,
        policy: Policy,
        before_open: impl FnOnce(),
        before_read: impl FnOnce(),
        hook: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, ManifestError> {
        self.read_detail(
            label,
            limit,
            (policy, false),
            before_open,
            before_read,
            hook,
        )
        .map_err(ReadDetail::manifest)
    }

    fn read_detail(
        &self,
        label: impl AsRef<OsStr>,
        limit: usize,
        options: (Policy, bool),
        before_open: impl FnOnce(),
        before_read: impl FnOnce(),
        hook: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, ReadDetail> {
        let (policy, capture) = options;
        let parts: Vec<_> = label.as_ref().as_bytes().split(|b| *b == b'/').collect();
        if policy.max_components.is_some_and(|max| parts.len() > max)
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == b"." || *p == b".." || p.contains(&0))
        {
            return Err(ManifestError::Path.into());
        }
        let mut dirs = Vec::new();
        for part in &parts[..parts.len() - 1] {
            let parent = dirs.last().unwrap_or(&self.fd);
            let component = OsStr::from_bytes(part);
            let named_dir = if capture {
                let stat = fs::statat(parent, component, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|e| ReadDetail::Open(map_open(e)))?;
                if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                    return Err(ManifestError::Policy.into());
                }
                Some(stat)
            } else {
                None
            };
            let dir = fs::openat(parent, component, directory_flags(), Mode::empty())
                .map_err(|e| ReadDetail::Open(map_open(e)))?;
            if let Some(named_dir) = named_dir {
                let opened = fs::fstat(&dir).map_err(|_| Error::Unreadable)?;
                if !identity(&named_dir, &opened) {
                    return Err(ReadDetail::ChangedBeforeRead);
                }
            }
            dirs.push(dir);
        }
        let parent = dirs.last().unwrap_or(&self.fd);
        let name = OsStr::from_bytes(parts[parts.len() - 1]);
        let named = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_open)?;
        if !valid_file(&named, policy) {
            return Err(
                if FileType::from_raw_mode(named.st_mode) == FileType::Symlink {
                    ManifestError::Open(Error::Unsafe)
                } else {
                    ManifestError::Policy
                }
                .into(),
            );
        }
        if capture && named.st_size >= 0 && named.st_size as u64 > limit as u64 {
            return Err(ManifestError::InitialOversize.into());
        }
        before_open();
        let fd = fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
            Mode::empty(),
        )
        .map_err(|e| ReadDetail::Open(map_open(e)))?;
        let before = fs::fstat(&fd).map_err(|_| Error::Unreadable)?;
        if !stable(&named, &before) {
            return Err(ReadDetail::ChangedBeforeRead);
        }
        if !valid_file(&before, policy) {
            return Err(ManifestError::Policy.into());
        }
        if before.st_size < 0 {
            return Err(ManifestError::Policy.into());
        }
        if u64::try_from(before.st_size).map_err(|_| Error::Unsafe)? > limit as u64 {
            return Err(ManifestError::InitialOversize.into());
        }
        before_read();
        let mut file = std::fs::File::from(fd);
        let mut bytes = Vec::new();
        let mut buf = [0u8; 65536];
        let mut overflow = false;
        loop {
            let remaining = limit.saturating_sub(bytes.len());
            let count = buf.len().min(remaining.saturating_add(1));
            let n = match file.read(&mut buf[..count]) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(ManifestError::Open(Error::Unreadable).into()),
            };
            if n == 0 {
                break;
            }
            if n > remaining {
                overflow = true;
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        hook(&mut bytes);
        let after = fs::fstat(&file).map_err(|e| ReadDetail::PostIo(map_open(e)))?;
        if !stable(&before, &after) || (!overflow && !size_matches(&after, bytes.len())) {
            return Err(ReadDetail::FileChanged);
        }
        if capture && overflow {
            return Err(ReadDetail::FileChanged);
        }
        for (i, dir) in dirs.iter().enumerate() {
            let parent = if i == 0 { &self.fd } else { &dirs[i - 1] };
            let named = fs::statat(
                parent,
                OsStr::from_bytes(parts[i]),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|e| ReadDetail::PostIo(map_open(e)))?;
            let opened = fs::fstat(dir).map_err(|e| ReadDetail::PostIo(map_open(e)))?;
            if FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || !identity(&named, &opened)
            {
                return Err(ReadDetail::PathChanged);
            }
        }
        let named_after = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| ReadDetail::PostIo(map_open(e)))?;
        if !identity(&after, &named_after) {
            return Err(ReadDetail::PathChanged);
        }
        if !valid_file(&named_after, policy) {
            return Err(ReadDetail::PathChanged);
        }
        let root_now = fs::open(&self.path, directory_flags(), Mode::empty())
            .map_err(|e| ReadDetail::PostIo(map_open(e)))?;
        if !identity(
            &self.initial,
            &fs::fstat(root_now).map_err(|e| ReadDetail::PostIo(map_open(e)))?,
        ) {
            return Err(ReadDetail::RootChanged);
        }
        if overflow {
            return Err(ManifestError::Overflow.into());
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
    pub(super) struct Fixture(pub(super) PathBuf);
    impl Fixture {
        pub(super) fn new() -> Self {
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
    fn manifest_limit_and_permission_boundary() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let path = f.0.join("manifest");
        let limit = 4 * 1024 * 1024;
        let root = Root::open(&f.0).unwrap();
        std::fs::write(&path, []).unwrap();
        assert_eq!(
            root.read_manifest_detailed(Path::new("manifest"), 0),
            Ok(vec![])
        );
        std::fs::write(&path, vec![b' '; limit]).unwrap();
        assert_eq!(
            root.read_manifest_detailed(Path::new("manifest"), limit)
                .unwrap()
                .len(),
            limit
        );
        assert_eq!(
            root.read_manifest_detailed(Path::new("manifest"), limit - 1),
            Err(ManifestError::InitialOversize)
        );
        if rustix::process::getuid().as_raw() != 0 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let result = root.read_manifest_detailed(Path::new("manifest"), limit);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(result, Err(ManifestError::Open(Error::Unreadable)));
        }
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
    fn changed_file_takes_precedence_over_read_overflow() {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), b"one").unwrap();
        let root = Root::open(&f.0).unwrap();
        let result = root.read_with_hooks(
            "file",
            3,
            Policy::manifest(),
            || {},
            || {
                std::fs::write(f.0.join("file"), b"longer").unwrap();
            },
            |_| {},
        );
        assert_eq!(result, Err(Error::Changed));
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
        symlink("dir/file", repo.join("link")).unwrap();
        std::fs::hard_link(repo.join("dir/file"), repo.join("hard")).unwrap();
        assert_eq!(
            root.read_manifest_detailed(Path::new("link"), 2),
            Err(ManifestError::Open(Error::Unsafe))
        );
        assert_eq!(
            root.read_manifest_detailed(Path::new("hard"), 2),
            Err(ManifestError::Policy)
        );
        assert_eq!(
            root.read_manifest_detailed(Path::new("dir"), 2),
            Err(ManifestError::Policy)
        );
        assert_eq!(
            root.read_manifest_detailed(Path::new("dir/file"), 1),
            Err(ManifestError::Policy)
        );
        std::fs::remove_file(repo.join("hard")).unwrap();
        assert_eq!(
            root.read_manifest_detailed(Path::new("dir/file"), 1),
            Err(ManifestError::InitialOversize)
        );
        assert_eq!(
            root.read_manifest_detailed(&f.0.join("outside"), 2),
            Err(ManifestError::External)
        );
        assert_eq!(
            root.read_manifest_detailed(Path::new("dir/../file"), 2),
            Err(ManifestError::Path)
        );

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
            root.read_with_hooks(
                "file",
                8,
                Policy::source(),
                || {},
                || {},
                |body| body.clear()
            ),
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
                || {},
                |_| {}
            ),
            Err(Error::Changed)
        );
    }
    #[test]
    fn capture_public_validation_and_policy() {
        let f = Fixture::new();
        let dir = f.0.join(".oms/artifacts/output-digest");
        std::fs::create_dir_all(&dir).unwrap();
        let name = "20260101T123456Z-1.log";
        let path = dir.join(name);
        std::fs::write(&path, b"body").unwrap();
        let root = Root::open(&f.0).unwrap();
        assert_eq!(root.read_capture(name, 4).unwrap(), b"body");
        assert_eq!(root.read_capture(name, 64 * 1024 * 1024).unwrap(), b"body");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        assert_eq!(
            root.read_capture(name, 3),
            Err(CaptureReadError::InitialOversize)
        );
        if rustix::process::getuid().as_raw() != 0 {
            assert_eq!(
                root.read_capture(name, 4),
                Err(CaptureReadError::Unreadable)
            );
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            root.read_capture(name, 3),
            Err(CaptureReadError::InitialOversize)
        );
        assert_eq!(
            root.read_capture(name, 0),
            Err(CaptureReadError::InvalidLimit)
        );
        assert_eq!(
            root.read_capture(name, 64 * 1024 * 1024 + 1),
            Err(CaptureReadError::InvalidLimit)
        );
        for bad in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "x\0y",
            "20260101T123456Z-.log",
        ] {
            assert_eq!(
                root.read_capture(bad, 0),
                Err(CaptureReadError::InvalidPath)
            );
        }
        assert_eq!(
            root.read_capture("20260101T123456Z-2.log", 1),
            Err(CaptureReadError::Missing)
        );
        std::fs::hard_link(&path, dir.join("hard")).unwrap();
        assert_eq!(root.read_capture(name, 4), Err(CaptureReadError::Unsafe));
        std::fs::remove_file(dir.join("hard")).unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink("missing", &path).unwrap();
        assert_eq!(root.read_capture(name, 4), Err(CaptureReadError::Unsafe));
    }

    #[test]
    fn capture_change_stages_and_special_files() {
        let f = Fixture::new();
        let repo = f.0.join("repo");
        let dir = repo.join(".oms/artifacts/output-digest");
        std::fs::create_dir_all(&dir).unwrap();
        let name = "20260101T123456Z-1.log";
        let path = dir.join(name);
        std::fs::write(&path, b"one").unwrap();
        let root = Root::open(&repo).unwrap();
        assert_eq!(
            root.read_capture_with_hooks(
                name,
                3,
                || {},
                || std::fs::write(&path, b"longer").unwrap(),
                |_| {}
            ),
            Err(CaptureReadError::FileChanged)
        );
        std::fs::write(&path, b"one").unwrap();
        assert_eq!(
            root.read_capture_with_hooks(name, 3, || {}, || {}, |body: &mut Vec<u8>| body.clear()),
            Err(CaptureReadError::FileChanged)
        );
        assert_eq!(
            root.read_capture_with_hooks(
                name,
                3,
                || {
                    std::fs::rename(&path, dir.join("old")).unwrap();
                    std::fs::write(&path, b"two").unwrap();
                },
                || {},
                |_| {}
            ),
            Err(CaptureReadError::ChangedBeforeRead)
        );
        use std::os::unix::fs::MetadataExt;
        let observed_file = std::fs::File::open(&path).unwrap();
        let before_rename = std::cell::Cell::new(None);
        let source_metadata = |stat: std::fs::Metadata| {
            (
                stat.size(),
                stat.nlink(),
                stat.mtime(),
                stat.mtime_nsec(),
                stat.ctime(),
                stat.ctime_nsec(),
            )
        };
        let result = root.read_capture_with_hooks(
            name,
            3,
            || {},
            || before_rename.set(Some(source_metadata(observed_file.metadata().unwrap()))),
            |_| {
                std::fs::rename(&path, dir.join("old2")).unwrap();
                std::fs::write(&path, b"two").unwrap();
            },
        );
        let changed =
            before_rename.get().unwrap() != source_metadata(observed_file.metadata().unwrap());
        assert_eq!(
            result,
            Err(if changed {
                CaptureReadError::FileChanged
            } else {
                CaptureReadError::PathChanged
            })
        );
        use std::os::unix::fs::PermissionsExt;
        let result = root.read_capture_with_hooks(
            name,
            3,
            || {},
            || {},
            |_| {
                std::fs::rename(&dir, repo.join("moved")).unwrap();
                std::fs::create_dir(&dir).unwrap();
                std::fs::set_permissions(
                    repo.join("moved"),
                    std::fs::Permissions::from_mode(0o000),
                )
                .unwrap();
            },
        );
        std::fs::set_permissions(repo.join("moved"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert_eq!(result, Err(CaptureReadError::PathChanged));
        std::fs::remove_dir(&dir).unwrap();
        std::fs::rename(repo.join("moved"), &dir).unwrap();
        assert_eq!(
            root.read_capture_with_hooks(
                name,
                3,
                || {},
                || {},
                |_| {
                    std::fs::rename(&dir, repo.join("moved")).unwrap();
                    std::fs::create_dir(&dir).unwrap();
                    std::fs::rename(&repo, f.0.join("old-repo")).unwrap();
                    std::fs::create_dir(&repo).unwrap();
                }
            ),
            Err(CaptureReadError::PathChanged)
        );
        std::fs::remove_dir(&repo).unwrap();
        std::fs::rename(f.0.join("old-repo"), &repo).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        std::fs::rename(repo.join("moved"), &dir).unwrap();
        assert_eq!(
            root.read_capture_with_hooks(
                name,
                3,
                || {},
                || {},
                |_| {
                    std::fs::rename(&repo, f.0.join("old-repo")).unwrap();
                    std::fs::create_dir(&repo).unwrap();
                }
            ),
            Err(CaptureReadError::RootChanged)
        );
    }

    #[test]
    fn capture_rejects_parent_links_fifo_and_socket() {
        let f = Fixture::new();
        let dir = f.0.join(".oms/artifacts/output-digest");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("20260101T123456Z-1.log");
        let root = Root::open(&f.0).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            root.read_capture("20260101T123456Z-1.log", 1),
            Err(CaptureReadError::Unsafe)
        );
        std::fs::remove_file(&path).unwrap();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let socket_absolute = f.0.canonicalize().unwrap().join("s");
        let cwd_parts: Vec<_> = cwd.components().collect();
        let socket_parts: Vec<_> = socket_absolute.components().collect();
        let common = cwd_parts
            .iter()
            .zip(&socket_parts)
            .take_while(|(a, b)| a == b)
            .count();
        let mut socket_path = PathBuf::new();
        for _ in common..cwd_parts.len() {
            socket_path.push("..");
        }
        for part in &socket_parts[common..] {
            socket_path.push(part.as_os_str());
        }
        if socket_absolute.as_os_str().as_bytes().len() < socket_path.as_os_str().as_bytes().len() {
            socket_path = socket_absolute;
        }
        match std::os::unix::net::UnixListener::bind(&socket_path) {
            Ok(socket) => {
                std::fs::rename(&socket_path, &path).unwrap();
                assert_eq!(
                    root.read_capture("20260101T123456Z-1.log", 1),
                    Err(CaptureReadError::Unsafe)
                );
                drop(socket);
                std::fs::remove_file(&path).unwrap();
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("socket fixture unavailable: permission denied");
            }
            Err(e) => panic!("socket fixture failed: {e}"),
        }
        std::fs::remove_dir(&dir).unwrap();
        symlink("missing", &dir).unwrap();
        assert_eq!(
            root.read_capture("20260101T123456Z-1.log", 1),
            Err(CaptureReadError::Unsafe)
        );
    }
    #[test]
    fn capture_open_and_postcheck_io_errors() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let dir = f.0.join(".oms/artifacts/output-digest");
        std::fs::create_dir_all(&dir).unwrap();
        let name = "20260101T123456Z-1.log";
        let path = dir.join(name);
        std::fs::write(&path, b"one").unwrap();
        let root = Root::open(&f.0).unwrap();
        assert_eq!(
            root.read_capture_with_hooks(
                name,
                3,
                || {
                    std::fs::remove_file(&path).unwrap();
                    symlink("target", &path).unwrap();
                },
                || {},
                |_| {}
            ),
            Err(CaptureReadError::UnsafeOpen)
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"one").unwrap();
        assert_eq!(
            root.read_capture_with_hooks(
                name,
                3,
                || {},
                || {},
                |_| {
                    std::fs::remove_file(&path).unwrap();
                    std::fs::write(&path, b"one").unwrap();
                }
            ),
            Err(CaptureReadError::FileChanged)
        );
        let repo = f.0.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let inside = repo.join(".oms/artifacts/output-digest");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(inside.join(name), b"one").unwrap();
        let moved_root = Root::open(&repo).unwrap();
        assert_eq!(
            moved_root.read_capture_with_hooks(
                name,
                3,
                || {},
                || {},
                |_| {
                    std::fs::rename(&repo, f.0.join("gone")).unwrap();
                }
            ),
            Err(CaptureReadError::Missing)
        );
        if rustix::process::getuid().as_raw() != 0 {
            let result = root.read_capture_with_hooks(
                name,
                3,
                || {},
                || {},
                |_| {
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
                },
            );
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(result, Err(CaptureReadError::Unreadable));
        }
    }
}
