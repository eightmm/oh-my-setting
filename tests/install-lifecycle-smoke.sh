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

shutil.copytree(source, target, ignore=ignore, ignore_dangling_symlinks=True)
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
standalone_installer="$TMP/install-standalone.sh"
cp "$upstream/install.sh" "$standalone_installer"

export HOME="$TMP/home"
unset NVM_DIR
mkdir -p "$HOME"
# Every expectation below is a string built from $HOME and compared against a
# path the installer resolved with `pwd -P`. Git Bash reaches the same directory
# both through its POSIX mount and through the drive-letter form, and the first
# Windows run failed on exactly that: the copy was recorded under one spelling
# and looked up under the other. Resolve HOME the way the installer will, once,
# before anything derives a path from it.
HOME="$(cd "$HOME" && pwd -P)"
export HOME
export CODEX_HOME="$HOME/.codex"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export TMPDIR="$TMP/runtime"
export OMS_LOCK_DIR="$HOME/.cache/oh-my-setting/locks"
export OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT=1
export OH_MY_SETTING_REPO_URL="$upstream"
export OH_MY_SETTING_GENERATE_SLURM=0
export OH_MY_SETTING_GENERATE_MACHINE=0
export OH_MY_SETTING_STAR_PROMPT=0
export OH_MY_SETTING_CODEX_PLUGIN=0
export OH_MY_SETTING_REQUIRE_TOOLS=0
export OH_MY_SETTING_MODEL_DOCTOR=0
# Auto-update is on by default; route the trigger at a sandbox cron file so
# this suite never touches the real user's crontab or systemd manager.
export OH_MY_SETTING_AUTO_UPDATE_METHOD=cron
export OH_MY_SETTING_AUTO_UPDATE_CRON_FILE="$TMP/autoupdate.cron"
bin="$TMP/bin"
initial_npm_prefix="$TMP/initial-npm-prefix"
antigravity_settings="$HOME/.gemini/antigravity-cli/settings.json"
mkdir -p "$HOME/.codex" "$(dirname "$antigravity_settings")" \
  "$TMPDIR" "$bin" "$initial_npm_prefix" "$HOME/.nvm"
cat > "$HOME/.nvm/nvm.sh" <<'EOF'
printf '%s\n' sourced > "$HOME/.nvm-was-sourced"
EOF
printf 'user rules\n' > "$HOME/.codex/AGENTS.md"
printf 'user profile marker\n' > "$HOME/.profile"
cat > "$antigravity_settings" <<'EOF'
{
  "theme": "user-before",
  "permissions": {
    "allow": ["read_file(*)", "user(existing)"]
  }
}
EOF
  for cli in codex claude agy gh ntn uv uvx node; do
  {
    printf '%s\n' '#!/usr/bin/env bash'
    if [ "$cli" = node ]; then
      printf '%s\n' 'if [ "${1:-}" = -p ]; then echo 24; elif [ "${1:-}" = --version ]; then echo v24.18.0; else echo node 24.18.0; fi'
    else
      case "$cli" in
        claude) locked=2.1.226 ;;
        codex) locked=0.147.0 ;;
        agy) locked=1.1.11 ;;
        gh) locked=2.97.0 ;;
        ntn) locked=0.21.9 ;;
        uv|uvx) locked=0.12.3 ;;
      esac
      printf '%s\n' "echo '$cli $locked'"
    fi
  } > "$bin/$cli"
  chmod +x "$bin/$cli"
done
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
case "${1:-} ${2:-} ${3:-}" in
  "plugin marketplace list") exit 0 ;;
esac
case "${1:-} ${2:-}" in
  "plugin list") printf '%s\n' '{"installed":[]}' ; exit 0 ;;
esac
printf '%s\n' 'codex 0.147.0'
EOF
cat > "$bin/agy" <<'EOF'
#!/usr/bin/env bash
state="$HOME/.agy-oh-my-setting-installed"
case "${1:-} ${2:-}" in
  "plugin install") touch "$state"; printf '%s\n' 'installed'; exit 0 ;;
  "plugin list")
    if [ -f "$state" ]; then
      printf '%s\n' '{"imports":[{"name":"oh-my-setting"}]}'
    else
      printf '%s\n' '{"imports":[]}'
    fi
    exit 0
    ;;
  "plugin uninstall") rm -f "$state"; exit 0 ;;
