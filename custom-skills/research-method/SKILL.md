---
name: research-method
description: >
  Hypothesis-driven loop for ML and scientific experiments. Use to turn a
  question into a falsifiable hypothesis, design the smallest disconfirming
  run, pre-register prediction/baseline/metric, interpret results, coordinate
  experiment claims, reproduce or compare runs, or trace an output to its run.
---

# Research Method

Decide what to run and what a result means. Use `chem-bio-ml` for domain
correctness and `spec-interview` for build contracts.

## Core Loop

1. State the question and observation that would change the conclusion.
2. Write a falsifiable hypothesis: change X affects metric M on split S by a
   declared threshold versus baseline B.
3. Predict direction and rough magnitude before running.
4. Choose the cheapest experiment that could disprove the claim. Change one
   independent variable and hold the rest fixed.
5. Compare with a baseline or ablation under the same evaluation contract.
6. Record command, config, seed, data/version boundary, commit/diff, metric, and
   result whether positive, negative, or null.
7. Compare result to prediction, label met/not-met/surprising, and revise one
   conclusion or next question.

Do not launch a long or expensive run until the question, hypothesis,
prediction, changed variable, baseline, metric/split, and success or abandonment
threshold are explicit.

## Route to One Reference

- Ledger versus capsule, exact reproduction, run diff, output provenance:
  [run-provenance.md](references/run-provenance.md)
- Study-board claims, run IDs, reconciliation, multi-agent coordination:
  [experiment-coordination.md](references/experiment-coordination.md)
- Anti-patterns and optional independent design attack:
  [design-review.md](references/design-review.md)

Prefer `oms research-runner ... -- <cmd>` for a research claim because it
pre-registers the contract before calling the run ledger. Use a raw run ledger
for mechanical runs that do not test a claim.

Report the pre-registered contract, command/resource boundary, actual metric,
comparison to baseline and prediction, limitations, conclusion, and next
falsifying step. Preserve failed hypotheses so agents do not repeat them.
