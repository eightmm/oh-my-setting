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

# Anchored to the git worktree root so manifests do not fork per subdirectory.
STATE_ROOT="$(oms_repo_root "$PWD")"
MANIFEST_DIR="${OMS_MANIFEST_DIR:-$STATE_ROOT/.oms/manifests}"
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
declare -a ENTITY_COLUMNS=()
POLICY=""
TIME_COLUMN=""
DERIVE_CMD=""
DERIVE_SCRIPT=""
SPLIT_RATIONALE=""
declare -a SOURCES=()
declare -a SOURCE_DIGESTS=()
declare -a ENTITY_KINDS=()
SCAN_FILE=""
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: data-manifest.sh create --name NAME --split LABEL=FILE [options]
       data-manifest.sh check --name NAME
       data-manifest.sh leakage --name NAME [--show-examples]
       data-manifest.sh list [--json]
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
  --policy P         Declare how the splits were made, so `leakage` checks that
                     policy's invariant and not only ID overlap:
                       random    exact ID overlap only
                       group     needs --key-column; no key may cross splits
                       scaffold  as group, for a precomputed scaffold key
                       cold      needs --entity-column; no entity (drug, target,
                                 ...) may cross splits, each column on its own
                       time      needs --time-column; list --split oldest first;
                                 every row of a later split must be later than
                                 every row of an earlier one
  --time-column NAME CSV/TSV header column holding a number or an ISO-8601
                     date for --policy time; values must be present and use
                     consistently naive or timezone-aware timestamps.
  --entity-column NAME  CSV/TSV header column for --policy cold. Repeatable;
                     also recorded and checked like a --key-column.
  --derive-cmd TEXT  The command that computed the key columns (recorded as
                     text; never run here).
  --derive-script FILE  The script that computed them; its sha256 is recorded
                     and `check` warns when the file changed or is gone.
  --source NAME=REF  A named upstream source and a free-text reference
                     (accession, release tag, URL, DOI, internal label), e.g.
                     --source pdbbind=v2020. Repeatable. Stored as given;
                     nothing is fetched or validated. A REF that looks like a
                     credential or carries URL userinfo is refused.
  --source-digest NAME=SHA256  The sha256 you computed for that source's
                     archive (64 hex characters; format-checked only).
  --split-rationale TEXT  One line: why this policy and these keys fit the
                     claim, e.g. "cold by protein family: test families unseen".
  --entity-kind COLUMN=KIND  What an entity/key column identifies: protein,
                     ligand, complex, pocket, assay or other. Repeatable. A
                     label only; `leakage` uses it to say "cold by protein".
  --note TEXT        Free-text note.
  (default: each non-empty line is one ID.)

check    Recompute current split files; nonzero exit on any drift.
leakage  Report ID overlap between splits; nonzero exit if any overlap. Also
         reports overlap on every recorded --key-column, tagged [key], the
         declared policy's invariant, and IDs repeated inside one split
         (a warning). Exact matches only: analogs, homologs and other
         near-duplicates need the project's own similarity tooling.
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

