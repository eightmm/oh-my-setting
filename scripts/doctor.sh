#!/usr/bin/env bash
set -euo pipefail

# Verify the install: symlink identity, tools, skills, and manifest sync for all three agent CLIs.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0
REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-1}"

# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT/scripts/lib/harness-residue.sh"

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
  if [ "$major" -lt 4 ]; then
    echo "warn: bash 4+ recommended; current bash is ${BASH_VERSION:-unknown}"
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

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    if [ -L "$target_root/$name" ]; then
      # Symlink install: certify the link points at THIS checkout's skill,
      # not a shadowed copy from another install.
      check_path "$target_root/$name" "$skill"
    else
      check_path "$target_root/$name/SKILL.md"
    fi
  done
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
        for key in ("artifact", "patch"):
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

check_cmd git
check_cmd curl
check_cmd node
check_cmd npm
check_cmd uv
check_cmd claude
check_cmd codex
check_cmd agy
# gh is optional: only the GitHub-source / git-cli-workflow features need it,
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

check_path "$ROOT/AGENTS.md"
check_path "$ROOT/skills.manifest.json"
check_path "$ROOT/.agents/plugins/marketplace.json"
check_path "$ROOT/plugins/oh-my-setting/.codex-plugin/plugin.json"
check_path "$ROOT/plugins/oh-my-setting/hooks.json"
check_path "$HOME/.codex/AGENTS.md" "$ROOT/AGENTS.md"
check_path "$HOME/.claude/CLAUDE.md" "$ROOT/AGENTS.md"
check_path "$HOME/.gemini/AGENTS.md" "$ROOT/AGENTS.md"
check_custom_skills "$HOME/.codex/skills"
check_custom_skills "$HOME/.claude/skills"
check_custom_skills "$HOME/.gemini/antigravity/skills"
check_path "$HOME/.oh-my-setting-prompts" "$ROOT/prompts"
check_path "$HOME/.oh-my-setting-workflows" "$ROOT/workflows"
check_path "$HOME/.local/bin/oms" "$ROOT/scripts/oms"

if ! "$ROOT/scripts/skill-doctor.sh"; then
  FAILED=1
fi

if ! "$ROOT/scripts/install-skills.sh" >/dev/null; then
  echo "fail: skills.manifest.json out of sync (run scripts/install-skills.sh for details)"
  FAILED=1
fi

if command -v codex >/dev/null 2>&1; then
  if codex plugin list --json 2>/dev/null |
     python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if any(p.get("name")=="oh-my-setting" and p.get("installed") for p in d.get("installed", [])) else 1)' 2>/dev/null; then
    echo "ok: codex plugin oh-my-setting"
  else
    echo "note: codex plugin oh-my-setting not installed (run scripts/install-codex-plugin.sh)"
  fi
fi

check_harness_state

if [ "$FAILED" -ne 0 ]; then
  echo "doctor: failed"
  exit 1
fi

echo "doctor: ok"
