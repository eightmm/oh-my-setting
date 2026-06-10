# CONFIGS

Config schema reference. All experiments live in `configs/*.yaml`.

## Top-Level Keys

| Key | Type | Default | Range | Description |
|-----|------|---------|-------|-------------|
| `seed` | int | 42 | >= 0 | global seed |
| `data.path` | str | — | — | dataset root |
| `data.batch_size` | int | — | > 0 | global batch |
| `data.num_workers` | int | 4 | >= 0 | dataloader workers |
| `model.name` | str | — | — | architecture key |
| `model.<...>` | — | — | — | architecture-specific |
| `train.lr` | float | 3e-4 | > 0 | AdamW lr |
| `train.muon_lr` | float | 0.02 | > 0 | Muon lr |
| `train.weight_decay` | float | 0.01 | >= 0 |  |
| `train.warmup_ratio` | float | 0.1 | [0,1] |  |
| `train.cooldown_ratio` | float | 0.3 | [0,1] |  |
| `train.grad_clip` | float | 1.0 | > 0 |  |
| `train.precision` | str | bf16 | bf16/fp16/fp32 |  |
| `train.total_steps` | int | — | > 0 |  |
| `log.project` | str | — | — | wandb project |
| `log.mode` | str | online | online/offline/disabled |  |
| `ckpt.dir` | str | outputs/checkpoints | — |  |
| `ckpt.save_every` | int | — | > 0 | steps |

## Override Rules

- CLI override wins over yaml.
- yaml override wins over default.
- No silent defaults for required keys — assert at startup.

## Adding a Key

1. Add to default config (`configs/default.yaml`).
2. Document here.
3. Validate at script entry with `assert`.

## Update Triggers

New config key, default change, or removed key -> update this file.
