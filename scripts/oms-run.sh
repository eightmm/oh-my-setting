#!/usr/bin/env bash
set -euo pipefail

# The thin spine that ties runs together. Each of the focused tools
# (run-ledger, run-capsule, experiment-board, run-reconcile, data-manifest)
# keeps owning its own JSONL and CLI; this adds ONE shared key — run_id — and
# ONE append-only join index, plus a READ-ONLY query view. Coherence lives in
# the query layer, not a control layer: writers stay independent and
# Unix-composable. There is deliberately no orchestrator/god-command here.
#
#   id=$(oms-run.sh new)          # mint a run id
#   export OMS_RUN_ID="$id"       # tools auto-link when this is set
#   oms-run.sh link --tool X --event E --path P   # manual link (also used by tools)
#   oms-run.sh show "$id"         # join everything keyed by the run id
#   oms-run.sh ls                 # recent runs

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT_LIB/file-lock.sh"

# Separate from run-capsule's own .oms/runs/index.jsonl — this is the join
# spine across all tools, not the capsule list.
INDEX="${OMS_RUN_INDEX:-.oms/runs/spine.jsonl}"

usage() {
  cat <<'EOF'
Usage: oms-run.sh new [--note TEXT]
       oms-run.sh link --tool NAME --event NAME [--path PATH] [--run-id ID] [--detail TEXT]
       oms-run.sh show <run_id>
       oms-run.sh ls [N]
       oms-run.sh diff <run_id_a> <run_id_b>
       oms-run.sh validate [--dir DIR]

The run spine: a canonical run_id and an append-only join index
(.oms/runs/index.jsonl) over the independent run tools.

new     Mint and print a run id (UTC-host-gitshort-rand). Export it as
        OMS_RUN_ID so the other tools link their records automatically.
link    Append one join row {run_id, ts, tool, event, path, detail}.
        run_id defaults to $OMS_RUN_ID.
show    Join and print every indexed record for a run id.
ls      Summarize the most recent runs (default 10).
diff    Compare two runs' capsules: commit, env, config, seeds, metric deltas.
validate Check every .oms/**/*.jsonl parses and report schema versions; nonzero
         on any malformed line. The guard against silent JSONL/schema drift.

This is read/append only — it never launches or mutates a run. The heavy
records stay in each tool's own file; the index is a rebuildable cache.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

index_append() {
  local index="$1"
  local row_file="$2"
  cat "$row_file" >> "$index"
}

cmd_new() {
  local note=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --note) [ "$#" -ge 2 ] || fail "--note requires text"; note="$2"; shift 2 ;;
      *) fail "unknown new argument: $1" ;;
    esac
  done
  local host gitshort rand id
  host="$(hostname -s 2>/dev/null || echo host)"
  gitshort="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
  rand="$(od -An -N3 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n' || echo "$$")"
  id="$(date -u +%Y%m%dT%H%M%SZ)-$host-$gitshort-$rand"
  # Record the mint itself so `show` works even before any tool links.
  OMS_RUN_ID="$id" cmd_link --tool oms-run --event new ${note:+--detail "$note"} >/dev/null
  printf '%s\n' "$id"
}

cmd_link() {
  local run_id="${OMS_RUN_ID:-}"
  local tool="" event="" path="" detail=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --run-id) [ "$#" -ge 2 ] || fail "--run-id requires a value"; run_id="$2"; shift 2 ;;
      --tool) [ "$#" -ge 2 ] || fail "--tool requires a name"; tool="$2"; shift 2 ;;
      --event) [ "$#" -ge 2 ] || fail "--event requires a name"; event="$2"; shift 2 ;;
      --path) [ "$#" -ge 2 ] || fail "--path requires a value"; path="$2"; shift 2 ;;
      --detail) [ "$#" -ge 2 ] || fail "--detail requires text"; detail="$2"; shift 2 ;;
      *) fail "unknown link argument: $1" ;;
    esac
  done
  [ -n "$run_id" ] || fail "link requires a run id (--run-id or \$OMS_RUN_ID)"
  [ -n "$tool" ] || fail "link requires --tool"
  [ -n "$event" ] || fail "link requires --event"
  mkdir -p "$(dirname "$INDEX")"
  local ts row_tmp
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  row_tmp="$(mktemp)" || fail "mktemp failed"
  python3 - "$run_id" "$ts" "$tool" "$event" "$path" "$detail" > "$row_tmp" <<'PY'
import json, sys
a = sys.argv[1:]
row = {"run_id": a[0], "ts": a[1], "tool": a[2], "event": a[3]}
if a[4]: row["path"] = a[4]
if a[5]: row["detail"] = a[5]
print(json.dumps(row, ensure_ascii=False))
PY
  oms_with_file_lock "$INDEX" index_append "$INDEX" "$row_tmp"
  rm -f "$row_tmp"
}

cmd_show() {
  [ "$#" -eq 1 ] || fail "show requires exactly one run id"
  [ -f "$INDEX" ] || fail "no run index at $INDEX"
  OMS_TARGET="$1" python3 - "$INDEX" <<'PY'
import json, os, sys
target = os.environ["OMS_TARGET"]
rows = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    if r.get("run_id") == target:
        rows.append(r)
if not rows:
    sys.stderr.write("no records for run id: %s\n" % target)
    sys.exit(2)
print("run: %s" % target)
for r in rows:
    extra = ""
    if r.get("path"): extra += "  %s" % r["path"]
    if r.get("detail"): extra += "  (%s)" % r["detail"]
    print("  %s  %-14s %-10s%s" % (r.get("ts"), r.get("tool"), r.get("event"), extra))
PY
}

