# Checkpoints

Save portable model keys without a DDP `module.` prefix:

```python
raw_model = model.module if hasattr(model, "module") else model
model_state = raw_model.state_dict()
```

Rank 0 writes the file. Every rank loads from CPU first, restores the unwrapped
model, then moves or wraps it according to the startup contract.

At minimum record:

- model state;
- optimizer and scheduler state for every optimizer/scheduler;
- AMP scaler state when used;
- epoch, global optimizer step, and best-metric state;
- configuration/data identifiers and code commit;
- RNG and sampler state when exact next-step equivalence is required.

Use `torch.load(path, map_location="cpu", weights_only=True)` for portable
tensor-only checkpoints. Permit `weights_only=False` only for a trusted file
created under the same controlled code boundary.

Regression checks:

1. unwrapped save -> unwrapped load;
2. wrapped save -> unwrapped load with no key rewriting;
3. resumed next update matches uninterrupted training when exact resume is a
   stated requirement;
4. partial/shape-mismatched loads fail explicitly unless migration is designed.
