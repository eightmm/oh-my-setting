# oh-my-setting Project Rules

## Entrypoints
- `rules/global-AGENTS.md`: install-wide policy; read it unless already loaded.
- `templates/`: language/ML/data/HPC policy and contracts, not global rules.
- `crates/oms/src/registry.rs`: public verbs; owners in `crates/oms-command-*`.
- `install.sh`: installer; `tests/install-lifecycle-smoke.sh`: lifecycle E2E.
- `scripts/check.sh`: affected/full gate; `scripts/lib/critical-paths.sh`.
- `.github/workflows/test.yml`: hosted CI (Linux-only).
- `crates/oms-scrub/tests/scan_contract.rs`: outbound scrubber contract.
- `docs/README.md`: docs map; `docs/COMPONENTS.md`: command catalog.
- `crates/oms-command-panel`: terminal panel; `crates/oms-panel-core`: non-TUI core.
- `./verify [--seed]`: single root verifier; four named stages, smoke JSON on stdout.

## Rules
- A native Rust harness shared by Codex, Claude Code and Antigravity, with Bash
  install and verification entrypoints: preserve behavior across all three.
- Hosts: Linux (Ubuntu 20.04 floor), WSL2 and macOS run the runtime and
  installer; native Windows is refused. Require Bash 5.0+, keep the documented
  core lifecycle GNU/BSD-compatible, and compare paths after `pwd -P`.
- Shared-namespace artifacts carry the oms marker: skills and output styles
  `oms-*`, MCP tools `oms_*`, work branches `oms/*`; `skill-forge add` refuses
  new unprefixed project skills; legacy stored names stay readable. Exempt:
  provider-mandated names (plugin layouts), files referenced by path (`roles/`,
  `config/`). `oms` is the single PATH entry: verbs are `oms <verb>`, never
  `oms oms-<verb>`, one spelling each; the dispatcher holds no aliases.
- Changing scripts or install contracts preserves regression coverage: extend
  the canonical test or fixture first; add one only for an unexercised path,
  and say which. A commit adding tests looks for one it supersedes. A recurring
  defect gets one regression test or one enforced check rule, not more prose.
- Keep install, update, repair and uninstall ownership transitions reversible.
- While editing, run affected checks, not a full gate per edit or commit:
  `scripts/check.sh --affected --changed-from BASE --changed-to HEAD`. Docs get
  reference checks only; critical/uncertain paths (critical-paths.sh: lifecycle,
  hooks/guards, scrubber, permission policy, selector, workflows, core Rust
  crates) and `OMS_AFFECTED_FULL=1` get full coverage. Skill/template/role prose
  uses its bounded `skill-manifest` suite; executable templates stay broad.
- Before reporting completion, run the root `./verify`.
- Land/install with `oms land`: it checks the committed HEAD, pushes only while
  HEAD and the tracked tree stay unchanged, runs no duplicate hook gate, defaults
  to `check.sh --affected --ci-parity`. Install needs same-SHA green CI plus `gate`,
  `lint`, `focused`, `native (ubuntu-latest, copy)`, optional `rust (ubuntu-24.04)`.
- Hosted CI is Linux-only for PRs and main pushes alike: pure-Rust lint, Rust
  fmt/clippy/tests on Ubuntu, pure-Bash focused E2E and a Linux copy-install
  lifecycle, on pinned Rust `1.99.1`. Only plan-authorized skips pass the stable
  gate. CI evidence covers its Linux runner userland. macOS remains an optional
  runtime and installation host.
- Every tracked file passes `scrubber_passes_tracked_repository_sources`: in
  fixtures split key-shaped strings and home paths across quotes; avoid
  identifiers that read as a credential assignment.
