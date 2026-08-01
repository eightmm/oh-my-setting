#!/usr/bin/env bash
set -euo pipefail

# Project-scoped skills, forged from evidence. The agent writes a skill it
# can back with inspection or repeated experience; this script provides the
# rails: validate against the Agent Skills budgets, refuse sensitive content,
# store under .oms/skills/, link into the project skill roots every CLI reads
# natively (.agents/skills for Codex/Antigravity, .claude/skills for Claude),
# and keep the links out of git. Skills are local project state — reviewable
# with `list`/`show`, never committed by default.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"

usage() {
  cat <<'EOF'
Usage: skill-forge.sh [--repo PATH] <command> [args]

Commands:
  add --name NAME [--file F]  Store a project skill from F (or stdin): must
                              pass spec validation and the sensitive-content
                              scrubber; then linked and hidden from git.
  validate [NAME]             Validate one or all project skills.
  link                        (Re)link valid skills into .agents/skills and
                              .claude/skills; prune stale owned links.
  list [--json]               List project skills with validity.
  show NAME                   Print a project skill.
  remove NAME                 Remove a skill and its links.
  status                      One-line health summary (used by the doctor).
  contracts                   List declared verify contracts, one
                              "name<TAB>command" row per skill that has one.

Project skills live in <repo>/.oms/skills/<name>/SKILL.md. They load through
each CLI's native project skill discovery — no router entry, no manifest.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) break ;;
  esac
done

REPO="$(oms_repo_root "$REPO")"
SKILLS_DIR="$REPO/.oms/skills"

valid_name() {
  printf '%s' "$1" | grep -Eq '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
}

# Spec conformance for one skill dir: frontmatter name matches the directory,
# a description substantial enough to route on, and the 500-line body budget.
validate_skill_dir() {
  local dir="$1"
  OMS_SF_DIR="$dir" python3 - <<'PY'
import os
import re
import sys

dir_path = os.environ["OMS_SF_DIR"]
path = os.path.join(dir_path, "SKILL.md")
if not os.path.isfile(path):
    print("missing SKILL.md")
    sys.exit(1)
if os.path.getsize(path) > 256 * 1024:
    print("SKILL.md exceeds the 256 KiB input budget")
    sys.exit(1)
with open(path, encoding="utf-8") as fh:
    text = fh.read()
lines = text.splitlines()
if not lines or lines[0].strip() != "---":
    print("missing frontmatter")
    sys.exit(1)
try:
    end = lines.index("---", 1)
except ValueError:
    print("unterminated frontmatter")
    sys.exit(1)
meta = {}
for line in lines[1:end]:
    match = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", line)
    if match:
        meta[match.group(1)] = match.group(2).strip()
name = meta.get("name", "")
directory_name = os.path.basename(dir_path)
if not re.fullmatch(r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?", directory_name):
    print("directory name must be lowercase kebab-case")
    sys.exit(1)
if name != directory_name:
    print("name must match directory: %r" % name)
    sys.exit(1)
if len(meta.get("description", "")) < 40:
    print("description too thin to route on")
    sys.exit(1)
if len(lines) > 500:
    print("over the 500-line body budget (%d)" % len(lines))
    sys.exit(1)
# Optional verification contract: a repo command whose success is the skill's
# evidence. Syntax-only here — the harness records and reminds, it never
# executes standing context on its own.
if "verify" in meta:
    verify = meta["verify"]
    if not verify:
        print("verify: declared but empty")
        sys.exit(1)
    import shlex
    try:
        if not shlex.split(verify):
            raise ValueError
    except ValueError:
        print("verify: is not a parseable command: %r" % verify)
        sys.exit(1)
PY
}

# The declared verify contract of one skill dir, empty when none.
skill_verify_contract() {
  local dir="$1"
  OMS_SF_DIR="$dir" python3 - <<'PY' 2>/dev/null || true
import os, re
path = os.path.join(os.environ["OMS_SF_DIR"], "SKILL.md")
try:
    lines = open(path, encoding="utf-8").read().splitlines()
except OSError:
    raise SystemExit
if not lines or lines[0].strip() != "---":
    raise SystemExit
try:
    end = lines.index("---", 1)
except ValueError:
    raise SystemExit
for line in lines[1:end]:
    match = re.match(r"^verify:\s*(.+)$", line)
    if match:
        print(match.group(1).strip())
        break
PY
}

# The scrubber gate: a project skill is injected into every future session,
# so secret-shaped content must never be stored, linked, or suggested.
scrub_skill_dir() {
  local dir="$1"
  local file
  if [ -n "$(find "$dir" -type l -print -quit 2>/dev/null)" ]; then
    printf 'symlinks are not allowed in project skills\n'
    return 1
  fi
  while IFS= read -r -d '' file; do
    if agent_memory_file_has_sensitive_content "$file"; then
      printf 'sensitive content: %s\n' "$file"
      return 1
    fi
  done < <(find "$dir" -type f -print0)
  return 0
}

link_roots() {
  printf '%s\n' "$REPO/.agents/skills" "$REPO/.claude/skills"
}

owned_link_target() {
  # Prints the resolved target when the link is a symlink into this repo's
  # .oms/skills; fails otherwise so foreign entries are never touched.
  local link="$1"
  local target
  [ -L "$link" ] || return 1
  target="$(readlink "$link")"
  case "$target" in
    "$SKILLS_DIR"/*) printf '%s\n' "$target" ;;
    *) return 1 ;;
  esac
}

hide_from_git() {
  local name="$1"
  git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || return 0
  "$ROOT/scripts/project-private.sh" --repo "$REPO" apply \
    --path ".agents/skills/$name" --path ".claude/skills/$name" \
    >/dev/null 2>&1 || true
}

cmd_link() {
  local root skill name reason linked=0
  while IFS= read -r root; do
    mkdir -p "$root"
    # Prune links we own whose skill no longer exists or no longer validates.
    for skill in "$root"/*; do
      [ -e "$skill" ] || [ -L "$skill" ] || continue
      owned_link_target "$skill" >/dev/null || continue
      name="$(basename "$skill")"
      if [ ! -f "$SKILLS_DIR/$name/SKILL.md" ] ||
         ! reason="$(validate_skill_dir "$SKILLS_DIR/$name")" ||
         ! scrub_skill_dir "$SKILLS_DIR/$name" >/dev/null; then
        rm -f "$skill"
        echo "skill-forge: unlinked $name from $root"
      fi
    done
  done < <(link_roots)
  [ -d "$SKILLS_DIR" ] || { echo "skill-forge: no project skills"; return 0; }
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    name="$(basename "$skill")"
    if ! reason="$(validate_skill_dir "$skill")"; then
      echo "skill-forge: not linking $name: $reason" >&2
      continue
    fi
    if ! reason="$(scrub_skill_dir "$skill")"; then
      echo "skill-forge: not linking $name: $reason" >&2
      continue
    fi
    while IFS= read -r root; do
      ln -sfn "$skill" "$root/$name"
    done < <(link_roots)
    hide_from_git "$name"
    linked=$((linked + 1))
  done
  echo "skill-forge: $linked skill(s) linked"
}

cmd_add() {
  local name="" file="" dir
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --name) [ "$#" -ge 2 ] || fail "--name requires a value"; name="$2"; shift 2 ;;
      --file) [ "$#" -ge 2 ] || fail "--file requires a path"; file="$2"; shift 2 ;;
      *) fail "unknown add argument: $1" ;;
    esac
  done
  [ -n "$name" ] || fail "add requires --name"
  valid_name "$name" || fail "skill names are lowercase kebab-case: $name"
  dir="$SKILLS_DIR/$name"
  if [ -e "$dir" ] || [ -L "$dir" ]; then
    fail "project skill already exists: $name"
  fi
  mkdir -p "$dir"
  if [ -n "$file" ]; then
    [ -f "$file" ] || fail "no such file: $file"
    cp "$file" "$dir/SKILL.md"
  else
    cat > "$dir/SKILL.md"
  fi
  local reason
  if ! reason="$(validate_skill_dir "$dir")"; then
    rm -rf "$dir"
    fail "rejected $name: $reason"
  fi
  if ! reason="$(scrub_skill_dir "$dir")"; then
    rm -rf "$dir"
    fail "rejected $name: $reason (a project skill is standing context; scrub it first)"
  fi
  cmd_link >/dev/null
  echo "skill-forge: added $name ($dir/SKILL.md)"
}

cmd_validate() {
  local target="${1:-}" skill name reason rc=0 checked=0
  [ -z "$target" ] || valid_name "$target" ||
    fail "skill names are lowercase kebab-case: $target"
  [ -d "$SKILLS_DIR" ] || { echo "skill-forge: no project skills"; return 0; }
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    name="$(basename "$skill")"
    [ -z "$target" ] || [ "$name" = "$target" ] || continue
    checked=$((checked + 1))
    if ! reason="$(validate_skill_dir "$skill")"; then
      echo "invalid: $name: $reason"
      rc=1
      continue
    fi
    if ! reason="$(scrub_skill_dir "$skill")"; then
      echo "invalid: $name: $reason"
      rc=1
      continue
    fi
    echo "ok: $name"
  done
  [ -n "$target" ] && [ "$checked" -eq 0 ] && fail "no such skill: $target"
  return "$rc"
}

cmd_list() {
  local as_json=0 skill name state
  [ "${1:-}" = "--json" ] && as_json=1
  if [ "$as_json" -eq 1 ]; then
    printf '{"schema": 1, "skills": ['
    local first=1
    for skill in "$SKILLS_DIR"/*; do
      [ -d "$skill" ] || continue
      name="$(basename "$skill")"
      state=valid
      validate_skill_dir "$skill" >/dev/null 2>&1 &&
        scrub_skill_dir "$skill" >/dev/null 2>&1 || state=invalid
      [ "$first" -eq 1 ] || printf ', '
      first=0
      python3 -c 'import json,sys; print(json.dumps({"name": sys.argv[1], "state": sys.argv[2]}, sort_keys=True), end="")' "$name" "$state"
    done
    printf ']}\n'
    return 0
  fi
  [ -d "$SKILLS_DIR" ] || { echo "no project skills"; return 0; }
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    name="$(basename "$skill")"
    if validate_skill_dir "$skill" >/dev/null 2>&1 &&
       scrub_skill_dir "$skill" >/dev/null 2>&1; then
      echo "valid: $name"
    else
      echo "invalid: $name"
    fi
  done
}

cmd_show() {
  [ "$#" -eq 1 ] || fail "show requires exactly one name"
  valid_name "$1" || fail "skill names are lowercase kebab-case: $1"
  [ -f "$SKILLS_DIR/$1/SKILL.md" ] || fail "no such skill: $1"
  cat "$SKILLS_DIR/$1/SKILL.md"
}

cmd_remove() {
  [ "$#" -eq 1 ] || fail "remove requires exactly one name"
  local name="$1" root
  valid_name "$name" || fail "skill names are lowercase kebab-case: $name"
  [ -d "$SKILLS_DIR/$name" ] || fail "no such skill: $name"
  while IFS= read -r root; do
    owned_link_target "$root/$name" >/dev/null && rm -f "$root/$name"
  done < <(link_roots)
  rm -rf "${SKILLS_DIR:?}/$name"
  echo "skill-forge: removed $name"
}

cmd_status() {
  local total=0 invalid=0 contracts=0 skill
  if [ ! -d "$SKILLS_DIR" ]; then
    echo "skill-forge: no project skills"
    return 0
  fi
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    total=$((total + 1))
    validate_skill_dir "$skill" >/dev/null 2>&1 &&
      scrub_skill_dir "$skill" >/dev/null 2>&1 || invalid=$((invalid + 1))
    [ -z "$(skill_verify_contract "$skill")" ] || contracts=$((contracts + 1))
  done
  if [ "$invalid" -gt 0 ]; then
    echo "skill-forge: $invalid of $total project skill(s) invalid (run 'oms skill-forge validate')"
    return 1
  fi
  if [ "$contracts" -gt 0 ]; then
    echo "skill-forge: $total project skill(s) valid, $contracts with a verify contract"
  else
    echo "skill-forge: $total project skill(s) valid"
  fi
}

# One row per declared verify contract; consumed by agent-task close to
# remind — never to execute.
cmd_contracts() {
  local skill contract
  [ -d "$SKILLS_DIR" ] || return 0
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    contract="$(skill_verify_contract "$skill")"
    [ -z "$contract" ] || printf '%s\t%s\n' "$(basename "$skill")" "$contract"
  done
}

case "${1:-}" in
  add) shift; cmd_add "$@" ;;
  contracts) shift; cmd_contracts "$@" ;;
  validate) shift; cmd_validate "$@" ;;
  link) shift; cmd_link "$@" ;;
  list) shift; cmd_list "$@" ;;
  show) shift; cmd_show "$@" ;;
  remove) shift; cmd_remove "$@" ;;
  status) shift; cmd_status "$@" ;;
  ""|help) usage ;;
  *) fail "unknown command: $1" ;;
esac
