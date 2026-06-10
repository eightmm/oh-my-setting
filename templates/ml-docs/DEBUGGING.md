# DEBUGGING

Common failure modes and first-pass fixes.

## NaN / Inf Loss

1. Check input: `assert torch.isfinite(x).all()`
2. Check labels: range, no NaN
3. Lower LR by 10x; if disappears -> LR/init issue
4. Disable AMP; if disappears -> precision issue (fp16 overflow; try bf16)
5. Check loss function: log(0), div by 0, sqrt of negative
6. Grad clip on; check `grad_norm` before clip

## OOM

1. Reduce batch; use grad accumulation to keep effective batch
2. `PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True`
3. Enable activation checkpointing
4. Switch fp32 -> bf16
5. `del` unused tensors; `torch.cuda.empty_cache()` only in eval, not training loop
6. Profile with `torch.cuda.memory._record_memory_history()`

## DDP Hang

- Mismatched collectives across ranks — log entry/exit of every `all_reduce` / `all_gather`
- `find_unused_parameters=True` only if needed (perf cost); fix unused params instead
- `NCCL_DEBUG=INFO` for transport errors
- Timeout: `dist.init_process_group(timeout=timedelta(minutes=30))`

## Shape Mismatch

- Print shapes at boundary: dataloader out, model in, model out, loss in
- Do not pad/truncate to hide it — fix root cause

## Slow Training

- Profile: `torch.profiler.profile(activities=[CPU, CUDA])`
- Check dataloader: `num_workers`, `pin_memory=True`, `persistent_workers=True`
- Check `cudnn.benchmark = True` (if shapes are static)
- Verify AMP actually active (`autocast` context)
- Check no `.item()` / `.cpu()` in hot loop (sync point)

## Metric Regression

- Diff config vs last good run
- Re-run baseline on current code; if breaks, code regression
- Check data version + preprocessing version
- Check seed pinned

## Resume Broken

- Optimizer/scheduler state mismatch -> reset them, keep model
- LR not resuming -> scheduler state not saved/loaded
- wandb resume: pass `id=run_id, resume="allow"`

## When Stuck

1. Minimal repro: smallest data + 1 step
2. Bisect last known good commit
3. Open issue with: command, config diff, full stack, env (`uv run python -m torch.utils.collect_env`)
