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
        #[cfg(not(target_os = "linux"))]
        {
            Err("native executing-artifact proof is unavailable on this host; trusted bootstrap identity is required".into())
        }
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
