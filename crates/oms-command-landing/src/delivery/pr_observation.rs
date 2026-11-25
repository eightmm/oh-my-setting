//! While a pull-request landing (publish-only or `--merge`) reads CI, keep both
//! a verifiable request owner and the receipt lock taken by older writers
//! before they can gate or publish.
use super::{Run, TargetMoved, open_pr, save};
use crate::{Error, Result, V, check, ci, digest, reconcile, require, storage};
use std::time::Instant;

pub(crate) fn owner_name(stamp: &str) -> String {
    format!("{stamp}.ci-owner")
}

fn lock_error(e: oms_lock::Error) -> Error {
    Error::new(e.exit_code(), e.to_string())
}

pub(super) struct Checkpoint {
    root: oms_safe_fs::Root,
    global: oms_lock::Lock,
    receipt: Vec<u8>,
    holder: V,
    owner: oms_lock::Guard,
    legacy_receipt: oms_lock::Guard,
}

pub(super) struct Reentry {
    pub(super) guard: oms_lock::Guard,
    pub(super) receipt: Vec<u8>,
    pub(super) moved: Option<TargetMoved>,
}

impl Checkpoint {
    pub(super) fn begin(
        r: &Run<'_>,
        row: &V,
        active: &mut Option<oms_lock::Guard>,
    ) -> Result<Self> {
        let root = oms_safe_fs::Root::open_private_workspace(r.directory, |_| false)
            .map_err(|_| Error::new(2, "private landing store unavailable for CI observation"))?;
        let name = format!("{}.json", r.stamp);
        let receipt = row.json(false)?;
        require(
            reconcile::read_private(&root, &name).as_ref() == Some(&receipt)
                && reconcile::matches_checkout(
                    row,
                    &r.snapshot.head,
                    r.stamp,
                    &digest(r.repo.as_os_str().as_encoded_bytes()),
                )
                && row.get("remote").text() == Some(r.remote)
                && row.get("target").text() == Some(r.target)
                && row.get("head_branch").text() == Some(r.head_branch)
                && row.get("mode").text() == Some("pr")
                && row.get("gate").get("command").text() == Some(r.gate)
                && row.get("gate").get("rc") == &0usize.into(),
            "private PR checkpoint does not match this landing",
        )?;
        let holder = reconcile::read_private(&root, "active-holder")
            .and_then(|raw| V::parse(&raw).ok())
            .ok_or_else(|| Error::new(2, "current landing holder unavailable"))?;
        let birth = oms_command_operations::lock::process_start(std::process::id());
        require(
            holder.get("sha").text() == Some(&r.snapshot.head)
                && holder.get("receipt").text() == Some(name.as_str())
                && holder.get("pid") == &V::from(std::process::id() as usize)
                && birth
                    .as_deref()
                    .is_some_and(|b| !b.is_empty() && holder.get("start").text() == Some(b)),
            "cannot verify the current landing process before CI observation",
        )?;
        let path = r.directory.join(owner_name(r.stamp));
        save(
            &r.directory.join(format!("{}.ci-holder", r.stamp)),
            &holder,
            r.c.interrupted,
        )?;
        let owner = storage::lock(&path, r.c.interrupted)?;
        let legacy_receipt = storage::lock(r.receipt, r.c.interrupted)?;
        save(
            &r.directory.join("active-holder"),
            &V::Null,
            r.c.interrupted,
        )?;
        active
            .take()
            .ok_or_else(|| Error::new(2, "missing publication lock"))?
            .release()
            .map_err(lock_error)?;
        Ok(Self {
            root,
            global: storage::file_lock(&r.directory.join("active"))?,
            receipt,
            holder,
            owner,
            legacy_receipt,
        })
    }

    pub(super) fn check_legacy_claim(&self) -> Result<()> {
        if self.global.probe() == oms_lock::Status::Held
            && reconcile::read_private(&self.root, "active-holder")
                .and_then(|raw| V::parse(&raw).ok())
                .is_some_and(|h| h.get("receipt") == self.holder.get("receipt"))
        {
            return Err(Error::new(
                75,
                "PR recorded; an existing writer is waiting on this receipt; final CI observation deferred",
            ));
        }
        Ok(())
    }

    pub(super) fn reenter(self, r: &Run<'_>, original_pr: &V, base: &str) -> Result<Reentry> {
        let root = oms_safe_fs::Root::open_private_workspace(r.directory, |_| false)
            .map_err(|_| Error::new(2, "landing store changed during CI"))?;
        let settings = oms_lock::Settings::from_env().map_err(lock_error)?;
        let started = Instant::now();
        let timeout = settings.timeout;
        let lock =
            oms_lock::Lock::for_file(&r.directory.join("active"), settings).map_err(lock_error)?;
        let guard = loop {
            check(r.c.interrupted)?;
            match lock.try_acquire() {
                Ok(guard) => break guard,
                Err(oms_lock::Error::Busy) => {
                    // A legacy duplicate may hold the global lock while waiting
                    // for this receipt. Yield without a write instead of cycling.
                    self.check_legacy_claim()?;
                    if started.elapsed() >= timeout {
                        return Err(Error::new(
                            75,
                            "PR recorded; publication lock busy, final CI receipt deferred; rerun the same oms land command to resume this receipt",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(lock_error(e)),
            }
        };
        self.owner.check().map_err(lock_error)?;
        self.legacy_receipt.check().map_err(lock_error)?;
        save(
            &r.directory.join("active-holder"),
            &self.holder,
            r.c.interrupted,
        )?;
        require(
            reconcile::read_private(&root, &format!("{}.json", r.stamp)).as_ref()
                == Some(&self.receipt),
            "PR receipt changed during CI; final observation withheld",
        )?;
        let moved = validate_destination(r, original_pr, base)?;
        guard.check().map_err(lock_error)?;
        Ok(Reentry {
            guard,
            receipt: self.receipt,
            moved,
        })
    }
}

// Remote source and base are observed separately. Only the source OID has
// a server-side precondition on GitHub's eventual merge request.
pub(super) fn validate_destination(
    r: &Run<'_>,
    original_pr: &V,
    base: &str,
) -> Result<Option<TargetMoved>> {
    r.git.transport_safe()?;
    require(
        r.git
            .text(&["remote", "get-url", "--push", "--all", r.remote])?
            == r.destination
            && r.git.snapshot()? == *r.snapshot,
        "reviewed checkout or destination changed during CI; final observation withheld",
    )?;
    let pr = open_pr(r, &ci::query_repo(r.destination)?)?;
    require(
        pr.as_ref().is_some_and(|p| p == original_pr),
        "exact open PR changed during CI; final observation withheld",
    )?;
    require(
        crate::review::remote_tip(r.git, r.destination, r.head_branch)?.as_deref()
            == Some(&r.snapshot.head),
        "reviewed PR head changed during CI; final observation withheld",
    )?;
    Ok(TargetMoved::observe(
        crate::review::remote_tip(r.git, r.destination, r.target)?,
        base,
        "target moved during CI; final observation withheld. Fetch the target, merge it into the PR branch and rerun land with the same head branch so CI checks the combined tree",
    ))
}
