#!/usr/bin/env bash
set -euo pipefail

# Capture a reproducibility capsule around a run: the exact commit, the
# uncommitted diff, config/env/seed/output fingerprints, and the command +
# result. run-ledger.sh records a one-line git-tracked experiment row; the
# capsule is the richer local bundle that makes a run actually reproducible —
# "which code + config + env produced this checkpoint" — and can regenerate the
# exact tree state later. Extraction is mechanical: no model call.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

RUNS_DIR="${OMS_RUNS_DIR:-$PWD/.oms/runs}"
SCHEMA=1

NOTE=""
LEDGER_OPT=1
LEDGER_FILE=""
METRICS_FILE=""
declare -a CONFIGS=()
declare -a OUTPUTS=()
declare -a SEEDS=()
SCAN_FILE=""
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: run-capsule.sh run [options] -- <command...>
       run-capsule.sh list [N]
       run-capsule.sh show <id>
       run-capsule.sh whence <file>
       run-capsule.sh reproduce <id>
       run-capsule.sh verify <id>

Capture a reproducibility capsule around a command and record it under
.oms/runs/<id>/ (git-ignored). Exit code mirrors the command.

run options:
  --note TEXT      Free-text note stored in the capsule.
  --config PATH    Config file to fingerprint (sha256). Repeatable.
  --output PATH    Output/checkpoint path to record (path+size+mtime, not
                   content). Repeatable.
  --seed N         Seed to record. Repeatable.
  --metrics PATH   JSON metrics file folded into the capsule result.
  --no-ledger      Do not also append a run-ledger row.
  --ledger PATH    Ledger path for the companion row (default run-ledger default).

list [N]           Show the last N capsules (default 10).
show <id>          Print the capsule JSON.
whence <file>      Print which run produced a checkpoint/output file (by sha).
reproduce <id>     Print the exact checkout + env + command to recreate the run.
verify <id>        Compare the current tree/env to the capsule; nonzero on drift.

Capture is mechanical (no model). The command line is recorded verbatim —
do not put secrets in arguments.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  [ -z "$SCAN_FILE" ] || rm -f "$SCAN_FILE"
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

runs_index() {
  printf '%s/index.jsonl\n' "$RUNS_DIR"
}

append_index_row() {
  local index="$1"
  local row_file="$2"
  cat "$row_file" >> "$index"
}

# --- capture helpers --------------------------------------------------------

