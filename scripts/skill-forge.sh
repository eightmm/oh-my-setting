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
  add --name NAME [--file F]  Store a project skill from F (or stdin): NAME
                              carries the oms- prefix, and the content must
                              pass spec validation and the sensitive-content
                              scrubber; then linked and hidden from git.
  validate [NAME]             Validate one or all project skills.
  link                        (Re)link valid skills into .agents/skills and
                              .claude/skills; prune stale owned links.
  list [--json]               List project skills with validity.
  show NAME                   Print a project skill.
  remove NAME                 Remove a skill and its links.
  status                      Health summary; flags stale project skills.
  contracts                   List declared verify contracts, one
                              "name<TAB>command" row per skill that has one.
  eval NAME --suite FILE      Evaluate an explicit black-box trigger/task
      [--allow-host-commands] baseline. Task commands require the named flag;
      [--record] [--json]     --record appends content-free runtime telemetry.
  preview --source SOURCE     Validate a full skill bundle in quarantine and
      [--ref REF] [--subdir P] print its pinned digest without project writes.
  import|update ... --apply   Publish a reviewed immutable bundle revision;
                              update requires an exact current digest CAS.
  rollback NAME --to SHA      Repoint an imported skill to a verified stored
      --expected-current-sha256 SHA --apply
  derive --from SOURCE        Build an inert, provenance-bound draft from a
      --id ID --name NAME     thread, attempt artifact, or journal event.

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
SKILL_STORE="$REPO/.oms/skill-store"
SKILL_LIFECYCLE_HELPER="$ROOT/scripts/lib/skill-lifecycle.py"

valid_name() {
  printf '%s' "$1" | grep -Eq '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
}

# Spec conformance for one skill dir: frontmatter name matches the directory,
# a description substantial enough to route on, and the 500-line body budget.
# Strict mode (add-time only) additionally enforces the Agent Skills portable
# shape — name/description/compatibility budgets and no top-level fields
# outside the spec set. Read paths stay lenient so an upgraded gate never
# unlinks skills an older gate accepted.
validate_skill_dir() {
  local dir="$1"
  OMS_SF_DIR="$dir" OMS_SF_STRICT="${2:-0}" python3 - <<'PY'
import os
import re
import shlex
import sys

dir_path = os.environ["OMS_SF_DIR"]
strict = os.environ.get("OMS_SF_STRICT") == "1"
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
metadata_map = {}
in_metadata = False
for line in lines[1:end]:
    top = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", line)
    if top:
        meta[top.group(1)] = top.group(2).strip()
        in_metadata = top.group(1) == "metadata"
        continue
    nested = re.match(r"^[ \t]+([A-Za-z0-9_-]+):\s*(.*)$", line)
    if in_metadata and nested:
        metadata_map[nested.group(1)] = nested.group(2).strip()
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
# evidence. Its portable home is metadata.verify; top-level verify: stays
# readable for skills stored before that convention. Syntax-only here — the
# harness records and reminds, it never executes standing context on its own.
for where, declared in (("metadata verify", metadata_map), ("verify", meta)):
    if "verify" not in declared:
        continue
    verify = declared["verify"]
    if not verify:
        print("%s: declared but empty" % where)
        sys.exit(1)
    try:
        if not shlex.split(verify):
            raise ValueError
    except ValueError:
        print("%s: is not a parseable command: %r" % (where, verify))
        sys.exit(1)
if strict:
    if len(name) > 64 or "--" in name:
        print("name must be 1-64 chars with no consecutive hyphens")
        sys.exit(1)
    if len(meta.get("description", "")) > 1024:
        print("description exceeds the 1024-character budget")
        sys.exit(1)
    if len(meta.get("compatibility", "")) > 500:
        print("compatibility exceeds the 500-character budget")
        sys.exit(1)
    known = {"name", "description", "license", "compatibility", "metadata",
             "allowed-tools"}
    for key in meta:
        if key in known:
            continue
        if key == "verify":
            print("verify belongs under the metadata: map (portable extension slot)")
        else:
            print("non-portable frontmatter field %r — put extensions under metadata:" % key)
        sys.exit(1)
PY
}

