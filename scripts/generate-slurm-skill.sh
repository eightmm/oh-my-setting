#!/usr/bin/env bash
# Compatibility shim: the generator writes a private cluster reference
# (local/slurm.md), not a skill, and was renamed to say so. Old receipts,
# docs, and habits keep working through this name.
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/generate-slurm-reference.sh" "$@"
