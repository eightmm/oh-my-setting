#!/usr/bin/env bash
set -euo pipefail

# Redaction-first diagnostic bundle for peer consultation and bug reports.
# Harness state is intentionally rich and therefore unsafe to copy ad hoc:
# task packets, ledgers, and journals carry paths, goals, and command lines.
# This collects only bounded, derived summaries (state, state-verify,
# task/journal status, fail-ledger rows, artifact index counts — never
# artifact contents, raw logs, diffs, datasets, or credentials), strips the
# repo path, home directory, and hostname from every file, then runs the
# shared sensitive-content scrubber; a file that still trips it is replaced
# by an omission notice. MANIFEST.md records what was included, what was
# omitted and why, and what is never collected.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

usage() {
  cat <<'EOF'
Usage: support-bundle.sh [--repo PATH] [--out DIR] [--dry-run]

Collect a redacted, bounded diagnostic bundle from a repository's harness
state, safe to attach to a peer consultation or bug report.

Options:
  --repo PATH  Repository to bundle. Default: current directory.
  --out DIR    Parent directory for the bundle.
               Default: <repo>/.oms/support/.
  --dry-run    Print what would be collected; write nothing.
  -h, --help   Show this help.

Exit status: 0 bundle written (omissions are recorded, not fatal), 2 misuse.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

REPO="$PWD"
OUT=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --out) [ "$#" -ge 2 ] || fail "--out requires a directory"; OUT="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")"
[ -d "$REPO/.oms" ] || fail "$REPO is not harness-adopted (no .oms)"
[ -n "$OUT" ] || OUT="$REPO/.oms/support"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BUNDLE="$OUT/support-bundle-$STAMP"

COLLECTORS='repo-state.txt state-verify.txt task-status.json journal-status.json failures.json artifacts-summary.txt harness-version.txt'

if [ "$DRY_RUN" = "1" ]; then
  echo "would write $BUNDLE with:"
  for name in $COLLECTORS; do
    echo "  $name"
  done
  echo "redactions: repo path -> <REPO>, home -> <HOME>, hostname -> <HOST>; sensitive-content scan per file"
  exit 0
fi

mkdir -p "$BUNDLE"

collect() {
  local name="$1"
  shift
  # A collector that fails still leaves a record; the bundle never aborts on
  # one family.
  "$@" > "$BUNDLE/$name" 2>&1 || true
}

collect repo-state.txt "$ROOT/scripts/state.sh" --repo "$REPO"
collect state-verify.txt "$ROOT/scripts/state-verify.sh" --repo "$REPO"
collect task-status.json "$ROOT/scripts/agent-task.sh" --repo "$REPO" status --json
collect journal-status.json "$ROOT/scripts/journal.sh" status --repo "$REPO" --json
collect failures.json "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" list --json

OMS_SB_INDEX="$REPO/.oms/artifacts/index.jsonl" python3 - > "$BUNDLE/artifacts-summary.txt" 2>&1 <<'PY' || true
import collections
import json
import os

path = os.environ["OMS_SB_INDEX"]
kinds = collections.Counter()
providers = collections.Counter()
rows = 0
last = ""
try:
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                continue
            rows += 1
            kinds[str(row.get("kind") or "?")] += 1
            providers[str(row.get("provider") or "?")] += 1
            last = max(last, str(row.get("ts") or ""))
except OSError:
    print("no artifact index")
    raise SystemExit
print("rows: %d" % rows)
print("last: %s" % (last or "-"))
for label, counter in (("kind", kinds), ("provider", providers)):
    for key, count in sorted(counter.items()):
        print("%s %s: %d" % (label, key, count))
PY

{
  printf 'version: %s\n' "$(cat "$ROOT/VERSION" 2>/dev/null || echo '?')"
  printf 'commit: %s\n' "$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo '?')"
} > "$BUNDLE/harness-version.txt"

# Redact, then scan. Order matters: the scrubber treats any absolute home,
# scratch, or cluster path as sensitive, so stripping the known-safe local
# identifiers first lets a clean file survive while anything else still
# triggers whole-file omission.
HOSTNAME_VALUE="$(hostname 2>/dev/null || echo '')"
INCLUDED="$BUNDLE/.included"
OMITTED="$BUNDLE/.omitted"
: > "$INCLUDED"
: > "$OMITTED"
for name in $COLLECTORS; do
  file="$BUNDLE/$name"
  [ -f "$file" ] || continue
  OMS_SB_FILE="$file" OMS_SB_REPO="$REPO" OMS_SB_HOME="$HOME" \
  OMS_SB_HOST="$HOSTNAME_VALUE" python3 - <<'PY'
import os

path = os.environ["OMS_SB_FILE"]
with open(path, encoding="utf-8", errors="replace") as handle:
    text = handle.read()
for value, marker in (
    (os.environ["OMS_SB_REPO"], "<REPO>"),
    (os.environ["OMS_SB_HOME"], "<HOME>"),
    (os.environ["OMS_SB_HOST"], "<HOST>"),
):
    if value:
        text = text.replace(value, marker)
with open(path, "w", encoding="utf-8") as handle:
    handle.write(text)
PY
  if agent_memory_file_has_sensitive_content "$file"; then
    printf '%s\n' "$name" >> "$OMITTED"
    printf '[omitted: the redacted content still matched the sensitive-content patterns]\n' > "$file"
  else
    printf '%s\t%s\n' "$name" "$(wc -c < "$file" | tr -d ' ')" >> "$INCLUDED"
  fi
done

{
  echo "# Support Bundle"
  echo
  echo "- created: $STAMP (UTC)"
  echo "- redactions: repo path -> <REPO>, home -> <HOME>, hostname -> <HOST>"
  echo
  echo "## Included"
  echo
  if [ -s "$INCLUDED" ]; then
    while IFS="$(printf '\t')" read -r name bytes; do
      echo "- $name ($bytes bytes)"
    done < "$INCLUDED"
  else
    echo "- none"
  fi
  echo
  echo "## Omitted"
  echo
  if [ -s "$OMITTED" ]; then
    while IFS= read -r name; do
      echo "- $name — redacted content still matched the sensitive-content patterns"
    done < "$OMITTED"
  else
    echo "- none"
  fi
  echo
  echo "## Never collected"
  echo
  echo "- raw logs, command transcripts, artifact contents, diffs or patches"
  echo "- datasets, checkpoints, run capsules"
  echo "- credentials, environment variables, machine or cluster details"
} > "$BUNDLE/MANIFEST.md"
rm -f "$INCLUDED" "$OMITTED"

echo "support-bundle: $BUNDLE"
