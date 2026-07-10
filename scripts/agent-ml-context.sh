#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
MAX_BYTES="${OMS_AGENT_ML_CONTEXT_BYTES:-5000}"
LEDGER=""
FORCE=0

usage() {
  cat <<'EOF'
Usage: agent-ml-context.sh [options]

Emit a compact ML project digest for provider prompts. It includes sanitized
PROJECT.md contract fields, data-manifest identifiers, entrypoint file names,
verification hints, and recent experiment ledger rows. It does not include raw
data, split paths, checkpoints, logs, or environment files.

Options:
  --repo PATH       Repo/directory. Default: PWD.
  --ledger PATH     Experiment ledger. Default: REPO/docs/EXPERIMENTS.jsonl.
  --max-bytes N     Output byte budget. Default: 5000.
  --force           Emit even when detect-project-style.sh is not ml.
  -h, --help        Show help.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { echo "error: --repo requires path" >&2; exit 2; }
      REPO="$2"
      shift 2
      ;;
    --ledger)
      [ "$#" -ge 2 ] || { echo "error: --ledger requires path" >&2; exit 2; }
      LEDGER="$2"
      shift 2
      ;;
    --max-bytes)
      [ "$#" -ge 2 ] || { echo "error: --max-bytes requires number" >&2; exit 2; }
      MAX_BYTES="$2"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$MAX_BYTES" in
  *[!0-9]*|"")
    echo "error: --max-bytes must be a positive integer" >&2
    exit 2
    ;;
esac
[ "$MAX_BYTES" -gt 0 ] || {
  echo "error: --max-bytes must be a positive integer" >&2
  exit 2
}

REPO="$(cd "$REPO" && pwd)"
LEDGER="${LEDGER:-$REPO/docs/EXPERIMENTS.jsonl}"

style="$("$ROOT/scripts/detect-project-style.sh" "$REPO" 2>/dev/null || echo general)"
if [ "$style" != "ml" ] && [ "$FORCE" -ne 1 ]; then
  exit 0
fi

tmp="$(mktemp)" || exit 1
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -f "$tmp"
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

append_file_list() {
  local title="$1"
  shift
  local found

  printf '\n## %s\n\n' "$title" >> "$tmp"
  found="$(cd "$REPO" && find . -maxdepth 3 \( \
      -name ".git" -o -name ".venv" -o -name "node_modules" -o \
      -name "__pycache__" -o -name "data" -o -name "datasets" -o \
      -name "outputs" -o -name "checkpoints" -o -name "wandb" -o \
      -name "runs" \
    \) -prune -o \( "$@" \) -type f -print 2>/dev/null |
    sed 's#^\./##' | sort | head -30)"
  if [ -n "$found" ]; then
    printf '%s\n' "$found" | sed 's/^/- /' >> "$tmp"
  else
    printf 'None found.\n' >> "$tmp"
  fi
}

{
  printf '# ML Agent Context Digest\n\n'
  printf -- '- generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- project-style: %s\n' "$style"
  printf -- '- repo-path: omitted\n'
  printf -- '- policy: raw data, checkpoints, private env, wandb/tensorboard logs, and machine paths are omitted\n'
} > "$tmp"

printf '\n## Project Spec\n\n' >> "$tmp"
if [ -f "$REPO/PROJECT.md" ]; then
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$REPO/PROJECT.md" <<'PY' >> "$tmp"
import re
import sys

allowed = {
    "State", "Goal", "Type", "Scope", "Non-goals", "Success criteria",
    "Required checks", "Baseline/metric", "Task type", "Prediction unit",
    "Inference-time information boundary", "Entity IDs/standardization",
    "Source snapshot/provenance", "Label/target definition",
    "Label units/direction/censoring/replicates", "Negative provenance",
    "Split policy", "Split/group keys", "Leakage risks",
    "Train-only fitted transforms", "Data manifest",
    "Calibration/applicability-domain plan",
}
path_like = re.compile(
    r"(?:[A-Za-z][A-Za-z0-9+.-]*://|"
    r"(?:^|\s)(?:(?:\.\.?|~)?[/\\]|[A-Za-z]:[/\\])\S*|"
    r"(?:^|\s)(?:data|datasets|splits|private|outputs|checkpoints|runs|wandb|mnt|home|tmp)[/\\]\S*|"
    r"\.(?:csv|tsv|json|jsonl|parquet|arrow|h5|hdf5|pkl|pt|pth|ckpt|np[yz])(?:\s|$))",
    re.IGNORECASE,
)
count = 0
for raw in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = raw.rstrip("\r\n")
    if not line.startswith("- ") or ":" not in line:
        continue
    key, value = line[2:].split(":", 1)
    if key not in allowed:
        continue
    value = " ".join(value.strip().split())
    if path_like.search(value):
        value = "[redacted path-or-URL]"
    output = f"- {key}: {value}"
    print(output[:237] + "..." if len(output) > 240 else output)
    count += 1
    if count >= 30:
        break
