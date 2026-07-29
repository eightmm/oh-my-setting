#!/usr/bin/env bash
set -euo pipefail

# Real install -> update -> uninstall contract, reusable from every hosted OS.
# The fixture repository is built from the working tree so CI tests the patch,
# including uncommitted files in a pull-request checkout.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-install-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
# macOS TMPDIR ends in a slash, so the mktemp template yields a path with "//"
# in it. Every expectation below is compared against a source string the
# installer recorded from `pwd -P`, which has no double slash — the run then
# fails on paths that are the same directory spelled two ways. Normalize once.
TMP="$(cd "$TMP" && pwd -P)"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

PYTHON=""
if command -v python3 >/dev/null 2>&1; then
  PYTHON=python3
elif command -v python >/dev/null 2>&1; then
  PYTHON=python
else
  fail "Python 3 is required"
fi

upstream="$TMP/upstream"
"$PYTHON" - "$ROOT" "$upstream" <<'PY'
import os
import shutil
import sys

source, target = sys.argv[1:]
excluded = {".git", ".oms", "__pycache__", "local"}

def ignore(_directory, names):
    return [name for name in names if name in excluded]

shutil.copytree(source, target, ignore=ignore)
PY

export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=ci
export GIT_AUTHOR_EMAIL=ci@example.com
export GIT_COMMITTER_NAME=ci
export GIT_COMMITTER_EMAIL=ci@example.com
git -C "$upstream" init -b main -q
git -C "$upstream" add -A
git -C "$upstream" commit -qm "install fixture"

export HOME="$TMP/home"
mkdir -p "$HOME"
# Every expectation below is a string built from $HOME and compared against a
# path the installer resolved with `pwd -P`. Git Bash reaches the same directory
# both through its POSIX mount and through the drive-letter form, and the first
# Windows run failed on exactly that: the copy was recorded under one spelling
# and looked up under the other. Resolve HOME the way the installer will, once,
# before anything derives a path from it.
HOME="$(cd "$HOME" && pwd -P)"
export HOME
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export TMPDIR="$TMP/runtime"
export OH_MY_SETTING_REPO_URL="$upstream"
export OH_MY_SETTING_GENERATE_SLURM=0
export OH_MY_SETTING_GENERATE_MACHINE=0
export OH_MY_SETTING_STAR_PROMPT=0
export OH_MY_SETTING_CODEX_PLUGIN=0
export OH_MY_SETTING_REQUIRE_TOOLS=0
export OH_MY_SETTING_MODEL_DOCTOR=0
bin="$TMP/bin"
mkdir -p "$HOME/.codex" "$TMPDIR" "$bin"
printf 'user rules\n' > "$HOME/.codex/AGENTS.md"
for cli in codex claude agy gh; do
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$bin/$cli"
  chmod +x "$bin/$cli"
done
export PATH="$bin:$PATH"

(cd "$HOME" && bash "$upstream/install.sh" --no-tools)
dest="$HOME/.oh-my-setting"
[ -d "$dest/.git" ] || fail "install did not clone the fixture"
# Same reason as HOME: the checkout is the source side of every ownership
# comparison, so it has to be spelled the way link.sh spelled it.
dest="$(cd "$dest" && pwd -P)"
# GNU mktemp collapses a "//" from a trailing-slash TMPDIR and BSD mktemp does
# not, so this condition cannot be reproduced on Linux at all. Assert it instead
# of letting it surface three assertions later as "managed target mismatch",
# which says nothing about the cause.
case "$HOME:$dest" in
  *//*) fail "paths are not normalized (HOME=$HOME dest=$dest)" ;;
esac
export PATH="$HOME/.local/bin:$PATH"
command -v python3 >/dev/null 2>&1 ||
  fail "installer did not expose a python3 command"
[ ! -e "$HOME/.bashrc" ] ||
  fail "minimal install modified .bashrc"
shim_created=0
if [ -f "$HOME/.local/bin/python3" ] &&
   grep -Fq 'managed by oh-my-setting' "$HOME/.local/bin/python3"; then
  shim_created=1
fi

