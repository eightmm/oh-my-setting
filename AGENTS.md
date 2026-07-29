# oh-my-setting Project Rules

- The install-wide policy source is `rules/global-AGENTS.md`; read it when the
  current agent has not already loaded the installed global rules.
- This repository maintains a Bash harness shared by Codex, Claude Code, and
  Antigravity. Preserve behavior across all three providers.
- Keep scripts compatible with Bash 3.2, GNU/BSD userlands, and Windows Git
  Bash for the documented core lifecycle.
- Write a behavior regression before changing scripts or install contracts.
- Keep install, update, repair, and uninstall ownership transitions reversible.
- Run `bash scripts/check.sh` before commit or push. CI additionally verifies
  the real lifecycle on Linux, macOS, and Windows Git Bash, and only the macOS
  job has a stock Bash 3.2 parser and a BSD userland.
