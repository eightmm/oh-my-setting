#!/usr/bin/env bash
set -euo pipefail

# Fingerprint dataset splits so agents can detect silent data drift and, above
# all, train/eval leakage. A manifest records, per split, the file content hash,
# the row count, and an order-independent hash of the ID set. `check` flags
# drift against the recorded manifest; `leakage` reports ID overlap between
# splits (train ∩ test etc.) — the failure that silently inflates ML results.
# Only hashes/counts are stored, never raw rows, so large/private data stays out.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

MANIFEST_DIR="${OMS_MANIFEST_DIR:-$PWD/.oms/manifests}"
# schema 3 adds per-key (id -> key) mapping fingerprint + empty count; schema 2
# added per-split key-column fingerprints. Older manifests (no "keys" or no
# "pair_sha256") are still read; their absent fields are simply not compared.
SCHEMA=3

NAME=""
NOTE=""
ID_COLUMN=""
ID_INDEX=""
SHOW_EXAMPLES=0
declare -a SPLITS=()
declare -a KEY_COLUMNS=()
SCAN_FILE=""
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: data-manifest.sh create --name NAME --split LABEL=FILE [options]
       data-manifest.sh check --name NAME
       data-manifest.sh leakage --name NAME [--show-examples]
       data-manifest.sh list
       data-manifest.sh show --name NAME

Fingerprint dataset splits to catch data drift and train/eval leakage.
Stores only content hashes, row counts, and ID-set hashes under
.oms/manifests/ (no raw rows).

create options:
  --name NAME        Manifest name (required).
  --split LABEL=FILE Split file, e.g. --split train=train.txt. Repeatable.
  --id-column NAME   Treat files as CSV/TSV and use this header column as the ID.
  --id-index N       Use 0-based column N (CSV/TSV) as the ID.
  --key-column NAME  Extra CSV/TSV header column to check for entity overlap on
                     top of the ID (repeatable). Use for chem-bio leakage that
                     exact-ID overlap misses: the project precomputes columns
                     like inchikey, scaffold, uniprot, sequence_cluster, or
                     assay_id, and this flags train/eval overlap on each.
  --note TEXT        Free-text note.
  (default: each non-empty line is one ID.)

check    Recompute current split files; nonzero exit on any drift.
leakage  Report ID overlap between splits; nonzero exit if any overlap. Also
         reports overlap on every recorded --key-column, tagged [key].
  --show-examples    Print up to 5 overlapping IDs/keys (off by default; values
                     may be sensitive).
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

