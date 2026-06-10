---
name: ml-training
description: >
  PyTorch training defaults: Muon + AdamW optimizer split, trapezoidal (WSD)
  LR schedule, DDP multi-GPU setup, checkpoint format, CUDA environment
  variables, and cuEquivariance for equivariant models. Use when writing or
  reviewing training scripts, optimizers, schedulers, multi-GPU/DDP code,
  or checkpoint save/load.
---

Training defaults for ML projects. Loaded on demand — these are not global
rules; apply them when working on training code.

## Environment

```bash
export CUDA_VISIBLE_DEVICES=0
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
export HF_HOME=~/.cache/huggingface
export TOKENIZERS_PARALLELISM=false
export CUBLAS_WORKSPACE_CONFIG=:4096:8
```

## Optimizer: Muon + AdamW (PyTorch 2.9+)

- **Muon**: 2D+ params (Linear, Conv weights) -- `lr=0.02, momentum=0.95`
- **AdamW**: 1D params (bias, LayerNorm, Embedding) -- `lr=3e-4, betas=(0.9, 0.95)`

```python
def configure_optimizers(model: nn.Module, lr: float = 3e-4, muon_lr: float = 0.02, weight_decay: float = 0.01) -> list:
    muon_params, adamw_params = [], []
    for name, param in model.named_parameters():
        if not param.requires_grad:
            continue
        (muon_params if param.ndim >= 2 else adamw_params).append(param)
    optimizers = []
    if muon_params:
        optimizers.append(Muon(muon_params, lr=muon_lr, momentum=0.95))
    if adamw_params:
        optimizers.append(AdamW(adamw_params, lr=lr, weight_decay=weight_decay, betas=(0.9, 0.95)))
    return optimizers
```

## LR Scheduler: Trapezoidal (WSD)

warmup(10%) -> stable(60%) -> cooldown(30%)

```python
def get_trapezoidal_scheduler(optimizer, total_steps: int, warmup_ratio: float = 0.1, cooldown_ratio: float = 0.3):
    warmup = int(total_steps * warmup_ratio)
    cooldown = int(total_steps * cooldown_ratio)
    stable = total_steps - warmup - cooldown
    def lr_lambda(step):
        if step < warmup: return step / warmup
        elif step < warmup + stable: return 1.0
        else: return 1.0 - (step - warmup - stable) / cooldown
    return LambdaLR(optimizer, lr_lambda)
```

## Multi-GPU Training (DDP)

```bash
uv run torchrun --standalone --nproc_per_node=2 scripts/train.py --config configs/train.yaml
CUDA_VISIBLE_DEVICES=0,1 uv run torchrun --standalone --nproc_per_node=2 scripts/train.py
```

**CRITICAL**: `torch.cuda.set_device()` BEFORE `dist.init_process_group()`

```python
def setup_ddp():
    rank = int(os.environ.get('RANK', 0))
    local_rank = int(os.environ.get('LOCAL_RANK', 0))
    world_size = int(os.environ.get('WORLD_SIZE', 1))
    if world_size > 1:
        torch.cuda.set_device(local_rank)
        dist.init_process_group(backend='nccl')
    return rank, local_rank, world_size
```

```python
model = DDP(model, device_ids=[local_rank], output_device=local_rank,
            find_unused_parameters=False, gradient_as_bucket_view=True, static_graph=True)
```

DataLoader:

- `DistributedSampler(dataset, num_replicas=world_size, rank=rank, shuffle=True, seed=42)`
- `batch_size_per_gpu = batch_size // world_size`
- **CRITICAL**: call `train_sampler.set_epoch(epoch)` every epoch

Key patterns:

- **Checkpoint save**: rank 0 only. `model.module.state_dict()` for DDP-wrapped model
- **Checkpoint load**: all ranks. `map_location=f'cuda:{local_rank}'`
- **Validation**: `dist.all_reduce` for scalar metrics, `dist.all_gather` for tensors
- **Barrier**: `dist.barrier()` before/after validation and checkpoint save
- **Logging/wandb/tqdm**: rank 0 only
- **Loss averaging**: `dist.all_reduce(loss, op=ReduceOp.SUM)` then `/ world_size`
- **Cleanup**: `dist.destroy_process_group()` in finally block

## Equivariant Models: cuEquivariance

```python
import cuequivariance as cue
import cuequivariance_torch as cuet

irreps_in = cue.Irreps("O3", "32x0e + 32x1o")
linear = cuet.EquivariantLinear(irreps_in=irreps_in, irreps_out=irreps_out, layout=cue.ir_mul)
tp = cuet.EquivariantTensorProduct(irreps_in1=..., irreps_in2=..., irreps_out=..., layout=cue.ir_mul)
sh = cuet.SphericalHarmonics(irreps=..., normalize=True)
# e3nn interop: cue.Irreps("O3", str(e3nn_irreps))
```

## Checkpoint

Keys: `model_state_dict`, `optimizer_state_dicts`, `scheduler_state_dicts`, `epoch`, `step`, `metrics`.

```python
# Save
torch.save({"epoch": epoch, "step": step, "model_state_dict": model.state_dict(),
    "optimizer_state_dicts": [o.state_dict() for o in optimizers],
    "scheduler_state_dicts": [s.state_dict() for s in schedulers], "metrics": metrics},
    save_dir / f"checkpoint_epoch{epoch:04d}_step{step:07d}.pt")
# Load
ckpt = torch.load(path, map_location="cpu", weights_only=True)
model.load_state_dict(ckpt["model_state_dict"])
# weights_only=False only for checkpoints you created yourself (arbitrary code
# execution risk on untrusted files); keep saved objects weights_only-safe.
```
