#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/skills.manifest.json"

if [ ! -f "$MANIFEST" ]; then
  echo "error: missing $MANIFEST" >&2
  exit 1
fi

FAILED=0

check_skill() {
  local name="$1"
  local source="$2"
  local path="$ROOT/$source"
  local actual_name

  case "$source" in
    custom-skills/*) ;;
    *)
      echo "external: $name -> $source"
      return
      ;;
  esac

  if [ ! -d "$path" ]; then
    echo "missing: $name -> $source"
    FAILED=1
    return
  fi
  if [ ! -f "$path/SKILL.md" ]; then
    echo "missing SKILL.md: $name -> $source/SKILL.md"
    FAILED=1
    return
  fi
  actual_name="$(awk -F: '
    /^name:[[:space:]]*/ {
      sub(/^[[:space:]]+/, "", $2)
      sub(/[[:space:]]+$/, "", $2)
      print $2
      exit
    }
  ' "$path/SKILL.md")"
  if [ -z "$actual_name" ]; then
    echo "missing skill name: $source/SKILL.md"
    FAILED=1
    return
  fi
  if [ "$actual_name" != "$name" ]; then
    echo "name mismatch: $name -> $source/SKILL.md has $actual_name"
    FAILED=1
    return
  fi
  echo "ok: $name -> $source"
}

read_manifest_enabled() {
  if command -v jq >/dev/null 2>&1; then
    jq -r '.skills[] | select(.enabled == true) | [.name, .source] | @tsv' "$MANIFEST"
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c '
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
for s in data.get("skills", []):
    if s.get("enabled") and s.get("source"):
        print(f"{s[\"name\"]}\t{s[\"source\"]}")
' "$MANIFEST"
  else
    echo "error: need jq or python3 to parse $MANIFEST" >&2
    exit 1
  fi
}

while IFS=$'\t' read -r name source; do
  [ -n "$name" ] || continue
  check_skill "$name" "$source"
done < <(read_manifest_enabled)

if [ "$FAILED" -ne 0 ]; then
  echo "install-skills: failed"
  exit 1
fi

echo "install-skills: ok"
echo "Custom skills live in $ROOT/custom-skills and are symlinked by scripts/link.sh."
