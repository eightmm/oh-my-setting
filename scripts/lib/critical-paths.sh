#!/usr/bin/env bash
# shellcheck shell=bash

# The one list of paths whose change is never judged by test selection: the
# gate runs the complete selection for them (`check.sh --affected`), and the
# fail-ledger names the advisor when a failure is recorded while one is
# modified. Both ask this function; neither keeps its own copy. Order matters:
# the prose-only carve-out sits between the boundaries and the broad globs.
#
# oms_critical_path_reason PATH
#   prints "selector-boundary:PATH" or "contract-boundary:PATH" and returns 0
#   for a critical path; prints nothing and returns 1 otherwise.
oms_critical_path_reason() {
  case "$1" in
    .github/workflows/*|scripts/check.sh|scripts/graph.sh|scripts/lib/oms_graph/*|tests/graph-smoke.sh|tests/run-smoke-shard.sh)
      printf 'selector-boundary:%s\n' "$1"
      return 0
      ;;
    AGENTS.md|*/AGENTS.md|CLAUDE.md|*/CLAUDE.md|GEMINI.md|*/GEMINI.md|PROJECT.md|SECURITY.md)
      printf 'contract-boundary:%s\n' "$1"
      return 0
      ;;
    custom-skills/*.md|templates/*.md) return 1 ;;
    install.sh|scripts/install*|scripts/uninstall*|scripts/update*|scripts/doctor.sh|\
    scripts/python-runtime.sh|scripts/land.sh|scripts/patch-admit.sh|scripts/pre-push-check.sh|\
    scripts/lib/oms-common.sh|\
    scripts/lib/install*|scripts/lib/python-runtime.sh|scripts/lib/*lock*|scripts/lib/*state*|\
    scripts/lib/*artifact*|scripts/lib/oms_runtime/common.py|scripts/lib/oms_runtime/child_policy.py|\
    scripts/lib/durable-jsonl.py|rules/*|config/*|templates/*|custom-skills/*|plugins/*)
      printf 'contract-boundary:%s\n' "$1"
      return 0
      ;;
  esac
  return 1
}
