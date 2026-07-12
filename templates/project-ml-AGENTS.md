# ML Project Guidelines

- Prefer local conventions over global defaults.
- Read `PROJECT.md` when it exists. Stop for spec work only when unresolved choices affect the requested change.
- Change only task-relevant lines.
- Evidence before edit; inspect data path, loader, model, loss, metrics, tests.
- Implementation checks require reading the relevant source files/scripts, not
  only docs, status output, memory, or prior summaries.
- No masking bugs with broad `try/except`, fallback `if`, silent `return None`, or zero padding.
- Do not patch shape/data mismatches by padding/truncating unless spec requires it.
- Python: use `uv sync/add/run`; local `.venv`.

## Output And Artifacts

- Compact, direct, low-token. Fragments and arrows OK when clear.
- Commit messages: Conventional Commits; subject <= 50 chars; body only for non-obvious why/risk.
- Markdown/docs: short sections, bullets, direct commands; keep setup, safety, and specs explicit.

## ML Project Startup Workflow

This workflow applies to project startup and broad/architecture-shaping ML work,
not clear bounded maintenance.

1. Skeleton: create missing standard directories only. No model, training, data processing, or dependency changes yet.
2. Interview: clarify goal, users, non-goals, task type, data, metric, baseline, constraints, compute, and verification.
3. Spec: write/update `PROJECT.md` with confirmed answers, paths, commands, risks, and open decisions.
4. Confirm: wait for user confirmation if `PROJECT.md` is draft/incomplete or decisions affect data/API/model/deps/compute.
5. Code: implement only after the spec is confirmed.

## Standard ML Layout

```text
.
|-- configs/
|-- data/
|   |-- raw/
|   `-- processed/
|-- docs/
|-- notebooks/
|-- outputs/
|-- scripts/
|   |-- train.py
|   |-- eval.py
|   `-- infer.py
|-- src/
|   `-- project_name/
|       |-- data/
|       |-- models/
|       |-- training/
|       |-- evaluation/
|       `-- utils/
`-- tests/
```

- `scripts/` are thin CLI entrypoints; reusable logic belongs under `src/`.
- Notebooks are exploration only; do not make production code depend on notebooks.
- Keep configs in `configs/`; do not hardcode experiment hyperparameters in scripts.
- Treat `data/raw`, `data/processed`, `outputs`, `checkpoints`, `wandb`, and `runs` as gitignored unless explicitly intended.
- Do not commit private data, large datasets, checkpoints, generated outputs, or real secrets.

## Environment

- Training defaults (optimizer, LR schedule, DDP, checkpoint format, CUDA env
  vars) live in the `ml-training` skill; load it for training setup work
  instead of restating defaults here.
- For chemical, biological, or therapeutic data, load the `chem-bio-ml` skill
  before deciding splits, labels, metrics, or featurization. It routes molecule,
  protein, interaction, biologic, nucleic-acid/gene-editing, reaction,
  generation, cellular/omics, imaging, and biomedical-network tasks to
  domain-specific leakage guardrails.
- For experiment design and result interpretation, load the `research-method`
  skill: falsifiable hypothesis, pre-registered metric, baseline, ledgered result.
- Use `uv` for Python envs: `uv sync`, `uv add`, `uv run`.
- Keep project env local at `.venv`; do not use global Python/pip unless confirmed.
- Read machine specs from `~/.oh-my-setting/local/machine.md` when compute affects behavior.
- Do not duplicate full machine specs into project docs; record only project-specific compute constraints.
- Update machine snapshot when GPU, driver/CUDA, RAM, storage, Slurm partition, or Python base changes.

## Docs

- Keep `PROJECT.md` as the active spec gate; keep durable knowledge under `docs/`.
- Core ML docs scaffolded by default: `DATA.md`, `MODEL.md`, `EVALUATION.md`,
  `EXPERIMENTS.md`, and `REPRODUCIBILITY.md`. Create optional docs only when
  the project needs them; use `apply-project-template.sh ml . --full-docs` for
  the complete template set.
- Use `docs/decisions/NNNN-title.md` for architecture/data/API decisions expensive to reverse.
- Update docs when changing data format, model interface, config schema, checkpoint format, metric, or experiment protocol.
- Version-pin chain: `DATA.md` <-> `MODEL.md` <-> `TRAINING.md` <-> `CHECKPOINTS.md`. Any bump invalidates downstream.

## Test Strategy

- Test interfaces, not every tiny function.
- Priority: data validation -> dataloader batch -> model architecture -> output contract.
- Check shapes, dtypes, masks, devices, seeds, NaN/Inf, empty/small batches.
- Model checks: forward pass, loss contract, checkpoint load, inference output.
- Add narrow unit tests only for fragile pure logic or past bugs.

## ML Reliability

- No fake green: do not skip/xfail/edit tests to hide failure.
- Root cause first: trace NaN, shape mismatch, metric jump, data error upstream.
- Reproducible runs: command, config, seed, data version, commit, checkpoint.
- Metrics contract: define metric before training; compare against baseline.
- Research runs: prefer `oms research-runner ... -- <cmd>` for hypothesis-testing experiments; it pre-registers the claim and then calls the run ledger.
- Run ledger: use `oms run-ledger -- <cmd>` for simple mechanical runs; read `docs/EXPERIMENTS.jsonl` before proposing new experiments.
- Long logs/failed jobs: digest with `oms job-digest <logfile|jobid>` instead of reading raw logs.
- Coordination: `oms state` when resuming (open runs, claims, failures); claim long runs on `oms experiment-board`; run `oms data-manifest check --name <manifest>` and `oms data-manifest leakage --name <manifest>` before training when registered splits exist.

## ML Safety Stop

- Never infer label meaning, split policy, leakage boundary, or metric direction from file names alone.
- Ask before using validation/test data for preprocessing fit, feature selection, normalization, threshold tuning, checkpoint choice, or early stopping.
- Do not run long training unless data, dataloader, model, and loss smoke checks pass.
- Before long training or Slurm submission, run focused data/model/loss smoke
  checks. Use `oms peer-review --ml` only for an explicit cross-agent or release
  gate.

## Project Commands

- Setup: `uv sync`
- Verify (contract): `scripts/check.sh fast` -- run before claiming work done; `scripts/check.sh gpu` for GPU smoke
- Data check:
- Dataloader smoke:
- Model smoke:
- Train smoke:
- Inference check:
- Test:

## Project Contracts

- Raw data:
- Processed data:
- Split file:
- Config path:
- Output/log path:
- Checkpoint path:
- Primary metric:
- Baseline:
- Do not touch:
