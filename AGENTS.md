# oh-my-setting Project Rules

- The install-wide policy source is `rules/global-AGENTS.md`; read it when the
  current agent has not already loaded the installed global rules.
- This repository maintains a Bash harness shared by Codex, Claude Code, and
  Antigravity. Preserve behavior across all three providers.
- Keep scripts compatible with Bash 3.2 and GNU/BSD userlands.
- Write a behavior regression before changing scripts or install contracts.
- Keep install, update, repair, and uninstall ownership transitions reversible.
- Run `bash scripts/check.sh` before commit or push. CI additionally verifies the
  real install lifecycle and macOS portability.
