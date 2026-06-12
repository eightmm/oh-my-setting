#!/usr/bin/env bash
set -euo pipefail

REPO="$PWD"
INDEX_FILE=""
ACTION="list"
ACTION_SET=0
LIMIT=""
LIMIT_SET=0
PRUNE_FILES=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: artifact-index.sh [options] [list|latest|latest-run|failures|prune] [N]

Inspect the harness artifact index. Provider artifacts still live under
.oms/artifacts/; this index is a compact JSONL lookup table.

Commands:
  list [N]       Show the last N rows (default 20).
  latest         Show the most recent row.
  latest-run     Show a compact summary for the most recent run id.
  failures [N]   Show the last N non-zero-exit rows.
  prune [N]      Keep only the most recent N rows (default 1000); the index is
                 append-only, so prune it when it grows. Add --files to delete
                 unreferenced regular files under REPO/.oms/artifacts.

Options:
  --repo PATH    Repo/directory. Default: PWD.
  --file PATH    Index path. Default: REPO/.oms/artifacts/index.jsonl.
  --files        With prune, delete orphaned artifact/patch files.
  --dry-run      With prune --files, print file deletions without changing files.
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
    --files)
      PRUNE_FILES=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    list|latest|latest-run|failures|prune)
      [ "$ACTION_SET" -eq 0 ] || fail "unknown argument: $1"
      [ "$LIMIT_SET" -eq 0 ] || fail "unknown argument: $1"
      ACTION="$1"
      ACTION_SET=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      [ "$LIMIT_SET" -eq 0 ] || fail "unknown argument: $1"
      LIMIT="$1"
      LIMIT_SET=1
      shift
      ;;
  esac
done

if { [ "$ACTION" = "latest" ] || [ "$ACTION" = "latest-run" ]; } && [ "$LIMIT_SET" -eq 1 ]; then
  fail "unknown argument: $LIMIT"
fi
if [ "$PRUNE_FILES" -eq 1 ] && [ "$ACTION" != "prune" ]; then
  fail "--files is only valid with prune"
fi
if [ "$DRY_RUN" -eq 1 ] && { [ "$ACTION" != "prune" ] || [ "$PRUNE_FILES" -eq 0 ]; }; then
  fail "--dry-run is only valid with prune --files"
fi
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
  if [ "$before" -le "$LIMIT" ] && [ "$PRUNE_FILES" -eq 0 ]; then
    echo "artifact-index: $before rows, within keep=$LIMIT; nothing pruned"
    exit 0
  fi

  tmp="$(mktemp)" || fail "mktemp failed"
  trap 'rm -f "$tmp"' EXIT

  if [ "$before" -le "$LIMIT" ]; then
    cat "$INDEX_FILE" > "$tmp"
    echo "artifact-index: $before rows, within keep=$LIMIT; nothing pruned"
  else
    tail -n "$LIMIT" "$INDEX_FILE" > "$tmp"
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "artifact-index: would prune $before -> $LIMIT rows"
    else
      # In-place overwrite (not mv) so the index keeps its inode, permissions,
      # and any symlink the user set up.
      cat "$tmp" > "$INDEX_FILE"
      echo "artifact-index: pruned $before -> $LIMIT rows"
    fi
  fi

  if [ "$PRUNE_FILES" -eq 1 ]; then
    python3 - "$REPO" "$INDEX_FILE" "$tmp" "$DRY_RUN" <<'EOF'
import json, os, stat, sys

repo, index_file, kept_index, dry = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"
artifacts_root = os.path.realpath(os.path.join(repo, ".oms", "artifacts"))
index_real = os.path.realpath(index_file)


def inside(path, root):
    try:
        return os.path.commonpath([path, root]) == root
    except ValueError:
        return False


def resolve_index_path(value):
    if not isinstance(value, str) or not value:
        return None
    candidate = value if os.path.isabs(value) else os.path.join(repo, value)
    real = os.path.realpath(candidate)
    if not inside(real, artifacts_root):
        return None
    return real


