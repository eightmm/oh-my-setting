use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};

/// A local preparation receipt selects immutable bytes, never an executable path.
/// It is bound to the installation and checkout from which preparation started.
#[derive(Deserialize, Serialize)]
pub(crate) struct Prepared {
    schema: u32,
    source_root: PathBuf,
    install_receipt: PathBuf,
    source_device: u64,
    source_inode: u64,
    pub current_commit: String,
    pub target_commit: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub edge_branch: Option<String>,
    pub manifest_sha256: String,
    install_receipt_sha256: String,
    pub prepared_at: String,
}

fn directory(paths: &Paths) -> PathBuf {
    paths.config.join("native-preparation")
}
pub(crate) fn path(paths: &Paths) -> PathBuf {
    directory(paths).join("prepared.json")
}
fn journal(paths: &Paths) -> PathBuf {
    directory(paths).join("native-lifecycle/transaction.json")
}

/// Preparation recovery may remove only its staged payload and preparation
/// receipt. It must never recover the live installation's update journal.
pub(crate) fn transaction(paths: &Paths) -> Result<Transaction> {
    let mut scratch = paths.clone();
    scratch.config = directory(paths);
    r::private_dir(&scratch.config)?;
    Transaction::begin(&scratch)
}

/// Resolve only a pending preparation journal before another lifecycle path
/// adopts a prepared cache. Callers hold the installation and receipt locks.
pub(crate) fn recover(paths: &Paths) -> Result<()> {
    if r::exists(&journal(paths)) {
        transaction(paths)?.commit()?;
    }
    Ok(())
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Prepared {
    pub(crate) fn capture(
        paths: &Paths,
        current: &str,
        target: &str,
        reference: &str,
        edge_branch: Option<String>,
    ) -> Result<Self> {
        let source = fs::symlink_metadata(&paths.source)?;
        let row = Self {
            schema: 1,
            source_root: paths.source.clone(),
            install_receipt: paths.receipt.clone(),
            source_device: source.dev(),
            source_inode: source.ino(),
            current_commit: current.into(),
            target_commit: target.into(),
            reference: reference.into(),
            edge_branch,
            manifest_sha256: crate::payload::prepared_digest(paths, target)?,
            install_receipt_sha256: r::hash(&r::read(&paths.receipt, r::LIMIT)?),
            prepared_at: r::now(),
        };
        row.validate(paths)?;
        Ok(row)
    }

    fn validate(&self, paths: &Paths) -> Result<()> {
        if self.schema != 1
            || self.source_root != paths.source
            || self.install_receipt != paths.receipt
            || !hex(&self.current_commit, 40)
            || !hex(&self.target_commit, 40)
            || !hex(&self.manifest_sha256, 64)
            || !hex(&self.install_receipt_sha256, 64)
            || chrono::DateTime::parse_from_rfc3339(&self.prepared_at).is_err()
        {
            return r::fail("invalid or foreign prepared update receipt");
        }
        super::update::validate_reference(&self.reference)?;
        match (&self.edge_branch, self.reference.as_str()) {
            (Some(branch), "edge") if branch.starts_with("refs/heads/") => {
                super::update::validate_reference(branch)?;
            }
            (None, reference) if reference != "edge" => (),
            _ => return r::fail("invalid prepared edge branch"),
        }
        Ok(())
    }

    pub(crate) fn require_current(&self, paths: &Paths, current: &str) -> Result<()> {
        let source = fs::symlink_metadata(&paths.source)?;
        if self.current_commit != current
            || source.dev() != self.source_device
            || source.ino() != self.source_inode
            || r::hash(&r::read(&paths.receipt, r::LIMIT)?) != self.install_receipt_sha256
        {
            return Err(r::Error(
                75,
                "prepared update is stale; prepare again from the current installation".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn verify_payload(&self, paths: &Paths) -> Result<()> {
        if crate::payload::prepared_digest(paths, &self.target_commit)? != self.manifest_sha256 {
            return r::fail("prepared update payload changed; prepare again");
        }
        Ok(())
    }

    pub(crate) fn write(&self, paths: &Paths, tx: &mut Transaction) -> Result<()> {
        let value = serde_json::to_value(self)
            .map_err(|_| r::Error(2, "cannot serialize prepared update receipt".into()))?;
        tx.write(&path(paths), &r::bytes(&value)?, 0o600)
    }

    pub(crate) fn projection(&self, status: &str, reason: Option<&str>) -> Value {
        json!({
            "schema": 1,
            "status": status,
            "current_commit": self.current_commit,
            "target_commit": self.target_commit,
            "ref": self.reference,
            "prepared_at": self.prepared_at,
            "manifest_sha256": self.manifest_sha256,
            "apply_command": if status == "prepared" { Some("oms update --apply-prepared") } else { None },
            "reason": reason,
        })
    }
}

pub(crate) fn load(paths: &Paths) -> Result<Option<Prepared>> {
    let file = path(paths);
    if !r::exists(&file) {
        return Ok(None);
    }
    let dir = fs::symlink_metadata(directory(paths))?;
    let metadata = fs::symlink_metadata(&file)?;
    let uid = rustix::process::geteuid().as_raw();
    if !dir.is_dir()
        || dir.file_type().is_symlink()
        || dir.uid() != uid
        || dir.mode() & 0o022 != 0
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return r::fail("unsafe prepared update receipt");
    }
    let value = r::parse(&r::read(&file, 65536)?)?;
    let row: Prepared = serde_json::from_value(value)
        .map_err(|_| r::Error(2, "invalid prepared update receipt".into()))?;
    row.validate(paths)?;
    Ok(Some(row))
}

/// Read-only: status never repairs interrupted preparation or installation.
pub(crate) fn status(paths: &Paths, current: &str) -> Result<Value> {
    if r::exists(&journal(paths)) {
        return Ok(json!({
            "schema": 1, "status": "incomplete", "current_commit": current,
            "target_commit": null, "ref": null, "prepared_at": null,
            "apply_command": null,
            "reason": "preparation is running or interrupted; wait, or prepare again after interruption",
        }));
    }
    let Some(row) = load(paths)? else {
        return Ok(json!({
            "schema": 1, "status": "none", "current_commit": current,
            "target_commit": null, "ref": null, "prepared_at": null,
            "apply_command": null, "reason": null,
        }));
    };
    let state = if let Err(error) = row.require_current(paths, current) {
        row.projection("stale", Some(&error.1))
    } else if !r::exists(
        &paths
            .source
            .join("target/oms-native")
            .join(&row.target_commit),
    ) {
        row.projection(
            "missing_payload",
            Some("prepared payload is missing; prepare again"),
        )
    } else if let Err(error) = row.verify_payload(paths) {
        row.projection("invalid_payload", Some(&error.1))
    } else {
        row.projection("prepared", None)
    };
    let mut state = state;
    state["current_commit"] = json!(current);
    Ok(state)
}
