#!/usr/bin/env bash
# shellcheck disable=SC2034 # sourced callers consume OMS_OPS_FAILED
# shellcheck shell=bash

# Shared checks and cleanup for installed agent links/skills.
if [ -z "${ROOT:-}" ]; then
  case "${BASH_SOURCE[0]}" in
    */*) ROOT="$(cd "${BASH_SOURCE[0]%/*}/../.." && pwd -P)" ;;
    *) ROOT="$(cd ../.. && pwd -P)" ;;
  esac
fi

# Exported state read by thin wrapper scripts.
# shellcheck disable=SC2034
OMS_OPS_REMOVED=0
# shellcheck disable=SC2034
OMS_OPS_WOULD_REMOVE=0
# shellcheck disable=SC2034
OMS_OPS_FAILED=0

oms_ops_reset_cleanup_counters() {
  OMS_OPS_REMOVED=0
  OMS_OPS_WOULD_REMOVE=0
}

oms_ops_reset_check_state() {
  OMS_OPS_FAILED=0
}

oms_ops_legacy_source_matches() {
  local current="$1"
  local rel="$2"
  local old_root="$HOME/.oh-my-setting"

  [ "$current" = "$ROOT/$rel" ] || [ "$current" = "$old_root/$rel" ]
}

oms_ops_remove_symlink() {
  local target="$1"
  local reason="$2"
  local dry_run="${3:-0}"

  [ -L "$target" ] || return 0
  if [ "$dry_run" = "1" ]; then
    printf 'would remove: %s (%s)\n' "$target" "$reason"
    OMS_OPS_WOULD_REMOVE=$((OMS_OPS_WOULD_REMOVE + 1))
  else
    rm -f "$target"
    printf 'removed: %s (%s)\n' "$target" "$reason"
    OMS_OPS_REMOVED=$((OMS_OPS_REMOVED + 1))
  fi
}

oms_ops_remove_legacy_link() {
  local target="$1"
  local rel="$2"
  local dry_run="${3:-0}"
  local current

  [ -L "$target" ] || return 0
  current="$(readlink "$target")"
  if oms_ops_legacy_source_matches "$current" "$rel"; then
    oms_ops_remove_symlink "$target" "legacy oh-my-setting link" "$dry_run"
  fi
}

oms_ops_latest_backup() {
  local target="$1"
  local dir
  local base

  while [ "$target" != / ] && [ "${target%/}" != "$target" ]; do
    target="${target%/}"
  done
  dir="${target%/*}"
  [ "$dir" != "$target" ] || dir=.
  [ -n "$dir" ] || dir=/
  if [ "$target" = / ]; then base=/; else base="${target##*/}"; fi
  # -H: a dotfiles-managed parent (~/.codex -> elsewhere) is still searched.
  find -H "$dir" -maxdepth 1 -name "$base.backup.*" -print 2>/dev/null |
    LC_ALL=C sort |
    tail -n 1
}

oms_ops_migrate_legacy_workflow_link() {
  local target="$HOME/.oh-my-setting-workflows"
  local dry_run="${1:-0}"
  local current
  local backup

  [ -L "$target" ] || return 0
  current="$(readlink "$target")"
  oms_ops_legacy_source_matches "$current" "workflows" || return 0
  backup="$(oms_ops_latest_backup "$target")"

  if [ "$dry_run" = "1" ]; then
    if [ -n "$backup" ]; then
      printf 'would remove %s and restore %s\n' "$target" "$backup"
    else
      printf 'would remove %s\n' "$target"
    fi
    OMS_OPS_WOULD_REMOVE=$((OMS_OPS_WOULD_REMOVE + 1))
    return 0
  fi

  rm -f "$target"
  printf 'removed: %s (legacy oh-my-setting workflows link)\n' "$target"
  OMS_OPS_REMOVED=$((OMS_OPS_REMOVED + 1))
  if [ -n "$backup" ]; then
    mv "$backup" "$target"
    printf 'restored: %s from %s\n' "$target" "$backup"
  fi
}