PY
  else
    printf 'PROJECT.md fields omitted because python3 is unavailable.\n' >> "$tmp"
  fi
else
  printf 'No PROJECT.md found.\n' >> "$tmp"
fi

printf '\n## Data Manifests\n\n' >> "$tmp"
if command -v python3 >/dev/null 2>&1 && find "$REPO/.oms/manifests" -maxdepth 1 -name '*.json' -print -quit 2>/dev/null | grep -q .; then
  python3 - "$REPO/.oms/manifests" <<'PY' >> "$tmp"
import json
import pathlib
import sys

for path in sorted(pathlib.Path(sys.argv[1]).glob("*.json"))[:10]:
    try:
        row = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        continue
    name = str(row.get("name", "unnamed"))[:80]
    id_key = row.get("id_column") or row.get("id_index") or "unspecified"
    keys = row.get("leakage_keys") or []
    if isinstance(keys, dict):
        keys = list(keys)
    labels = []
    for split in row.get("splits") or []:
        if isinstance(split, dict) and split.get("label"):
            labels.append(str(split["label"]))
    print(f"- name={name} id={id_key} leakage_keys={','.join(map(str, keys)) or 'none'} splits={','.join(labels) or 'none'}")
PY
else
  printf 'No data manifests found.\n' >> "$tmp"
fi

append_file_list "Likely ML Entry Points" \
  -name 'train.py' -o -name 'eval.py' -o -name 'evaluate.py' -o \
  -name 'infer.py' -o -name 'inference.py' -o -name 'model.py' -o \
  -name 'dataset.py' -o -name 'dataloader.py'

append_file_list "Likely Config Files" \
  -path './configs/*.yaml' -o -path './configs/*.yml' -o -path './configs/*.toml' -o \
  -name 'config.yaml' -o -name 'config.yml' -o -name 'pyproject.toml'

printf '\n## Verification Contract\n\n' >> "$tmp"
if [ -x "$REPO/scripts/check.sh" ]; then
  printf -- '- scripts/check.sh exists and is executable\n' >> "$tmp"
  if oms_check_sh_has_ml_smoke "$REPO/scripts/check.sh"; then
    printf -- '- preferred ML smoke: bash scripts/check.sh ml-smoke\n' >> "$tmp"
  fi
  printf -- '- fallback fast check: bash scripts/check.sh fast\n' >> "$tmp"
else
  printf 'No executable scripts/check.sh found.\n' >> "$tmp"
fi

printf '\n## Recent Experiments\n\n' >> "$tmp"
if [ -s "$LEDGER" ]; then
  if agent_memory_file_has_sensitive_content "$LEDGER"; then
    printf 'Experiment ledger omitted because it contains sensitive-looking content.\n' >> "$tmp"
  elif command -v python3 >/dev/null 2>&1; then
    tail -n "${OMS_AGENT_ML_LEDGER_ROWS:-8}" "$LEDGER" |
      python3 -c '
import json, sys
for line in sys.stdin:
    try:
        r = json.loads(line)
    except Exception:
        continue
    cmd = " ".join(r.get("cmd", []))
    if len(cmd) > 120:
        cmd = cmd[:117] + "..."
    note = r.get("note") or ""
    if len(note) > 100:
        note = note[:97] + "..."
    dirty = "+dirty" if r.get("dirty") else ""
    row = "- {} exit={} {}s sha={}{} cmd={}".format(
        r.get("ts", "unknown"), r.get("exit", "?"),
        r.get("duration_s", "?"), r.get("git_sha", "?"), dirty, cmd)
    if note:
        row += " note={}".format(note)
    print(row)
' >> "$tmp"
  else
    tail -n "${OMS_AGENT_ML_LEDGER_ROWS:-8}" "$LEDGER" | sed 's/^/- /' >> "$tmp"
  fi
else
  printf 'No docs/EXPERIMENTS.jsonl ledger found.\n' >> "$tmp"
fi

printf '\n## Omitted By Design\n\n' >> "$tmp"
printf -- '- data/, datasets/, outputs/, checkpoints/, wandb/, runs/\n' >> "$tmp"
printf -- '- .env*, credentials, private keys, absolute machine paths, cluster/node details\n' >> "$tmp"

if agent_memory_file_has_sensitive_content "$tmp"; then
  echo "warning: ML context omitted because digest contains sensitive-looking content" >&2
  exit 0
fi

bytes="$(wc -c < "$tmp" | tr -d ' ')"
if [ "$bytes" -gt "$MAX_BYTES" ]; then
  head -c "$MAX_BYTES" "$tmp"
  printf '\n\n... ML context truncated at %s bytes.\n' "$MAX_BYTES"
else
  cat "$tmp"
fi
