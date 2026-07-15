#!/usr/bin/env bash
set -euo pipefail

# Verify the install: symlink identity, tools, skills, and manifest sync for all three agent CLIs.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
FAILED=0
REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"
REPAIR=0
ORIGINAL_ARGS=("$@")

# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT/scripts/lib/harness-residue.sh"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

usage() {
  cat <<'EOF'
Usage: doctor.sh [--repair] [-h|--help]

Verify the canonical install. --repair relinks from the receipt owner, or
from this checkout for a legacy install without a receipt. An invalid or
unavailable receipt owner is never replaced automatically.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repair) REPAIR=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

RECEIPT="$(oms_install_receipt_path)"
RECEIPT_STATE="missing"
INSTALL_ROOT="$ROOT"
if [ -f "$RECEIPT" ]; then
  if INSTALL_ROOT="$(oms_install_receipt_owner "$RECEIPT")"; then
    RECEIPT_STATE="valid"
  else
    RECEIPT_STATE="invalid"
    INSTALL_ROOT="$ROOT"
  fi
fi

if [ "$RECEIPT_STATE" = "valid" ] && [ "$ROOT" != "$INSTALL_ROOT" ] &&
   [ -x "$INSTALL_ROOT/scripts/doctor.sh" ]; then
  echo "delegating doctor to canonical owner: $INSTALL_ROOT"
  exec "$INSTALL_ROOT/scripts/doctor.sh" "${ORIGINAL_ARGS[@]}"
fi

codex_plugin_installed() {
  command -v codex >/dev/null 2>&1 &&
    codex plugin list --json 2>/dev/null |
      python3 -c 'import json,sys; d=json.load(sys.stdin); target="oh-my-setting@oh-my-setting-local"; sys.exit(0 if any(p.get("pluginId")==target and p.get("installed") for p in d.get("installed", [])) else 1)' 2>/dev/null
}

repair_install() {
  local repair_root="$INSTALL_ROOT"
  local plugin_mode="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"

  if [ "$RECEIPT_STATE" = "invalid" ]; then
    echo "error: refusing repair with invalid install receipt: $RECEIPT" >&2
    echo "hint: choose the intended checkout and run its scripts/link.sh" >&2
    return 1
  fi
  if [ "$RECEIPT_STATE" = "valid" ] &&
     { [ ! -d "$repair_root" ] || [ ! -x "$repair_root/scripts/link.sh" ]; }; then
    echo "error: refusing repair; receipt owner is unavailable: $repair_root" >&2
    echo "hint: restore that checkout or run scripts/link.sh from the intended owner" >&2
    return 1
  fi
  echo "repairing canonical links from: $repair_root"
  "$repair_root/scripts/link.sh"
  if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ] &&
     [ -x "$repair_root/scripts/install-claude-hooks.sh" ]; then
    "$repair_root/scripts/install-claude-hooks.sh"
  fi
  if [ -x "$repair_root/scripts/install-codex-plugin.sh" ] &&
     { [ "$plugin_mode" = "1" ] ||
       { [ "$plugin_mode" = "auto" ] && codex_plugin_installed; }; }; then
    "$repair_root/scripts/install-codex-plugin.sh"
  fi
}

if [ "$REPAIR" = "1" ]; then
  repair_install
  exec "$ROOT/scripts/doctor.sh"
fi

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

check_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: command $1"
  elif [ "$REQUIRE_TOOLS" = "1" ]; then
    echo "missing: command $1"
    FAILED=1
  else
    echo "optional missing: command $1"
  fi
}

check_optional_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: optional command $1"
  else
    echo "optional missing: command $1"
  fi
}

check_bash_version() {
  local major="${BASH_VERSINFO[0]:-0}"
  if [ "$major" -lt 3 ]; then
    echo "unsupported: bash 3.2+ required; current bash is ${BASH_VERSION:-unknown}"
    FAILED=1
  else
    echo "ok: bash ${BASH_VERSION:-unknown}"
  fi
}

check_path() {
  # Optional $2: the exact symlink target this install expects. Existence
  # alone is not parity — a link resolving to a foreign/stale file means this
  # agent runs different rules while doctor would report ok.
  # -L before -e: a dangling symlink "exists" as a link but resolves to
  # nothing — exactly the breakage that silently strips an agent's rules.
  if [ -L "$1" ] && [ ! -e "$1" ]; then
    echo "broken link: $1 -> $(readlink "$1")"
    FAILED=1
  elif [ -L "$1" ] && [ -n "${2:-}" ] && [ "$(readlink "$1")" != "$2" ]; then
    echo "linked elsewhere: $1 -> $(readlink "$1") (expected $2)"
    FAILED=1
  elif [ ! -L "$1" ] && [ -e "$1" ] && [ -n "${2:-}" ]; then
    echo "not a symlink: $1 (expected link to $2)"
    FAILED=1
  elif [ -e "$1" ]; then
    echo "ok: $1"
  else
    echo "missing: $1"
    FAILED=1
  fi
}

