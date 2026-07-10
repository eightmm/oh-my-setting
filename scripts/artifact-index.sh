#!/usr/bin/env bash
set -euo pipefail

ROOT_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")/lib" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

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
Usage: artifact-index.sh [options] [list|latest|latest-run|failures|validate|migrate|prune] [N]

Inspect the harness artifact index. Provider artifacts still live under
.oms/artifacts/; this index is a compact JSONL lookup table.

Commands:
  list [N]       Show the last N rows (default 20).
  latest         Show the most recent row.
  latest-run     Show a compact summary for the most recent run id.
  failures [N]   Show the last N non-zero-exit rows.
  validate       Validate schema, lineage ids, paths, and references.
  migrate        Idempotently upgrade legacy rows and recover unique basenames.
  prune [N]      Keep only the most recent N rows (default 1000); the index is
                 append-only, so prune it when it grows. Add --files to delete
                 unreferenced regular files under REPO/.oms/artifacts.

Options:
  --repo PATH    Repo/directory. Default: PWD.
  --file PATH    Index path. Default: REPO/.oms/artifacts/index.jsonl.
  --files        With prune, delete orphaned artifact/patch files.
  --dry-run      With prune, print row/file changes without changing them.
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
    list|latest|latest-run|failures|validate|migrate|prune)
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
if { [ "$ACTION" = "validate" ] || [ "$ACTION" = "migrate" ]; } && [ "$LIMIT_SET" -eq 1 ]; then
  fail "unknown argument: $LIMIT"
fi
if [ "$PRUNE_FILES" -eq 1 ] && [ "$ACTION" != "prune" ]; then
  fail "--files is only valid with prune"
fi
if [ "$DRY_RUN" -eq 1 ] && [ "$ACTION" != "prune" ]; then
  fail "--dry-run is only valid with prune"
fi
if [ "$LIMIT_SET" -eq 0 ]; then
  [ "$ACTION" = "prune" ] && LIMIT="1000" || LIMIT="20"
fi
case "$LIMIT" in
  *[!0-9]*|"") fail "N must be a positive integer" ;;
esac
[ "$LIMIT" -gt 0 ] || fail "N must be a positive integer"

# Anchor to the git worktree root so the index does not fork per subdirectory.
REPO="$(oms_repo_root "$REPO")"
INDEX_FILE="${INDEX_FILE:-$REPO/.oms/artifacts/index.jsonl}"
[ -s "$INDEX_FILE" ] || fail "no artifact index at $INDEX_FILE"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

if [ "$ACTION" = "validate" ]; then
  python3 - "$REPO" "$INDEX_FILE" <<'PY'
import json, os, re, sys
repo, index = sys.argv[1:]
required = {"schema", "event_id", "operation_id", "artifact_id", "ts", "kind", "provider", "exit"}
ids = set()
errors = warnings = rows = 0
for lineno, line in enumerate(open(index, encoding="utf-8", errors="replace"), 1):
    if not line.strip():
        continue
    rows += 1
    try:
        row = json.loads(line)
    except Exception as exc:
        print(f"BAD line {lineno}: invalid JSON: {exc}")
        errors += 1
        continue
    if not isinstance(row, dict):
        print(f"BAD line {lineno}: row is not an object"); errors += 1; continue
    missing = sorted(required - row.keys())
    if missing:
        print(f"BAD line {lineno}: missing {','.join(missing)}"); errors += 1
    if row.get("schema") != 1:
        print(f"BAD line {lineno}: schema={row.get('schema')!r}, expected 1"); errors += 1
    eid = row.get("event_id")
    if eid in ids:
        print(f"BAD line {lineno}: duplicate event_id {eid}"); errors += 1
    elif isinstance(eid, str):
        ids.add(eid)
    for key in ("artifact", "patch", "source"):
        value = row.get(key)
        if value in (None, ""):
            continue
        if not isinstance(value, str) or os.path.isabs(value) or value == ".." or value.startswith("../"):
            print(f"BAD line {lineno}: unsafe {key} path"); errors += 1; continue
        path = os.path.realpath(os.path.join(repo, value))
        try:
            inside = os.path.commonpath([os.path.realpath(repo), path]) == os.path.realpath(repo)
        except ValueError:
            inside = False
        if not inside:
            print(f"BAD line {lineno}: escaping {key} path"); errors += 1
        elif not os.path.exists(path):
            print(f"STALE line {lineno}: missing {key}={value}"); warnings += 1
