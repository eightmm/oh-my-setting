# Distributed Training

Initialize the device before the process group:

```python
local_rank = int(os.environ["LOCAL_RANK"])
torch.cuda.set_device(local_rank)
dist.init_process_group(backend="nccl")
```

Use `DistributedSampler`, call `sampler.set_epoch(epoch)`, and define whether
the configured batch size is per-rank or global. Rank 0 owns user-facing logs
and checkpoint writes; all ranks participate in required collectives.

Default DDP options should remain conservative. Enable `static_graph` only
after confirming the autograd graph and used-parameter set are invariant.

## Unequal Valid Counts

DDP averages gradients across `world_size`. For local differentiable loss sum
`local_loss_sum` and detached globally reduced count `global_valid_count`, use:

```text
backward_loss = world_size * local_loss_sum / global_valid_count
```

Do not `all_reduce` the differentiable loss tensor. All-reduce a detached count;
for reporting, separately all-reduce detached local loss sums and counts. This
makes DDP's gradient average equal the single-process global masked mean even
when ranks have different atom/residue/pair counts.

Use `all_reduce` for scalar sums/counts and `all_gather` only when the complete
tensor is required. Avoid barriers around ordinary DDP operations; add them
only for a demonstrated ordering requirement. Destroy the process group in a
`finally` block.