# One free-text provenance field: one bounded printable line, no credential
# shape, no URL userinfo. The text is never echoed back in the refusal.
check_provenance_text() {
  local what="$1" text="$2" max="$3"
  case "$text" in *$'\n'*|*$'\r'*) fail "$what must be a single line" ;; esac
  [[ "$text" != *[[:cntrl:]]* ]] || fail "$what must not contain control characters"
  [ -n "${text//[[:space:]]/}" ] || fail "$what must not be empty"
  [ "${#text}" -le "$max" ] || fail "$what is longer than $max characters"
  if [[ "$text" =~ [A-Za-z][A-Za-z0-9+.-]*://[^/[:space:]]*@ ]]; then
    fail "$what contains URL userinfo (user@host); remove the credentials"
  fi
  SCAN_FILE="$(mktemp)" || fail "mktemp failed"
  printf '%s\n' "$text" > "$SCAN_FILE"
  if agent_memory_file_has_secret_content "$SCAN_FILE"; then
    fail "$what looks like a credential; refusing to record it"
  fi
  rm -f "$SCAN_FILE"
  SCAN_FILE=""
}

# --source / --source-digest / --split-rationale / --entity-kind: validated
# before anything is written. Nothing is fetched or parsed.
validate_provenance() {
  local item name ref digest col kind known seen
  local -a seen_names=() seen_cols=()
  for item in "${SOURCES[@]:-}"; do
    [ -n "$item" ] || continue
    name="${item%%=*}"; ref="${item#*=}"
    [ "$name" != "$item" ] || fail "bad --source (want NAME=REF): $item"
    case "$name" in
      ""|*[!A-Za-z0-9._-]*) fail "--source NAME must match [A-Za-z0-9._-]+" ;;
    esac
    for known in "${seen_names[@]:-}"; do
      [ "$known" != "$name" ] || fail "--source $name given twice"
    done
    seen_names+=("$name")
    check_provenance_text "--source $name REF" "$ref" 200
  done
  for item in "${SOURCE_DIGESTS[@]:-}"; do
    [ -n "$item" ] || continue
    name="${item%%=*}"; digest="${item#*=}"
    [ "$name" != "$item" ] || fail "bad --source-digest (want NAME=SHA256): $item"
    seen=0
    for known in "${seen_names[@]:-}"; do
      [ "$known" != "$name" ] || seen=1
    done
    [ "$seen" = 1 ] || fail "--source-digest $name has no matching --source $name"
    [[ "$digest" =~ ^[0-9A-Fa-f]{64}$ ]] || fail "--source-digest $name must be 64 hex characters (sha256)"
  done
  [ -z "$SPLIT_RATIONALE" ] || check_provenance_text "--split-rationale" "$SPLIT_RATIONALE" 240
  for item in "${ENTITY_KINDS[@]:-}"; do
    [ -n "$item" ] || continue
    col="${item%%=*}"; kind="${item#*=}"
    [ "$col" != "$item" ] && [ -n "$col" ] || fail "bad --entity-kind (want COLUMN=KIND): $item"
    case "$kind" in
      protein|ligand|complex|pocket|assay|other) ;;
      *) fail "--entity-kind KIND must be one of: protein, ligand, complex, pocket, assay, other" ;;
    esac
    seen=0
    for known in "${KEY_COLUMNS[@]:-}"; do
      [ "$known" != "$col" ] || seen=1
    done
    [ -z "$ID_COLUMN" ] || [ "$ID_COLUMN" != "$col" ] || seen=1
    [ "$seen" = 1 ] || fail "--entity-kind column $col is not an --entity-column, --key-column or --id-column"
    for known in "${seen_cols[@]:-}"; do
      [ "$known" != "$col" ] || fail "--entity-kind $col given twice"
    done
    seen_cols+=("$col")
  done
}