esac
printf '%s\n' 'agy 1.1.11'
EOF
chmod +x "$bin/codex" "$bin/agy"
mkdir -p "$initial_npm_prefix/bin" "$initial_npm_prefix/lib/node_modules"
python3 - "$ROOT/tools.lock.json" "$initial_npm_prefix" <<'PY'
import json, os, stat, sys
from pathlib import Path

lock = json.load(open(sys.argv[1], encoding="utf-8"))
prefix = Path(sys.argv[2])
root = prefix / "lib" / "node_modules"
for name, row in lock["npm"].items():
    package_root = root / row["package"]
    package_root.mkdir(parents=True, exist_ok=True)
    (package_root / "package.json").write_text(
        json.dumps({"name": row["package"], "version": row["version"]}),
        encoding="utf-8",
    )
    for native in row.get("native", {}).values():
        native_root = package_root / "node_modules" / native["alias"]
        native_root.mkdir(parents=True, exist_ok=True)
        (native_root / "package.json").write_text(
            json.dumps({"name": native["package"], "version": native["version"]}),
            encoding="utf-8",
        )
    binary = prefix / "bin" / row["binary"]
    if row["binary"] == "codex":
        body = """#!/usr/bin/env bash
case "${1:-} ${2:-} ${3:-}" in
  "plugin marketplace list") exit 0 ;;
  "plugin list --json") printf '%s\\n' '{"installed":[]}' ; exit 0 ;;
esac
printf '%s\\n' 'codex 0.147.0'
"""
    else:
        body = "#!/usr/bin/env bash\necho '%s %s'\n" % (row["binary"], row["version"])
    binary.write_text(body, encoding="utf-8")
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    # Real npm puts binaries under PREFIX/bin on POSIX but at the PREFIX root
    # on Windows, and managed_npm_binary looks only in the platform's place.
    # Write both so the reuse decision finds the managed binary on every
    # host; the extra copy is inert where unused.
    windows_binary = prefix / row["binary"]
    windows_binary.write_text(body, encoding="utf-8")
    windows_binary.chmod(
        windows_binary.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
PY
{
  printf '%s\n' '#!/usr/bin/env bash'
  printf '%s\n' "prefix='$initial_npm_prefix'"
  printf '%s\n' 'case "$1 $2 $3" in'
  printf '%s\n' '  "config get prefix") printf "%s\n" "$prefix" ;;'
  printf '%s\n' '  "prefix -g ") printf "%s\n" "$prefix" ;;'
  printf '%s\n' '  "root -g ") printf "%s/lib/node_modules\n" "$prefix" ;;'
  printf '%s\n' '  "list -g --depth=0") printf "%s\n" '\''{"dependencies":{"@anthropic-ai/claude-code":{"version":"2.1.226"},"@openai/codex":{"version":"0.147.0"},"ntn":{"version":"0.21.9"}}}'\'' ;;'
  printf '%s\n' '  *) exit 1 ;;'
  printf '%s\n' 'esac'
} > "$bin/npm"
chmod +x "$bin/npm"
export PATH="$bin:$PATH"

notion_data_source_id="ea343dea-4a66-4421-9653-dfc4fe68ed10"
lifecycle_lock="$OMS_LOCK_DIR/install-lifecycle.lock.d"
mkdir -p "$lifecycle_lock"
printf '%s\n' "$$" > "$lifecycle_lock/pid"
printf '%s\n' "$(date +%s)" > "$lifecycle_lock/started"
printf '%s\n' "fixture-live-owner" > "$lifecycle_lock/owner"
install_status=0
(cd "$HOME" && bash "$standalone_installer" --peer-permissions \
  --notion-data-source "$notion_data_source_id") > "$TMP/install-locked.txt" 2>&1 ||
  install_status=$?
[ "$install_status" = 75 ] ||
  fail "a concurrent lifecycle mutation must block before initial clone (got $install_status): $(cat "$TMP/install-locked.txt")"
[ ! -e "$HOME/.oh-my-setting" ] ||
  fail "a blocked initial install created its checkout"

# A crashed owner must not brick installation forever. The standalone copy has
# no scripts/lib beside it, so this also exercises the curl-style bootstrap
# path rather than accidentally sourcing the checkout's lock helper.
printf '%s\n' 999999 > "$lifecycle_lock/pid"
printf '%s\n' 0 > "$lifecycle_lock/started"
printf '%s\n' "fixture-dead-owner" > "$lifecycle_lock/owner"
(cd "$HOME" && bash "$standalone_installer" --peer-permissions \
  --notion-data-source "$notion_data_source_id") > "$TMP/install-out.txt" 2>&1 ||
  { cat "$TMP/install-out.txt" >&2; fail "install failed"; }
[ ! -e "$lifecycle_lock" ] || {
  tail -n 40 "$TMP/install-out.txt" >&2
  fail "successful install leaked its lifecycle lock"
}
grep -Fq "note: moved your existing" "$TMP/install-out.txt" ||
  fail "displacing existing rules must be announced at install time"
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
# --peer-permissions is the explicit opt-in: exact user-global grants are
# disclosed, only newly added rules are owned, and the prior state is retained.
[ -f "$antigravity_settings" ] ||
  fail "--peer-permissions did not write the Antigravity settings"
grep -Fq 'granting Antigravity user-global consult permissions: read_file(*), command(*) (all-MCP access remains approval-gated)' \
  "$TMP/install-out.txt" || fail "--peer-permissions did not disclose its exact grants"
if ! python3 - "$antigravity_settings" <<'PY'
import json
import sys

settings = sys.argv[1]
cfg = json.load(open(settings, encoding="utf-8"))
assert cfg["theme"] == "user-before", cfg
assert cfg["permissions"]["allow"] == [
    "read_file(*)", "user(existing)", "command(*)"
], cfg
state = json.load(open(settings + ".oh-my-setting-permissions.json", encoding="utf-8"))
assert state == {"schema": 1, "rules": ["command(*)"]}, state
backup = json.load(open(settings + ".bak", encoding="utf-8"))
assert backup["permissions"]["allow"] == ["read_file(*)", "user(existing)"], backup
PY
then
  fail "--peer-permissions did not track only its new consult grants"
fi
grep -Fq 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.bashrc" ||
  fail "required tool install did not persist ~/.local/bin on PATH"
for shell_rc in .profile .zshrc .zprofile; do
  grep -Fq 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/$shell_rc" ||
    fail "required tool install did not persist ~/.local/bin in $shell_rc"
done
grep -Fq 'user profile marker' "$HOME/.profile" ||
  fail "required tool install replaced the existing Bash login profile"
[ ! -e "$HOME/.bash_profile" ] && [ ! -e "$HOME/.bash_login" ] ||
  fail "tool install created a higher-priority Bash login file over .profile"
[ ! -e "$HOME/.nvm-was-sourced" ] ||
  fail "an unrelated mismatched nvm.sh was sourced despite exact active Node"
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
  "$HOME/.codex/skills/oms-agent-harness:$dest/custom-skills/oms-agent-harness" \
  "$HOME/.claude/skills/oms-agent-harness:$dest/custom-skills/oms-agent-harness" \
  "$HOME/.gemini/antigravity/skills/oms-agent-harness:$dest/custom-skills/oms-agent-harness"; do
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
grep -Fq "claude-statusline.py" "$HOME/.claude/settings.json" ||
  fail "install did not register the Claude main HUD"
grep -Fq "claude-subagent-statusline.py" "$HOME/.claude/settings.json" ||
  fail "install did not register the Claude subagent HUD"
[ ! -e "$dest/local/machine.md" ] ||
  fail "minimal install generated a machine snapshot"
grep -Fq '# oh-my-setting autoupdate:begin' "$TMP/autoupdate.cron" 2>/dev/null ||
  fail "default install did not register the auto-update trigger"
grep -Fq 'auto-update.sh" apply' "$TMP/autoupdate.cron" ||
  fail "default auto-update trigger must be apply mode"
[ ! -e "$HOME/.config/systemd/user/oh-my-setting-autoupdate.timer" ] ||
  fail "forced cron method still wrote a systemd unit"
oms list > "$TMP/oms-tools.txt"
grep -Fq plan-run "$TMP/oms-tools.txt" || fail "dispatcher omitted plan-run"
grep -Fq model-doctor "$TMP/oms-tools.txt" || fail "dispatcher omitted model-doctor"
grep -Fq journal "$TMP/oms-tools.txt" || fail "dispatcher omitted journal"
work_journal_config="$XDG_CONFIG_HOME/oh-my-setting/work-journal.json"
[ -f "$work_journal_config" ] || fail "install did not configure Work Journal"
python3 - "$work_journal_config" "$notion_data_source_id" <<'PY'
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema_version"] == 1
assert row["notion"]["data_source_id"] == sys.argv[2]
assert "token" not in json.dumps(row).lower()
PY
oms journal status --repo "$upstream" --json > "$TMP/journal-status.json"
python3 - "$TMP/journal-status.json" <<'PY'
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["notion"]["configured"] is True
assert row["notion"]["credential_present"] is False
PY
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

# Every mutating lifecycle entrypoint shares the same user lock. Read-only
# health and availability reports remain usable while a long install/update is
# active, but no second writer may cross the boundary.
mkdir -p "$lifecycle_lock"
printf '%s\n' "$$" > "$lifecycle_lock/pid"
printf '%s\n' "$(date +%s)" > "$lifecycle_lock/started"
printf '%s\n' "fixture-live-owner" > "$lifecycle_lock/owner"
(cd "$HOME" && "$dest/scripts/update.sh" --check) > "$TMP/locked-check.txt" 2>&1 ||
  { cat "$TMP/locked-check.txt" >&2; fail "read-only update check waited on the lifecycle lock"; }
(cd "$HOME" && "$dest/scripts/doctor.sh") > "$TMP/locked-doctor.txt" 2>&1 ||
  { cat "$TMP/locked-doctor.txt" >&2; fail "read-only doctor waited on the lifecycle lock"; }
(cd "$HOME" && "$dest/scripts/doctor.sh" --surfaces) > "$TMP/locked-surfaces.txt" 2>&1 || true
if grep -Fq 'install lifecycle lock' "$TMP/locked-surfaces.txt"; then
  fail "read-only surfaces report waited on the lifecycle lock"
fi

before_locked_update="$(git -C "$dest" rev-parse HEAD)"
update_status=0
(cd "$HOME" && "$dest/scripts/update.sh" --no-tools) > "$TMP/locked-update.txt" 2>&1 ||
  update_status=$?
[ "$update_status" = 75 ] ||
  fail "concurrent update should fail on the lifecycle lock (got $update_status): $(cat "$TMP/locked-update.txt")"
[ "$(git -C "$dest" rev-parse HEAD)" = "$before_locked_update" ] ||
  fail "blocked update changed the checkout"

rm -f "$HOME/.codex/AGENTS.md"
repair_status=0
(cd "$HOME" && "$dest/scripts/doctor.sh" --repair) > "$TMP/locked-repair.txt" 2>&1 ||
  repair_status=$?
[ "$repair_status" = 75 ] ||
  fail "concurrent repair should fail on the lifecycle lock (got $repair_status): $(cat "$TMP/locked-repair.txt")"
[ ! -e "$HOME/.codex/AGENTS.md" ] || fail "blocked repair relinked a managed target"

uninstall_status=0
(cd "$HOME" && "$dest/scripts/uninstall.sh" --yes) > "$TMP/locked-uninstall.txt" 2>&1 ||
  uninstall_status=$?
[ "$uninstall_status" = 75 ] ||
  fail "concurrent uninstall should fail on the lifecycle lock (got $uninstall_status): $(cat "$TMP/locked-uninstall.txt")"
[ -f "$XDG_CONFIG_HOME/oh-my-setting/install.json" ] ||
  fail "blocked uninstall removed the install receipt"
grep -Fq 'skill-router.sh' "$HOME/.claude/settings.json" ||
  fail "blocked uninstall removed the Claude hooks"
rm -rf "$lifecycle_lock"

(cd "$HOME" && "$dest/scripts/update.sh" --no-tools)
[ "$(git -C "$dest" rev-parse HEAD)" = "$(git -C "$upstream" rev-parse HEAD)" ] ||
  fail "update did not fast-forward to fixture HEAD"
oms_install_target_matches "$dest/rules/global-AGENTS.md" "$HOME/.codex/AGENTS.md" ||
  fail "update did not reconcile the managed config"
[ "$(find "$HOME/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' -print |
  wc -l | tr -d ' ')" = 1 ] ||
  fail "update created a duplicate backup"

# Repair is convergence, not a new install profile. A valid schema-2 receipt
# is the authority even when the caller's shell carries opposite component
# values; link.sh must not rewrite opt-outs and managed hooks must converge off.
receipt="$XDG_CONFIG_HOME/oh-my-setting/install.json"
python3 - "$receipt" <<'PY'
import json
import sys

path = sys.argv[1]
row = json.load(open(path, encoding="utf-8"))
row["profile"] = "custom"
row["ref"] = "release/pinned"
row["previous_commit"] = "1" * 40
row["components"].update({
    "tools": False,
    "claude_hooks": False,
    "codex_plugin": False,
    "auto_update": False,
    "machine_snapshot": False,
    "slurm_snapshot": False,
})
row["component_modes"] = {
    "auto_update": "check",
    "machine_snapshot": "0",
    "slurm_snapshot": "0",
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
rm -f "$HOME/.codex/AGENTS.md"
(cd "$HOME" && OH_MY_SETTING_PROFILE=full OH_MY_SETTING_REF=edge \
  OH_MY_SETTING_INSTALL_TOOLS=1 OH_MY_SETTING_CLAUDE_HOOKS=1 \
  OH_MY_SETTING_CODEX_PLUGIN=1 OH_MY_SETTING_AUTO_UPDATE=1 \
  OH_MY_SETTING_GENERATE_MACHINE=1 OH_MY_SETTING_GENERATE_SLURM=1 \
  "$dest/scripts/doctor.sh" --repair) > "$TMP/hydrated-repair.txt" 2>&1 ||
  { cat "$TMP/hydrated-repair.txt" >&2; fail "receipt-hydrated doctor repair failed"; }
oms_install_target_matches "$dest/rules/global-AGENTS.md" "$HOME/.codex/AGENTS.md" ||
  fail "receipt-hydrated repair did not relink the managed rules"
if grep -Fq 'skill-router.sh' "$HOME/.claude/settings.json" 2>/dev/null; then
  fail "receipt hook opt-out was ignored during repair"
fi
python3 - "$receipt" <<'PY' || fail "doctor repair rewrote the schema-2 install profile"
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["profile"] == "custom", row
assert row["ref"] == "release/pinned", row
assert row["previous_commit"] == "1" * 40, row
assert row["components"] == {
    "tools": False,
    "claude_hooks": False,
    "codex_plugin": False,
    "auto_update": False,
    "machine_snapshot": False,
    "slurm_snapshot": False,
}, row
assert row["component_modes"] == {
    "auto_update": "check",
    "machine_snapshot": "0",
    "slurm_snapshot": "0",
}, row
PY

# These edits happen after installation and therefore belong to the user. The
# duplicate command grant also proves uninstall consumes only its own one copy.
printf 'keep me\n' > "$dest/user-local.txt"
reinstall_status=0
(cd "$HOME" && bash "$standalone_installer" --no-connect-services) \
  >"$TMP/reinstall-dirty.out" 2>&1 || reinstall_status=$?
[ "$reinstall_status" -ne 0 ] ||
  fail "reinstall silently updated a dirty managed checkout"
grep -Fq 'dirty' "$TMP/reinstall-dirty.out" ||
  fail "dirty reinstall refusal was not explained: $(cat "$TMP/reinstall-dirty.out")"
[ "$(cat "$dest/user-local.txt")" = 'keep me' ] ||
  fail "dirty reinstall changed the user-owned file"
rm -f "$dest/user-local.txt"

python3 - "$antigravity_settings" <<'PY'
import json
import sys

path = sys.argv[1]
cfg = json.load(open(path, encoding="utf-8"))
cfg["theme"] = "user-after"
cfg["permissions"]["allow"].extend(["user(after)", "command(*)"])
with open(path, "w", encoding="utf-8") as handle:
    json.dump(cfg, handle, indent=2)
    handle.write("\n")
PY

printf 'keep before explicit purge\n' > "$dest/user-local.txt"
purge_status=0
(cd "$HOME" && "$dest/scripts/uninstall.sh" --yes --purge) \
  >"$TMP/purge-dirty.out" 2>&1 || purge_status=$?
[ "$purge_status" -ne 0 ] || fail "purge deleted a dirty checkout without explicit consent"
grep -Fq 'purge-dirty' "$TMP/purge-dirty.out" ||
  fail "dirty purge refusal did not name the override: $(cat "$TMP/purge-dirty.out")"
[ -f "$dest/user-local.txt" ] || fail "refused dirty purge removed the user-owned file"
[ -e "$HOME/.codex/AGENTS.md" ] ||
  fail "dirty purge refusal partially unlinked the installation"

(cd "$HOME" && "$dest/scripts/uninstall.sh" --yes --purge --purge-dirty)
[ -f "$HOME/.codex/AGENTS.md" ] &&
  [ "$(sed -n '1p' "$HOME/.codex/AGENTS.md")" = "user rules" ] ||
  fail "uninstall did not restore the original Codex rules"
[ ! -e "$HOME/.claude/CLAUDE.md" ] || fail "uninstall left Claude rules"
[ ! -e "$HOME/.gemini/AGENTS.md" ] || fail "uninstall left Gemini rules"
if grep -Fq "skill-router.sh" "$HOME/.claude/settings.json" 2>/dev/null; then
  fail "uninstall left the Claude hook"
fi
if grep -Fq "claude-statusline.py" "$HOME/.claude/settings.json" 2>/dev/null; then
  fail "uninstall left the Claude main HUD"
fi
if grep -Fq "claude-subagent-statusline.py" "$HOME/.claude/settings.json" 2>/dev/null; then
  fail "uninstall left the Claude subagent HUD"
fi
if ! python3 - "$antigravity_settings" <<'PY'
import json
import sys

cfg = json.load(open(sys.argv[1], encoding="utf-8"))
assert cfg["theme"] == "user-after", cfg
assert cfg["permissions"]["allow"] == [
    "read_file(*)", "user(existing)", "user(after)", "command(*)"
], cfg
PY
then
  fail "uninstall did not preserve user-owned Antigravity settings"
fi
[ ! -e "$antigravity_settings.oh-my-setting-permissions.json" ] ||
  fail "uninstall left managed Antigravity permission ownership state"
[ "$shim_created" = 0 ] || [ ! -e "$HOME/.local/bin/python3" ] ||
  fail "uninstall left its Python shim"
[ -z "$(find "$HOME" -name '*.oh-my-setting-managed.json' -print -quit)" ] ||
  fail "uninstall left a copy ownership sidecar"
[ ! -e "$work_journal_config" ] ||
  fail "uninstall --purge left Work Journal configuration"
[ ! -d "$dest" ] || fail "uninstall --purge left the checkout"
if grep -Fq 'oh-my-setting autoupdate' "$TMP/autoupdate.cron" 2>/dev/null; then
  fail "uninstall left the auto-update trigger"
fi

# The default install — tools enabled — was the one path CI never ran, and it is
# the path most users take. It is also what turned CI red for three commits:
# install-tools persists a PATH line into .bashrc, and the assertion left over
# from before tools were installed by default said that file must be untouched.
# Every tool is stubbed as already present, so install-tools short-circuits at
# each step and this stays a no-network test of the documented default.
tools_home="$TMP/home-tools"
tools_bin="$TMP/tools-bin"
npm_prefix="$TMP/npm-prefix"
mkdir -p "$tools_home" "$tools_bin" "$npm_prefix/bin" "$npm_prefix/lib/node_modules"
tools_home="$(cd "$tools_home" && pwd -P)"

for cli in claude codex agy gh ntn uv uvx node; do
  {
    printf '%s\n' '#!/usr/bin/env bash'
    # ensure_node reads the major version through `node -p`; anything below 22
    # sends the installer to nvm, which is exactly what must not happen here.
    if [ "$cli" = node ]; then
      printf '%s\n' 'if [ "${1:-}" = -p ]; then echo 24; elif [ "${1:-}" = --version ]; then echo v24.18.0; else echo node 24.18.0; fi'
    else
      case "$cli" in
        claude) locked=2.1.226 ;;
        codex) locked=0.147.0 ;;
        agy) locked=1.1.11 ;;
        gh) locked=2.97.0 ;;
        ntn) locked=0.21.9 ;;
        uv|uvx) locked=0.12.3 ;;
      esac
      printf '%s\n' "echo '$cli $locked'"
    fi
  } > "$tools_bin/$cli"
  chmod +x "$tools_bin/$cli"
