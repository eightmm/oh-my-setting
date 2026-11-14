//! Exclusive private export of the already rendered and masked result text.
use crate::{Error, Result, require};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
pub fn write(target: &Path, text: &str) -> Result<usize> {
    require(
        text.len() <= 16 * 1024 * 1024,
        "export exceeds result reader limit",
    )?;
    crate::outbound(text.as_bytes())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(target)
        .map_err(|_| Error::new("--export refuses to replace an existing or unsafe file"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| {
            if text.ends_with('\n') {
                Ok(())
            } else {
                file.write_all(b"\n")
            }
        })
        .and_then(|()| file.sync_all())
        .map_err(|_| Error {
            publication_uncertain: true,
            ..Error::new("export may be incomplete; inspect the new file")
        })?;
    Ok(text.bytes().filter(|&v| v == b'\n').count() + usize::from(!text.ends_with('\n')))
}
