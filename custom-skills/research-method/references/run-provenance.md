# Run Provenance

Use the run ledger for lightweight history. Use a capsule when exact state or
output provenance matters; it captures commit, uncommitted diff, config, env,
seed, output fingerprints, log, and result.

```bash
oms run-ledger --note baseline --metrics metrics.json -- uv run python train.py
oms run-capsule run --note baseline --config config.yaml --seed 7 \
  --metrics metrics.json --output ckpt/last.pt -- uv run python train.py
oms run-capsule reproduce <id>
oms run-capsule verify <id>
oms run-capsule whence ckpt/last.pt
oms run diff <run-a> <run-b>
```

Record the outcome metric in the same run row. A result without config, seed,
data boundary, and commit/diff cannot support a reproducible conclusion.

Use `whence` to trace an output fingerprint to the producing run. Use `diff`
to compare code/env/config/seed changes alongside metric deltas; if several
variables changed, do not attribute the result to one of them.

When a run overturns an earlier conclusion, record which row it supersedes.