cmd_create() {
  parse_common_args "$@"
  [ -n "$NAME" ] || fail "create requires --name"
  [ "${#SPLITS[@]}" -gt 0 ] || fail "create requires at least one --split LABEL=FILE"

  case "$POLICY" in
    ""|random) ;;
    group|scaffold)
      [ "${#KEY_COLUMNS[@]}" -gt 0 ] ||
        fail "--policy $POLICY needs at least one --key-column holding the precomputed group key" ;;
    cold)
      [ "${#ENTITY_COLUMNS[@]}" -gt 0 ] ||
        fail "--policy cold needs at least one --entity-column" ;;
    time)
      [ -n "$TIME_COLUMN" ] || fail "--policy time needs --time-column" ;;
    *) fail "--policy must be one of: random, group, scaffold, cold, time" ;;
  esac
  [ -z "$TIME_COLUMN" ] || [ "$POLICY" = time ] || fail "--time-column needs --policy time"
  [ "${#ENTITY_COLUMNS[@]}" -eq 0 ] || [ "$POLICY" = cold ] || fail "--entity-column needs --policy cold"
  [ -z "$DERIVE_SCRIPT" ] || [ -f "$DERIVE_SCRIPT" ] || fail "--derive-script not found: $DERIVE_SCRIPT"
  # An entity column is checked exactly like a key column, so it is one.
  local entity known seen
  for entity in "${ENTITY_COLUMNS[@]:-}"; do
    [ -n "$entity" ] || continue
    seen=0
    for known in "${KEY_COLUMNS[@]:-}"; do
      [ "$known" != "$entity" ] || seen=1
    done
    [ "$seen" = 1 ] || KEY_COLUMNS+=("$entity")
  done
  validate_provenance
  # Every column looked up by header name is validated in every split below.
  local -a header_columns=()
  [ "${#KEY_COLUMNS[@]}" -eq 0 ] || header_columns=("${KEY_COLUMNS[@]}")
  [ -z "$TIME_COLUMN" ] || header_columns+=("$TIME_COLUMN")

  mkdir -p "$MANIFEST_DIR"
  agent_memory_ensure_oms_ignore_for_path "$MANIFEST_DIR" 2>/dev/null || true

  # Key columns are looked up by CSV/TSV header name; validate they exist in
  # every split before recording them, so leakage cannot silently no-op later.
  if [ "${#header_columns[@]}" -gt 0 ]; then
    local pair label file
    for pair in "${SPLITS[@]}"; do
      label="${pair%%=*}"; file="${pair#*=}"
      [ -f "$file" ] || fail "split file not found: $file"
      OMS_FILE="$file" OMS_KEYS="$(printf '%s\n' "${header_columns[@]}")" python3 <<'PY' || exit $?
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
  local entities_nl=""
  [ "${#ENTITY_COLUMNS[@]}" -gt 0 ] && entities_nl="$(printf '%s\n' "${ENTITY_COLUMNS[@]}")"
  OMS_SPLITS="$splits_json" OMS_KEYS="$keys_nl" OMS_ENTITIES="$entities_nl" OMS_POLICY="$POLICY" \
  OMS_TIME_COLUMN="$TIME_COLUMN" OMS_DERIVE_CMD="$DERIVE_CMD" OMS_DERIVE_SCRIPT="$DERIVE_SCRIPT" \
  OMS_SOURCES="$(printf '%s\n' "${SOURCES[@]:-}")" OMS_SOURCE_DIGESTS="$(printf '%s\n' "${SOURCE_DIGESTS[@]:-}")" \
  OMS_RATIONALE="$SPLIT_RATIONALE" OMS_ENTITY_KINDS="$(printf '%s\n' "${ENTITY_KINDS[@]:-}")" \
    python3 - "$SCHEMA" "$NAME" "$ts" "$NOTE" "$ID_COLUMN" "$ID_INDEX" > "$tmp" <<'PY'
import hashlib, json, os, sys
a = sys.argv[1:]
env = os.environ.get
keys = [k for k in env("OMS_KEYS", "").splitlines() if k]
manifest = {
    "schema": int(a[0]),
    "name": a[1],
    "ts": a[2],
    "note": a[3],
    "id_column": a[4],
    "id_index": a[5],
    "leakage_keys": keys,
    "splits": json.loads(os.environ["OMS_SPLITS"]),
}
# Optional: how the splits were made and where their keys came from. Absent
# fields mean "not declared"; readers that predate them never look.
if env("OMS_POLICY"):
    manifest["policy"] = env("OMS_POLICY")
if env("OMS_TIME_COLUMN"):
    manifest["time_column"] = env("OMS_TIME_COLUMN")
entities = [k for k in env("OMS_ENTITIES", "").splitlines() if k]
if entities:
    manifest["entity_columns"] = entities
derive = {}
if env("OMS_DERIVE_CMD"):
    derive["cmd"] = " ".join(env("OMS_DERIVE_CMD").split())[:500]
