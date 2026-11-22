use crate::{Output, common::*};
use std::{ffi::OsString, fs, time::Duration};
fn zero(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}
fn full(ctx: &Context<'_>, reason: Option<&str>) -> Output {
    gate(ctx, None, None, reason)
}
fn gate(ctx: &Context<'_>, base: Option<&str>, head: Option<&str>, reason: Option<&str>) -> Output {
    let script = ctx.source.join("scripts/check.sh");
    if fs::symlink_metadata(ctx.source.join(".git")).is_err() || !script.is_file() {
        return fail(
            "pre-push: the pushed worktree has no Git root or scripts/check.sh; gate unproved",
        );
    }
    let mut argv = vec![OsString::from("bash"), script.into_os_string()];
    if let (Some(base), Some(head)) = (base, head) {
        argv.extend([
            "--quick".into(),
            "--changed-from".into(),
            base.into(),
            "--changed-to".into(),
            head.into(),
        ]);
    }
    match invocation(
        argv,
        None,
        ctx.source,
        Duration::from_secs(24 * 3600),
        64 * INPUT_MAX,
        ctx,
    ) {
        Ok(p) => {
            let mut err = reason
                .map(|r| format!("pre-push: {r}; running the full local gate\n").into_bytes())
                .unwrap_or_default();
            err.extend(p.stderr);
            (
                if p.complete {
                    p.code
                } else if p.code == 0 {
                    1
                } else {
                    p.code
                },
                p.stdout,
                err,
            )
        }
        Err(e) => fail(format!("pre-push: check runner unavailable ({e})")),
    }
}
/// `pre-merge-commit`: a real merge commit on main/master is refused unless
/// the landing workflow is running. Other branches merge freely.
fn pre_merge(land_active: bool, ctx: &Context<'_>) -> Output {
    if land_active || !on_protected_branch(ctx.cwd) {
        return out("");
    }
    fail(
        "pre-merge: merging into main/master outside the landing gate is refused\nuse `oms land --pr` or `oms land --local-merge --from <branch>`",
    )
}
pub fn run(args: &[OsString], raw: &[u8], ctx: &Context<'_>) -> Output {
    if args.first().is_some_and(|a| a == "--pre-merge") {
        return pre_merge(std::env::var("OMS_LAND_ACTIVE").as_deref() == Ok("1"), ctx);
    }
    // githooks(5): pre-push runs in the worktree being pushed, not the OMS install source.
    let work = root(ctx.cwd, ctx.cwd)
        .filter(|r| fs::symlink_metadata(r.join(".git")).is_ok())
        .unwrap_or_else(|| ctx.cwd.to_owned());
    let ctx = &Context {
        snapshot: ctx.snapshot,
        provider: ctx.provider,
        cwd: ctx.cwd,
        source: &work,
        interrupted: ctx.interrupted,
        host: ctx.host,
    };
    let full_mode = args.first().is_some_and(|a| a == "--full");
    let index = usize::from(full_mode);
    let remote = args.get(index).and_then(|s| s.to_str()).unwrap_or("origin");
    let remote_valid = git(
        ctx.source,
        &["check-ref-format", &format!("refs/remotes/{remote}/probe")],
        ctx,
    )
    .is_some();
    let refs = if remote_valid {
        git(
            ctx.source,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                &format!("refs/remotes/{remote}/"),
            ],
            ctx,
        )
    } else {
        None
    };
    let mut tag_only = refs.is_some();
    let refs = refs.unwrap_or_default();
    let mut records = 0;
    let mut updates = Vec::new();
    let mut malformed = false;
    for line in String::from_utf8_lossy(raw).lines() {
        records += 1;
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 {
            malformed = true;
            tag_only = false;
            continue;
        }
        let (local_ref, local_oid, remote_ref, remote_oid) =
            (fields[0], fields[1], fields[2], fields[3]);
        if !local_ref.starts_with("refs/tags/") || !remote_ref.starts_with("refs/tags/") {
            tag_only = false;
        }
        if zero(local_oid) {
            tag_only = false;
            continue;
        }
        if !zero(remote_oid) && remote_oid != local_oid {
            tag_only = false;
        }
        if tag_only {
            let commit = git(
                ctx.source,
                &["rev-parse", "--verify", &format!("{local_oid}^{{commit}}")],
                ctx,
            );
            let published = commit.as_ref().is_some_and(|commit| {
                refs.lines().any(|tip| {
                    git(
                        ctx.source,
                        &["merge-base", "--is-ancestor", commit, tip],
                        ctx,
                    )
                    .is_some()
                })
            });
            if !published {
                tag_only = false;
            }
        }
        updates.push((
            local_ref.to_owned(),
            local_oid.to_owned(),
            remote_ref.to_owned(),
            remote_oid.to_owned(),
        ));
    }
    if records > 0 && tag_only {
        return out("pre-push: tag-only push of published commits; gate skipped\n");
    }
    if !malformed && updates.is_empty() && (records > 0 || !full_mode) {
        return out("pre-push: deletion-only update; no local gate needed\n");
    }
    if full_mode || malformed {
        return full(ctx, None);
    }
    if updates.len() != 1 {
        return full(ctx, Some("multiple refs are being pushed"));
    }
    let (local_ref, local_oid, remote_ref, remote_oid) = &updates[0];
    if !local_ref.starts_with("refs/heads/") {
        return full(ctx, Some("a non-branch ref is being pushed"));
    }
    if !remote_ref.starts_with("refs/heads/") {
        return full(ctx, Some("the destination is not a branch ref"));
    }
    if git(
        ctx.source,
        &["cat-file", "-e", &format!("{local_oid}^{{commit}}")],
        ctx,
    )
    .is_none()
    {
        return full(ctx, Some("the pushed commit is unavailable locally"));
    }
    if git(ctx.source, &["rev-parse", "HEAD"], ctx).as_deref() != Some(local_oid) {
        return full(ctx, Some("the pushed tip is not the checked-out HEAD"));
    }
    if git(ctx.source, &["diff", "--quiet", local_oid, "--"], ctx).is_none() {
        return(1,vec![],b"pre-push: quick mode requires a clean tracked working tree\ncommit or stash tracked changes so the checked files equal the pushed commit\n".to_vec());
    }
    let base = if !zero(remote_oid) {
        if git(
            ctx.source,
            &["cat-file", "-e", &format!("{remote_oid}^{{tree}}")],
            ctx,
        )
        .is_none()
        {
            return full(ctx, Some("the remote base is unavailable locally"));
        }
        remote_oid.clone()
    } else {
        let main = format!("refs/remotes/{remote}/main");
        let base = if git(ctx.source, &["show-ref", "--verify", "--quiet", &main], ctx).is_some() {
            git(ctx.source, &["merge-base", local_oid, &main], ctx)
        } else {
            None
        };
        base.unwrap_or_else(|| {
            git(ctx.source, &["hash-object", "-t", "tree", "/dev/null"], ctx).unwrap_or_default()
        })
    };
    if base.is_empty() {
        return full(ctx, Some("the remote base is unavailable locally"));
    }
    let mut result = gate(ctx, Some(&base), Some(local_oid), None);
    let mut output =
        b"pre-push: quick local checks only; the full GitHub Actions gate remains required\n"
            .to_vec();
    output.extend(result.1);
    result.1 = output;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn merge(head: &str, land_active: bool) -> Output {
        let dir = std::env::temp_dir().join(format!("oms-premerge-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), head).unwrap();
        let (snapshot, stop) = (Snapshot::default(), AtomicBool::new(false));
        let ctx = Context {
            snapshot: &snapshot,
            provider: None,
            cwd: &dir,
            source: &dir,
            interrupted: &stop,
            host: &crate::NoHostServices,
        };
        let result = pre_merge(land_active, &ctx);
        std::fs::remove_dir_all(dir).unwrap();
        result
    }

    #[test]
    fn pre_merge_refuses_main_outside_a_landing_run_only() {
        let refused = merge("ref: refs/heads/main\n", false);
        assert_ne!(refused.0, 0);
        assert!(String::from_utf8_lossy(&refused.2).contains("oms land --local-merge"));
        assert_eq!(merge("ref: refs/heads/main\n", true).0, 0);
        assert_eq!(merge("ref: refs/heads/feat\n", false).0, 0);
    }
}