# True when the symlink's target lies inside this checkout or the legacy
# ~/.oh-my-setting root. The target is resolved physically (cd -P, so a ".."
# after a symlinked directory follows the kernel); a dangling target is foreign.
oms_ops_link_into_install() {
  local link="$1"
  local dest dir base root phys hops=0

  # The sentinel keeps trailing newlines that command substitution would strip;
  # readlink's own terminating newline is the only one removed.
  dest="$(readlink "$link" && printf x)" || return 1
  dest="${dest%x}"
  dest="${dest%$'\n'}"
  case "$dest" in
    /*) ;;
    *) dest="${link%/*}/$dest" ;;
  esac
  # Resolve every existing final-component symlink before checking ownership.
  # A dangling, looping, or otherwise unresolvable target is conservatively
  # treated as foreign so cleanup cannot remove a user's backup link.
  while [ -L "$dest" ]; do
    hops=$((hops + 1))
    [ "$hops" -le 40 ] || return 1
    dir="${dest%/*}"
    base="$(readlink "$dest" && printf x)" || return 1
    base="${base%x}"
    base="${base%$'\n'}"
    case "$base" in
      /*) dest="$base" ;;
      *) dest="$dir/$base" ;;
    esac
  done
  if [ -d "$dest" ]; then
    dest="$(cd -P "$dest" 2>/dev/null && pwd -P && printf x)" || return 1
    dest="${dest%x}"
    dest="${dest%$'\n'}"
  elif [ -e "$dest" ]; then
    dir="${dest%/*}"
    base="${dest##*/}"
    phys="$(cd -P "$dir" 2>/dev/null && pwd -P && printf x)" || return 1
    phys="${phys%x}"
    phys="${phys%$'\n'}"
    dest="$phys/$base"
  else
    return 1
  fi
  for root in "$ROOT" "$HOME/.oh-my-setting"; do
    phys="$(cd -P "$root" 2>/dev/null && pwd -P && printf x)" || phys="${root}x"
    phys="${phys%x}"
    phys="${phys%$'\n'}"
    case "$dest" in
      "$root"|"$root"/*|"$phys"|"$phys"/*) return 0 ;;
    esac
  done
  return 1
}

oms_ops_clean_backup_skill_links() {
  local target_root="$1"
  local dry_run="${2:-0}"
  local target

  [ -d "$target_root" ] || return 0
  while IFS= read -r -d '' target; do
    case "$target" in
      *.backup.[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9])
        continue
        ;;
    esac
    if oms_ops_link_into_install "$target"; then
      oms_ops_remove_symlink "$target" "backup skill symlink" "$dry_run"
    else
      printf 'kept: %s (backup skill symlink not owned by this install)\n' "$target"
    fi
  done < <(find "$target_root" -maxdepth 1 -type l -name '*.backup.*' -print0 2>/dev/null)
}

# Global skills shipped before the `oms-` namespace, as `old:new` rows. Skills
# install into roots shared with the user's own and other tools' skills, so an
# unprefixed name is a collision waiting to happen; the prefix is the fix and
# this table is what makes the transition legible. One table, two readers: the
# cleanup below removes links a previous install left under an old name, and
# skill-doctor reports whatever it could not remove.
oms_ops_legacy_skill_renames() {
  printf '%s\n' \
    'agent-harness:oms-agent-harness' \
    'oh-my-setting-ops:oms-ops' \
    'spec-interview:oms-spec-interview' \
    'trace:oms-trace' \
    'trust-boundary:oms-trust-boundary' \
    'gpu-workstation:oms-gpu-workstation' \
    'slurm:oms-slurm'
}

oms_ops_clean_legacy_skill_links() {
  local target_root="$1"
  local dry_run="${2:-0}"
  local skill
  local name
  local row

  [ -d "$target_root" ] || return 0
  oms_ops_clean_backup_skill_links "$target_root" "$dry_run"

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="${skill##*/}"
    oms_ops_remove_legacy_link "$target_root/$name" "custom-skills/$name" "$dry_run"
  done

  # A renamed skill's old link is invisible to the loop above, which only ever
  # sees current names, so these roots would keep it forever.
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    name="${row%%:*}"
    oms_ops_remove_legacy_link "$target_root/$name" "custom-skills/$name" "$dry_run"
  done <<< "$(oms_ops_legacy_skill_renames)"

  # These front doors were folded into oms-agent-harness. Keep their runtime
  # commands, but remove only legacy links owned by this checkout.
  for name in peer-ask peer-delegate peer-review; do
    oms_ops_remove_legacy_link "$target_root/$name" "custom-skills/$name" "$dry_run"
  done
}

