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
                "check-manifest",
                "manifest-receipt",
            ],
            &[
                "pr",
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
    if tool == "land" {
        require(
            !(out.has("pr") && out.has("local-merge")),
            "--pr and --local-merge are mutually exclusive",
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
            out.has("check-manifest") == out.has("manifest-receipt"),
            "--check-manifest and --manifest-receipt PATH are required together",
        )?;
        require(
            !out.has("execute-manifest") || out.has("check-manifest"),
            "--execute-manifest requires --check-manifest",
        )?;
        require(
            !out.has("check-manifest")
                || (out.words.is_empty() && !out.has("pr") && !out.has("local-merge")),
            "--check-manifest cannot be combined with a land action, --pr or --local-merge",
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
            "Usage: oms land [--repo PATH] [--remote NAME] [--target BRANCH] [--gate CMD] [--wait] [--ci-wait SECONDS] [--ci-full] [--no-update] [--sibling-wait SECONDS] [--ignore-siblings] [--handoff-receipt-to PATH] [--sync-shared PATH]\n       oms land --pr [--head-branch NAME] [--merge-method squash|merge|rebase] [land options]\n       oms land --local-merge --from REF [land options]\n       (OMS_LAND_MODE=pr|auto|direct selects the mode when neither --pr nor --local-merge is given)\n       oms land --check-manifest FILE --manifest-receipt PATH [--execute-manifest]  (multi-repo: verify, or fast-forward with --execute-manifest; exit 3 = partial hold)\n       oms land status [--repo PATH] [--json] [--active-exit]\n       oms land notes --from REV --to REV [--repo PATH] [--output NEW_FILE]\n       oms land blame-session [--repo PATH] [--json] [--file PATH] [--export-notes] COMMIT_OR_RANGE\n       oms land conflicts [--repo PATH] [--base REV] [--max N] [--json] [BRANCH...]\n"
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
            "Usage: oms ci-status [status|record|tick|history|flaky|protection|triage|suggest|feedback] [branch] [--limit N] [--sha COMMIT] [--json] [--refresh]\n       feedback [--json] reads durable untrusted PR/CI observations; record/tick refresh them.\n"
        }
        "report" => {
            "Usage: oms report <bug|feature> [--from-failure FINGERPRINT] [--title TEXT] [--message TEXT] [--dry-run] [--yes]\n"
        }
        _ => return None,
    })
}
