//! A host-recorded review reference addressed to one exact Room Main. It neither
//! impersonates a sender in the Room log nor starts planning or provider work.
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::model::{hex, identifier};
use crate::storage::{Directory, Store};
use crate::value::{Value, encode, obj, os_value, parse};
use crate::{Error, Result, Room};

const DIRECTORY: &str = "reviewed-issue-receipts";
const LIMIT: usize = 2048;

pub(super) struct IssueTarget<'a> {
    pub checkout: &'a Path,
    pub room: &'a str,
    pub owner: &'a str,
    pub consumer: &'a str,
    pub request: Option<&'a str>,
}

impl Room<'_> {
    pub(super) fn reviewed_issue(
        &self,
        target: &IssueTarget<'_>,
        selection: Option<(&str, &str)>,
        record: bool,
    ) -> Result<Value> {
        self.check_desktop_consumer(target.room, target.consumer)?;
        if target
            .request
            .is_some_and(|id| !crate::desktop::request_uuid(id))
            || (record && (selection.is_none() || target.request.is_none()))
            || (!record && selection.is_some() == target.request.is_some())
        {
            return Err(Error::new("invalid issue receipt selector"));
        }
        identifier(target.owner, "participant")?;
        if self
            .session
            .room
            .as_deref()
            .is_some_and(|r| r != target.room)
            || selection
                .is_some_and(|(issue, sha)| issue.is_empty() || issue.len() > 165 || !hex(sha, 64))
        {
            return Err(Error::new("invalid reviewed issue or Room binding"));
        }
        let (binding, existing) = self.issue_receipt_at(target, selection, None)?;
        if !record || existing.text("status") == "recorded" {
            return Ok(existing);
        }
        let (reference, pin) = selection.ok_or_else(|| Error::new("missing reviewed issue"))?;
        if existing.text("requestId") != selection_id(target.checkout, &binding, reference, pin)? {
            return Err(Error::new(
                "review this issue in the current Main before recording",
            ));
        }
        // The bounded GitHub GET must finish before either a lifecycle/Room lock
        // or a request publication lock is acquired for the write.
        self.services
            .check_reviewed_issue(target.checkout, &self.repo, reference, Some(pin))?;
        let (_, receipt) = self.issue_receipt_at(target, selection, Some(&binding))?;
        Ok(receipt)
    }

    fn issue_receipt_at(
        &self,
        target: &IssueTarget<'_>,
        selection: Option<(&str, &str)>,
        checked_binding: Option<&str>,
    ) -> Result<(String, Value)> {
        let store = self.storage()?;
        let guard = store.lock(target.room, self.stop)?;
        let operations = self.operations()?;
        let room = self.assignment_owner_with(target.room, target.consumer, operations.as_ref())?;
        if room.text("owner") != target.owner {
            return Err(Error::new("reviewed issue is addressed to another Main"));
        }
        let snapshot = self.snapshot(target.room)?;
        let enrollment = snapshot
            .rows
            .iter()
            .rev()
            .find(|r| {
                let event = &r["room_event"];
                event.text("participant") == target.owner
                    && matches!(
                        event.text("kind"),
                        "join" | "main-resume" | "main-swap" | "bind"
                    )
            })
            .ok_or_else(|| Error::new("Main enrollment unavailable"))?;
        let binding = crate::digest(&encode(
            &obj([
                ("checkout", os_value(target.checkout.as_os_str())),
                ("state", os_value(self.repo.as_os_str())),
                ("room", target.room.into()),
                ("participant", target.owner.into()),
                ("consumer", target.consumer.into()),
                ("device", snapshot.stat.st_dev.to_string().into()),
                ("inode", snapshot.stat.st_ino.to_string().into()),
                ("enrollment", enrollment["seq"].clone()),
            ]),
            false,
            true,
        )?);
        if checked_binding.is_some_and(|prior| prior != binding) {
            return Err(Error::new("Main binding changed during issue review"));
        }
        let request = match (target.request, selection) {
            (Some(request), _) => request.to_owned(),
            (None, Some((reference, pin))) => {
                selection_id(target.checkout, &binding, reference, pin)?
            }
            _ => return Err(Error::new("missing issue receipt selector")),
        };
        if let Some((reference, _)) = selection {
            self.services
                .check_reviewed_issue(target.checkout, &self.repo, reference, None)?;
        }
        let name = format!("{request}.json");
        let missing = |dir: Option<&Directory>| -> Result<Value> {
            guard.check().map_err(|e| Error::new(e.to_string()))?;
            operations.check()?;
            store.check()?;
            if let Some(dir) = dir {
                dir.private()?;
                if dir.exists(OsStr::new(&name))? {
                    return Err(Error::new("reviewed issue receipt appeared during lookup"));
                }
                dir.check()?;
            } else if store.oms.optional_child(OsStr::new(DIRECTORY))?.is_some() {
                return Err(Error::new("reviewed issue receipt directory changed"));
            }
            Ok(obj([
                ("status", "not_found".into()),
                ("requestId", request.as_str().into()),
            ]))
        };
        let dir = if checked_binding.is_some() {
            Some(store.oms.child(OsStr::new(DIRECTORY), true)?)
        } else {
            store.oms.optional_child(OsStr::new(DIRECTORY))?
        };
        let Some(dir) = dir else {
            return Ok((binding, missing(None)?));
        };
        dir.private()?;
        // UUIDs are shared by every Room of a state repository. Different
        // Room locks alone cannot serialize two first writers to the same UUID.
        let request_lock = if checked_binding.is_some() {
            let settings = oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?;
            let lock = oms_lock::Lock::for_file(
                &store.repo.join(".oms").join(DIRECTORY).join(&name),
                settings,
            )
            .map_err(|e| Error::new(e.to_string()))?;
            Some(Store::acquire(&lock, self.stop)?)
        } else {
            None
        };
        let existing = dir.exists(OsStr::new(&name))?;
        if !existing && checked_binding.is_none() {
            return Ok((binding, missing(Some(&dir))?));
        }
        let prior_bytes = existing
            .then(|| read_issue_bytes(&dir, &name))
            .transpose()?;
        let stored = prior_bytes.as_ref().map(|b| parse(b, LIMIT)).transpose()?;
        let (reference, pin) = selection.unwrap_or_else(|| {
            let old = stored.as_ref().expect("status only reads existing records");
            (old.text("issue"), old.text("sha256"))
        });
        let mut record = obj([
            ("schema", 1u64.into()),
            ("kind", "oms-reviewed-issue-receipt".into()),
            ("binding", binding.clone().into()),
            ("requestId", request.as_str().into()),
            ("room", target.room.into()),
            ("participant", target.owner.into()),
            ("issue", reference.into()),
            ("sha256", pin.into()),
        ]);
        if stored
            .as_ref()
            .is_none_or(|r| r.get("boundAddress").is_some())
        {
            if request != selection_id(target.checkout, &binding, reference, pin)? {
                return Err(Error::new(
                    "reviewed issue address no longer matches this checkout",
                ));
            }
            record.set("boundAddress", true);
        }
        if !hex(pin, 64) || stored.as_ref().is_some_and(|old| old != &record) {
            return Err(Error::new(
                "reviewed issue receipt conflicts with this Main or request",
            ));
        }
        if selection.is_none() {
            self.services
                .check_reviewed_issue(target.checkout, &self.repo, reference, None)?;
        }
        let check_receipt_storage = || -> Result<()> {
            guard.check().map_err(|e| Error::new(e.to_string()))?;
            if let Some(lock) = &request_lock {
                lock.check().map_err(|e| Error::new(e.to_string()))?;
            }
            store.check()?;
            dir.private()?;
            dir.check()
        };
        operations.check()?;
        check_receipt_storage()?;
        let expected = if let Some(bytes) = prior_bytes {
            bytes
        } else {
            let bytes = encode(&record, false, true)?;
            dir.publish_queue(
                OsStr::new(&format!(".{}.tmp", crate::unique()?)),
                OsStr::new(&name),
                &bytes,
            )?;
            operations.check()?;
            bytes
        };
        // Relay publication stays pinned across renames. Receipts must also be
        // reachable with the same bytes from the selected project's named path.
        if read_issue_bytes(&dir, &name)? != expected {
            return Err(Error::new("reviewed issue receipt changed during lookup"));
        }
        check_receipt_storage()?;
        let mut out = record;
        out.remove("binding");
        out.remove("boundAddress");
        out.set("status", "recorded");
        Ok((binding, out))
    }
}