oms_ops_cleanup_legacy_links() {
  local dry_run="${1:-0}"
  local pi_agent_dir="${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}"

  oms_ops_reset_cleanup_counters
  oms_ops_clean_backup_skill_links "$HOME/.codex/skills" "$dry_run"
  oms_ops_clean_backup_skill_links "$HOME/.claude/skills" "$dry_run"
  oms_ops_clean_backup_skill_links "$HOME/.gemini/antigravity/skills" "$dry_run"
  oms_ops_remove_legacy_link "$HOME/.codex/skills/oh-my-setting" "custom-skills" "$dry_run"
  oms_ops_remove_legacy_link "$HOME/.claude/skills/oh-my-setting" "custom-skills" "$dry_run"
  oms_ops_remove_legacy_link "$HOME/.gemini/GEMINI.md" "AGENTS.md" "$dry_run"
  oms_ops_remove_legacy_link "$pi_agent_dir/AGENTS.md" "AGENTS.md" "$dry_run"
  oms_ops_migrate_legacy_workflow_link "$dry_run"
  oms_ops_clean_legacy_skill_links "$HOME/.agents/skills" "$dry_run"
  oms_ops_clean_legacy_skill_links "$pi_agent_dir/skills" "$dry_run"
}

oms_ops_extract_skill_name() {
  local file="$1"
  awk -F: '
    /^name:[[:space:]]*/ {
      sub(/^[[:space:]]+/, "", $2)
      sub(/[[:space:]]+$/, "", $2)
      print $2
      exit
    }
  ' "$file"
}

# The custom-skills sources linking installs: enabled, and every `requires`
# command present, so a Slurm skill never reaches a workstation. Linking and
# the loaded-skill check share this selection.
oms_ops_linkable_skill_sources() {
  local sources
  sources="$(python3 - "$ROOT/skills.manifest.json" <<'PY'
import json
import shutil
import sys
for skill in json.load(open(sys.argv[1], encoding="utf-8")).get("skills", []):
    source = str(skill.get("source", ""))
    if skill.get("enabled") is not True or not source.startswith("custom-skills/"):
        continue
    requires = skill.get("requires") or []
    if any(shutil.which(str(req)) is None for req in requires):
        continue
    print(source)
PY
)" || return 1
  # Windows Python writes CRLF.
  printf '%s\n' "${sources//$'\r'/}"
}

# agy can list the skills it actually loaded without a model turn. A root that
# exists on disk can still load nothing (agy moved its root once already), so
# compare what agy reports with the enabled oms-* manifest entries. Read-only;
# any failure to ask agy skips rather than fails, since CI and the portability
# lanes have no provider CLI.
oms_ops_check_agy_loaded_skills() {
  local wanted=()
  local source
  local bounder=""
  local agy_out
  local parsed
  local checked=0
  local missing=0
  local row

  printf '## Antigravity loaded skills\n\n'

  if ! command -v agy >/dev/null 2>&1; then
    printf 'skip: agy is not on PATH; loaded-skill check skipped\n\n'
    return 0
  fi
  if command -v timeout >/dev/null 2>&1; then
    bounder=timeout
  elif command -v gtimeout >/dev/null 2>&1; then
    bounder=gtimeout
  fi
  if [ -z "$bounder" ] || ! command -v python3 >/dev/null 2>&1; then
    printf 'skip: no timeout or python3 to bound agy /skills; loaded-skill check skipped\n\n'
    return 0
  fi

  while IFS= read -r source; do
    case "${source##*/}" in oms-*) wanted+=("${source##*/}") ;; esac
  done <<< "$(oms_ops_linkable_skill_sources)"
  if [ "${#wanted[@]}" -eq 0 ]; then
    printf 'skip: no installable oms-* skills in the manifest; loaded-skill check skipped\n\n'
    return 0
  fi

  # agy blocks on login when it has no credentials, so the wall bounds the probe.
  agy_out="$("$bounder" -k 5 20 agy -p "/skills" --output-format json </dev/null 2>/dev/null)" ||
    { printf 'skip: agy /skills did not finish cleanly; loaded-skill check skipped\n\n'; return 0; }

  parsed="$(printf '%s' "$agy_out" | python3 -I -c '
import json
import sys

try:
    loaded = {s["name"] for s in json.load(sys.stdin)["command"]["data"]["skills"]}
except (ValueError, KeyError, TypeError):
    sys.exit(3)
want = sys.argv[1:]
print("checked %d" % len(want))
for name in want:
    if name not in loaded:
        print("missing " + name)
