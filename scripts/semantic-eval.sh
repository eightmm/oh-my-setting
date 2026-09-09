#!/usr/bin/env bash
set -euo pipefail

# Migration only: do not execute legacy specs or reinterpret their results.
usage() {
  printf '%s\n' \
    'semantic-eval is retired; no evaluation is performed.' \
    'Use oms patch-admit for deterministic checks of a patch against current HEAD,' \
    'and oms peer-review --gate --prompt "<review rubric>" for the current diff.' \
    'These do not reproduce historical-base evaluations or convert old reports into admission.' \
    'Existing reports are preserved. No host checks, model calls, or report writes are performed.'
}

if [ "$#" -eq 1 ]; then
  case "$1" in -h|--help) usage; exit 0 ;; esac
fi
usage >&2
exit 2
