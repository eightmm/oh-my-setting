#!/usr/bin/env bash
# Preemption-aware Slurm batch script. Copy it, fill the TODO lines, then check
# it without submitting anything:  oms slurm-lint job.sh
#
# What Slurm does and what this script does are separate. Slurm decides whether
# a preempted job goes back in the queue (--requeue below, or the cluster's
# JobRequeue setting) and when the warning signal arrives (--signal below).
# This script only reacts: it passes the signal to the training process, waits
# for it to write a checkpoint, and on the next start resumes from the newest
# one. Exit codes after preemption differ between clusters, so none is assumed.

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

# Newest checkpoint, if any: a requeued job starts this script again from the
# top with the same job id, so resuming is this lookup and nothing else.
resume=""
for candidate in "$CKPT_DIR"/*; do
  [ -f "$candidate" ] || continue
  if [ -z "$resume" ] || [ "$candidate" -nt "$resume" ]; then
    resume="$candidate"
  fi
done
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
status=0
wait "$child" || status=$?
if kill -0 "$child" 2>/dev/null; then
  status=0
  wait "$child" || status=$?
fi
exit "$status"