# shellcheck source=scripts/lib/install-contract.sh
. "$dest/scripts/lib/install-contract.sh"
for pair in \
  "$HOME/.codex/AGENTS.md:$dest/rules/global-AGENTS.md" \
  "$HOME/.claude/CLAUDE.md:$dest/rules/global-AGENTS.md" \
  "$HOME/.gemini/AGENTS.md:$dest/rules/global-AGENTS.md" \
  "$HOME/.codex/skills/agent-harness:$dest/custom-skills/agent-harness" \
  "$HOME/.claude/skills/agent-harness:$dest/custom-skills/agent-harness" \
  "$HOME/.gemini/antigravity/skills/agent-harness:$dest/custom-skills/agent-harness"; do
  path="${pair%%:*}"
  source="${pair#*:}"
  oms_install_target_matches "$source" "$path" ||
    fail "managed target mismatch: $path"
done

expected_mode="$(oms_install_link_mode)"
[ "$(oms_install_receipt_field link_mode)" = "$expected_mode" ] ||
  fail "receipt did not persist $expected_mode mode"
backup="$(find "$HOME/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' -print -quit)"
[ -n "$backup" ] && [ "$(sed -n '1p' "$backup")" = "user rules" ] ||
  fail "install did not preserve the existing Codex rules"
grep -Fq "skill-router.sh" "$HOME/.claude/settings.json" ||
  fail "install did not register the Claude hook"
[ ! -e "$dest/local/machine.md" ] ||
  fail "minimal install generated a machine snapshot"
[ ! -e "$HOME/.config/systemd/user/oh-my-setting-autoupdate.timer" ] ||
  fail "minimal install registered auto-update"
oms list > "$TMP/oms-tools.txt"
grep -Fq plan-run "$TMP/oms-tools.txt" || fail "dispatcher omitted plan-run"
grep -Fq model-doctor "$TMP/oms-tools.txt" || fail "dispatcher omitted model-doctor"
"$dest/scripts/status.sh" > "$TMP/status.txt"
case "$expected_mode" in
  copy)
    grep -Fq -- "$HOME/.codex/AGENTS.md: copied" "$TMP/status.txt" ||
      fail "status did not report copy mode"
    ;;
  symlink)
    grep -Fq -- "$HOME/.codex/AGENTS.md: linked" "$TMP/status.txt" ||
      fail "status did not report symlink mode"
    ;;
esac

git -C "$upstream" commit --allow-empty -qm "fixture update"
(cd "$HOME" && "$dest/scripts/update.sh" --no-tools)
[ "$(git -C "$dest" rev-parse HEAD)" = "$(git -C "$upstream" rev-parse HEAD)" ] ||
  fail "update did not fast-forward to fixture HEAD"
oms_install_target_matches "$dest/rules/global-AGENTS.md" "$HOME/.codex/AGENTS.md" ||
  fail "update did not reconcile the managed config"
[ "$(find "$HOME/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' -print |
  wc -l | tr -d ' ')" = 1 ] ||
  fail "update created a duplicate backup"

(cd "$HOME" && "$dest/scripts/uninstall.sh" --yes --purge)
[ -f "$HOME/.codex/AGENTS.md" ] &&
  [ "$(sed -n '1p' "$HOME/.codex/AGENTS.md")" = "user rules" ] ||
  fail "uninstall did not restore the original Codex rules"
[ ! -e "$HOME/.claude/CLAUDE.md" ] || fail "uninstall left Claude rules"
[ ! -e "$HOME/.gemini/AGENTS.md" ] || fail "uninstall left Gemini rules"
if grep -Fq "skill-router.sh" "$HOME/.claude/settings.json" 2>/dev/null; then
  fail "uninstall left the Claude hook"
fi
[ "$shim_created" = 0 ] || [ ! -e "$HOME/.local/bin/python3" ] ||
  fail "uninstall left its Python shim"
[ -z "$(find "$HOME" -name '*.oh-my-setting-managed.json' -print -quit)" ] ||
  fail "uninstall left a copy ownership sidecar"
[ ! -d "$dest" ] || fail "uninstall --purge left the checkout"

echo "install-lifecycle: ok ($expected_mode)"
