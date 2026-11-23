# oh-my-setting Project Rules

- The install-wide policy source is `rules/global-AGENTS.md`; read it when the
  current agent has not already loaded the installed global rules.
- Keep language/ML/data/HPC policy in templates/contracts, not in the global
  rules every session loads.
- This repository maintains a native Rust harness shared by Codex, Claude Code,
  and Antigravity, with Bash installation and verification entrypoints.
  Preserve behavior across all three providers.
- Supported hosts are Linux (Ubuntu 20.04 floor), WSL2 and macOS; native
  Windows is retired and refused. Require Bash 5.0+ and keep scripts compatible
  with GNU/BSD userlands for the documented core lifecycle; resolve paths with
  `pwd -P` before comparing them: one directory has more than one spelling.
- Artifacts this repo publishes into shared namespaces carry the oms
  marker: skills and output styles are `oms-*`, MCP tools `oms_*`, work
  branches `oms/*`, and `skill-forge add` refuses new unprefixed project
  skills. Stored skills under legacy names stay readable. Exempt:
  provider-mandated names (plugin directory layouts) and repo-internal
  files referenced by path (`roles/`, `prompts/`, `config/`).
- The single PATH entry is `oms`, so its subcommands already sit inside
  the marker: name them `oms <verb>`, never `oms oms-<verb>`. One spelling
  per verb — the dispatcher holds no aliases. Public verbs are described in
  the native registry and dispatched to their owning command crate.
- Preserve regression coverage when changing scripts or install contracts.
  Extend the canonical test or fixture first; add one only for a path no test
  exercises, and say which. Tests grew faster than code (ratio 0.54 to 0.70 in
  two months): a commit that adds tests looks for one it supersedes.
- Keep install, update, repair, and uninstall ownership transitions reversible.
- During development, run affected checks; do not run a full gate for every
  edit or local commit. For committed ranges use `scripts/check.sh --affected`
  with `--changed-from BASE --changed-to HEAD`: docs use reference checks
  only, alone or beside code; critical/uncertain changes
  (`scripts/lib/critical-paths.sh`: lifecycle, hooks/guards, scrubber,
  permission policy, selector, workflows, core Rust crates) use full coverage,
  and `OMS_AFFECTED_FULL=1` forces it. Skill/template/role prose uses its
  bounded contract suite (`skill-manifest`); executable templates stay broad.
- Deploy/install changed OMS code with `oms land`: it checks the committed HEAD
  and pushes only while HEAD and the tracked tree remain unchanged, without a
  duplicate hook gate. Its default gate is `check.sh --affected --ci-parity`.
  Installation needs successful CI for that same SHA plus full proof (`gate`
  + `lint` + `focused` + `native (ubuntu-latest, copy)`, with optional
  `rust (ubuntu-24.04)`).
- CI is Linux-only and scopes PRs and main pushes alike using pure-Rust jobs
  (Linux lint, Rust fmt/clippy/tests on Ubuntu, pure-Bash focused E2E suites,
  and a Linux copy-install lifecycle test) with pinned Rust `1.99.1`. macOS
  remains a supported runtime and install host with GNU/BSD-compatible scripts
  (no macOS CI; `tests/bsd-portability-smoke.sh` runs on the host userland).
  Only plan-authorized skips pass the stable gate. CI remains authoritative.
- Every tracked file passes the outbound scrubber
  (`scrubber_passes_tracked_repository_sources`): in fixtures, split key-shaped
  strings and literal home paths across quotes, and avoid identifiers that read
  as a credential assignment.
