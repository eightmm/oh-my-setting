# BENCHMARKS

Throughput / memory baselines. Catch perf regressions early.

## Train

| Hardware | Batch | Precision | Tokens/s or Samples/s | Step time (ms) | GPU mem (GB) | Commit | Date |
|----------|-------|-----------|-----------------------|----------------|--------------|--------|------|
|          |       |           |                       |                |              |        |      |

## Inference

| Hardware | Batch | Precision | Latency (ms) | Throughput | GPU mem (GB) | Commit | Date |
|----------|-------|-----------|--------------|------------|--------------|--------|------|
|          |       |           |              |            |              |        |      |

## Measurement Protocol

- Warmup: N steps (discarded)
- Measure: M steps, report median + p95
- Same data shard / fixed input length
- `torch.cuda.synchronize()` before/after timing

## Regression Policy

- > 10% throughput drop or > 10% memory increase vs last entry -> investigate before merge.
- Record root cause in `EXPERIMENTS.md`.

## Update Triggers

Architecture, precision, batch policy, dependency major version, or hardware change -> add new row.