capture_env_json() {
  # Best-effort environment fingerprint; every probe degrades to null/empty.
  python3 <<'PY'
import json, platform, shutil, subprocess

def run(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return ""

def probe(code):
    try:
        out = subprocess.run(["python3", "-c", code], capture_output=True, text=True, timeout=20)
        return out.stdout.strip() or None
    except Exception:
        return None

env = {
    "python": platform.python_version(),
    "platform": platform.platform(),
    "torch": probe("import torch;print(torch.__version__)"),
    "cuda": probe("import torch;print(torch.version.cuda)"),
    "nvidia_driver": None,
    "gpu_names": [],
}
if shutil.which("nvidia-smi"):
    drv = run(["nvidia-smi", "--query-gpu=driver_version", "--format=csv,noheader"])
    env["nvidia_driver"] = (drv.splitlines() or [None])[0]
    names = run(["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"])
    env["gpu_names"] = [n.strip() for n in names.splitlines() if n.strip()]

freeze = ""
if shutil.which("uv"):
    freeze = run(["uv", "pip", "freeze"])
if not freeze:
    freeze = run(["python3", "-m", "pip", "freeze"])
env["freeze_sha256"] = None
if freeze:
    import hashlib
    env["freeze_sha256"] = hashlib.sha256(freeze.encode()).hexdigest()
print(json.dumps(env))
PY
}

# Save a RECONSTRUCTABLE env lock into the bundle (fingerprint != rebuildable):
# the project's uv.lock if present, else a frozen package list. reproduce can
# then point at it to recreate the environment.
save_env_lock() {
  local bundle="$1"
  if [ -f uv.lock ]; then
    cp uv.lock "$bundle/env.uv.lock" 2>/dev/null || true
  elif command -v uv >/dev/null 2>&1 && uv pip freeze >"$bundle/env.freeze.txt" 2>/dev/null \
       && [ -s "$bundle/env.freeze.txt" ]; then
    :
  elif python3 -m pip freeze >"$bundle/env.freeze.txt" 2>/dev/null && [ -s "$bundle/env.freeze.txt" ]; then
    :
  else
    rm -f "$bundle/env.freeze.txt" 2>/dev/null || true
  fi
}

capture_git_json() {
  # cwd-relative git provenance; writes uncommitted.diff into $1 when dirty.
  local bundle="$1"
  local commit_full="none" commit_short="none" branch="none"
  local dirty=0 diff_sha="" diff_rel="" status_short=""
  if git rev-parse --git-dir >/dev/null 2>&1; then
    commit_full="$(git rev-parse HEAD 2>/dev/null || echo 'no-commit')"
    commit_short="$(git rev-parse --short HEAD 2>/dev/null || echo 'no-commit')"
    branch="$(git symbolic-ref --quiet --short HEAD 2>/dev/null || echo 'detached')"
    dirty="$(git status --porcelain --untracked-files=no | wc -l | tr -d ' ')"
    if [ "$dirty" -gt 0 ]; then
      if git rev-parse --verify HEAD >/dev/null 2>&1; then
        git diff HEAD > "$bundle/uncommitted.diff" 2>/dev/null || true
      else
        { git diff --cached; git diff; } > "$bundle/uncommitted.diff" 2>/dev/null || true
      fi
      if [ -s "$bundle/uncommitted.diff" ]; then
        diff_sha="$(oms_sha256_file "$bundle/uncommitted.diff" | cut -c1-16)"
        diff_rel="uncommitted.diff"
      else
        rm -f "$bundle/uncommitted.diff"
      fi
      status_short="$(git status --porcelain --untracked-files=no | head -n 40)"
    fi
  fi
  OMS_GIT_STATUS="$status_short" python3 - \
    "$commit_full" "$commit_short" "$branch" "$dirty" "$diff_sha" "$diff_rel" <<'PY'
import json, os, sys
a = sys.argv[1:]
print(json.dumps({
    "commit_full": a[0], "commit_short": a[1], "branch": a[2],
    "dirty": int(a[3]), "diff_sha256": a[4], "diff_file": a[5],
    "status_short": os.environ.get("OMS_GIT_STATUS", ""),
}))
PY
}

configs_json() {
  local c sha
  local out="["
  local first=1
  for c in "${CONFIGS[@]:-}"; do
    [ -n "$c" ] || continue
    [ -f "$c" ] || { echo "capsule: config not found, skipped: $c" >&2; continue; }
    sha="$(oms_sha256_file "$c" | cut -c1-32)"
    [ "$first" = 1 ] || out="$out,"
    out="$out$(OMS_P="$c" OMS_S="$sha" python3 -c 'import json,os;print(json.dumps({"path":os.environ["OMS_P"],"sha256":os.environ["OMS_S"]}))')"
    first=0
  done
  printf '%s]\n' "$out"
}

outputs_json() {
  # Hash outputs up to a size cap so a checkpoint can be traced back to its
  # producing run (see `whence`). Multi-GB files are recorded path+size+mtime
  # only, with hashed=false, to avoid an expensive full read.
  OMS_OUTPUTS="$(printf '%s\n' "${OUTPUTS[@]:-}")" \
  OMS_HASH_MAX="${OMS_CAPSULE_HASH_MAX:-536870912}" python3 <<'PY'
import hashlib, json, os
cap = int(os.environ.get("OMS_HASH_MAX", "536870912"))
rows = []
for p in os.environ.get("OMS_OUTPUTS", "").splitlines():
    if not p:
        continue
    row = {"path": p, "exists": os.path.exists(p), "hashed": False}
    try:
        st = os.stat(p)
        row["size"] = st.st_size
        row["mtime"] = int(st.st_mtime)
        if os.path.isfile(p) and st.st_size <= cap:
            h = hashlib.sha256()
            with open(p, "rb") as fh:
                for chunk in iter(lambda: fh.read(1 << 20), b""):
                    h.update(chunk)
            row["sha256"] = h.hexdigest()
            row["hashed"] = True
    except OSError:
        pass
    rows.append(row)
print(json.dumps(rows))
PY
}

# --- subcommands ------------------------------------------------------------

cmd_run() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --note) [ "$#" -ge 2 ] || fail "--note requires text"; NOTE="$2"; shift 2 ;;
      --config) [ "$#" -ge 2 ] || fail "--config requires path"; CONFIGS+=("$2"); shift 2 ;;
      --output) [ "$#" -ge 2 ] || fail "--output requires path"; OUTPUTS+=("$2"); shift 2 ;;
      --seed) [ "$#" -ge 2 ] || fail "--seed requires value"; SEEDS+=("$2"); shift 2 ;;
      --metrics) [ "$#" -ge 2 ] || fail "--metrics requires path"; METRICS_FILE="$2"; shift 2 ;;
      --no-ledger) LEDGER_OPT=0; shift ;;
      --ledger) [ "$#" -ge 2 ] || fail "--ledger requires path"; LEDGER_FILE="$2"; shift 2 ;;
      --) shift; break ;;
      *) fail "unknown run argument before --: $1" ;;
    esac
  done
  [ "$#" -gt 0 ] || fail "run requires a command after --"

  local ts id bundle
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
  bundle="$RUNS_DIR/$id"
  agent_memory_ensure_oms_ignore_for_path "$RUNS_DIR" 2>/dev/null || true
  mkdir -p "$bundle"

  local git_json env_json cfg_json out_json
  git_json="$(capture_git_json "$bundle")"
  env_json="$(capture_env_json)"
  save_env_lock "$bundle"
  cfg_json="$(configs_json)"
  out_json="$(outputs_json)"

  # The command's combined output is teed to the bundle log (part of the
  # capsule) and to stderr (live), leaving stdout free for the capsule id so
  # `id=$(run-capsule run ...)` is reliable.
  local start_s status duration_s
  start_s="$(date +%s)"
  set +e
  "$@" 2>&1 | tee "$bundle/output.log" >&2
  status="${PIPESTATUS[0]}"
  set -e
  duration_s=$(( $(date +%s) - start_s ))

  local metrics_json=""
  [ -n "$METRICS_FILE" ] && [ -f "$METRICS_FILE" ] && metrics_json="$(cat "$METRICS_FILE")"

  OMS_GIT="$git_json" OMS_ENV="$env_json" OMS_CFG="$cfg_json" OMS_OUT="$out_json" \
  OMS_METRICS="$metrics_json" OMS_SEEDS="$(printf '%s\n' "${SEEDS[@]:-}")" \
  python3 - "$SCHEMA" "$id" "$ts" "${OMS_AGENT:-unknown}" "$PWD" "$NOTE" \
    "$status" "$duration_s" "${SLURM_JOB_ID:-}" "$@" <<'PY' > "$bundle/capsule.json"
