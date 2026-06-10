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

Emit a compact ML project digest for provider prompts. It includes entrypoint
file names, verification contract hints, and recent experiment ledger rows. It
does not include raw data, checkpoints, wandb/tensorboard logs, or environment
files.

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

REPO="$(cd "$REPO" && pwd)"
LEDGER="${LEDGER:-$REPO/docs/EXPERIMENTS.jsonl}"

style="$("$ROOT/scripts/detect-project-style.sh" "$REPO" 2>/dev/null || echo general)"
if [ "$style" != "ml" ] && [ "$FORCE" -ne 1 ]; then
  exit 0
fi

tmp="$(mktemp)" || exit 1
cleanup() { rm -f "$tmp"; }
trap cleanup EXIT

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
  if grep -Eq '(^|[^A-Za-z0-9_-])ml-smoke([^A-Za-z0-9_-]|$)' "$REPO/scripts/check.sh"; then
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
