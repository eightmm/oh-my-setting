#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${OH_MY_SETTING_SLURM_REF:-$ROOT/custom-skills/slurm-hpc/references/cluster.generated.md}"
RAW_DIR="${OH_MY_SETTING_SLURM_RAW_DIR:-$ROOT/local/slurm}"
WRITE_RAW="${OH_MY_SETTING_SLURM_WRITE_RAW:-0}"

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

run_or_note() {
  local label="$1"
  shift

  printf '## %s\n\n' "$label"
  if has_cmd "$1"; then
    "$@" 2>&1 || true
  else
    printf 'missing command: `%s`\n' "$1"
  fi
  printf '\n'
}

mkdir -p "$(dirname "$OUT")"

{
  printf '# Local Slurm Cluster\n\n'
  printf 'Generated: %s\n' "$(date -Iseconds)"
  printf 'Host: %s\n' "$(hostname 2>/dev/null || true)"
  printf 'User: %s\n\n' "${USER:-unknown}"

  printf 'Use this as local reference only. Do not commit generated cluster details.\n\n'

  run_or_note "Partitions" sinfo -o "%P|%a|%l|%D|%G|%m|%c|%N"
  run_or_note "Node Summary" sinfo -Nel
  run_or_note "Partition Details" scontrol show partition
  run_or_note "My Queue" squeue -u "${USER:-}" -o "%.18i %.9P %.40j %.8u %.2t %.10M %.6D %R"

  printf '## Agent Defaults To Fill\n\n'
  printf -- '- Preferred partition:\n'
  printf -- '- Account:\n'
  printf -- '- CPU default:\n'
  printf -- '- GPU default:\n'
  printf -- '- Memory default:\n'
  printf -- '- Time default:\n'
  printf -- '- Log path:\n'
  printf -- '- Checkpoint path:\n'
} > "$OUT"

if [ "$WRITE_RAW" = "1" ]; then
  mkdir -p "$RAW_DIR"
  has_cmd sinfo && sinfo > "$RAW_DIR/sinfo.txt" 2>&1 || true
  has_cmd scontrol && scontrol show partition > "$RAW_DIR/partitions.txt" 2>&1 || true
  has_cmd scontrol && scontrol show nodes > "$RAW_DIR/nodes.txt" 2>&1 || true
fi

echo "wrote $OUT"
if [ "$WRITE_RAW" = "1" ]; then
  echo "wrote raw Slurm outputs under $RAW_DIR"
fi
