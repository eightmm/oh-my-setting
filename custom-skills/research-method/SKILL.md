---
name: research-method
description: >
  Hypothesis-driven research loop for experimental work (ML and science):
  turn a question into a falsifiable hypothesis, design the smallest
  experiment that could disprove it, predict the outcome before running,
  compare against a baseline, and record the result — pass or fail. Use when
  planning experiments, interpreting results, or deciding the next run; also
  when coordinating runs across agents ("claim this experiment", "실험 겹치지
  않게"), reproducing or comparing runs ("reproduce that run", "diff run A vs
  B", "실험 재현", "런 비교"), or tracing an output back to its run ("which
  run produced this checkpoint", run-capsule whence). Complements
  spec-interview (build), chem-bio-ml (domain correctness), and the run
  ledger (history).
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
- For a run you may need to reproduce exactly, wrap it in a capsule instead of a
  bare ledger row. `run-capsule.sh` records the commit, the *uncommitted diff*,
  config/env/seed/output fingerprints, the captured output log, and the result
  under `.oms/runs/<id>/` (git-ignored), and still writes the companion ledger
  row. Later, `run-capsule.sh reproduce <id>` prints the exact checkout + diff
  apply + command, and `verify <id>` flags drift between the capsule and the
  current tree. This matters most when several agents mutate the tree between
  runs — the bare commit no longer identifies what actually ran.

```bash
~/.oh-my-setting/scripts/run-capsule.sh run --note "baseline" \
  --config config.yaml --seed 7 --metrics metrics.json --output ckpt/last.pt \
  -- uv run python train.py
~/.oh-my-setting/scripts/run-capsule.sh list
~/.oh-my-setting/scripts/run-capsule.sh reproduce <id>
~/.oh-my-setting/scripts/run-capsule.sh verify <id>
```

## Coordinating Across Agents

When several agents run experiments on the same project, claim before you run so
two of them don't burn GPU on the same idea. The study board is the shared
intent layer above the ledger: it tracks the hypothesis, owner, lifecycle, and
result, and refuses a duplicate claim (with stale-claim recovery if the owner
went away).

```bash
~/.oh-my-setting/scripts/experiment-board.sh claim --hypothesis "scaffold split helps" --baseline random
~/.oh-my-setting/scripts/experiment-board.sh start  --id scaffold-split-helps --job <slurm_id>
~/.oh-my-setting/scripts/experiment-board.sh finish --id scaffold-split-helps --result "AUC 0.82 vs 0.74" --next "try cluster split"
~/.oh-my-setting/scripts/experiment-board.sh list            # active claims
```

The board records intent; `run-ledger`/`run-capsule` record what actually ran;
`run-reconcile` writes back the job's terminal state. Claim → run (capsule) →
reconcile → finish.

Tie one experiment's records together with a run id. Mint it once and export
it; the tools then auto-link their records into a shared join index, and
`oms-run.sh show` joins them back. The tools stay independent — the run id is
just a foreign key, not an orchestrator.

```bash
id=$(~/.oh-my-setting/scripts/oms-run.sh new --note "scaffold split"); export OMS_RUN_ID="$id"
~/.oh-my-setting/scripts/experiment-board.sh claim --id scaffold --hypothesis "scaffold helps"
~/.oh-my-setting/scripts/run-capsule.sh run --config c.yaml -- uv run python train.py
~/.oh-my-setting/scripts/oms-run.sh show "$id"   # board + capsule + ledger for this run
~/.oh-my-setting/scripts/oms-run.sh ls
~/.oh-my-setting/scripts/oms-run.sh diff <run_a> <run_b>   # commit/env/config/seed + metric deltas
```

`diff` answers the actual research question — "did my change help, and was it
isolated?" — by joining two runs' capsules: it shows commit/env/config/seed
differences alongside the metric deltas (e.g. `metric:auc … Δ +0.08`).

Trace a checkpoint back to the run that produced it (capsule hashes `--output`
files up to a size cap):

```bash
~/.oh-my-setting/scripts/run-capsule.sh run --output ckpt/best.pt -- uv run python train.py
~/.oh-my-setting/scripts/run-capsule.sh whence ckpt/best.pt   # -> producing run id + commit/config/env
```

## Stop

Do not launch a long/expensive run until question, falsifiable hypothesis,
predicted outcome, single variable, baseline, and pre-registered success bar
are all stated. If a result is being interpreted after the fact, check it
against the anti-patterns above before acting on it.

For a high-stakes run, have three models attack the design first:
`peer-ask.sh --hypothesis --prompt "<hypothesis + planned experiment>"`
injects the falsifiability/confound/baseline/variance checklist into each
advisor. It is the design-time counterpart to `peer-review.sh --ml`
(which gates the diff at code time).
