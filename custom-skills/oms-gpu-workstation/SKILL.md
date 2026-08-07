---
name: oms-gpu-workstation
description: Discipline for launching training or inference on a local GPU machine (not a Slurm cluster): check VRAM and running processes before launching, serialize GPU jobs through the tsp queue instead of racing other work, read the machine snapshot for hardware limits, and triage CUDA OOM systematically.
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

## CUDA OOM triage (in order)

1. Confirm nothing else holds VRAM (`nvidia-smi`); reclaim zombie processes.
2. Reduce batch size / enable gradient accumulation.
3. Mixed precision or activation checkpointing.
4. Only then consider model sharding or a smaller model — and record the
   working configuration in the run ledger row.
