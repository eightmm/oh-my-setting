# CHECKPOINTS

Checkpoint contract. Version-pinned to data + model + training.

## Schema

```python
{
    "epoch": int,
    "step": int,
    "model_state_dict": dict,
    "optimizer_state_dicts": list[dict],
    "scheduler_state_dicts": list[dict],
    "metrics": dict,
    "config": dict,           # full training config snapshot
    "data_version": str,      # matches DATA.md version
    "model_version": str,     # matches MODEL.md version
    "commit": str,            # git SHA
}
```

## Save

```python
torch.save({
    "epoch": epoch, "step": step,
    "model_state_dict": model.state_dict(),
    "optimizer_state_dicts": [o.state_dict() for o in optimizers],
    "scheduler_state_dicts": [s.state_dict() for s in schedulers],
    "metrics": metrics, "config": config,
    "data_version": DATA_VERSION, "model_version": MODEL_VERSION,
    "commit": git_sha(),
}, save_dir / f"checkpoint_epoch{epoch:04d}_step{step:07d}.pt")
```

- DDP: rank 0 only, `model.module.state_dict()` if wrapped.

## Load

```python
ckpt = torch.load(path, map_location="cpu", weights_only=False)
assert ckpt["data_version"] == DATA_VERSION, "data version mismatch"
assert ckpt["model_version"] == MODEL_VERSION, "model version mismatch"
model.load_state_dict(ckpt["model_state_dict"])
```

## Retention

- Keep: best (primary metric), last, every N epochs
- Storage path: `outputs/checkpoints/` (gitignored)
- Cleanup policy:

## Compatibility

- Data version bump -> old ckpts incompatible
- Model version bump -> old ckpts incompatible
- Optimizer state may be dropped on resume; document if so

## Update Triggers

Schema change -> bump ckpt format version + migration note.