import json, os, sys
a = sys.argv[1:]
def load(name, default):
    raw = os.environ.get(name, "")
    if not raw:
        return default
    try:
        return json.loads(raw)
    except Exception:
        return default
metrics = load("OMS_METRICS", None)
if not isinstance(metrics, dict):
    metrics = None
seeds = [s for s in os.environ.get("OMS_SEEDS", "").splitlines() if s]
row = {
    "schema": int(a[0]),
    "id": a[1],
    "ts": a[2],
    "agent": a[3],
    "cwd": a[4],
    "note": a[5],
    "git": load("OMS_GIT", {}),
    "env": load("OMS_ENV", {}),
    "configs": load("OMS_CFG", []),
    "outputs": load("OMS_OUT", []),
    "seeds": seeds,
    "slurm_job_id": a[8],
    "command": a[9:],
    "log_file": "output.log",
    "result": {"exit": int(a[6]), "duration_s": int(a[7]), "metrics": metrics},
}
print(json.dumps(row, ensure_ascii=False, allow_nan=False, indent=2))
PY

  SCAN_FILE="$bundle/capsule.json"
  if agent_memory_file_has_sensitive_content "$bundle/capsule.json"; then
    echo "capsule: warning: capsule looks sensitive (command/paths); it is local-only under .oms/runs" >&2
  fi
  SCAN_FILE=""

  # Append a compact index row (locked: concurrent runs share the index).
  local index row_tmp
  index="$(runs_index)"
  row_tmp="$(mktemp)" || fail "mktemp failed"
  python3 - "$bundle/capsule.json" "$id" "$ts" "$status" "$duration_s" "$PWD" > "$row_tmp" <<'PY'
