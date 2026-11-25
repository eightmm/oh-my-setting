#!/usr/bin/env bash
# Preemption-aware Slurm batch script. Copy it, fill the TODO lines, then check
# it without submitting anything:  oms slurm-lint job.sh
#
# What Slurm does and what this script does are separate. Slurm decides whether
# a preempted job goes back in the queue (--requeue below, or the cluster's
# JobRequeue setting) and when the warning signal arrives (--signal below).
# This script only reacts: it passes the signal to the training process, waits
# for it to write a checkpoint, and on the next start resumes only from a
# provenance-checked checkpoint. Exit codes after preemption differ between clusters, so none is assumed.

#SBATCH --job-name=TODO-name
#SBATCH --time=TODO-D-HH:MM:SS
#SBATCH --output=logs/%x-%j.out
#SBATCH --requeue
#SBATCH --signal=B:USR1@120
# TODO: account, queue, QOS, CPUs, memory and GPUs, copied from PROJECT.md or
# `oms snapshot --cluster`. An array needs a concurrency limit, e.g. 0-99%8.

set -euo pipefail
cd "${SLURM_SUBMIT_DIR:-.}"
mkdir -p logs

CKPT_DIR="${CKPT_DIR:-checkpoints/${SLURM_JOB_NAME:-job}}"
mkdir -p "$CKPT_DIR"

# This template requires OMS on PATH and the canonical/shared project checkout
# (registered contract and original run ledger) on the execution host. A Git
# bundle alone does not carry ignored .oms state or install OMS. For a remote
# Git-only checkout, collect the checkpoint to the canonical project, verify it
# there, then use the project's explicit staged resume path; do not copy a ledger
# to manufacture authority on the remote host.
# The application writes this manifest atomically after writing its checkpoint.
# It binds the registered experiment/run, frozen inputs, seed, step, RNG-state
# identity and exact checkpoint bytes. OMS never loads model data. Do not select
# by mtime: a newer file can belong to another run or configuration.
resume=""
if [ -n "${OMS_CHECKPOINT_MANIFEST:-}" ]; then
  : "${OMS_EXPERIMENT_ID:?checkpoint resume needs the registered experiment id}"
  : "${OMS_EXPERIMENT_RUN_ID:?checkpoint resume needs the exact run id}"
  resume=$(oms runtime experiment checkpoint \
    --id "$OMS_EXPERIMENT_ID" --run-id "$OMS_EXPERIMENT_RUN_ID" \
    --manifest "$OMS_CHECKPOINT_MANIFEST" --path-only)
elif [ "${SLURM_RESTART_COUNT:-0}" -gt 0 ]; then
  echo "requeued run has no checkpoint provenance manifest; refusing a silent fresh start" >&2
  exit 75
fi
echo "job ${SLURM_JOB_ID:-local} restart ${SLURM_RESTART_COUNT:-0}: resume from ${resume:-nothing}"

# --signal=B:USR1@120 delivers USR1 to this shell 120 seconds before the time
# limit or a preemption. Bash runs a trap only between commands, so the work
# runs in the background and the shell waits on it.
child=""
forward() {
  echo "signal $1: asking the run to checkpoint"
  [ -z "$child" ] || kill -s "$1" "$child" 2>/dev/null || true
}
trap 'forward USR1' USR1
trap 'forward TERM' TERM

# TODO: the training command. It must save into "$CKPT_DIR" when it receives
# USR1 (and periodically), and accept the checkpoint to resume from.
srun TODO-python train.py --checkpoint-dir "$CKPT_DIR" ${resume:+--resume "$resume"} &
child=$!

# A trapped signal interrupts wait; wait again so the checkpoint can finish.
while :; do
  status=0
  wait "$child" || status=$?
  if [ "$status" -le 128 ] || ! kill -0 "$child" 2>/dev/null; then
    break
  fi
done
exit "$status"