// UUIDv8, derived from a verified enrollment and an exact reviewed version.
fn selection_id(checkout: &Path, binding: &str, reference: &str, pin: &str) -> Result<String> {
    let meta = std::fs::symlink_metadata(checkout).map_err(|e| Error::new(e.to_string()))?;
    if !meta.is_dir() {
        return Err(Error::new("reviewed issue checkout is unavailable"));
    }
    let digest = crate::digest(
        format!(
            "oms-reviewed-issue-v1\0{binding}\0{}:{}\0{reference}\0{pin}",
            meta.dev(),
            meta.ino()
        )
        .as_bytes(),
    );
    let variant = 8 + (u8::from_str_radix(&digest[16..17], 16).unwrap() & 3);
    Ok(format!(
        "{}-{}-8{}-{variant:x}{}-{}",
        &digest[..8],
        &digest[8..12],
        &digest[13..16],
        &digest[17..20],
        &digest[20..32]
    ))
}

fn read_issue_bytes(dir: &Directory, name: &str) -> Result<Vec<u8>> {
    let (bytes, stat) = dir.read(OsStr::new(name), LIMIT)?;
    if stat.st_mode & 0o7777 != 0o600 || stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(Error::new(
            "reviewed issue receipt must be private and owned",
        ));
    }
    Ok(bytes)
}