import json, sys
cap = json.load(open(sys.argv[1]))
a = sys.argv[2:]
print(json.dumps({
    "id": a[0], "ts": a[1], "exit": int(a[2]), "duration_s": int(a[3]),
    "cwd": a[4],
    "git_sha": cap.get("git", {}).get("commit_short", "none"),
    "dirty": cap.get("git", {}).get("dirty", 0),
    "command": cap.get("command", []),
}, ensure_ascii=False))
PY
  oms_with_file_lock "$index" append_index_row "$index" "$row_tmp"
  rm -f "$row_tmp"

  # Companion git-tracked ledger row, reusing run-ledger (no re-run of CMD).
  if [ "$LEDGER_OPT" = 1 ]; then
    local -a led=(--note "$NOTE" --no-gate)
    [ -n "$LEDGER_FILE" ] && led+=(--file "$LEDGER_FILE")
    [ -n "$METRICS_FILE" ] && [ -f "$METRICS_FILE" ] && led+=(--metrics "$METRICS_FILE")
    OMS_RUN_LEDGER_STATUS_OVERRIDE="$status" "$ROOT/scripts/run-ledger.sh" \
      "${led[@]}" -- "$@" >/dev/null 2>&1 || \
      echo "capsule: companion ledger row failed (capsule still saved)" >&2
  fi

  # Thin-spine join: link this capsule to the active run id when one is set.
  if [ -n "${OMS_RUN_ID:-}" ]; then
    "$ROOT/scripts/oms-run.sh" link --tool run-capsule --event capture \
      --path "$bundle/capsule.json" --detail "exit $status" >/dev/null 2>&1 || true
  fi

  echo "capsule: $bundle/capsule.json (exit $status, ${duration_s}s)" >&2
  printf '%s\n' "$id"
  exit "$status"
}

capsule_path() {
  local id="$1"
  local p="$RUNS_DIR/$id/capsule.json"
  [ -f "$p" ] || fail "no capsule: $id"
  printf '%s\n' "$p"
}

cmd_list() {
  [ "$#" -le 1 ] || fail "list takes at most N"
  local n="${1:-10}"
  case "$n" in *[!0-9]*|"") fail "N must be a positive integer" ;; esac
  local index
  index="$(runs_index)"
  [ -f "$index" ] || { echo "no capsules recorded"; return 0; }
  tail -n "$n" "$index" | python3 -c '
import json, sys
for line in sys.stdin:
    try: r = json.loads(line)
    except Exception: continue
    dirty = "+dirty" if r.get("dirty") else ""
    print("%s  %s  exit=%s  %ss  sha=%s%s  %s" % (
        r.get("id"), r.get("ts"), r.get("exit"), r.get("duration_s"),
        r.get("git_sha"), dirty, " ".join(r.get("command", []))))
'
}

cmd_show() {
  [ "$#" -eq 1 ] || fail "show requires <id>"
  cat "$(capsule_path "$1")"
}

# Reverse provenance: which run produced this checkpoint/output file?
cmd_whence() {
  [ "$#" -eq 1 ] || fail "whence requires a file path"
  [ -f "$1" ] || fail "no such file: $1"
  [ -d "$RUNS_DIR" ] || fail "no capsules under $RUNS_DIR"
  local want
  want="$(oms_sha256_file "$1")" || fail "could not hash $1"
  OMS_WANT="$want" OMS_TARGET="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")" \
    python3 - "$RUNS_DIR" <<'PY'
import glob, json, os, sys
want = os.environ["OMS_WANT"]
target = os.environ["OMS_TARGET"]
hits = []
for cap in glob.glob(os.path.join(sys.argv[1], "*", "capsule.json")):
    try:
        c = json.load(open(cap))
    except Exception:
        continue
    for o in c.get("outputs", []):
        if o.get("sha256") == want or (
            os.path.abspath(o.get("path", "")) == target and not o.get("hashed")
        ):
            hits.append((c.get("id"), c.get("ts"), o.get("path"),
                         "sha" if o.get("sha256") == want else "path"))
if not hits:
    sys.stderr.write("no run produced this file (no matching output sha)\n")
    sys.exit(1)
for run_id, ts, path, how in hits:
    print("%s  %s  (%s match: %s)" % (run_id, ts, how, path))
PY
}

