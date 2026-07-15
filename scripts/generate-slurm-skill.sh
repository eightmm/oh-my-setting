#!/usr/bin/env bash
set -euo pipefail

# Generate the local Slurm reference for the slurm-hpc skill from this cluster.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${OH_MY_SETTING_SLURM_REF:-$ROOT/custom-skills/slurm-hpc/references/cluster.generated.md}"
RAW_DIR="${OH_MY_SETTING_SLURM_RAW_DIR:-$ROOT/local/slurm}"
WRITE_RAW="${OH_MY_SETTING_SLURM_WRITE_RAW:-0}"
CURRENT_USER="${USER:-$(id -un 2>/dev/null || printf 'unknown')}"

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

TMP_SNAPSHOT="$(mktemp -d "${TMPDIR:-/tmp}/oms-slurm-snapshot.XXXXXX")" || exit 1
trap 'rm -rf "$TMP_SNAPSHOT"' EXIT HUP INT TERM
PARTITION_RECORDS="$TMP_SNAPSHOT/partitions.txt"
NODE_RECORDS="$TMP_SNAPSHOT/nodes.txt"
ASSOC_RECORDS="$TMP_SNAPSHOT/associations.txt"
QOS_RECORDS="$TMP_SNAPSHOT/qos.txt"
ASSOC_COLUMNS='Cluster|Account|User|Partition|DefaultQOS|QOS|GrpTRES|MaxTRES|MaxTRESPJ|MaxJobs|MaxSubmit|MaxWall'
QOS_COLUMNS='Name|Priority|Preempt|PreemptMode|Flags|UsageFactor|GrpTRES|MaxTRESPU|MaxTRESPJ|MaxWall|MaxJobsPU|MaxSubmitPU'

: > "$PARTITION_RECORDS"
: > "$NODE_RECORDS"
: > "$ASSOC_RECORDS"
: > "$QOS_RECORDS"

if has_cmd scontrol; then
  scontrol -o show partition > "$PARTITION_RECORDS" 2>/dev/null || : > "$PARTITION_RECORDS"
  scontrol -o show node > "$NODE_RECORDS" 2>/dev/null || : > "$NODE_RECORDS"
fi

if has_cmd sacctmgr; then
  if ! sacctmgr -nP show assoc user="$CURRENT_USER" \
    format=Cluster,Account,User,Partition,DefaultQOS,QOS,GrpTRES,MaxTRES,MaxTRESPJ,MaxJobs,MaxSubmit,MaxWall \
    > "$ASSOC_RECORDS" 2>/dev/null; then
    : > "$ASSOC_RECORDS"
  fi
  if ! sacctmgr -nP show qos \
    format=Name,Priority,Preempt,PreemptMode,Flags,UsageFactor,GrpTRES,MaxTRESPU,MaxTRESPJ,MaxWall,MaxJobsPU,MaxSubmitPU \
    > "$QOS_RECORDS" 2>/dev/null; then
    QOS_COLUMNS='Name|Priority|GrpTRES|MaxWall'
    sacctmgr -nP show qos format=Name,Priority,GrpTRES,MaxWall \
      > "$QOS_RECORDS" 2>/dev/null || : > "$QOS_RECORDS"
  fi
fi

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
  local value
  if has_cmd sinfo; then
    value="$(sinfo -h -o "%P" 2>/dev/null | awk '/\*/ { gsub(/\*/, "", $1); print $1; exit }')"
    [ -z "$value" ] || {
      printf '%s\n' "$value"
      return 0
    }
  fi
  awk '
    {
      name=""; default=""
      for (i=1; i<=NF; i++) {
        if ($i ~ /^PartitionName=/) { name=$i; sub(/^PartitionName=/, "", name) }
        if ($i ~ /^Default=/) { default=$i; sub(/^Default=/, "", default) }
      }
      if (default == "YES") { print name; exit }
    }
  ' "$PARTITION_RECORDS"
}

partition_record() {
  local part_name="$1"
  awk -v part_name="$part_name" '
    {
      for (i=1; i<=NF; i++) {
        if ($i == "PartitionName=" part_name) { print; exit }
      }
    }
  ' "$PARTITION_RECORDS"
}

record_value() {
  local record="$1"
  local key="$2"
  printf '%s\n' "$record" | awk -v key="$key" '
    {
      prefix=key "="
      for (i=1; i<=NF; i++) {
        if (index($i, prefix) == 1) { print substr($i, length(prefix) + 1); exit }
      }
    }
  '
}

assoc_values() {
  local part_name="$1"
  local column="$2"
  local values
  values="$(awk -F'|' -v part_name="$part_name" -v column="$column" '
    $4 == part_name && $column != "" && !seen[$column]++ { print $column }
  ' "$ASSOC_RECORDS")"
  if [ -z "$values" ]; then
    values="$(awk -F'|' -v column="$column" '
      $4 == "" && $column != "" && !seen[$column]++ { print $column }
    ' "$ASSOC_RECORDS")"
  fi
  printf '%s\n' "$values" | awk 'NF { if (out != "") out=out ", "; out=out $0 } END { print out }'
}