done
python3 - "$ROOT/tools.lock.json" "$npm_prefix" <<'PY'
import json, stat, sys
from pathlib import Path

lock = json.load(open(sys.argv[1], encoding="utf-8"))
prefix = Path(sys.argv[2])
root = prefix / "lib" / "node_modules"
for row in lock["npm"].values():
    package_root = root / row["package"]
    package_root.mkdir(parents=True, exist_ok=True)
    (package_root / "package.json").write_text(
        json.dumps({"name": row["package"], "version": row["version"]}), encoding="utf-8"
    )
    for native in row.get("native", {}).values():
        native_root = package_root / "node_modules" / native["alias"]
        native_root.mkdir(parents=True, exist_ok=True)
        (native_root / "package.json").write_text(
            json.dumps({"name": native["package"], "version": native["version"]}), encoding="utf-8"
        )
    binary = prefix / "bin" / row["binary"]
    if row["binary"] == "codex":
        body = """#!/usr/bin/env bash
case "${1:-} ${2:-} ${3:-}" in
  "plugin marketplace list") exit 0 ;;
  "plugin list --json") printf '%s\\n' '{"installed":[]}' ; exit 0 ;;
esac
printf '%s\\n' 'codex 0.147.0'
"""
    else:
        body = "#!/usr/bin/env bash\necho '%s %s'\n" % (row["binary"], row["version"])
    binary.write_text(body, encoding="utf-8")
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    # Real npm puts binaries under PREFIX/bin on POSIX but at the PREFIX root
    # on Windows, and managed_npm_binary looks only in the platform's place.
    # Write both so the reuse decision finds the managed binary on every
    # host; the extra copy is inert where unused.
    windows_binary = prefix / row["binary"]
    windows_binary.write_text(body, encoding="utf-8")
    windows_binary.chmod(
        windows_binary.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
PY
{
  printf '%s\n' '#!/usr/bin/env bash'
  printf '%s\n' "prefix='$npm_prefix'"
  printf '%s\n' 'case "$1 $2 $3" in'
  printf '%s\n' '  "config get prefix") printf "%s\n" "$prefix" ;;'
  printf '%s\n' '  "prefix -g ") printf "%s\n" "$prefix" ;;'
  printf '%s\n' '  "root -g ") printf "%s/lib/node_modules\n" "$prefix" ;;'
  printf '%s\n' '  "list -g --depth=0") printf "%s\n" '\''{"dependencies":{"@anthropic-ai/claude-code":{"version":"2.1.226"},"@openai/codex":{"version":"0.147.0"},"ntn":{"version":"0.21.9"}}}'\'' ;;'
  printf '%s\n' '  *) exit 1 ;;'
  printf '%s\n' 'esac'
} > "$tools_bin/npm"
chmod +x "$tools_bin/npm"

(cd "$tools_home" && env -u NVM_DIR -u CODEX_HOME HOME="$tools_home" \
  XDG_CONFIG_HOME="$tools_home/.config" XDG_CACHE_HOME="$tools_home/.cache" \
  PATH="$tools_bin:$PATH" bash "$upstream/install.sh") > "$TMP/tools-install.txt" 2>&1 ||
  { cat "$TMP/tools-install.txt" >&2; fail "the default install must succeed"; }
grep -Fq 'tools: ok' "$TMP/tools-install.txt" ||
  fail "the default install did not run install-tools"
grep -Fq 'doctor: ok' "$TMP/tools-install.txt" ||
  fail "the default install did not end on a passing doctor"
# Without the opt-in flag a default install must not widen Antigravity's
# authority — it reports the denial and names the flag instead.
grep -Fq 'rerun with --peer-permissions' "$TMP/tools-install.txt" ||
  fail "the default install did not report ungranted peer permissions"
[ ! -f "$tools_home/.gemini/antigravity-cli/settings.json" ] ||
  fail "a default install silently granted Antigravity permissions"
# The documented behaviour: every complete install makes user-local tools
# available in later shells.
grep -Fq 'export PATH="$HOME/.local/bin:$PATH"' "$tools_home/.bashrc" ||
  fail "the default install must persist ~/.local/bin on PATH"
grep -Fq 'export PATH="$HOME/.local/bin:$PATH"' "$tools_home/.profile" ||
  fail "the default install must persist ~/.local/bin in the Bash login profile"
[ ! -e "$tools_home/.bash_profile" ] ||
  fail "the default install created .bash_profile and masked lower-priority profiles"

# An unrelated executable that merely prints the locked version must not make
# doctor certify it as the npm-managed provider package.
cp "$tools_home/.local/bin/codex" "$TMP/managed-codex-shim"
printf '%s\n' '#!/usr/bin/env bash' "echo 'codex 0.147.0'" \
  > "$tools_home/.local/bin/codex"
chmod +x "$tools_home/.local/bin/codex"
HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
  XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
  OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
  bash "$upstream/scripts/doctor.sh" > "$TMP/tool-shadow-doctor.txt" 2>&1 || true
grep -Fq 'tool version drift: codex (PATH command is not the managed npm package)' \
  "$TMP/tool-shadow-doctor.txt" ||
  fail "doctor accepted a same-version PATH shadow as the managed Codex package"
mv "$TMP/managed-codex-shim" "$tools_home/.local/bin/codex"

native_alias="$($PYTHON - "$upstream/tools.lock.json" <<'PY'
import json, platform, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
os_name = {"Darwin": "darwin", "Linux": "linux", "Windows": "windows"}[platform.system()]
machine = platform.machine().lower()
arch = "amd64" if machine in ("amd64", "x86_64") else "arm64"
print(row["npm"]["codex"]["native"][os_name + "-" + arch]["alias"])
PY
)"
native_alias="${native_alias//$'\r'/}"
native_manifest="$npm_prefix/lib/node_modules/@openai/codex/node_modules/$native_alias/package.json"
cp "$native_manifest" "$TMP/native-manifest.backup"
$PYTHON - "$native_manifest" <<'PY'
import json, sys
path = sys.argv[1]
row = json.load(open(path, encoding="utf-8"))
row["version"] = "0.0.0"
json.dump(row, open(path, "w", encoding="utf-8"))
PY
HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
  XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
  OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
  bash "$upstream/scripts/doctor.sh" > "$TMP/native-drift-doctor.txt" 2>&1 || true
