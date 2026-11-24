//! `land reconcile`: observation-only recovery for a pull-request landing whose
//! exact reviewed head was merged after the delivery ancestor guard stopped a
//! rerun. It reads the saved receipt, GitHub, the live target and CI; it never
//! writes receipts, refs, status, installs, claims, pushes or merges.
use crate::{
    Context, Output, Result, V, ci, cli::Options, delivery, git::Git, hex, obj, require, string,
};
use std::{path::Path, time::Duration};

pub(super) fn matches_request(head: &str, stamp: &str) -> bool {
    stamp
        .strip_prefix(head)
        .and_then(|s| s.strip_prefix('-'))
        .is_some_and(|s| hex(s, 24))
}

pub(super) fn read_private(root: &oms_safe_fs::Root, name: &str) -> Option<Vec<u8>> {
    root.read_no_follow(
        Path::new(name),
        oms_safe_fs::ByteReadOptions {
            max_bytes: 64 * 1024,
            missing_ok: false,
        },
    )
    .ok()
}

pub(super) fn matches_checkout(row: &V, head: &str, stamp: &str, repo_digest: &str) -> bool {
    row.get("schema") == &1usize.into()
        && row.get("sha").text() == Some(head)
        && row.get("request").text() == Some(stamp)
        && row.get("repo").text() == Some(repo_digest)
}

const ALLOWED: [&str; 6] = [
    "repo",
    "remote",
    "target",
    "reviewed-head",
    "request",
    "json",
];

struct Refusal {
    verdict: &'static str,
    reason: &'static str,
}
macro_rules! need {
    ($cond:expr, $verdict:expr, $reason:expr) => {
        if !$cond {
            return Ok(Err(Refusal {
                verdict: $verdict,
                reason: $reason,
            }));
        }
    };
}
macro_rules! tri {
    ($c:expr, $e:expr, $verdict:expr, $reason:expr) => {
        match $e {
            Ok(v) => v,
            Err(_) => {
                crate::check($c.interrupted)?;
                return Ok(Err(Refusal {
                    verdict: $verdict,
                    reason: $reason,
                }));
            }
        }
    };
}

pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(
        o.words.len() == 1 && o.values.keys().all(|k| ALLOWED.contains(&k.as_str())),
        "land reconcile accepts only --repo, --remote, --target, --reviewed-head, --request and --json",
    )?;
    let head = o.get("reviewed-head");
    require(
        hex(head, 40) || hex(head, 64),
        "--reviewed-head must be a full lowercase commit id",
    )?;
    require(
        !o.has("request") || matches_request(head, o.get("request")),
        "--request must be <reviewed-head>-<24 hex>",
    )?;
    let remote = o.value("remote", "origin");
    require(
        !remote.is_empty()
            && !remote.starts_with('-')
            && remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid remote name",
    )?;
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    let target = o.value("target", "main");
    require(target.len() <= 255, "target branch name is too long")?;
    git.run(&["check-ref-format", &format!("refs/heads/{target}")])?;
    let observed = observe(c, o, &git, &repo, head, remote, target)?;
    let (mut row, code) = match observed {
        Ok(row) => (row, 0),
        Err(r) => (
            obj([
                ("verdict", r.verdict.into()),
                ("reason", r.reason.into()),
                ("reviewed_head", head.into()),
            ]),
            1,
        ),
    };
    row.set("schema", 1usize);
    row.set("kind", "oms-land-reconcile-v1");
    row.set("writes", "none");
    let mut out = if o.has("json") {
        Output::json(&row)?
    } else {
        Output::text(format!(
            "land reconcile: {} ({})\n  reviewed head: {head}\n",
            string(&row, "verdict"),
            string(&row, "reason")
        ))
    };
    out.code = code;
    Ok(out)
}