check_custom_skills() {
  local target_root="$1"
  local skill
  local name
  local source

  while IFS= read -r source; do
    [ -n "$source" ] || continue
    skill="$INSTALL_ROOT/$source"
    name="$(basename "$skill")"
    if [ -L "$target_root/$name" ]; then
      # Symlink install: certify the link points at THIS checkout's skill,
      # not a shadowed copy from another install.
      check_path "$target_root/$name" "$skill"
    elif [ -d "$target_root/$name" ]; then
      # Copy fallback must preserve nested references and agent metadata too;
      # checking SKILL.md alone can certify a partially installed skill.
      if diff -qr "$skill" "$target_root/$name" >/dev/null 2>&1; then
        echo "ok: $target_root/$name (copy parity)"
      else
        echo "copy differs: $target_root/$name (expected resource parity with $skill)"
        FAILED=1
      fi
    else
      check_path "$target_root/$name/SKILL.md"
    fi
  done < <(python3 - "$INSTALL_ROOT/skills.manifest.json" <<'PY'
import json
import sys
for skill in json.load(open(sys.argv[1], encoding="utf-8")).get("skills", []):
    source = str(skill.get("source", ""))
    if skill.get("enabled") is True and source.startswith("custom-skills/"):
        print(source)
PY
)
}

check_install_receipt() {
  local recorded_commit
  local current_commit
  local recorded_plugin_hash
  local current_plugin_hash

  printf '\n# install ownership\n'
  case "$RECEIPT_STATE" in
    missing)
      echo "note: legacy install has no receipt; expecting this checkout: $ROOT"
      echo "hint: run scripts/link.sh to record canonical ownership"
      ;;
    invalid)
      echo "invalid install receipt: $RECEIPT"
      FAILED=1
      ;;
    valid)
      echo "ok: install receipt $RECEIPT"
      echo "canonical root: $INSTALL_ROOT"
      if [ "$INSTALL_ROOT" != "$ROOT" ]; then
        echo "note: current checkout is not canonical: $ROOT"
      fi
      if [ ! -d "$INSTALL_ROOT" ]; then
        echo "missing: canonical install root $INSTALL_ROOT"
        FAILED=1
        return 0
      fi
      recorded_commit="$(oms_install_receipt_field commit "$RECEIPT" 2>/dev/null || true)"
      current_commit="$(git -C "$INSTALL_ROOT" rev-parse HEAD 2>/dev/null || true)"
      if [ -n "$recorded_commit" ] && [ -n "$current_commit" ] &&
         [ "$recorded_commit" != "$current_commit" ]; then
        echo "stale install receipt commit: $recorded_commit (source is $current_commit)"
        FAILED=1
      else
        echo "ok: install receipt commit"
      fi
      recorded_plugin_hash="$(oms_install_receipt_field plugin.sha256 "$RECEIPT" 2>/dev/null || true)"
      current_plugin_hash="$(oms_install_plugin_hash "$INSTALL_ROOT")"
      if [ -n "$recorded_plugin_hash" ] && [ "$recorded_plugin_hash" != "$current_plugin_hash" ]; then
        echo "stale install receipt plugin hash"
        FAILED=1
      else
        echo "ok: install receipt plugin hash"
      fi
      ;;
  esac
}

