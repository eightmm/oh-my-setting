# oh-my-setting Project Rules

- The install-wide policy source is `rules/global-AGENTS.md`; read it when the
  current agent has not already loaded the installed global rules.
- This repository maintains a Bash harness shared by Codex, Claude Code, and
  Antigravity. Preserve behavior across all three providers.
- Keep scripts compatible with Bash 3.2, GNU/BSD userlands, and Windows Git
  Bash for the documented core lifecycle. On that path, strip `\r` from any
  value read back from `python3` and resolve paths with `pwd -P` before
  comparing them: Windows Python emits CRLF, and one directory has more than
  one spelling.
- Artifacts this repo publishes into shared namespaces carry the oms
  marker: skills and output styles are `oms-*`, MCP tools `oms_*`, work
  branches `oms/*`, and `skill-forge add` refuses new unprefixed project
  skills. Stored skills under legacy names stay readable. Exempt:
  provider-mandated names (plugin directory layouts) and repo-internal
  files referenced by path (`roles/`, `prompts/`, `config/`).
- The single PATH entry is `oms`, so its subcommands already sit inside
  the marker: name them `oms <verb>`, never `oms oms-<verb>`. One spelling
  per verb — the dispatcher holds no aliases, and the verb is the script
  filename.
- Preserve regression coverage when changing scripts or install contracts.
  Extend the canonical test or fixture first; add one only for uncovered behavior.
- Keep install, update, repair, and uninstall ownership transitions reversible.
- During development, run affected checks; do not run a full gate for every
  edit or local commit. For committed ranges use `scripts/check.sh --affected`
  with `--changed-from BASE --changed-to HEAD`: ordinary docs use reference
  checks; positive graph evidence selects tests; critical/uncertain changes
  use full coverage. Skill/template prose and the model registry use their
  bounded contract suites; executable templates and permission policy stay broad.
- Before deployment/installing changed OMS code, run `bash scripts/check.sh`
  once. Prefer `oms land`: it checks the committed HEAD and pushes only while
  HEAD and the tracked tree remain unchanged, without a duplicate hook gate.
  Installation follows successful CI and must target that same SHA.
  Direct pushes keep the full hook unless the destination requires CI's
  risk-based `gate`; only then may local quick feedback replace it. Workflow
  presence alone is not branch protection.
- CI scopes PRs and main pushes alike. Full selections plus weekly/manual runs
  verify the native Linux/macOS/Windows lifecycle, stock Bash 3.2/BSD userland,
  and Python 3.9. Only skips authorized by the plan can pass the stable gate.
