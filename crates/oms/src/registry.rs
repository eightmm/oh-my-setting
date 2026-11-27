//! Read-only root navigation over embedded command descriptions.
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const USAGE: &[u8] = br#"Usage: oms <tool> [args...]
       oms                         # terminal control panel (interactive TTY)
       oms list [--all]
       oms help
       oms --version

Dispatch to an oh-my-setting harness tool by name. `list` prints daily work,
collaboration, delivery and maintenance entrypoints; `list --all` also reveals
optional workflows and lower-level tools. The command-routing reference selects
intent variants behind those entries. docs/COMPONENTS.md
describes the existing subsystems and docs/OMS-RUNTIME.md explains the typed
runtime projection.

The terminal panel is the human entrypoint. The other tools are the agent's
hands; give your coding agent the goal instead of composing harness commands.
"#;

pub(crate) const PUBLIC: &str = "advise\nagent-call\nagent-events\nagent-memory\nagent-plan\nagent-role\nagent-supervisor\nagent-task\napply-project-template\napproval-inbox\nartifact-index\nartifact-view\nauto-update\nautopilot\ncandidate\nchange-guard\ncheckpoint\nci-status\ncite\ncleanup\nconnect-services\nconsult\ndashboard\ndata-manifest\ndeps\ndoctor\ndraft-pr\nexperiment-board\nexternal\nfail-ledger\ngc\ngithub\ngoal-drive\ngraph\nharness\nhud\ninbox\ninit\ninput\ninstall-profile\nintent\njob-digest\njournal\nland\nlease\nlinear\nmodel-doctor\nmodels\noutput-digest\npanel\npatch-admit\npatch-land\npeer-ask\npeer-delegate\npeer-review\nplan-from-spec\nplan-run\nproject-doctor\nproject-private\nprovider-permissions\nrecipe\nremove-project-template\nreport\nroom\nrun\nrun-ledger\nrun-reconcile\nruntime\nscratch-worktree\nsession\nsession-handoff\nskill-doctor\nskill-forge\nslurm-lint\nslurm-submit\nsnapshot\nstate\nstate-verify\nstatus\nsync-shared\nthread\ntick\ntsp-queue\nuninstall\nupdate";
const FRONTDOOR: &str = "# Daily work\npanel\ninbox\nagent-plan\njournal\n# Collaboration and delivery\nconsult\npeer-delegate\npeer-review\nland\n# Setup and maintenance\ninit\nstatus\ndoctor\nupdate\nuninstall";

const DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "advise",
        "Cross-CLI advisor pass at a decision point: compose an adversarial advisor prompt (decision context + unresolved fail-ledger rows + optional session digest) and send it to one other local agent CLI via agent-call.sh.",
    ),
    (
        "agent-call",
        "Call one local agent CLI for a read-only independent pass.",
    ),
    (
        "agent-events",
        "Append, follow, validate, and project the normalized agent lifecycle stream.",
    ),
    (
        "agent-memory",
        "Maintain harness-owned memory shared by Codex, Claude Code, and Antigravity.",
    ),
    (
        "agent-plan",
        "Execution tasks, dependencies, claims and review state for multi-agent work.",
    ),
    (
        "agent-role",
        "Named, reusable role profiles shared by the three agent CLIs.",
    ),
    (
        "agent-supervisor",
        "Queue and supervise bounded attempts without landing, committing, or pushing.",
    ),
    (
        "agent-task",
        "Maintain the current resume note; agent-plan owns execution tasks and claims.",
    ),
    (
        "apply-project-template",
        "Apply oh-my-setting project rule blocks and scaffold PROJECT.md.",
    ),
    (
        "approval-inbox",
        "Request, decide, and consume durable approvals with compare-and-set fencing.",
    ),
    ("artifact-index", "Inspect the harness artifact index."),
    (
        "artifact-view",
        "Build an offline experiment comparison and decision report from recorded results.",
    ),
    ("auto-update", "Check for or apply oh-my-setting updates."),
    (
        "autopilot",
        "Optional bounded execution of an explicitly approved goal over existing plan/drive/landing contracts.",
    ),
    (
        "candidate",
        "Keep results from different tools and runs attached to one candidate id in .oms/candidates.jsonl.",
    ),
    ("change-guard", "Advisory guard for live agent edits."),
    ("checkpoint", "Reversible snapshots of tracked Git state."),
    (
        "ci-status",
        "Print the CI conclusion for a branch's HEAD (default: current branch) so a red run can't go unnoticed after a push — the \"nobody looked\" failure mode.",
    ),
    (
        "cite",
        "Per-repo literature notes and identifier checks: add/list/verify/export over .oms/literature.jsonl.",
    ),
    ("cleanup", "Clean safe oh-my-setting install leftovers."),
    (
        "connect-services",
        "Connect account-backed services used by the harness without ever reading or persisting their credentials.",
    ),
    ("consult", "One verb for consulting other agents mid-task."),
    (
        "dashboard",
        "Read-only terminal dashboard of OMS-managed collaboration state.",
    ),
    (
        "data-manifest",
        "Fingerprint dataset splits so agents can detect silent data drift and, above all, train/eval leakage.",
    ),
    (
        "deps",
        "Show which version of a third-party package this project pins and has installed, with the documentation URLs the package itself declares.",
    ),
    (
        "doctor",
        "Verify managed install identity, tools, skills, and manifest synchronization.",
    ),
    (
        "draft-pr",
        "Publish one verified clean HEAD as a create-only GitHub branch and Draft PR.",
    ),
    (
        "experiment-board",
        "A coordination layer ABOVE single tasks: a shared study board so three agents do not duplicate experiments.",
    ),
    (
        "external",
        "Receipts for external science skills, MCP servers, models and datasets in .oms/external.jsonl: version, sha256, declared license, environment lock and whether use can send data out.",
    ),
    ("fail-ledger", "Cross-session, cross-agent failure memory."),
    (
        "gc",
        "Retention sweep for repo-local .oms state (--dry-run by default).",
    ),
    (
        "github",
        "Same-repository GitHub issue and PR references: offline status and cache freshness, explicit validated refresh, merged-PR adoption by previewed selection, and one idempotent evidence comment.",
    ),
    (
        "goal-drive",
        "Bounded goal driver: acceptance -> isolated task batch -> ordered exact commits over an EXISTING human-approved plan until the plan's acceptance command passes, the cycle cap is hit, tasks run out, or anything looks unsafe.",
    ),
    (
        "graph",
        "Project graph and execution graph over the existing OMS control plane: deterministic code structure queries, context packs, GraphSpec validation, pure route evaluation, and event-backed runs that call plan-run and patch-land instead of re-implementing them (docs/GRAPH-ENGINEERING.md).",
    ),
    (
        "harness",
        "Optional experimental bridge to run Codex models in Pi or Claude Code, or launch native Codex.",
    ),
    (
        "hud",
        "Render the compact OMS status strip for terminals, tmux, and provider statuslines.",
    ),
    (
        "inbox",
        "Prioritized attention view derived entirely from `oms state`.",
    ),
    (
        "init",
        "First move for an agent landing in a repo with no .oms/ state.",
    ),
    (
        "input",
        "Capture attachments and prepare inputs for explicit room delivery.",
    ),
    (
        "install-profile",
        "Inspect or install only the tools required by selected OMS capability profiles, reusing the digest-verified transactional installers.",
    ),
    (
        "intent",
        "Vague goal -> reviewed intent spec -> PROJECT.md.",
    ),
    (
        "job-digest",
        "Compress a training/Slurm log (or a Slurm job id) into a short markdown digest sized for agent context: error patterns, last traceback, tail.",
    ),
    (
        "journal",
        "Read, inspect, rebuild, configure, or sync the local Work Journal.",
    ),
    (
        "land",
        "Gate a committed worktree and open its pull request (merge on request) as one detached job.",
    ),
    (
        "lease",
        "Advisory file leases: parallel sessions in one repository declare, with a time limit, which paths they intend to edit, so others see it before they collide.",
    ),
    (
        "linear",
        "Bridge Linear agent sessions into an OMS Room and send outbound-scanned activity replies with retry-safe receipts.",
    ),
    (
        "model-doctor",
        "Diagnose provider binaries and cached capabilities without selecting models.",
    ),
    (
        "models",
        "Print cached provider capabilities so callers can choose an explicit model.",
    ),
    (
        "output-digest",
        "Run a command and print a bounded digest of its output instead of the whole log.",
    ),
    (
        "panel",
        "Open the terminal control panel for Codex and Claude Code, using existing OMS state and peer workflows.",
    ),
    ("patch-admit", "Admission gate for delegated patches."),
    (
        "patch-land",
        "Land a delegated patch onto the main tree, safely.",
    ),
    (
        "peer-ask",
        "Ask the same question to a provider council, then persist each answer as an artifact.",
    ),
    (
        "peer-delegate",
        "Delegate a write task to another agent CLI.",
    ),
    (
        "peer-review",
        "Review an existing diff with an independent provider council.",
    ),
    ("plan-from-spec", "PROJECT.md -> proposed plan tasks."),
    (
        "plan-run",
        "Execute exactly one pre-authorized agent-plan task.",
    ),
    (
        "project-doctor",
        "Verify that all agent-facing files in a project give every agent the same view.",
    ),
    (
        "project-private",
        "Keeps a project's agent-facing harness files out of git without touching the repo's own .gitignore.",
    ),
    (
        "provider-permissions",
        "Check, and optionally grant, the standing permissions a provider CLI needs to work headlessly for this harness.",
    ),
    (
        "recipe",
        "Explicit local recipes, workspace actions, extension packs and requests.",
    ),
    (
        "remove-project-template",
        "Remove oh-my-setting managed project rule blocks.",
    ),
    (
        "report",
        "Preview a privacy-checked OMS bug report or feature request before publication.",
    ),
    (
        "room",
        "Shared work rooms and addressed messages on the existing OMS thread log.",
    ),
    ("run", "The thin spine that ties runs together."),
    (
        "run-ledger",
        "Run a command and append one JSONL row to the project's experiment ledger: timestamp, command, git SHA, dirty state, Slurm job id, exit code, duration.",
    ),
    (
        "run-reconcile",
        "Long Slurm jobs outlive the episodic agent session that launched them.",
    ),
    (
        "runtime",
        "Typed projections and optional execution services over the existing OMS plane.",
    ),
    (
        "scratch-worktree",
        "Scratch worktrees a parent session can add or remove while workers run.",
    ),
    (
        "session",
        "Search local sessions, fork conversations and code, or restore them.",
    ),
    (
        "session-handoff",
        "Read one agent CLI's session transcript and distill it into a compact, portable handoff digest that another agent can load as context.",
    ),
    (
        "skill-doctor",
        "Diagnose duplicate or missing skill entries across all three agents' skill roots and Codex's shared ~/.agents overlay, and rightsize the skills themselves: a SKILL.md that keeps growing stops being a router into detail and becomes context an agent pays for on every load, whether or not the task needs it.",
    ),
    (
        "skill-forge",
        "Project-scoped skills, forged from evidence.",
    ),
    (
        "slurm-lint",
        "Static check of one Slurm job script against the cluster snapshot's QOS.",
    ),
    ("slurm-submit", "Submit a Slurm job once per request key."),
    (
        "snapshot",
        "Write the private local context agents read before touching this machine.",
    ),
    ("state", "Read-only view over the repo-local .oms state."),
    (
        "state-verify",
        "Read-only consistency verdict over a repository's .oms harness state.",
    ),
    (
        "status",
        "Installation summary: link identity, tools, active task and update state; use inbox for project attention.",
    ),
    (
        "sync-shared",
        "Fetch and fast-forward the shared checkout only between canonical snapshots.",
    ),
    (
        "thread",
        "Shared conversations and live delivery use one append-only thread log.",
    ),
    ("tick", "Unattended maintenance tick for registered repos."),
    (
        "tsp-queue",
        "Single-machine job queue wrapper for tsp (task-spooler).",
    ),
    (
        "uninstall",
        "Remove oh-my-setting managed links/copies (and restore backups when available).",
    ),
    (
        "update",
        "Check, update, or roll back the managed installation.",
    ),
];