cmd_ls() {
  [ "$#" -le 1 ] || fail "ls takes at most N"
  local n="${1:-10}"
  case "$n" in *[!0-9]*|"") fail "N must be a positive integer" ;; esac
  [ -f "$INDEX" ] || { echo "no runs"; return 0; }
  OMS_N="$n" python3 - "$INDEX" <<'PY'
import json, os, sys
n = int(os.environ["OMS_N"])
runs = {}
order = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    rid = r.get("run_id")
    if not rid:
        continue
    if rid not in runs:
        runs[rid] = {"first": r.get("ts"), "tools": set(), "events": []}
        order.append(rid)
    runs[rid]["last"] = r.get("ts")
    if r.get("tool"):
        runs[rid]["tools"].add(r["tool"])
    runs[rid]["events"].append("%s/%s" % (r.get("tool"), r.get("event")))
for rid in order[-n:]:
    d = runs[rid]
    print("%s  tools=[%s]  events=%d  last=%s" % (
        rid, ",".join(sorted(d["tools"])), len(d["events"]), d.get("last")))
PY
}

cmd_diff() {
  [ "$#" -eq 2 ] || fail "diff requires two run ids"
  [ -f "$INDEX" ] || fail "no run index at $INDEX"
  OMS_A="$1" OMS_B="$2" python3 - "$INDEX" <<'PY'
import json, os, sys

index = sys.argv[1]
a_id, b_id = os.environ["OMS_A"], os.environ["OMS_B"]

def capsule_path(run_id):
    p = None
    for line in open(index, encoding="utf-8", errors="replace"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("run_id") == run_id and r.get("tool") == "run-capsule" and r.get("path"):
            p = r["path"]  # last capsule wins
    return p

def load(run_id):
    p = capsule_path(run_id)
    if not p or not os.path.exists(p):
        return None, p
    try:
        return json.load(open(p)), p
    except Exception:
        return None, p

a, pa = load(a_id)
b, pb = load(b_id)
if a is None or b is None:
    miss = a_id if a is None else b_id
    sys.stderr.write("no capsule found for run %s (path=%s)\n" %
                     (miss, pa if a is None else pb))
    sys.exit(2)

def line(label, va, vb):
    same = "==" if va == vb else "!="
    print("%-16s %s  %s  |  %s" % (label, same, va, vb))

print("diff: %s  vs  %s" % (a_id, b_id))
ga, gb = a.get("git", {}), b.get("git", {})
line("commit", ga.get("commit_short"), gb.get("commit_short"))
line("dirty", ga.get("dirty"), gb.get("dirty"))
ea, eb = a.get("env", {}), b.get("env", {})
for k in ("python", "torch", "cuda"):
    line(k, ea.get(k), eb.get(k))
line("seeds", ",".join(a.get("seeds", [])) or "-", ",".join(b.get("seeds", [])) or "-")

# config files by path -> sha
ca = {c["path"]: c.get("sha256") for c in a.get("configs", [])}
cb = {c["path"]: c.get("sha256") for c in b.get("configs", [])}
for path in sorted(set(ca) | set(cb)):
    line("cfg:" + os.path.basename(path), ca.get(path, "-"), cb.get(path, "-"))

# numeric metric deltas
ma = (a.get("result") or {}).get("metrics") or {}
mb = (b.get("result") or {}).get("metrics") or {}
for k in sorted(set(ma) | set(mb)):
    va, vb = ma.get(k), mb.get(k)
    delta = ""
    if isinstance(va, (int, float)) and isinstance(vb, (int, float)):
        delta = "  (Δ %+g)" % (vb - va)
    print("%-16s    %s  |  %s%s" % ("metric:" + k, va, vb, delta))
PY
}

cmd_validate() {
  local dir=".oms"
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --dir) [ "$#" -ge 2 ] || fail "--dir requires a path"; dir="$2"; shift 2 ;;
      *) fail "unknown validate argument: $1" ;;
    esac
  done
  [ -d "$dir" ] || { echo "validate: no $dir directory"; return 0; }
  OMS_DIR="$dir" python3 <<'PY'
import glob, json, os, sys
root = os.environ["OMS_DIR"]
files = sorted(glob.glob(os.path.join(root, "**", "*.jsonl"), recursive=True))
bad = 0
total = 0
for f in files:
    lines = 0
    errors = 0
    schemas = set()
    for i, line in enumerate(open(f, encoding="utf-8", errors="replace"), 1):
        if not line.strip():
            continue
        lines += 1
        try:
            r = json.loads(line)
        except Exception as e:
            errors += 1
            print("BAD   %s:%d  %s" % (f, i, e))
            continue
        if isinstance(r, dict) and "schema" in r:
            schemas.add(r["schema"])
    total += 1
    tag = "ok   " if errors == 0 else "FAIL "
    sver = (" schema=%s" % ",".join(str(s) for s in sorted(schemas))) if schemas else ""
    print("%s %s  (%d rows%s)" % (tag, f, lines, sver))
    if errors:
        bad += 1
if total == 0:
    print("validate: no .jsonl records found")
print("validate: %d file(s), %d with errors" % (total, bad), file=sys.stderr)
sys.exit(1 if bad else 0)
PY
}

case "${1:-}" in
  new) shift; cmd_new "$@" ;;
  link) shift; cmd_link "$@" ;;
  show) shift; cmd_show "$@" ;;
  ls) shift; cmd_ls "$@" ;;
  diff) shift; cmd_diff "$@" ;;
  validate) shift; cmd_validate "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