if env("OMS_DERIVE_SCRIPT"):
    digest = hashlib.sha256()
    with open(env("OMS_DERIVE_SCRIPT"), "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            digest.update(chunk)
    derive["script"] = env("OMS_DERIVE_SCRIPT")
    derive["script_sha256"] = digest.hexdigest()
if derive:
    manifest["derive"] = derive
# Optional provenance, all free text or labels as given (validated by the
# caller): upstream sources, why this split, what each key column identifies.
digests = {}
for line in env("OMS_SOURCE_DIGESTS", "").splitlines():
    if "=" in line:
        digests[line.split("=", 1)[0]] = line.split("=", 1)[1].lower()
sources = []
for line in env("OMS_SOURCES", "").splitlines():
    if "=" in line:
        name, ref = line.split("=", 1)
        row = {"name": name, "ref": " ".join(ref.split())}
        if name in digests:
            row["sha256"] = digests[name]
        sources.append(row)
if sources:
    manifest["sources"] = sources
if env("OMS_RATIONALE"):
    manifest["split_rationale"] = " ".join(env("OMS_RATIONALE").split())
kinds = {}
for line in env("OMS_ENTITY_KINDS", "").splitlines():
    if "=" in line:
        kinds[line.split("=", 1)[0]] = line.split("=", 1)[1]
if kinds:
    manifest["entity_kinds"] = kinds
print(json.dumps(manifest, ensure_ascii=False, indent=2))
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
  # Where the keys came from: a changed or missing key script is a warning, not
  # drift (the key sets themselves are fingerprinted above), because the keys on
  # disk may predate the edit.
  OMS_MANIFEST="$p" python3 <<'PY'
import hashlib, json, os
derive = json.load(open(os.environ["OMS_MANIFEST"])).get("derive")
if isinstance(derive, dict) and derive.get("script"):
    path = derive["script"]
    if not os.path.isfile(path):
        print("WARN  derive: key script is gone (%s)" % path)
    else:
        digest = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(65536), b""):
                digest.update(chunk)
        if digest.hexdigest() != derive.get("script_sha256"):
            print("WARN  derive: key script changed since the manifest was written (%s); "
                  "recompute the keys and recreate the manifest" % path)
        else:
            print("ok    derive: key script unchanged")
# Declared provenance is printed as recorded; nothing here is fetched or
# re-verified, so it never changes the exit code.
m = json.load(open(os.environ["OMS_MANIFEST"]))
for src in m.get("sources") or []:
    if isinstance(src, dict):
        print("info  source %s: %s%s" % (src.get("name"), src.get("ref"),
              " (sha256 %s, as declared)" % src["sha256"] if src.get("sha256") else ""))
if m.get("split_rationale"):
    print("info  split rationale: %s" % m["split_rationale"])
if isinstance(m.get("entity_kinds"), dict) and m["entity_kinds"]:
    print("info  entity kinds: %s" % ", ".join("%s=%s" % kv for kv in sorted(m["entity_kinds"].items())))
PY
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
          # Capture first, then head the file: comm piped into an
          # early-exiting head took SIGPIPE under pipefail precisely when
          # the leak was largest, killing the report right after its first
          # LEAKAGE line with the remaining pairs unchecked.
          comm -12 "$a" "$b" > "$tmpdir/leak-examples"
          head -n 5 "$tmpdir/leak-examples" | sed 's/^/    /'
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

  # The declared split policy's own invariant, IDs repeated inside one split,
  # and what none of this looked at. Exit 1 = the policy is violated, 2 = the
  # manifest or a file cannot support the check (fail closed).
  local policy_found=0
  OMS_MANIFEST="$p" OMS_SHOW="$SHOW_EXAMPLES" python3 <<'PY' || policy_found=$?
import csv, json, os, re, sys
from datetime import datetime, timezone

m = json.load(open(os.environ["OMS_MANIFEST"]))
show = os.environ.get("OMS_SHOW") == "1"
policy = m.get("policy") or ""
keys = [k for k in m.get("leakage_keys", []) if isinstance(k, str)]
entities = [k for k in m.get("entity_columns", []) if isinstance(k, str)]
splits = [(s["label"], s["file"]) for s in m.get("splits", [])]
found = 0

def stop(text):
    sys.stderr.write("leakage: %s\n" % text)
    sys.exit(2)

def table(path):
    with open(path, newline="", encoding="utf-8", errors="replace") as fh:
        sample = fh.read(4096); fh.seek(0)
        delim = "\t" if "\t" in sample and sample.count("\t") >= sample.count(",") else ","
        return list(csv.reader(fh, delimiter=delim))