' "${wanted[@]}" 2>/dev/null)" ||
    { printf 'skip: agy /skills output is not the expected JSON; loaded-skill check skipped\n\n'; return 0; }
  parsed="${parsed//$'\r'/}"

  while IFS= read -r row; do
    case "$row" in
      "checked "*) checked="${row#checked }" ;;
      "missing "*)
        printf 'fail: agy did not load enabled skill %s\n' "${row#missing }"
        missing=$((missing + 1))
        OMS_OPS_FAILED=1
        ;;
    esac
  done <<< "$parsed"
  if [ "$missing" -eq 0 ]; then
    printf 'ok: agy loaded all %s enabled oms-* skills\n' "$checked"
  fi
  printf '\n'
}

oms_ops_check_skill_root() {
  local label="$1"
  local root="$2"
  local file
  local name
  local total=0
  local issues=0
  local backup
  local prev
  # Newline-delimited "name<TAB>path" records: bash 3.2 (macOS) has no
  # associative arrays, and `declare -A` aborts the whole check under set -e.
  local first_seen=""

  printf '## %s\n\n' "$label"

  if [ ! -d "$root" ]; then
    printf 'skip: %s missing\n\n' "$root"
    return 0
  fi

  while IFS= read -r -d '' backup; do
    # A backup link outside this install is the user's; cleanup keeps it.
    oms_ops_link_into_install "$backup" || continue
    printf 'stale backup skill link: %s\n' "$backup"
    OMS_OPS_FAILED=1
    issues=1
  done < <(find "$root" -maxdepth 1 -type l -name '*.backup.*' -print0 2>/dev/null)

  while IFS= read -r -d '' file; do
    name="$(oms_ops_extract_skill_name "$file")"
    if [ -z "$name" ]; then
      printf 'missing skill name: %s\n' "$file"
      OMS_OPS_FAILED=1
      issues=1
      continue
    fi

    total=$((total + 1))
    prev="$(printf '%s' "$first_seen" | awk -F'\t' -v n="$name" '$1 == n { print $2; exit }')"
    if [ -n "$prev" ]; then
      printf 'duplicate skill name: %s\n' "$name"
      printf '  first: %s\n' "$prev"
      printf '  also:  %s\n' "$file"
      OMS_OPS_FAILED=1
      issues=1
    else
      first_seen="${first_seen}${name}$(printf '\t')${file}
"
    fi
  done < <(find -L "$root" -mindepth 2 -maxdepth 2 -name SKILL.md -type f -print0 2>/dev/null)

  if [ "$issues" -eq 0 ]; then
    printf 'ok: %s (%s skills, no duplicates)\n' "$root" "$total"
  fi
  printf '\n'
}

# Codex loads both its product-specific root and the shared ~/.agents root.
# A per-directory check can therefore report two clean roots while the model
# still receives two skills with the same name. Check that effective overlay
# without treating the expected copies across different agent products as
# duplicates.
oms_ops_check_skill_overlay() {
  local label="$1"
  shift
  local root
  local file
  local name
  local previous
  local previous_file
  local previous_root
  local roots=0
  local issues=0
  local first_seen=""

  printf '## %s\n\n' "$label"

  for root in "$@"; do
    [ -d "$root" ] || continue
    roots=$((roots + 1))
    while IFS= read -r -d '' file; do
      name="$(oms_ops_extract_skill_name "$file")"
      [ -n "$name" ] || continue
      previous="$(printf '%s' "$first_seen" |
        awk -F'\t' -v n="$name" '$1 == n { print $2 "\t" $3; exit }')"
      if [ -n "$previous" ]; then
        previous_file="${previous%%$'\t'*}"
        previous_root="${previous#*$'\t'}"
        if [ "$previous_root" != "$root" ]; then
          printf 'duplicate skill name: %s\n' "$name"
          printf '  first: %s\n' "$previous_file"
          printf '  also:  %s\n' "$file"
          OMS_OPS_FAILED=1
          issues=1
        fi
      else
        first_seen="${first_seen}${name}$(printf '\t')${file}$(printf '\t')${root}
"
      fi
    done < <(find -L "$root" -mindepth 2 -maxdepth 2 -name SKILL.md -type f -print0 2>/dev/null)
  done

  if [ "$roots" -lt 2 ]; then
    printf 'skip: fewer than two skill roots are present\n'
  elif [ "$issues" -eq 0 ]; then
    printf 'ok: no duplicate names across the effective overlay\n'
  fi
  printf '\n'
}