check_snapshots() {
  local machine_mode=0
  local slurm_mode=0
  local machine_path="${OH_MY_SETTING_MACHINE_SNAPSHOT:-$INSTALL_ROOT/local/machine.md}"
  local slurm_path="${OH_MY_SETTING_SLURM_REF:-$INSTALL_ROOT/custom-skills/slurm-hpc/references/cluster.generated.md}"

  [ "$RECEIPT_STATE" = valid ] || return 0
  machine_mode="$(oms_install_receipt_mode machine_snapshot 0 "$RECEIPT")"
  slurm_mode="$(oms_install_receipt_mode slurm_snapshot 0 "$RECEIPT")"
  printf '\n# snapshots\n'
  echo "machine snapshot mode: $machine_mode"
  echo "Slurm snapshot mode: $slurm_mode"

  case "$machine_mode" in
    1|auto)
      if OH_MY_SETTING_MACHINE_SNAPSHOT="$machine_path" \
        "$INSTALL_ROOT/scripts/write-machine-snapshot.sh" --check >/dev/null 2>&1; then
        echo "ok: machine snapshot"
      else
        echo "invalid or missing machine snapshot: $machine_path"
        FAILED=1
      fi
      ;;
  esac
  case "$slurm_mode" in
    1)
      if OH_MY_SETTING_SLURM_REF="$slurm_path" \
        "$INSTALL_ROOT/scripts/generate-slurm-skill.sh" --check >/dev/null 2>&1; then
        echo "ok: Slurm snapshot"
      else
        echo "invalid or missing Slurm snapshot: $slurm_path"
        FAILED=1
      fi
      ;;
    auto)
      if command -v sinfo >/dev/null 2>&1; then
        if OH_MY_SETTING_SLURM_REF="$slurm_path" \
          "$INSTALL_ROOT/scripts/generate-slurm-skill.sh" --check >/dev/null 2>&1; then
          echo "ok: Slurm snapshot"
        else
          echo "invalid or missing Slurm snapshot while Slurm is available: $slurm_path"
          FAILED=1
        fi
      elif [ -f "$slurm_path" ]; then
        echo "note: retained Slurm snapshot; current host has no sinfo"
      else
        echo "ok: Slurm auto snapshot not applicable on this host"
      fi
      ;;
  esac
}

check_codex_plugin() {
  local mode="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"
  local plugin_version
  local marketplace_name
  local cache
  local expected_hash
  local marker_hash=""
  local marker_root=""
  local actual_hash

  if [ "$mode" = "0" ]; then
    echo "note: codex plugin check disabled (OH_MY_SETTING_CODEX_PLUGIN=0)"
    return 0
  fi
  case "$mode" in
    1|auto) ;;
    *)
      echo "fail: OH_MY_SETTING_CODEX_PLUGIN must be 0, 1, or auto"
      FAILED=1
      return 0
      ;;
  esac
  command -v codex >/dev/null 2>&1 || return 0
  if ! codex_plugin_installed; then
    if [ "$mode" = "auto" ]; then
      echo "note: optional codex plugin not installed"
      return 0
    fi
    echo "fail: expected codex plugin not installed: oh-my-setting@oh-my-setting-local"
    echo "hint: run $INSTALL_ROOT/scripts/install-codex-plugin.sh"
    FAILED=1
    return 0
  fi

  plugin_version="$(oms_install_plugin_version "$INSTALL_ROOT")"
  marketplace_name="$(python3 - "$INSTALL_ROOT/.agents/plugins/marketplace.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh)["name"])
PY
)"
  cache="${CODEX_HOME:-$HOME/.codex}/plugins/cache/$marketplace_name/oh-my-setting/$plugin_version"
  expected_hash="$(oms_install_receipt_field plugin.sha256 "$RECEIPT" 2>/dev/null || oms_install_plugin_hash "$INSTALL_ROOT")"

  if [ ! -d "$cache" ]; then
    echo "fail: codex plugin cache missing: $cache"
    FAILED=1
    return 0
  fi
  [ ! -f "$cache/.oh-my-setting-source-sha256" ] ||
    marker_hash="$(sed -n '1p' "$cache/.oh-my-setting-source-sha256")"
  [ ! -f "$cache/.oh-my-setting-source-root" ] ||
    marker_root="$(sed -n '1p' "$cache/.oh-my-setting-source-root")"
  actual_hash="$(oms_install_tree_hash "$cache")"
  if [ "$marker_root" != "$INSTALL_ROOT" ] ||
     [ "$marker_hash" != "$expected_hash" ] ||
     [ "$actual_hash" != "$expected_hash" ]; then
    echo "fail: stale codex plugin cache: $cache"
    echo "hint: run $INSTALL_ROOT/scripts/install-codex-plugin.sh"
    FAILED=1
  else
    echo "ok: codex plugin oh-my-setting (cache parity)"
  fi
}

