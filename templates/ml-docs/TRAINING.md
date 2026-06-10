# TRAINING

Standard recipe. Deviations must be noted in `EXPERIMENTS.md`.

## Optimizer (Muon + AdamW)

- Muon: 2D+ params (Linear, Conv weights) — `lr=0.02, momentum=0.95`
- AdamW: 1D params (bias, LayerNorm, Embedding) — `lr=3e-4, betas=(0.9, 0.95), weight_decay=0.01`

```python
def configure_optimizers(model, lr=3e-4, muon_lr=0.02, weight_decay=0.01):
    muon_params, adamw_params = [], []
    for _, p in model.named_parameters():
        if not p.requires_grad:
            continue
        (muon_params if p.ndim >= 2 else adamw_params).append(p)
    optimizers = []
    if muon_params:
        optimizers.append(Muon(muon_params, lr=muon_lr, momentum=0.95))
    if adamw_params:
        optimizers.append(AdamW(adamw_params, lr=lr, weight_decay=weight_decay, betas=(0.9, 0.95)))
    return optimizers
```

## LR Schedule (Trapezoidal / WSD)

warmup(10%) -> stable(60%) -> cooldown(30%)

```python
def get_trapezoidal_scheduler(optimizer, total_steps, warmup_ratio=0.1, cooldown_ratio=0.3):
    warmup = int(total_steps * warmup_ratio)
    cooldown = int(total_steps * cooldown_ratio)
    stable = total_steps - warmup - cooldown
    def lr_lambda(step):
        if step < warmup: return step / warmup
        if step < warmup + stable: return 1.0
        return 1.0 - (step - warmup - stable) / cooldown
    return LambdaLR(optimizer, lr_lambda)
```

## Precision

- AMP: `bf16` (preferred) / `fp16` (with GradScaler)
- TF32: `torch.backends.cuda.matmul.allow_tf32 = True`

## Stability

- Grad clip: `clip_grad_norm_(..., max_norm=1.0)`
- NaN/Inf guard: abort step, log, do not silently skip
- Loss scale (fp16 only): dynamic via `GradScaler`

## Batch / Steps

- Global batch:
- Per-GPU batch:
- Grad accumulation:
- Total steps / epochs:

## DDP (multi-GPU)

```bash
uv run torchrun --standalone --nproc_per_node=<N> scripts/train.py --config configs/train.yaml
```

- `torch.cuda.set_device(local_rank)` BEFORE `dist.init_process_group`
- `DistributedSampler.set_epoch(epoch)` every epoch
- Checkpoint save: rank 0 only; load: all ranks with `map_location=f'cuda:{local_rank}'`
- Loss avg: `all_reduce(SUM) / world_size`

## Verification

- Smoke: 1 step on 1 batch passes, loss finite, grads finite
- Overfit: tiny subset (8-32 samples) reaches near-zero loss
- Full: matches baseline in `EVALUATION.md`

## Update Triggers

Optimizer/schedule/precision/batch policy change -> note in `EXPERIMENTS.md` + bump training version.