cmd_reproduce() {
  [ "$#" -eq 1 ] || fail "reproduce requires <id>"
  local p
  p="$(capsule_path "$1")"
  OMS_BUNDLE="$RUNS_DIR/$1" python3 - "$p" <<'PY'
import json, os, sys
cap = json.load(open(sys.argv[1]))
g = cap.get("git", {})
e = cap.get("env", {})
bundle = os.environ.get("OMS_BUNDLE", "")
cwd = cap.get("cwd", ".")
print("# Reproduce capsule %s (captured %s by %s)" % (cap.get("id"), cap.get("ts"), cap.get("agent")))
print("cd %s" % cwd)
commit = g.get("commit_full", "none")
if commit not in ("none", "no-commit", ""):
    print("git checkout %s" % commit)
if g.get("diff_file"):
    print("git apply %s/%s   # restore uncommitted changes at capture time" % (bundle, g["diff_file"]))
if os.path.exists(os.path.join(bundle, "env.uv.lock")):
    print("uv sync --frozen   # from %s/env.uv.lock" % bundle)
elif os.path.exists(os.path.join(bundle, "env.freeze.txt")):
    print("uv pip install -r %s/env.freeze.txt   # or: pip install -r ..." % bundle)
print("# env at capture: python=%s torch=%s cuda=%s" % (e.get("python"), e.get("torch"), e.get("cuda")))
if cap.get("seeds"):
    print("# seeds: %s" % ", ".join(cap["seeds"]))
print("# command:")
print(" ".join(cap.get("command", [])))
PY
}

cmd_verify() {
  [ "$#" -eq 1 ] || fail "verify requires <id>"
  local p bundle
  p="$(capsule_path "$1")"
  bundle="$RUNS_DIR/$1"
  local cur_commit="none" cur_diff=""
  if git rev-parse --git-dir >/dev/null 2>&1; then
    cur_commit="$(git rev-parse HEAD 2>/dev/null || echo 'no-commit')"
    if [ "$(git status --porcelain --untracked-files=no | wc -l | tr -d ' ')" -gt 0 ]; then
      if git rev-parse --verify HEAD >/dev/null 2>&1; then
        cur_diff="$(git diff HEAD | oms_sha256_stream | cut -c1-16)"
      else
        cur_diff="$( { git diff --cached; git diff; } | oms_sha256_stream | cut -c1-16)"
      fi
    fi
  fi
  OMS_CUR_COMMIT="$cur_commit" OMS_CUR_DIFF="$cur_diff" python3 - "$p" <<'PY'
import json, os, sys
cap = json.load(open(sys.argv[1]))
g = cap.get("git", {})
e = cap.get("env", {})
cur_commit = os.environ.get("OMS_CUR_COMMIT", "")
cur_diff = os.environ.get("OMS_CUR_DIFF", "")
drift = 0
def line(ok, label, want, got):
    global drift
    tag = "ok  " if ok else "DRIFT"
    if not ok:
        drift = 1
    print("%s %s: capsule=%s current=%s" % (tag, label, want, got))
line(g.get("commit_full") == cur_commit, "commit", g.get("commit_full"), cur_commit)
line((g.get("diff_sha256") or "") == cur_diff, "uncommitted-diff",
     g.get("diff_sha256") or "(clean)", cur_diff or "(clean)")
# Env drift is informational only (WARN, does not fail verify) — the silent
# repro-killer for protein-ligand stacks (torch/CUDA/driver skew).
import platform, subprocess
def probe(code):
    try:
        out = subprocess.run(["python3", "-c", code], capture_output=True, text=True, timeout=20)
        return out.stdout.strip() or None
    except Exception:
        return None
cur = {
    "python": platform.python_version(),
    "torch": probe("import torch;print(torch.__version__)"),
    "cuda": probe("import torch;print(torch.version.cuda)"),
}
for k in ("python", "torch", "cuda"):
    want, got = e.get(k), cur.get(k)
    tag = "ok   " if want == got else "WARN "
    print("%s env-%s: capsule=%s current=%s" % (tag, k, want, got))
sys.exit(1 if drift else 0)
PY
}

case "${1:-}" in
  run) shift; cmd_run "$@" ;;
  list) shift; cmd_list "$@" ;;
  show) shift; cmd_show "$@" ;;
  whence) shift; cmd_whence "$@" ;;
  reproduce) shift; cmd_reproduce "$@" ;;
  verify) shift; cmd_verify "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
