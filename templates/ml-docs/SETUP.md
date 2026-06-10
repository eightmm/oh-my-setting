# SETUP

Environment lock for reproducibility. Pin everything that affects training output.

## Tooling

- Package manager: `uv` (no pip/conda direct calls)
- Python: see `pyproject.toml` `requires-python`
- Install: `uv sync`
- Run: `uv run <command>`

## Hardware Targets

- GPU model:
- CUDA driver / runtime:
- cuDNN:
- RAM / VRAM minimum:

## Env Vars

```bash
export CUDA_VISIBLE_DEVICES=0
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
export HF_HOME=~/.cache/huggingface
export TOKENIZERS_PARALLELISM=false
export CUBLAS_WORKSPACE_CONFIG=:4096:8
```

## Determinism

- Global seed:
- `torch.use_deterministic_algorithms(True)`: yes/no (perf tradeoff)
- `cudnn.benchmark`: false for repro, true for speed

## First Run

```bash
uv sync
uv run python scripts/smoke.py   # data + model + 1 step
```

## Update Triggers

GPU/driver/CUDA, Python base, or core dep version change -> update this file + machine snapshot.