# Manifest names become a file path under MANIFEST_DIR; reject anything that
# could escape it or write outside (this runs from agent-driven prompts).
validate_manifest_name() {
  case "$1" in
    ""|.|..|*/*|*\\*|*..*|*[!A-Za-z0-9._-]*)
      fail "manifest name must match [A-Za-z0-9._-]+ with no path separators or .." ;;
  esac
}

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

manifest_path() {
  validate_manifest_name "$1"
  printf '%s/%s.json\n' "$MANIFEST_DIR" "$1"
}

require_manifest() {
  local p
  p="$(manifest_path "$1")"
  [ -f "$p" ] || fail "no manifest: $1 ($p)"
  printf '%s\n' "$p"
}

# Emit one split's fingerprint as a JSON object on stdout, or fail loudly.
fingerprint_split() {
  local label="$1"
  local file="$2"
  [ -f "$file" ] || fail "split file not found: $file"
  local keys_nl=""
  [ "${#KEY_COLUMNS[@]}" -gt 0 ] && keys_nl="$(printf '%s\n' "${KEY_COLUMNS[@]}")"
  OMS_LABEL="$label" OMS_FILE="$file" OMS_ID_COLUMN="$ID_COLUMN" \
  OMS_ID_INDEX="$ID_INDEX" OMS_KEYS="$keys_nl" python3 <<'PY'
import csv, hashlib, json, os, sys

label = os.environ["OMS_LABEL"]
path = os.environ["OMS_FILE"]
col = os.environ.get("OMS_ID_COLUMN", "")
idx = os.environ.get("OMS_ID_INDEX", "")

content = hashlib.sha256()
with open(path, "rb") as fh:
    for chunk in iter(lambda: fh.read(65536), b""):
        content.update(chunk)

ids = []
if col or idx != "":
    with open(path, newline="", encoding="utf-8", errors="replace") as fh:
        sample = fh.read(4096)
        fh.seek(0)
        delim = "\t" if "\t" in sample and sample.count("\t") >= sample.count(",") else ","
        reader = csv.reader(fh, delimiter=delim)
        rows = list(reader)
    if not rows:
        ids = []
    elif col:
        header = rows[0]
        if col not in header:
            sys.stderr.write("error: id-column %r not in header of %s\n" % (col, path))
            sys.exit(2)
        ci = header.index(col)
        ids = [r[ci].strip() for r in rows[1:] if len(r) > ci and r[ci].strip()]
    else:
        ci = int(idx)
        ids = [r[ci].strip() for r in rows if len(r) > ci and r[ci].strip()]
else:
    with open(path, encoding="utf-8", errors="replace") as fh:
        ids = [ln.strip() for ln in fh if ln.strip()]

uniq = sorted(set(ids))
id_hash = hashlib.sha256("\n".join(uniq).encode()).hexdigest()

# Fingerprint each recorded key column's value set. In chem-bio the key
# assignment (scaffold/cluster/assay) IS the split contract, so a changed key
# set must be detectable even when the ID set is stable. A missing column is
# recorded as a sentinel so `check` reports it as drift rather than crashing.
out = {
    "label": label,
    "file": path,
    "sha256": content.hexdigest(),
    "rows": len(ids),
    "unique_ids": len(uniq),
    "id_sha256": id_hash,
}
def _sha(lines):
    return hashlib.sha256("\n".join(lines).encode()).hexdigest()

key_cols = [k for k in os.environ.get("OMS_KEYS", "").splitlines() if k]
if key_cols:
    with open(path, newline="", encoding="utf-8", errors="replace") as fh:
        sample = fh.read(4096); fh.seek(0)
        delim = "\t" if "\t" in sample and sample.count("\t") >= sample.count(",") else ","
        krows = list(csv.reader(fh, delimiter=delim))
    kheader = krows[0] if krows else []
    # ID column for the (id -> key) mapping fingerprint, so a key set that is
    # unchanged but reassigned across rows (e.g. scaffolds permuted) is still
    # caught as drift. None when the ID is line-based (no column to pair on).
    id_ci = None
    if col and col in kheader:
        id_ci = kheader.index(col)
    elif idx != "":
        id_ci = int(idx)
    keys_out = {}
    for k in key_cols:
        if k not in kheader:
            keys_out[k] = {"unique": -1, "sha256": "<missing>"}
            continue
        kci = kheader.index(k)
        vals, pairs, empty = [], [], 0
        for r in krows[1:]:
            kval = r[kci].strip() if len(r) > kci else ""
            if kval:
                vals.append(kval)
            else:
                empty += 1
            if id_ci is not None and len(r) > id_ci and r[id_ci].strip():
                pairs.append(r[id_ci].strip() + "\t" + kval)
        uniqv = sorted(set(vals))
        entry = {"unique": len(uniqv), "sha256": _sha(uniqv), "empty": empty}
        # empty keys are legitimate (e.g. acyclic molecules have no Murcko
        # scaffold); record the count for visibility rather than dropping silently.
        if id_ci is not None:
            entry["pair_sha256"] = _sha(sorted(pairs))
        keys_out[k] = entry
    out["keys"] = keys_out
print(json.dumps(out, ensure_ascii=False))
PY
}

# Collect the ID set of a split file as newline output (for overlap checks).
emit_ids() {
  local file="$1"
  OMS_FILE="$file" OMS_ID_COLUMN="$ID_COLUMN" OMS_ID_INDEX="$ID_INDEX" python3 <<'PY'
import csv, os, sys
path = os.environ["OMS_FILE"]
col = os.environ.get("OMS_ID_COLUMN", "")
idx = os.environ.get("OMS_ID_INDEX", "")
if col or idx != "":
    with open(path, newline="", encoding="utf-8", errors="replace") as fh:
        sample = fh.read(4096); fh.seek(0)
        delim = "\t" if "\t" in sample and sample.count("\t") >= sample.count(",") else ","
        rows = list(csv.reader(fh, delimiter=delim))
    if not rows:
        sys.exit(0)
    if col:
        header = rows[0]
        if col not in header:
            sys.stderr.write("error: id-column %r not in header of %s\n" % (col, path))
            sys.exit(2)
        ci = header.index(col)
        body = rows[1:]
    else:
        ci = int(idx); body = rows
    for r in body:
        if len(r) > ci and r[ci].strip():
            print(r[ci].strip())
else:
    with open(path, encoding="utf-8", errors="replace") as fh:
        for ln in fh:
            if ln.strip():
                print(ln.strip())
PY
}

cmd_create() {
  parse_common_args "$@"
  [ -n "$NAME" ] || fail "create requires --name"
  [ "${#SPLITS[@]}" -gt 0 ] || fail "create requires at least one --split LABEL=FILE"

  mkdir -p "$MANIFEST_DIR"
  agent_memory_ensure_oms_ignore_for_path "$MANIFEST_DIR" 2>/dev/null || true

  # Key columns are looked up by CSV/TSV header name; validate they exist in
  # every split before recording them, so leakage cannot silently no-op later.
  if [ "${#KEY_COLUMNS[@]}" -gt 0 ]; then
    local pair label file
    for pair in "${SPLITS[@]}"; do
      label="${pair%%=*}"; file="${pair#*=}"
      [ -f "$file" ] || fail "split file not found: $file"
      OMS_FILE="$file" OMS_KEYS="$(printf '%s\n' "${KEY_COLUMNS[@]}")" python3 <<'PY' || exit $?
import csv, os, sys
path = os.environ["OMS_FILE"]
keys = [k for k in os.environ["OMS_KEYS"].splitlines() if k]
with open(path, newline="", encoding="utf-8", errors="replace") as fh:
    sample = fh.read(4096); fh.seek(0)
    delim = "\t" if "\t" in sample and sample.count("\t") >= sample.count(",") else ","
    header = next(csv.reader(fh, delimiter=delim), [])
missing = [k for k in keys if k not in header]
if missing:
    sys.stderr.write("error: key-column(s) %r not in header of %s\n" % (missing, path))
    sys.exit(2)
PY
    done
  fi

  local splits_json="["
  local first=1 entry label file pair
  for pair in "${SPLITS[@]}"; do
    label="${pair%%=*}"
    file="${pair#*=}"
    [ "$label" != "$pair" ] && [ -n "$file" ] || fail "bad --split (want LABEL=FILE): $pair"
    entry="$(fingerprint_split "$label" "$file")" || exit $?
    [ "$first" = 1 ] || splits_json="$splits_json,"
    splits_json="$splits_json$entry"
    first=0
  done
  splits_json="$splits_json]"

  local ts out tmp
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  out="$(manifest_path "$NAME")"
  tmp="$(mktemp)" || fail "mktemp failed"
  local keys_nl=""
  [ "${#KEY_COLUMNS[@]}" -gt 0 ] && keys_nl="$(printf '%s\n' "${KEY_COLUMNS[@]}")"
  OMS_SPLITS="$splits_json" OMS_KEYS="$keys_nl" python3 - "$SCHEMA" "$NAME" "$ts" "$NOTE" \
    "$ID_COLUMN" "$ID_INDEX" > "$tmp" <<'PY'
import json, os, sys
a = sys.argv[1:]
keys = [k for k in os.environ.get("OMS_KEYS", "").splitlines() if k]
print(json.dumps({
    "schema": int(a[0]),
    "name": a[1],
    "ts": a[2],
    "note": a[3],
    "id_column": a[4],
    "id_index": a[5],
    "leakage_keys": keys,
    "splits": json.loads(os.environ["OMS_SPLITS"]),
}, ensure_ascii=False, indent=2))
PY
  SCAN_FILE="$tmp"
  if agent_memory_file_has_sensitive_content "$tmp"; then
    echo "data-manifest: warning: manifest looks sensitive (paths); it is local under .oms/manifests" >&2
  fi
  mv "$tmp" "$out"
  SCAN_FILE=""
  echo "data-manifest: wrote $out" >&2
  printf '%s\n' "$out"
}

cmd_check() {
  parse_common_args "$@"
  [ -n "$NAME" ] || fail "check requires --name"
  local p
  p="$(require_manifest "$NAME")"
  # Restore the manifest's id-column/index and key columns so the recompute
  # matches capture (key columns make fingerprint_split emit their hashes).
  ID_COLUMN="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("id_column",""))' "$p")"
  ID_INDEX="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("id_index",""))' "$p")"
  KEY_COLUMNS=()
  local k
  while IFS= read -r k; do
    [ -n "$k" ] && KEY_COLUMNS+=("$k")
  done < <(python3 -c 'import json,sys
for k in json.load(open(sys.argv[1])).get("leakage_keys", []): print(k)' "$p")

  local drift=0 want_json label file cur verdict status msg
  while IFS= read -r want_json; do
    [ -n "$want_json" ] || continue
    label="$(printf '%s' "$want_json" | python3 -c 'import json,sys;print(json.load(sys.stdin)["label"])')"
    file="$(printf '%s' "$want_json" | python3 -c 'import json,sys;print(json.load(sys.stdin)["file"])')"
    [ -n "$label" ] || continue
    if [ ! -f "$file" ]; then
      echo "DRIFT $label: file missing ($file)"
      drift=1
      continue
    fi
    cur="$(fingerprint_split "$label" "$file")" || exit $?
    # Compare byte hash, then the logical ID set and every key-column set; a
    # changed key set (e.g. a new scaffold/cluster assignment) is drift even
    # when the ID set is stable.
    verdict="$(OMS_WANT="$want_json" OMS_CUR="$cur" python3 <<'PY'
import json, os
w = json.loads(os.environ["OMS_WANT"]); c = json.loads(os.environ["OMS_CUR"])
label, file = w["label"], w["file"]
if c.get("sha256") == w.get("sha256"):
    print("ok\tok    %s: unchanged" % label)
    raise SystemExit
reasons = []
if c.get("id_sha256") != w.get("id_sha256"):
    reasons.append("ID set")
wk, ck = w.get("keys", {}), c.get("keys", {})
for k in sorted(set(wk) | set(ck)):
    a, b = wk.get(k, {}), ck.get(k, {})
    # drift on the key value set, or on the (id -> key) mapping when both sides
    # recorded it (older manifests predate pair_sha256 -- no false positive).
    # No apostrophes in this heredoc: it sits inside $(...) and bash 3.2
    # (macOS) quote-scans heredoc bodies there, so an odd quote breaks bash -n.
    sha_diff = a.get("sha256") != b.get("sha256")
    pair_diff = ("pair_sha256" in a and "pair_sha256" in b
                 and a["pair_sha256"] != b["pair_sha256"])
    if sha_diff or pair_diff:
        reasons.append("key '%s'" % k)
if reasons:
    print("DRIFT\tDRIFT %s: %s changed (%s)" % (label, ", ".join(reasons), file))
else:
    print("WARN\tWARN  %s: file bytes changed but ID/key sets identical (%s)" % (label, file))
PY
)"
    status="${verdict%%$'\t'*}"
    msg="${verdict#*$'\t'}"
    echo "$msg"
    [ "$status" = "DRIFT" ] && drift=1
  done <<EOF
$(python3 -c 'import json, sys
m = json.load(open(sys.argv[1]))
for s in m.get("splits", []):
    print(json.dumps(s, ensure_ascii=False))' "$p")
EOF
  [ "$drift" = 0 ] || exit 1
}

cmd_leakage() {
  parse_common_args "$@"
  [ -n "$NAME" ] || fail "leakage requires --name"
  local p
  p="$(require_manifest "$NAME")"
  ID_COLUMN="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("id_column",""))' "$p")"
  ID_INDEX="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("id_index",""))' "$p")"

  local tmpdir
  tmpdir="$(mktemp -d)" || fail "mktemp failed"
  local split_json label file idx
  idx=0
  while IFS= read -r split_json; do
    [ -n "$split_json" ] || continue
    label="$(printf '%s' "$split_json" | python3 -c 'import json,sys;print(json.load(sys.stdin)["label"])')"
    file="$(printf '%s' "$split_json" | python3 -c 'import json,sys;print(json.load(sys.stdin)["file"])')"
    [ -n "$label" ] || continue
    # Fail closed: a leakage gate that silently skips an unreadable split could
    # report "clean" while never checking it.
    [ -f "$file" ] || { rm -rf "$tmpdir"; fail "leakage: recorded split file missing: $file"; }
    printf '%s' "$label" > "$tmpdir/$idx.label"
    if ! emit_ids "$file" > "$tmpdir/$idx.ids.raw"; then
      rm -rf "$tmpdir"
      fail "leakage: cannot extract IDs from $file (recorded id-column missing?)"
    fi
    sort -u "$tmpdir/$idx.ids.raw" > "$tmpdir/$idx.ids"
    idx=$((idx + 1))
  done <<EOF
$(python3 -c '
import json, sys
m = json.load(open(sys.argv[1]))
for s in m.get("splits", []):
    print(json.dumps({"label": s["label"], "file": s["file"]}, ensure_ascii=False))
' "$p")
EOF

  local found=0 a b overlap
  local -a files=("$tmpdir"/*.ids)
  local i j
  for ((i = 0; i < ${#files[@]}; i++)); do
    for ((j = i + 1; j < ${#files[@]}; j++)); do
      a="${files[i]}"; b="${files[j]}"
      [ -f "$a" ] && [ -f "$b" ] || continue
      overlap="$(comm -12 "$a" "$b" | wc -l | tr -d ' ')"
      local la lb
      la="$(cat "$tmpdir/$(basename "$a" .ids).label")"
      lb="$(cat "$tmpdir/$(basename "$b" .ids).label")"
      if [ "$overlap" -gt 0 ]; then
        echo "LEAKAGE $la ∩ $lb: $overlap shared ID(s)"
        found=1
        if [ "$SHOW_EXAMPLES" = 1 ]; then
          comm -12 "$a" "$b" | head -n 5 | sed 's/^/    /'
        fi
      else
        echo "ok      $la ∩ $lb: no overlap"
      fi
    done
  done
  rm -rf "$tmpdir"

  # Entity overlap on recorded key columns (chem-bio leakage). Recomputed from
  # the current files, like the ID overlap above. The project owns the chemistry
  # (canonical SMILES -> inchikey/scaffold, sequence -> cluster); the harness
  # only flags train/eval overlap on those precomputed columns.
  local key_found=0
  OMS_MANIFEST="$p" OMS_SHOW="$SHOW_EXAMPLES" python3 <<'PY' || key_found=$?
import csv, json, os, sys

m = json.load(open(os.environ["OMS_MANIFEST"]))
keys = m.get("leakage_keys", [])
if not keys:
    sys.exit(0)
splits = [(s["label"], s["file"]) for s in m.get("splits", [])]
show = os.environ.get("OMS_SHOW") == "1"

def column_values(path, key):
    with open(path, newline="", encoding="utf-8", errors="replace") as fh:
        sample = fh.read(4096); fh.seek(0)
        delim = "\t" if "\t" in sample and sample.count("\t") >= sample.count(",") else ","
        rows = list(csv.reader(fh, delimiter=delim))
    if not rows:
        return set()
    header = rows[0]
    if key not in header:
        return None
    ci = header.index(key)
    return {r[ci].strip() for r in rows[1:] if len(r) > ci and r[ci].strip()}

found = 0
for key in keys:
    vals = {}
    for label, path in splits:
        # Fail closed (exit 2): never report a key "clean" by silently skipping
        # a split whose file or key column went missing.
        if not os.path.isfile(path):
            sys.stderr.write("leakage: recorded split file missing: %s\n" % path)
            sys.exit(2)
        v = column_values(path, key)
        if v is None:
            sys.stderr.write("leakage: recorded key-column %r not in header of %s\n" % (key, path))
            sys.exit(2)
        vals[label] = v
    labels = list(vals)
    for i in range(len(labels)):
        for j in range(i + 1, len(labels)):
            la, lb = labels[i], labels[j]
            overlap = vals[la] & vals[lb]
            if overlap:
                print("LEAKAGE[%s] %s ∩ %s: %d shared key(s)" % (key, la, lb, len(overlap)))
                found = 1
                if show:
                    for x in sorted(overlap)[:5]:
                        print("    " + x)
            else:
                print("ok[%s]      %s ∩ %s: no overlap" % (key, la, lb))
sys.exit(1 if found else 0)
PY
  case "$key_found" in
    0) ;;
    1) found=1 ;;
    *) fail "leakage: key-column check failed" ;;
  esac

  [ "$found" = 0 ] || exit 1
}

cmd_list() {
  [ -d "$MANIFEST_DIR" ] || { echo "no manifests"; return 0; }
  local f
  for f in "$MANIFEST_DIR"/*.json; do
    [ -f "$f" ] || continue
    python3 -c '
import json, sys
m = json.load(open(sys.argv[1]))
splits = ",".join(s["label"] for s in m.get("splits", []))
print("%-24s splits=[%s] ts=%s" % (m.get("name"), splits, m.get("ts")))
' "$f"
  done
}

cmd_show() {
  parse_common_args "$@"
  [ -n "$NAME" ] || fail "show requires --name"
  cat "$(require_manifest "$NAME")"
}

parse_common_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --name) [ "$#" -ge 2 ] || fail "--name requires a value"; NAME="$2"; shift 2 ;;
      --split) [ "$#" -ge 2 ] || fail "--split requires LABEL=FILE"; SPLITS+=("$2"); shift 2 ;;
      --id-column) [ "$#" -ge 2 ] || fail "--id-column requires a name"; ID_COLUMN="$2"; shift 2 ;;
      --id-index) [ "$#" -ge 2 ] || fail "--id-index requires N"
        case "$2" in *[!0-9]*|"") fail "--id-index must be a non-negative integer" ;; esac
        ID_INDEX="$2"; shift 2 ;;
      --key-column) [ "$#" -ge 2 ] || fail "--key-column requires a name"; KEY_COLUMNS+=("$2"); shift 2 ;;
      --note) [ "$#" -ge 2 ] || fail "--note requires text"; NOTE="$2"; shift 2 ;;
      --show-examples) SHOW_EXAMPLES=1; shift ;;
      *) fail "unknown argument: $1" ;;
    esac
  done
  [ -z "$ID_COLUMN" ] || [ -z "$ID_INDEX" ] ||
    fail "--id-column and --id-index are mutually exclusive"
}

case "${1:-}" in
  create) shift; cmd_create "$@" ;;
  check) shift; cmd_check "$@" ;;
  leakage) shift; cmd_leakage "$@" ;;
  list) shift; [ "$#" -eq 0 ] || fail "list takes no arguments"; cmd_list ;;
  show) shift; cmd_show "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
