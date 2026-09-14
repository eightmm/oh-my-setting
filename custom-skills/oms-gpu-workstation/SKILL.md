---
name: oms-gpu-workstation
description: Safely schedule local GPU training or inference and diagnose CUDA OOM using OMS resource snapshots and the GPU queue. Not for Slurm jobs.
compatibility: Requires a local NVIDIA GPU (nvidia-smi on PATH); job serialization uses the tsp task spooler.
---

# GPU Workstation

A local GPU box is a shared, unscheduled resource: the discipline the cluster
scheduler would impose is yours to apply.

## Before launching

1. `nvidia-smi` once: free VRAM, running processes, and whose they are. Do
   not start a job that plainly does not fit or that races a live run.
2. Static hardware facts (GPU model, VRAM size, CPU/RAM, disks) are in the
   machine snapshot: `local/machine.md` under the oh-my-setting install root
   (`oms status` reports the root; refresh with `oms snapshot`).
   Read it instead of re-deriving the hardware every session.
3. If another job is running or queued, do not wait-loop in the session —
   enqueue via the GPU queue below and report the queue position.

## Serializing jobs

- `oms tsp-queue` wraps the task-spooler GPU queue: submit long runs there so
  concurrent sessions serialize instead of OOM-ing each other.
- Long or expensive runs also go through `oms run-ledger` so parallel agents
  see them and duplicates are caught before they burn hours.

## CUDA OOM diagnosis

Distinguish competing processes, retained tensors, input/padding growth and
the task's real memory requirement before changing the experiment. Use live
resource state and the failing configuration; an old snapshot cannot prove
that resources are free. Never terminate another job or an unverified PID.
Clean up only a confirmed task-owned process within existing authority.

Choose the smallest remedy supported by that evidence. Queuing or correcting
an unintended allocation may suffice. Batch size, gradient accumulation,
precision, checkpointing and sharding are options, not a fixed ladder or
assumed equivalents. Check their effect on the task's numerical behavior,
effective batch, stochastic state and resume contract before adopting them.
Changing model, data, precision or a scientific comparison beyond the approved
contract requires user direction; do not silently shrink the problem to fit.

Validate the selected remedy with the smallest authorized representative run.
Record the configuration and what was actually verified in the existing run
ledger. A memory-only check does not establish scientific or performance parity.
