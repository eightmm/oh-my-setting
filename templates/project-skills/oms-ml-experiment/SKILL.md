---
name: oms-ml-experiment
description: Pre-register claim-bearing ML experiments and preserve comparable results, provenance and compute authority; ordinary code edits need no experiment workflow.
---

# ML Experiment Discipline

Use the project's existing scientific contract and authorized compute budget.
Do not launch a run merely because this skill was loaded.

1. **Check for duplicates**: `oms experiment-board list` — another agent or
   session may already be running or have finished this configuration.
2. **Pre-register hypothesis runs**: if this run tests a claim, register it
   before launch with `oms runtime experiment launch` (hypothesis, pre-registered
   metric, baseline). A verdict recorded after the fact is not evidence.
3. **Gate the launch**: `experiment launch` already uses `oms run-ledger`;
   use the ledger directly for mechanical runs, not as a second launch.
   It runs the project's `check.sh` pre-flight, warns on duplicates, and
   writes one JSONL row per run. Record eval scalars with `--metrics`.
4. **Keep provenance**: link the existing command, environment, data and model
   identities. Use `oms run capsule` when a portable capture is needed;
   do not create another artifact if the run already has this evidence.
   `oms run capsule --config PATH` fingerprints a config (a small JSON/YAML one
   is also stored by key) and `--data NAME` links a registered data manifest
   (fingerprint and row counts, never data); `oms run diff A B` then names what
   changed between two runs. `--data` does not re-read split files: re-run
   `oms data-manifest check`.
5. **Record the outcome**: metrics into the ledger row, the conclusion into
   the task packet (`oms agent-task update --decision/--result`) so the Work
   Journal daily carries it.

Rank past runs with `oms run-ledger top --metric KEY` before proposing a new
configuration — the best known baseline is a lookup, not a memory. A
git-tracked `metric-contract.json` beside the ledger fixes each metric's
direction and eval code; `top` then ranks only comparable runs and prints "not
comparable" (exit 4) otherwise. With several seeds, `top --by-group` ranks by
mean with n, stdev and a bootstrap interval; one-seed groups are listed apart
and support no "better" claim.
