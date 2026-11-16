//! Command-local proof of the executing native artifact. A pathname, source
//! inventory or supplied digest cannot establish this kernel relationship.
use crate::{Result, storage};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    os::unix::fs::{FileExt, MetadataExt},
    sync::Arc,
};

/// Trusted native bootstrap boundary. Implementations must positively prove
/// that this retained descriptor is the current process's executing artifact
/// using native kernel identity. Path comparisons, bundle/source hashes and
/// declarations from peers are insufficient. No blanket callback adapter is
/// provided: each platform implementation owns its actual native proof.
pub trait ExecutingImage: Send + Sync {
    fn verify_current(&self, image: &File) -> Result<()>;
}

struct Snapshot {
    image: File,
    generation: Metadata,
    identity: Arc<dyn ExecutingImage>,
    process: u32,
    digest: String,
}

/// Clones retain one checked descriptor and one digest, without another hash
/// or persistent cache. Each consumer re-establishes the native relationship
/// and file generation before using this command's snapshot.
#[derive(Clone)]
pub struct Controller(Arc<Snapshot>);
impl Controller {
    pub fn capture_current() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let identity = Arc::new(linux::KernelImage::open()?);
            let image = identity.executable()?;
            Self::from_authenticated_descriptor(image, identity)
        }
        #[cfg(target_os = "macos")]
        {
            let identity = Arc::new(macos::KernelImage::open()?);
            let image = identity.executable()?;
            Self::from_authenticated_descriptor(image, identity)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err("native executing-artifact proof is unavailable on this host; trusted bootstrap identity is required".into())
        }
    }

    /// Captures `image` only when it is the current process's executable;
    /// `None` means it is a different file (for example another checkout's).
    #[cfg(target_os = "macos")]
    pub fn capture_if_current(image: &File) -> Result<Option<Self>> {
        // A different file is not an error: launchers and updaters inspect
        // other artifacts. Strict checks run only for the executing image.
        if !macos::is_current_exe(image) {
            return Ok(None);
        }
        let identity = Arc::new(macos::KernelImage::open()?);
        let image = image.try_clone().map_err(|e| e.to_string())?;
        Self::from_authenticated_descriptor(image, identity).map(Some)
    }

    /// The bootstrap implementation must prove the descriptor's executing
    /// identity before and after hashing and on every later use. Supplying an
    /// arbitrary open file plus a pathname/digest checker violates this native
    /// service contract and cannot be used by production composition.
    pub fn from_authenticated_descriptor(
        image: File,
        identity: Arc<dyn ExecutingImage>,
    ) -> Result<Self> {
        identity.verify_current(&image)?;
        let generation = image.metadata().map_err(|e| e.to_string())?;
        if !generation.is_file() || generation.size() == 0 || generation.mode() & 0o002 != 0 {
            return Err("native executing artifact is unsafe".into());
        }
        // Positional reads do not inherit or race a duplicated descriptor's
        // shared cursor. Size/generation checks still fence concurrent change.
        let mut state = Sha256::new();
        let mut buffer = [0; 128 * 1024];
        let mut offset = 0;
        while offset < generation.size() {
            let length = (generation.size() - offset).min(buffer.len() as u64) as usize;
            let count = image
                .read_at(&mut buffer[..length], offset)
                .map_err(|e| e.to_string())?;
            if count == 0 {
                return Err("native executing-artifact generation changed during read".into());
            }
            state.update(&buffer[..count]);
            offset += count as u64;
        }
        if image
            .read_at(&mut buffer[..1], offset)
            .map_err(|e| e.to_string())?
            != 0
        {
            return Err("native executing-artifact generation changed during read".into());
        }
        let controller = Self(Arc::new(Snapshot {
            image,
            generation,
            identity,
            process: std::process::id(),
            digest: format!("{:x}", state.finalize()),
        }));
        controller.check()?;
        Ok(controller)
    }

    pub fn check(&self) -> Result<()> {
        if self.0.process != std::process::id() {
            return Err("native executing-artifact snapshot belongs to another process".into());
        }
        self.0.identity.verify_current(&self.0.image)?;
        let current = self.0.image.metadata().map_err(|e| e.to_string())?;
        if !current.is_file() || !storage::generation(&self.0.generation, &current) {
            return Err("native executing-artifact generation changed".into());
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<&str> {
        self.check()?;
        Ok(&self.0.digest)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use rustix::fs::{self as rfs, Mode, OFlags};

    pub(super) struct KernelImage {
        proc_root: File,
        process_dir: File,
        process: u32,
    }
    impl KernelImage {
        pub(super) fn open() -> Result<Self> {
            let proc_root: File = rfs::open(
                "/proc",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| e.to_string())?
            .into();
            if rfs::fstatfs(&proc_root).map_err(|e| e.to_string())?.f_type != rfs::PROC_SUPER_MAGIC
            {
                return Err("native executing-artifact procfs proof unavailable".into());
            }
            let process = std::process::id();
            let named =
                rfs::readlinkat(&proc_root, "self", Vec::new()).map_err(|e| e.to_string())?;
            if named.to_bytes() != process.to_string().as_bytes() {
                return Err("native executing-artifact PID namespace is unproven".into());
            }
            let process_dir: File = rfs::openat(
                &proc_root,
                process.to_string(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| e.to_string())?
            .into();
            if rfs::fstatfs(&process_dir)
                .map_err(|e| e.to_string())?
                .f_type
                != rfs::PROC_SUPER_MAGIC
            {
                return Err("native executing-artifact process directory is not procfs".into());
            }
            Ok(Self {
                proc_root,
                process_dir,
                process,
            })
        }
        pub(super) fn executable(&self) -> Result<File> {
            if self.process != std::process::id() {
                return Err("native executing-artifact process changed".into());
            }
            // This fixed kernel magic link is deliberately followed against a
            // proven procfs process directory; no caller pathname is opened.
            rfs::openat(
                &self.process_dir,
                "exe",
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|e| e.to_string())
        }
    }
    impl ExecutingImage for KernelImage {
        fn verify_current(&self, image: &File) -> Result<()> {
            let named =
                rfs::readlinkat(&self.proc_root, "self", Vec::new()).map_err(|e| e.to_string())?;
            if named.to_bytes() != self.process.to_string().as_bytes() {
                return Err("native executing-artifact PID namespace changed".into());
            }
            let current = self.executable()?;
            if !storage::generation(
                &image.metadata().map_err(|e| e.to_string())?,
                &current.metadata().map_err(|e| e.to_string())?,
            ) {
                return Err(
                    "retained descriptor is not the current native executing artifact".into(),
                );
            }
            Ok(())
        }
    }
}

/// Ownership and permission policy for the macOS path-based proof. Pure over
/// metadata so Linux CI also exercises every refusal. It mirrors the strictness
/// of the bootstrap authority checks: a trusted owner is the effective user or
/// root, and nobody else may rewrite the image or rename entries above it.
#[cfg(any(target_os = "macos", test))]
mod policy {
    use super::*;

    fn trusted(meta: &Metadata, euid: u32) -> bool {
        meta.uid() == euid || meta.uid() == 0
    }
    /// The executable leaf: one link, nonempty, no group/world write, no setid.
    pub(super) fn leaf(meta: &Metadata, euid: u32) -> bool {
        meta.is_file()
            && meta.nlink() == 1
            && meta.size() > 0
            && trusted(meta, euid)
            && meta.mode() & 0o6022 == 0
    }
    /// Every ancestor must be trusted and not group-writable. The immediate
    /// parent may never be world-writable, even sticky, since the image is
    /// then replaceable by its owner-peers. Higher ancestors (for example
    /// /tmp) tolerate world-write only when sticky and root-owned.
    pub(super) fn ancestor(meta: &Metadata, euid: u32, immediate: bool) -> bool {
        let world = meta.mode() & 0o002 != 0;
        meta.is_dir()
            && trusted(meta, euid)
            && meta.mode() & 0o4020 == 0
            && (!world || (!immediate && meta.uid() == 0 && meta.mode() & 0o1000 != 0))
    }
    /// Later use must see the very generation captured at open and offered.
    pub(super) fn generation(initial: &Metadata, held: &Metadata, offered: &Metadata) -> bool {
        storage::generation(initial, held) && storage::generation(held, offered)
    }
}

/// macOS has no procfs. The proof is the kernel's `F_GETPATH` vnode name of a
/// descriptor opened without following links, bound to the process's
/// executable path by (dev, ino) on every use. Replacing or moving the
/// executable fails closed.
#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use rustix::fs::{self as rfs, Mode, OFlags};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    pub(super) struct KernelImage {
        file: File,
        initial: Metadata,
        path: PathBuf,
        process: u32,
    }
    fn open_leaf(path: &Path) -> Result<File> {
        rfs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|e| e.to_string())
    }
    fn same(left: &Metadata, right: &Metadata) -> bool {
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    fn current_exe() -> Result<PathBuf> {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| e.to_string())
    }
    fn ancestors_private(path: &Path) -> Result<()> {
        let euid = rustix::process::geteuid().as_raw();
        for (depth, directory) in path.ancestors().skip(1).enumerate() {
            let meta = std::fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
            if !policy::ancestor(&meta, euid, depth == 0) {
                return Err("native executing artifact has an unsafe ancestor directory".into());
            }
        }
        Ok(())
    }
    fn kernel_path(file: &File) -> Result<PathBuf> {
        let named = rfs::getpath(file).map_err(|e| e.to_string())?;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(named.to_bytes())))
    }
    /// Cheap identity probe: does `image` share the running executable's
    /// vnode? Any failure to resolve it means "not that file", never an error,
    /// so unrelated launchers and artifacts never trip the strict checks.
    pub(super) fn is_current_exe(image: &File) -> bool {
        let (Ok(path), Ok(meta)) = (current_exe(), image.metadata()) else {
            return false;
        };
        open_leaf(&path)
            .and_then(|f| f.metadata().map_err(|e| e.to_string()))
            .is_ok_and(|current| same(&current, &meta))
    }
    impl KernelImage {
        pub(super) fn open() -> Result<Self> {
            let file = open_leaf(&current_exe()?)?;
            let initial = file.metadata().map_err(|e| e.to_string())?;
            if !policy::leaf(&initial, rustix::process::geteuid().as_raw()) {
                return Err("native executing artifact is unsafe".into());
            }
            let path = kernel_path(&file)?;
            ancestors_private(&path)?;
            let image = Self {
                file,
                initial,
                path,
                process: std::process::id(),
            };
            image.verify_current(&image.file)?;
            Ok(image)
        }
        pub(super) fn executable(&self) -> Result<File> {
            if self.process != std::process::id() {
                return Err("native executing-artifact process changed".into());
            }
            self.file.try_clone().map_err(|e| e.to_string())
        }
    }
    impl ExecutingImage for KernelImage {
        fn verify_current(&self, image: &File) -> Result<()> {
            if self.process != std::process::id() {
                return Err("native executing-artifact process changed".into());
            }
            let held = self.file.metadata().map_err(|e| e.to_string())?;
            let offered = image.metadata().map_err(|e| e.to_string())?;
            if !policy::leaf(&held, rustix::process::geteuid().as_raw())
                || !policy::generation(&self.initial, &held, &offered)
            {
                return Err(
                    "retained descriptor is not the current native executing artifact".into(),
                );
            }
            if kernel_path(&self.file)? != self.path {
                return Err("native executing artifact was moved or unlinked".into());
            }
            ancestors_private(&self.path)?;
            // Both the kernel's name for the descriptor and the process's own
            // executable path must still resolve to this exact vnode.
            let by_name = open_leaf(&self.path)?
                .metadata()
                .map_err(|e| e.to_string())?;
            let by_exe = open_leaf(&current_exe()?)?
                .metadata()
                .map_err(|e| e.to_string())?;
            if !same(&held, &by_name) || !same(&held, &by_exe) {
                return Err("native executing artifact was replaced".into());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn euid() -> u32 {
        rustix::process::geteuid().as_raw()
    }
    fn file(name: &str, mode: u32) -> (std::path::PathBuf, Metadata) {
        let path = std::env::temp_dir().join(format!("oms-ref-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"image").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        (path, meta)
    }

    #[test]
    fn leaf_policy_refuses_unsafe_images() {
        let (path, ok) = file("leaf-ok", 0o755);
        assert!(policy::leaf(&ok, euid()));
        // A different effective user distrusts a file we own (root is always
        // trusted, so this edge only exists for unprivileged owners).
        if ok.uid() != 0 {
            assert!(!policy::leaf(&ok, ok.uid() + 1));
        }
        for (name, mode) in [("group", 0o775), ("world", 0o757), ("setuid", 0o4755)] {
            let (p, meta) = file(name, mode);
            assert!(!policy::leaf(&meta, euid()), "{name}");
            std::fs::remove_file(p).unwrap();
        }
        std::fs::hard_link(&path, path.with_extension("link")).unwrap();
        let linked = std::fs::symlink_metadata(&path).unwrap();
        assert!(!policy::leaf(&linked, euid()), "extra hard link");
        std::fs::remove_file(path.with_extension("link")).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(
            !policy::leaf(&std::fs::symlink_metadata(&path).unwrap(), euid()),
            "empty"
        );
        assert!(!policy::leaf(
            &std::fs::symlink_metadata(std::env::temp_dir()).unwrap(),
            euid()
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ancestor_policy_separates_immediate_parent_from_higher_ancestors() {
        let dir = std::env::temp_dir().join(format!("oms-ref-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let meta = |mode: u32| {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            std::fs::symlink_metadata(&dir).unwrap()
        };
        for immediate in [true, false] {
            assert!(policy::ancestor(&meta(0o755), euid(), immediate));
            assert!(
                !policy::ancestor(&meta(0o775), euid(), immediate),
                "group-writable"
            );
            assert!(
                !policy::ancestor(&meta(0o777), euid(), immediate),
                "world-writable"
            );
            // Sticky is not enough unless root owns it and it is not the parent.
            assert!(!policy::ancestor(&meta(0o1777), euid(), immediate) || meta(0o1777).uid() == 0);
        }
        let own = meta(0o755);
        if own.uid() != 0 {
            assert!(
                !policy::ancestor(&own, own.uid() + 1, false),
                "foreign owner"
            );
        }
        let (path, regular) = file("not-dir", 0o755);
        assert!(!policy::ancestor(&regular, euid(), false));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
        // A root-owned sticky /tmp is acceptable only above the parent.
        if let Ok(tmp) = std::fs::symlink_metadata("/tmp")
            && tmp.uid() == 0
            && tmp.mode() & 0o1002 == 0o1002
            && tmp.mode() & 0o020 == 0
        {
            assert!(policy::ancestor(&tmp, euid(), false));
            assert!(!policy::ancestor(&tmp, euid(), true));
        }
    }

    #[test]
    fn generation_policy_rejects_change_since_capture_or_offer() {
        let (path, initial) = file("generation", 0o755);
        let held = std::fs::symlink_metadata(&path).unwrap();
        assert!(policy::generation(&initial, &held, &held));
        std::fs::write(&path, b"tampered image").unwrap();
        let changed = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            !policy::generation(&initial, &changed, &changed),
            "size change since open"
        );
        assert!(
            !policy::generation(&changed, &changed, &initial),
            "offered differs"
        );
        let (other, different) = file("generation-other", 0o755);
        assert!(
            !policy::generation(&changed, &changed, &different),
            "different inode"
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(other).unwrap();
    }

    #[test]
    fn controller_refuses_unauthenticated_images_without_a_provider() {
        struct Never;
        impl ExecutingImage for Never {
            fn verify_current(&self, _: &File) -> Result<()> {
                Err("not the executing image".into())
            }
        }
        let (path, _) = file("provider", 0o755);
        let image = File::open(&path).unwrap();
        assert!(Controller::from_authenticated_descriptor(image, Arc::new(Never)).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
