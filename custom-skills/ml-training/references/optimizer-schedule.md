# Optimizers and Schedules

Start with the project's established optimizer or AdamW. Treat Muon and WSD as
opt-in experiments with a declared baseline; neither is a universal default.

## Parameter Groups

Group by module semantics, not tensor rank. Embedding tables are 2D but should
not be sent to Muon merely because `param.ndim >= 2`. Keep bias, normalization,
embedding, scalar, and explicitly excluded parameters in AdamW. Put only
allowlisted Linear/Conv-like weight matrices in a specialized optimizer.

Verify that every trainable parameter appears exactly once and log group names,
counts, element totals, optimizer, LR, and weight decay.

## Schedule Safety

- Validate `total_steps >= 1` and phase ratios in `[0, 1]` with a sum at most 1.
- Define behavior for zero-length warmup, stable, or cooldown phases.
- Clamp the multiplier to the intended range and test the first/last step.
- Derive `total_steps` after gradient accumulation and distributed sampler
  behavior are known.

For WSD, compute integer phase boundaries first. A zero-length warmup starts at
the stable multiplier; a zero-length cooldown keeps the stable multiplier
through the final update. Never divide by a phase length before checking it is
positive.

Specialized optimizer adoption requires an ablation against the baseline with
the same data order, update count, effective batch size, and evaluation policy.
