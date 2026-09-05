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
- Run relevant checks before local commits and `bash scripts/check.sh` before
  push. Prefer `oms land` for release: it checks the committed HEAD once and
  pushes only while HEAD and the tracked tree remain unchanged. Do not run a
  second full gate through the pre-push hook after that successful check.
  Direct pushes retain the full hook; quick mode requires protected-branch CI.
  CI additionally verifies
  the real lifecycle on Linux, macOS, and Windows Git Bash, and only the macOS
  job has a stock Bash 3.2 parser and a BSD userland.