def ids(path):
    col, idx = m.get("id_column") or "", m.get("id_index")
    if col or idx not in ("", None):
        rows = table(path)
        if not rows:
            return []
        if col:
            if col not in rows[0]:
                stop("recorded id-column %r not in header of %s" % (col, path))
            ci, body = rows[0].index(col), rows[1:]
        else:
            ci, body = int(idx), rows
        return [r[ci].strip() for r in body if len(r) > ci and r[ci].strip()]
    with open(path, encoding="utf-8", errors="replace") as fh:
        return [ln.strip() for ln in fh if ln.strip()]

# A row counted twice inside one split inflates that split and, in a time or
# group split, can hide which copy the policy was applied to.
for label, path in splits:
    seen, repeated = set(), set()
    for value in ids(path):
        (repeated if value in seen else seen).add(value)
    if repeated:
        print("WARN    %s: %d ID(s) appear more than once inside the split" % (label, len(repeated)))
        if show:
            for value in sorted(repeated)[:5]:
                print("    " + value)

if policy in ("group", "scaffold") and not keys:
    stop("policy %s is declared but the manifest records no key column" % policy)
if policy == "cold" and not (entities and all(e in keys for e in entities)):
    stop("policy cold is declared but its entity columns are not recorded as keys")

if policy == "time":
    column = m.get("time_column") or ""
    if not column:
        stop("policy time is declared but the manifest records no time column")
    values = {}
    for label, path in splits:
        rows = table(path)
        if not rows or column not in rows[0]:
            stop("recorded time-column %r not in header of %s" % (column, path))
        ci = rows[0].index(column)
        raw = [r[ci].strip() if len(r) > ci else "" for r in rows[1:]]
        empty = sum(1 for v in raw if not v)
        if empty:
            stop("%s: %d row(s) have no %s value" % (label, empty, column))
        raw = [v for v in raw if v]
        if not raw:
            stop("split %s has no %s values to order" % (label, column))
        values[label] = raw
    def number(text):
        try:
            value = float(text)
        except ValueError:
            return None
        return value if value == value and abs(value) != float("inf") else None
    flat = [v for vals in values.values() for v in vals]
    if all(number(v) is not None for v in flat):
        ordered = {label: [number(v) for v in vals] for label, vals in values.items()}
    else:
        try:
            ordered = {label: [datetime.fromisoformat(v) for v in vals] for label, vals in values.items()}
        except ValueError:
            stop("time column %r must hold numbers or valid ISO-8601 dates throughout" % column)
        aware = {v.utcoffset() is not None for vals in ordered.values() for v in vals}
        if len(aware) > 1:
            stop("time column %r mixes naive and timezone-aware timestamps" % column)
        if True in aware:
            ordered = {label: [v.astimezone(timezone.utc) for v in vals] for label, vals in ordered.items()}
    labels = [label for label, _ in splits]
    for i in range(len(labels)):
        for j in range(i + 1, len(labels)):
            early, late = labels[i], labels[j]
            newest = max(ordered[early])
            behind = sum(1 for v in ordered[late] if v <= newest)
            if behind:
                found = 1
                print("LEAKAGE[time:%s] %s -> %s: %d row(s) of %s are not later than the newest %s row"
                      % (column, early, late, behind, late, early))
                if show:
                    print("    newest %s: %s; oldest %s: %s" % (early, str(newest).removesuffix(" 00:00:00"), late, str(min(ordered[late])).removesuffix(" 00:00:00")))
            else:
                print("ok[time:%s]      %s -> %s: every %s row is later" % (column, early, late, late))