referenced = set()
with open(kept_index) as f:
    for line in f:
        try:
            row = json.loads(line)
        except Exception:
            continue
        for key in ("artifact", "patch", "source"):
            resolved = resolve_index_path(row.get(key))
            if resolved:
                referenced.add(resolved)

orphans = []
for dirpath, dirnames, filenames in os.walk(artifacts_root, followlinks=False):
    for name in filenames:
        path = os.path.join(dirpath, name)
        try:
            st = os.stat(path, follow_symlinks=False)
        except OSError:
            continue
        if not stat.S_ISREG(st.st_mode):
            continue
        real = os.path.realpath(path)
        if not inside(real, artifacts_root):
            continue
        if real == index_real or name in ("index.jsonl", ".gitignore"):
            continue
        if real not in referenced:
            orphans.append(path)

count = 0
for path in sorted(orphans):
    rel = os.path.relpath(path, repo)
    if dry:
        print(f"would delete: {rel}")
    else:
        try:
            st = os.stat(path, follow_symlinks=False)
        except OSError:
            continue
        if not stat.S_ISREG(st.st_mode):
            continue
        os.unlink(path)
        print(f"deleted: {rel}")
    count += 1

if dry:
    print(f"artifact-index: would delete {count} orphan file(s)")
else:
    print(f"artifact-index: deleted {count} orphan file(s)")
EOF
  fi
  exit 0
fi

python3 - "$INDEX_FILE" "$ACTION" "$LIMIT" <<'EOF'
import json, os, re, sys

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


def format_row(r):
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
    return "  ".join(parts)


RUN_RE = re.compile(r".*-([0-9]{8}T[0-9]{6}Z-[0-9]+)(?:-r([0-9]+))?\.md$")


def run_info(row):
    artifact = row.get("artifact")
    if not isinstance(artifact, str) or not artifact:
        return None
    match = RUN_RE.match(os.path.basename(artifact))
    if not match:
        return None
    return match.group(1), int(match.group(2) or 0)


def run_sort_ts(run_id):
    stamp = run_id.split("-", 1)[0]
    date, time_z = stamp.split("T", 1)
    time = time_z.rstrip("Z")
    return "%s-%s-%sT%s:%s:%sZ" % (
        date[:4], date[4:6], date[6:8], time[:2], time[2:4], time[4:6]
    )


def latest_run(rows):
    groups = []
    by_run = {}
    order = 0
    for row in rows:
        order += 1
        parsed = run_info(row)
        if not parsed:
            groups.append({
                "sort": (str(row.get("ts", "")), order),
                "run_id": None,
                "rows": [(order, row)],
                "round": 0,
            })
            continue
        run_id, round_no = parsed
        run_ts = run_sort_ts(run_id)
        group = by_run.get(run_id)
        if not group:
            group = {
                "sort": (run_ts, order),
                "run_id": run_id,
                "rows": [],
                "round": 0,
            }
            by_run[run_id] = group
            groups.append(group)
        group["rows"].append((order, row))
        group["round"] = max(group["round"], round_no)
        group["sort"] = max(group["sort"], (run_ts, order))

    if not groups:
        return

    group = max(groups, key=lambda g: g["sort"])
    if not group["run_id"]:
        for _, row in group["rows"]:
            print(format_row(row))
        return

    selected = {}
    for order, row in group["rows"]:
        parsed = run_info(row)
        round_no = parsed[1] if parsed else 0
        key = (str(row.get("kind", "")), str(row.get("provider", "")))
        prev = selected.get(key)
        if not prev or (round_no, order) >= prev[0]:
            selected[key] = ((round_no, order), row)

    kinds = ",".join(sorted({str(r.get("kind", "")) for _, r in group["rows"] if r.get("kind")}))
    print("run: %s  kind=%s  debate_round=%s" % (group["run_id"], kinds, group["round"]))
    for key in sorted(selected):
        print(format_row(selected[key][1]))


if action == "latest-run":
    latest_run(rows)
    sys.exit(0)
if action == "latest":
    rows = rows[-1:]
else:
    rows = rows[-limit:]
for r in rows:
    print(format_row(r))
EOF
