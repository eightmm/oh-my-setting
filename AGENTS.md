# oh-my-setting Project Rules

- The install-wide policy source is `rules/global-AGENTS.md`; read it when the
  current agent has not already loaded the installed global rules.
- Keep language/ML/data/HPC policy in templates/contracts, not in the global
  rules every session loads.
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
  Extend the canonical test or fixture first; add one only for a path no test
  exercises, and say which. Tests grew faster than code (ratio 0.54 to 0.70 in
  two months): a commit that adds tests looks for one it supersedes.
- Keep install, update, repair, and uninstall ownership transitions reversible.
- During development, run affected checks; do not run a full gate for every
  edit or local commit. For committed ranges use `scripts/check.sh --affected`
  with `--changed-from BASE --changed-to HEAD`: ordinary docs use reference
  checks; positive graph evidence selects tests; critical/uncertain changes
  use full coverage. Skill/template prose and the model registry use their
  bounded contract suites; executable templates and permission policy stay broad.
- Deploy/install changed OMS code with `oms land`: it checks the committed HEAD
  and pushes only while HEAD and the tracked tree remain unchanged, without a
  duplicate hook gate. Its default gate is `check.sh --affected --ci-parity`:
  a full selection runs lint and the quick lane, never CI's full lanes, with
  provider CLIs hidden from PATH and a Bash 3.2 parse of changed shell files.
  Installation needs successful CI for that same SHA plus full proof: a full
  local gate (`--gate 'bash scripts/check.sh --parallel'`) or a CI run whose
  jobs executed the full selection. Affected-only gates and runs push but
  never install; the receipt's `update.authority` names the proof.
  Direct pushes keep the full hook unless the destination requires CI's
  risk-based `gate`; only then may local quick feedback replace it. Workflow
  presence alone is not branch protection.
- CI scopes PRs and main pushes alike. Full selections keep Linux lint, focused,
  smoke and copy lifecycle coverage. Native macOS/Windows lifecycle, stock Bash
  3.2/BSD and Python 3.9 run for install/lifecycle/portability/Python compatibility
  changes and weekly/manual runs. Only plan-authorized skips pass the stable
  gate. In this repository `oms land` defaults to CI-parity affected checks
  against the destination; explicit `--gate` wins and CI remains
  authoritative. Smoke shards and focused lanes balance by measured cost
  (`# smoke-weight:` lines, focused registration order); refresh from CI timings.
