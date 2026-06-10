# MODEL

Architecture contract. Input/output shapes are public API.

## Overview

- Family / paper:
- Task type:
- Parameter count:
- FLOPs (per forward, batch=1):

## Input

- Shape: `[B, ...]`
- Dtype:
- Required preprocessing:

## Output

- Shape: `[B, ...]`
- Dtype:
- Semantics (logits / probs / regression):

## Architecture

```text
Input
  -> Block A (...)
  -> Block B (...)
  -> Head
Output
```

- Layer-by-layer table (or link to `src/<pkg>/models/`):

## Assumptions / Invariants

- Equivariance / symmetry:
- Padding / masking convention:
- Variable-length handling:

## Smoke Test

```bash
uv run python scripts/check_model.py
```

- Forward pass, loss contract, checkpoint round-trip, inference output shape.

## Update Triggers

Any architectural change (layer count, dim, activation, normalization, head) -> bump model version + invalidate ckpts.