# The declared verify contract of one skill dir, empty when none.
# metadata.verify is the portable form and wins; legacy top-level verify:
# keeps working for skills stored before the metadata convention.
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
legacy = ""
in_metadata = False
for line in lines[1:end]:
    top = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", line)
    if top:
        in_metadata = top.group(1) == "metadata"
        if top.group(1) == "verify" and top.group(2).strip():
            legacy = top.group(2).strip()
        continue
    nested = re.match(r"^[ \t]+verify:\s*(.+)$", line)
    if in_metadata and nested:
        print(nested.group(1).strip())
        raise SystemExit
if legacy:
    print(legacy)
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
    "$SKILLS_DIR"/*|"$SKILL_STORE"/*) printf '%s\n' "$target" ;;
    *) return 1 ;;
  esac
}

link_skill_target() {
  local root="$1" name="$2" target="$3" existing
  if [ -e "$root/$name" ] || [ -L "$root/$name" ]; then
    existing="$(owned_link_target "$root/$name" 2>/dev/null || true)"
    if [ -z "$existing" ]; then
      echo "error: foreign project skill entry blocks link: $root/$name" >&2
      return 2
    fi
  fi
  ln -sfn "$target" "$root/$name"
}

imported_targets() {
  [ -d "$SKILL_STORE" ] || return 0
  python3 "$SKILL_LIFECYCLE_HELPER" --repo "$REPO" active-targets
}

imported_target_for() {
  local wanted="$1" name target
  while IFS=$'\t' read -r name target; do
    [ "$name" = "$wanted" ] || continue
    printf '%s\n' "$target"
    return 0
  done < <(imported_targets)
  return 1
}

hide_from_git() {
  local name="$1"
  git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || return 0
  "$ROOT/scripts/project-private.sh" --repo "$REPO" apply \
    --path ".agents/skills/$name" --path ".claude/skills/$name" \
    >/dev/null 2>&1 || true
}

cmd_link() {
  local root skill name reason target linked=0 rc=0
  while IFS= read -r root; do
    mkdir -p "$root"
    # Prune links we own whose skill no longer exists or no longer validates.
    for skill in "$root"/*; do
      [ -e "$skill" ] || [ -L "$skill" ] || continue
      target="$(owned_link_target "$skill" 2>/dev/null || true)"
      [ -n "$target" ] || continue
      name="$(basename "$skill")"
      if [ ! -f "$target/SKILL.md" ] ||
         ! reason="$(validate_skill_dir "$target")" ||
         ! scrub_skill_dir "$target" >/dev/null; then
        rm -f "$skill"
        echo "skill-forge: unlinked $name from $root"
      fi
    done
  done < <(link_roots)
  if [ -d "$SKILLS_DIR" ]; then
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
        link_skill_target "$root" "$name" "$skill" || rc=$?
      done < <(link_roots)
      hide_from_git "$name"
      linked=$((linked + 1))
    done
  fi
  if [ -d "$SKILL_STORE" ]; then
    while IFS=$'\t' read -r name target; do
      [ -n "$name" ] && [ -n "$target" ] || continue
      if [ -e "$SKILLS_DIR/$name" ] || [ -L "$SKILLS_DIR/$name" ]; then
        echo "error: imported and local skills share a name: $name" >&2
        rc=2
        continue
      fi
      if ! reason="$(validate_skill_dir "$target")" ||
         ! scrub_skill_dir "$target" >/dev/null; then
        echo "error: imported skill revision is invalid: $name: $reason" >&2
        rc=2
        continue
      fi
      while IFS= read -r root; do
        link_skill_target "$root" "$name" "$target" || rc=$?
      done < <(link_roots)
      hide_from_git "$name"
      linked=$((linked + 1))
    done < <(imported_targets)
  fi
  echo "skill-forge: $linked skill(s) linked"
  return "$rc"
}

skill_lifecycle_apply_unlocked() {
  local output rc=0
  agent_memory_ensure_oms_ignore "$REPO" >/dev/null 2>&1 || true
  output="$(mktemp "${TMPDIR:-/tmp}/oms-skill-lifecycle-output.XXXXXX")"
  python3 "$SKILL_LIFECYCLE_HELPER" --repo "$REPO" "$@" >"$output" || rc=$?
  if [ "$rc" -eq 0 ]; then
    cmd_link >&2 || rc=$?
  fi
  cat "$output"
  rm -f "$output"
  return "$rc"
}

cmd_lifecycle() {
  local command="$1" mutating=0 argument
  shift
  for argument in "$@"; do
    [ "$argument" != --apply ] && [ "$argument" != --record ] || mutating=1
  done
  if [ "$mutating" -eq 1 ]; then
    oms_with_file_lock "$SKILL_STORE/.authority" \
      skill_lifecycle_apply_unlocked "$command" "$@"
  else
    python3 "$SKILL_LIFECYCLE_HELPER" --repo "$REPO" "$command" "$@"
  fi
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
  # Forged skills load into shared namespaces (.claude/skills,
  # .agents/skills) beside user-owned skills; the oms- prefix marks their
  # provenance there. Add-time only: stored unprefixed skills stay valid.
  case "$name" in
    oms-*) : ;;
    *) fail "harness-forged skills carry the oms- prefix: retry as oms-$name (the frontmatter name must match)" ;;
  esac
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
  if ! reason="$(validate_skill_dir "$dir" 1)"; then
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
  local target="${1:-}" skill name reason imported rc=0 checked=0
  [ -z "$target" ] || valid_name "$target" ||
    fail "skill names are lowercase kebab-case: $target"
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
    if sed -n '2,/^---$/p' "$skill/SKILL.md" | grep -q '^verify:'; then
      echo "note: $name: top-level verify is non-portable; declare it under metadata:"
    fi
  done
  while IFS=$'\t' read -r name imported; do
    [ -n "$name" ] && [ -n "$imported" ] || continue
    [ -z "$target" ] || [ "$name" = "$target" ] || continue
    checked=$((checked + 1))
    if ! reason="$(validate_skill_dir "$imported")" ||
       ! scrub_skill_dir "$imported" >/dev/null; then
      echo "invalid: $name: $reason"
      rc=1
      continue
    fi
    echo "ok: $name (imported)"
  done < <(imported_targets)
  [ -n "$target" ] && [ "$checked" -eq 0 ] && fail "no such skill: $target"
  return "$rc"
}

cmd_list() {
  local as_json=0 skill name state imported
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
    while IFS=$'\t' read -r name imported; do
      [ -n "$name" ] && [ -n "$imported" ] || continue
      state=valid
      validate_skill_dir "$imported" >/dev/null 2>&1 &&
        scrub_skill_dir "$imported" >/dev/null 2>&1 || state=invalid
      [ "$first" -eq 1 ] || printf ', '
      first=0
      python3 -c 'import json,sys; print(json.dumps({"name": sys.argv[1], "state": sys.argv[2], "origin": "imported"}, sort_keys=True), end="")' "$name" "$state"
    done < <(imported_targets)
    printf ']}\n'
    return 0
  fi
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
  while IFS=$'\t' read -r name imported; do
    [ -n "$name" ] && [ -n "$imported" ] || continue
    if validate_skill_dir "$imported" >/dev/null 2>&1 &&
       scrub_skill_dir "$imported" >/dev/null 2>&1; then
      echo "valid: $name (imported)"
    else
      echo "invalid: $name (imported)"
    fi
  done < <(imported_targets)
}

cmd_show() {
  local target
  [ "$#" -eq 1 ] || fail "show requires exactly one name"
  valid_name "$1" || fail "skill names are lowercase kebab-case: $1"
  if [ -f "$SKILLS_DIR/$1/SKILL.md" ]; then
    cat "$SKILLS_DIR/$1/SKILL.md"
    return 0
  fi
  target="$(imported_target_for "$1" 2>/dev/null || true)"
  [ -n "$target" ] && [ -f "$target/SKILL.md" ] || fail "no such skill: $1"
  cat "$target/SKILL.md"
}

cmd_remove() {
  [ "$#" -eq 1 ] || fail "remove requires exactly one name"
  local name="$1" root imported
  valid_name "$name" || fail "skill names are lowercase kebab-case: $name"
  imported="$(imported_target_for "$name" 2>/dev/null || true)"
  [ -d "$SKILLS_DIR/$name" ] || [ -n "$imported" ] || fail "no such skill: $name"
  while IFS= read -r root; do
    owned_link_target "$root/$name" >/dev/null && rm -f "$root/$name"
  done < <(link_roots)
  if [ -d "$SKILLS_DIR/$name" ]; then
    rm -rf "${SKILLS_DIR:?}/$name"
  else
    rm -f "$SKILL_STORE/$name/lock.json"
  fi
  echo "skill-forge: removed $name"
}

skill_is_stale() {
  local file="$1" days="$2"
  OMS_SF_FILE="$file" OMS_SF_STALE_DAYS="$days" python3 - <<'PY'
import os
import time

path = os.environ["OMS_SF_FILE"]
days = int(os.environ["OMS_SF_STALE_DAYS"])
raise SystemExit(0 if time.time() - os.path.getmtime(path) > days * 86400 else 1)
PY
}

cmd_status() {
  local total=0 invalid=0 contracts=0 stale=0 skill stale_days name imported
  stale_days="${OMS_SKILL_STALE_DAYS:-90}"
  case "$stale_days" in
    *[!0-9]*|'') fail "OMS_SKILL_STALE_DAYS must be a non-negative integer" ;;
  esac
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    total=$((total + 1))
    if validate_skill_dir "$skill" >/dev/null 2>&1 &&
       scrub_skill_dir "$skill" >/dev/null 2>&1; then
      if [ "$stale_days" -gt 0 ] && skill_is_stale "$skill/SKILL.md" "$stale_days"; then
        stale=$((stale + 1))
      fi
    else
      invalid=$((invalid + 1))
    fi
    [ -z "$(skill_verify_contract "$skill")" ] || contracts=$((contracts + 1))
  done
  while IFS=$'\t' read -r name imported; do
    [ -n "$name" ] && [ -n "$imported" ] || continue
    total=$((total + 1))
    if ! validate_skill_dir "$imported" >/dev/null 2>&1 ||
       ! scrub_skill_dir "$imported" >/dev/null 2>&1; then
      invalid=$((invalid + 1))
    fi
    [ -z "$(skill_verify_contract "$imported")" ] || contracts=$((contracts + 1))
  done < <(imported_targets)
  if [ "$total" -eq 0 ]; then
    echo "skill-forge: no project skills"
    return 0
  fi
  if [ "$invalid" -gt 0 ]; then
    echo "skill-forge: $invalid of $total project skill(s) invalid (run 'oms skill-forge validate')"
    return 1
  fi
  if [ "$contracts" -gt 0 ]; then
    echo "skill-forge: $total project skill(s) valid, $contracts with a verify contract"
  else
    echo "skill-forge: $total project skill(s) valid"
  fi
  if [ "$stale" -gt 0 ]; then
    echo "skill-forge: $stale project skill(s) untouched >${stale_days}d — review with oms skill-forge list"
  fi
}

# One row per declared verify contract; consumed by agent-task close to
# remind — never to execute.
cmd_contracts() {
  local skill contract name imported
  for skill in "$SKILLS_DIR"/*; do
    [ -d "$skill" ] || continue
    contract="$(skill_verify_contract "$skill")"
    [ -z "$contract" ] || printf '%s\t%s\n' "$(basename "$skill")" "$contract"
  done
  while IFS=$'\t' read -r name imported; do
    [ -n "$name" ] && [ -n "$imported" ] || continue
    contract="$(skill_verify_contract "$imported")"
    [ -z "$contract" ] || printf '%s\t%s\n' "$name" "$contract"
  done < <(imported_targets)
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
  eval|preview|import|update|rollback|derive)
    command="$1"; shift; cmd_lifecycle "$command" "$@"
    ;;
  ""|help) usage ;;
  *) fail "unknown command: $1" ;;
esac