print(f"artifact-index: {rows} row(s), {errors} error(s), {warnings} stale reference(s)")
sys.exit(1 if errors or warnings else 0)
PY
  exit $?
fi

if [ "$ACTION" = "migrate" ]; then
  artifact_index_migrate_locked() {
    python3 - "$REPO" "$INDEX_FILE" <<'PY'
import hashlib, json, os, re, shutil, sys, tempfile
repo, index = sys.argv[1:]
root = os.path.join(repo, ".oms", "artifacts")
all_files = {}
for dirpath, _, files in os.walk(root):
    for name in files:
        all_files.setdefault(name, []).append(os.path.join(dirpath, name))

def digest(path):
    if not path or not os.path.isfile(path): return ""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""): h.update(chunk)
    return h.hexdigest()

def deterministic(prefix, row, lineno):
    raw = json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + f"#{lineno}"
    return prefix + hashlib.sha256(raw.encode()).hexdigest()[:32]

def migrate_path(row, key):
    value = row.get(key)
    if not isinstance(value, str) or not value: return False
    path = value if os.path.isabs(value) else os.path.join(repo, value)
    real_repo = os.path.realpath(repo); real = os.path.realpath(path)
    try: internal = os.path.commonpath([real_repo, real]) == real_repo
    except ValueError: internal = False
    if internal and os.path.exists(path):
        rel = os.path.relpath(path, repo)
        if row[key] != rel: row[key] = rel; return True
        return False
    matches = all_files.get(os.path.basename(value), [])
    if len(matches) == 1:
        row[key] = os.path.relpath(matches[0], repo)
        return True
    ext = {"name": os.path.basename(value), "owned": False, "legacy_unresolved": True}
    h = digest(path)
    if h: ext["sha256"] = h; ext.pop("legacy_unresolved", None)
    row.pop(key, None); row[key + "_external"] = ext
    return True

rows = []; changed = legacy = 0
for lineno, line in enumerate(open(index, encoding="utf-8", errors="replace"), 1):
    if not line.strip(): continue
    row = json.loads(line)
    before = json.dumps(row, sort_keys=True, ensure_ascii=False)
    was_legacy = row.get("schema") != 1
    if was_legacy: legacy += 1
    for key in ("artifact", "patch", "source"): migrate_path(row, key)
    row["schema"] = 1
    row.setdefault("event_id", deterministic("evt_legacy_", row, lineno))
    legacy_op = ""
    artifact = row.get("artifact", "")
    m = re.search(r"([0-9]{8}T[0-9]{6}Z-[0-9]+)", os.path.basename(artifact))
    if m: legacy_op = "op_legacy_" + m.group(1)
    row.setdefault("operation_id", legacy_op or deterministic("op_legacy_", row, lineno))
    primary = ""
    for key in ("artifact", "patch"):
        if row.get(key): primary = digest(os.path.join(repo, row[key])) or primary
    row.setdefault("artifact_id", "sha256:" + (primary or hashlib.sha256(row["event_id"].encode()).hexdigest()))
    if json.dumps(row, sort_keys=True, ensure_ascii=False) != before: changed += 1
    rows.append(row)
real = os.path.realpath(index)
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(real))
try:
    with os.fdopen(fd, "w", encoding="utf-8") as out:
        for row in rows: out.write(json.dumps(row, ensure_ascii=False, allow_nan=False) + "\n")
    shutil.copymode(real, tmp); os.replace(tmp, real)
except Exception:
    try: os.unlink(tmp)
    except OSError: pass
    raise
