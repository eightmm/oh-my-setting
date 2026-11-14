use crate::{Result, Value, error, json};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use std::path::Path;

pub const MAX_FILE: usize = 8 * 1024 * 1024;
pub const MAX_STREAM: usize = 32 * 1024 * 1024;

pub fn read(root: &Path, path: &Path, limit: usize, missing: bool) -> Result<Vec<u8>> {
    Root::open(root)
        .map_err(|e| error(2, format!("{e:?}")))?
        .read_no_follow(
            path,
            ByteReadOptions {
                missing_ok: missing,
                max_bytes: limit,
            },
        )
        .map_err(|e| error(2, format!("state read: {e:?}")))
}

pub fn lock(path: &Path) -> Result<oms_lock::Guard> {
    let settings = oms_lock::Settings::from_env().map_err(|e| error(e.exit_code(), e))?;
    oms_lock::Lock::for_file(path, settings)
        .and_then(|l| l.acquire(None))
        .map_err(|e| error(e.exit_code(), e))
}

pub fn replace_locked(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    Root::open(root)
        .map_err(|e| error(2, format!("{e:?}")))?
        .atomic_mutate(
            path,
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: true,
                    max_bytes: MAX_STREAM,
                },
                create_parent: true,
                max_output: MAX_STREAM,
                ..Default::default()
            },
            |_| Ok::<_, String>(bytes.to_vec()),
        )
        .map_err(|e| {
            error(
                1,
                format!("state publication: {e:?}; reconcile before retry"),
            )
        })?;
    Ok(())
}

pub fn rows(root: &Path, path: &Path) -> Result<(Vec<Value>, usize)> {
    let bytes = read(root, path, MAX_STREAM, true)?;
    let mut rows = Vec::new();
    let mut bad = 0;
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
    {
        match json::parse_legacy(line, MAX_FILE) {
            Ok(value) if matches!(value, Value::Object(_)) => rows.push(value),
            _ => bad += 1,
        }
    }
    Ok((rows, bad))
}

pub fn append_locked(root: &Path, path: &Path, row: &Value) -> Result<()> {
    let mut encoded = json::default_bytes(row, false)?;
    encoded.push(b'\n');
    let filesystem = Root::open(root).map_err(|e| error(2, format!("{e:?}")))?;
    // The shared atomic writer keeps torn prior input separate from this row.
    filesystem
        .atomic_mutate(
            path,
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: true,
                    max_bytes: MAX_STREAM,
                },
                create_parent: true,
                max_output: MAX_STREAM,
                ..Default::default()
            },
            |old| {
                let mut new = old.to_vec();
                if !new.is_empty() && new.last() != Some(&b'\n') {
                    new.push(b'\n');
                }
                new.extend_from_slice(&encoded);
                Ok::<_, String>(new)
            },
        )
        .map_err(|e| error(1, format!("state append: {e:?}; reconcile before retry")))?;
    Ok(())
}

pub fn append(root: &Path, path: &Path, row: &Value) -> Result<()> {
    let target = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let _guard = lock(&target)?;
    append_locked(root, path, row)
}
