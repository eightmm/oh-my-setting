use crate::{Result, require};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Default, Debug)]
pub(crate) struct Options {
    pub values: BTreeMap<String, Vec<OsString>>,
    pub words: Vec<String>,
}
impl Options {
    pub fn has(&self, k: &str) -> bool {
        self.values.contains_key(k)
    }
    pub fn get(&self, k: &str) -> &str {
        self.values
            .get(k)
            .and_then(|v| v.last())
            .and_then(|v| v.to_str())
            .unwrap_or("")
    }
    pub fn value<'a>(&'a self, k: &str, default: &'a str) -> &'a str {
        if self.has(k) { self.get(k) } else { default }
    }
    pub fn all(&self, k: &str) -> Vec<String> {
        self.values
            .get(k)
            .into_iter()
            .flatten()
            .map(|v| v.to_string_lossy().into_owned())
            .collect()
    }
    pub fn path(&self, k: &str, cwd: &Path) -> Option<PathBuf> {
        self.values.get(k).and_then(|v| v.last()).map(|v| {
            let p = Path::new(v);
            if p.is_absolute() {
                p.to_owned()
            } else {
                cwd.join(p)
            }
        })
    }
    pub fn action<'a>(&'a self, default: &'a str) -> &'a str {
        self.words.first().map(String::as_str).unwrap_or(default)
    }
}
pub(crate) fn parse(tool: &str, args: &[OsString]) -> Result<Options> {
    let (valued, flags): (&[&str], &[&str]) = match tool {
        "patch-admit" => (
            &[
                "repo",
                "patch",
                "verify",
                "regression-test",
                "report",
                "plan-task",
                "covers",
                "allow-floor-change",
                "red-check",
            ],
            &[
                "ml",
                "keep-worktree",
                "allow-verifier-change",
                "allow-test-reduction",
                "allow-restructure",
                "accept-encoding-loss",
            ],
        ),
        "patch-land" => (
            &[
                "repo",
                "patch",
                "verify",
                "plan-task",
                "panel-accept",
                "allow-floor-change",
                "approval",
                "approval-token",
                "approval-version",
            ],
            &[
                "ml",
                "allow-verifier-change",
                "allow-test-reduction",
                "allow-restructure",
                "accept-salvaged",
                "accept-encoding-loss",
                "request-approval",
                "recover",
            ],
        ),
        "land" => (
            &[
                "repo",
                "remote",
                "target",
                "gate",
                "from",
                "to",
                "output",
                "ci-wait",
                "sibling-wait",
                "handoff-receipt-to",
                "sync-shared",
                "base",
                "max",
                "file",
                "head-branch",
                "merge-method",
                "why",
                "check-manifest",
                "manifest-receipt",
                "expected-preview-sha256",
            ],
            &[
                "pr",
                "direct",
                "merge",
                "local-merge",
                "wait",
                "ci-full",
                "no-update",
                "json",
                "active-exit",
                "ignore-siblings",
                "export-notes",
                "execute-manifest",
            ],
        ),
        "change-guard" => (&["repo", "file", "allow", "deny"], &["from-task", "strict"]),
        "checkpoint" => (
            &["repo", "label"],
            &["untracked", "reuse", "untracked-only", "json", "apply"],
        ),
        "candidate" => (
            &[
                "id",
                "kind",
                "ref",
                "smiles",
                "note",
                "tool",
                "metric",
                "value",
                "unit",
                "run",
                "external",
                "conditions",
                "format",
            ],
            &[
                "store-structure",
                "unlinked",
                "higher-is-better",
                "lower-is-better",
                "json",
            ],
        ),
        "scratch-worktree" => (&["repo", "base", "owner-pid"], &["force"]),
        "cleanup" => (&[], &["dry-run", "apply"]),
        "sync-shared" => (&["repo", "remote", "target"], &[]),
        "draft-pr" => (
            &[
                "repo",
                "remote",
                "base",
                "verify",
                "intent",
                "expected-head",
                "expected-tree",
                "expected-base-sha",
                "expected-spec-sha256",
                "review-evidence",
            ],
            &["json"],
        ),
        "ci-status" => (&["limit", "sha"], &["record", "json", "refresh"]),
        "report" => (&["from-failure", "title", "message"], &["dry-run", "yes"]),
        "github" => (
            &["repo", "issue", "pull", "ttl-s", "limit", "expected-sha256"],
            &["json", "linked", "apply", "yes"],
        ),
        _ => return Err(crate::Error::new(2, "unknown command")),
    };
    let mut out = Options::default();
    let mut i = 0;
    while i < args.len() {
        let s = args[i]
            .to_str()
            .ok_or_else(|| crate::Error::new(2, "arguments must be UTF-8 text"))?;
        if s == "--help" || s == "-h" {
            out.values.insert("help".into(), vec![]);
            return Ok(out);
        }
        if let Some(k) = s.strip_prefix("--") {
            if valued.contains(&k) {
                i += 1;
                require(i < args.len(), format!("--{k} requires a value"))?;
                require(
                    !args[i].is_empty(),
                    format!("--{k} requires a nonempty value"),
                )?;
                out.values
                    .entry(k.into())
                    .or_default()
                    .push(args[i].clone());
            } else if flags.contains(&k) {
                out.values.insert(k.into(), vec![]);
            } else {
                return Err(crate::Error::new(2, format!("unknown argument: {s}")));
            }
        } else {
            require(!s.starts_with('-'), format!("unknown argument: {s}"))?;
            out.words.push(s.into());
        }
        i += 1;
    }
    if tool == "patch-land" && out.has("panel-accept") {
        require(
            !out.has("recover") && !out.has("plan-task"),
            "panel acceptance uses its own task and recovery transaction",
        )?;
        require(
            !out.get("panel-accept").is_empty() && out.get("panel-accept").len() <= 1024 * 1024,
            "panel acceptance fence is empty or exceeds its bound",
        )?;
    }
    if tool == "land" {
        let action = out.action("");
        if matches!(action, "batch-preview" | "batch-check") {
            let verify = action == "batch-check";
            require(
                out.values.keys().all(|key| {
                    ["repo", "base", "json"].contains(&key.as_str())
                        || verify && key == "expected-preview-sha256"
                }) && out
                    .values
                    .get("base")
                    .is_some_and(|values| values.len() == 1)
                    && out
                        .values
                        .get("repo")
                        .is_none_or(|values| values.len() == 1),
                if verify {
                    "land batch-check requires one --base and accepts only --expected-preview-sha256, --repo and --json"
                } else {
                    "land batch-preview requires one --base and accepts only --repo and --json"
                },
            )?;
            require(
                !verify
                    || (out
                        .values
                        .get("expected-preview-sha256")
                        .is_some_and(|v| v.len() == 1)
                        && crate::hex(out.get("expected-preview-sha256"), 64)),
                "land batch-check requires one exact --expected-preview-sha256",
            )?;
            return Ok(out);
        }
        require(
            !out.has("expected-preview-sha256"),
            "--expected-preview-sha256 is only accepted by land batch-check",
        )?;
        require(
            ["pr", "direct", "local-merge"]
                .iter()
                .filter(|k| out.has(k))
                .count()
                <= 1,
            "--pr, --direct and --local-merge are mutually exclusive",
        )?;
        require(
            !out.has("local-merge") || out.has("from"),
            "--local-merge requires --from REF",
        )?;
        require(
            !out.has("merge-method")
                || ["squash", "merge", "rebase"].contains(&out.get("merge-method")),
            "--merge-method must be squash, merge or rebase",
        )?;
        require(
            !out.has("merge-method") || out.has("merge"),
            "--merge-method requires --merge",
        )?;
        require(
            out.has("check-manifest") == out.has("manifest-receipt"),
            "--check-manifest and --manifest-receipt PATH are required together",
        )?;
        require(
            !out.has("execute-manifest") || out.has("check-manifest"),
            "--execute-manifest requires --check-manifest",
        )?;
        require(
            !out.has("check-manifest")
                || (out.words.is_empty()
                    && !out.has("pr")
                    && !out.has("direct")
                    && !out.has("local-merge")),
            "--check-manifest cannot be combined with a land action, --pr, --direct or --local-merge",
        )?;
    }
    Ok(out)
}
pub(crate) fn usage(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "patch-admit" => {
            "Usage: oms patch-admit --patch FILE [--repo PATH] [--verify CMD] [--plan-task ID] [--report FILE] [--regression-test CMD] [--red-check CMD] [--covers ID] [--ml] [--keep-worktree] [--allow-verifier-change] [--allow-test-reduction] [--allow-restructure] [--allow-floor-change REASON] [--accept-encoding-loss]\n"
        }
        "patch-land" => {
            "Usage: oms patch-land [--patch FILE | --plan-task ID] [--repo PATH] [--verify CMD] [--recover] [--request-approval | --approval ID --approval-token GRANT --approval-version N] [--ml] [--allow-verifier-change] [--allow-test-reduction] [--allow-restructure] [--allow-floor-change REASON] [--accept-salvaged] [--accept-encoding-loss]\n"
        }
        "land" => {
            "Usage: oms land [--repo PATH] [--remote NAME] [--target BRANCH] [--gate CMD] [--wait] [--ci-wait SECONDS] [--ci-full] [--no-update] [--sibling-wait SECONDS] [--ignore-siblings] [--handoff-receipt-to PATH] [--sync-shared PATH]\n       oms land [--pr] [--why TEXT] [--head-branch NAME] [--merge [--merge-method squash|merge|rebase]] [land options]\n       oms land --direct [land options]\n       oms land --local-merge --from REF [land options]\n       (default: a GitHub destination opens a pull request and leaves it unmerged unless --merge; other destinations push directly. --direct to GitHub is limited to a docs-only change of at most 2 lines. OMS_LAND_MODE=pr|direct selects the mode when no mode flag is given)\n       oms land --check-manifest FILE --manifest-receipt PATH [--execute-manifest]  (multi-repo: verify, or fast-forward with --execute-manifest; exit 3 = partial hold)\n       oms land status [--repo PATH] [--json] [--active-exit]\n       oms land notes --from REV --to REV [--repo PATH] [--output NEW_FILE]\n       oms land blame-session [--repo PATH] [--json] [--file PATH] [--export-notes] COMMIT_OR_RANGE\n       oms land conflicts [--repo PATH] [--base REV] [--max N] [--json] [BRANCH...]\n       oms land batch-preview --base REF REF... [--repo PATH] [--json]
       oms land batch-check --base REF --expected-preview-sha256 SHA256 REF... [--repo PATH] [--json]\n"
        }
        "change-guard" => {
            "Usage: oms change-guard [--repo PATH] [--file PATH] [--allow PATH] [--deny PATH] [--from-task] [--strict] <begin|check|end|status>\n"
        }
        "checkpoint" => {
            "Usage: oms checkpoint [--repo PATH] <create|list|verify|restore> [ID] [--label TEXT] [--untracked] [--reuse] [--untracked-only] [--json] [--apply]\n"
        }
        "candidate" => {
            "Usage: oms candidate <add|result|show|table> [ID] [--id CID] [--kind KIND] [--ref LABEL=VALUE] [--smiles TEXT|-] [--store-structure] [--note TEXT] [--tool NAME] [--metric KEY] [--value NUMBER] [--unit U] [--run ID|--external ID|--unlinked] [--conditions TEXT] [--higher-is-better|--lower-is-better] [--json|--format md|tsv]\n"
        }
        "scratch-worktree" => {
            "Usage: oms scratch-worktree add [--repo PATH] [--base REV] [--owner-pid PID]\n       oms scratch-worktree remove [--repo PATH] [--force] PATH\n       oms scratch-worktree list [--repo PATH]\n"
        }
        "cleanup" => "Usage: oms cleanup [--dry-run|--apply]\n",
        "sync-shared" => "Usage: oms sync-shared [--repo PATH] [--remote NAME] [--target BRANCH]\n",
        "draft-pr" => {
            "Usage: oms draft-pr prepare --base BRANCH --verify COMMAND [--repo PATH] [--remote NAME] [--expected-head SHA] [--expected-tree SHA] [--expected-base-sha SHA] [--expected-spec-sha256 H] [--review-evidence TEXT]\n       oms draft-pr <publish|comments|merged> --intent FILE [--repo PATH]\n"
        }
        "ci-status" => {
            "Usage: oms ci-status [status|record|tick|history|flaky|protection|triage|suggest|feedback|attention] [branch] [--limit N] [--sha COMMIT] [--json] [--refresh]\n       feedback [--json] reads durable untrusted PR/CI observations; record/tick refresh them.\n       attention [branch] [--limit 1..100] [--json] reads recent scheduled/manual runs (default branch unless selected); never records or dispatches.\n"
        }
        "report" => {
            "Usage: oms report <bug|feature> [--from-failure FINGERPRINT] [--title TEXT] [--message TEXT] [--dry-run] [--yes]\n"
        }
        "github" => {
            "Usage: oms github status [--json] [--repo PATH]\n       oms github refresh [--issue REF]... [--pull REF]... [--linked] [--ttl-s N] [--repo PATH]\n       oms github adopt-merged [--limit N] [--apply --expected-sha256 H] [--ttl-s N] [--repo PATH]\n       oms github post-evidence --issue REF --pull REF --yes [--repo PATH]\n\nREF is N, '#N' or owner/repo#N in the origin repository. status is offline:\nproject mode ('- GitHub project: owner/repo' under ## Collaboration in\nPROJECT.md or in .github/oms-project.md as committed at HEAD; both must agree)\nand its source (local|tracked|both), or inactive when only the committed file\nnames a repository other than origin (a fork) or origin is not on github.com.\nA fork opts in only by committing its own declaration; a PROJECT.md naming the\nfork conflicts with the inherited file and is refused. Also shown: origin, the task packet and plan issue links, and cache freshness.\nrefresh and adopt-merged are the only network reads; they validate each\nresponse against the request and append sanitized rows to\n.oms/github/cache.jsonl (title, labels, body only as sha256; TTL 900s).\nadopt-merged previews merged pull requests and caches exactly that selection\nonly with --apply and its selection sha256. post-evidence is the only write:\nwith project mode on and a fresh merged cache row, it records a receipt in\n.oms/github/writes.jsonl, posts one fixed-template comment carrying an\noms-evidence marker unless the authenticated account's own marker comment is\nalready present (a marker by anyone else is ignored), and never reposts.\n"
        }
        _ => return None,
    })
}
