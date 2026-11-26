use crate::{Error, Result, require};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub(crate) struct Args {
    pub action: String,
    pub values: BTreeMap<String, OsString>,
    pub flags: BTreeSet<String>,
}
impl Args {
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
    pub fn has(&self, name: &str) -> bool {
        self.flag(name) || self.values.contains_key(name)
    }
    pub fn text(&self, name: &str) -> Result<&str> {
        self.values
            .get(name)
            .map(|s| {
                s.to_str()
                    .ok_or_else(|| Error::new(2, format!("--{name} requires UTF-8 text")))
            })
            .transpose()
            .map(|s| s.unwrap_or(""))
    }
    pub fn get<'a>(&'a self, name: &str, default: &'a str) -> Result<&'a str> {
        let s = self.text(name)?;
        Ok(if self.has(name) { s } else { default })
    }
    pub fn required(&self, name: &str) -> Result<&str> {
        let s = self.text(name)?;
        require(!s.is_empty(), format!("--{name} is required"))?;
        Ok(s)
    }
    pub fn path(&self, name: &str, default: &Path) -> PathBuf {
        self.values
            .get(name)
            .map(PathBuf::from)
            .unwrap_or_else(|| default.into())
    }
    pub fn number(&self, name: &str, default: u64, low: u64, high: u64) -> Result<u64> {
        if !self.has(name) {
            return Ok(default);
        }
        let raw = self.required(name)?;
        require(
            raw.bytes().all(|b| b.is_ascii_digit()),
            format!("--{name} must be {low}..{high}"),
        )?;
        raw.parse::<u64>()
            .ok()
            .filter(|n| (low..=high).contains(n))
            .ok_or_else(|| Error::new(2, format!("--{name} must be {low}..{high}")))
    }
    pub fn set(&mut self, name: &str, value: impl Into<OsString>) {
        self.values.insert(name.into(), value.into());
    }
    pub fn parse(tool: &str, raw: &[OsString]) -> Result<Self> {
        let (actions, values, flags): (&[&str], &[&str], &[&str]) = match tool {
            "intent" => (
                &["draft", "show", "adopt", "amend", "issues"],
                &[
                    "repo",
                    "to",
                    "goal",
                    "id",
                    "reason",
                    "kind",
                    "expected-project-sha256",
                    "model",
                    "fallback-model",
                    "fallback-to",
                    "reasoning-effort",
                    "provider-timeout",
                    "from-file",
                    "from-issue",
                    "from-project-issue",
                    "source-sha256",
                    "github",
                    "label",
                    "limit",
                ],
                &["dry-run", "json"],
            ),
            "plan-from-spec" => (
                &[],
                &[
                    "repo",
                    "to",
                    "worker-provider",
                    "collaboration",
                    "council-providers",
                    "council-rounds",
                    "max-tasks",
                    "id-prefix",
                    "allowed",
                    "model",
                    "fallback-model",
                    "reasoning-effort",
                    "provider-timeout",
                    "apply",
                    "validate-spec",
                    "milestone",
                    "expected-proposal-sha256",
                    "issue",
                    "issue-source-sha256",
                    "restamp",
                ],
                &[
                    "with-issue-requirements",
                    "allow-verifier-change",
                    "discover",
                    "scope-only",
                    "roadmap",
                ],
            ),
            "plan-run" => (
                &[],
                &[
                    "repo",
                    "to",
                    "id",
                    "expected-task-sha256",
                    "repair",
                    "allow-floor-change",
                    "model",
                    "fallback-model",
                    "reasoning-effort",
                    "context-pack",
                    "expected-context-pack-sha256",
                    "max-writers",
                    "max-review-backlog",
                    "base-ref",
                    "base-sha",
                    "candidates",
                    "verify-cmd",
                ],
                &[
                    "next",
                    "parallel",
                    "verify-combined",
                    "land",
                    "auto-repair",
                    "allow-verifier-change",
                    "retry-known",
                    "dry-run",
                ],
            ),
            "autopilot" => (
                &[
                    "propose", "run", "status", "abandon", "reenter", "shadow", "decide",
                ],
                &[
                    "repo",
                    "proposal",
                    "expected-proposal-sha256",
                    "planner",
                    "worker",
                    "reviewer",
                    "allowed",
                    "base",
                    "remote",
                    "max-cycles",
                    "initial-tasks",
                    "replan-tasks",
                    "milestones",
                    "expected-decision-sha256",
                    "action",
                    "wait-binding",
                    "expected-wait-sha256",
                    "allow-floor-change",
                    "provider-timeout",
                    "planner-timeout",
                    "worker-timeout",
                    "reviewer-timeout",
                    "planner-model",
                    "worker-model",
                    "reviewer-model",
                    "planner-fallback-model",
                    "worker-fallback-model",
                    "reviewer-fallback-model",
                    "planner-reasoning-effort",
                    "worker-reasoning-effort",
                    "reviewer-reasoning-effort",
                    "collaboration",
                    "council-providers",
                    "council-rounds",
                    "review-mode",
                    "reason",
                    "session",
                    "issue",
                    "issue-source-sha256",
                    "restamp",
                ],
                &[
                    "with-issue-requirements",
                    "auto-repair",
                    "retry-known",
                    "allow-verifier-change",
                    "discover",
                    "draft-pr",
                ],
            ),
            "goal-drive" => (
                &[],
                &[
                    "repo",
                    "to",
                    "max-cycles",
                    "run-id",
                    "contract-id",
                    "allow-floor-change",
                    "model",
                    "fallback-model",
                    "reasoning-effort",
                    "provider-timeout",
                    "expected-ref",
                    "campaign",
                    "commit-task",
                    "commit-message",
                ],
                &[
                    "auto-repair",
                    "retry-known",
                    "allow-verifier-change",
                    "dry-run",
                    "status",
                ],
            ),
            "install-plan" => (&[], &[], &["install", "update"]),
            _ => return Err(Error::new(2, "unknown autonomy command")),
        };
        let mut out = Self::default();
        let mut at = 0;
        while at < raw.len() {
            let word = raw[at]
                .to_str()
                .ok_or_else(|| Error::new(2, "non-text option"))?;
            if matches!(word, "-h" | "--help") {
                out.flags.insert("help".into());
                at += 1;
                continue;
            }
            if let Some(key) = word.strip_prefix("--") {
                require(!out.has(key), format!("{word} given twice"))?;
                if flags.contains(&key) {
                    out.flags.insert(key.into());
                    at += 1;
                    continue;
                }
                require(values.contains(&key), format!("unknown argument: {word}"))?;
                let next = raw
                    .get(at + 1)
                    .ok_or_else(|| Error::new(2, format!("{word} requires a value")))?;
                require(
                    !next.is_empty(),
                    format!("{word} requires a nonempty value"),
                )?;
                out.values.insert(key.into(), next.clone());
                at += 2;
            } else {
                require(
                    actions.contains(&word) && out.action.is_empty(),
                    format!("unknown or multiple actions: {word}"),
                )?;
                out.action = word.into();
                at += 1;
            }
        }
        if !out.flag("help") && !actions.is_empty() {
            require(!out.action.is_empty(), "an action is required")?;
        }
        Ok(out)
    }
}
pub(crate) fn usage(tool: &str) -> &'static str {
    match tool {
        "intent" => {
            "Usage: oms intent draft --goal TEXT [--kind feature|bugfix|quick] [--to PROVIDER]\n       oms intent draft (--from-file PATH | --from-issue URL) [--dry-run | --source-sha256 SHA]\n       oms intent draft --from-project-issue N --dry-run\n       oms intent issues --github OWNER/REPO --label LABEL [--limit 1..20] [--json]\n       oms intent show [--id ID]\n       oms intent amend --reason TEXT\n       oms intent adopt --id ID [--expected-project-sha256 SHA]\nCandidates are untrusted drafts. Only explicit adopt writes PROJECT.md; a live autopilot receipt refuses a contract swap."
        }
        "plan-from-spec" => {
            "Usage: oms plan-from-spec [--repo PATH] [--to PROVIDER] [--max-tasks 1..12]\n       oms plan-from-spec --apply FILE --expected-proposal-sha256 SHA\n       oms plan-from-spec --restamp FILE --expected-proposal-sha256 SHA --issue REF\n       oms plan-from-spec --validate-spec FILE | --scope-only | --roadmap\nOptions: --allowed PATHS --id-prefix PREFIX --milestone ID --discover\n         --worker-provider PROVIDER --collaboration off|auto\n         --council-providers P,P --council-rounds 0..3 (auto only; defaults codex,claude,antigravity and 1)\n         --model MODEL --fallback-model MODEL --reasoning-effort E --provider-timeout DUR\n         --issue REF (N, #N or owner/repo#N; same repository as origin) [--issue-source-sha256 SHA]\n         --with-issue-requirements opts in to sending that issue title and body to the selected planner and any configured council.\n         Preview a Project issue source pin with intent draft --from-project-issue N --dry-run. First apply rechecks OPEN and exact source; complete replay is local.\nGenerated proposals never apply themselves. Apply binds exact reviewed bytes, plan CAS, base, spec and acceptance surface.\nThe harness stamps one issue on the proposal and every task: --issue, else the adopted intent's issue while PROJECT.md is unchanged. GitHub project mode refuses to apply an issue-less proposal; --restamp writes a new stamped copy locally."
        }
        "plan-run" => {
            "Usage: oms plan-run --to PROVIDER (--id ID | --next) [--repo PATH]\n       oms plan-run --to PROVIDER --parallel [--max-writers 1..16] [--max-review-backlog 1..64] [--repo PATH]\n       oms plan-run --verify-combined --base-ref REF --base-sha SHA --candidates SHAS [--verify-cmd CMD] [--repo PATH]\nOptions: --repair 0..3 --land --auto-repair --retry-known --dry-run\n         --expected-task-sha256 SHA (serial --id only; requires exact ready task before any reclaim or claim)\n         --model MODEL --fallback-model MODEL --reasoning-effort E (auto|low|medium|high|xhigh|max|ultra)\n         --allow-verifier-change --allow-floor-change REASON --context-pack FILE\n         --expected-context-pack-sha256 SHA (pin supplied pack bytes; required for reviewed Lead findings)\nReviewed Lead findings require serial --id, --expected-task-sha256 and the original Room; they grant no scope or model override.\nRuns one preauthorized task; stops in review unless --land explicitly requests admission. Never commits or publishes.\n--parallel (opt-in; one controller per state repository) keeps up to --max-writers (default 2) non-overlapping ready tasks running, refilling each slot as a task ends. Failures release only exact unstarted claims. An unconfirmed running lease needs reconciliation. Landing is serialized. Exit 3: review backlog reached --max-review-backlog and no new task was claimed; 1: a task failed; 130: interrupted; 75: another controller holds the lock. --id and --dry-run are refused."
        }
        "autopilot" => {
            "Usage: oms autopilot [options] propose|run|status|abandon|reenter|shadow|decide\nOptions: --proposal FILE --expected-proposal-sha256 SHA --repo PATH\n         --planner P --worker P --reviewer P --allowed PATHS --base BRANCH --remote NAME\n         --max-cycles 1..10 --initial-tasks 1..12 --replan-tasks 1..2 --milestones ID,ID\n         --auto-repair --retry-known --allow-verifier-change --allow-floor-change REASON\n         --review-mode shadow|gate|off --collaboration off|auto --draft-pr --discover\n         --council-providers P,P --council-rounds 0..3 (auto only; frozen through recovery)\n         --provider-timeout DUR --ROLE-timeout DUR --ROLE-model MODEL\n         --ROLE-fallback-model MODEL --ROLE-reasoning-effort E --reason TEXT --session ID\n         propose --issue REF [--issue-source-sha256 SHA [--with-issue-requirements]]; propose --restamp FILE --expected-proposal-sha256 SHA --issue REF [--issue-source-sha256 SHA]\n         rebinds a proposal-review receipt to a stamped copy and clears blocked: github-issue\n         decide --expected-decision-sha256 SHA --action inspect|propose-revision|wait|resume|stop\n         wait requires --wait-binding FILE --expected-wait-sha256 SHA\nStatus returns a checked next action and exact continuation. Parent agents inspect evidence and review proposals internally; ordinary transitions require no new user approval. Milestones are an initial authorized PROJECT list and share the same owner and lifetime budget.\nExit 4 requires exact proposal review; unsafe stops exit 3. Frozen contracts, parks and exhausted lifetime budgets cannot silently reset."
        }
        "goal-drive" => {
            "Usage: oms goal-drive [--repo PATH] [--to PROVIDER] [--max-cycles 1..10]\nOptions: --run-id ID --contract-id OWNER_ID --expected-ref REF --auto-repair\n         --retry-known --allow-verifier-change --allow-floor-change REASON\n         --model MODEL --fallback-model MODEL --reasoning-effort E --provider-timeout DUR\n         --campaign FILE [--dry-run|--status]\n         --commit-task ID [--commit-message TEXT]\nDrive an existing approved plan with fresh acceptance and bounded attempts. Every terminal writes a progress receipt; unsafe stops park."
        }
        _ => {
            "Usage: oms install-plan --install | --update\nWrite-free preview. STEP: STATUS: detail; statuses: change, current, skip, not-previewed, note. Unavailable previews are reported, never guessed."
        }
    }
}