fn observe(
    c: &Context<'_>,
    o: &Options,
    git: &Git<'_>,
    repo: &Path,
    head: &str,
    remote: &str,
    target: &str,
) -> Result<std::result::Result<V, Refusal>> {
    const R: &str = "refused";
    const U: &str = "unverified";
    tri!(c, git.transport_safe(), R, "repo-context-unsafe");
    let destination = tri!(
        c,
        git.text(&["remote", "get-url", "--push", "--all", remote]),
        R,
        "destination-unreadable"
    );
    need!(destination.lines().count() == 1, R, "destination-ambiguous");
    let slug = tri!(c, ci::query_repo(&destination), R, "destination-invalid");
    let Some(repository) = slug.strip_prefix("github.com/") else {
        return Ok(Err(Refusal {
            verdict: R,
            reason: "destination-not-github",
        }));
    };
    let canonical = [
        "https://github.com/",
        "ssh://git@github.com/",
        "git@github.com:",
    ]
    .iter()
    .any(|prefix| {
        destination
            .strip_prefix(prefix)
            .is_some_and(|path| path == repository || path.strip_suffix(".git") == Some(repository))
    });
    need!(canonical, R, "destination-not-github");
    let directory = tri!(c, delivery::state_directory(git), R, "state-unavailable");
    let root = tri!(
        c,
        oms_safe_fs::Root::open_private_workspace(&directory, |_| false),
        R,
        "receipt-store-untrusted"
    );
    let stamp = if o.has("request") {
        o.get("request").to_owned()
    } else {
        let mut found = None;
        let entries = tri!(c, std::fs::read_dir(&directory), R, "receipt-missing");
        for (index, entry) in entries.enumerate() {
            crate::check(c.interrupted)?;
            need!(index < 2048, U, "receipt-scan-limit-use-request");
            let entry = tri!(c, entry, U, "receipt-scan-unreadable");
            if let Some(stem) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                && matches_request(head, stem)
            {
                need!(found.is_none(), R, "receipt-ambiguous");
                found = Some(stem.to_owned());
            }
        }
        need!(found.is_some(), R, "receipt-missing");
        found.unwrap()
    };
    let name = format!("{stamp}.json");
    let raw = tri!(
        c,
        read_private(&root, &name).ok_or(()),
        R,
        "receipt-untrusted"
    );
    let row = tri!(c, V::parse(&raw), R, "receipt-invalid");
    let gate = row.get("gate").get("command").text().unwrap_or_default();
    let head_branch = string(&row, "head_branch");
    let full_gate = oms_command_plan::floor::split(gate, false)
        .is_ok_and(|words| words == ["bash", "scripts/check.sh", "--parallel"]);
    need!(
        matches_checkout(
            &row,
            head,
            &stamp,
            &crate::digest(repo.as_os_str().as_encoded_bytes())
        ) && string(&row, "remote") == remote
            && string(&row, "target") == target
            && string(&row, "mode") == "pr"
            && row.get("gate").get("rc") == &0usize.into()
            && matches!(string(&row, "state").as_str(), "awaiting-merge" | "passed")
            && row.get("push").get("rc") == &0usize.into()
            && row.get("push").get("head_branch").text() == Some(head_branch.as_str())
            && crate::helpers::recorded_base(&row).ok().flatten().is_some()
            && row.get("gate").get("scope").text()
                == Some(if full_gate { "full" } else { "unknown" })
            && !gate.trim().is_empty()
            && gate.len() <= 4096
            && crate::safe(gate.as_bytes()).is_ok(),
        R,
        "receipt-identity-mismatch"
    );
    need!(
        head_branch.len() <= 255
            && !head_branch.is_empty()
            && git
                .succeeds(&["check-ref-format", &format!("refs/heads/{head_branch}")])
                .unwrap_or(false),
        R,
        "receipt-head-branch-invalid"
    );
    let expected = tri!(
        c,
        delivery::request_stamp(
            head,
            &[
                destination.trim(),
                remote,
                target,
                gate,
                "pr",
                &head_branch,
                ""
            ],
        ),
        R,
        "receipt-identity-mismatch"
    );
    need!(expected == stamp, R, "request-identity-mismatch");
    let pr_url = string(&row, "pr_url");
    need!(
        row.get("pr_number").integer(true) && !pr_url.is_empty(),
        R,
        "receipt-pr-missing"
    );
    let number = row.get("pr_number").py();

    need!(
        number.parse::<u64>().is_ok_and(|n| n > 0)
            && pr_url == format!("https://github.com/{repository}/pull/{number}"),
        R,
        "pr-identity-mismatch"
    );
    let view = tri!(
        c,
        ci::gh(
            c,
            &[
                "pr",
                "view",
                &number,
                "--repo",
                &slug,
                "--json",
                "number,state,url,headRefOid,headRefName,baseRefName,isCrossRepository,mergeCommit,statusCheckRollup",
            ],
            Duration::from_secs(30),
        ),
        U,
        "github-unreadable"
    );
    need!(
        view.get("number").py() == number && string(&view, "url") == pr_url,
        R,
        "pr-identity-mismatch"
    );
    need!(
        view.get("isCrossRepository") == &V::Bool(false),
        R,
        "pr-foreign-repository"
    );
    need!(string(&view, "state") == "MERGED", R, "pr-not-merged");
    need!(string(&view, "headRefOid") == head, R, "pr-head-moved");
    need!(
        string(&view, "headRefName") == head_branch && string(&view, "baseRefName") == target,
        R,
        "pr-ref-mismatch"
    );
    let merge = view
        .get("mergeCommit")
        .get("oid")
        .text()
        .unwrap_or_default()
        .to_owned();
    need!(
        hex(&merge, 40) || hex(&merge, 64),
        R,
        "merge-commit-missing"
    );

    let tip = tri!(
        c,
        crate::review::remote_tip(git, &destination, target),
        U,
        "target-unreadable"
    );
    let Some(tip) = tip else {
        return Ok(Err(Refusal {
            verdict: R,
            reason: "target-absent",
        }));
    };
    let have = |sha: &str| {
        git.succeeds(&[
            "--no-replace-objects",
            "cat-file",
            "-e",
            &format!("{sha}^{{commit}}"),
        ])
        .unwrap_or(false)
    };
    need!(
        have(&merge) && have(&tip) && have(head),
        U,
        "objects-not-local"
    );
    if tip != merge {
        // Traverse physical commits, regardless of local replacement refs or grafts.
        let ancestry = tri!(
            c,
            git.output(
                &[
                    "--no-replace-objects".as_ref(),
                    "merge-base".as_ref(),
                    "--is-ancestor".as_ref(),
                    merge.as_ref(),
                    tip.as_ref()
                ],
                None,
                &[("GIT_GRAFT_FILE".into(), "/dev/null".into())]
            ),
            U,
            "reachability-unreadable"
        );
        need!(matches!(ancestry.code, 0 | 1), U, "reachability-unreadable");
        need!(ancestry.code == 0, R, "merge-not-on-target");
    }
    let tree = |sha: &str| {
        git.text(&[
            "--no-replace-objects",
            "rev-parse",
            "--verify",
            &format!("{sha}^{{tree}}"),
        ])
    };
    let merge_tree = tri!(c, tree(&merge), U, "tree-unreadable");
    let head_tree = tri!(c, tree(head), U, "tree-unreadable");

    need!(merge_tree == head_tree, U, "merged-tree-differs");
    let evidence = tri!(
        c,
        ci::reconciled_pr(
            c,
            repository,
            &number,
            head,
            &head_branch,
            target,
            view.get("statusCheckRollup")
        ),
        U,
        "pr-ci-unverified"
    );
    let evidence = match evidence {
        ci::ReconciledCi::Full(evidence) => evidence,
        ci::ReconciledCi::Pending => {
            return Ok(Err(Refusal {
                verdict: U,
                reason: "pr-ci-pending",
            }));
        }
        ci::ReconciledCi::Unsuccessful => {
            return Ok(Err(Refusal {
                verdict: R,
                reason: "pr-ci-not-successful",
            }));
        }
    };
    Ok(Ok(obj([
        ("verdict", "verified".into()),
        ("reason", "same-tree".into()),
        ("reviewed_head", head.into()),
        ("request", stamp.into()),
        ("repository", repository.into()),
        ("target", target.into()),
        ("reviewed_tree", head_tree.into()),
        (
            "pr",
            obj([
                ("number", row.get("pr_number").clone()),
                ("state", "MERGED".into()),
            ]),
        ),
        ("merge_commit", merge.into()),
        ("target_tip", tip.into()),
        ("tree_equal", true.into()),
        ("local_gate_scope", row.get("gate").get("scope").clone()),
        ("ci", evidence),
    ])))
}