job_default_values() {
  local job_defaults="$1"
  local prefix="$2"
  printf '%s\n' "$job_defaults" | tr ',' '\n' |
    awk -v prefix="$prefix" 'index($0, prefix) == 1 {
      if (out != "") out=out ", "; out=out $0
    } END { print out }'
}

effective_memory_defaults() {
  local record="$1"
  local job_defaults="$2"
  local key value job_value
  {
    for key in DefMemPerCPU DefMemPerNode DefMemPerGPU; do
      value="$(record_value "$record" "$key")"
      [ -z "$value" ] || printf '%s=%s\n' "$key" "$value"
    done
    job_value="$(job_default_values "$job_defaults" DefMem)"
    [ -z "$job_value" ] || printf '%s\n' "$job_value" | tr ',' '\n' | sed 's/^[[:space:]]*//'
  } | awk 'NF && !seen[$0]++ { if (out != "") out=out ", "; out=out $0 } END { print out }'
}

print_record_section() {
  local file="$1"
  local unavailable="$2"
  if [ -s "$file" ]; then
    printf '```text\n'
    cat "$file"
    printf '```\n'
  else
    printf '%s\n' "$unavailable"
  fi
}

print_pipe_section() {
  local columns="$1"
  local file="$2"
  if [ -s "$file" ]; then
    printf '```text\n%s\n' "$columns"
    cat "$file"
    printf '```\n'
  else
    printf 'Unavailable (command missing, accounting disabled, or access denied).\n'
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
  printf 'Generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'Host: %s\n' "$(hostname 2>/dev/null || true)"
  printf 'User: %s\n\n' "${USER:-unknown}"

  printf 'Use this as local reference only. Do not commit generated cluster details.\n\n'

  printf '## Summary\n\n'
  partition_summary
  printf '\n'

  printf '## Partitions\n\n'
  partition_table
  printf '\n\n'

  printf '## Partition Configuration\n\n'
  print_record_section "$PARTITION_RECORDS" 'Unavailable (`scontrol -o show partition` failed or is not installed).'
  printf '\n'

  printf '## Node Configuration\n\n'
  print_record_section "$NODE_RECORDS" 'Unavailable (`scontrol -o show node` failed or is not installed).'
  printf '\n'

  printf '## Associations (current user)\n\n'
  printf 'Source: `sacctmgr -nP show assoc user=%s ...`\n\n' "$CURRENT_USER"
  print_pipe_section "$ASSOC_COLUMNS" "$ASSOC_RECORDS"
  printf '\n'

  printf '## QOS\n\n'
  printf 'Source: `sacctmgr -nP show qos ...`\n\n'
  print_pipe_section "$QOS_COLUMNS" "$QOS_RECORDS"
  printf '\n'

  printf '## Current Queue\n\n'
  queue_summary
  printf '\n'

  default_part="$(default_partition)"
  default_record="$(partition_record "$default_part")"
  job_defaults="$(record_value "$default_record" JobDefaults)"
  account="$(assoc_values "$default_part" 2)"
  default_qos="$(assoc_values "$default_part" 5)"
  [ -n "$default_qos" ] || default_qos="$(record_value "$default_record" QoS)"
  cpu_default="$(job_default_values "$job_defaults" DefCpu)"
  memory_default="$(effective_memory_defaults "$default_record" "$job_defaults")"
  time_default="$(record_value "$default_record" DefaultTime)"

  printf '## Effective Submission Defaults\n\n'
  printf 'Values below are copied from Slurm configuration/associations; missing values are not inferred.\n\n'
  printf -- '- Preferred partiti''on: %s\n' "${default_part:-(not configured)}"
  printf -- '- Account: %s\n' "${account:-(not configured)}"
  printf -- '- QOS default: %s\n' "${default_qos:-(not configured)}"
  printf -- '- CPU default: %s\n' "${cpu_default:-(not configured)}"
  printf -- '- GPU default: (not configured)\n'
  printf -- '- Memory default: %s\n' "${memory_default:-(not configured)}"
  printf -- '- Time default: %s\n' "${time_default:-(not configured)}"
  printf -- '- Log path:\n'
  printf -- '- Checkpoint path:\n'
} > "$OUT"

if [ "$WRITE_RAW" = "1" ]; then
  mkdir -p "$RAW_DIR"
  has_cmd sinfo && sinfo > "$RAW_DIR/sinfo.txt" 2>&1 || true
  has_cmd sinfo && sinfo -Nel > "$RAW_DIR/nodes-summary.txt" 2>&1 || true
  has_cmd scontrol && scontrol show partition > "$RAW_DIR/partitions.txt" 2>&1 || true
  has_cmd scontrol && scontrol show nodes > "$RAW_DIR/nodes.txt" 2>&1 || true
  [ ! -s "$ASSOC_RECORDS" ] || cp "$ASSOC_RECORDS" "$RAW_DIR/associations-current-user.psv"
  [ ! -s "$QOS_RECORDS" ] || cp "$QOS_RECORDS" "$RAW_DIR/qos.psv"
fi

echo "wrote $OUT"
if [ "$WRITE_RAW" = "1" ]; then
  echo "wrote raw Slurm outputs under $RAW_DIR"
fi
