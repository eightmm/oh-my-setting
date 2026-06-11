---
name: research-method
description: >
  Hypothesis-driven research loop for experimental work (ML and science):
  turn a question into a falsifiable hypothesis, design the smallest
  experiment that could disprove it, predict the outcome before running,
  compare against a baseline, and record the result — pass or fail. Use when
  planning experiments, interpreting results, or deciding the next run.
  Complements spec-interview (build), chem-bio-ml (domain correctness), and
  the run ledger (history).
---

The loop that keeps experiments honest. Not domain knowledge (see
`chem-bio-ml`) and not a build spec (see `spec-interview`) — the discipline
that decides what to run and what a result means.

## The Loop

1. **Question** — state the specific question this run answers. If you cannot
   say what observation would change your mind, it is not a research step yet.
2. **Hypothesis** — make it falsifiable: "X improves metric M on split S by
   ≥ δ versus baseline B." A claim no experiment could refute is not testable.
3. **Predict first** — write the expected outcome (direction and rough
   magnitude) BEFORE running. A run with no prior prediction cannot surprise
   you, and surprise is the signal.
4. **Smallest falsifying experiment** — the cheapest run that could disprove
   the hypothesis. One independent variable; hold the rest fixed. Prefer a
   subset/epoch-limited probe before a full GPU run.
5. **Baseline** — never evaluate in isolation. Compare against a prior result,
   a trivial baseline, or an ablation. "Better than nothing" is not better.
6. **Run through the ledger** — for ML/research runs, prefer
   `research-runner.sh` so question, hypothesis, prediction, baseline, metric,
   success threshold, and single changed variable are pre-registered before it
   calls `run-ledger.sh`. Use raw `run-ledger.sh` only for simple mechanical
   runs that are not testing a research claim.
7. **Compare to the prediction** — state met / not-met / surprising, with the
   number. A failed prediction is the most informative outcome — keep it.
8. **Revise** — update the hypothesis or the next question. One conclusion per
   run; do not bundle.

## Pre-Register

Before a long or expensive run, fill the `## Experiment Pre-Registration`
section in PROJECT.md (scaffolded on ml projects; the project doctor warns
when the ledger has runs but the section is missing):

- the metric and its split (domain-appropriate — see `chem-bio-ml`),
- the success threshold (the δ that would count as real),
- what result would make you abandon the direction.

Deciding the bar after seeing results is how noise becomes a "finding."

## Anti-Patterns (stop and flag)

- **HARKing** — writing the hypothesis after seeing the result.
- **Goalpost drift** — changing the metric/threshold mid-stream to clear it.
- **Cherry-picking** — reporting the best seed/checkpoint without the spread;
  report variance across seeds, not a single lucky run.
- **No baseline / no ablation** — a gain you cannot attribute is not a result.
- **Confirmation runs** — only running configs expected to win; design the run
  that could break the idea.
- **Leakage-flattered metrics** — a number from a leaky split is not evidence
  (defer to `chem-bio-ml` for split policy).
- **Bundled changes** — moving several variables, so the cause is unknowable.

## Recording

- Keep negative and null results; they prune the search space and prevent
  re-running dead ends. The ledger is the lab notebook.
- Record the outcome metric in the same row: have the run write a small JSON
  of scalar metrics and pass it as `research-runner.sh --metrics <file> -- <cmd>`
  or `run-ledger.sh --metrics <file> -- <cmd>`, so the hypothesis's number lives
  with its command and commit.
- A result without its config, seed, data version, and commit is not a result.
- When a run overturns a prior conclusion, note which ledger row it supersedes.

## Stop

Do not launch a long/expensive run until question, falsifiable hypothesis,
predicted outcome, single variable, baseline, and pre-registered success bar
are all stated. If a result is being interpreted after the fact, check it
against the anti-patterns above before acting on it.

For a high-stakes run, have three models attack the design first:
`multi-agent-ask.sh --hypothesis --prompt "<hypothesis + planned experiment>"`
injects the falsifiability/confound/baseline/variance checklist into each
advisor. It is the design-time counterpart to `multi-agent-review --ml`
(which gates the diff at code time).
