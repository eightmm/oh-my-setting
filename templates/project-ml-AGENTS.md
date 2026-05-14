# ML Project Guidelines

- Prefer local conventions over global defaults.
- Read `PROJECT.md` first. If missing/draft/incomplete, interview and update it before coding.
- Change only task-relevant lines.
- New/vague work: interview -> spec -> confirm -> code.
- Evidence before edit; inspect data path, loader, model, loss, metrics, tests.
- No masking bugs with broad `try/except`, fallback `if`, silent `return None`, or zero padding.
- Do not patch shape/data mismatches by padding/truncating unless spec requires it.
- Python: use `uv sync/add/run`; local `.venv`.

## Test Strategy

- Test interfaces, not every tiny function.
- Priority: data validation -> dataloader batch -> model architecture -> output contract.
- Check shapes, dtypes, masks, devices, seeds, NaN/Inf, empty/small batches.
- Model checks: forward pass, loss contract, checkpoint load, inference output.
- Add narrow unit tests only for fragile pure logic or past bugs.

## ML Reliability

- No fake green: do not skip/xfail/edit tests to hide failure.
- Root cause first: trace NaN, shape mismatch, metric jump, data error upstream.
- Guard leakage: splits, labels, normalization fit scope, duplicate samples.
- Reproducible runs: command, config, seed, data version, commit, checkpoint.
- Metrics contract: define metric before training; compare against baseline.

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