pub(super) fn is_public(tool: &std::ffi::OsStr) -> bool {
    tool.to_str()
        .is_some_and(|name| PUBLIC.lines().any(|entry| entry == name))
}

pub(super) fn owns(args: &[OsString]) -> bool {
    args.first().is_some_and(|arg| {
        matches!(
            arg.as_bytes(),
            b"help" | b"list" | b"-h" | b"--help" | b"--version"
        )
    })
}

pub(super) fn run(args: &[OsString], _source: &Path) -> (i32, Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let err = Vec::new();
    let selected = match args.first().map(|arg| arg.as_bytes()) {
        Some(b"-h" | b"--help") => return (0, USAGE.to_vec(), err),
        Some(b"--version") if args.len() == 1 => {
            return (
                0,
                format!("{}\n", oms_runtime::CORE_VERSION).into_bytes(),
                err,
            );
        }
        Some(b"--version") => {
            return (
                2,
                out,
                b"error: oms --version takes no arguments\n".to_vec(),
            );
        }
        Some(b"help") => {
            out.extend_from_slice(USAGE);
            out.push(b'\n');
            FRONTDOOR
        }
        Some(b"list") => match args.get(1).map(|arg| arg.as_bytes()) {
            None | Some(b"") => FRONTDOOR,
            Some(b"--all") => PUBLIC,
            _ => return (2, out, b"error: oms list accepts --all\n".to_vec()),
        },
        _ => {
            return (
                2,
                out,
                b"error: invalid root navigation invocation\n".to_vec(),
            );
        }
    };
    for name in selected.lines() {
        if let Some(title) = name.strip_prefix("# ") {
            out.extend_from_slice(title.as_bytes());
            out.extend_from_slice(b":\n");
            continue;
        }
        let description = DESCRIPTIONS
            .iter()
            .find(|(tool, _)| *tool == name)
            .expect("every listed tool has an embedded description")
            .1;
        out.extend_from_slice(format!("{name:<26} {description}").as_bytes());
        out.push(b'\n');
    }
    (0, out, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_registry_covers_public_and_frontdoor_without_scripts() {
        let names: Vec<_> = PUBLIC.lines().collect();
        assert_eq!(names.len(), 85);
        for tool in FRONTDOOR.lines().filter(|line| !line.starts_with("# ")) {
            assert!(names.contains(&tool));
        }
        let missing = Path::new("no-scripts-here");
        let (code, all, err) = run(&["list".into(), "--all".into()], missing);
        assert_eq!(code, 0);
        assert!(err.is_empty());
        let rendered = std::str::from_utf8(&all).unwrap();
        assert_eq!(rendered.lines().count(), 85);
        for (line, name) in rendered.lines().zip(&names) {
            assert!(line.starts_with(&format!("{name:<26} ")));
            assert!(line.len() > 27);
        }
        let (code, front, _) = run(&["list".into()], missing);
        assert_eq!(code, 0);
        assert!(front.starts_with(b"Daily work:\n"));
        let front = std::str::from_utf8(&front).unwrap();
        assert_eq!(
            front.lines().filter(|line| !line.ends_with(':')).count(),
            13
        );
        for advanced in [
            "autopilot",
            "harness",
            "dashboard",
            "hud",
            "runtime",
            "tick",
            "patch-land",
            "lease",
        ] {
            assert!(is_public(std::ffi::OsStr::new(advanced)));
            assert!(
                !front
                    .lines()
                    .any(|line| line.starts_with(&format!("{advanced:<26} ")))
            );
        }
        let (code, help, _) = run(&["help".into()], missing);
        assert_eq!(code, 0);
        assert!(help.ends_with(front.as_bytes()));
        assert_eq!(run(&["list".into(), "bad".into()], missing).0, 2);
    }
}
