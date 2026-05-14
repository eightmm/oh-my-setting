#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${OH_MY_SETTING_SLURM_REF:-$ROOT/custom-skills/slurm-hpc/references/cluster.generated.md}"
RAW_DIR="${OH_MY_SETTING_SLURM_RAW_DIR:-$ROOT/local/slurm}"
WRITE_RAW="${OH_MY_SETTING_SLURM_WRITE_RAW:-0}"

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

partition_table() {
  if ! has_cmd sinfo; then
    printf 'missing command: `sinfo`\n'
    return 0
  fi

  sinfo -h -o "%P|%a|%l|%D|%G|%m|%c|%N" 2>/dev/null |
    awk -F'|' '
      BEGIN {
        print "Name|State|Time|Nodes|CPUs/node|Mem|GPU/GRES|NodeList"
        print "---|---|---|---:|---:|---:|---|---"
      }
      {
        name=$1
        sub(/\*/, "", name)
        gres=$5
        if (gres == "(null)") gres="-"
        print name "|" $2 "|" $3 "|" $4 "|" $7 "|" $6 "|" gres "|" $8
      }
    '
}

partition_summary() {
  if ! has_cmd sinfo; then
    printf -- '- Slurm not detected.\n'
    return 0
  fi

  sinfo -h -o "%P|%l|%D|%G|%c|%N" 2>/dev/null |
    awk -F'|' '
      {
        name=$1
        sub(/\*/, "", name)
        gres=$4
        class="CPU"
        if (gres != "(null)" && gres != "") class="GPU"
        split(name, parts, "_")
        family=parts[1]
        limit=$2
        nodes[family "|" class] += $3
        cpus[family "|" class] = $5
        limits[family "|" class, limit] = 1
        if (gres != "(null)" && gres != "") greses[family "|" class, gres] = 1
      }
      END {
        for (key in nodes) {
          split(key, p, "|")
          t=""
          for (idx in limits) {
            split(idx, parts, SUBSEP)
            if (parts[1] == key) t = t (t ? ", " : "") parts[2]
          }
          g=""
          for (idx in greses) {
            split(idx, parts, SUBSEP)
            if (parts[1] == key) g = g (g ? "; " : "") parts[2]
          }
          if (g == "") g = "-"
          printf "- %s %s: nodes=%d, cpus/node=%s, time=%s, gres=%s\n", p[1], p[2], nodes[key], cpus[key], t, g
        }
      }
    ' | sort
}

default_partition() {
  if has_cmd sinfo; then
    sinfo -h -o "%P" 2>/dev/null | awk '/\*/ { gsub(/\*/, "", $1); print $1; exit }'
  fi
}

queue_summary() {
  if has_cmd squeue; then
    squeue -u "${USER:-}" -o "%.18i %.9P %.32j %.2t %.10M %.6D %R" 2>/dev/null || true
  else
    printf 'missing command: `squeue`\n'
  fi
}

mkdir -p "$(dirname "$OUT")"

{
  printf '# Local Slurm Cluster\n\n'
  printf 'Generated: %s\n' "$(date -Iseconds)"
  printf 'Host: %s\n' "$(hostname 2>/dev/null || true)"
  printf 'User: %s\n\n' "${USER:-unknown}"

  printf 'Use this as local reference only. Do not commit generated cluster details.\n\n'

  printf '## Summary\n\n'
  partition_summary
  printf '\n'

  printf '## Partitions\n\n'
  partition_table
  printf '\n\n'

  printf '## Current Queue\n\n'
  queue_summary
  printf '\n'

  printf '## Agent Defaults To Fill\n\n'
  printf -- '- Preferred partition: %s\n' "$(default_partition)"
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
  has_cmd sinfo && sinfo -Nel > "$RAW_DIR/nodes-summary.txt" 2>&1 || true
  has_cmd scontrol && scontrol show partition > "$RAW_DIR/partitions.txt" 2>&1 || true
  has_cmd scontrol && scontrol show nodes > "$RAW_DIR/nodes.txt" 2>&1 || true
fi

echo "wrote $OUT"
if [ "$WRITE_RAW" = "1" ]; then
  echo "wrote raw Slurm outputs under $RAW_DIR"
fi