kinds = m.get("entity_kinds") if isinstance(m.get("entity_kinds"), dict) else {}
rationale = m.get("split_rationale") if isinstance(m.get("split_rationale"), str) else ""
if policy in ("cold", "scaffold"):
    # Labels only: what the entity (cold) or group (scaffold) columns identify.
    columns = entities if policy == "cold" else keys
    labelled = []
    for column in columns:
        if isinstance(kinds.get(column), str) and kinds[column] and kinds[column] not in labelled:
            labelled.append(kinds[column])
    if not labelled:
        print("WARN    policy %s has no --entity-kind recorded; the report cannot say what crosses splits" % policy)
    if not rationale:
        print("WARN    policy %s has no --split-rationale recorded; the claim it supports is not stated" % policy)
    if labelled:
        policy = "%s by %s" % (policy, "+".join(labelled))

if policy:
    checked = ["ID overlap"]
    if keys:
        checked.append("key overlap on " + ", ".join(keys))
    if policy == "time":
        checked.append("time order on " + (m.get("time_column") or ""))
    print("policy: %s (checked: %s)" % (policy, "; ".join(checked)))
    if rationale:
        print("rationale: %s" % rationale)
else:
    print("policy: none declared (checked: ID overlap%s)" % ("; key overlap on " + ", ".join(keys) if keys else ""))
print("note: exact matches only; similar compounds, homologous sequences and other near-duplicates were not compared")
sys.exit(found)
PY
  case "$policy_found" in
    0) ;;
    1) found=1 ;;
    *) fail "leakage: split policy check failed" ;;
  esac

  [ "$found" = 0 ] || exit 1
}

cmd_list() {
  local as_json=0
  if [ "${1:-}" = "--json" ]; then as_json=1; shift; fi
  [ "$#" -eq 0 ] || fail "list takes no arguments"
  if [ ! -d "$MANIFEST_DIR" ]; then
    if [ "$as_json" -eq 1 ]; then
      echo '{"schema": 1, "manifests": []}'
    else
      echo "no manifests"
    fi
    return 0
  fi
  OMS_DM_DIR="$MANIFEST_DIR" OMS_DM_JSON="$as_json" python3 -c '
import glob, json, os
rows = []
for path in sorted(glob.glob(os.path.join(os.environ["OMS_DM_DIR"], "*.json"))):
    try:
        m = json.load(open(path, encoding="utf-8"))
    except Exception:
        continue
    labels = [s.get("label") for s in m.get("splits", [])]
    if os.environ.get("OMS_DM_JSON") == "1":
        rows.append({"name": m.get("name"), "ts": m.get("ts"),
                     "splits": labels, "path": path})
    else:
        print("%-24s splits=[%s] ts=%s" % (m.get("name"), ",".join(l or "" for l in labels), m.get("ts")))
if os.environ.get("OMS_DM_JSON") == "1":
    print(json.dumps({"schema": 1, "manifests": rows}, ensure_ascii=False))
'
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
      --policy) [ "$#" -ge 2 ] || fail "--policy requires a name"; POLICY="$2"; shift 2 ;;
      --time-column) [ "$#" -ge 2 ] || fail "--time-column requires a name"; TIME_COLUMN="$2"; shift 2 ;;
      --entity-column) [ "$#" -ge 2 ] || fail "--entity-column requires a name"; ENTITY_COLUMNS+=("$2"); shift 2 ;;
      --derive-cmd) [ "$#" -ge 2 ] || fail "--derive-cmd requires text"; DERIVE_CMD="$2"; shift 2 ;;
      --derive-script) [ "$#" -ge 2 ] || fail "--derive-script requires a file"; DERIVE_SCRIPT="$2"; shift 2 ;;
      --source) [ "$#" -ge 2 ] || fail "--source requires NAME=REF"; SOURCES+=("$2"); shift 2 ;;
      --source-digest) [ "$#" -ge 2 ] || fail "--source-digest requires NAME=SHA256"; SOURCE_DIGESTS+=("$2"); shift 2 ;;
      --split-rationale) [ "$#" -ge 2 ] || fail "--split-rationale requires text"; SPLIT_RATIONALE="$2"; shift 2 ;;
      --entity-kind) [ "$#" -ge 2 ] || fail "--entity-kind requires COLUMN=KIND"; ENTITY_KINDS+=("$2"); shift 2 ;;
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
  list) shift; cmd_list "$@" ;;
  show) shift; cmd_show "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
