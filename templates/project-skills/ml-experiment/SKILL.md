---
name: ml-experiment
description: Run training and evaluation as registered experiments instead of ad-hoc launches: check the experiment board for duplicates, pre-register hypothesis runs, gate long runs through the run ledger, capture a reproducibility capsule, and record outcomes where the next session reads them.
---

# ML Experiment Discipline

An unrecorded run is a run the next session repeats. Before launching
anything that costs more than a smoke test:

1. **Check for duplicates**: `oms experiment-board list` — another agent or
   session may already be running or have finished this configuration.
2. **Pre-register hypothesis runs**: if this run tests a claim, register it
   before launch with `oms research-runner` (hypothesis, pre-registered
   metric, baseline). A verdict recorded after the fact is not evidence.
3. **Gate the launch**: long or expensive runs go through `oms run-ledger`
   — it runs the project's `check.sh` pre-flight, warns on duplicates, and
   writes one JSONL row per run. Record eval scalars with `--metrics`.
4. **Capture reproducibility**: `oms run-capsule` bundles the exact command,
   environment, and data provenance so an output can be traced back to its
   run.
5. **Record the outcome**: metrics into the ledger row, the conclusion into
   the task packet (`oms agent-task update --decision/--result`) so the Work
   Journal daily carries it.

Rank past runs with `oms run-ledger top --metric KEY` before proposing a new
configuration — the best known baseline is a lookup, not a memory.
