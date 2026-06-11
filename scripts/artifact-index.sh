#!/usr/bin/env bash
set -euo pipefail

REPO="$PWD"
INDEX_FILE=""
ACTION="list"
LIMIT=""
LIMIT_SET=0

usage() {
  cat <<'EOF'
Usage: artifact-index.sh [options] [list|latest|failures|prune] [N]

Inspect the harness artifact index. Provider artifacts still live under
.oms/artifacts/; this index is a compact JSONL lookup table.

Commands:
  list [N]       Show the last N rows (default 20).
  latest         Show the most recent row.
  failures [N]   Show the last N non-zero-exit rows.
  prune [N]      Keep only the most recent N rows (default 1000); the index is
                 append-only, so prune it when it grows.

Options:
  --repo PATH    Repo/directory. Default: PWD.
  --file PATH    Index path. Default: REPO/.oms/artifacts/index.jsonl.
  -h, --help     Show help.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || fail "--file requires path"
      INDEX_FILE="$2"
      shift 2
      ;;
    list|latest|failures|prune)
      ACTION="$1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      LIMIT="$1"
      LIMIT_SET=1
      shift
      ;;
  esac
done

if [ "$LIMIT_SET" -eq 0 ]; then
  [ "$ACTION" = "prune" ] && LIMIT="1000" || LIMIT="20"
fi
case "$LIMIT" in
  *[!0-9]*|"") fail "N must be a positive integer" ;;
esac
[ "$LIMIT" -gt 0 ] || fail "N must be a positive integer"

REPO="$(cd "$REPO" && pwd)"
INDEX_FILE="${INDEX_FILE:-$REPO/.oms/artifacts/index.jsonl}"
[ -s "$INDEX_FILE" ] || fail "no artifact index at $INDEX_FILE"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

if [ "$ACTION" = "prune" ]; then
  before="$(wc -l < "$INDEX_FILE" | tr -d ' ')"
  if [ "$before" -le "$LIMIT" ]; then
    echo "artifact-index: $before rows, within keep=$LIMIT; nothing pruned"
    exit 0
  fi
  tmp="$(mktemp)" || fail "mktemp failed"
  trap 'rm -f "$tmp"' EXIT
  # In-place overwrite (not mv) so the index keeps its inode, permissions,
  # and any symlink the user set up.
  tail -n "$LIMIT" "$INDEX_FILE" > "$tmp" && cat "$tmp" > "$INDEX_FILE"
  echo "artifact-index: pruned $before -> $LIMIT rows"
  exit 0
fi

python3 - "$INDEX_FILE" "$ACTION" "$LIMIT" <<'EOF'
import json, sys
path, action, limit = sys.argv[1], sys.argv[2], int(sys.argv[3])
rows = []
with open(path) as f:
    for line in f:
        try:
            r = json.loads(line)
        except Exception:
            continue
        if action == "failures" and int(r.get("exit", 0)) == 0:
            continue
        rows.append(r)
if action == "latest":
    rows = rows[-1:]
else:
    rows = rows[-limit:]
for r in rows:
    parts = [
        r.get("ts", ""),
        str(r.get("kind", "")),
        str(r.get("provider", "")),
        "exit=%s" % r.get("exit", ""),
    ]
    if "verify_exit" in r:
        parts.append("verify=%s" % r.get("verify_exit"))
    if r.get("artifact"):
        parts.append("artifact=%s" % r["artifact"])
    if r.get("patch"):
        parts.append("patch=%s" % r["patch"])
    if r.get("task_goal"):
        parts.append("goal=%s" % str(r["task_goal"])[:80])
    print("  ".join(parts))
EOF