print(f"artifact-index: migrated {changed} row(s); legacy={legacy}; total={len(rows)}")
PY
  }
  oms_with_file_lock "$INDEX_FILE" artifact_index_migrate_locked
  exit 0
fi

if [ "$ACTION" = "prune" ]; then
  artifact_index_prune_locked() {
  local before
  local tmp

  before="$(wc -l < "$INDEX_FILE" | tr -d ' ')"
  if [ "$before" -le "$LIMIT" ] && [ "$PRUNE_FILES" -eq 0 ]; then
    echo "artifact-index: $before rows, within keep=$LIMIT; nothing pruned"
    exit 0
  fi

  tmp="$(mktemp)" || fail "mktemp failed"

  if [ "$before" -le "$LIMIT" ]; then
    cat "$INDEX_FILE" > "$tmp"
    echo "artifact-index: $before rows, within keep=$LIMIT; nothing pruned"
  else
    tail -n "$LIMIT" "$INDEX_FILE" > "$tmp"
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "artifact-index: would prune $before -> $LIMIT rows"
    else
      # Atomic replace at the symlink TARGET: a truncate-then-write here left
      # a corrupt index if the prune was killed mid-write. Resolving realpath
      # keeps a user-symlinked index working, and the mode is copied over.
      # $tmp itself must survive for the --files step below.
      python3 - "$INDEX_FILE" "$tmp" <<'EOF' || fail "atomic index replace failed"
import os, shutil, sys, tempfile
index, kept = sys.argv[1], sys.argv[2]
real = os.path.realpath(index)
fd, tmp2 = tempfile.mkstemp(dir=os.path.dirname(real))
try:
    with os.fdopen(fd, "wb") as out, open(kept, "rb") as src:
        shutil.copyfileobj(src, out)
    shutil.copymode(real, tmp2)
    os.replace(tmp2, real)
except Exception:
    os.unlink(tmp2)
    raise
EOF
      echo "artifact-index: pruned $before -> $LIMIT rows"
    fi
  fi

  if [ "$PRUNE_FILES" -eq 1 ]; then
    python3 - "$REPO" "$INDEX_FILE" "$tmp" "$DRY_RUN" "${OMS_ARTIFACT_ORPHAN_GRACE:-86400}" <<'EOF'
import json, os, stat, sys, time

repo, index_file, kept_index, dry, grace_raw = sys.argv[1:]
dry = dry == "1"
try:
    grace = max(0, int(grace_raw))
except ValueError:
    raise SystemExit("error: OMS_ARTIFACT_ORPHAN_GRACE must be a non-negative integer")
now = time.time()
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
    dirnames[:] = [name for name in dirnames if not name.endswith(".lock")]
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
        if real == index_real or name in ("index.jsonl", ".gitignore") or name.endswith(".lock"):
            continue
        if real not in referenced and now - st.st_mtime >= grace:
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
  rm -f "$tmp"
}

  oms_with_file_lock "$INDEX_FILE" artifact_index_prune_locked
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
    if r.get("task_id"):
        parts.append("task=%s" % r["task_id"])
    if r.get("base_sha"):
        parts.append("base=%s" % r["base_sha"])
    if r.get("artifact"):
        parts.append("artifact=%s" % r["artifact"])
    if r.get("patch"):
        parts.append("patch=%s" % r["patch"])
    if r.get("task_goal"):
        parts.append("goal=%s" % str(r["task_goal"])[:80])
    return "  ".join(parts)


RUN_RE = re.compile(r".*-([0-9]{8}T[0-9]{6}Z-[0-9]+)(?:-r([0-9]+))?\.md$")


def run_info(row):
    operation = row.get("operation_id")
    artifact = row.get("artifact")
    round_no = 0
    if isinstance(artifact, str):
        match = RUN_RE.match(os.path.basename(artifact))
        if match:
            round_no = int(match.group(2) or 0)
    if isinstance(operation, str) and operation:
        return operation, round_no
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
        try:
            run_ts = run_sort_ts(run_id)
        except Exception:
            run_ts = str(row.get("ts", ""))
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
