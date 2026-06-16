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
  — valid ONLY when every rank has the same number of valid elements; for
  variable-size inputs use the element-weighted reduction below.
- **Cleanup**: `dist.destroy_process_group()` in finally block

## Loss Reduction & Masking (variable-size inputs)

Chem-bio batches have variable atoms/residues/pairs and heavy padding. A plain
`.mean()` silently weights the loss by structure size and lets padding or missing
labels contribute gradients — curves look healthy while the model fits the wrong
thing. Reduce explicitly against a declared denominator.

- Declare the unit: per-`sample` / `atom` / `residue` / `pair` / `positive`.
  Mask invalid/padded elements BEFORE reduction; never `.mean()` a padded tensor.
- **Multitask/sparse labels**: build a `label_mask`; never fill missing labels
  with 0 — the model regresses unmeasured targets toward 0 and corrupts the
  measured heads. NaN→0 is a silent bug, not imputation.
- **DDP**: normalize by total valid elements across ranks (all_reduce the count),
  not per-rank batch count or `/ world_size` — else ranks holding smaller
  structures dominate the gradient and training skews.
- Pooling / readout / attention must be mask-weighted too (extends ESM
  mask-pooling to graph readout and per-residue/atom losses).

```python
loss = (per_elem_loss * mask).sum()
n = mask.sum()
if world_size > 1:
    dist.all_reduce(loss); dist.all_reduce(n)   # both SUM
loss = loss / n.clamp(min=1)
```

## Reusable Source Blocks

When a project needs a known user-owned implementation such as an equivariant
GNN block, prefer the code-source registry over ad hoc copying:

```bash
~/.oh-my-setting/scripts/github-source.sh profile --user <github-user>
~/.oh-my-setting/scripts/github-source.sh discover --user <github-user> --query equivariant
~/.oh-my-setting/scripts/code-source.sh add flowfrag-equivariant \
  --repo <github-user>/flowfrag \
  --path flowfrag/equivariant.py \
  --target src/models/equivariant.py \
  --tags ml,gnn,equivariant \
  --license own-code
~/.oh-my-setting/scripts/code-source.sh fetch flowfrag-equivariant
```

After fetching, adapt imports, device/dtype assumptions, tests, and config names
for the current project. Treat provenance as required context, not proof that
the copied code is correct here.

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

**Verify symmetry, don't assume it.** One coordinate-dependent op (absolute
linear layer, uncentered coords, global projection) silently breaks SE(3)
equivariance — the model compiles and trains but fails under rotation. Add a
unit test: apply a random rotation+translation, assert invariant outputs are
unchanged and equivariant outputs (forces, vectors) rotate with it. Use float64
for the check (bf16/fp16 tolerances mask real bugs). Caveat: if inputs are
centered/Kabsch-aligned upstream, the test can pass on a non-equivariant model —
test the raw network.

```python
def verify_equivariance(model, batch, atol=1e-5):
    from scipy.spatial.transform import Rotation
    R = torch.tensor(Rotation.random().as_matrix(), dtype=torch.float64)
    t = torch.randn(1, 3, dtype=torch.float64)
    rot = batch.clone(); rot.pos = batch.pos.double() @ R.T + t
    a, b = model(batch), model(rot)
    assert torch.allclose(a.energy, b.energy, atol=atol)          # invariant
    assert torch.allclose(a.forces.double() @ R.T, b.forces, atol=atol)  # equivariant
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
