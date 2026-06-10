# REPRODUCIBILITY

Single-command repro for any reported number.

## Repro Command

```bash
git checkout <commit>
uv sync
uv run python scripts/train.py --config configs/<exp>.yaml --seed <seed>
uv run python scripts/eval.py  --config configs/<exp>.yaml --ckpt <path>
```

## Determinism Pins

- Global seed: set in config; passed to data, model, augmentation, dataloader worker_init
- `torch.manual_seed`, `np.random.seed`, `random.seed`, `torch.cuda.manual_seed_all`
- `torch.use_deterministic_algorithms(True)` (note perf cost)
- `cudnn.benchmark = False`, `cudnn.deterministic = True`
- DataLoader: `generator=torch.Generator().manual_seed(seed)`, `worker_init_fn` seeds workers

## What Pins a Run

| Element | Pinned by |
|---------|-----------|
| Code | git commit |
| Deps | `uv.lock` |
| Data | hash in `DATA.md` |
| Hardware | `~/.oh-my-setting/local/machine.md` |
| Hyperparams | config yaml in `configs/` |
| Seed | `--seed` arg |

## Known Non-Determinism

- Some CUDA kernels (atomic add, scatter) — document if used
- Mixed precision: results may differ across GPU generations
- Multi-GPU reduction order

## Update Triggers

Any new source of randomness -> add pin + note here.