harness_relpath() {
  local project_dir="$1"
  local path="$2"

  case "$path" in
    "$project_dir"/*) printf '%s\n' "${path#"$project_dir"/}" ;;
    *) printf '%s\n' "$path" ;;
  esac
}

check_harness_artifact_index() {
  local project_dir="$1"
  local index="$project_dir/.oms/artifacts/index.jsonl"
  local stats
  local bad
  local stale
  local schema1
  local canonical_out

  if [ ! -f "$index" ]; then
    echo "ok: artifact index absent"
    return 0
  fi

  command -v python3 >/dev/null 2>&1 || return 0
  stats="$(python3 - "$project_dir" "$index" <<'PY'
import json
import os
import sys

repo, index = sys.argv[1:]
bad = 0
stale = 0

with open(index, "r", encoding="utf-8") as f:
    for line in f:
        try:
            row = json.loads(line)
        except Exception:
            bad += 1
            continue
        if not isinstance(row, dict):
            continue
        for key in ("artifact", "patch", "source"):
            value = row.get(key)
            if not isinstance(value, str) or not value:
                continue
            path = value if os.path.isabs(value) else os.path.join(repo, value)
            if not os.path.exists(path):
                stale += 1

print(f"{bad} {stale}")
PY
)" || {
    echo "warn: artifact index audit failed"
    return 0
  }

  read -r bad stale <<< "$stats"
  if [ "${bad:-0}" -gt 0 ]; then
    echo "warn: artifact index has $bad invalid JSON line(s)"
  else
    echo "ok: artifact index JSONL"
  fi

  if [ "${stale:-0}" -gt 0 ]; then
    echo "warn: artifact index has $stale stale artifact/patch reference(s)"
  else
    echo "ok: artifact index references"
  fi

  schema1="$(python3 - "$index" <<'PY'
import json, sys
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        row = json.loads(line)
    except Exception:
        continue
    if isinstance(row, dict) and row.get("schema") == 1:
        print(1)
        break
PY
)"
  if [ "$schema1" = "1" ]; then
    if canonical_out="$("$ROOT/scripts/artifact-index.sh" --repo "$project_dir" validate 2>&1)"; then
      echo "ok: artifact index canonical validation"
    else
      echo "warn: artifact index canonical validation failed"
      printf '%s\n' "$canonical_out" | sed -n '1,5p' | sed 's/^/  /'
    fi
  fi
}

check_harness_run_state() {
  local project_dir="$1"
  local oms_dir="$project_dir/.oms"
  local bad

  command -v python3 >/dev/null 2>&1 || return 0
  bad="$(python3 - "$oms_dir" <<'PY'
import glob, json, os, sys
oms = sys.argv[1]
# Run-tool JSONL state written this family of tools (spine, experiments,
# reconcile, capsule/run index). Manifests are *.json (single object).
targets = []
targets += glob.glob(os.path.join(oms, "runs", "*.jsonl"))
targets += glob.glob(os.path.join(oms, "experiments.jsonl"))
bad = 0
for f in targets:
    try:
        with open(f, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if line.strip():
                    json.loads(line)
    except Exception:
        bad += 1
print(bad)
PY
)" || { echo "warn: run-state audit failed"; return 0; }
  if [ "${bad:-0}" -gt 0 ]; then
    echo "warn: $bad run-state JSONL file(s) have malformed lines (run oms-run.sh validate)"
  else
    echo "ok: run-state JSONL"
  fi
}

check_harness_sensitive_files() {
  local project_dir="$1"
  local oms_dir="$project_dir/.oms"
  local file
  local rel
  local sensitive=0

  file="$oms_dir/task/current.md"
  if [ -f "$file" ] && agent_memory_file_has_sensitive_content "$file"; then
    rel="$(harness_relpath "$project_dir" "$file")"
    echo "warn: sensitive-looking harness state: $rel"
    sensitive=$((sensitive + 1))
  fi

  if [ -d "$oms_dir/memory" ]; then
    while IFS= read -r -d '' file; do
      if agent_memory_file_has_sensitive_content "$file"; then
        rel="$(harness_relpath "$project_dir" "$file")"
        echo "warn: sensitive-looking harness state: $rel"
        sensitive=$((sensitive + 1))
      fi
    done < <(find "$oms_dir/memory" -maxdepth 1 -type f -print0 2>/dev/null)
  fi

  if [ "$sensitive" -eq 0 ]; then
    echo "ok: harness task/memory sensitive scan"
  fi
}

check_harness_residue() {
  local project_dir="$1"
  local stale_worktrees=0
  local dead_locks=0
  local temp_dirs=0
  local unindexed=0
  local residue=0

  if git -C "$project_dir" rev-parse --git-dir >/dev/null 2>&1; then
    stale_worktrees="$(oms_harness_count_stale_worktrees "$project_dir")"
  fi
  dead_locks="$(oms_harness_lock_residue_count)"
  temp_dirs="$(oms_harness_tmp_residue_count)"
  unindexed="$(oms_harness_count_unindexed_artifacts "$project_dir")"

  if [ "${stale_worktrees:-0}" -gt 0 ]; then
    echo "warn: $stale_worktrees stale git worktree registration(s)"
    residue=1
  fi
  if [ "${dead_locks:-0}" -gt 0 ]; then
    echo "warn: $dead_locks dead harness lock dir(s)"
    residue=1
  fi
  if [ "${temp_dirs:-0}" -gt 0 ]; then
    echo "warn: $temp_dirs dead harness temp dir(s)"
    residue=1
  fi
  if [ "${unindexed:-0}" -gt 0 ]; then
    echo "warn: $unindexed unindexed artifact file(s) (run artifact-index.sh prune --files)"
  fi
  if [ "$residue" -eq 1 ]; then
    echo "hint: run cleanup.sh --apply to remove safe harness residue"
  else
    echo "ok: no crash residue detected"
  fi
}

check_harness_state() {
  local project_dir="${OMS_DOCTOR_PROJECT_DIR:-}"
  local oms_dir

  if [ -z "$project_dir" ]; then
    [ -d "$PWD/.oms" ] || return 0
    project_dir="$PWD"
  fi

  if ! project_dir="$(cd "$project_dir" 2>/dev/null && pwd)"; then
    echo "warn: harness project dir unavailable: ${OMS_DOCTOR_PROJECT_DIR:-$PWD}"
    return 0
  fi

  oms_dir="$project_dir/.oms"
  printf '\n# harness state\n'
  if [ ! -d "$oms_dir" ]; then
    echo "ok: no .oms harness state"
    return 0
  fi

  echo "ok: harness state $oms_dir"
  if [ -f "$oms_dir/.gitignore" ]; then
    echo "ok: .oms/.gitignore"
  else
    echo "warn: .oms/.gitignore missing (re-run any harness command)"
  fi

  check_harness_artifact_index "$project_dir"
  check_harness_run_state "$project_dir"
  check_harness_sensitive_files "$project_dir"
  check_harness_residue "$project_dir"
}

load_user_tool_paths

case "$REQUIRE_TOOLS" in
  0|1) ;;
  *)
    echo "error: OH_MY_SETTING_REQUIRE_TOOLS must be 0 or 1" >&2
    exit 2
    ;;
esac

check_install_receipt
check_snapshots

check_cmd git
check_cmd curl
check_cmd node
check_cmd npm
check_cmd uv
check_cmd claude
check_cmd codex
check_cmd agy
# gh is optional: only GitHub-source and explicit GitHub CLI workflows need it,
# and compute clusters routinely lack it (like the slurm tools below). Those
# features fail loudly on their own when gh is absent.
check_optional_cmd gh

check_bash_version

check_optional_cmd timeout
check_optional_cmd sbatch
check_optional_cmd srun
check_optional_cmd squeue
check_optional_cmd sinfo
check_optional_cmd scancel

check_path "$INSTALL_ROOT/rules/global-AGENTS.md"
check_path "$INSTALL_ROOT/skills.manifest.json"
check_path "$INSTALL_ROOT/.agents/plugins/marketplace.json"
check_path "$INSTALL_ROOT/plugins/oh-my-setting/.codex-plugin/plugin.json"
check_path "$INSTALL_ROOT/plugins/oh-my-setting/hooks.json"
check_path "$HOME/.codex/AGENTS.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
check_path "$HOME/.claude/CLAUDE.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
check_path "$HOME/.gemini/AGENTS.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
check_custom_skills "$HOME/.codex/skills"
check_custom_skills "$HOME/.claude/skills"
check_custom_skills "$HOME/.gemini/antigravity/skills"
check_path "$HOME/.oh-my-setting-prompts" "$INSTALL_ROOT/prompts"
check_path "$HOME/.local/bin/oms" "$INSTALL_ROOT/scripts/oms"

if ! "$ROOT/scripts/skill-doctor.sh"; then
  FAILED=1
fi

if ! "$INSTALL_ROOT/scripts/install-skills.sh" >/dev/null; then
  echo "fail: skills.manifest.json out of sync (run scripts/install-skills.sh for details)"
  FAILED=1
fi

check_codex_plugin

check_harness_state

if [ "$FAILED" -ne 0 ]; then
  echo "doctor: failed"
  exit 1
fi

echo "doctor: ok"
