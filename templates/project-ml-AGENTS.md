# ML Project Guidelines

- Prefer local conventions over global defaults.
- Read `PROJECT.md` first. If missing/draft/incomplete, interview and update it before coding.
- Change only task-relevant lines.
- Evidence before edit; inspect data path, loader, model, loss, metrics, tests.
- No masking bugs with broad `try/except`, fallback `if`, silent `return None`, or zero padding.
- Do not patch shape/data mismatches by padding/truncating unless spec requires it.
- Python: use `uv sync/add/run`; local `.venv`.

## ML Startup Workflow

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

- Use `uv` for Python envs: `uv sync`, `uv add`, `uv run`.
- Keep project env local at `.venv`; do not use global Python/pip unless confirmed.
- Read machine specs from `~/.oh-my-setting/local/machine.md` when compute affects behavior.
- Do not duplicate full machine specs into project docs; record only project-specific compute constraints.
- Update machine snapshot when GPU, driver/CUDA, RAM, storage, Slurm partition, or Python base changes.

## Docs

- Keep `PROJECT.md` as the active spec gate; keep durable knowledge under `docs/`.
- Create docs only after interview -> concrete outline -> confirm -> write.
- Do not create empty placeholder docs without confirmed purpose, scope, owner, and update trigger.
- Docs roles: `architecture.md` for flows/contracts; `data.md` for schema/labels/splits/leakage; `experiments.md` for runs/metrics/conclusions; `operations.md` for commands/logs/checkpoints/recovery.
- Use `docs/decisions/NNNN-title.md` for architecture/data/API decisions expensive to reverse.
- Update docs when changing data format, model interface, config schema, checkpoint format, metric, or experiment protocol.

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

## ML Safety Stop

- Never infer label meaning, split policy, leakage boundary, or metric direction from file names alone.
- Ask before using validation/test data for preprocessing fit, feature selection, normalization, threshold tuning, checkpoint choice, or early stopping.
- Do not run long training unless data, dataloader, model, and loss smoke checks pass.

## Project Commands

- Setup: `uv sync`
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