grep -Fq 'platform-native payload does not match the lock' \
  "$TMP/native-drift-doctor.txt" ||
  fail "doctor accepted a mismatched Codex native payload manifest"
mv "$TMP/native-manifest.backup" "$native_manifest"

transaction_marker="$npm_prefix/lib/node_modules/@openai/codex.oh-my-setting-transaction"
printf 'schema=1\n' > "$transaction_marker"
HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
  XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
  OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
  bash "$upstream/scripts/doctor.sh" > "$TMP/npm-residue-doctor.txt" 2>&1 || true
grep -Fq 'interrupted npm transaction residue is present' \
  "$TMP/npm-residue-doctor.txt" ||
  fail "doctor ignored interrupted npm transaction residue"
rm -f "$transaction_marker"

uv_digest="$($PYTHON - "$tools_bin/uv" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
uv_digest="${uv_digest//$'\r'/}"
printf 'sha256=%s\n' "$uv_digest" > "$tools_bin/uv.oh-my-setting-managed"
printf '# changed after managed install\n' >> "$tools_bin/uv"
HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
  XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
  OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
  bash "$upstream/scripts/doctor.sh" > "$TMP/direct-digest-doctor.txt" 2>&1 || true
grep -Fq 'tool version drift: uv (managed binary digest changed)' \
  "$TMP/direct-digest-doctor.txt" ||
  fail "doctor accepted a changed managed direct-tool binary"
sed -i.bak '$d' "$tools_bin/uv"
rm -f "$tools_bin/uv.bak" "$tools_bin/uv.oh-my-setting-managed"

echo "install-lifecycle: ok ($expected_mode)"
